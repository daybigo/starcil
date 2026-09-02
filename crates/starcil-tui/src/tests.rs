use std::collections::BTreeMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color as RatatuiColor;
use ratatui::Terminal;
use starcil_config::{
    parse_config, Action, Config, HostAppearance, KeyBindings, SoundPolicy, ToastDelivery,
    ToastPosition,
};
use starcil_domain::AgentStatus;
use starcil_platform::{Clipboard, ClipboardError};
use starcil_protocol::attach::{
    attr_bits, CursorState, InputFrame, PaneMouseEncoding, PaneMouseMode, PaneMouseTracking,
    RowPatch, Run, StyleDef, TerminalFrame,
};
use starcil_protocol::events::EventFrame;
use starcil_protocol::types::{
    AgentInfo, PaneInfo, PaneLayoutEntry, PaneLayoutSnapshot, PaneRect, SessionSnapshot, TabInfo,
    ScrollMetrics, WorkspaceInfo,
};
use starcil_protocol::{Incoming, SuccessResponse};

use crate::{
    protocol_style, ratatui_color, render_app, App, AppEffect, ApplyOutcome, ClientMsg, FakeLink,
    Mode, PaneMirror, ServerMsg,
};
use crate::mouse::{ChromeTarget, MouseAction, UiGeometry, modal_rect};
use crate::scrollback::{EditorError, EditorLauncher};
use crate::settings::SECTION_NAMES;
use crate::sound::{SoundController, SoundError, SoundPlayer, SoundRequest};

fn test_config() -> Config {
    let mut config = Config::default();
    config.onboarding = Some(false);
    config.ui.sidebar_width = 20;
    config.ui.sidebar_min_width = 18;
    config.ui.sidebar_max_width = 36;
    config.ui.pane_gaps = false;
    config.ui.hide_tab_bar_when_single_tab = true;
    config.ui.prompt_new_tab_name = false;
    config
}

fn session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        version: "0.2.0".to_owned(),
        protocol_major: 1,
        protocol_minor: 0,
        session: "foundry".to_owned(),
        revision: 7,
        focused_workspace_id: "w1".to_owned(),
        focused_tab_id: "t1".to_owned(),
        focused_pane_id: "p1".to_owned(),
        workspaces: vec![WorkspaceInfo {
            workspace_id: "w1".to_owned(),
            label: "alpha".to_owned(),
            cwd: "C:/repo".to_owned(),
            focused: true,
            revision: 7,
            tabs: vec!["t1".to_owned()],
            tokens: BTreeMap::new(),
            worktree: None,
        }],
        tabs: vec![TabInfo {
            tab_id: "t1".to_owned(),
            workspace_id: "w1".to_owned(),
            label: "main".to_owned(),
            focused: true,
            panes: vec!["p1".to_owned(), "p2".to_owned()],
            zoomed: None,
        }],
        panes: vec![pane_info("p1", "left", true), pane_info("p2", "right", false)],
        layouts: vec![PaneLayoutSnapshot {
            workspace_id: "w1".to_owned(),
            tab_id: "t1".to_owned(),
            area: PaneRect {
                x: 0,
                y: 0,
                width: 100,
                height: 24,
            },
            focused_pane_id: "p1".to_owned(),
            zoomed: None,
            panes: vec![
                PaneLayoutEntry {
                    pane_id: "p1".to_owned(),
                    rect: PaneRect {
                        x: 0,
                        y: 0,
                        width: 50,
                        height: 24,
                    },
                    focused: true,
                },
                PaneLayoutEntry {
                    pane_id: "p2".to_owned(),
                    rect: PaneRect {
                        x: 50,
                        y: 0,
                        width: 50,
                        height: 24,
                    },
                    focused: false,
                },
            ],
        }],
        agents: vec![AgentInfo {
            agent: "codex".to_owned(),
            agent_status: AgentStatus::Working,
            pane_id: "p1".to_owned(),
            terminal_id: "term-p1".to_owned(),
            workspace_id: "w1".to_owned(),
            tab_id: "t1".to_owned(),
            cwd: "C:/repo".to_owned(),
            focused: true,
            revision: 7,
            state_change_seq: 4,
            name: Some("forge".to_owned()),
            terminal_title: None,
            terminal_title_stripped: None,
            agent_session: None,
            foreground_cwd: None,
            tokens: BTreeMap::new(),
        }],
    }
}

fn pane_info(pane_id: &str, label: &str, focused: bool) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_owned(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_owned(),
        tab_id: "t1".to_owned(),
        focused,
        cwd: "C:/repo".to_owned(),
        agent_status: if focused {
            AgentStatus::Working
        } else {
            AgentStatus::Idle
        },
        revision: 7,
        label: Some(label.to_owned()),
        agent: focused.then(|| "codex".to_owned()),
        agent_name: focused.then(|| "forge".to_owned()),
        terminal_title: None,
        terminal_title_stripped: None,
        foreground_cwd: None,
        agent_session: None,
        scroll: Some(ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 30,
            viewport_rows: 3,
        }),
        tokens: BTreeMap::new(),
        state_change_seq: Some(4),
        shell_idle: None,
    }
}

fn frame(pane_id: &str, seq: u64, snapshot: bool, rows: Vec<RowPatch>) -> TerminalFrame {
    TerminalFrame {
        pane_id: pane_id.to_owned(),
        seq,
        generation: 3,
        cols: 16,
        rows: 3,
        snapshot,
        styles: if snapshot {
            vec![
                StyleDef {
                    fg: 0x01_e8_e8_e8,
                    bg: 0,
                    attrs: 0,
                },
                StyleDef {
                    fg: 0x02_00_00_45,
                    bg: 0x01_12_34_56,
                    attrs: attr_bits::BOLD,
                },
            ]
        } else {
            Vec::new()
        },
        patches: rows,
        cursor: Some(CursorState {
            row: 0,
            col: 7,
            visible: true,
        }),
        scroll: Some(ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 30,
            viewport_rows: 3,
        }),
        mouse: Some(PaneMouseMode {
            alternate_screen: false,
            tracking: PaneMouseTracking::None,
            encoding: PaneMouseEncoding::Default,
        }),
    }
}

fn row(row: u16, text: &str, style: u32) -> RowPatch {
    RowPatch {
        row,
        runs: vec![Run {
            col: 0,
            style,
            text: text.to_owned(),
        }],
    }
}

fn app_with_frames(config: Config) -> App<FakeLink> {
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(frame("p1", 1, true, vec![row(0, "LEFT", 0)])),
        ServerMsg::TerminalFrame(frame("p2", 1, true, vec![row(0, "RIGHT", 1)])),
    ]);
    let mut app = App::new(config, HostAppearance::Dark, link).expect("valid app config");
    app.poll();
    app
}

fn app_with_pane_text(config: Config, rows: Vec<RowPatch>) -> App<FakeLink> {
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(frame("p1", 1, true, rows)),
        ServerMsg::TerminalFrame(frame("p2", 1, true, vec![row(0, "RIGHT", 1)])),
    ]);
    let mut app = App::new(config, HostAppearance::Dark, link).expect("valid app config");
    app.poll();
    app
}

fn snapshot_with_background_agent(agent: &str) -> SessionSnapshot {
    let mut snapshot = session_snapshot();
    snapshot.workspaces.push(WorkspaceInfo {
        workspace_id: "w2".to_owned(),
        label: "beta".to_owned(),
        cwd: "C:/other".to_owned(),
        focused: false,
        revision: 7,
        tabs: vec!["t2".to_owned()],
        tokens: BTreeMap::new(),
        worktree: None,
    });
    snapshot.tabs.push(TabInfo {
        tab_id: "t2".to_owned(),
        workspace_id: "w2".to_owned(),
        label: "agents".to_owned(),
        focused: false,
        panes: vec!["p3".to_owned()],
        zoomed: None,
    });
    let mut pane = pane_info("p3", "background", false);
    pane.workspace_id = "w2".to_owned();
    pane.tab_id = "t2".to_owned();
    pane.agent_status = AgentStatus::Working;
    pane.agent = Some(agent.to_owned());
    pane.agent_name = Some("background-agent".to_owned());
    snapshot.panes.push(pane);
    snapshot.agents.push(AgentInfo {
        agent: agent.to_owned(),
        agent_status: AgentStatus::Working,
        pane_id: "p3".to_owned(),
        terminal_id: "term-p3".to_owned(),
        workspace_id: "w2".to_owned(),
        tab_id: "t2".to_owned(),
        cwd: "C:/other".to_owned(),
        focused: false,
        revision: 7,
        state_change_seq: 4,
        name: Some("background-agent".to_owned()),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_session: None,
        foreground_cwd: None,
        tokens: BTreeMap::new(),
    });
    snapshot
}

fn render_buffer(app: &App<FakeLink>, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render_app(app, frame))
        .expect("headless render");
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

fn press(app: &mut App<FakeLink>, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, modifiers))
        .expect("key handling");
}

fn press_with_clipboard(
    app: &mut App<FakeLink>,
    clipboard: &mut FakeClipboard,
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    app.handle_key_with_clipboard(KeyEvent::new(code, modifiers), clipboard)
        .expect("key handling with clipboard");
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[derive(Debug, Default)]
struct FakeClipboard {
    text: String,
    writes: Vec<String>,
}

impl Clipboard for FakeClipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError> {
        Ok(self.text.clone())
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.text = text.to_owned();
        self.writes.push(text.to_owned());
        Ok(())
    }

    fn has_image(&mut self) -> Result<bool, ClipboardError> {
        Ok(false)
    }
}

#[derive(Debug, Default)]
struct RecordingSoundPlayer {
    requests: Vec<SoundRequest>,
}

impl SoundPlayer for RecordingSoundPlayer {
    fn play(&mut self, request: &SoundRequest) -> Result<(), SoundError> {
        self.requests.push(request.clone());
        Ok(())
    }
}

#[derive(Debug, Default)]
struct RecordingEditor {
    paths: Vec<std::path::PathBuf>,
}

impl EditorLauncher for RecordingEditor {
    fn open(&mut self, path: &std::path::Path) -> Result<(), EditorError> {
        self.paths.push(path.to_owned());
        Ok(())
    }
}

fn request_methods(messages: &[ClientMsg]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| match message {
            ClientMsg::Request(request) => Some(request.method.as_str()),
            ClientMsg::Input(_) => None,
        })
        .collect()
}

#[test]
fn renders_two_panes_sidebar_hidden_single_tab_and_accent_border() {
    let app = app_with_frames(test_config());
    let buffer = render_buffer(&app, 100, 30);
    let text = buffer_text(&buffer);

    assert!(text.contains("alpha"));
    assert!(text.contains("LEFT"));
    assert!(text.contains("RIGHT"));
    assert_eq!(
        buffer.cell((20, 0)).unwrap().symbol(),
        "╭",
        "the pane border starts on the first row when the single-tab bar is hidden"
    );
    assert_eq!(buffer.cell((21, 1)).unwrap().symbol(), "L");
    assert_eq!(buffer.cell((61, 1)).unwrap().symbol(), "R");
    assert_eq!(
        buffer.cell((20, 0)).unwrap().fg,
        ratatui_color(app.theme().tokens.accent)
    );
}

