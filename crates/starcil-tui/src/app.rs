use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use serde_json::{Value, json};
use starcil_config::{
    Action, AgentPanelSort, Color, Config, ConfigFileError, ConfigSetting, Diagnostic,
    EffectiveKeymap, HostAppearance, InAppToastPosition, Key, KeyChord, KeyContext, NamedKey,
    ResolvedTheme, SidebarCollapsedMode, ThemeError, ToastDelivery, ToastPosition,
    build_effective_keymap, config_path, resolve_theme, save_config_setting,
    save_onboarding_choice,
};
use starcil_domain::AgentStatus;
use starcil_platform::{Clipboard, ClipboardError};
use starcil_protocol::Incoming;
use starcil_protocol::Request;
use starcil_protocol::attach::InputFrame;
use starcil_protocol::types::SessionSnapshot;
use thiserror::Error;

use crate::composer::{
    Candidate, CompletionContext, CompletionSource, FsCompletionSource, History, LineEditor,
    complete,
};
use crate::input::key_event_to_chord;
use crate::link::{ClientMsg, ServerLink, ServerMsg};
use crate::mirror::{ApplyOutcome, PaneMirror};
use crate::dock::{detect_dock_agents, DockAgent};
use crate::mouse::{MouseAction, MouseController, UiGeometry};
use crate::scrollback::{EditorError, EditorLauncher, ScrollbackController};
use crate::selection::SelectionController;
use crate::settings::{SettingsEditor, SettingsEvent};
use crate::sound::{
    SoundController, SoundError, SoundPlayer, SoundRequest, request_for_transition,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Terminal,
    Prefix,
    Navigate,
    Resize,
    Copy,
    Modal(Modal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Help { filter: String },
    WorkspacePicker { selected: usize },
    Confirm { action: Action, index: Option<u8> },
    /// A staged update waits: yes installs it, no leaves it in the menu.
    UpdatePrompt { version: String },
    Prompt { kind: PromptKind, value: String },
    Menu { selected: usize },
    Settings,
    ContextMenu {
        target: ContextTarget,
        x: u16,
        y: u16,
        selected: usize,
    },
    Onboarding,
}

/// What a right-click landed on; each target gets its own menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextTarget {
    Pane(String),
    Tab(String),
    Workspace(String),
}

