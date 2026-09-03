//! The real TUI client: a ServerLink over the session named pipe plus the
//! crossterm event/render loop driving starcil-tui's App.

use crossterm::event::{Event as CtEvent, KeyEventKind};
#[cfg(not(windows))]
use crossterm::event;
use crossterm::{execute, terminal};
use serde_json::Value;
use starcil_protocol::attach::{ClientMode, Hello, HelloBody, InputFrame, TerminalFrame};
use starcil_protocol::types::SessionSnapshot;
use starcil_protocol::Incoming;
use starcil_platform::Transport as _;
use starcil_tui::{App, ClientMsg, ServerLink, ServerMsg};
use std::sync::mpsc::{Receiver, Sender};

/// ServerLink over the session named pipe using the async (overlapped)
/// named-pipe client on a background tokio thread. A single synchronous
/// duplex handle would deadlock on Windows: a parked ReadFile blocks any
/// concurrent WriteFile on the same kernel object.
pub struct PipeLink {
    out_tx: Sender<Value>,
    in_rx: Receiver<ServerMsg>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PipeLink {
    pub fn connect(session: &str, cols: u16, rows: u16) -> Result<Self, String> {
        let endpoint = starcil_platform::TransportEndpoint::for_session(session)
            .map_err(|e| e.to_string())?;
        let hello = Hello {
            hello: HelloBody {
                protocol_major: starcil_protocol::PROTOCOL_MAJOR,
                protocol_minor: starcil_protocol::PROTOCOL_MINOR,
                version: env!("CARGO_PKG_VERSION").to_string(),
                mode: ClientMode::Tui,
                capabilities: vec![],
                cols: Some(cols),
                rows: Some(rows),
                takeover: None,
                target: None,
            },
        };
        let (out_tx, out_rx) = std::sync::mpsc::channel::<Value>();
        let (in_tx, in_rx) = std::sync::mpsc::channel::<ServerMsg>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive_flag = alive.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                // Retry briefly while the server arms the next pipe instance
                // (or binds its Unix socket).
                let mut transport = {
                    let mut attempt = 0u32;
                    loop {
                        match starcil_platform::connect_session(
                            &endpoint,
                            starcil_platform::DEFAULT_MAX_FRAME_SIZE,
                        )
                        .await
                        {
                            Ok(t) => break t,
                            Err(e) => {
                                attempt += 1;
                                if attempt >= 40 {
                                    let _ = ready_tx.send(Err(e.to_string()));
                                    return;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                        }
                    }
                };
                if let Ok(v) = serde_json::to_value(&hello) {
                    if transport.send(v).await.is_err() {
                        let _ = ready_tx.send(Err("handshake failed".into()));
                        return;
                    }
                }
                let _ = ready_tx.send(Ok(()));
                pump_transport(transport, out_rx, in_tx).await;
                alive_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            });
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(PipeLink { out_tx, in_rx, alive }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("pipe link thread died during connect".into()),
        }
    }
}

impl ServerLink for PipeLink {
    fn send(&mut self, msg: ClientMsg) {
        let value = match msg {
            ClientMsg::Request(r) => serde_json::to_value(&r),
            ClientMsg::Input(f) => serde_json::to_value(&f),
        };
        if let Ok(v) = value {
            let _ = self.out_tx.send(v);
        }
    }