#[test]
fn default_look_uses_exact_palette_split_sidebar_tabs_and_rounded_panes() {
    let mut config = Config::default();
    config.onboarding = Some(false);
    // This golden pins the gap layout; the shipped default is pane_gaps=false.
    config.ui.pane_gaps = true;
    let mut app = app_with_frames(config);
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.tabs.push(TabInfo {
        tab_id: "t2".to_owned(),
        workspace_id: "w1".to_owned(),
        label: "perf".to_owned(),
        focused: false,
        panes: Vec::new(),
        zoomed: None,
    });
    app.set_snapshot(snapshot);
    let buffer = render_buffer(&app, 120, 30);
    let text = buffer_text(&buffer);

    assert_eq!(app.theme().name, "starcil");
    assert_eq!(buffer.cell((119, 29)).unwrap().bg, RatatuiColor::Rgb(0x1a, 0x1d, 0x22));
    assert_eq!(buffer.cell((0, 29)).unwrap().bg, RatatuiColor::Rgb(0x15, 0x18, 0x1d));
    // One-row tab bar: active block on row 0, panes from row 1.
    assert_eq!(buffer.cell((26, 0)).unwrap().bg, RatatuiColor::Rgb(0x4a, 0x9e, 0xff));
    assert_eq!(buffer.cell((26, 0)).unwrap().fg, RatatuiColor::Rgb(0x0c, 0x0c, 0x0b));
    assert!(buffer.cell((26, 0)).unwrap().modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(buffer.cell((36, 0)).unwrap().bg, RatatuiColor::Rgb(0x20, 0x24, 0x2c));
    assert_eq!(buffer.cell((27, 2)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((27, 2)).unwrap().fg, RatatuiColor::Rgb(0x4a, 0x9e, 0xff));
    assert_eq!(buffer.cell((29, 2)).unwrap().symbol(), "l", "title starts two cells past the corner");
    assert_eq!(buffer.cell((73, 2)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((73, 2)).unwrap().fg, RatatuiColor::Rgb(0x2b, 0x30, 0x38));
    assert_eq!(buffer.cell((72, 2)).unwrap().symbol(), " ", "split panes keep one gap cell");
    assert!(text.contains("1 alpha"));
    assert!(text.contains("new"));
    assert!(text.contains("menu"));
    assert!(text.contains("agents"));
    assert!(text.contains("⠋ left·codex"));
    assert!(text.contains("working"));
    assert_eq!(buffer.cell((0, 1)).unwrap().bg, RatatuiColor::Rgb(0x20, 0x24, 0x2c));
    assert_eq!(buffer.cell((0, 16)).unwrap().bg, RatatuiColor::Rgb(0x20, 0x24, 0x2c));
    assert_eq!(buffer.cell((2, 17)).unwrap().fg, RatatuiColor::Rgb(0xe6, 0xc0, 0x60));
    assert!(!text.contains("TERMINAL"), "the default layout has no global footer");
}

#[test]
fn a_single_pane_has_no_border_title_or_gap_and_fills_the_tab_body() {
    let mut config = Config::default();
    config.onboarding = Some(false);
    let mut app = app_with_frames(config);
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.tabs[0].panes = vec!["p1".to_owned()];
    snapshot.layouts[0].panes.truncate(1);
    app.set_snapshot(snapshot);

    let buffer = render_buffer(&app, 120, 30);
    assert_eq!(buffer.cell((26, 1)).unwrap().symbol(), "L");
    assert_ne!(buffer.cell((26, 1)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((26, 1)).unwrap().bg, RatatuiColor::Rgb(0x1a, 0x1d, 0x22));
    let mut clipboard = FakeClipboard::default();
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 26, 1),
            Rect::new(0, 0, 120, 30),
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::BeginSelection { ref pane_id, row: 0, col: 0 } if pane_id == "p1"
    ));
}

#[test]
fn mode_machine_routes_actions_and_never_leaks_consumed_keys() {
    let mut app = app_with_frames(test_config());
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.tabs.extend([
        TabInfo {
            tab_id: "t2".to_owned(),
            workspace_id: "w1".to_owned(),
            label: "second".to_owned(),
            focused: false,
            panes: Vec::new(),
            zoomed: None,
        },
        TabInfo {
            tab_id: "t3".to_owned(),
            workspace_id: "w1".to_owned(),
            label: "third".to_owned(),
            focused: false,
            panes: Vec::new(),
            zoomed: None,
        },
    ]);
    app.set_snapshot(snapshot);
    app.link_mut().take_sent();

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(app.mode(), &Mode::Prefix);
    press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    let sent = app.link_mut().take_sent();
    assert_eq!(request_methods(&sent), vec!["pane.split"]);
    assert!(sent.iter().all(|message| matches!(message, ClientMsg::Request(_))));

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.local_zoom(), Some("p1"));
    assert_eq!(request_methods(&app.link_mut().take_sent()), vec!["pane.zoom"]);

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
    assert_eq!(
        request_methods(&app.link_mut().take_sent()),
        vec!["tab.focus"]
    );

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Navigate);
    press(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(
        request_methods(&app.link_mut().take_sent()),
        vec!["pane.focus_direction"]
    );
    assert_eq!(app.mode(), &Mode::Navigate);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Resize);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(request_methods(&app.link_mut().take_sent()), vec!["pane.resize"]);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);

    press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    let sent = app.link_mut().take_sent();
    assert!(matches!(
        sent.as_slice(),
        [ClientMsg::Input(InputFrame::Text { pane_id, text })]
            if pane_id == "p1" && text == "a"
    ));

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    let sent = app.link_mut().take_sent();
    assert!(matches!(
        sent.as_slice(),
        [ClientMsg::Input(InputFrame::Keys { pane_id, keys })]
            if pane_id == "p1" && keys == &["ctrl+b"]
    ));

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(matches!(
        app.link_mut().take_sent().as_slice(),
        [ClientMsg::Input(InputFrame::Keys { pane_id, keys })]
            if pane_id == "p1" && keys == &["ctrl+v"]
    ));
    assert!(app.effects().is_empty());

    app.set_remote_client(true);
    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(app.link().sent().is_empty(), "C3 clipboard seams stay local");
    assert_eq!(app.take_effects(), vec![AppEffect::RemoteImagePaste]);
}

#[test]
fn help_overlay_is_generated_from_the_complete_custom_effective_keymap() {
    let mut config = test_config();
    config.keys.help = KeyBindings::one("prefix+f12");
    config.keys.new_tab = KeyBindings::one("ctrl+alt+c");
    let mut app = app_with_frames(config);
    app.dispatch_action(Action::Help, None).expect("open help");

    let expected = app
        .keymap()
        .terminal
        .iter()
        .chain(app.keymap().navigate.iter())
        .map(|(chord, binding)| (chord.to_string(), binding.action.name()))
        .collect::<Vec<_>>();
    let text = buffer_text(&render_buffer(&app, 150, 100));
    assert!(text.contains("prefix+f12"));
    assert!(text.contains("ctrl+alt+c"));
    for (chord, action) in expected {
        assert!(text.contains(&chord), "missing effective chord {chord}");
        assert!(text.contains(action), "missing bound action {action}");
    }
}

#[test]
fn pane_mirror_applies_three_patches_reuses_styles_and_requests_gap_resync() {
    let mut mirror = PaneMirror::new("p1");
    assert_eq!(
        mirror.apply(&frame("p1", 1, true, vec![row(0, "abc", 1)])),
        ApplyOutcome::Applied
    );
    assert_eq!(mirror.styles().len(), 2);
    assert_eq!(mirror.apply(&frame("p1", 2, false, vec![row(0, "def", 1)])), ApplyOutcome::Applied);
    assert_eq!(mirror.apply(&frame("p1", 3, false, vec![row(1, "ghi", 0)])), ApplyOutcome::Applied);
    assert_eq!(mirror.apply(&frame("p1", 4, false, vec![row(0, "jkl", 1)])), ApplyOutcome::Applied);
    assert!(mirror.line_text(0).starts_with("jkl"));
    assert!(mirror.line_text(1).starts_with("ghi"));
    assert_eq!(mirror.styles().len(), 2, "empty patch tables reuse the snapshot table");
    assert_eq!(
        mirror.cell(0, 0).unwrap().style,
        protocol_style(mirror.styles()[1])
    );
    assert_eq!(
        mirror.apply(&frame("p1", 6, false, vec![row(2, "gap", 0)])),
        ApplyOutcome::ResyncRequired
    );
    assert_eq!(
        mirror.apply(&frame("p1", 7, false, vec![row(2, "late", 0)])),
        ApplyOutcome::AwaitingSnapshot
    );

    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(frame("p1", 1, true, vec![row(0, "ok", 0)])),
        ServerMsg::TerminalFrame(frame("p1", 3, false, vec![row(0, "gap", 0)])),
    ]);
    let mut app = App::new(test_config(), HostAppearance::Dark, link).unwrap();
    app.poll();
    assert!(matches!(
        app.link().sent(),
        [ClientMsg::Input(InputFrame::Resync { pane_id })] if pane_id == "p1"
    ));
}

#[test]
fn incremental_style_tables_do_not_restyle_untouched_cells() {
    let orange = StyleDef {
        fg: 0x01_ff_8a_00,
        bg: 0,
        attrs: 0,
    };
    let white = StyleDef {
        fg: 0x01_ff_ff_ff,
        bg: 0,
        attrs: 0,
    };
    let mut first = frame("p1", 1, true, vec![row(0, "A", 0)]);
    first.styles = vec![orange];
    let mut second = frame("p1", 2, false, vec![row(1, "B", 0)]);
    second.styles = vec![white];
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(first),
        ServerMsg::TerminalFrame(second),
    ]);
    let mut app = App::new(test_config(), HostAppearance::Dark, link).unwrap();
    app.poll();

    let buffer = render_buffer(&app, 100, 30);
    assert_eq!(buffer.cell((21, 1)).unwrap().symbol(), "A");
    assert_eq!(
        buffer.cell((21, 1)).unwrap().fg,
        RatatuiColor::Rgb(0xff, 0x8a, 0x00),
        "an untouched cell keeps the concrete style resolved from its own frame"
    );
    assert_eq!(buffer.cell((21, 2)).unwrap().fg, RatatuiColor::Rgb(0xff, 0xff, 0xff));
}

#[test]
fn partial_row_patch_clears_stale_cells_and_styles_beyond_its_runs() {
    let mut mirror = PaneMirror::new("p1");
    assert_eq!(
        mirror.apply(&frame("p1", 1, true, vec![row(0, "stale glyphs", 1)])),
        ApplyOutcome::Applied
    );
    assert_eq!(
        mirror.apply(&frame("p1", 2, false, vec![row(0, "new", 0)])),
        ApplyOutcome::Applied
    );

    assert!(mirror.line_text(0).starts_with("new"));
    for col in 3..mirror.cols() {
        let cell = mirror.cell(0, col).unwrap();
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.style, ratatui::style::Style::default());
    }
}

#[test]
fn full_snapshot_clears_rows_omitted_by_the_new_frame() {
    let mut mirror = PaneMirror::new("p1");
    assert_eq!(
        mirror.apply(&frame(
            "p1",
            1,
            true,
            vec![row(0, "old", 0), row(1, "stale wrapped row", 0)],
        )),
        ApplyOutcome::Applied
    );

    assert_eq!(
        mirror.apply(&frame("p1", 2, true, vec![row(0, "fresh", 0)])),
        ApplyOutcome::Applied
    );
    assert!(mirror.line_text(0).starts_with("fresh"));
    assert!(mirror.line_text(1).trim().is_empty());
    assert!(!mirror.screen_text().contains("stale wrapped row"));
}

#[test]
fn session_snapshot_drops_only_mirrors_whose_terminal_id_changed() {
    let mut app = app_with_frames(test_config());
    assert!(app.mirror("p1").unwrap().line_text(0).starts_with("LEFT"));
    assert!(app.mirror("p2").unwrap().line_text(0).starts_with("RIGHT"));

    let mut reattached = app.snapshot().unwrap().clone();
    reattached
        .panes
        .iter_mut()
        .find(|pane| pane.pane_id == "p1")
        .unwrap()
        .terminal_id = "term-p1-reattached".to_owned();
    app.set_snapshot(reattached);

    assert_eq!(app.mirror("p1").unwrap().screen_text(), "");
    assert!(
        app.mirror("p2").unwrap().line_text(0).starts_with("RIGHT"),
        "an unchanged terminal keeps its mirror"
    );

    app.link_mut()
        .push(ServerMsg::TerminalFrame(frame("p1", 1, true, vec![row(0, "NEW", 0)])));
    app.poll();
    assert!(app.mirror("p1").unwrap().line_text(0).starts_with("NEW"));
}

#[test]
fn themes_change_rendered_accent_cells_and_custom_override_wins() {
    let catppuccin = app_with_frames(test_config());
    let cat_color = render_buffer(&catppuccin, 100, 30)
        .cell((60, 0))
        .unwrap()
        .fg;

    let mut dracula_config = test_config();
    dracula_config.theme.name = "dracula".to_owned();
    let dracula = app_with_frames(dracula_config);
    let dracula_color = render_buffer(&dracula, 100, 30)
        .cell((60, 0))
        .unwrap()
        .fg;
    assert_ne!(cat_color, dracula_color);

    let mut custom_config = test_config();
    custom_config
        .theme
        .custom
        .insert("accent".to_owned(), "#123456".to_owned());
    let custom = app_with_frames(custom_config);
    let custom_color = render_buffer(&custom, 100, 30)
        .cell((20, 0))
        .unwrap()
        .fg;
    assert_eq!(custom_color, RatatuiColor::Rgb(0x12, 0x34, 0x56));

    let style = protocol_style(StyleDef {
        fg: 0x01_ab_cd_ef,
        bg: 0x02_00_00_c8,
        attrs: attr_bits::BOLD | attr_bits::UNDERLINE,
    });
    assert_eq!(style.fg, Some(RatatuiColor::Rgb(0xab, 0xcd, 0xef)));
    assert_eq!(style.bg, Some(RatatuiColor::Indexed(200)));
}

#[test]
fn mobile_layout_stacks_panes_and_zoom_fills_the_content_column() {
    let mut config = test_config();
    config.ui.mobile_width_threshold = 64;
    let mut app = app_with_frames(config);
    let mobile = render_buffer(&app, 60, 24);
    assert_eq!(mobile.cell((1, 2)).unwrap().symbol(), "L");
    assert_eq!(mobile.cell((1, 13)).unwrap().symbol(), "R");

    app.dispatch_action(Action::Zoom, None).unwrap();
    let zoomed = render_buffer(&app, 60, 24);
    let text = buffer_text(&zoomed);
    assert!(text.contains("LEFT"));
    assert!(!text.contains("RIGHT"));
}

#[test]
fn sidebar_divider_drag_resizes_runtime_width_within_configured_bounds() {
    let mut app = app_with_frames(test_config());
    app.link_mut().take_sent();
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 19, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 29, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::ResizeSidebar { width: 30 }
    );
    assert_eq!(app.config().ui.sidebar_width, 30);
    assert_eq!(UiGeometry::calculate(&app, area).sidebar.width, 30);

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 0, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::ResizeSidebar { width: 18 }
    );
    assert_eq!(app.config().ui.sidebar_width, app.config().ui.sidebar_min_width);

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 99, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::ResizeSidebar { width: 36 }
    );
    assert_eq!(app.config().ui.sidebar_width, app.config().ui.sidebar_max_width);
    assert!(app.link().sent().is_empty(), "sidebar resizing stays client-local");

    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 99, 2),
        area,
        &mut clipboard,
    )
    .unwrap();
}

#[test]
fn divider_border_click_focuses_neighbor_but_drag_only_resizes() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut click = app_with_frames(test_config());
    click.link_mut().take_sent();

    assert_eq!(
        click
            .handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 60, 0),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(
        click
            .handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), 60, 0),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::FocusPane("p2".to_owned())
    );
    assert_eq!(click.snapshot().unwrap().focused_pane_id, "p2");
    assert_eq!(request_methods(click.link().sent()), vec!["pane.focus"]);

    let mut drag = app_with_frames(test_config());
    drag.link_mut().take_sent();
    drag.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 60, 0),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(
        drag.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 64, 0),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Resize { ref pane_id, direction: "right", amount }
            if pane_id == "p1" && (amount - 0.05).abs() < f64::EPSILON
    ));
    assert_eq!(
        drag.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), 64, 0),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(drag.snapshot().unwrap().focused_pane_id, "p1");
    assert_eq!(request_methods(drag.link().sent()), vec!["pane.resize"]);
}