impl ContextTarget {
    pub fn id(&self) -> &str {
        match self {
            Self::Pane(id) | Self::Tab(id) | Self::Workspace(id) => id,
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Pane(_) => " Pane ",
            Self::Tab(_) => " Tab ",
            Self::Workspace(_) => " Workspace ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    CopySelection,
    CopyScreen,
    Paste,
    Rename,
    Close,
    Zoom,
    NewTab,
    NewWorkspace,
}

impl ContextMenuAction {
    /// Pane menu (kept as `ALL` for the existing call sites and tests).
    pub const ALL: [Self; 6] = [
        Self::CopySelection,
        Self::CopyScreen,
        Self::Paste,
        Self::Rename,
        Self::Close,
        Self::Zoom,
    ];
    pub const TAB: [Self; 3] = [Self::NewTab, Self::Rename, Self::Close];
    pub const WORKSPACE: [Self; 3] = [Self::NewWorkspace, Self::Rename, Self::Close];

    pub fn items(target: &ContextTarget) -> &'static [Self] {
        match target {
            ContextTarget::Pane(_) => &Self::ALL,
            ContextTarget::Tab(_) => &Self::TAB,
            ContextTarget::Workspace(_) => &Self::WORKSPACE,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CopySelection => "Copy selection",
            Self::CopyScreen => "Copy screen",
            Self::Paste => "Paste",
            Self::Rename => "Rename",
            Self::Close => "Close",
            Self::Zoom => "Zoom",
            Self::NewTab => "New tab",
            Self::NewWorkspace => "New workspace",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Settings,
    Keybinds,
    ReloadConfig,
    ApplyUpdate,
    Detach,
}

impl MenuAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Keybinds => "keybinds",
            Self::ReloadConfig => "reload config",
            Self::ApplyUpdate => "update ready",
            Self::Detach => "detach",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    NewWorkspace,
    RenameWorkspace,
    NewTab,
    RenameTab,
    RenamePane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarState {
    Expanded,
    Compact,
    Hidden,
}

/// How long an in-app toast stays on screen before it self-dismisses.
const TOAST_TTL: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastMessage {
    pub message: String,
    pub position: ToastPosition,
    pub created: std::time::Instant,
}

/// Side effects owned by the executable shell or by the explicitly deferred
/// C3 interaction modules, rather than by the server transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEffect {
    /// Open the native OS folder picker on a background thread; the client
    /// loop reports the choice back through `App::folder_picked`.
    OpenFolderPicker,
    ApplyUpdate,
    OpenNotificationTarget,
    RemoteImagePaste,
    EditScrollback,
    EnterCopyMode,
    OpenEditor { path: PathBuf },
    RunCommand { index: usize },
    /// `ui.toast.delivery = "system"`: hand the message to the OS
    /// notification service (a short-lived helper process, off the render
    /// thread).
    DesktopNotification { title: String, body: String },
    /// `ui.toast.delivery = "terminal"`: ask the outer terminal to show it
    /// (OSC 777 / OSC 9 on stdout), which also works over ssh.
    TerminalNotification { title: String, body: String },
}

/// Title of every notification handed to the OS or the outer terminal.
const NOTIFICATION_TITLE: &str = "Starcil";

/// "codex finished in beta": an agent the user is not looking at (another
/// workspace) finished or stopped to ask something. Toast-delivered by
/// `ui.toast.delivery`, so it is silent by default.
fn agent_announcement(
    snapshot: &SessionSnapshot,
    pane: &starcil_protocol::types::PaneInfo,
    previous: AgentStatus,
    next: AgentStatus,
) -> Option<String> {
    if previous == next || pane.workspace_id == snapshot.focused_workspace_id {
        return None;
    }
    let what = match next {
        AgentStatus::Done => "finished",
        AgentStatus::Blocked => "needs input",
        _ => return None,
    };
    let agent = pane
        .agent_name
        .clone()
        .or_else(|| pane.agent.clone())
        .unwrap_or_else(|| "agent".to_owned());
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == pane.workspace_id)
        .map(|workspace| workspace.label.as_str())
        .unwrap_or(pane.workspace_id.as_str());
    Some(format!("{agent} {what} in {workspace}"))
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Theme(#[from] ThemeError),
    #[error(transparent)]
    Config(#[from] ConfigFileError),
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
    #[error(transparent)]
    Editor(#[from] EditorError),
}

/// One Tab press computed these; repeated presses walk `candidates` as long
/// as the line still reads what the last press wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionCycle {
    pane_id: String,
    start: usize,
    end: usize,
    candidates: Vec<Candidate>,
    index: usize,
    text: String,
    cursor: usize,
}

/// ctrl+r in the composer: the query typed so far and whether it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pane_id: String,
    pub query: String,
    pub found: bool,
    /// The draft before the search began (ctrl+c / ctrl+g bring it back).
    stash: LineEditor,
    /// Index of the current hit in the history (ctrl+r again goes older).
    hit: Option<usize>,
}

pub struct App<L: ServerLink> {
    link: L,
    config: Config,
    appearance: HostAppearance,
    theme: ResolvedTheme,
    keymap: EffectiveKeymap,
    keymap_diagnostics: Vec<Diagnostic>,
    snapshot: Option<SessionSnapshot>,
    mirrors: BTreeMap<String, PaneMirror>,
    modes: Vec<Mode>,
    sidebar: SidebarState,
    local_zoom: Option<String>,
    last_pane_id: Option<String>,
    request_seq: u64,
    detached: bool,
    remote_client: bool,
    update_ready: Option<String>,
    /// Version the update prompt already asked about (asks once per version).
    update_prompted: Option<String>,
    toasts: Vec<ToastMessage>,
    effects: Vec<AppEffect>,
    mouse: MouseController,
    mouse_debug: bool,
    last_mouse_event: Option<MouseEvent>,
    mouse_event_counter: u64,
    /// Explicit object for the open rename prompt (context menus); `None`
    /// means the focused one, as for keyboard-driven prompts.
    prompt_target: Option<String>,
    /// Layout area last reported to the server (`ClientArea`), so PTYs are
    /// sized to the cells panes really get, not the whole terminal.
    reported_layout_area: Option<(u16, u16)>,
    /// The in-pane composer's draft, PER PANE: every shell pane keeps its own
    /// line editor (text + cursor), nothing is shared across tabs (Cesar's
    /// report). A shell pane's composer owns the keyboard whenever the shell
    /// sits at its prompt — typing lands below, never in the prompt at the
    /// top; a program running under the shell takes the keys instead.
    composers: BTreeMap<String, LineEditor>,
    /// Command history shared by every composer; the client seeds it from
    /// the shell's own history file.
    history: History,
    /// Tab cycling: the candidates computed for the token under the cursor.
    /// Any other key drops it.
    completion: Option<CompletionCycle>,
    /// ctrl+r incremental history search in the focused composer.
    search: Option<SearchState>,
    /// Filesystem and PATH behind Tab; tests inject a fake.
    completion_source: Box<dyn CompletionSource>,
    /// Clock for the sidebar's working spinner.
    started: std::time::Instant,
    spinner_frame: usize,
    /// Agent launchers detected on PATH (ui.dock.agents order).
    dock_agents: Vec<DockAgent>,
    /// Folder chosen in the picker, per pane: that pane's cwd label.
    dock_cwd: BTreeMap<String, String>,
    /// Last `ReserveRows` sent to the server: (pane, rows). The in-pane
    /// composer cedes PTY rows; this keeps the server in sync as focus and
    /// agent state move around.
    reported_reservation: Option<(String, u16)>,
    /// Redraw signal for state changed outside input handling (folder picks).
    ui_revision: u64,
    selection: SelectionController,
    scrollback: ScrollbackController,
    settings: SettingsEditor,
    sound_requests: Vec<SoundRequest>,
    config_path: Option<PathBuf>,
}

impl<L: ServerLink> App<L> {
    pub fn new(config: Config, appearance: HostAppearance, link: L) -> Result<Self, AppError> {
        let keymap_build = build_effective_keymap(&config.keys);
        let theme = resolve_app_theme(&config, appearance)?;
        let sidebar = if config.ui.sidebar_start_collapsed {
            match config.ui.sidebar_collapsed_mode {
                SidebarCollapsedMode::Compact => SidebarState::Compact,
                SidebarCollapsedMode::Hidden => SidebarState::Hidden,
            }
        } else {
            SidebarState::Expanded
        };
        let dock_agents = detect_dock_agents(&config.ui.dock.agents);
        let mut modes = vec![Mode::Terminal];
        if config.should_show_onboarding() {
            modes.push(Mode::Modal(Modal::Onboarding));
        }
        let mouse_debug = matches!(
            std::env::var("STARCIL_MOUSE_DEBUG").as_deref(),
            Ok("1")
        );
        Ok(Self {
            link,
            config,
            appearance,
            theme,
            keymap: keymap_build.keymap,
            keymap_diagnostics: keymap_build.diagnostics,
            snapshot: None,
            mirrors: BTreeMap::new(),
            modes,
            sidebar,
            local_zoom: None,
            last_pane_id: None,
            request_seq: 0,
            detached: false,
            remote_client: false,
            update_ready: None,
            update_prompted: None,
            toasts: Vec::new(),
            effects: Vec::new(),
            mouse: MouseController::default(),
            mouse_debug,
            last_mouse_event: None,
            mouse_event_counter: 0,
            prompt_target: None,
            reported_layout_area: None,
            composers: BTreeMap::new(),
            history: History::default(),
            completion: None,
            search: None,
            completion_source: Box::new(FsCompletionSource::default()),
            started: std::time::Instant::now(),
            spinner_frame: 0,
            dock_agents,
            dock_cwd: BTreeMap::new(),
            reported_reservation: None,
            ui_revision: 0,
            selection: SelectionController::default(),
            scrollback: ScrollbackController::default(),
            settings: SettingsEditor::default(),
            sound_requests: Vec::new(),
            config_path: config_path(),
        })
    }

    pub fn with_config_path(mut self, path: Option<PathBuf>) -> Self {
        self.config_path = path;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn theme(&self) -> &ResolvedTheme {
        &self.theme
    }

    pub fn keymap(&self) -> &EffectiveKeymap {
        &self.keymap
    }

    pub fn keymap_diagnostics(&self) -> &[Diagnostic] {
        &self.keymap_diagnostics
    }

    pub fn snapshot(&self) -> Option<&SessionSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn mirror(&self, pane_id: &str) -> Option<&PaneMirror> {
        self.mirrors.get(pane_id)
    }

    pub fn mirrors(&self) -> &BTreeMap<String, PaneMirror> {
        &self.mirrors
    }

    pub fn mode(&self) -> &Mode {
        self.modes.last().expect("terminal mode is always present")
    }

    pub fn modes(&self) -> &[Mode] {
        &self.modes
    }

    pub fn sidebar_state(&self) -> SidebarState {
        self.sidebar
    }

    pub fn local_zoom(&self) -> Option<&str> {
        self.local_zoom.as_deref()
    }

    pub fn detached(&self) -> bool {
        self.detached
    }

    pub fn set_remote_client(&mut self, remote: bool) {
        self.remote_client = remote;
    }

    pub fn is_remote_client(&self) -> bool {
        self.remote_client
    }

    pub fn set_update_ready(&mut self, version: impl Into<String>) {
        self.update_ready = Some(version.into());
    }

    pub fn update_ready(&self) -> Option<&str> {
        self.update_ready.as_deref()
    }

    /// The staged update was applied: drop the menu row.
    pub fn clear_update_ready(&mut self) {
        self.update_ready = None;
    }

    /// Launcher row under the pointer, for hover feedback.
    pub fn hovered_dock(&self) -> Option<crate::mouse::DockHover> {
        self.mouse.hovered_dock()
    }

    /// Pointer over the sidebar's sections divider.
    pub fn hovered_split(&self) -> bool {
        self.mouse.hovered_split()
    }

    /// Spinner frame to shimmer the `agents` header with, while any agent
    /// is working; `None` when everything is idle.
    pub fn agents_shimmer(&self) -> Option<usize> {
        let working = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .agents
                .iter()
                .any(|agent| agent.agent_status == AgentStatus::Working)
        });
        working.then_some(self.spinner_frame)
    }

    pub fn notify(&mut self, message: impl Into<String>) {
        self.push_toast(message);
    }

    pub(crate) fn menu_actions(&self) -> Vec<MenuAction> {
        let mut actions = vec![
            MenuAction::Settings,
            MenuAction::Keybinds,
            MenuAction::ReloadConfig,
        ];
        if self.update_ready().is_some() {
            actions.push(MenuAction::ApplyUpdate);
        }
        actions.push(MenuAction::Detach);
        actions
    }

    /// The chrome element under the mouse pointer (tab, `+`, sidebar row),
    /// for hover feedback in the renderer.
    /// The tab being dragged along the tab bar, if any.
    pub fn dragging_tab(&self) -> Option<&str> {
        self.mouse.dragging_tab()
    }

    pub fn hovered_chrome(&self) -> Option<&crate::mouse::ChromeTarget> {
        self.mouse.hovered()
    }

    pub fn toasts(&self) -> &[ToastMessage] {
        &self.toasts
    }

    pub fn effects(&self) -> &[AppEffect] {
        &self.effects
    }

    pub fn selection(&self) -> &SelectionController {
        &self.selection
    }

    pub fn scrollback(&self) -> &ScrollbackController {
        &self.scrollback
    }

    pub fn settings(&self) -> &SettingsEditor {
        &self.settings
    }

    pub fn wants_mouse_capture(&self) -> bool {
        self.config.ui.mouse_capture
    }

    pub fn set_mouse_debug(&mut self, on: bool) {
        self.mouse_debug = on;
        if !on {
            self.last_mouse_event = None;
            self.mouse_event_counter = 0;
        }
    }

    pub(crate) fn mouse_debug_line(&self) -> Option<String> {
        let event = self.last_mouse_event.as_ref()?;
        let modifiers = if event.modifiers.is_empty() {
            "NONE".to_owned()
        } else {
            format!("{:?}", event.modifiers)
        };
        Some(format!(
            "mouse: {:?} {},{} mods={} #{}",
            event.kind, event.column, event.row, modifiers, self.mouse_event_counter
        ))
    }

    pub fn take_effects(&mut self) -> Vec<AppEffect> {
        std::mem::take(&mut self.effects)
    }

    pub fn take_sound_requests(&mut self) -> Vec<SoundRequest> {
        std::mem::take(&mut self.sound_requests)
    }

    pub fn play_pending_sounds<P: SoundPlayer>(
        &mut self,
        controller: &mut SoundController<P>,
    ) -> Vec<SoundError> {
        controller.play_all(self.take_sound_requests())
    }

    pub fn launch_pending_editors<E: EditorLauncher>(
        &mut self,
        editor: &mut E,
    ) -> Result<usize, AppError> {
        let mut retained = Vec::with_capacity(self.effects.len());
        let mut opened = 0;
        for effect in std::mem::take(&mut self.effects) {
            match effect {
                AppEffect::OpenEditor { path } => {
                    editor.open(&path)?;
                    opened += 1;
                }
                effect => retained.push(effect),
            }
        }
        self.effects = retained;
        Ok(opened)
    }

    pub fn link(&self) -> &L {
        &self.link
    }

    pub fn link_mut(&mut self) -> &mut L {
        &mut self.link
    }

    pub fn set_snapshot(&mut self, snapshot: SessionSnapshot) {
        let previous_terminal_ids = self
            .snapshot
            .as_ref()
            .map(|previous| {
                previous
                    .panes
                    .iter()
                    .map(|pane| (pane.pane_id.clone(), pane.terminal_id.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        if let Some(previous) = &self.snapshot {
            if previous.focused_pane_id != snapshot.focused_pane_id {
                self.last_pane_id = Some(previous.focused_pane_id.clone());
            }
        }
        for pane in &snapshot.panes {
            if previous_terminal_ids
                .get(&pane.pane_id)
                .is_some_and(|terminal_id| terminal_id != &pane.terminal_id)
            {
                self.mirrors
                    .insert(pane.pane_id.clone(), PaneMirror::new(&pane.pane_id));
            } else {
                self.mirrors
                    .entry(pane.pane_id.clone())
                    .or_insert_with(|| PaneMirror::new(&pane.pane_id));
            }
        }
        self.mirrors
            .retain(|pane_id, _| snapshot.panes.iter().any(|pane| &pane.pane_id == pane_id));
        // Per-pane composer state dies with its pane.
        let exists = |pane_id: &str| snapshot.panes.iter().any(|pane| pane.pane_id == pane_id);
        self.composers.retain(|pane_id, _| exists(pane_id));
        self.dock_cwd.retain(|pane_id, _| exists(pane_id));
        if let Some(previous) = &self.snapshot {
            // A picked folder previews the cwd until the shell reports where
            // it really went.
            for pane in &snapshot.panes {
                if previous
                    .panes
                    .iter()
                    .any(|before| before.pane_id == pane.pane_id && before.cwd != pane.cwd)
                {
                    self.dock_cwd.remove(&pane.pane_id);
                }
            }
            if previous.focused_pane_id != snapshot.focused_pane_id {
                self.completion = None;
                self.search = None;
            }
        }
        self.snapshot = Some(snapshot);
    }

    /// `pane.shell_idle_changed`: the shell reached its prompt (`idle: true`)
    /// or started a program (`false`). Applied locally, no snapshot refetch.
    fn apply_shell_idle(&mut self, data: &Value) {
        let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) else {
            return;
        };
        let idle = data.get("idle").and_then(Value::as_bool);
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter_mut().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        pane.shell_idle = idle;
    }

    /// `pane.cwd_changed`: the shell moved (a `cd` the user typed); the
    /// composer's label and its completions follow. Applied locally.
    fn apply_cwd_changed(&mut self, data: &Value) {
        let (Some(pane_id), Some(cwd)) = (
            data.get("pane_id").and_then(Value::as_str),
            data.get("cwd").and_then(Value::as_str),
        ) else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter_mut().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        if pane.cwd == cwd {
            return;
        }
        pane.cwd = cwd.to_owned();
        // The picked folder was a preview of exactly this.
        self.dock_cwd.remove(pane_id);
        self.completion = None;
        self.ui_revision = self.ui_revision.saturating_add(1);
    }

    pub fn poll(&mut self) {
        self.toasts
            .retain(|toast| toast.created.elapsed() < TOAST_TTL);
        self.spinner_frame = (self.started.elapsed().as_millis() / 100) as usize
            % crate::render::SPINNER_FRAMES.len();
        // A staged update asks once per version, when nothing else is open;
        // the menu's "update ready" row asks again later.
        if let Some(version) = self.update_ready.clone() {
            if self.update_prompted.as_ref() != Some(&version)
                && matches!(self.mode(), Mode::Terminal)
            {
                self.update_prompted = Some(version.clone());
                self.modes
                    .push(Mode::Modal(Modal::UpdatePrompt { version }));
            }
        }
        let messages = self.link.drain();
        for message in messages {
            match message {
                ServerMsg::SessionSnapshot(snapshot) => self.set_snapshot(snapshot),
                ServerMsg::TerminalFrame(frame) => {
                    let pane_id = frame.pane_id.clone();
                    let outcome = self
                        .mirrors
                        .entry(pane_id.clone())
                        .or_insert_with(|| PaneMirror::new(&pane_id))
                        .apply(&frame);
                    if let Some(scroll) = self
                        .mirrors
                        .get(&pane_id)
                        .and_then(PaneMirror::scroll)
                    {
                        self.scrollback.set_offset(
                            pane_id.clone(),
                            scroll.offset_from_bottom,
                            scroll.max_offset_from_bottom,
                        );
                    }
                    if outcome == ApplyOutcome::ResyncRequired {
                        self.link.send(ClientMsg::Input(InputFrame::Resync { pane_id }));
                    }
                }
                ServerMsg::Incoming(incoming) => self.handle_incoming(incoming),
            }
        }
    }

    fn handle_incoming(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Success(success) => {
                let Some(text) = success.result.get("text").and_then(Value::as_str) else {
                    return;
                };
                match self.scrollback.complete_read(&success.id, text) {
                    Ok(Some(document)) => self
                        .effects
                        .push(AppEffect::OpenEditor { path: document.path }),
                    Ok(None) => {}
                    Err(error) => self.push_toast(error.to_string()),
                }
            }
            Incoming::Error(error) => {
                self.push_toast(format!("Request failed: {}", error.error.message));
            }
            Incoming::Event(event) => self.handle_event(&event.event, &event.data),
        }
    }

    pub fn handle_key(&mut self, event: KeyEvent) -> Result<(), AppError> {
        self.handle_key_inner(event, None)
    }

    pub fn handle_key_with_clipboard<C: Clipboard>(
        &mut self,
        event: KeyEvent,
        clipboard: &mut C,
    ) -> Result<(), AppError> {
        self.handle_key_inner(event, Some(clipboard))
    }

    fn handle_key_inner(
        &mut self,
        event: KeyEvent,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        let Some(chord) = key_event_to_chord(&event) else {
            return Ok(());
        };
        if matches!(self.mode(), Mode::Terminal) && Self::is_altgr_text_event(event) {
            if self.composer_focused() {
                if let KeyCode::Char(character) = event.code {
                    self.composer_insert_text(&character.to_string());
                }
                return Ok(());
            }
            self.forward_to_pane(event, chord);
            return Ok(());
        }
        if is_copy_shortcut(event)
            && matches!(self.mode(), Mode::Terminal | Mode::Copy)
            && clipboard.is_some()
        {
            let pane_id = self.focused_pane_id();
            if let (Some(pane_id), Some(clipboard)) = (pane_id, clipboard) {
                self.copy_selection(&pane_id, clipboard)?;
            }
            return Ok(());
        }
        match self.mode().clone() {
            Mode::Modal(modal) => {
                self.handle_modal(modal, &event, chord, clipboard)?;
                // Modal keys are consumed here even when the handler closes the
                // modal. They must never be reconsidered as terminal input.
                Ok(())
            }
            Mode::Navigate => self.handle_navigate(chord),
            Mode::Resize => self.handle_resize(chord),
            Mode::Prefix => self.handle_prefix(chord),
            Mode::Copy => self.handle_copy(chord, clipboard),
            Mode::Terminal => self.handle_terminal(event, chord, clipboard),
        }
    }

    pub fn dispatch_action(&mut self, action: Action, index: Option<u8>) -> Result<(), AppError> {
        self.dispatch_action_inner(action, index, false)
    }

    fn handle_terminal(
        &mut self,
        event: KeyEvent,
        chord: KeyChord,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        if chord.key == Key::Named(NamedKey::Esc) {
            if let Some(pane_id) = self.focused_pane_id() {
                if self.scrollback.offset(&pane_id) > 0 {
                    self.snap_to_live(&pane_id);
                    return Ok(());
                }
            }
        }
        if let Some(bound) = self.keymap.binding(KeyContext::Terminal, &chord).copied() {
            if bound.action == Action::RemoteImagePaste && !self.remote_client {
                if let Some(clipboard) = clipboard {
                    self.paste_from_clipboard(self.focused_pane_id(), clipboard)?;
                    return Ok(());
                }
                if self.composer_focused() {
                    // No clipboard on this path: nothing to paste, and the
                    // chord must not reach the prompt above.
                    return Ok(());
                }
            }
            if bound.action == Action::RemoteImagePaste && !self.remote_client {
                self.forward_to_pane(event, chord);
                return Ok(());
            }
            return self.dispatch_action(bound.action, bound.index);
        }
        if chord == self.keymap.prefix {
            self.modes.push(Mode::Prefix);
            return Ok(());
        }
        if self.composer_focused() {
            return self.handle_composer_key(event, chord);
        }
        self.forward_to_pane(event, chord);
        Ok(())
    }

    fn handle_prefix(&mut self, chord: KeyChord) -> Result<(), AppError> {
        self.exit_mode();
        if chord == self.keymap.prefix {
            if let Some(pane_id) = self.focused_pane_id() {
                self.link.send(ClientMsg::Input(InputFrame::Keys {
                    pane_id,
                    keys: vec![self.keymap.prefix.to_string()],
                }));
            }
            return Ok(());
        }
        if chord.key == Key::Named(NamedKey::Esc) {
            return Ok(());
        }
        let prefixed = KeyChord {
            requires_prefix: true,
            ..chord
        };
        if let Some(bound) = self.keymap.binding(KeyContext::Terminal, &prefixed).copied() {
            self.dispatch_action(bound.action, bound.index)?;
        }
        Ok(())
    }

    fn handle_navigate(&mut self, chord: KeyChord) -> Result<(), AppError> {
        if matches!(chord.key, Key::Named(NamedKey::Esc | NamedKey::Enter)) {
            self.exit_mode();
            return Ok(());
        }
        let alias = match chord.key {
            Key::Named(NamedKey::Left) => Some(Action::NavigatePaneLeft),
            Key::Named(NamedKey::Right) => Some(Action::NavigatePaneRight),
            _ => None,
        };
        if let Some(action) = alias {
            return self.dispatch_action(action, None);
        }
        if let Some(bound) = self.keymap.binding(KeyContext::Navigate, &chord).copied() {
            self.dispatch_action(bound.action, bound.index)?;
        }
        Ok(())
    }

    fn handle_resize(&mut self, chord: KeyChord) -> Result<(), AppError> {
        if chord.key == Key::Named(NamedKey::Esc) {
            self.exit_mode();
            return Ok(());
        }
        let direction = direction_for_chord(chord);
        if let Some(direction) = direction {
            self.request(
                "pane.resize",
                json!({"pane_id": self.focused_pane_id(), "direction": direction, "amount": 0.05}),
            );
        }
        Ok(())
    }

    fn handle_modal(
        &mut self,
        modal: Modal,
        event: &KeyEvent,
        chord: KeyChord,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        match modal {
            Modal::Help { mut filter } => match event.code {
                KeyCode::Esc => self.exit_mode(),
                KeyCode::Backspace => {
                    filter.pop();
                    self.replace_modal(Modal::Help { filter });
                }
                KeyCode::Char('q') if filter.is_empty() => self.exit_mode(),
                KeyCode::Char('/') if filter.is_empty() => {}
                KeyCode::Char(character)
                    if !event.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    filter.push(character);
                    self.replace_modal(Modal::Help { filter });
                }
                _ => {}
            },
            Modal::WorkspacePicker { mut selected } => {
                let workspace_count = self
                    .snapshot
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.workspaces.len());
                match chord.key {
                    Key::Named(NamedKey::Esc) => self.exit_mode(),
                    Key::Named(NamedKey::Up) | Key::Character('k') => {
                        selected = selected.saturating_sub(1);
                        self.replace_modal(Modal::WorkspacePicker { selected });
                    }
                    Key::Named(NamedKey::Down) | Key::Character('j') => {
                        selected = (selected + 1).min(workspace_count.saturating_sub(1));
                        self.replace_modal(Modal::WorkspacePicker { selected });
                    }
                    Key::Named(NamedKey::Enter) => {
                        let workspace_id = self
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.workspaces.get(selected))
                            .map(|workspace| workspace.workspace_id.clone());
                        self.exit_mode();
                        if let Some(workspace_id) = workspace_id {
                            self.request("workspace.focus", json!({"workspace_id": workspace_id}));
                        }
                    }
                    _ => {}
                }
            }
            Modal::Confirm { action, index } => match chord.key {
                Key::Named(NamedKey::Esc) | Key::Character('n') => self.exit_mode(),
                Key::Named(NamedKey::Enter) | Key::Character('y') => {
                    self.exit_mode();
                    self.dispatch_action_inner(action, index, true)?;
                }
                _ => {}
            },
            Modal::UpdatePrompt { .. } => match chord.key {
                Key::Named(NamedKey::Esc) | Key::Character('n') => self.exit_mode(),
                Key::Named(NamedKey::Enter) | Key::Character('y') => {
                    self.exit_mode();
                    self.effects.push(AppEffect::ApplyUpdate);
                }
                _ => {}
            },
            Modal::Prompt { kind, mut value } => match event.code {
                KeyCode::Esc => self.exit_mode(),
                KeyCode::Backspace => {
                    value.pop();
                    self.replace_modal(Modal::Prompt { kind, value });
                }
                KeyCode::Enter => {
                    self.exit_mode();
                    if !value.trim().is_empty() {
                        self.submit_prompt(kind, value);
                    }
                }
                KeyCode::Char(character)
                    if !event.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    value.push(character);
                    self.replace_modal(Modal::Prompt { kind, value });
                }
                _ => {}
            },
            Modal::Menu { mut selected } => {
                let item_count = self.menu_actions().len();
                match chord.key {
                    Key::Named(NamedKey::Esc) => self.exit_mode(),
                    Key::Named(NamedKey::Up) | Key::Character('k') => {
                        selected = selected.saturating_sub(1);
                        self.replace_modal(Modal::Menu { selected });
                    }
                    Key::Named(NamedKey::Down) | Key::Character('j') => {
                        selected = (selected + 1).min(item_count.saturating_sub(1));
                        self.replace_modal(Modal::Menu { selected });
                    }
                    Key::Named(NamedKey::Enter) => self.run_menu_action(selected)?,
                    _ => {}
                }
            }
            Modal::Settings => self.handle_settings_key(*event)?,
            Modal::ContextMenu {
                target,
                x,
                y,
                mut selected,
            } => {
                let items = ContextMenuAction::items(&target);
                match chord.key {
                    Key::Named(NamedKey::Esc) => self.exit_mode(),
                    Key::Named(NamedKey::Up) | Key::Character('k') => {
                        selected = selected.saturating_sub(1);
                        self.replace_modal(Modal::ContextMenu {
                            target,
                            x,
                            y,
                            selected,
                        });
                    }
                    Key::Named(NamedKey::Down) | Key::Character('j') => {
                        selected = (selected + 1).min(items.len() - 1);
                        self.replace_modal(Modal::ContextMenu {
                            target,
                            x,
                            y,
                            selected,
                        });
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.exit_mode();
                        self.run_context_action(items[selected], &target, clipboard)?;
                    }
                    _ => {}
                }
            }
            Modal::Onboarding => {
                let delivery = match event.code {
                    KeyCode::Char('1') => Some(ToastDelivery::Starcil),
                    KeyCode::Char('2') => Some(ToastDelivery::Terminal),
                    KeyCode::Char('3') => Some(ToastDelivery::System),
                    KeyCode::Char('4') => Some(ToastDelivery::Off),
                    _ => None,
                };
                if let Some(delivery) = delivery {
                    self.complete_onboarding(delivery)?;
                }
            }
        }
        Ok(())
    }

    fn handle_settings_key(&mut self, event: KeyEvent) -> Result<(), AppError> {
        let previous = self.config.clone();
        match self.settings.handle_key(event, &mut self.config) {
            SettingsEvent::None => {}
            SettingsEvent::Close => self.exit_mode(),
            SettingsEvent::Changed(setting) => {
                if let Err(error) = self.apply_setting(&setting) {
                    self.config = previous;
                    self.theme = resolve_app_theme(&self.config, self.appearance)?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn run_menu_action(&mut self, index: usize) -> Result<(), AppError> {
        let Some(action) = self.menu_actions().get(index).copied() else {
            return Ok(());
        };
        self.exit_mode();
        match action {
            MenuAction::Settings => self.dispatch_action(Action::Settings, None)?,
            MenuAction::Keybinds => self.dispatch_action(Action::Help, None)?,
            MenuAction::ReloadConfig => self.dispatch_action(Action::ReloadConfig, None)?,
            MenuAction::ApplyUpdate => {
                if let Some(version) = self.update_ready.clone() {
                    self.modes
                        .push(Mode::Modal(Modal::UpdatePrompt { version }));
                }
            }
            MenuAction::Detach => self.dispatch_action(Action::Detach, None)?,
        }
        Ok(())
    }

    fn handle_copy(
        &mut self,
        chord: KeyChord,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        let Some(state) = self.scrollback.copy_state().cloned() else {
            self.exit_mode();
            return Ok(());
        };
        let (rows, cols) = self
            .mirrors
            .get(&state.pane_id)
            .map_or((0, 0), |mirror| (mirror.rows(), mirror.cols()));
        let movement = match chord.key {
            Key::Named(NamedKey::Left) | Key::Character('h') => Some((0, -1)),
            Key::Named(NamedKey::Down) | Key::Character('j') => Some((1, 0)),
            Key::Named(NamedKey::Up) | Key::Character('k') => Some((-1, 0)),
            Key::Named(NamedKey::Right) | Key::Character('l') => Some((0, 1)),
            _ => None,
        };
        if let Some((row_delta, col_delta)) = movement {
            self.scrollback.move_copy_cursor(
                row_delta,
                col_delta,
                rows,
                cols,
                &mut self.selection,
            );
            return Ok(());
        }
        match chord.key {
            Key::Named(NamedKey::Esc) => {
                self.leave_copy_mode();
            }
            Key::Character('v') | Key::Character(' ') => {
                self.scrollback
                    .toggle_copy_selection(&mut self.selection);
            }
            Key::Character('y') | Key::Named(NamedKey::Enter) => {
                if let Some(clipboard) = clipboard {
                    if self.copy_selection(&state.pane_id, clipboard)? {
                        self.leave_copy_mode();
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn submit_prompt(&mut self, kind: PromptKind, value: String) {
        let target = self.prompt_target.take();
        match kind {
            PromptKind::NewWorkspace => self.request("workspace.create", json!({"label": value})),
            PromptKind::RenameWorkspace => self.request(
                "workspace.rename",
                json!({"workspace_id": target.or_else(|| self.focused_workspace_id()), "label": value}),
            ),
            // `focus`: a tab you just named is the one you want to be in.
            PromptKind::NewTab => self.request(
                "tab.create",
                json!({"workspace_id": self.focused_workspace_id(), "label": value, "focus": true}),
            ),
            PromptKind::RenameTab => self.request(
                "tab.rename",
                json!({"tab_id": target.or_else(|| self.focused_tab_id()), "label": value}),
            ),
            PromptKind::RenamePane => self.request(
                "pane.rename",
                json!({"pane_id": target.or_else(|| self.focused_pane_id()), "label": value}),
            ),
        }
    }

    fn dispatch_action_inner(
        &mut self,
        action: Action,
        index: Option<u8>,
        confirmed: bool,
    ) -> Result<(), AppError> {
        match action {
            Action::Help => self.modes.push(Mode::Modal(Modal::Help {
                filter: String::new(),
            })),
            Action::Settings => self.modes.push(Mode::Modal(Modal::Settings)),
            Action::Detach => {
                self.detached = true;
            }
            Action::ReloadConfig => self.request("server.reload_config", json!({})),
            Action::OpenNotificationTarget => self.effects.push(AppEffect::OpenNotificationTarget),
            Action::WorkspacePicker => self
                .modes
                .push(Mode::Modal(Modal::WorkspacePicker { selected: 0 })),
            Action::Goto => self.modes.push(Mode::Navigate),
            Action::NewWorkspace => {
                if self.config.ui.prompt_new_workspace_name {
                    self.prompt(PromptKind::NewWorkspace);
                } else {
                    self.request("workspace.create", json!({}));
                }
            }
            Action::NewWorktree => self.request(
                "worktree.create",
                json!({"workspace_id": self.focused_workspace_id()}),
            ),
            Action::OpenWorktree => self.request(
                "worktree.open",
                json!({"workspace_id": self.focused_workspace_id()}),
            ),
            Action::RemoveWorktree if !confirmed => self.confirm(action, index),
            Action::RemoveWorktree => self.request(
                "worktree.remove",
                json!({"workspace_id": self.focused_workspace_id()}),
            ),
            Action::RenameWorkspace => self.prompt(PromptKind::RenameWorkspace),
            Action::CloseWorkspace if self.config.ui.confirm_close && !confirmed => {
                self.confirm(action, index)
            }
            Action::CloseWorkspace => self.request(
                "workspace.close",
                json!({"workspace_id": self.focused_workspace_id()}),
            ),
            Action::PreviousWorkspace => self.focus_relative_workspace(-1),
            Action::NextWorkspace => self.focus_relative_workspace(1),
            Action::PreviousAgent => self.focus_relative_agent(-1),
            Action::NextAgent => self.focus_relative_agent(1),
            Action::FocusAgent => self.focus_agent_index(index),
            Action::RemoteImagePaste => self.effects.push(AppEffect::RemoteImagePaste),
            Action::NewTab => {
                if self.config.ui.prompt_new_tab_name {
                    let label = self.next_tab_label(None);
                    self.prompt_with(PromptKind::NewTab, label);
                } else {
                    self.request(
                        "tab.create",
                        json!({"workspace_id": self.focused_workspace_id(), "focus": true}),
                    );
                }
            }
            Action::RenameTab => self.prompt(PromptKind::RenameTab),
            Action::PreviousTab => self.focus_relative_tab(-1),
            Action::NextTab => self.focus_relative_tab(1),
            Action::SwitchTab => self.focus_tab_index(index),
            Action::SwitchWorkspace => self.focus_workspace_index(index),
            Action::CloseTab => self.request(
                "tab.close",
                json!({"tab_id": self.focused_tab_id()}),
            ),
            Action::RenamePane => self.prompt(PromptKind::RenamePane),
            Action::EditScrollback => {
                if let Some(pane_id) = self.focused_pane_id() {
                    let request_id = self.request_with_id(
                        "pane.read",
                        json!({
                            "pane_id": pane_id,
                            "source": "recent-unwrapped",
                            "lines": 10000,
                            "format": "text"
                        }),
                    );
                    self.scrollback.register_read(request_id, pane_id);
                }
            }
            Action::CopyMode => {
                if let Some(pane_id) = self.focused_pane_id() {
                    let rows = self.mirror(&pane_id).map_or(0, PaneMirror::rows);
                    self.selection.clear();
                    self.scrollback.enter_copy(pane_id, rows);
                    self.modes.push(Mode::Copy);
                }
            }
            Action::FocusPaneLeft | Action::NavigatePaneLeft => self.focus_pane("left"),
            Action::FocusPaneDown | Action::NavigatePaneDown => self.focus_pane("down"),
            Action::FocusPaneUp | Action::NavigatePaneUp => self.focus_pane("up"),
            Action::FocusPaneRight | Action::NavigatePaneRight => self.focus_pane("right"),
            Action::SwapPaneLeft => self.swap_pane("left"),
            Action::SwapPaneDown => self.swap_pane("down"),
            Action::SwapPaneUp => self.swap_pane("up"),
            Action::SwapPaneRight => self.swap_pane("right"),
            Action::CyclePaneNext => self.focus_relative_pane(1),
            Action::CyclePanePrevious => self.focus_relative_pane(-1),
            Action::LastPane => self.focus_last_pane(),
            Action::SplitVertical => self.request(
                "pane.split",
                json!({"pane_id": self.focused_pane_id(), "direction": "right"}),
            ),
            Action::SplitHorizontal => self.request(
                "pane.split",
                json!({"pane_id": self.focused_pane_id(), "direction": "down"}),
            ),
            Action::ClosePane => self.request(
                "pane.close",
                json!({"pane_id": self.focused_pane_id()}),
            ),
            Action::Zoom => {
                let focused = self.focused_pane_id();
                self.local_zoom = if self.local_zoom.as_deref() == focused.as_deref() {
                    None
                } else {
                    focused.clone()
                };
                self.request("pane.zoom", json!({"pane_id": focused}));
            }
            Action::ResizeMode => self.modes.push(Mode::Resize),
            Action::FocusInput => {
                // The composer owns the keyboard whenever it shows; the
                // binding brings the live bottom (input included) back into
                // view for a user lost in the scrollback.
                if let Some(pane_id) = crate::mouse::composer_pane_id(self) {
                    self.snap_to_live(&pane_id);
                }
            }
            Action::OpenFolder => self.effects.push(AppEffect::OpenFolderPicker),
            Action::DockAgent => {
                if let Some(index) = index {
                    self.launch_dock_agent(usize::from(index).saturating_sub(1));
                }
            }
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::NavigateWorkspaceUp => self.focus_relative_workspace(-1),
            Action::NavigateWorkspaceDown => self.focus_relative_workspace(1),
            Action::CustomCommand(command_index) => self
                .effects
                .push(AppEffect::RunCommand { index: command_index }),
        }
        Ok(())
    }

    fn focus_pane(&mut self, direction: &str) {
        self.request(
            "pane.focus_direction",
            json!({"pane_id": self.focused_pane_id(), "direction": direction}),
        );
    }

    fn swap_pane(&mut self, direction: &str) {
        self.request(
            "pane.swap",
            json!({"pane_id": self.focused_pane_id(), "direction": direction}),
        );
    }

    fn focus_relative_workspace(&mut self, delta: isize) {
        let target = self.snapshot.as_ref().and_then(|snapshot| {
            let current = snapshot
                .workspaces
                .iter()
                .position(|workspace| workspace.workspace_id == snapshot.focused_workspace_id)?;
            let index = wrapped_index(current, snapshot.workspaces.len(), delta)?;
            Some(snapshot.workspaces[index].workspace_id.clone())
        });
        if let Some(workspace_id) = target {
            self.request("workspace.focus", json!({"workspace_id": workspace_id}));
        }
    }

    fn focus_workspace_index(&mut self, index: Option<u8>) {
        let target = index.and_then(|index| {
            self.snapshot
                .as_ref()?
                .workspaces
                .get(index.saturating_sub(1) as usize)
                .map(|workspace| workspace.workspace_id.clone())
        });
        if let Some(workspace_id) = target {
            self.request("workspace.focus", json!({"workspace_id": workspace_id}));
        }
    }

    fn focus_relative_tab(&mut self, delta: isize) {
        let target = self.snapshot.as_ref().and_then(|snapshot| {
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == snapshot.focused_workspace_id)
                .collect::<Vec<_>>();
            let current = tabs
                .iter()
                .position(|tab| tab.tab_id == snapshot.focused_tab_id)?;
            let index = wrapped_index(current, tabs.len(), delta)?;
            Some(tabs[index].tab_id.clone())
        });
        if let Some(tab_id) = target {
            self.request("tab.focus", json!({"tab_id": tab_id}));
        }
    }

    fn focus_tab_index(&mut self, index: Option<u8>) {
        let target = index.and_then(|index| {
            let snapshot = self.snapshot.as_ref()?;
            snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == snapshot.focused_workspace_id)
                .nth(index.saturating_sub(1) as usize)
                .map(|tab| tab.tab_id.clone())
        });
        if let Some(tab_id) = target {
            self.request("tab.focus", json!({"tab_id": tab_id}));
        }
    }

    fn focus_relative_agent(&mut self, delta: isize) {
        let target = self.snapshot.as_ref().and_then(|snapshot| {
            let agents = sorted_agents(snapshot, self.config.ui.agent_panel_sort);
            let current = agents.iter().position(|agent| agent.focused).unwrap_or(0);
            let index = wrapped_index(current, agents.len(), delta)?;
            Some(agents[index].pane_id.clone())
        });
        self.focus_agent_target(target);
    }

    fn focus_agent_index(&mut self, index: Option<u8>) {
        let target = index.and_then(|index| {
            let snapshot = self.snapshot.as_ref()?;
            sorted_agents(snapshot, self.config.ui.agent_panel_sort)
                .get(index.saturating_sub(1) as usize)
                .map(|agent| agent.pane_id.clone())
        });
        self.focus_agent_target(target);
    }

    fn focus_agent_target(&mut self, target: Option<String>) {
        if let Some(target) = target {
            self.request("agent.focus", json!({"target": target}));
        }
    }

    fn focus_relative_pane(&mut self, delta: isize) {
        let target = self.snapshot.as_ref().and_then(|snapshot| {
            let panes = snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == snapshot.focused_tab_id)
                .collect::<Vec<_>>();
            let current = panes
                .iter()
                .position(|pane| pane.pane_id == snapshot.focused_pane_id)?;
            let index = wrapped_index(current, panes.len(), delta)?;
            Some(panes[index].pane_id.clone())
        });
        if let Some(target) = target {
            self.focus_pane_locally(&target);
        }
    }

    fn focus_last_pane(&mut self) {
        if let Some(target) = self.last_pane_id.clone() {
            self.focus_pane_locally(&target);
        }
    }

    /// Reorder `tab_id` to `insert_index` among its workspace's tabs in the
    /// local snapshot (`snapshot.tabs` holds every workspace's tabs in one
    /// list, so the move happens inside that workspace's run of entries).
    fn move_tab_locally(&mut self, tab_id: &str, insert_index: usize) {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        let Some(position) = snapshot.tabs.iter().position(|tab| tab.tab_id == tab_id) else {
            return;
        };
        let tab = snapshot.tabs.remove(position);
        let siblings: Vec<usize> = snapshot
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, other)| other.workspace_id == tab.workspace_id)
            .map(|(index, _)| index)
            .collect();
        let at = siblings
            .get(insert_index)
            .copied()
            .or_else(|| siblings.last().map(|last| last + 1))
            .unwrap_or(position);
        let workspace_id = tab.workspace_id.clone();
        snapshot.tabs.insert(at, tab);
        if let Some(workspace) = snapshot
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.workspace_id == workspace_id)
        {
            if let Some(listed) = workspace.tabs.iter().position(|id| id == tab_id) {
                let id = workspace.tabs.remove(listed);
                let at = insert_index.min(workspace.tabs.len());
                workspace.tabs.insert(at, id);
            }
        }
        self.ui_revision = self.ui_revision.saturating_add(1);
    }

    fn focus_pane_locally(&mut self, pane_id: &str) {
        let Some(previous) = self.snapshot.as_mut().and_then(|snapshot| {
            let target = snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .map(|pane| (pane.workspace_id.clone(), pane.tab_id.clone()))?;
            let previous = std::mem::replace(&mut snapshot.focused_pane_id, pane_id.to_owned());
            snapshot.focused_workspace_id = target.0.clone();
            snapshot.focused_tab_id = target.1.clone();
            for workspace in &mut snapshot.workspaces {
                workspace.focused = workspace.workspace_id == target.0;
            }
            for tab in &mut snapshot.tabs {
                tab.focused = tab.tab_id == target.1;
            }
            for pane in &mut snapshot.panes {
                pane.focused = pane.pane_id == pane_id;
            }
            for layout in &mut snapshot.layouts {
                if layout.tab_id == target.1 {
                    layout.focused_pane_id = pane_id.to_owned();
                    for pane in &mut layout.panes {
                        pane.focused = pane.pane_id == pane_id;
                    }
                }
            }
            Some(previous)
        }) else {
            return;
        };
        self.last_pane_id = Some(previous);
        // The local snapshot echo above gives instant feedback; the server is
        // the source of truth, so it must record the same pane focus or its
        // next snapshot push would revert the click to the old pane.
        self.request("pane.focus", json!({"pane_id": pane_id}));
    }

    fn prompt(&mut self, kind: PromptKind) {
        self.prompt_with(kind, String::new());
    }

    /// Prompt whose input starts filled in, so a bare Enter accepts the
    /// suggestion and Backspace edits it.
    fn prompt_with(&mut self, kind: PromptKind, value: String) {
        self.prompt_target = None;
        self.modes
            .push(Mode::Modal(Modal::Prompt { kind, value }));
    }

    /// The name a new tab is offered by default: its position in the
    /// workspace (current count plus one), so nobody has to invent a label.
    fn next_tab_label(&self, workspace_id: Option<&str>) -> String {
        let workspace_id = workspace_id
            .map(str::to_owned)
            .or_else(|| self.focused_workspace_id());
        let count = self
            .snapshot
            .as_ref()
            .zip(workspace_id.as_deref())
            .map_or(0, |(snapshot, workspace_id)| {
                snapshot
                    .tabs
                    .iter()
                    .filter(|tab| tab.workspace_id == workspace_id)
                    .count()
            });
        (count + 1).to_string()
    }

    /// Prompt that acts on an explicit object rather than the focused one.
    fn prompt_for(&mut self, kind: PromptKind, target: String) {
        self.prompt(kind);
        self.prompt_target = Some(target);
    }

    fn confirm(&mut self, action: Action, index: Option<u8>) {
        self.modes
            .push(Mode::Modal(Modal::Confirm { action, index }));
    }

    fn replace_modal(&mut self, modal: Modal) {
        if let Some(mode) = self.modes.last_mut() {
            *mode = Mode::Modal(modal);
        }
    }

    fn exit_mode(&mut self) {
        if self.modes.len() > 1 {
            self.modes.pop();
        }
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar = match self.sidebar {
            SidebarState::Expanded => match self.config.ui.sidebar_collapsed_mode {
                SidebarCollapsedMode::Compact => SidebarState::Compact,
                SidebarCollapsedMode::Hidden => SidebarState::Hidden,
            },
            SidebarState::Compact | SidebarState::Hidden => SidebarState::Expanded,
        };
    }

    fn complete_onboarding(&mut self, delivery: ToastDelivery) -> Result<(), AppError> {
        self.config.onboarding = Some(false);
        self.config.ui.toast.delivery = delivery;
        if let Some(path) = &self.config_path {
            save_onboarding_choice(path, delivery)?;
        }
        self.exit_mode();
        self.push_toast("Notification setup saved");
        Ok(())
    }

    fn push_toast(&mut self, message: impl Into<String>) {
        let message = message.into();
        match self.config.ui.toast.delivery {
            ToastDelivery::Starcil => self.toasts.push(ToastMessage {
                message,
                position: toast_position(self.config.ui.toast.starcil.position),
                created: std::time::Instant::now(),
            }),
            ToastDelivery::System => self.effects.push(AppEffect::DesktopNotification {
                title: NOTIFICATION_TITLE.to_owned(),
                body: message,
            }),
            ToastDelivery::Terminal => self.effects.push(AppEffect::TerminalNotification {
                title: NOTIFICATION_TITLE.to_owned(),
                body: message,
            }),
            ToastDelivery::Off => {}
        }
    }

    fn push_clipboard_toast(&mut self, message: impl Into<String>) {
        if self.config.ui.toast.clipboard.enabled {
            self.toasts.push(ToastMessage {
                message: message.into(),
                position: self.config.ui.toast.clipboard.position,
                created: std::time::Instant::now(),
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn age_toasts(&mut self, by: std::time::Duration) {
        for toast in &mut self.toasts {
            toast.created -= by;
        }
    }

    pub fn dock_agents(&self) -> &[DockAgent] {
        &self.dock_agents
    }

    #[cfg(test)]
    pub(crate) fn set_dock_agents(&mut self, agents: Vec<DockAgent>) {
        self.dock_agents = agents;
    }

    /// True while the focused pane's OWN composer owns the keyboard: always,
    /// for a shell pane sitting at its prompt. There is exactly one place to
    /// type — the prompt at the top never takes keys while the composer
    /// shows (Cesar: "why would there be two"). A program running under the
    /// shell (vim, a script asking y/n) gets the keys instead, until the
    /// prompt returns.
    pub fn composer_focused(&self) -> bool {
        crate::mouse::composer_pane_id(self).is_some_and(|pane_id| {
            self.snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id))
                .is_none_or(|pane| pane.shell_idle != Some(false))
        })
    }

    /// Frame of the sidebar's working spinner. Pinned to 0 while no agent is
    /// working so redraw fingerprints stay quiet.
    pub fn spinner_frame(&self) -> usize {
        let working = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .agents
                .iter()
                .any(|agent| agent.agent_status == AgentStatus::Working)
        });
        if working { self.spinner_frame } else { 0 }
    }

    #[cfg(test)]
    pub(crate) fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    /// The focused pane's composer draft; each pane keeps its own.
    pub fn composer_text(&self) -> String {
        self.composer_editor().map(LineEditor::text).unwrap_or_default()
    }

    /// Cursor position in the draft, in chars.
    pub fn composer_cursor(&self) -> usize {
        self.composer_editor().map_or(0, LineEditor::cursor)
    }

    /// The ctrl+r search running in the focused composer, if any.
    pub fn composer_search(&self) -> Option<&SearchState> {
        let pane_id = crate::mouse::composer_pane_id(self)?;
        self.search.as_ref().filter(|search| search.pane_id == pane_id)
    }

    fn composer_editor(&self) -> Option<&LineEditor> {
        crate::mouse::composer_pane_id(self).and_then(|pane_id| self.composers.get(&pane_id))
    }

    /// Seed the shared command history (the client reads the shell's own
    /// history file at startup).
    pub fn seed_history(&mut self, lines: impl IntoIterator<Item = String>) {
        self.history.seed(lines);
    }

    /// History entries, newest last.
    pub fn history_entries(&self) -> &[String] {
        self.history.entries()
    }

    pub fn set_completion_source(&mut self, source: Box<dyn CompletionSource>) {
        self.completion_source = source;
    }

    /// First char of the draft shown in a composer row `width` cells wide
    /// (the prompt and any search prefix already taken out): the line
    /// scrolls so the cursor always stays in view.
    pub fn composer_scroll(&self, width: usize) -> usize {
        let Some(editor) = self.composer_editor() else {
            return 0;
        };
        let widths: Vec<usize> = editor.chars().iter().map(|c| composer_char_width(*c)).collect();
        // One cell stays free for the cursor block after the last char.
        let room = width.saturating_sub(1);
        let mut skip = 0;
        let mut shown: usize = widths[..editor.cursor()].iter().sum();
        while shown > room && skip < editor.cursor() {
            shown -= widths[skip];
            skip += 1;
        }
        skip
    }

    /// Insert text at the cursor of the focused composer (paste, AltGr
    /// glyphs). Newlines stay: Enter runs the lines one after another.
    fn composer_insert_text(&mut self, text: &str) {
        let Some(pane_id) = crate::mouse::composer_pane_id(self) else {
            return;
        };
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            return;
        }
        self.composers
            .entry(pane_id)
            .or_default()
            .insert_str(text);
        self.after_composer_edit();
    }

    /// Any edit ends a Tab cycle and a history walk (the text shown stays).
    fn after_composer_edit(&mut self) {
        self.completion = None;
        self.history.reset();
    }

    /// A click on the input row puts the cursor under the pointer:
    /// `offset` cells from the row's left edge, `width` cells in the row.
    fn place_composer_cursor(&mut self, offset: u16, width: u16) {
        let Some(pane_id) = crate::mouse::composer_pane_id(self) else {
            return;
        };
        let prefix = self
            .composer_search()
            .map_or(0, |search| search_prefix(search).chars().count());
        let available = usize::from(width).saturating_sub(COMPOSER_PROMPT_WIDTH + prefix);
        let skip = self.composer_scroll(available);
        let target = usize::from(offset).saturating_sub(COMPOSER_PROMPT_WIDTH + prefix);
        let Some(editor) = self.composers.get_mut(&pane_id) else {
            return;
        };
        let mut column = 0usize;
        let mut index = skip;
        for character in &editor.chars()[skip..] {
            let cell_width = composer_char_width(*character);
            if column + cell_width > target {
                break;
            }
            column += cell_width;
            index += 1;
        }
        editor.set_cursor(index);
        self.completion = None;
    }

    pub fn ui_revision(&self) -> u64 {
        self.ui_revision
    }

    /// The folder shown in the focused pane's composer: a folder picked in
    /// THAT pane wins, otherwise its cwd from the snapshot. Never another
    /// pane's pick.
    pub fn dock_cwd_label(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == snapshot.focused_pane_id)?;
        Some(
            self.dock_cwd
                .get(&pane.pane_id)
                .cloned()
                .unwrap_or_else(|| pane.cwd.clone()),
        )
    }

    /// Result of the native folder picker. `None` means cancelled.
    pub fn folder_picked(&mut self, path: Option<String>) {
        let Some(path) = path else {
            return;
        };
        if let Some(pane_id) = self.focused_pane_id() {
            self.dock_cwd.insert(pane_id, path.clone());
        }
        self.ui_revision = self.ui_revision.saturating_add(1);
        let focused_has_agent = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == snapshot.focused_pane_id)
                .is_some_and(|pane| pane.agent.is_some() || pane.agent_name.is_some())
        });
        if focused_has_agent {
            // Never inject shell commands into an AI composer; the folder
            // still applies to every pane opened from the dock.
            self.push_toast(format!("Folder for new panes: {path}"));
            return;
        }
        if let Some(pane_id) = self.focused_pane_id() {
            self.snap_to_live(&pane_id);
            // pushd changes directory AND drive on cmd, and is Push-Location
            // in PowerShell — the one spelling both shells accept.
            self.link.send(ClientMsg::Input(InputFrame::Text {
                pane_id: pane_id.clone(),
                text: format!("pushd \"{path}\""),
            }));
            self.link.send(ClientMsg::Input(InputFrame::Keys {
                pane_id,
                keys: vec!["enter".to_owned()],
            }));
        }
    }

    /// Run the dock agent in the CURRENT pane (Cesar: same window, no split).
    /// Once the CLI is detected there, the composer and the shortcuts hide.
    fn launch_dock_agent(&mut self, position: usize) {
        let Some(agent) = self.dock_agents.get(position).cloned() else {
            return;
        };
        // Only a plain shell can take the command; with an agent running the
        // dock is hidden anyway, this is the keyboard-path guard.
        let Some(pane_id) = crate::mouse::composer_pane_id(self) else {
            return;
        };
        self.snap_to_live(&pane_id);
        self.link.send(ClientMsg::Input(InputFrame::Text {
            pane_id: pane_id.clone(),
            text: agent.command,
        }));
        self.link.send(ClientMsg::Input(InputFrame::Keys {
            pane_id,
            keys: vec!["enter".to_owned()],
        }));
    }

    /// Keys while the composer owns the keyboard. Line editing (cursor,
    /// words, kills), history (up/down by prefix, ctrl+r), Tab completion
    /// and paste all happen HERE, in the draft — nothing reaches the PTY
    /// until Enter. The chords a shell user expects to hit the program
    /// (ctrl+c to interrupt, ctrl+l to clear, ctrl+d to exit, ctrl+z…) still
    /// pass through when the draft has nothing for them to act on.
    fn handle_composer_key(&mut self, event: KeyEvent, chord: KeyChord) -> Result<(), AppError> {
        if chord.mods.ctrl && !chord.mods.alt && chord.key == Key::Character('o') {
            self.effects.push(AppEffect::OpenFolderPicker);
            return Ok(());
        }
        // The composer belongs to the focused shell pane; its draft lives in
        // that pane's slot only.
        let Some(pane_id) = crate::mouse::composer_pane_id(self) else {
            return Ok(());
        };
        if self.search.as_ref().is_some_and(|search| search.pane_id != pane_id) {
            self.search = None;
        }
        if self.search.is_some() {
            return self.handle_search_key(&pane_id, event, chord);
        }
        let mods = chord.mods;
        if mods.meta.is_some() {
            self.forward_to_pane(event, chord);
            return Ok(());
        }
        let has_draft = self
            .composers
            .get(&pane_id)
            .is_some_and(|draft| !draft.is_empty());
        if mods.ctrl && !mods.alt {
            if chord.key == Key::Character('r') {
                self.begin_search(&pane_id);
                return Ok(());
            }
            let editor = self.composers.entry(pane_id.clone()).or_default();
            let handled = match chord.key {
                // ctrl+c discards a draft (Claude Code's habit); with nothing
                // drafted it interrupts the program in the pane.
                Key::Character('c') if has_draft => {
                    editor.clear();
                    true
                }
                // ctrl+d deletes under the cursor while there is a draft; on
                // an empty line it reaches the shell (bash, PSReadLine).
                Key::Character('d') if has_draft => editor.delete(),
                Key::Character('a') => {
                    editor.move_home();
                    true
                }
                Key::Character('e') => {
                    editor.move_end();
                    true
                }
                Key::Character('u') | Key::Named(NamedKey::Home) => editor.kill_to_start(),
                Key::Character('k') | Key::Named(NamedKey::End) => editor.kill_to_end(),
                Key::Character('w') | Key::Named(NamedKey::Backspace) => {
                    editor.delete_word_back()
                }
                Key::Named(NamedKey::Delete) => editor.delete_word_forward(),
                Key::Named(NamedKey::Left) => {
                    editor.word_left();
                    true
                }
                Key::Named(NamedKey::Right) => {
                    editor.word_right();
                    true
                }
                // ctrl+enter: a line break in the draft (Enter runs it line
                // by line), the same chord Claude Code and codex use.
                Key::Named(NamedKey::Enter) => {
                    editor.insert_char('\n');
                    true
                }
                _ => false,
            };
            if handled {
                self.after_composer_edit();
            } else if !matches!(
                chord.key,
                Key::Character('a' | 'e' | 'u' | 'k' | 'w')
                    | Key::Named(NamedKey::Home | NamedKey::End | NamedKey::Left | NamedKey::Right)
                    | Key::Named(NamedKey::Backspace | NamedKey::Delete | NamedKey::Enter)
            ) {
                // ctrl+l, ctrl+z, ctrl+c/ctrl+d on an empty line…: the
                // program's. Editing chords with nothing to edit stay here.
                self.forward_to_pane(event, chord);
            }
            return Ok(());
        }
        if mods.alt && !mods.ctrl {
            let editor = self.composers.entry(pane_id.clone()).or_default();
            let handled = match chord.key {
                Key::Character('b') => {
                    editor.word_left();
                    true
                }
                Key::Character('f') => {
                    editor.word_right();
                    true
                }
                Key::Character('d') => editor.delete_word_forward(),
                Key::Named(NamedKey::Backspace) => editor.delete_word_back(),
                // alt+enter (meta+enter, readline's newline).
                Key::Named(NamedKey::Enter) => {
                    editor.insert_char('\n');
                    true
                }
                _ => false,
            };
            if handled {
                self.after_composer_edit();
            } else if !matches!(
                chord.key,
                Key::Character('b' | 'f' | 'd') | Key::Named(NamedKey::Backspace | NamedKey::Enter)
            ) {
                self.forward_to_pane(event, chord);
            }
            return Ok(());
        }
        match chord.key {
            // shift+enter breaks the line inside the draft; Enter runs the
            // draft line by line (the paste path). Only terminals that report
            // a modified Enter get here — the rest see a plain Enter.
            Key::Named(NamedKey::Enter) if mods.shift => {
                self.composers.entry(pane_id).or_default().insert_char('\n');
                self.after_composer_edit();
            }
            Key::Named(NamedKey::Enter) => self.submit_composer(&pane_id),
            Key::Named(NamedKey::Backspace) => {
                self.composers.entry(pane_id).or_default().backspace();
                self.after_composer_edit();
            }
            Key::Named(NamedKey::Delete) => {
                self.composers.entry(pane_id).or_default().delete();
                self.after_composer_edit();
            }
            Key::Named(NamedKey::Left) => {
                self.composers.entry(pane_id).or_default().move_left();
                self.completion = None;
            }
            Key::Named(NamedKey::Right) => {
                self.composers.entry(pane_id).or_default().move_right();
                self.completion = None;
            }
            Key::Named(NamedKey::Home) => {
                self.composers.entry(pane_id).or_default().move_home();
                self.completion = None;
            }
            Key::Named(NamedKey::End) => {
                self.composers.entry(pane_id).or_default().move_end();
                self.completion = None;
            }
            Key::Named(NamedKey::Up) => {
                let current = self.composer_text();
                if let Some(entry) = self.history.previous(&current) {
                    self.composers.entry(pane_id).or_default().set_text(&entry);
                }
                self.completion = None;
            }
            Key::Named(NamedKey::Down) => {
                let current = self.composer_text();
                if let Some(entry) = self.history.next(&current) {
                    self.composers.entry(pane_id).or_default().set_text(&entry);
                }
                self.completion = None;
            }
            Key::Named(NamedKey::Tab) => self.cycle_completion(&pane_id, !mods.shift),
            Key::Named(NamedKey::Esc) => {
                // Esc clears the draft. With nothing drafted it does nothing:
                // the keyboard never leaves the composer at the prompt.
                if has_draft {
                    self.composers.remove(&pane_id);
                    self.after_composer_edit();
                }
            }
            Key::Named(NamedKey::Space) => {
                self.composers.entry(pane_id).or_default().insert_char(' ');
                self.after_composer_edit();
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown) | Key::Function(_) => {
                self.forward_to_pane(event, chord);
            }
            _ => {
                if let KeyCode::Char(character) = event.code {
                    let plain = !event.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) || Self::is_altgr_text_event(event);
                    if plain {
                        self.composers.entry(pane_id).or_default().insert_char(character);
                        self.after_composer_edit();
                    }
                }
            }
        }
        Ok(())
    }

    /// Enter: the draft runs in the pane. Text and Enter travel as SEPARATE
    /// writes (one combined write reads as a paste to TUIs — hard-won ConPTY
    /// lesson); a multi-line draft (pasted) runs line by line.
    fn submit_composer(&mut self, pane_id: &str) {
        self.snap_to_live(pane_id);
        self.completion = None;
        let text = self
            .composers
            .remove(pane_id)
            .map(|editor| editor.text())
            .unwrap_or_default();
        self.history.push(&text);
        if text.is_empty() {
            self.link.send(ClientMsg::Input(InputFrame::Keys {
                pane_id: pane_id.to_owned(),
                keys: vec!["enter".to_owned()],
            }));
            return;
        }
        for line in text.split('\n') {
            if !line.is_empty() {
                self.link.send(ClientMsg::Input(InputFrame::Text {
                    pane_id: pane_id.to_owned(),
                    text: line.to_owned(),
                }));
            }
            self.link.send(ClientMsg::Input(InputFrame::Keys {
                pane_id: pane_id.to_owned(),
                keys: vec!["enter".to_owned()],
            }));
        }
    }

    /// Tab / shift+Tab: complete the token under the cursor against the
    /// pane's live cwd (paths) or the commands on PATH (first word), and walk
    /// the candidates on repeated presses the way PowerShell does.
    fn cycle_completion(&mut self, pane_id: &str, forward: bool) {
        self.history.reset();
        let editor = self.composers.entry(pane_id.to_owned()).or_default().clone();
        let reuse = self.completion.as_ref().is_some_and(|cycle| {
            cycle.pane_id == pane_id
                && cycle.text == editor.text()
                && cycle.cursor == editor.cursor()
                && cycle.candidates.len() > 1
        });
        if reuse {
            if let Some(cycle) = self.completion.as_mut() {
                let len = cycle.candidates.len();
                cycle.index = if forward {
                    (cycle.index + 1) % len
                } else {
                    (cycle.index + len - 1) % len
                };
            }
        } else {
            let Some(cwd) = self.dock_cwd_label() else {
                return;
            };
            let history_commands = self.history.command_words();
            let context = CompletionContext {
                cwd: &cwd,
                windows: cfg!(windows),
                history_commands: &history_commands,
            };
            let Some(completion) = complete(
                editor.chars(),
                editor.cursor(),
                &context,
                self.completion_source.as_ref(),
            ) else {
                return;
            };
            let index = if forward { 0 } else { completion.candidates.len() - 1 };
            self.completion = Some(CompletionCycle {
                pane_id: pane_id.to_owned(),
                start: completion.start,
                end: completion.end,
                candidates: completion.candidates,
                index,
                text: String::new(),
                cursor: 0,
            });
        }
        let Some(cycle) = self.completion.as_mut() else {
            return;
        };
        let candidate = cycle.candidates[cycle.index].clone();
        let editor = self.composers.entry(pane_id.to_owned()).or_default();
        editor.replace(cycle.start, cycle.end, &candidate.text, candidate.cursor_back);
        cycle.end = cycle.start + candidate.text.chars().count();
        cycle.text = editor.text();
        cycle.cursor = editor.cursor();
        if cycle.candidates.len() == 1 {
            self.completion = None;
        }
    }

    fn begin_search(&mut self, pane_id: &str) {
        self.after_composer_edit();
        let stash = self.composers.get(pane_id).cloned().unwrap_or_default();
        self.search = Some(SearchState {
            pane_id: pane_id.to_owned(),
            query: String::new(),
            found: true,
            stash,
            hit: None,
        });
    }

    /// Keys during ctrl+r: chars refine the query, ctrl+r goes older,
    /// Backspace shortens, Enter runs the hit, Esc/arrows keep it for
    /// editing, ctrl+c/ctrl+g bring the old draft back; anything else
    /// accepts the hit and is handled as usual.
    fn handle_search_key(
        &mut self,
        pane_id: &str,
        event: KeyEvent,
        chord: KeyChord,
    ) -> Result<(), AppError> {
        let mods = chord.mods;
        if mods.ctrl && !mods.alt && mods.meta.is_none() {
            match chord.key {
                Key::Character('r') => {
                    self.search_step(pane_id, true);
                    return Ok(());
                }
                Key::Character('c' | 'g') => {
                    if let Some(search) = self.search.take() {
                        self.composers.insert(pane_id.to_owned(), search.stash);
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        match chord.key {
            Key::Named(NamedKey::Enter) if !mods.ctrl && !mods.alt => {
                self.search = None;
                self.submit_composer(pane_id);
            }
            Key::Named(
                NamedKey::Esc
                | NamedKey::Left
                | NamedKey::Right
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::Up
                | NamedKey::Down
                | NamedKey::Tab,
            ) if !mods.ctrl && !mods.alt => {
                self.search = None;
            }
            Key::Named(NamedKey::Backspace) if !mods.ctrl && !mods.alt => {
                if let Some(search) = self.search.as_mut() {
                    search.query.pop();
                }
                self.search_step(pane_id, false);
            }
            _ => {
                let plain = !event.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) || Self::is_altgr_text_event(event);
                if let (KeyCode::Char(character), true) = (event.code, plain) {
                    if let Some(search) = self.search.as_mut() {
                        search.query.push(character);
                    }
                    self.search_step(pane_id, false);
                    return Ok(());
                }
                self.search = None;
                return self.handle_composer_key(event, chord);
            }
        }
        Ok(())
    }

    /// Re-run the search for the current query; `older` continues past the
    /// current hit. A miss keeps the last hit on the row, flagged.
    fn search_step(&mut self, pane_id: &str, older: bool) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.query.is_empty() {
            search.found = true;
            search.hit = None;
            let stash = search.stash.clone();
            self.composers.insert(pane_id.to_owned(), stash);
            return;
        }
        let before = if older { search.hit } else { None };
        match self.history.search_backward(&search.query, before) {
            Some((index, entry)) => {
                search.hit = Some(index);
                search.found = true;
                self.composers
                    .entry(pane_id.to_owned())
                    .or_default()
                    .set_text(&entry);
            }
            None => search.found = false,
        }
    }

    /// Report the pane layout area (terminal minus sidebar and tab bar) to the
    /// server whenever it changes. The server lays panes out and sizes their
    /// PTYs over this area, so it must be exactly what the renderer gives the
    /// panes — reporting the whole terminal made every PTY wider and taller
    /// than its frame and the content spilled past the borders.
    pub fn sync_layout_area(&mut self, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let geometry = UiGeometry::calculate(self, area);
        let main = geometry.main;
        let desired = geometry
            .composer
            .as_ref()
            .map(|composer| (composer.pane_id.clone(), composer.rows));
        let current = (main.width.max(1), main.height.max(1));
        if self.reported_layout_area != Some(current) {
            self.reported_layout_area = Some(current);
            self.link.send(ClientMsg::Input(InputFrame::ClientArea {
                cols: current.0,
                rows: current.1,
            }));
        }
        // Keep the server's per-pane row reservation in step with the in-pane
        // composer: clear the old pane's reservation when it moves or hides.
        if self.reported_reservation != desired {
            if let Some((previous_pane, _)) = &self.reported_reservation {
                let still_same_pane = desired
                    .as_ref()
                    .is_some_and(|(pane, _)| pane == previous_pane);
                if !still_same_pane {
                    self.link.send(ClientMsg::Input(InputFrame::ReserveRows {
                        pane_id: previous_pane.clone(),
                        rows: 0,
                    }));
                }
            }
            if let Some((pane_id, rows)) = &desired {
                self.link.send(ClientMsg::Input(InputFrame::ReserveRows {
                    pane_id: pane_id.clone(),
                    rows: *rows,
                }));
            }
            self.reported_reservation = desired;
        }
    }

    #[cfg(test)]
    pub(crate) fn reported_layout_area(&self) -> Option<(u16, u16)> {
        self.reported_layout_area
    }

    pub fn handle_mouse<C: Clipboard>(
        &mut self,
        event: MouseEvent,
        area: Rect,
        clipboard: &mut C,
    ) -> Result<MouseAction, AppError> {
        if self.mouse_debug {
            self.mouse_event_counter = self.mouse_event_counter.saturating_add(1);
            self.last_mouse_event = Some(event.clone());
        }
        let mut controller = std::mem::take(&mut self.mouse);
        let action = controller.route(self, area, event);
        self.mouse = controller;
        match action.clone() {
            MouseAction::Ignored => {}
            MouseAction::NewWorkspace => self.dispatch_action(Action::NewWorkspace, None)?,
            MouseAction::OpenMenu => self.modes.push(Mode::Modal(Modal::Menu { selected: 0 })),
            MouseAction::NewTab => self.dispatch_action(Action::NewTab, None)?,
            MouseAction::FocusPane(pane_id) => self.focus_pane_locally(&pane_id),
            MouseAction::FocusComposer => {
                if let Some(pane_id) = crate::mouse::composer_pane_id(self) {
                    self.snap_to_live(&pane_id);
                }
            }
            MouseAction::PlaceComposerCursor { offset, width } => {
                self.place_composer_cursor(offset, width);
            }
            MouseAction::DockLaunch(position) => self.launch_dock_agent(position),
            MouseAction::OpenFolderPicker => self.effects.push(AppEffect::OpenFolderPicker),
            MouseAction::ToggleSidebar => self.toggle_sidebar(),
            MouseAction::FocusWorkspace(workspace_id) => {
                self.request("workspace.focus", json!({"workspace_id": workspace_id}));
            }
            MouseAction::FocusTab(tab_id) => {
                self.request("tab.focus", json!({"tab_id": tab_id}));
            }
            MouseAction::MoveTab {
                tab_id,
                insert_index,
            } => {
                // Local echo first so the bar follows the pointer; the
                // server's `tab.moved` snapshot then lands on the same order.
                self.move_tab_locally(&tab_id, insert_index);
                self.request(
                    "tab.move",
                    json!({"tab_id": tab_id, "insert_index": insert_index}),
                );
            }
            MouseAction::FocusAgent(pane_id) => {
                self.focus_agent_target(Some(pane_id));
            }
            MouseAction::BeginSelection { pane_id, row, col } => {
                // A click on the content focuses the pane; the keyboard stays
                // with (or moves to) that pane's composer — clicking the
                // prompt at the top to type there lands the typing below.
                self.focus_pane_locally(&pane_id);
                let double_click = self.selection.begin(pane_id.clone(), row, col);
                if double_click {
                    if let Some(mirror) = self.mirrors.get(&pane_id) {
                        self.selection.select_word(&pane_id, row, col, mirror);
                    }
                    if self.config.ui.copy_on_select {
                        self.copy_selection(&pane_id, clipboard)?;
                    }
                }
            }
            MouseAction::UpdateSelection { pane_id, row, col } => {
                self.selection.update(&pane_id, row, col);
            }
            MouseAction::FinishSelection => {
                let pane_id = self
                    .selection
                    .selection()
                    .map(|selection| selection.pane_id.clone());
                if self.selection.finish() && self.config.ui.copy_on_select {
                    if let Some(pane_id) = pane_id {
                        self.copy_selection(&pane_id, clipboard)?;
                    }
                }
            }
            MouseAction::Resize {
                pane_id,
                direction,
                amount,
            } => self.request(
                "pane.resize",
                json!({"pane_id": pane_id, "direction": direction, "amount": amount}),
            ),
            MouseAction::ResizeSidebar { width } => {
                self.config.ui.sidebar_width = width;
            }
            MouseAction::ResizeSidebarSplit { percent } => {
                self.config.ui.sidebar_section_split_percent = percent;
                self.ui_revision = self.ui_revision.saturating_add(1);
            }
            MouseAction::SaveSidebarSplit => {
                let percent = self.config.ui.sidebar_section_split_percent;
                self.apply_setting(&ConfigSetting::SidebarSectionSplit(percent))?;
            }
            MouseAction::Scroll {
                pane_id,
                lines,
                alternate_screen,
            } => {
                if alternate_screen {
                    let key = if lines >= 0 { "up" } else { "down" };
                    self.link.send(ClientMsg::Input(InputFrame::Keys {
                        pane_id,
                        keys: vec![key.to_owned(); lines.unsigned_abs() as usize],
                    }));
                } else {
                    let maximum = self
                        .mirrors
                        .get(&pane_id)
                        .and_then(PaneMirror::scroll)
                        .map_or(0, |metrics| metrics.max_offset_from_bottom);
                    self.scrollback.scroll_by(&pane_id, lines, maximum);
                    self.link.send(ClientMsg::Input(InputFrame::Scroll {
                        pane_id,
                        delta: lines.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                    }));
                }
            }
            MouseAction::ContextMenu { target, x, y } => {
                self.open_context_menu(target, x, y);
            }
            MouseAction::CloseModal => {
                if matches!(self.mode(), Mode::Modal(modal) if !matches!(modal, Modal::Onboarding)) {
                    self.exit_mode();
                }
            }
            MouseAction::CloseModalAndContextMenu { target, x, y } => {
                if !matches!(self.mode(), Mode::Modal(Modal::Onboarding)) {
                    self.exit_mode();
                    self.open_context_menu(target, x, y);
                }
            }
            MouseAction::ContextMenuItem { index, activate } => {
                if let Mode::Modal(Modal::ContextMenu { target, x, y, .. }) = self.mode().clone() {
                    let items = ContextMenuAction::items(&target);
                    if index < items.len() {
                        if activate {
                            self.exit_mode();
                            self.run_context_action(items[index], &target, Some(clipboard))?;
                        } else {
                            self.replace_modal(Modal::ContextMenu {
                                target,
                                x,
                                y,
                                selected: index,
                            });
                        }
                    }
                }
            }
            MouseAction::MenuItem { index, activate } => {
                if index < self.menu_actions().len()
                    && matches!(self.mode(), Mode::Modal(Modal::Menu { .. }))
                {
                    if activate {
                        self.run_menu_action(index)?;
                    } else {
                        self.replace_modal(Modal::Menu { selected: index });
                    }
                }
            }
            MouseAction::SettingsSection(index) => {
                if matches!(self.mode(), Mode::Modal(Modal::Settings)) {
                    self.settings.select_section(index);
                }
            }
            MouseAction::SettingsRow { index, activate } => {
                if matches!(self.mode(), Mode::Modal(Modal::Settings))
                    && index < self.settings.rows().len()
                {
                    self.settings.select_row(index);
                    if activate {
                        self.handle_settings_key(KeyEvent::new(
                            KeyCode::Enter,
                            KeyModifiers::NONE,
                        ))?;
                    }
                }
            }
            MouseAction::Paste(pane_id) => {
                self.paste_from_clipboard(Some(pane_id), clipboard)?;
            }
            MouseAction::Passthrough {
                pane_id,
                data_base64,
            } => self.link.send(ClientMsg::Input(InputFrame::Bytes {
                pane_id,
                data_base64,
            })),
            MouseAction::MouseTracking {
                pane_id,
                data_base64,
                focus,
            } => {
                if focus {
                    self.focus_pane_locally(&pane_id);
                }
                self.link.send(ClientMsg::Input(InputFrame::Bytes {
                    pane_id,
                    data_base64,
                }));
            }
        }
        Ok(action)
    }

    fn open_context_menu(&mut self, target: ContextTarget, x: u16, y: u16) {
        // Right-clicking focuses what was clicked, like a left click would.
        match &target {
            ContextTarget::Pane(pane_id) => self.focus_pane_locally(pane_id),
            ContextTarget::Tab(tab_id) => self.focus_tab_locally(tab_id),
            ContextTarget::Workspace(workspace_id) => {
                self.request("workspace.focus", json!({"workspace_id": workspace_id}));
            }
        }
        self.modes.push(Mode::Modal(Modal::ContextMenu {
            target,
            x,
            y,
            selected: 0,
        }));
    }

    /// Local echo of a tab focus plus the server request (same contract as
    /// `focus_pane_locally`: the next snapshot must agree with the click).
    fn focus_tab_locally(&mut self, tab_id: &str) {
        if let Some(snapshot) = self.snapshot.as_mut() {
            if let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) {
                let workspace_id = tab.workspace_id.clone();
                snapshot.focused_tab_id = tab_id.to_owned();
                snapshot.focused_workspace_id = workspace_id.clone();
                for tab in &mut snapshot.tabs {
                    tab.focused = tab.tab_id == tab_id;
                }
                for workspace in &mut snapshot.workspaces {
                    workspace.focused = workspace.workspace_id == workspace_id;
                }
            }
        }
        self.request("tab.focus", json!({"tab_id": tab_id}));
    }

    fn run_context_action(
        &mut self,
        action: ContextMenuAction,
        target: &ContextTarget,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        match target {
            ContextTarget::Pane(pane_id) => {
                self.focus_pane_locally(pane_id);
                self.run_pane_context_action(action, pane_id, clipboard)
            }
            ContextTarget::Tab(tab_id) => {
                let workspace_id = self.snapshot.as_ref().and_then(|snapshot| {
                    snapshot
                        .tabs
                        .iter()
                        .find(|tab| &tab.tab_id == tab_id)
                        .map(|tab| tab.workspace_id.clone())
                });
                match action {
                    ContextMenuAction::NewTab => {
                        if self.config.ui.prompt_new_tab_name {
                            let label = self.next_tab_label(workspace_id.as_deref());
                            self.prompt_with(PromptKind::NewTab, label);
                        } else {
                            self.request(
                                "tab.create",
                                json!({"workspace_id": workspace_id, "focus": true}),
                            );
                        }
                    }
                    ContextMenuAction::Rename => {
                        self.prompt_for(PromptKind::RenameTab, tab_id.clone());
                    }
                    ContextMenuAction::Close => {
                        self.request("tab.close", json!({"tab_id": tab_id}));
                    }
                    _ => {}
                }
                Ok(())
            }
            ContextTarget::Workspace(workspace_id) => {
                match action {
                    ContextMenuAction::NewWorkspace => {
                        self.dispatch_action(Action::NewWorkspace, None)?;
                    }
                    ContextMenuAction::Rename => {
                        self.prompt_for(PromptKind::RenameWorkspace, workspace_id.clone());
                    }
                    ContextMenuAction::Close => {
                        if self.config.ui.confirm_close {
                            // The confirm acts on the focused workspace; the
                            // menu already requested focus on this one.
                            self.confirm(Action::CloseWorkspace, None);
                        } else {
                            self.request(
                                "workspace.close",
                                json!({"workspace_id": workspace_id}),
                            );
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
        }
    }

    fn run_pane_context_action(
        &mut self,
        action: ContextMenuAction,
        pane_id: &str,
        clipboard: Option<&mut dyn Clipboard>,
    ) -> Result<(), AppError> {
        match action {
            ContextMenuAction::CopySelection => {
                if let Some(clipboard) = clipboard {
                    self.copy_selection(pane_id, clipboard)?;
                }
            }
            ContextMenuAction::CopyScreen => {
                if let Some(clipboard) = clipboard {
                    self.copy_screen(pane_id, clipboard)?;
                }
            }
            ContextMenuAction::Paste => {
                if let Some(clipboard) = clipboard {
                    self.paste_from_clipboard(Some(pane_id.to_owned()), clipboard)?;
                }
            }
            ContextMenuAction::Rename => {
                self.prompt_for(PromptKind::RenamePane, pane_id.to_owned());
            }
            ContextMenuAction::Close => {
                self.request("pane.close", json!({"pane_id": pane_id}));
            }
            ContextMenuAction::Zoom => self.dispatch_action(Action::Zoom, None)?,
            ContextMenuAction::NewTab | ContextMenuAction::NewWorkspace => {}
        }
        Ok(())
    }

    fn copy_selection(
        &mut self,
        pane_id: &str,
        clipboard: &mut dyn Clipboard,
    ) -> Result<bool, AppError> {
        let text = self
            .mirrors
            .get(pane_id)
            .and_then(|mirror| self.selection.selected_text(mirror));
        let Some(text) = text else {
            return Ok(false);
        };
        clipboard.set_text(&text)?;
        self.push_clipboard_toast("Copied to clipboard");
        Ok(true)
    }

    fn copy_screen(
        &mut self,
        pane_id: &str,
        clipboard: &mut dyn Clipboard,
    ) -> Result<bool, AppError> {
        let Some(text) = self.mirrors.get(pane_id).map(PaneMirror::screen_text) else {
            return Ok(false);
        };
        clipboard.set_text(&text)?;
        self.push_clipboard_toast("Screen copied to clipboard");
        Ok(true)
    }

    fn paste_from_clipboard(
        &mut self,
        pane_id: Option<String>,
        clipboard: &mut dyn Clipboard,
    ) -> Result<bool, AppError> {
        let Some(pane_id) = pane_id else {
            return Ok(false);
        };
        let text = clipboard.get_text()?;
        if text.is_empty() {
            return Ok(false);
        }
        self.snap_to_live(&pane_id);
        // Pasting where the composer owns the keyboard lands in the draft:
        // the shell prompt above never takes text on its own.
        if self.composer_focused()
            && crate::mouse::composer_pane_id(self).as_deref() == Some(pane_id.as_str())
        {
            self.composer_insert_text(&text);
            return Ok(true);
        }
        self.link
            .send(ClientMsg::Input(InputFrame::Text { pane_id, text }));
        Ok(true)
    }

    /// Leave any scrollback view on `pane_id` so the live bottom (where input
    /// echoes) is visible again.
    fn snap_to_live(&mut self, pane_id: &str) {
        let offset = self.scrollback.offset(pane_id);
        if offset > 0 {
            self.scrollback.return_live(pane_id);
            self.link.send(ClientMsg::Input(InputFrame::Scroll {
                pane_id: pane_id.to_owned(),
                delta: -(offset.min(i32::MAX as u64) as i32),
            }));
        }
    }

    fn leave_copy_mode(&mut self) {
        let pane_id = self
            .scrollback
            .copy_state()
            .map(|state| state.pane_id.clone());
        self.scrollback.exit_copy();
        self.selection.clear();
        if let Some(pane_id) = pane_id {
            let offset = self.scrollback.offset(&pane_id);
            if offset > 0 {
                self.scrollback.return_live(&pane_id);
                self.link.send(ClientMsg::Input(InputFrame::Scroll {
                    pane_id,
                    delta: -(offset.min(i32::MAX as u64) as i32),
                }));
            }
        }
        self.exit_mode();
    }

    fn apply_setting(&mut self, setting: &ConfigSetting) -> Result<(), AppError> {
        if let Some(path) = &self.config_path {
            save_config_setting(path, setting)?;
        }
        self.theme = resolve_app_theme(&self.config, self.appearance)?;
        self.request("server.reload_config", json!({}));
        Ok(())
    }

    fn handle_event(&mut self, name: &str, data: &Value) {
        if name == "pane.shell_idle_changed" {
            self.apply_shell_idle(data);
            return;
        }
        if name == "pane.cwd_changed" {
            self.apply_cwd_changed(data);
            return;
        }
        if name != "pane.agent_status_changed" {
            return;
        }
        let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) else {
            return;
        };
        let Some(next) = data
            .get("agent_status")
            .or_else(|| data.get("status"))
            .and_then(Value::as_str)
            .and_then(AgentStatus::parse)
        else {
            return;
        };
        let incoming_seq = data.get("state_change_seq").and_then(Value::as_u64);
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        if incoming_seq.is_some_and(|seq| seq <= pane.state_change_seq.unwrap_or_default()) {
            return;
        }
        let previous = pane.agent_status;
        let announcement = agent_announcement(snapshot, pane, previous, next);
        if let Some(request) = request_for_transition(
            &self.config,
            self.config_path.as_deref(),
            snapshot,
            pane_id,
            previous,
            next,
        ) {
            self.sound_requests.push(request);
        }
        if let Some(text) = announcement {
            self.push_toast(text);
        }
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        if let Some(pane) = snapshot.panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
            pane.agent_status = next;
            if let Some(seq) = incoming_seq {
                pane.state_change_seq = Some(seq);
            }
        }
        if let Some(agent) = snapshot.agents.iter_mut().find(|agent| agent.pane_id == pane_id) {
            agent.agent_status = next;
            if let Some(seq) = incoming_seq {
                agent.state_change_seq = seq;
            }
        }
    }

    pub fn dismiss_toast(&mut self) {
        if !self.toasts.is_empty() {
            self.toasts.remove(0);
        }
    }

    fn forward_to_pane(&mut self, event: KeyEvent, chord: KeyChord) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        // Typing always lands at the live bottom: leave any scrollback view
        // first so the echoed input is on screen.
        self.snap_to_live(&pane_id);
        let plain_text = !event.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
        ) || Self::is_altgr_text_event(event);
        if let KeyCode::Char(character) = event.code {
            if plain_text {
                self.link.send(ClientMsg::Input(InputFrame::Text {
                    pane_id,
                    text: character.to_string(),
                }));
                return;
            }
        }
        self.link.send(ClientMsg::Input(InputFrame::Keys {
            pane_id,
            keys: vec![crate::input::pane_key_chord(chord).to_string()],
        }));
    }

    /// AltGr on Spanish/Latin-American layouts (`\ @ # { } [ ] |`) arrives as
    /// Ctrl+Alt plus the produced glyph; that is text for the pane, not a chord.
    /// `ctrl+alt+<letter>` stays a chord.
    fn is_altgr_text_event(event: KeyEvent) -> bool {
        matches!(
            event.code,
            KeyCode::Char(character)
                if event.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_ascii_alphanumeric()
        )
    }

    fn request(&mut self, method: &str, params: Value) {
        self.request_with_id(method, params);
    }

    fn request_with_id(&mut self, method: &str, params: Value) -> String {
        self.request_seq = self.request_seq.saturating_add(1);
        let id = format!("tui:{}", self.request_seq);
        self.link.send(ClientMsg::Request(Request::new(
            id.clone(),
            method,
            params,
        )));
        id
    }

    fn focused_workspace_id(&self) -> Option<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.focused_workspace_id.clone())
    }

    fn focused_tab_id(&self) -> Option<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.focused_tab_id.clone())
    }

    fn focused_pane_id(&self) -> Option<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.focused_pane_id.clone())
    }
}