    fn drain(&mut self) -> Vec<ServerMsg> {
        self.in_rx.try_iter().collect()
    }
}

/// Connection liveness for the client loop: when the transport dies the loop
/// exits instead of idling on a dead link.
pub trait LinkHealth {
    fn link_alive(&self) -> bool;
}

impl LinkHealth for PipeLink {
    fn link_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl LinkHealth for RemoteLink {
    fn link_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Shared bidirectional pump: outgoing values drain non-blocking, incoming
/// frames classify into ServerMsg. Used by the pipe and ssh links.
async fn pump_transport(
    mut transport: impl starcil_platform::Transport,
    out_rx: Receiver<Value>,
    in_tx: Sender<ServerMsg>,
) {
    loop {
        while let Ok(v) = out_rx.try_recv() {
            if transport.send(v).await.is_err() {
                return;
            }
        }
        match tokio::time::timeout(std::time::Duration::from_millis(15), transport.recv()).await {
            Ok(Ok(Some(value))) => {
                if let Some(msg) = classify(value) {
                    if in_tx.send(msg).is_err() {
                        return;
                    }
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) => return,
            Err(_) => {}
        }
    }
}

fn classify(value: Value) -> Option<ServerMsg> {
    if value.get("welcome").is_some() {
        return None;
    }
    if value.get("event").and_then(Value::as_str) == Some("session.snapshot") {
        let snap: SessionSnapshot = serde_json::from_value(value.get("data")?.clone()).ok()?;
        return Some(ServerMsg::SessionSnapshot(snap));
    }
    if value.get("pane_id").is_some() && value.get("seq").is_some() && value.get("patches").is_some() {
        let frame: TerminalFrame = serde_json::from_value(value).ok()?;
        return Some(ServerMsg::TerminalFrame(frame));
    }
    let incoming: Incoming = serde_json::from_value(value).ok()?;
    Some(ServerMsg::Incoming(incoming))
}

/// ServerLink over an SSH-bridged remote server: a background tokio thread
/// owns the SshTransport and pumps both directions through channels.
pub struct RemoteLink {
    out_tx: Sender<Value>,
    in_rx: Receiver<ServerMsg>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RemoteLink {
    pub fn connect(target: &str, session: Option<&str>, cols: u16, rows: u16) -> Result<Self, String> {
        let target = starcil_remote::RemoteTarget::parse(target).map_err(|e| e.to_string())?;
        let runtime_dir = starcil_platform::PlatformPaths::discover()
            .map(|p| p.runtime_dir().to_path_buf())
            .map_err(|e| e.to_string())?;
        let mut options = starcil_remote::SshConnectOptions::new(target, runtime_dir);
        if let Some(s) = session {
            options = options.with_session(s);
        }
        let hello = Hello {
            hello: HelloBody {
                protocol_major: starcil_protocol::PROTOCOL_MAJOR,
                protocol_minor: starcil_protocol::PROTOCOL_MINOR,
                version: env!("CARGO_PKG_VERSION").to_string(),
                mode: ClientMode::Tui,
                capabilities: vec![],
                cols: Some(cols),
                rows: Some(rows),
                takeover: None,
                target: None,
            },
        };
        let (out_tx, out_rx) = std::sync::mpsc::channel::<Value>();
        let (in_tx, in_rx) = std::sync::mpsc::channel::<ServerMsg>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive_flag = alive.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                let mut transport = match starcil_remote::SshTransport::connect(options).await {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                if let Ok(v) = serde_json::to_value(&hello) {
                    if transport.send(v).await.is_err() {
                        let _ = ready_tx.send(Err("handshake failed over ssh".into()));
                        return;
                    }
                }
                let _ = ready_tx.send(Ok(()));
                pump_transport(transport, out_rx, in_tx).await;
                alive_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            });
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(RemoteLink { out_tx, in_rx, alive }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("remote bridge thread died during connect".into()),
        }
    }
}

impl ServerLink for RemoteLink {
    fn send(&mut self, msg: ClientMsg) {
        let value = match msg {
            ClientMsg::Request(r) => serde_json::to_value(&r),
            ClientMsg::Input(f) => serde_json::to_value(&f),
        };
        if let Ok(v) = value {
            let _ = self.out_tx.send(v);
        }
    }

    fn drain(&mut self) -> Vec<ServerMsg> {
        self.in_rx.try_iter().collect()
    }
}

/// Run the interactive client until detach/quit. Returns the exit code.
pub fn run(session: &str) -> i32 {
    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let link = match PipeLink::connect(session, cols, rows) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("starcil: {e}");
            return 1;
        }
    };
    run_with_link(link, false)
}

/// Attach through SSH to a remote Starcil server.
pub fn run_remote(target: &str, session: Option<&str>) -> i32 {
    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let link = match RemoteLink::connect(target, session, cols, rows) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("starcil: remote attach failed: {e}");
            return 1;
        }
    };
    run_with_link(link, true)
}