#[test]
fn settings_modal_swallows_background_mouse_and_outside_click_closes() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config()).with_config_path(None);
    app.dispatch_action(Action::Settings, None).unwrap();
    app.link_mut().take_sent();

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::ScrollUp, 90, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(app.scrollback().offset("p2"), 0);
    assert!(matches!(app.mode(), Mode::Modal(crate::Modal::Settings)));

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 90, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::CloseModal
    );
    assert_eq!(app.mode(), &Mode::Terminal);
    assert_eq!(app.snapshot().unwrap().focused_pane_id, "p1");
    assert!(app.selection().selection().is_none());
    assert!(app.link().sent().is_empty());
}

#[test]
fn context_menu_hover_and_click_execute_the_hit_item() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 21, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.link_mut().take_sent();
    let modal = match app.mode() {
        Mode::Modal(modal @ crate::Modal::ContextMenu { .. }) => modal.clone(),
        mode => panic!("expected context menu, got {mode:?}"),
    };
    let rect = modal_rect(&app, area, &modal);

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Moved, rect.x + 1, rect.y + 3),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::ContextMenuItem {
            index: 2,
            activate: false,
        }
    );
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { selected: 2, .. })
    ));

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 1, rect.y + 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::ContextMenuItem {
            index: 1,
            activate: true,
        }
    );
    assert_eq!(app.mode(), &Mode::Terminal);
    assert_eq!(clipboard.text, "LEFT");
    assert_eq!(clipboard.writes, vec!["LEFT"]);
}

#[test]
fn settings_modal_clicks_switch_sections_select_rows_and_activate_selected_rows() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config()).with_config_path(None);
    app.dispatch_action(Action::Settings, None).unwrap();
    app.link_mut().take_sent();

    let modal = match app.mode() {
        Mode::Modal(modal @ crate::Modal::Settings) => modal.clone(),
        mode => panic!("expected settings modal, got {mode:?}"),
    };
    let rect = modal_rect(&app, area, &modal);
    let panes_index = 3;
    let preceding_width = SECTION_NAMES[..panes_index]
        .iter()
        .map(|name| name.chars().count() as u16 + 3)
        .sum::<u16>();
    assert_eq!(
        app.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                rect.x + 3 + preceding_width,
                rect.y + 2,
            ),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::SettingsSection(panes_index)
    );
    assert_eq!(app.settings().section_index(), panes_index);

    let modal = match app.mode() {
        Mode::Modal(modal @ crate::Modal::Settings) => modal.clone(),
        mode => panic!("expected settings modal, got {mode:?}"),
    };
    let rect = modal_rect(&app, area, &modal);
    let borders_before = app.config().ui.pane_borders;
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 2, rect.y + 4),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::SettingsRow {
            index: 0,
            activate: true,
        }
    );
    assert_eq!(app.config().ui.pane_borders, !borders_before);

    let gaps_before = app.config().ui.pane_gaps;
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 2, rect.y + 5),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::SettingsRow {
            index: 1,
            activate: false,
        }
    );
    assert_eq!(app.config().ui.pane_gaps, gaps_before);
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 2, rect.y + 5),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::SettingsRow {
            index: 1,
            activate: true,
        }
    );
    assert_eq!(app.config().ui.pane_gaps, !gaps_before);
    assert_eq!(
        request_methods(app.link().sent()),
        vec!["server.reload_config", "server.reload_config"]
    );
}

#[test]
fn menu_popup_opens_from_sidebar_and_renders_only_base_rows_without_update() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());

    assert_eq!(app.update_ready(), None);
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::OpenMenu
    );
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::Menu { selected: 0 })
    ));
    let rendered = buffer_text(&render_buffer(&app, area.width, area.height));
    assert!(rendered.contains("settings"));
    assert!(rendered.contains("keybinds"));
    assert!(rendered.contains("reload config"));
    assert!(rendered.contains("detach"));
    assert!(!rendered.contains("update ready"));

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 99, 29),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::CloseModal
    );
    assert_eq!(app.mode(), &Mode::Terminal);
}

#[test]
fn staged_update_row_renders_and_keyboard_activation_emits_apply_effect() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut config = test_config();
    config.ui.toast.delivery = ToastDelivery::Starcil;
    let mut app = app_with_frames(config);

    app.set_update_ready("0.2.0");
    assert_eq!(app.update_ready(), Some("0.2.0"));
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(buffer_text(&render_buffer(&app, 100, 30)).contains("update ready"));

    for _ in 0..3 {
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::Menu { selected: 3 })
    ));
    // The row asks yes/no first; yes applies.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.mode(),
        &Mode::Modal(crate::Modal::UpdatePrompt {
            version: "0.2.0".to_owned(),
        })
    );
    assert!(app.take_effects().is_empty());
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    assert_eq!(app.take_effects(), vec![AppEffect::ApplyUpdate]);

    app.notify("Update staged");
    assert_eq!(app.toasts().last().unwrap().message, "Update staged");
}

#[test]
fn menu_mouse_hover_and_reload_click_send_one_request_then_close() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.link_mut().take_sent();
    let modal = match app.mode() {
        Mode::Modal(modal @ crate::Modal::Menu { .. }) => modal.clone(),
        mode => panic!("expected menu modal, got {mode:?}"),
    };
    let rect = modal_rect(&app, area, &modal);

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Moved, rect.x + 1, rect.y + 3),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::MenuItem {
            index: 2,
            activate: false,
        }
    );
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::Menu { selected: 2 })
    ));
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 1, rect.y + 3),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::MenuItem {
            index: 2,
            activate: true,
        }
    );
    assert_eq!(app.mode(), &Mode::Terminal);
    assert_eq!(request_methods(app.link().sent()), vec!["server.reload_config"]);
}

#[test]
fn menu_keyboard_opens_settings_without_resurfacing_and_escape_closes_menu() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config()).with_config_path(None);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
        area,
        &mut clipboard,
    )
    .unwrap();
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode(), Mode::Modal(crate::Modal::Settings)));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(app.mode(), Mode::Modal(crate::Modal::Menu { .. })));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
}

#[test]
fn mouse_routes_chrome_and_panes_scrolls_locally_and_drives_the_context_menu() {
    let mut routing_config = test_config();
    routing_config.ui.hide_tab_bar_when_single_tab = false;
    let mut app = app_with_frames(routing_config);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 1, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::FocusWorkspace(ref id) if id == "w1"
    ));
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 1, 16),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::FocusAgent(ref id) if id == "p1"
    ));
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 21, 0),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::FocusTab(ref id) if id == "t1"
    ));
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 61, 3),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::BeginSelection { ref pane_id, .. } if pane_id == "p2"
    ));
    assert_eq!(app.snapshot().unwrap().focused_pane_id, "p2");
    assert_eq!(
        request_methods(app.link().sent()),
        vec!["workspace.focus", "agent.focus", "tab.focus", "pane.focus"]
    );

    let mut controls_config = test_config();
    controls_config.ui.hide_tab_bar_when_single_tab = false;
    let mut controls = app_with_frames(controls_config);
    assert_eq!(
        controls
            .handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 1, 13),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::NewWorkspace
    );
    assert_eq!(
        controls
            .handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 31, 0),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::NewTab
    );
    assert_eq!(
        request_methods(controls.link().sent()),
        vec!["workspace.create", "tab.create"]
    );
    assert_eq!(
        controls
            .handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::OpenMenu
    );
    assert!(matches!(
        controls.mode(),
        Mode::Modal(crate::Modal::Menu { selected: 0 })
    ));

    let mut app = app_with_frames(test_config());
    app.link_mut().take_sent();
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::ScrollUp, 21, 1),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Scroll { lines: 3, alternate_screen: false, .. }
    ));
    assert_eq!(app.scrollback().offset("p1"), 3);
    assert!(buffer_text(&render_buffer(&app, 100, 30)).contains("[3 lines up]"));
    assert!(matches!(
        app.link_mut().take_sent().as_slice(),
        [ClientMsg::Input(InputFrame::Scroll { pane_id, delta: 3 })] if pane_id == "p1"
    ));

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 59, 10),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Ignored
    );
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 63, 10),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Resize { ref pane_id, direction: "right", amount }
            if pane_id == "p1" && (amount - 0.05).abs() < f64::EPSILON
    ));
    assert!(matches!(
        app.link().sent(),
        [ClientMsg::Request(request)]
            if request.method == "pane.resize"
                && request.params.get("pane_id").and_then(serde_json::Value::as_str) == Some("p1")
                && request.params.get("direction").and_then(serde_json::Value::as_str) == Some("right")
                && request.params.get("amount").and_then(serde_json::Value::as_f64)
                    .is_some_and(|amount| (amount - 0.05).abs() < f64::EPSILON)
    ));
    app.link_mut().take_sent();
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 63, 10),
        area,
        &mut clipboard,
    )
    .unwrap();

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 21, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    let menu = buffer_text(&render_buffer(&app, 100, 30));
    assert!(menu.contains("Copy selection"));
    assert!(menu.contains("Copy screen"));
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Down, KeyModifiers::NONE);
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(clipboard.text, "LEFT");

    let mut alternate = frame("p1", 1, true, vec![row(0, "ALT", 0)]);
    alternate.mouse = Some(PaneMouseMode {
        alternate_screen: true,
        tracking: PaneMouseTracking::None,
        encoding: PaneMouseEncoding::Default,
    });
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(alternate),
    ]);
    let mut alt_app = App::new(test_config(), HostAppearance::Dark, link).unwrap();
    alt_app.poll();
    alt_app
        .handle_mouse(
            mouse(MouseEventKind::ScrollUp, 21, 1),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert!(matches!(
        alt_app.link().sent(),
        [ClientMsg::Input(InputFrame::Keys { pane_id, keys })]
            if pane_id == "p1" && keys == &vec!["up".to_owned(); 3]
    ));

    let mut disabled_config = test_config();
    disabled_config.ui.mouse_capture = false;
    let mut disabled = app_with_frames(disabled_config);
    assert!(!disabled.wants_mouse_capture());
    assert_eq!(
        disabled
            .handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 61, 1),
                area,
                &mut clipboard,
            )
            .unwrap(),
        MouseAction::Ignored
    );
}

#[test]
fn tracking_pane_forwards_wheel_as_sgr_bytes_without_scrollback() {
    let mut tracked = frame("p1", 1, true, vec![row(0, "TRACK", 0)]);
    tracked.mouse = Some(PaneMouseMode {
        alternate_screen: true,
        tracking: PaneMouseTracking::PressRelease,
        encoding: PaneMouseEncoding::Sgr,
    });
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(tracked),
    ]);
    let mut app = App::new(test_config(), HostAppearance::Dark, link).unwrap();
    app.poll();
    let mut clipboard = FakeClipboard::default();

    let action = app
        .handle_mouse(
            mouse(MouseEventKind::ScrollUp, 21, 1),
            Rect::new(0, 0, 100, 30),
            &mut clipboard,
        )
        .unwrap();

    assert_eq!(
        action,
        MouseAction::Passthrough {
            pane_id: "p1".to_owned(),
            data_base64: "G1s8NjQ7MTsxTQ==".to_owned(),
        }
    );
    assert_eq!(app.scrollback().offset("p1"), 0);
    assert!(matches!(
        app.link().sent(),
        [ClientMsg::Input(InputFrame::Bytes { pane_id, data_base64 })]
            if pane_id == "p1" && data_base64 == "G1s8NjQ7MTsxTQ=="
    ));
}

#[test]
fn tracking_pane_left_click_forwards_bytes_and_focuses_without_selection() {
    let mut tracked = frame("p1", 1, true, vec![row(0, "TRACK", 0)]);
    tracked.mouse = Some(PaneMouseMode {
        alternate_screen: true,
        tracking: PaneMouseTracking::PressRelease,
        encoding: PaneMouseEncoding::Sgr,
    });
    let link = FakeLink::new([
        ServerMsg::SessionSnapshot(session_snapshot()),
        ServerMsg::TerminalFrame(tracked),
    ]);
    let mut app = App::new(test_config(), HostAppearance::Dark, link).unwrap();
    app.poll();
    app.link_mut().take_sent();
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 21, 1),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::MouseTracking { focus: true, .. }
    ));
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), 21, 1),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::MouseTracking { focus: false, .. }
    ));

    let sent = app.link().sent();
    assert_eq!(request_methods(sent), vec!["pane.focus"]);
    assert_eq!(
        sent.iter()
            .filter(|message| matches!(message, ClientMsg::Input(InputFrame::Bytes { .. })))
            .count(),
        2
    );
    assert!(app.selection().selection().is_none());
    assert!(clipboard.writes.is_empty());
}