fn resolve_app_theme(
    config: &Config,
    appearance: HostAppearance,
) -> Result<ResolvedTheme, ThemeError> {
    let mut theme = resolve_theme(&config.theme, appearance)?;
    if !config.theme.custom.contains_key("accent") {
        let accent = config.ui.accent.parse::<Color>()?;
        if accent != Color::Reset {
            theme.tokens.set("accent", accent)?;
        }
    }
    Ok(theme)
}

fn toast_position(position: InAppToastPosition) -> ToastPosition {
    match position {
        InAppToastPosition::TopLeft => ToastPosition::TopLeft,
        InAppToastPosition::TopRight => ToastPosition::TopRight,
        InAppToastPosition::BottomLeft => ToastPosition::BottomLeft,
        InAppToastPosition::BottomRight => ToastPosition::BottomRight,
    }
}

/// Cells the `❯ ` prompt takes on the composer row.
pub const COMPOSER_PROMPT_WIDTH: usize = 2;

/// Cells one draft char takes on the row (`⏎` stands in for a newline).
pub fn composer_char_width(character: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    if character == '\n' {
        return 1;
    }
    character.width().filter(|width| *width > 0).unwrap_or(1)
}

/// The `(reverse-i-search)'query': ` label before the draft during ctrl+r.
pub fn search_prefix(search: &SearchState) -> String {
    let state = if search.found { "" } else { "failed " };
    format!("({state}reverse-i-search)'{}': ", search.query)
}