fn run_with_link(link: impl ServerLink + LinkHealth, remote: bool) -> i32 {
    let path = starcil_config::config_path().or_else(starcil_config::default_config_path);
    let report = path
        .as_deref()
        .map(starcil_config::load)
        .unwrap_or_else(|| starcil_config::parse_config(""));
    let mut app = match App::new(report.config, starcil_config::HostAppearance::Dark, link) {
        Ok(a) => a.with_config_path(path),
        Err(e) => {
            eprintln!("starcil: {e}");
            return 1;
        }
    };
    // The composer's up/down history starts with what the shell already
    // remembers (PSReadLine's file on Windows, bash/zsh's elsewhere).
    app.seed_history(starcil_tui::composer::load_shell_history());
    app.set_remote_client(remote);

    // Background release check: stage the new release silently, then let
    // the menu offer the swap. Remote clients never touch local binaries.
    let update_rx = if !remote && app.config().update.version_check {
        Some(spawn_update_check(app.config().update.channel))
    } else {
        None
    };
    let mut staged_update: Option<starcil_update::StagedUpdate> = None;
    // Folder picker results arrive from a worker thread: the native dialog
    // blocks, so it must never run on the render loop.
    let (folder_tx, folder_rx) = std::sync::mpsc::channel::<Option<String>>();
    let mut folder_picker_open = false;

    if terminal::enable_raw_mode().is_err() {
        eprintln!("starcil: this command needs an interactive terminal");
        return 1;
    }
    let mut out = std::io::stdout();
    let _ = execute!(out, terminal::EnterAlternateScreen);
    let mouse_captured = app.wants_mouse_capture();
    if mouse_captured {
        let _ = execute!(out, crossterm::event::EnableMouseCapture);
    }
    let mut clipboard = ClientClipboard::new();
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut term = match ratatui::Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = terminal::disable_raw_mode();
            eprintln!("starcil: {e}");
            return 1;
        }
    };

    let debug = std::env::var_os("STARCIL_CLIENT_DEBUG").is_some();
    if debug {
        eprintln!("client: entering main loop");
    }
    // Input on a dedicated thread: crossterm's poll/read stays isolated there
    // (polling from the render loop proved unreliable under ConPTY).
    let (ev_tx, ev_rx) = std::sync::mpsc::channel::<CtEvent>();
    // Windows: own the console input queue so mouse records are translated by
    // a stuck-button-tolerant parser (hosts like Warp never forward the right
    // release); keyboard records still go through crossterm. See wininput.rs.
    #[cfg(windows)]
    crate::wininput::spawn_input_thread(ev_tx);
    #[cfg(not(windows))]
    std::thread::spawn(move || loop {
        // Blocking read: zero polling latency between keystroke and delivery.
        match event::read() {
            Ok(e) => {
                if ev_tx.send(e).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });
    let mut iterations: u64 = 0;
    let mut subscribed: Vec<String> = Vec::new();
    let mut last_fingerprint: u64 = 0;
    let mut last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let code = loop {
        iterations += 1;
        if debug && (iterations == 1 || iterations % 300 == 0) {
            eprintln!(
                "client: iter={iterations} mode={:?} snapshot={} mirrors={}",
                app.mode(),
                app.snapshot().is_some(),
                app.mirrors().len()
            );
        }
        // Drain input events delivered by the input thread; the recv timeout
        // paces the render loop.
        let mut had_input = false;
        match ev_rx.recv_timeout(std::time::Duration::from_millis(16)) {
            Ok(first) => {
                let mut pending = vec![first];
                pending.extend(ev_rx.try_iter());
                for ev in pending {
                    match ev {
                        CtEvent::Key(k) if k.kind != KeyEventKind::Release => {
                            had_input = true;
                            if debug {
                                eprintln!("key: {k:?} mode: {:?}", app.mode());
                            }
                            if let Err(e) = app.handle_key(k) {
                                eprintln!("starcil: input error: {e}");
                                break;
                            }
                        }
                        CtEvent::Mouse(m) => {
                            had_input = true;
                            let size = term.size().unwrap_or(ratatui::layout::Size { width: 120, height: 40 });
                            let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                            if let Err(e) = app.handle_mouse(m, area, &mut clipboard) {
                                tracing::debug!(error = %e, "mouse handling error");
                            }
                        }
                        CtEvent::Resize(_, _) => {
                            // The layout-area sync below reports the new pane
                            // area (terminal minus sidebar/tab bar) to the server.
                            had_input = true;
                        }
                        _ => {}
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 1,
        }
        app.poll();
        if !app.link().link_alive() {
            eprintln!("starcil: server connection lost");
            break 1;
        }
        if debug && iterations <= 2 {
            eprintln!("client: iter={iterations} step=post-app-poll");
        }

        // Keep the frame subscription in sync with the panes we can see.
        if let Some(snapshot) = app.snapshot() {
            let panes: Vec<String> = snapshot.panes.iter().map(|p| p.pane_id.clone()).collect();
            if panes != subscribed {
                subscribed = panes.clone();
                app.link_mut()
                    .send(ClientMsg::Input(InputFrame::Subscribe { pane_ids: panes }));
            }
        }

        if let Some(rx) = &update_rx {
            if let Ok(staged) = rx.try_recv() {
                app.set_update_ready(staged.release.version.to_string());
                staged_update = Some(staged);
            }
        }

        if let Ok(result) = folder_rx.try_recv() {
            folder_picker_open = false;
            app.folder_picked(result);
        }
        for effect in app.take_effects() {
            match effect {
                starcil_tui::AppEffect::OpenFolderPicker => {
                    if !folder_picker_open {
                        folder_picker_open = true;
                        let tx = folder_tx.clone();
                        let start_in = app.dock_cwd_label();
                        std::thread::spawn(move || {
                            let _ = tx.send(pick_folder(start_in));
                        });
                    }
                }
                starcil_tui::AppEffect::ApplyUpdate => {
                    let Some(staged) = staged_update.take() else {
                        continue;
                    };
                    match starcil_update::apply(&staged) {
                        Ok(_) => {
                            app.clear_update_ready();
                            app.notify(format!(
                                "Updated to {} — restart starcil to finish",
                                staged.release.version
                            ));
                        }
                        Err(error) => {
                            app.notify(format!("Update failed: {error}"));
                            tracing::warn!(error = %error, "staged update apply failed");
                        }
                    }
                }
                other => tracing::debug!(?other, "app effect"),
            }
        }
        if app.detached() {
            break 0;
        }
        let fingerprint = client_fingerprint(&app);
        let stale = last_draw.elapsed() >= std::time::Duration::from_millis(250);
        // Keep the server's layout area equal to the cells panes really get.
        // Anything that moves the sidebar or tab bar (toggle, drag, config,
        // terminal resize, tab count) changes it; the App only sends on change.
        if let Ok(size) = term.size() {
            app.sync_layout_area(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        }
        if had_input || fingerprint != last_fingerprint || stale {
            if term.draw(|f| starcil_tui::render_app(&app, f)).is_err() {
                break 1;
            }
            last_fingerprint = fingerprint;
            last_draw = std::time::Instant::now();
        }
    };

    let _ = terminal::disable_raw_mode();
    if mouse_captured {
        let _ = execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
    let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
    code
}

/// Check the release feed once in the background and stage a newer build if
/// one exists. The receiver yields at most one StagedUpdate; every failure
/// (offline, no repo, no asset) is silence — exactly like `starcil update`.
fn spawn_update_check(
    channel: starcil_config::UpdateChannel,
) -> std::sync::mpsc::Receiver<starcil_update::StagedUpdate> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(paths) = starcil_platform::PlatformPaths::discover() else {
            return;
        };
        let Some(platform) = starcil_update::Platform::current() else {
            return;
        };
        let Ok(current_executable) = std::env::current_exe() else {
            return;
        };
        let Ok(current_version) = semver::Version::parse(env!("CARGO_PKG_VERSION")) else {
            return;
        };
        let channel = match channel {
            starcil_config::UpdateChannel::Stable => starcil_update::Channel::Stable,
            starcil_config::UpdateChannel::Preview => starcil_update::Channel::Preview,
        };
        let updater = starcil_update::Updater::new(
            starcil_update::UreqHttpClient,
            starcil_update::UpdateConfig::new(paths.data_dir(), current_executable, platform),
        );
        let Ok(Some(release)) = updater.check(channel, &current_version) else {
            return;
        };
        if let Ok(staged) = updater.download_and_stage(&release) {
            let _ = tx.send(staged);
        }
    });
    rx
}

/// Cheap render fingerprint: covers server-driven changes (snapshot revision,
/// per-pane frame sequences, scroll offsets) plus toast lifecycle. Input-driven
/// changes redraw via the had_input path.
/// Blocking native folder chooser, called on a worker thread. `None` means
/// cancelled or unavailable. `STARCIL_FOLDER_PICKER=fake:<path>` short-circuits
/// for automated end-to-end tests (a real dialog cannot run headless).
fn pick_folder(start_in: Option<String>) -> Option<String> {
    match std::env::var("STARCIL_FOLDER_PICKER") {
        Ok(value) if value == "off" => return None,
        Ok(value) => {
            if let Some(path) = value.strip_prefix("fake:") {
                return Some(path.to_owned());
            }
        }
        Err(_) => {}
    }
    native_pick_folder(start_in)
}

#[cfg(any(windows, target_os = "macos"))]
fn native_pick_folder(start_in: Option<String>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a folder");
    if let Some(start) = start_in.filter(|start| std::path::Path::new(start).is_dir()) {
        dialog = dialog.set_directory(start);
    }
    dialog
        .pick_folder()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_pick_folder(start_in: Option<String>) -> Option<String> {
    // zenity first, then kdialog; both print the chosen path on stdout.
    let start = start_in.unwrap_or_default();
    let attempts: [(&str, Vec<String>); 2] = [
        (
            "zenity",
            vec![
                "--file-selection".to_owned(),
                "--directory".to_owned(),
                format!("--filename={start}"),
            ],
        ),
        ("kdialog", vec!["--getexistingdirectory".to_owned(), start.clone()]),
    ];
    for (program, args) in attempts {
        let Ok(output) = std::process::Command::new(program).args(&args).output() else {
            continue;
        };
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return Some(path);
            }
        }
        // The program exists: a failure here is a cancel, not a missing tool.
        return None;
    }
    None
}

fn client_fingerprint(app: &App<impl ServerLink>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(s) = app.snapshot() {
        s.revision.hash(&mut h);
        s.focused_pane_id.hash(&mut h);
        s.panes.len().hash(&mut h);
        for p in &s.panes {
            p.agent_status.as_str().hash(&mut h);
        }
    }
    for (id, m) in app.mirrors() {
        id.hash(&mut h);
        m.last_seq().hash(&mut h);
    }
    app.toasts().len().hash(&mut h);
    app.ui_revision().hash(&mut h);
    app.composer_focused().hash(&mut h);
    app.composer_text().hash(&mut h);
    app.composer_cursor().hash(&mut h);
    app.composer_search().map(|search| (search.query.clone(), search.found)).hash(&mut h);
    // Advances every 100ms only while an agent is working: that is what
    // redraws the sidebar spinner between input events.
    app.spinner_frame().hash(&mut h);
    app.modes().len().hash(&mut h);
    std::mem::discriminant(app.mode()).hash(&mut h);
    h.finish()
}

/// Clipboard for mouse copy flows: real arboard when available, silent no-op
/// otherwise (headless terminals).
enum ClientClipboard {
    Real(starcil_platform::ArboardClipboard),
    Noop,
}

impl ClientClipboard {
    fn new() -> Self {
        match starcil_platform::ArboardClipboard::new() {
            Ok(c) => ClientClipboard::Real(c),
            Err(_) => ClientClipboard::Noop,
        }
    }
}

impl starcil_platform::Clipboard for ClientClipboard {
    fn get_text(&mut self) -> Result<String, starcil_platform::ClipboardError> {
        match self {
            ClientClipboard::Real(c) => c.get_text(),
            ClientClipboard::Noop => Ok(String::new()),
        }
    }

    fn set_text(&mut self, text: &str) -> Result<(), starcil_platform::ClipboardError> {
        match self {
            ClientClipboard::Real(c) => c.set_text(text),
            ClientClipboard::Noop => Ok(()),
        }
    }

    fn has_image(&mut self) -> Result<bool, starcil_platform::ClipboardError> {
        match self {
            ClientClipboard::Real(c) => c.has_image(),
            ClientClipboard::Noop => Ok(false),
        }
    }
}