#[test]
fn altgr_punctuation_forwards_as_text_while_letters_remain_chords() {
    let mut app = app_with_frames(test_config());
    app.link_mut().take_sent();

    press(
        &mut app,
        KeyCode::Char('\\'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(matches!(
        app.link_mut().take_sent().as_slice(),
        [ClientMsg::Input(InputFrame::Text { pane_id, text })]
            if pane_id == "p1" && text == "\\"
    ));

    press(
        &mut app,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(matches!(
        app.link().sent(),
        [ClientMsg::Input(InputFrame::Keys { pane_id, keys })]
            if pane_id == "p1" && keys == &vec!["ctrl+alt+a".to_owned()]
    ));
}

#[test]
fn selection_highlights_copies_on_demand_or_select_and_double_clicks_words() {
    let mut config = test_config();
    config.ui.copy_on_select = false;
    let mut app = app_with_frames(config);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 22, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 24, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 24, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(clipboard.writes.is_empty(), "copy_on_select=false must stay local");
    let selected = render_buffer(&app, 100, 30);
    assert_eq!(
        selected.cell((22, 1)).unwrap().bg,
        ratatui_color(app.theme().tokens.selection)
    );
    press_with_clipboard(
        &mut app,
        &mut clipboard,
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(clipboard.text, "EFT");
    assert_eq!(app.toasts().last().unwrap().position, ToastPosition::BottomCenter);
    assert!(buffer_text(&render_buffer(&app, 100, 30)).contains("Copied to clipboard"));

    app.link_mut().take_sent();
    clipboard.text = "pasted text".to_owned();
    press_with_clipboard(
        &mut app,
        &mut clipboard,
        KeyCode::Char('v'),
        KeyModifiers::CONTROL,
    );
    assert!(matches!(
        app.link().sent(),
        [ClientMsg::Input(InputFrame::Text { pane_id, text })]
            if pane_id == "p1" && text == "pasted text"
    ));

    let mut auto_config = test_config();
    auto_config.ui.copy_on_select = true;
    let mut auto = app_with_pane_text(auto_config, vec![row(0, "hello world", 0)]);
    let mut auto_clipboard = FakeClipboard::default();
    for _ in 0..2 {
        auto.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 22, 1),
            area,
            &mut auto_clipboard,
        )
        .unwrap();
        auto.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), 22, 1),
            area,
            &mut auto_clipboard,
        )
        .unwrap();
    }
    assert_eq!(auto_clipboard.text, "hello");
    assert!(auto.selection().is_selected("p1", 0, 4));
}

#[test]
fn right_release_alone_still_opens_the_context_menu() {
    // Some host terminals reserve the right-button press and forward only
    // the release; the menu must open either way, and a press+release pair
    // must not open it twice or instantly close it.
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Right), 25, 3),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { .. })
    ));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 25, 3),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Right), 25, 3),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(
        matches!(app.mode(), Mode::Modal(crate::Modal::ContextMenu { .. })),
        "press+release keeps exactly one menu open"
    );
    assert_eq!(
        app.modes()
            .iter()
            .filter(|mode| matches!(mode, Mode::Modal(_)))
            .count(),
        1
    );
}

#[test]
fn realistic_right_click_event_storm_keeps_one_context_menu_at_press_anchor() {
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.link_mut().take_sent();

    for event in [
        mouse(MouseEventKind::Moved, 23, 2),
        mouse(MouseEventKind::Moved, 24, 3),
        mouse(MouseEventKind::Down(MouseButton::Right), 25, 3),
        mouse(MouseEventKind::Drag(MouseButton::Right), 28, 4),
        mouse(MouseEventKind::Up(MouseButton::Right), 28, 4),
        mouse(MouseEventKind::Moved, 29, 4),
    ] {
        app.handle_mouse(event, area, &mut clipboard).unwrap();
    }

    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu {
            target,
            x: 25,
            y: 3,
            ..
        }) if *target == crate::ContextTarget::Pane("p1".to_owned())
    ));
    assert_eq!(
        app.modes()
            .iter()
            .filter(|mode| matches!(mode, Mode::Modal(crate::Modal::ContextMenu { .. })))
            .count(),
        1
    );
    assert_eq!(request_methods(app.link().sent()), vec!["pane.focus"]);
}

#[test]
fn right_click_outside_stray_settings_replaces_it_with_context_menu() {
    let mut app = app_with_frames(test_config()).with_config_path(None);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.dispatch_action(Action::Settings, None).unwrap();
    app.link_mut().take_sent();

    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 90, 2),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::CloseModalAndContextMenu {
            target: crate::ContextTarget::Pane("p2".to_owned()),
            x: 90,
            y: 2,
        }
    );
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu {
            target,
            x: 90,
            y: 2,
            selected: 0,
        }) if *target == crate::ContextTarget::Pane("p2".to_owned())
    ));
    assert_eq!(
        app.modes()
            .iter()
            .filter(|mode| matches!(mode, Mode::Modal(_)))
            .count(),
        1
    );
    assert_eq!(request_methods(app.link().sent()), vec!["pane.focus"]);
}

#[test]
fn second_right_click_reanchors_existing_context_menu() {
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 25, 3),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 80, 20),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::CloseModalAndContextMenu {
            target: crate::ContextTarget::Pane("p2".to_owned()),
            x: 80,
            y: 20,
        }
    );

    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu {
            target,
            x: 80,
            y: 20,
            selected: 0,
        }) if *target == crate::ContextTarget::Pane("p2".to_owned())
    ));
    assert_eq!(
        app.modes()
            .iter()
            .filter(|mode| matches!(mode, Mode::Modal(crate::Modal::ContextMenu { .. })))
            .count(),
        1
    );
}

#[test]
fn mouse_debug_overlay_is_opt_in_and_ticks_identical_events() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let event = mouse(MouseEventKind::Moved, 42, 7);

    let mut off = app_with_frames(test_config());
    off.set_mouse_debug(false);
    off.handle_mouse(event.clone(), area, &mut clipboard).unwrap();
    assert!(!buffer_text(&render_buffer(&off, 100, 30)).contains("mouse:"));

    let mut on = app_with_frames(test_config());
    on.set_mouse_debug(true);
    on.handle_mouse(event.clone(), area, &mut clipboard).unwrap();
    let first = buffer_text(&render_buffer(&on, 100, 30));
    assert!(first.contains("mouse: Moved 42,7 mods=NONE #1"));

    on.handle_mouse(event, area, &mut clipboard).unwrap();
    let second = buffer_text(&render_buffer(&on, 100, 30));
    assert!(second.contains("mouse: Moved 42,7 mods=NONE #2"));
}

#[test]
fn typing_while_scrolled_snaps_the_pane_back_to_live() {
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, 21, 1), area, &mut clipboard)
        .unwrap();
    assert_eq!(app.scrollback().offset("p1"), 3);
    app.link_mut().take_sent();

    press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    let sent = app.link_mut().take_sent();
    assert!(
        matches!(
            &sent[0],
            ClientMsg::Input(InputFrame::Scroll { pane_id, delta: -3 }) if pane_id == "p1"
        ),
        "typing must first return the view to the live bottom: {sent:?}"
    );
    assert!(
        matches!(
            &sent[1],
            ClientMsg::Input(InputFrame::Text { pane_id, text }) if pane_id == "p1" && text == "a"
        ),
        "the keystroke still reaches the pane after the snap: {sent:?}"
    );
    assert_eq!(app.scrollback().offset("p1"), 0);
}

#[test]
fn plain_click_focuses_the_pane_and_never_copies() {
    let mut config = test_config();
    config.ui.copy_on_select = true;
    let mut app = app_with_frames(config);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.link_mut().take_sent();

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 61, 2),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 61, 2),
        area,
        &mut clipboard,
    )
    .unwrap();

    assert!(clipboard.writes.is_empty(), "a click focuses, it must not copy");
    assert!(app.toasts().is_empty(), "no clipboard toast for a plain click");
    assert!(app.selection().selection().is_none(), "clicks leave no selection");
    assert_eq!(app.snapshot().unwrap().focused_pane_id, "p2");
    assert_eq!(request_methods(app.link().sent()), vec!["pane.focus"]);
}

#[test]
fn toasts_expire_after_their_ttl() {
    let mut config = test_config();
    config.ui.copy_on_select = true;
    let mut app = app_with_pane_text(config, vec![row(0, "hello world", 0)]);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);

    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 21, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 24, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 24, 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.toasts().len(), 1, "drag selection raises the copy toast");
    app.poll();
    assert_eq!(app.toasts().len(), 1, "fresh toasts survive polls");
    app.age_toasts(std::time::Duration::from_secs(5));
    app.poll();
    assert!(app.toasts().is_empty(), "toasts self-dismiss after the TTL");
}

#[test]
fn divider_drag_resizes_incrementally_and_grabs_from_the_neighbor_border() {
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.link_mut().take_sent();

    // Consecutive drag events each resize by the newly traveled cells only.
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 59, 10),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 63, 10),
        area,
        &mut clipboard,
    )
    .unwrap();
    let second = app
        .handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 67, 10),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert!(matches!(
        second,
        MouseAction::Resize { direction: "right", amount, .. }
            if (amount - 0.05).abs() < f64::EPSILON
    ));
    app.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 67, 10),
        area,
        &mut clipboard,
    )
    .unwrap();

    // The neighbor pane's leading border cell also arms the same divider.
    app.link_mut().take_sent();
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 60, 10),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Ignored
    );
    assert!(matches!(
        app.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 56, 10),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::Resize { ref pane_id, direction: "left", amount }
            if pane_id == "p1" && (amount - 0.05).abs() < f64::EPSILON
    ));
}

#[test]
fn copy_mode_yanks_exact_cells_and_scrollback_edit_opens_the_returned_document() {
    let mut app = app_with_pane_text(
        test_config(),
        vec![row(0, "alpha", 0), row(1, "bravo", 0)],
    );
    let mut clipboard = FakeClipboard::default();
    app.dispatch_action(Action::CopyMode, None).unwrap();
    assert_eq!(app.mode(), &Mode::Copy);
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Char('k'), KeyModifiers::NONE);
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Char('v'), KeyModifiers::NONE);
    for _ in 0..4 {
        press_with_clipboard(&mut app, &mut clipboard, KeyCode::Char('l'), KeyModifiers::NONE);
    }
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(clipboard.text, "bravo");
    assert_eq!(app.mode(), &Mode::Terminal);

    app.link_mut().take_sent();
    app.dispatch_action(Action::EditScrollback, None).unwrap();
    let sent = app.link_mut().take_sent();
    let request = match sent.as_slice() {
        [ClientMsg::Request(request)] => request,
        other => panic!("expected pane.read, got {other:?}"),
    };
    assert_eq!(request.method, "pane.read");
    assert_eq!(request.params["source"], "recent-unwrapped");
    let request_id = request.id.clone();
    app.link_mut().push(ServerMsg::Incoming(Incoming::Success(SuccessResponse {
        id: request_id,
        result: serde_json::json!({
            "type": "pane_read",
            "pane_id": "p1",
            "source": "recent-unwrapped",
            "format": "text",
            "lines": 2,
            "text": "alpha\nbravo\n"
        }),
    })));
    app.poll();
    assert!(matches!(app.effects(), [AppEffect::OpenEditor { .. }]));
    let mut editor = RecordingEditor::default();
    assert_eq!(app.launch_pending_editors(&mut editor).unwrap(), 1);
    let path = editor.paths.pop().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbravo\n");
    fs::remove_file(path).unwrap();
}

#[test]
fn settings_editor_persists_golden_toml_applies_theme_and_requests_reload() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "starcil-tui-settings-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("config.toml");
    let source = concat!(
        "# settings golden\n",
        "[theme]\n",
        "name = \"catppuccin\"\n\n",
        "[ui]\n",
        "accent = \"cyan\"\n\n",
        "[ui.toast]\n",
        "delivery = \"off\"\n",
    );
    fs::write(&path, source).unwrap();
    let mut settings_config = test_config();
    settings_config.ui.accent = "cyan".to_owned();
    let mut app = app_with_frames(settings_config).with_config_path(Some(path.clone()));
    app.link_mut().take_sent();
    app.dispatch_action(Action::Settings, None).unwrap();
    // Theme section: the accent row sits after the theme list; Down clamps.
    for _ in 0..(starcil_config::BUILTIN_THEME_NAMES.len() + 1) {
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    for character in "#123456".chars() {
        press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    // Tab through "sound" into "toasts", then cycle the delivery value.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE);

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        concat!(
            "# settings golden\n",
            "[theme]\n",
            "name = \"catppuccin\"\n\n",
            "[ui]\n",
            "accent = \"#123456\"\n\n",
            "[ui.toast]\n",
            "delivery = \"starcil\"\n",
        )
    );
    assert_eq!(
        app.theme().tokens.pane_border_active,
        "#123456".parse().unwrap()
    );
    let rendered = buffer_text(&render_buffer(&app, 100, 40));
    assert!(rendered.contains("settings"));
    assert!(rendered.contains("toasts"));
    assert!(rendered.contains("Toast delivery"));
    assert!(rendered.contains("starcil"));
    assert!(rendered.contains("tab section"));
    assert_eq!(
        request_methods(app.link().sent()),
        vec!["server.reload_config", "server.reload_config"]
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn settings_theme_section_applies_the_selected_theme_row() {
    let mut app = app_with_frames(test_config()).with_config_path(None);
    app.link_mut().take_sent();
    app.dispatch_action(Action::Settings, None).unwrap();
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.theme().name, starcil_config::BUILTIN_THEME_NAMES[1]);
    assert_eq!(
        request_methods(app.link().sent()),
        vec!["server.reload_config"]
    );
    let rendered = buffer_text(&render_buffer(&app, 100, 40));
    assert!(rendered.contains(&format!("{} ✓", starcil_config::BUILTIN_THEME_NAMES[1])));
}

#[test]
fn sounds_fire_only_for_background_enabled_agent_transitions() {
    let event = || {
        ServerMsg::Incoming(Incoming::Event(EventFrame {
            event: "pane.agent_status_changed".to_owned(),
            data: serde_json::json!({
                "pane_id": "p3",
                "agent_status": "blocked",
                "state_change_seq": 5
            }),
            revision: Some(8),
        }))
    };

    let mut background = App::new(
        test_config(),
        HostAppearance::Dark,
        FakeLink::new([ServerMsg::SessionSnapshot(snapshot_with_background_agent("codex")), event()]),
    )
    .unwrap();
    background.poll();
    let mut recorder = SoundController::new(RecordingSoundPlayer::default());
    assert!(background.play_pending_sounds(&mut recorder).is_empty());
    assert_eq!(recorder.player().requests.len(), 1);
    assert_eq!(recorder.player().requests[0].pane_id, "p3");

    let mut focused_snapshot = snapshot_with_background_agent("codex");
    focused_snapshot.focused_workspace_id = "w2".to_owned();
    let mut focused = App::new(
        test_config(),
        HostAppearance::Dark,
        FakeLink::new([ServerMsg::SessionSnapshot(focused_snapshot), event()]),
    )
    .unwrap();
    focused.poll();
    assert!(focused.take_sound_requests().is_empty());

    let mut off_config = test_config();
    off_config.ui.sound.agents.codex = SoundPolicy::Off;
    let mut disabled = App::new(
        off_config,
        HostAppearance::Dark,
        FakeLink::new([ServerMsg::SessionSnapshot(snapshot_with_background_agent("codex")), event()]),
    )
    .unwrap();
    disabled.poll();
    assert!(disabled.take_sound_requests().is_empty());
}

#[test]
fn onboarding_choice_persists_notification_delivery() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "starcil-tui-onboarding-{}-{unique}",
        std::process::id()
    ));
    let path = directory.join("config.toml");
    let link = FakeLink::new([ServerMsg::SessionSnapshot(session_snapshot())]);
    let mut app = App::new(Config::default(), HostAppearance::Dark, link)
        .unwrap()
        .with_config_path(Some(path.clone()));
    app.poll();
    app.link_mut().take_sent();
    assert!(matches!(app.mode(), Mode::Modal(crate::Modal::Onboarding)));

    press(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    assert_eq!(
        app.link()
            .sent()
            .iter()
            .filter(|message| matches!(message, ClientMsg::Input(_)))
            .count(),
        0,
        "the onboarding choice must be consumed before terminal routing"
    );
    let report = parse_config(&fs::read_to_string(&path).expect("onboarding config written"));
    assert!(report.is_valid(), "{:?}", report.diagnostics);
    assert_eq!(report.config.onboarding, Some(false));
    assert_eq!(report.config.ui.toast.delivery, ToastDelivery::Starcil);

    fs::remove_dir_all(directory).expect("remove isolated test directory");
}