fn is_copy_shortcut(event: KeyEvent) -> bool {
    matches!(event.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && event.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

fn direction_for_chord(chord: KeyChord) -> Option<&'static str> {
    match chord.key {
        Key::Named(NamedKey::Left) | Key::Character('h') => Some("left"),
        Key::Named(NamedKey::Down) | Key::Character('j') => Some("down"),
        Key::Named(NamedKey::Up) | Key::Character('k') => Some("up"),
        Key::Named(NamedKey::Right) | Key::Character('l') => Some("right"),
        _ => None,
    }
}

fn wrapped_index(current: usize, len: usize, delta: isize) -> Option<usize> {
    (len > 0).then(|| (current as isize + delta).rem_euclid(len as isize) as usize)
}

fn sorted_agents(
    snapshot: &SessionSnapshot,
    sort: AgentPanelSort,
) -> Vec<&starcil_protocol::types::AgentInfo> {
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    match sort {
        AgentPanelSort::Priority => agents.sort_by(|left, right| {
            right
                .agent_status
                .priority()
                .cmp(&left.agent_status.priority())
                .then_with(|| left.agent.cmp(&right.agent))
        }),
        AgentPanelSort::Spaces => agents.sort_by(|left, right| {
            left.workspace_id
                .cmp(&right.workspace_id)
                .then_with(|| left.agent.cmp(&right.agent))
        }),
    }
    agents
}