fn sent_requests(messages: &[ClientMsg]) -> Vec<(String, serde_json::Value)> {
    messages
        .iter()
        .filter_map(|message| match message {
            ClientMsg::Request(request) => Some((request.method.clone(), request.params.clone())),
            ClientMsg::Input(_) => None,
        })
        .collect()
}

#[test]
fn right_click_on_a_tab_opens_the_tab_menu_and_close_closes_that_tab() {
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    let mut app = app_with_frames(config);
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.link_mut().take_sent();

    // Tab bar sits on row 0 right of the 20-cell sidebar; the first tab block
    // starts at x=20.
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 23, 0),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { target, selected: 0, .. })
            if *target == crate::ContextTarget::Tab("t1".to_owned())
    ));
    let text = buffer_text(&render_buffer(&app, 100, 30));
    assert!(text.contains(" Tab "), "tab menu title: {text}");
    assert!(text.contains("New tab") && text.contains("Rename") && text.contains("Close"));
    assert!(!text.contains("Copy selection"), "pane items must not leak into the tab menu");
    assert_eq!(request_methods(app.link().sent()), vec!["tab.focus"]);
    app.link_mut().take_sent();

    // Third item is Close: two downs + Enter.
    for key in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        app.handle_key(KeyEvent::new(key, KeyModifiers::NONE)).unwrap();
    }
    let requests = sent_requests(app.link().sent());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "tab.close");
    assert_eq!(requests[0].1["tab_id"], "t1");
    assert!(matches!(app.mode(), Mode::Terminal));
}

#[test]
fn right_click_on_a_workspace_opens_the_workspace_menu_with_explicit_rename_target() {
    let mut app = app_with_frames(test_config());
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    app.link_mut().take_sent();

    // Expanded sidebar: the first workspace row starts on row 1.
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 4, 2),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(
        action,
        MouseAction::ContextMenu {
            target: crate::ContextTarget::Workspace("w1".to_owned()),
            x: 4,
            y: 2,
        }
    );
    let text = buffer_text(&render_buffer(&app, 100, 30));
    assert!(text.contains(" Workspace "), "{text}");
    assert!(text.contains("New workspace"));
    assert_eq!(request_methods(app.link().sent()), vec!["workspace.focus"]);
    app.link_mut().take_sent();

    // Second item is Rename: the prompt must rename w1 even though the
    // server has not echoed the focus change yet.
    for key in [KeyCode::Down, KeyCode::Enter] {
        app.handle_key(KeyEvent::new(key, KeyModifiers::NONE)).unwrap();
    }
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::Prompt { kind: crate::PromptKind::RenameWorkspace, .. })
    ));
    for character in ['o', 'p', 's'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let requests = sent_requests(app.link().sent());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "workspace.rename");
    assert_eq!(requests[0].1["workspace_id"], "w1");
    assert_eq!(requests[0].1["label"], "ops");
}

#[test]
fn layout_area_reported_to_server_excludes_sidebar_and_tab_bar_and_only_on_change() {
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    let mut app = app_with_frames(config);
    app.link_mut().take_sent();

    let client_areas = |messages: &[ClientMsg]| {
        messages
            .iter()
            .filter_map(|message| match message {
                ClientMsg::Input(starcil_protocol::attach::InputFrame::ClientArea {
                    cols,
                    rows,
                }) => Some((*cols, *rows)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    app.sync_layout_area(Rect::new(0, 0, 100, 30));
    // 100 cols minus the 20-cell sidebar; 30 rows minus the 1-row tab bar
    // (the composer lives inside the pane now, not below it).
    assert_eq!(client_areas(app.link().sent()), vec![(80, 29)]);
    assert_eq!(app.reported_layout_area(), Some((80, 29)));
    app.link_mut().take_sent();

    app.sync_layout_area(Rect::new(0, 0, 100, 30));
    assert!(client_areas(app.link().sent()).is_empty(), "unchanged area is not resent");

    app.dispatch_action(Action::ToggleSidebar, None).unwrap();
    app.sync_layout_area(Rect::new(0, 0, 100, 30));
    let after_toggle = client_areas(app.link().sent());
    assert_eq!(after_toggle.len(), 1);
    assert!(after_toggle[0].0 > 80, "hiding or compacting the sidebar widens the pane area");

    app.link_mut().take_sent();
    app.sync_layout_area(Rect::new(0, 0, 120, 40));
    assert_eq!(client_areas(app.link().sent()).len(), 1, "terminal resize reports again");
}

/// The fixture's focused pane hosts an agent; composer features need a shell.
fn clear_agents<L: crate::ServerLink>(app: &mut App<L>) {
    let mut snapshot = app.snapshot().unwrap().clone();
    for pane in &mut snapshot.panes {
        pane.agent = None;
        pane.agent_name = None;
    }
    app.set_snapshot(snapshot);
}

fn fake_dock() -> Vec<crate::dock::DockAgent> {
    vec![
        crate::dock::DockAgent {
            name: "claude".to_owned(),
            command: "claude".to_owned(),
            glyph: crate::dock::dock_glyph("claude").map(|(glyph, _)| glyph),
        },
        crate::dock::DockAgent {
            name: "codex".to_owned(),
            command: "codex".to_owned(),
            glyph: crate::dock::dock_glyph("codex").map(|(glyph, _)| glyph),
        },
    ]
}

fn sent_inputs(messages: &[ClientMsg]) -> Vec<InputFrame> {
    messages
        .iter()
        .filter_map(|message| match message {
            ClientMsg::Input(frame) => Some(frame.clone()),
            ClientMsg::Request(_) => None,
        })
        .collect()
}





#[test]
fn context_menu_hover_works_with_a_stuck_right_button_and_arrows_move_selection() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());

    // Open the workspace menu with a right press whose release never arrives
    // (Warp behaviour) — the pointer then moves with the right bit stuck.
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 4, 2),
        area,
        &mut clipboard,
    )
    .unwrap();
    let rect = match app.mode() {
        Mode::Modal(modal @ crate::Modal::ContextMenu { .. }) => {
            modal_rect(&app, area, modal)
        }
        mode => panic!("expected context menu, got {mode:?}"),
    };
    // Motion over the second item arrives as Drag(Right), not Moved.
    app.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Right), rect.x + 2, rect.y + 2),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { selected: 1, .. })
    ));
    // Plain hover keeps working too.
    app.handle_mouse(
        mouse(MouseEventKind::Moved, rect.x + 2, rect.y + 1),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { selected: 0, .. })
    ));
    // Arrow keys walk the items.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { selected: 2, .. })
    ));
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(
        app.mode(),
        Mode::Modal(crate::Modal::ContextMenu { selected: 1, .. })
    ));
}

#[test]
fn sidebar_toggle_button_collapses_and_expands_by_mouse() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    assert_eq!(app.sidebar_state(), crate::SidebarState::Expanded);

    let geometry = UiGeometry::calculate(&app, area);
    let toggle = geometry.sidebar_toggle;
    assert!(toggle.width > 0);
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), toggle.x, toggle.y),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::ToggleSidebar);
    assert_eq!(app.sidebar_state(), crate::SidebarState::Compact);

    // The compact rail's header is the same button: it expands back.
    let geometry = UiGeometry::calculate(&app, area);
    let toggle = geometry.sidebar_toggle;
    assert!(toggle.width > 0);
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), toggle.x, toggle.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.sidebar_state(), crate::SidebarState::Expanded);
}

#[test]
fn composer_lives_inside_the_focused_shell_pane_and_hides_with_agents() {
    let area = Rect::new(0, 0, 100, 30);
    let mut app = app_with_frames(test_config());
    app.set_dock_agents(fake_dock());

    // The fixture's focused pane runs an agent: no composer, no shortcuts.
    assert!(UiGeometry::calculate(&app, area).composer.is_none());
    let text = buffer_text(&render_buffer(&app, 100, 30));
    assert!(!text.contains("❯"), "no input while an agent runs: {text}");
    assert!(!text.contains("open folder"));

    // A plain shell gets the whole panel inside its pane.
    clear_agents(&mut app);
    let geometry = UiGeometry::calculate(&app, area);
    let composer = geometry.composer.clone().expect("composer for the shell pane");
    assert_eq!(composer.pane_id, "p1");
    assert_eq!(
        composer.rows,
        9,
        "panel (border + 2 buttons + folder + border) + line + input + line + status row"
    );
    let pane = geometry.panes.iter().find(|pane| pane.pane_id == "p1").unwrap();
    assert_eq!(
        pane.content.y + pane.content.height,
        composer.dock_panel.y,
        "the pane's content ends where the composer begins"
    );
    assert_eq!(composer.dock_items[0].y, composer.dock_panel.y + 1);
    assert!(composer.dock_items[0].x >= pane.outer.x + 1, "stack sits inside the pane");
    assert_eq!(composer.folder_button.y, composer.dock_items[1].y + 1);
    assert_eq!(composer.border.y, composer.folder_button.y + 2, "panel bottom border between");
    assert_eq!(composer.input.y, composer.border.y + 1);
    assert_eq!(composer.bottom_border.y, composer.input.y + 1, "double line");
    assert_eq!(composer.spacer.y, composer.bottom_border.y + 1);
    // The cwd is the status row's left-aligned text, like Claude Code's.
    assert_eq!(composer.cwd_label.y, composer.spacer.y);
    assert_eq!(composer.cwd_label.x, composer.spacer.x + 1);

    let buffer = render_buffer(&app, 100, 30);
    let text = buffer_text(&buffer);
    let brand = ratatui_color(app.theme().tokens.brand);
    assert_eq!(brand, RatatuiColor::Rgb(0x8B, 0x5C, 0xF6));
    for line in [composer.border, composer.bottom_border] {
        assert_eq!(buffer.cell((line.x + 2, line.y)).unwrap().symbol(), "─");
        assert_eq!(buffer.cell((line.x + 2, line.y)).unwrap().fg, brand);
        assert_eq!(
            buffer.cell((line.x + line.width - 1, line.y)).unwrap().symbol(),
            "─",
            "the line runs the whole width; nothing overlays it"
        );
    }
    let status_row = (composer.spacer.x..composer.spacer.x + composer.spacer.width)
        .map(|x| buffer.cell((x, composer.spacer.y)).unwrap().symbol().to_owned())
        .collect::<String>();
    let cwd = app.dock_cwd_label().expect("fixture pane has a cwd");
    assert!(
        status_row.trim_start().starts_with(cwd.trim_start()),
        "cwd on the left of the status row: {status_row:?}"
    );
    assert_eq!(
        buffer.cell((composer.input.x, composer.input.y)).unwrap().symbol(),
        "❯"
    );
    assert!(text.contains("claude") && text.contains("codex"));
    assert!(text.contains("open folder"));
    assert!(text.contains("Workspaces"), "{text}");
}

#[test]
fn composer_reserves_pty_rows_on_the_server_and_releases_them() {
    let mut app = app_with_frames(test_config());
    app.set_dock_agents(fake_dock());
    app.link_mut().take_sent();
    let area = Rect::new(0, 0, 100, 30);

    let reservations = |messages: &[ClientMsg]| {
        messages
            .iter()
            .filter_map(|message| match message {
                ClientMsg::Input(InputFrame::ReserveRows { pane_id, rows }) => {
                    Some((pane_id.clone(), *rows))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // Agent focused: nothing reserved.
    app.sync_layout_area(area);
    assert!(reservations(app.link().sent()).is_empty());
    app.link_mut().take_sent();

    // Shell focused: the composer rows are ceded server-side.
    clear_agents(&mut app);
    app.sync_layout_area(area);
    assert_eq!(reservations(app.link().sent()), vec![("p1".to_owned(), 9)]);
    app.link_mut().take_sent();
    app.sync_layout_area(area);
    assert!(reservations(app.link().sent()).is_empty(), "no resend when unchanged");

    // The agent comes back (detection fires): the reservation clears.
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].agent = Some("claude".to_owned());
    app.set_snapshot(snapshot);
    app.sync_layout_area(area);
    assert_eq!(reservations(app.link().sent()), vec![("p1".to_owned(), 0)]);
}

#[test]
fn composer_focus_types_locally_and_enter_sends_text_then_enter() {
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();

    // A shell pane's composer owns the keyboard from the start: no click, no
    // chord (Cesar: typing must land below, never in the prompt at the top).
    assert!(app.composer_focused());

    for character in ['h', 'o', 'l', 'a', ' ', 'y', 'a'] {
        press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
    }
    assert_eq!(app.composer_text(), "hola ya");
    assert!(
        sent_inputs(app.link().sent()).is_empty(),
        "typing stays in the composer, nothing reaches the pane"
    );

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let inputs = sent_inputs(app.link().sent());
    assert_eq!(
        inputs,
        vec![
            InputFrame::Text {
                pane_id: "p1".to_owned(),
                text: "hola ya".to_owned(),
            },
            InputFrame::Keys {
                pane_id: "p1".to_owned(),
                keys: vec!["enter".to_owned()],
            },
        ],
        "text and Enter travel as separate writes"
    );
    assert_eq!(app.composer_text(), "");

    // Esc on an empty composer does nothing: the keyboard never moves up to
    // the prompt, there is one place to type.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.composer_focused());
    app.link_mut().take_sent();
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(sent_inputs(app.link().sent()).is_empty(), "typing still stays below");
    assert_eq!(app.composer_text(), "x");
}

#[test]
fn dock_click_and_alt_digit_run_the_agent_in_the_current_pane() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    app.set_dock_agents(fake_dock());
    clear_agents(&mut app);
    app.link_mut().take_sent();

    // Click the second dock button inside the pane composer.
    let composer = UiGeometry::calculate(&app, area).composer.unwrap();
    let item = composer.dock_items[1];
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), item.x + 1, item.y),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::DockLaunch(1));
    let inputs = sent_inputs(app.link().sent());
    assert_eq!(
        inputs,
        vec![
            InputFrame::Text {
                pane_id: "p1".to_owned(),
                text: "codex".to_owned(),
            },
            InputFrame::Keys {
                pane_id: "p1".to_owned(),
                keys: vec!["enter".to_owned()],
            },
        ],
        "the agent runs in the CURRENT pane, no split"
    );
    assert!(
        !request_methods(app.link().sent()).contains(&"pane.split"),
        "no new pane"
    );
    app.link_mut().take_sent();

    // alt+1 launches dock item 1 from the keyboard (keys.dock_agent).
    press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
    let inputs = sent_inputs(app.link().sent());
    assert!(inputs.contains(&InputFrame::Text {
        pane_id: "p1".to_owned(),
        text: "claude".to_owned(),
    }));

    // With an agent in the pane, the launchers are inert.
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].agent = Some("claude".to_owned());
    app.set_snapshot(snapshot);
    app.link_mut().take_sent();
    press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
    assert!(sent_inputs(app.link().sent()).is_empty());
}

#[test]
fn folder_pick_pushd_in_shells_but_only_remembers_for_agent_panes() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut config = test_config();
    config.ui.toast.delivery = ToastDelivery::Starcil;
    let mut app = app_with_frames(config);
    app.set_dock_agents(fake_dock());
    clear_agents(&mut app);

    // Click the open-folder button inside the pane composer.
    let composer = UiGeometry::calculate(&app, area).composer.unwrap();
    let folder = composer.folder_button;
    assert!(folder.height > 0);
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), folder.x + 2, folder.y),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::OpenFolderPicker);
    assert!(app.take_effects().contains(&AppEffect::OpenFolderPicker));

    app.link_mut().take_sent();
    app.folder_picked(Some(r"C:\proyectos\demo app".to_owned()));
    let inputs = sent_inputs(app.link().sent());
    assert!(
        inputs.contains(&InputFrame::Text {
            pane_id: "p1".to_owned(),
            text: r#"pushd "C:\proyectos\demo app""#.to_owned(),
        }),
        "a shell pane gets a visual cd: {inputs:?}"
    );
    assert!(app.dock_cwd_label().unwrap().contains("demo app"));

    // A pane hosting an agent must never get shell commands injected.
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].agent = Some("claude".to_owned());
    app.set_snapshot(snapshot);
    app.link_mut().take_sent();
    app.folder_picked(Some(r"C:\otra".to_owned()));
    assert!(sent_inputs(app.link().sent()).is_empty());
    assert!(app.toasts().last().unwrap().message.contains(r"C:\otra"));

    // Cancel changes nothing.
    let label = app.dock_cwd_label();
    app.folder_picked(None);
    assert_eq!(app.dock_cwd_label(), label);
}

fn plus_button(app: &App<FakeLink>, area: Rect) -> Rect {
    UiGeometry::calculate(app, area)
        .chrome
        .iter()
        .find(|region| region.target == ChromeTarget::NewTab)
        .expect("the tab bar shows a + button")
        .rect
}

#[test]
fn new_tab_prompt_is_prefilled_with_the_next_position_so_enter_alone_creates_it() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    config.ui.prompt_new_tab_name = true;
    let mut app = app_with_frames(config);
    app.link_mut().take_sent();

    // The fixture workspace has one tab: the next one is number 2.
    let plus = plus_button(&app, area);
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), plus.x + 1, plus.y),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::NewTab);
    assert_eq!(
        app.mode(),
        &Mode::Modal(crate::Modal::Prompt {
            kind: crate::PromptKind::NewTab,
            value: "2".to_owned(),
        })
    );
    let text = buffer_text(&render_buffer(&app, 100, 30));
    assert!(text.contains("2▏"), "the suggestion shows in the input: {text}");

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    let requests = sent_requests(app.link().sent());
    let (method, params) = requests.last().expect("tab.create request");
    assert_eq!(method, "tab.create");
    assert_eq!(params["label"], "2");
    assert_eq!(params["workspace_id"], "w1");
    assert_eq!(params["focus"], true, "the tab you just created is where you go");

    // Backspace still lets the user type their own name over the suggestion.
    app.link_mut().take_sent();
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), plus.x + 1, plus.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let requests = sent_requests(app.link().sent());
    assert_eq!(requests.last().unwrap().1["label"], "qa");

    // The tab context menu's "New tab" offers the same suggestion.
    let tab = UiGeometry::calculate(&app, area)
        .chrome
        .iter()
        .find(|region| matches!(region.target, ChromeTarget::Tab(_)))
        .unwrap()
        .rect;
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), tab.x + 1, tab.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.mode(),
        &Mode::Modal(crate::Modal::Prompt {
            kind: crate::PromptKind::NewTab,
            value: "2".to_owned(),
        })
    );
}

#[test]
fn hovering_the_plus_button_highlights_it_even_with_warp_stuck_right_button() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    let mut app = app_with_frames(config);
    let plus = plus_button(&app, area);
    let bar = UiGeometry::calculate(&app, area).tab_bar.unwrap();
    let label_row = bar.y + bar.height - 1;
    let active_bg = ratatui_color(app.theme().tokens.tab_active_bg);

    // At rest the `+` sits on the panel background.
    let buffer = render_buffer(&app, 100, 30);
    let cell = buffer.cell((plus.x + 1, label_row)).unwrap();
    assert_eq!(cell.symbol(), "+");
    assert_ne!(cell.bg, active_bg);
    assert_eq!(app.hovered_chrome(), None);

    // Pointer motion over it. Under Warp the right-button release never
    // arrives, so motion comes in as Drag(Right): it must count as hover.
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Right), plus.x + 1, plus.y),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::Ignored);
    assert_eq!(app.hovered_chrome(), Some(&ChromeTarget::NewTab));
    let buffer = render_buffer(&app, 100, 30);
    let cell = buffer.cell((plus.x + 1, label_row)).unwrap();
    assert_eq!(cell.symbol(), "+");
    assert_eq!(cell.bg, active_bg, "hovered + takes the active-tab look");

    // Plain motion hovers a tab.
    let tab = UiGeometry::calculate(&app, area)
        .chrome
        .iter()
        .find(|region| matches!(region.target, ChromeTarget::Tab(_)))
        .unwrap()
        .rect;
    app.handle_mouse(
        mouse(MouseEventKind::Moved, tab.x + 1, tab.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.hovered_chrome(), Some(&ChromeTarget::Tab("t1".to_owned())));

    // Leaving the chrome clears the highlight.
    app.handle_mouse(mouse(MouseEventKind::Moved, 60, 20), area, &mut clipboard)
        .unwrap();
    assert_eq!(app.hovered_chrome(), None);
    let buffer = render_buffer(&app, 100, 30);
    assert_ne!(buffer.cell((plus.x + 1, label_row)).unwrap().bg, active_bg);

    // A click on the `+` opens the prompt; modal routing drops the hover so a
    // closed modal never leaves a stale highlight behind.
    let mut prompting = test_config();
    prompting.ui.hide_tab_bar_when_single_tab = false;
    prompting.ui.prompt_new_tab_name = true;
    let mut app = app_with_frames(prompting);
    app.handle_mouse(
        mouse(MouseEventKind::Moved, plus.x + 1, plus.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.hovered_chrome(), Some(&ChromeTarget::NewTab));
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), plus.x + 1, plus.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Moved, 60, 20), area, &mut clipboard)
        .unwrap();
    assert_eq!(app.hovered_chrome(), None);
}

#[test]
fn header_is_one_row_with_tab_labels_and_workspaces_text_level() {
    let area = Rect::new(0, 0, 100, 30);
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    let app = app_with_frames(config);
    let geometry = UiGeometry::calculate(&app, area);
    let bar = geometry.tab_bar.expect("tab bar");
    // A cell row is indivisible: one row is the only height where the text
    // sits vertically centered in the band (Cesar: "centrado").
    assert_eq!(bar.height, 1);
    assert_eq!(geometry.sidebar_toggle.height, 1);
    let buffer = render_buffer(&app, 100, 30);
    let active_bg = ratatui_color(app.theme().tokens.tab_active_bg);
    let row_text = |y: u16| {
        (0..100)
            .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
            .collect::<String>()
    };

    // Tab labels, the `+`, and the sidebar header share row 0.
    let top = row_text(0);
    assert!(top.contains(" main "), "{top:?}");
    assert!(top.contains(" + "), "{top:?}");
    assert!(top.contains("◫  Workspaces"), "{top:?}");
    assert_eq!(buffer.cell((bar.x + 1, 0)).unwrap().bg, active_bg);
    assert!(!row_text(1).contains("Workspaces"));

    // Panes and workspace rows start right under it.
    assert_eq!(geometry.main.y, 1);
    let pane = geometry.panes.iter().find(|pane| pane.pane_id == "p1").unwrap();
    assert_eq!(pane.outer.y, 1);
    let first_workspace = geometry
        .chrome
        .iter()
        .find(|region| matches!(region.target, ChromeTarget::Workspace(_)))
        .unwrap()
        .rect;
    assert_eq!(first_workspace.y, 1);
}

#[test]
fn each_pane_keeps_its_own_composer_draft_focus_and_folder() {
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();

    // Draft in p1's composer (focused by default) and pick a folder for it.
    assert!(app.composer_focused());
    for character in ['l', 's'] {
        press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
    }
    assert_eq!(app.composer_text(), "ls");
    app.folder_picked(Some(r"C:\uno".to_owned()));
    assert!(app.dock_cwd_label().unwrap().contains("uno"));
    app.link_mut().take_sent();

    let focus = |app: &mut App<FakeLink>, pane_id: &str| {
        let mut snapshot = app.snapshot().unwrap().clone();
        snapshot.focused_pane_id = pane_id.to_owned();
        for pane in &mut snapshot.panes {
            pane.focused = pane.pane_id == pane_id;
        }
        app.set_snapshot(snapshot);
    };

    // Focus moves to p2 (another shell): its own blank composer, focused by
    // default, with p2's own folder — nothing leaks across panes.
    focus(&mut app, "p2");
    assert_eq!(app.composer_text(), "");
    assert!(app.composer_focused());
    assert!(!app.dock_cwd_label().unwrap().contains("uno"));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "x", "typing lands in p2's own draft");
    assert!(sent_inputs(app.link().sent()).is_empty(), "nothing reaches any PTY");
    // Esc clears p2's draft: p2's own state alone.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "");
    assert!(app.composer_focused());

    // Back on p1 everything is where it was left.
    focus(&mut app, "p1");
    assert_eq!(app.composer_text(), "ls");
    assert!(app.composer_focused());
    assert!(app.dock_cwd_label().unwrap().contains("uno"));
    focus(&mut app, "p2");
    assert_eq!(app.composer_text(), "", "Esc cleared p2's draft");

    // A closed pane takes its draft with it; the survivor keeps its own.
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes.retain(|pane| pane.pane_id != "p1");
    snapshot.focused_pane_id = "p2".to_owned();
    app.set_snapshot(snapshot);
    assert!(app.composer_focused());
    assert_eq!(app.composer_text(), "y");
    focus(&mut app, "p2");
    assert!(!app.dock_cwd_label().unwrap().contains("uno"), "p1's folder died with p1");
}

/// Completion source for the tests: a tiny fake tree, no real disk.
struct FakeCompletionFs;

impl crate::composer::CompletionSource for FakeCompletionFs {
    fn list_dir(&self, dir: &std::path::Path) -> Vec<(String, bool)> {
        let key = dir
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase();
        match key.as_str() {
            "c:/repo" => vec![
                ("tests".to_owned(), true),
                ("target".to_owned(), true),
                ("Cargo.toml".to_owned(), false),
            ],
            "c:/repo/tests" => vec![("e2e".to_owned(), true)],
            "c:/elsewhere" => vec![("other".to_owned(), true)],
            _ => vec![],
        }
    }

    fn path_commands(&self) -> Vec<String> {
        vec!["cargo".to_owned(), "git".to_owned()]
    }

    fn home_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}

fn type_text(app: &mut App<FakeLink>, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character), KeyModifiers::NONE);
    }
}

fn sep() -> char {
    if cfg!(windows) { '\\' } else { '/' }
}

#[test]
fn composer_edits_like_a_shell_line_with_a_visible_cursor() {
    let area = Rect::new(0, 0, 100, 30);
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();

    type_text(&mut app, "cd C:\\dev\\Starcil");
    assert_eq!(app.composer_cursor(), 17);
    // Cursor keys walk the line; words split at separators like PSReadLine.
    press(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(app.composer_cursor(), 10);
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(app.composer_cursor(), 0);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE);
    type_text(&mut app, " /d");
    assert_eq!(app.composer_text(), "cd /d C:\\dev\\Starcil", "typing inserts at the cursor");
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    press(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "cd /d C:\\dev\\", "ctrl+backspace kills a word");
    press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "cd /d C:\\");
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "d /d C:\\");
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "", "ctrl+k kills to the end");
    type_text(&mut app, "echo hola");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "la", "ctrl+u kills to the start");
    assert_eq!(app.composer_cursor(), 0);
    assert!(
        sent_inputs(app.link().sent()).is_empty(),
        "none of the editing chords reached the pane"
    );

    // The cursor block sits under the char at the cursor, mid-line.
    type_text(&mut app, "ho");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let composer = UiGeometry::calculate(&app, area).composer.clone().unwrap();
    let buffer = render_buffer(&app, 100, 30);
    let cursor_bg = ratatui_color(app.theme().tokens.cursor);
    let row = row_text(&buffer, composer.input.y, composer.input.x..composer.input.x + 8);
    assert!(row.starts_with("❯ hola"), "{row:?}");
    let cell = buffer.cell((composer.input.x + 3, composer.input.y)).unwrap();
    assert_eq!(cell.symbol(), "o");
    assert_eq!(cell.bg, cursor_bg, "the cursor highlights `o`");
    let after = buffer.cell((composer.input.x + 4, composer.input.y)).unwrap();
    assert_ne!(after.bg, cursor_bg);

    // Editing chords are never forwarded, but ctrl+l (clear) still is.
    app.link_mut().take_sent();
    press(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![InputFrame::Keys {
            pane_id: "p1".to_owned(),
            keys: vec!["ctrl+l".to_owned()],
        }]
    );
}

#[test]
fn composer_scrolls_a_long_draft_to_keep_the_cursor_in_view() {
    let area = Rect::new(0, 0, 80, 30);
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    let composer = UiGeometry::calculate(&app, area).composer.clone().unwrap();
    let width = usize::from(composer.input.width);
    let long: String = (0..width + 20).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    type_text(&mut app, &long);
    let buffer = render_buffer(&app, 80, 30);
    let row = row_text(&buffer, composer.input.y, composer.input.x..composer.input.x + composer.input.width);
    let tail: String = long.chars().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    assert!(row.contains(&tail), "the end of the draft is on screen: {row:?}");
    assert!(!row.contains("❯ abc"), "the head scrolled off");
    // Home brings the head back.
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    let buffer = render_buffer(&app, 80, 30);
    let row = row_text(&buffer, composer.input.y, composer.input.x..composer.input.x + 8);
    assert!(row.starts_with("❯ abc"), "{row:?}");
}

#[test]
fn composer_history_walks_by_prefix_and_ctrl_r_searches() {
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.seed_history(["git status", "cargo test", "git push"].map(String::from));
    app.link_mut().take_sent();

    // Up walks back, filtered by what was typed; Down returns to the draft.
    type_text(&mut app, "gi");
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "git push");
    assert_eq!(app.composer_cursor(), 8);
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "git status");
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "git status", "nothing older matches");
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "gi", "the draft comes back");
    assert!(sent_inputs(app.link().sent()).is_empty());

    // Submitting records the line; a bare Up recalls it.
    press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.history_entries().last().map(String::as_str), Some("git"));
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "git");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "");

    // ctrl+r: incremental search, ctrl+r again goes older, Enter runs it.
    app.link_mut().take_sent();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(app.composer_search().is_some());
    type_text(&mut app, "sta");
    assert_eq!(app.composer_text(), "git status");
    let search = app.composer_search().unwrap();
    assert_eq!(search.query, "sta");
    assert!(search.found);
    type_text(&mut app, "zz");
    assert!(!app.composer_search().unwrap().found, "no hit for `stazz`");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert!(app.composer_search().unwrap().found);
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    type_text(&mut app, "t");
    assert_eq!(app.composer_text(), "git", "newest hit for `t`");
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "git push", "older hit");
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "cargo test");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.composer_search().is_none());
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![
            InputFrame::Text {
                pane_id: "p1".to_owned(),
                text: "cargo test".to_owned(),
            },
            InputFrame::Keys {
                pane_id: "p1".to_owned(),
                keys: vec!["enter".to_owned()],
            },
        ]
    );

    // The search row shows the query; ctrl+g brings the old draft back.
    type_text(&mut app, "draft");
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_text(&mut app, "pu");
    assert_eq!(app.composer_text(), "git push");
    let buffer = render_buffer(&app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(text.contains("(reverse-i-search)'pu': git push"), "{text}");
    press(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(app.composer_search().is_none());
    assert_eq!(app.composer_text(), "draft");
}

#[test]
fn composer_tab_completes_against_the_live_cwd_and_cycles() {
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.set_completion_source(Box::new(FakeCompletionFs));
    app.link_mut().take_sent();
    let s = sep();

    // The fixture's pane lives in C:/repo: `cd t` offers its directories,
    // Tab cycles them, shift+Tab walks back, typing ends the cycle.
    type_text(&mut app, "cd t");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), format!("cd target{s}"));
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), format!("cd tests{s}"));
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), format!("cd target{s}"), "wraps around");
    press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.composer_text(), format!("cd tests{s}"));
    // Typing ends the cycle; the next Tab completes inside the folder.
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), format!("cd tests{s}e2e{s}"), "descends into the folder");
    assert_eq!(app.composer_cursor(), app.composer_text().chars().count());
    assert!(sent_inputs(app.link().sent()).is_empty(), "Tab never reaches the PTY");

    // Command position: PATH names (and the shell's own) complete too.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    type_text(&mut app, "car");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "cargo");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    type_text(&mut app, "nothinghere");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "nothinghere", "no candidates: the line stands");

    // The shell moved (`cd ..` the user ran): the event updates the cwd and
    // completions follow it, no snapshot round-trip.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    app.folder_picked(Some("C:/picked".to_owned()));
    assert!(app.dock_cwd_label().unwrap().contains("picked"), "a pick previews the cwd");
    app.link_mut().push(ServerMsg::Incoming(Incoming::Event(EventFrame {
        event: "pane.cwd_changed".to_owned(),
        data: serde_json::json!({"pane_id": "p1", "cwd": "C:/elsewhere"}),
        revision: None,
    })));
    let revision = app.ui_revision();
    app.poll();
    assert_eq!(app.snapshot().unwrap().panes[0].cwd, "C:/elsewhere");
    assert_eq!(app.dock_cwd_label().as_deref(), Some("C:/elsewhere"), "the shell's word wins");
    assert!(app.ui_revision() > revision, "the label redraws");
    type_text(&mut app, "ls o");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), format!("ls other{s}"));
}

#[test]
fn composer_takes_pastes_and_runs_a_multi_line_paste_line_by_line() {
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();
    let mut clipboard = FakeClipboard::default();

    type_text(&mut app, "echo ");
    clipboard.text = "uno\r\ndos\n".to_owned();
    press_with_clipboard(&mut app, &mut clipboard, KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(sent_inputs(app.link().sent()).is_empty(), "ctrl+v lands in the draft, not the pane");
    assert_eq!(app.composer_text(), "echo uno\ndos");
    let buffer = render_buffer(&app, 100, 30);
    assert!(buffer_text(&buffer).contains("echo uno⏎dos"), "newlines show as ⏎");

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let enter = InputFrame::Keys {
        pane_id: "p1".to_owned(),
        keys: vec!["enter".to_owned()],
    };
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![
            InputFrame::Text {
                pane_id: "p1".to_owned(),
                text: "echo uno".to_owned(),
            },
            enter.clone(),
            InputFrame::Text {
                pane_id: "p1".to_owned(),
                text: "dos".to_owned(),
            },
            enter,
        ],
        "one write + Enter per line"
    );

    // The middle-click / menu paste path lands in the draft as well.
    app.link_mut().take_sent();
    clipboard.text = "pwd".to_owned();
    let area = Rect::new(0, 0, 100, 30);
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Middle), 40, 5),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.composer_text(), "pwd");
    assert!(sent_inputs(app.link().sent()).is_empty());
}

#[test]
fn a_click_on_the_input_row_places_the_cursor() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    type_text(&mut app, "git commit");
    let composer = UiGeometry::calculate(&app, area).composer.clone().unwrap();
    // `❯ ` takes two cells: cell 2 is `g`, cell 6 is `c`.
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), composer.input.x + 6, composer.input.y),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.composer_cursor(), 4);
    type_text(&mut app, "-a ");
    assert_eq!(app.composer_text(), "git -a commit");
    // Past the end of the text: the cursor lands at the end.
    app.handle_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            composer.input.x + composer.input.width - 1,
            composer.input.y,
        ),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.composer_cursor(), 13);
    assert!(app.composer_focused());
}

#[test]
fn composer_owns_the_keyboard_and_a_content_click_keeps_it_there() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();
    assert!(app.composer_focused(), "a fresh shell pane types into its composer");

    // ctrl+c with a draft discards it; with nothing drafted it interrupts the
    // program in the pane. Other chords always reach the pane.
    press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.composer_text(), "");
    assert!(sent_inputs(app.link().sent()).is_empty());
    press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![InputFrame::Keys {
            pane_id: "p1".to_owned(),
            keys: vec!["ctrl+c".to_owned()],
        }]
    );
    app.link_mut().take_sent();
    press(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![InputFrame::Keys {
            pane_id: "p1".to_owned(),
            keys: vec!["ctrl+l".to_owned()],
        }]
    );
    app.link_mut().take_sent();
    assert!(app.composer_focused(), "chords never steal the focus");

    // A click on the pane's content (the prompt at the top, say, to type
    // there) focuses the pane and the typing lands BELOW: one place to
    // type, never two (Cesar's rule).
    let geometry = UiGeometry::calculate(&app, area);
    let pane = geometry.panes.iter().find(|pane| pane.pane_id == "p1").unwrap();
    let (x, y) = (pane.content.x + 1, pane.content.y + 1);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area, &mut clipboard)
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, y), area, &mut clipboard)
        .unwrap();
    assert!(app.composer_focused(), "the content click keeps the keyboard below");
    press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(sent_inputs(app.link().sent()).is_empty(), "nothing reaches the prompt above");
    assert_eq!(app.composer_text(), "z");

    // ctrl+space (focus_input) has nothing to move any more: a no-op that
    // never leaks to the pane either.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(app.composer_focused());
    assert!(sent_inputs(app.link().sent()).is_empty());
    assert_eq!(app.composer_text(), "z");

    // Esc clears the draft; a second Esc changes nothing.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.composer_text(), "");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.composer_focused());
    assert!(sent_inputs(app.link().sent()).is_empty());

    // Run a CLI there, leave it: the composer is back (the pane is a fresh
    // shell).
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].agent = Some("claude".to_owned());
    app.set_snapshot(snapshot);
    assert!(!app.composer_focused(), "no composer while the CLI runs");
    assert!(UiGeometry::calculate(&app, area).composer.is_none());
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].agent = None;
    app.set_snapshot(snapshot);
    assert!(UiGeometry::calculate(&app, area).composer.is_some());
    assert!(app.composer_focused(), "back from the CLI: keyboard on the composer");
}

#[test]
fn working_agents_spin_in_the_sidebar() {
    let mut config = Config::default();
    config.onboarding = Some(false);
    let mut app = app_with_frames(config);
    // The fixture's codex is working; the frame starts at 0 (the golden pins ⠋).
    assert_eq!(app.spinner_frame(), 0);
    app.set_spinner_frame(3);
    assert_eq!(app.spinner_frame(), 3);
    let text = buffer_text(&render_buffer(&app, 120, 30));
    assert!(text.contains("⠸ left·codex"), "{text}");

    // Nothing working: the frame pins to 0 so redraws stay quiet.
    let mut snapshot = app.snapshot().unwrap().clone();
    for agent in &mut snapshot.agents {
        agent.agent_status = AgentStatus::Idle;
    }
    for pane in &mut snapshot.panes {
        pane.agent_status = AgentStatus::Idle;
    }
    app.set_snapshot(snapshot);
    assert_eq!(app.spinner_frame(), 0);
    let text = buffer_text(&render_buffer(&app, 120, 30));
    assert!(text.contains("✓ left·codex"), "{text}");
}

#[test]
fn composer_yields_the_keyboard_while_a_program_runs_and_takes_it_back_at_the_prompt() {
    let area = Rect::new(0, 0, 100, 30);
    let mut app = app_with_frames(test_config());
    clear_agents(&mut app);
    app.link_mut().take_sent();
    assert!(app.composer_focused());

    // The shell starts a program (vim, a script asking y/n): keys go to it.
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].shell_idle = Some(false);
    app.set_snapshot(snapshot);
    assert!(!app.composer_focused());
    assert!(
        UiGeometry::calculate(&app, area).composer.is_some(),
        "still drawn (no resize churn), just not focused"
    );
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(
        sent_inputs(app.link().sent()),
        vec![InputFrame::Text {
            pane_id: "p1".to_owned(),
            text: "q".to_owned(),
        }]
    );
    app.link_mut().take_sent();

    // Even a user who blurred it meanwhile gets it back at the next prompt.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    let mut snapshot = app.snapshot().unwrap().clone();
    snapshot.panes[0].shell_idle = Some(true);
    app.set_snapshot(snapshot);
    assert!(app.composer_focused(), "back at the prompt the composer owns the keys");

    // The same transition as the live event, applied without a snapshot.
    let mut busy = session_snapshot();
    for pane in &mut busy.panes {
        pane.agent = None;
        pane.agent_name = None;
    }
    busy.panes[0].shell_idle = Some(false);
    let mut app = App::new(
        test_config(),
        HostAppearance::Dark,
        FakeLink::new([
            ServerMsg::SessionSnapshot(busy),
            ServerMsg::Incoming(Incoming::Event(EventFrame {
                event: "pane.shell_idle_changed".to_owned(),
                data: serde_json::json!({"pane_id": "p1", "idle": true}),
                revision: None,
            })),
        ]),
    )
    .unwrap();
    app.poll();
    assert_eq!(app.snapshot().unwrap().panes[0].shell_idle, Some(true));
    assert!(app.composer_focused());
}

fn row_text(buffer: &Buffer, y: u16, range: std::ops::Range<u16>) -> String {
    range
        .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
        .collect()
}

#[test]
fn dock_is_a_bordered_panel_with_hover_like_the_context_menu() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    app.set_dock_agents(fake_dock());
    clear_agents(&mut app);
    let composer = UiGeometry::calculate(&app, area)
        .composer
        .clone()
        .expect("composer");
    let panel = composer.dock_panel;
    assert_eq!(panel.height, 5, "border + 2 agents + folder + border");
    assert_eq!(composer.dock_items[0].y, panel.y + 1);
    assert_eq!(composer.folder_button.y, panel.y + 3);
    assert_eq!(
        composer.border.y,
        panel.y + panel.height,
        "the brand line follows the panel"
    );

    let buffer = render_buffer(&app, 100, 30);
    let accent = ratatui_color(app.theme().tokens.accent);
    assert_eq!(buffer.cell((panel.x, panel.y)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((panel.x, panel.y)).unwrap().fg, accent);
    assert_eq!(
        buffer.cell((panel.x, panel.y + panel.height - 1)).unwrap().symbol(),
        "╰"
    );
    let title = row_text(&buffer, panel.y, panel.x..panel.x + panel.width);
    assert!(title.contains("Launch"), "{title:?}");
    // Cesar's icons: ✴ for Claude Code, ֎ for Codex, nothing for anyone else.
    let claude_row = row_text(&buffer, composer.dock_items[0].y, panel.x..panel.x + panel.width);
    let codex_row = row_text(&buffer, composer.dock_items[1].y, panel.x..panel.x + panel.width);
    assert!(claude_row.contains("1 ✴ claude"), "{claude_row:?}");
    assert!(codex_row.contains("2 ֎ codex"), "{codex_row:?}");
    assert!(crate::dock::dock_glyph("gemini").is_none());
    assert_eq!(crate::mouse::dock_item_label(2, "gemini"), " 3 gemini ");

    // Hover over codex lights its row (active-tab look), nothing else.
    let item = composer.dock_items[1];
    app.handle_mouse(mouse(MouseEventKind::Moved, item.x + 2, item.y), area, &mut clipboard)
        .unwrap();
    assert_eq!(app.hovered_dock(), Some(crate::mouse::DockHover::Item(1)));
    let buffer = render_buffer(&app, 100, 30);
    let active_bg = ratatui_color(app.theme().tokens.tab_active_bg);
    assert_eq!(buffer.cell((item.x + 2, item.y)).unwrap().bg, active_bg);
    let other = composer.dock_items[0];
    assert_ne!(buffer.cell((other.x + 2, other.y)).unwrap().bg, active_bg);
    app.handle_mouse(
        mouse(
            MouseEventKind::Moved,
            composer.folder_button.x + 2,
            composer.folder_button.y,
        ),
        area,
        &mut clipboard,
    )
    .unwrap();
    assert_eq!(app.hovered_dock(), Some(crate::mouse::DockHover::Folder));
    app.handle_mouse(mouse(MouseEventKind::Moved, 60, 5), area, &mut clipboard)
        .unwrap();
    assert_eq!(app.hovered_dock(), None);
}

#[test]
fn sidebar_sections_divider_drags_and_persists_the_split() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    app.link_mut().take_sent();
    let divider = UiGeometry::calculate(&app, area).sidebar_split;
    assert_eq!(
        (divider.y, divider.height),
        (14, 1),
        "controls on 13, divider on 14, agents header on 15"
    );
    let buffer = render_buffer(&app, 100, 30);
    assert_eq!(buffer.cell((3, 14)).unwrap().symbol(), "─");
    assert!(row_text(&buffer, 13, 0..19).contains("new"));
    assert!(row_text(&buffer, 15, 0..19).contains("agents"));

    // Hover lights the line.
    app.handle_mouse(mouse(MouseEventKind::Moved, 5, 14), area, &mut clipboard)
        .unwrap();
    assert!(app.hovered_split());
    let buffer = render_buffer(&app, 100, 30);
    assert_eq!(
        buffer.cell((3, 14)).unwrap().fg,
        ratatui_color(app.theme().tokens.accent)
    );

    // Drag it down four rows: workspaces gain room, agents start lower.
    app.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 5, 14),
        area,
        &mut clipboard,
    )
    .unwrap();
    let action = app
        .handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 5, 18),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(action, MouseAction::ResizeSidebarSplit { percent: 63 });
    assert_eq!(UiGeometry::calculate(&app, area).sidebar_split.y, 18);
    let buffer = render_buffer(&app, 100, 30);
    assert!(row_text(&buffer, 17, 0..19).contains("menu"));
    assert!(row_text(&buffer, 19, 0..19).contains("agents"));

    // Releasing persists the split (config write + server reload).
    let up = app
        .handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), 5, 18),
            area,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(up, MouseAction::SaveSidebarSplit);
    assert!(request_methods(app.link().sent()).contains(&"server.reload_config"));
    // Chrome hit-tests follow the new rows: the menu button lives on row 17.
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 15, 17),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::OpenMenu
    );
}

#[test]
fn agents_heading_is_centered_and_shimmers_while_an_agent_works() {
    let mut config = Config::default();
    config.onboarding = Some(false);
    let mut app = app_with_frames(config);
    let width = 25u16; // sidebar 26 minus its border column
    let buffer = render_buffer(&app, 120, 30);
    let header = row_text(&buffer, 15, 0..width);
    assert!(!header.contains("grouped"), "{header:?}");
    let start = header.find("agents").expect("agents header") as i32;
    let right = i32::from(width) - (start + 6);
    assert!((start - right).abs() <= 1, "centered: {header:?}");
    let bright = ratatui_color(app.theme().tokens.fg);
    let start = start as u16;
    let lit = |buffer: &Buffer| {
        (start..start + 6)
            .filter(|x| buffer.cell((*x, 15)).unwrap().fg == bright)
            .count()
    };
    let first_lit = |buffer: &Buffer| {
        (start..start + 6).find(|x| buffer.cell((*x, 15)).unwrap().fg == bright)
    };

    // The fixture's codex is working: a band of letters is lit and it sweeps.
    app.set_spinner_frame(3);
    let frame3 = render_buffer(&app, 120, 30);
    assert!(lit(&frame3) >= 2, "{}", row_text(&frame3, 15, 0..width));
    app.set_spinner_frame(5);
    let frame5 = render_buffer(&app, 120, 30);
    assert_ne!(first_lit(&frame3), first_lit(&frame5), "the band sweeps");

    // Everything idle: plain dim text.
    let mut snapshot = app.snapshot().unwrap().clone();
    for agent in &mut snapshot.agents {
        agent.agent_status = AgentStatus::Idle;
    }
    app.set_snapshot(snapshot);
    assert_eq!(app.agents_shimmer(), None);
    let idle = render_buffer(&app, 120, 30);
    assert_eq!(lit(&idle), 0);
}

#[test]
fn update_ready_asks_once_with_yes_or_no_and_the_menu_asks_again() {
    let area = Rect::new(0, 0, 100, 30);
    let mut clipboard = FakeClipboard::default();
    let mut app = app_with_frames(test_config());
    app.set_update_ready("0.2.0");
    app.poll();
    assert_eq!(
        app.mode(),
        &Mode::Modal(crate::Modal::UpdatePrompt {
            version: "0.2.0".to_owned(),
        })
    );
    let text = buffer_text(&render_buffer(&app, 100, 30));
    assert!(text.contains("0.2.0") && text.contains("Enter update"), "{text}");

    // "no": closes, applies nothing, and stays quiet on later polls.
    press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    app.poll();
    assert_eq!(app.mode(), &Mode::Terminal);
    assert!(app.take_effects().is_empty());

    // The menu keeps its "update ready" row; picking it asks again and
    // "yes" applies.
    assert_eq!(
        app.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 15, 13),
            area,
            &mut clipboard,
        )
        .unwrap(),
        MouseAction::OpenMenu
    );
    let position = app
        .menu_actions()
        .iter()
        .position(|action| *action == crate::app::MenuAction::ApplyUpdate)
        .expect("update row in the menu");
    for _ in 0..position {
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.mode(),
        &Mode::Modal(crate::Modal::UpdatePrompt {
            version: "0.2.0".to_owned(),
        })
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.mode(), &Mode::Terminal);
    assert!(app.take_effects().contains(&AppEffect::ApplyUpdate));

    // Applied: the row goes away.
    app.clear_update_ready();
    assert!(!app.menu_actions().contains(&crate::app::MenuAction::ApplyUpdate));
}

#[test]
fn dragging_a_tab_along_the_bar_reorders_it_locally_and_sends_tab_move() {
    let mut config = test_config();
    config.ui.hide_tab_bar_when_single_tab = false;
    let mut snapshot = session_snapshot();
    let tab = |id: &str, label: &str, workspace: &str| TabInfo {
        tab_id: id.to_owned(),
        workspace_id: workspace.to_owned(),
        label: label.to_owned(),
        focused: false,
        panes: Vec::new(),
        zoomed: None,
    };
    snapshot.tabs.push(tab("t2", "build", "w1"));
    snapshot.tabs.push(tab("t3", "logs", "w1"));
    // A tab of another workspace shares `snapshot.tabs`: it must stay put.
    snapshot.tabs.push(tab("t9", "other", "w2"));
    snapshot.workspaces[0].tabs = vec!["t1".to_owned(), "t2".to_owned(), "t3".to_owned()];
    let link = FakeLink::new([ServerMsg::SessionSnapshot(snapshot)]);
    let mut app = App::new(config, HostAppearance::Dark, link).expect("valid app config");
    app.poll();
    let mut clipboard = FakeClipboard::default();
    let area = Rect::new(0, 0, 100, 30);
    let order = |app: &App<FakeLink>| -> Vec<String> {
        app.snapshot()
            .unwrap()
            .tabs
            .iter()
            .map(|tab| tab.tab_id.clone())
            .collect()
    };
    // Sidebar is 20 wide; each label pads to a 10-cell block:
    // main [20,30) · build [30,40) · logs [40,50).
    assert_eq!(row_text(&render_buffer(&app, 100, 30), 0, 20..50), " main      build     logs     ");

    // The press focuses, as a click always did.
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 22, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::FocusTab("t1".to_owned())
    );
    assert_eq!(app.dragging_tab(), None);
    // Short of the neighbour's midpoint (35) nothing moves yet, but the grab
    // is live and rendered as such.
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 34, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(app.dragging_tab(), Some("t1"));
    assert_eq!(order(&app), ["t1", "t2", "t3", "t9"]);
    // Crossing it swaps: local echo AND `tab.move` on the wire.
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 35, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::MoveTab {
            tab_id: "t1".to_owned(),
            insert_index: 1
        }
    );
    assert_eq!(order(&app), ["t2", "t1", "t3", "t9"]);
    assert_eq!(app.snapshot().unwrap().workspaces[0].tabs, ["t2", "t1", "t3"]);
    assert_eq!(row_text(&render_buffer(&app, 100, 30), 0, 20..50), " build     main      logs     ");
    // Holding still on the swapped layout does not bounce back (the
    // neighbour's centre moved to 25, behind the pointer).
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 35, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::Ignored
    );
    // Past the last tab's midpoint (45) it becomes the last one.
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 46, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::MoveTab {
            tab_id: "t1".to_owned(),
            insert_index: 2
        }
    );
    assert_eq!(order(&app), ["t2", "t3", "t1", "t9"]);
    // And all the way back to the front in one motion.
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 24, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::MoveTab {
            tab_id: "t1".to_owned(),
            insert_index: 0
        }
    );
    assert_eq!(order(&app), ["t1", "t2", "t3", "t9"]);
    assert_eq!(
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 24, 0), area, &mut clipboard)
            .unwrap(),
        MouseAction::Ignored
    );
    assert_eq!(app.dragging_tab(), None);
    let sent = sent_requests(app.link().sent());
    assert_eq!(
        sent.iter().map(|(method, _)| method.as_str()).collect::<Vec<_>>(),
        ["tab.focus", "tab.move", "tab.move", "tab.move"]
    );
    assert_eq!(sent[1].1, serde_json::json!({"tab_id": "t1", "insert_index": 1}));
    assert_eq!(sent[3].1, serde_json::json!({"tab_id": "t1", "insert_index": 0}));

    // A plain click (press + release, no motion) is still just a focus.
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 0), area, &mut clipboard)
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 0), area, &mut clipboard)
        .unwrap();
    let sent = sent_requests(app.link().sent());
    assert_eq!(sent.len(), 5);
    assert_eq!(sent[4].0, "tab.focus");
    assert_eq!(order(&app), ["t1", "t2", "t3", "t9"]);
}
