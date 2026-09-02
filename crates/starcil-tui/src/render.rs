use std::str::FromStr;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color as RatatuiColor, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use starcil_config::{
    AgentPanelSort, Color, HostCursor, NamedColor, RowToken, TabBarPosition, ThemeTokens,
    ToastPosition,
};
use starcil_protocol::attach::{StyleDef, attr_bits};
use starcil_protocol::types::{AgentInfo, PaneInfo, PaneLayoutSnapshot, WorkspaceInfo};

use crate::app::{
    App, ContextMenuAction, ContextTarget, MenuAction, Modal, Mode, PromptKind, SidebarState,
};
use crate::app::{COMPOSER_PROMPT_WIDTH, composer_char_width, search_prefix};
use crate::link::ServerLink;
use crate::mouse::{
    ChromeTarget, DockHover, SETTINGS_ROWS_INNER_LINE, SETTINGS_SECTION_INNER_LINE,
    SIDEBAR_SPLIT_TAIL, modal_rect,
};
use crate::settings::{SECTION_NAMES, SettingsRow};

pub fn render_app<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>) {
    let area = frame.area();
    let tokens = &app.theme().tokens;
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(ratatui_color(tokens.fg))
                .bg(ratatui_color(tokens.bg)),
        ),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mobile = area.width <= app.config().ui.mobile_width_threshold;
    let body = area;

    let sidebar_width = if mobile {
        0
    } else {
        match app.sidebar_state() {
            SidebarState::Expanded => app
                .config()
                .ui
                .sidebar_width
                .clamp(
                    app.config().ui.sidebar_min_width,
                    app.config().ui.sidebar_max_width,
                )
                .min(body.width.saturating_sub(12)),
            SidebarState::Compact => 3.min(body.width),
            SidebarState::Hidden => 0,
        }
    };
    let sidebar = Rect {
        x: body.x,
        y: body.y,
        width: sidebar_width,
        height: body.height,
    };
    let mut main = Rect {
        x: body.x + sidebar_width,
        y: body.y,
        width: body.width.saturating_sub(sidebar_width),
        height: body.height,
    };

    if sidebar.width > 0 {
        render_sidebar(app, frame, sidebar);
    }

    if mobile && main.height > 0 {
        let header = Rect {
            x: main.x,
            y: main.y,
            width: main.width,
            height: 1,
        };
        render_mobile_header(app, frame, header);
        main.y += 1;
        main.height = main.height.saturating_sub(1);
    }

    // MUST mirror UiGeometry::calculate (taller tab bar included).
    let show_tabs = should_show_tab_bar(app);
    let tab_rows = crate::mouse::TAB_BAR_ROWS.min(main.height.saturating_sub(1));
    let tab_bar = if show_tabs && tab_rows > 0 {
        let rect = match app.config().ui.tab_bar_position {
            TabBarPosition::Top => {
                let rect = Rect { height: tab_rows, ..main };
                main.y += tab_rows;
                main.height = main.height.saturating_sub(tab_rows);
                rect
            }
            TabBarPosition::Bottom => {
                let rect = Rect {
                    x: main.x,
                    y: main.y + main.height.saturating_sub(tab_rows),
                    width: main.width,
                    height: tab_rows,
                };
                main.height = main.height.saturating_sub(tab_rows);
                rect
            }
        };
        Some(rect)
    } else {
        None
    };
    if let Some(tab_bar) = tab_bar {
        render_tab_bar(app, frame, tab_bar);
    }
    render_panes(app, frame, main, mobile);
    render_composer(app, frame, area);

    if let Mode::Modal(modal) = app.mode() {
        render_modal(app, frame, area, modal);
    }
    if let Some(toast) = app.toasts().last() {
        render_toast(app, frame, area, toast);
    }
    if let Some(line) = app.mouse_debug_line() {
        render_mouse_debug(frame, area, line, tokens);
    }
}

/// The in-pane composer (Claude Code style), drawn INSIDE the focused shell
/// pane: the stacked launcher buttons, the brand line with the clickable cwd,
/// a prominent `❯` input, and a breathing row. The pane's PTY has already
/// ceded these rows (`InputFrame::ReserveRows`), so nothing is covered.
/// Hidden entirely while the pane runs an agent CLI.
fn render_composer<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>, area: Rect) {
    let geometry = crate::mouse::UiGeometry::calculate(app, area);
    let Some(composer) = &geometry.composer else {
        return;
    };
    let tokens = &app.theme().tokens;
    let brand = Style::default().fg(ratatui_color(tokens.brand));
    let dim = dim_style(tokens);

    // The launcher panel: a titled, bordered box like the right-click menu,
    // its rows lit under the pointer so it reads as clickable.
    let panel_bg = ratatui_color(tokens.panel_bg);
    if composer.dock_panel.width > 0 {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(accent_style(tokens))
                .title(Span::styled(
                    " Launch ",
                    accent_style(tokens).add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().fg(ratatui_color(tokens.fg)).bg(panel_bg)),
            composer.dock_panel,
        );
    }
    let hovered = app.hovered_dock();
    let row_style = |hover: bool| {
        if hover {
            Style::default()
                .fg(ratatui_color(tokens.tab_active_fg))
                .bg(ratatui_color(tokens.tab_active_bg))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ratatui_color(tokens.fg)).bg(panel_bg)
        }
    };
    let agents = app.dock_agents();
    for (position, rect) in composer.dock_items.iter().enumerate() {
        let Some(agent) = agents.get(position) else {
            continue;
        };
        let hover = hovered == Some(DockHover::Item(position));
        let base = row_style(hover);
        let mut spans = vec![Span::styled(
            format!(" {} ", position + 1),
            if hover { base } else { dim.bg(panel_bg) },
        )];
        let mut used = 3;
        if let Some((glyph, color)) = crate::dock::dock_glyph(&agent.name) {
            spans.push(Span::styled(
                format!("{glyph} "),
                if hover { base } else { base.fg(ratatui_color(color)) },
            ));
            used += 2;
        }
        spans.push(Span::styled(agent.name.clone(), base));
        used += agent.name.chars().count();
        let padding = usize::from(rect.width).saturating_sub(used);
        spans.push(Span::styled(" ".repeat(padding), base));
        frame.render_widget(Paragraph::new(Line::from(spans)), *rect);
    }
    if composer.folder_button.height > 0 {
        let base = row_style(hovered == Some(DockHover::Folder));
        let label = " 📁 open folder";
        // The emoji takes two cells: pad one less than the char count says.
        let padding = usize::from(composer.folder_button.width)
            .saturating_sub(label.chars().count() + 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{label}{}", " ".repeat(padding)),
                base,
            )),
            composer.folder_button,
        );
    }

    // Double brand line around the input, like Claude Code's own composer.
    let line = "─".repeat(usize::from(composer.border.width));
    frame.render_widget(Paragraph::new(line.clone()).style(brand), composer.border);
    frame.render_widget(Paragraph::new(line).style(brand), composer.bottom_border);
    // Status row under the bottom line: the clickable cwd on the LEFT.
    let cwd = crate::mouse::cwd_label_text(app, composer.spacer.width);
    if !cwd.is_empty() && composer.cwd_label.width > 0 {
        frame.render_widget(Paragraph::new(cwd).style(dim), composer.cwd_label);
    }

    // Input row: the draft with its cursor, scrolled so the cursor stays in
    // view; a ctrl+r search shows its query before the hit.
    let prompt_style = if app.composer_focused() {
        Style::default()
            .fg(ratatui_color(tokens.fg))
            .add_modifier(Modifier::BOLD)
    } else {
        dim.add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![Span::styled("❯ ", prompt_style)];
    if app.composer_focused() {
        let text_style = Style::default().fg(ratatui_color(tokens.fg));
        let cursor_style = Style::default()
            .fg(ratatui_color(tokens.bg))
            .bg(ratatui_color(tokens.cursor));
        let mut available = usize::from(composer.input.width).saturating_sub(COMPOSER_PROMPT_WIDTH);
        if let Some(search) = app.composer_search() {
            let prefix = search_prefix(search);
            available = available.saturating_sub(prefix.chars().count());
            spans.push(Span::styled(prefix, dim));
        }
        let skip = app.composer_scroll(available);
        let cursor = app.composer_cursor();
        let mut used = 0usize;
        for (index, character) in app.composer_text().chars().enumerate().skip(skip) {
            let width = composer_char_width(character);
            if used + width > available.saturating_sub(usize::from(index == cursor)) {
                break;
            }
            let glyph = if character == '\n' { '⏎' } else { character };
            let style = if index == cursor { cursor_style } else { text_style };
            spans.push(Span::styled(glyph.to_string(), style));
            used += width;
        }
        if cursor >= app.composer_text().chars().count() && used < available {
            spans.push(Span::styled(" ", cursor_style));
        }
    } else {
        spans.push(Span::styled(app.composer_text(), dim));
        spans.push(Span::styled(" ", Style::default().bg(ratatui_color(tokens.surface1))));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), composer.input);
}

fn render_sidebar<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>, area: Rect) {
    let tokens = &app.theme().tokens;
    let border = Style::default().fg(ratatui_color(tokens.sidebar_border));
    let background = Style::default()
        .fg(ratatui_color(tokens.sidebar_fg))
        .bg(ratatui_color(tokens.sidebar_bg));
    if app.sidebar_state() == SidebarState::Compact {
        // The header is the expand toggle; clicking it re-opens the sidebar.
        let mut lines = vec![Line::styled(
            "◫  ",
            accent_style(tokens).add_modifier(Modifier::BOLD),
        )];
        if let Some(snapshot) = app.snapshot() {
            for workspace in &snapshot.workspaces {
                let state = workspace_state(snapshot, &workspace.workspace_id);
                lines.push(Line::styled(
                    format!(" {} ", state_icon(&state)),
                    state_style(tokens, &state),
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .style(background)
                .block(Block::default().borders(Borders::RIGHT).border_style(border)),
            area,
        );
        return;
    }

    frame.render_widget(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(border)
            .style(background),
        area,
    );
    let content = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    if content.width == 0 || content.height == 0 {
        return;
    }
    // MUST mirror `chrome_regions`: workspaces, then the `new · menu` row,
    // then the draggable divider, then the agents section from `split`.
    let split = crate::mouse::sidebar_split(app, content.height);
    let spaces_area = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: split.saturating_sub(SIDEBAR_SPLIT_TAIL),
    };
    let controls_area = Rect {
        x: content.x,
        y: content.y + split.saturating_sub(SIDEBAR_SPLIT_TAIL),
        width: content.width,
        height: u16::from(split >= SIDEBAR_SPLIT_TAIL),
    };
    let divider_area = Rect {
        x: content.x,
        y: content.y + split.saturating_sub(1),
        width: content.width,
        height: u16::from(split >= 1),
    };
    let agents_area = Rect {
        x: content.x,
        y: content.y + split,
        width: content.width,
        height: content.height.saturating_sub(split),
    };

    let mut spaces = workspaces_heading(content.width, tokens);
    let mut agents = vec![agents_heading(content.width, tokens, app.agents_shimmer())];
    if let Some(snapshot) = app.snapshot() {
        for (index, workspace) in snapshot.workspaces.iter().enumerate() {
            let state = workspace_state(snapshot, &workspace.workspace_id);
            let selected = workspace.workspace_id == snapshot.focused_workspace_id;
            let row_style = if selected {
                Style::default()
                    .fg(ratatui_color(tokens.sidebar_selected_fg))
                    .bg(ratatui_color(tokens.sidebar_selected_bg))
            } else {
                background
            };
            append_workspace_rows(
                &mut spaces,
                workspace,
                index,
                &state,
                &app.config().ui.sidebar.spaces.rows,
                row_style,
                tokens,
                selected,
                content.width,
            );
            for _ in 0..app.config().ui.sidebar.spaces.row_gap {
                spaces.push(Line::default());
            }
        }

        let mut sorted_agents = snapshot.agents.iter().collect::<Vec<_>>();
        match app.config().ui.agent_panel_sort {
            AgentPanelSort::Priority => sorted_agents.sort_by(|left, right| {
                state_priority(&status_name(&left.agent_status))
                    .cmp(&state_priority(&status_name(&right.agent_status)))
                    .then_with(|| left.agent.cmp(&right.agent))
            }),
            AgentPanelSort::Spaces => sorted_agents.sort_by(|left, right| {
                left.workspace_id
                    .cmp(&right.workspace_id)
                    .then_with(|| left.agent.cmp(&right.agent))
            }),
        }
        for agent in sorted_agents {
            append_agent_rows(
                &mut agents,
                app,
                agent,
                background,
                tokens,
                content.width,
            );
            for _ in 0..app.config().ui.sidebar.agents.row_gap {
                agents.push(Line::default());
            }
        }
    } else {
        spaces.push(Line::styled(" waiting for session", dim_style(tokens)));
    }
    frame.render_widget(
        Paragraph::new(spaces).style(background),
        spaces_area,
    );
    if controls_area.height > 0 {
        frame.render_widget(
            Paragraph::new(sidebar_heading("new", Some("menu"), content.width, tokens))
                .style(background),
            controls_area,
        );
    }
    if divider_area.height > 0 {
        // The sections divider: a visible line the user drags up or down to
        // give workspaces or agents more room; lit under the pointer.
        let style = if app.hovered_split() {
            accent_style(tokens)
        } else {
            dim_style(tokens)
        };
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(content.width)))
                .style(style.bg(ratatui_color(tokens.sidebar_bg))),
            divider_area,
        );
    }
    frame.render_widget(
        Paragraph::new(agents).style(background),
        agents_area,
    );
}

/// Header row: the collapse toggle button, then the panel name. Same single
/// row as the tab bar, so both texts sit level and centered in their band.
fn workspaces_heading(width: u16, tokens: &ThemeTokens) -> Vec<Line<'static>> {
    let label = " Workspaces";
    let used = 3 + label.chars().count();
    let padding = usize::from(width).saturating_sub(used);
    vec![Line::from(vec![
        Span::styled(
            " ◫ ",
            Style::default()
                .fg(ratatui_color(tokens.fg))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{label}{}", " ".repeat(padding)),
            dim_style(tokens),
        ),
    ])]
}

/// The agents section header: `agents` centered, no sort label (Cesar
/// dropped it). While an agent works, a bright band sweeps the word in step
/// with the spinner so the section itself reads as busy.
fn agents_heading(width: u16, tokens: &ThemeTokens, shimmer: Option<usize>) -> Line<'static> {
    let label = "agents";
    let width = usize::from(width);
    let left = width.saturating_sub(label.len()) / 2;
    let mut spans = vec![Span::styled(" ".repeat(left), dim_style(tokens))];
    match shimmer {
        None => spans.push(Span::styled(label, dim_style(tokens))),
        Some(frame) => {
            let period = label.len() + 4;
            let head = (frame % period) as isize - 2;
            for (index, character) in label.chars().enumerate() {
                let style = match (index as isize - head).abs() {
                    0 => Style::default()
                        .fg(ratatui_color(tokens.fg))
                        .add_modifier(Modifier::BOLD),
                    1 => Style::default().fg(ratatui_color(tokens.fg)),
                    _ => dim_style(tokens),
                };
                spans.push(Span::styled(character.to_string(), style));
            }
        }
    }
    let used = left + label.len();
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        dim_style(tokens),
    ));
    Line::from(spans)
}

fn sidebar_heading(
    left: &str,
    right: Option<&str>,
    width: u16,
    tokens: &ThemeTokens,
) -> Line<'static> {
    let left = format!(" {left}");
    let right = right.map_or_else(String::new, |value| format!("{value} "));
    let padding = usize::from(width).saturating_sub(left.chars().count() + right.chars().count());
    Line::styled(
        format!("{left}{}{right}", " ".repeat(padding)),
        dim_style(tokens),
    )
}

fn append_workspace_rows(
    lines: &mut Vec<Line<'static>>,
    workspace: &WorkspaceInfo,
    workspace_index: usize,
    state: &str,
    rows: &[Vec<RowToken>],
    base: Style,
    tokens: &ThemeTokens,
    selected: bool,
    width: u16,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let values = row
            .iter()
            .filter_map(|token| {
                let value = match token.token() {
                    "index" => (workspace_index + 1).to_string(),
                    "state_icon" => workspace_state_icon(state).to_owned(),
                    "state_text" => state.to_owned(),
                    "workspace" => workspace.label.clone(),
                    "branch" => workspace_branch(workspace),
                    "git_status" => workspace.tokens.get("git_status").cloned().unwrap_or_default(),
                    custom if custom.starts_with('$') => workspace
                        .tokens
                        .get(&custom[1..])
                        .cloned()
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                (!value.is_empty()).then(|| (token, value))
            })
            .collect::<Vec<_>>();
        if !values.is_empty() {
            lines.push(line_from_values(
                values,
                base,
                tokens,
                state,
                selected,
                width,
                row_index > 0,
            ));
        }
    }
}

fn append_agent_rows<L: ServerLink>(
    lines: &mut Vec<Line<'static>>,
    app: &App<L>,
    agent: &AgentInfo,
    base: Style,
    tokens: &ThemeTokens,
    width: u16,
) {
    let Some(snapshot) = app.snapshot() else {
        return;
    };
    let rows = app
        .config()
        .ui
        .sidebar
        .agents
        .rows_by_agent
        .get(&agent.agent)
        .unwrap_or(&app.config().ui.sidebar.agents.rows);
    let state = status_name(&agent.agent_status);
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id);
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot.panes.iter().find(|pane| pane.pane_id == agent.pane_id);
    let base = if agent.focused {
        Style::default()
            .fg(ratatui_color(tokens.sidebar_selected_fg))
            .bg(ratatui_color(tokens.sidebar_selected_bg))
    } else {
        base
    };
    for (row_index, row) in rows.iter().enumerate() {
        let values = row
            .iter()
            .filter_map(|token| {
                let value = match token.token() {
                    "state_icon" => agent_state_icon(&state, app.spinner_frame()).to_owned(),
                    "state_text" => state.clone(),
                    "workspace" => workspace.map(|item| item.label.clone()).unwrap_or_default(),
                    "tab" => tab.map(|item| item.label.clone()).unwrap_or_default(),
                    "pane" => pane
                        .and_then(|item| item.label.clone())
                        .unwrap_or_else(|| agent.pane_id.clone()),
                    "agent" => agent.name.clone().unwrap_or_else(|| agent.agent.clone()),
                    "agent_kind" => agent.agent.clone(),
                    "terminal_title" => agent.terminal_title.clone().unwrap_or_default(),
                    "terminal_title_stripped" => {
                        agent.terminal_title_stripped.clone().unwrap_or_default()
                    }
                    custom if custom.starts_with('$') => agent
                        .tokens
                        .get(&custom[1..])
                        .cloned()
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                (!value.is_empty()).then(|| (token, value))
            })
            .collect::<Vec<_>>();
        if !values.is_empty() {
            lines.push(line_from_values(
                values,
                base,
                tokens,
                &state,
                agent.focused,
                width,
                row_index > 0,
            ));
        }
    }
}

fn line_from_values(
    values: Vec<(&RowToken, String)>,
    base: Style,
    tokens: &ThemeTokens,
    state: &str,
    selected: bool,
    width: u16,
    indented: bool,
) -> Line<'static> {
    let mut spans = vec![Span::styled(if indented { "  " } else { " " }, base)];
    let mut previous_token = None;
    for (index, (token, value)) in values.into_iter().enumerate() {
        if index > 0 {
            let separator = match (previous_token, token.token()) {
                (Some("state_icon" | "index"), _) | (_, "state_icon") => " ",
                (Some("pane" | "workspace"), "agent_kind") => "·",
                _ => " · ",
            };
            spans.push(Span::styled(separator, base.fg(ratatui_color(tokens.dim_fg))));
        }
        let mut style = match token.token() {
            "state_icon" | "state_text" => base.fg(ratatui_color(state_color(tokens, state))),
            "branch" if selected => base.fg(ratatui_color(tokens.accent)),
            "index" | "branch" | "git_status" => base.fg(ratatui_color(tokens.dim_fg)),
            "workspace" | "pane" | "agent" | "agent_kind" => base
                .fg(ratatui_color(tokens.sidebar_fg))
                .add_modifier(Modifier::BOLD),
            _ => base,
        };
        if let RowToken::Styled(styled) = token {
            if let Some(fg) = styled
                .fg
                .as_deref()
                .and_then(|value| Color::from_str(value).ok())
            {
                style = style.fg(ratatui_color(fg));
            }
            if let Some(bold) = styled.bold {
                style = if bold {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style.remove_modifier(Modifier::BOLD)
                };
            }
            if let Some(dim) = styled.dim {
                style = if dim {
                    style.add_modifier(Modifier::DIM)
                } else {
                    style.remove_modifier(Modifier::DIM)
                };
            }
        }
        spans.push(Span::styled(value, style));
        previous_token = Some(token.token());
    }
    let used = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let padding = usize::from(width).saturating_sub(used);
    if padding > 0 {
        spans.push(Span::styled(" ".repeat(padding), base));
    }
    Line::from(spans)
}

fn workspace_branch(workspace: &WorkspaceInfo) -> String {
    workspace
        .worktree
        .as_ref()
        .map(|worktree| worktree.branch.clone())
        .or_else(|| workspace.tokens.get("branch").cloned())
        .or_else(|| workspace.tokens.get("git_branch").cloned())
        .or_else(|| {
            workspace
                .cwd
                .rsplit(['/', '\\'])
                .find(|part| !part.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn render_mobile_header<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>, area: Rect) {
    let tokens = &app.theme().tokens;
    let label = app.snapshot().map_or_else(
        || "Starcil · waiting for session".to_owned(),
        |snapshot| {
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == snapshot.focused_workspace_id)
                .map(|workspace| workspace.label.as_str())
                .unwrap_or("workspace");
            let tab = snapshot
                .tabs
                .iter()
                .find(|tab| tab.tab_id == snapshot.focused_tab_id)
                .map(|tab| tab.label.as_str())
                .unwrap_or("tab");
            format!(" ◆ {workspace} · {tab}")
        },
    );
    frame.render_widget(
        Paragraph::new(label).style(
            Style::default()
                .fg(ratatui_color(tokens.tab_active_fg))
                .bg(ratatui_color(tokens.tab_active_bg)),
        ),
        area,
    );
}

fn should_show_tab_bar<L: ServerLink>(app: &App<L>) -> bool {
    let Some(snapshot) = app.snapshot() else {
        return true;
    };
    let count = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == snapshot.focused_workspace_id)
        .count();
    !(app.config().ui.hide_tab_bar_when_single_tab && count == 1)
}

fn render_tab_bar<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>, area: Rect) {
    let tokens = &app.theme().tokens;
    let hovered = app.hovered_chrome();
    let dragging = app.dragging_tab();
    let mut spans = Vec::new();
    let mut used = 0u16;
    if let Some(snapshot) = app.snapshot() {
        for tab in snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == snapshot.focused_workspace_id)
        {
            let width = tab_block_width(&tab.label, area.width.saturating_sub(used));
            if width == 0 {
                break;
            }
            let active = tab.tab_id == snapshot.focused_tab_id;
            let hover = matches!(hovered, Some(ChromeTarget::Tab(id)) if id == &tab.tab_id);
            let style = if dragging == Some(tab.tab_id.as_str()) {
                // Mid-drag: the grabbed tab is underlined on top of the
                // active look so the reorder reads as a grab, not a click.
                Style::default()
                    .fg(ratatui_color(tokens.tab_active_fg))
                    .bg(ratatui_color(tokens.tab_active_bg))
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else if active {
                Style::default()
                    .fg(ratatui_color(tokens.tab_active_fg))
                    .bg(ratatui_color(tokens.tab_active_bg))
                    .add_modifier(Modifier::BOLD)
            } else if hover {
                // Hovered inactive tab: the label lights up in place.
                Style::default()
                    .fg(ratatui_color(tokens.fg))
                    .bg(ratatui_color(tokens.panel_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(ratatui_color(tokens.tab_inactive_fg))
                    .bg(ratatui_color(tokens.panel_bg))
            };
            spans.push(Span::styled(padded_tab_label(&tab.label, width), style));
            used = used.saturating_add(width);
        }
        if area.width.saturating_sub(used) >= 3 {
            // The `+` takes the active-tab look under the pointer so it reads
            // as the button it is.
            let style = if matches!(hovered, Some(ChromeTarget::NewTab)) {
                Style::default()
                    .fg(ratatui_color(tokens.tab_active_fg))
                    .bg(ratatui_color(tokens.tab_active_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(ratatui_color(tokens.tab_inactive_fg))
                    .bg(ratatui_color(tokens.panel_bg))
                    .add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled(" + ", style));
        }
    }
    // One text row flush above the panes. A cell row is indivisible, so
    // this is the only height that keeps the labels vertically centered in
    // the band.
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(ratatui_color(tokens.panel_bg))),
        area,
    );
}

fn tab_block_width(label: &str, remaining: u16) -> u16 {
    (label.chars().count() as u16 + 2).max(10).min(remaining)
}

fn padded_tab_label(label: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let available = usize::from(width.saturating_sub(2));
    let label = label.chars().take(available).collect::<String>();
    let padding = available.saturating_sub(label.chars().count());
    format!(" {label}{} ", " ".repeat(padding))
}

fn render_panes<L: ServerLink>(
    app: &App<L>,
    frame: &mut Frame<'_>,
    area: Rect,
    mobile: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(snapshot) = app.snapshot() else {
        frame.render_widget(
            Paragraph::new(" Waiting for server snapshot…")
                .style(dim_style(&app.theme().tokens)),
            area,
        );
        return;
    };
    let Some(layout) = snapshot
        .layouts
        .iter()
        .find(|layout| layout.tab_id == snapshot.focused_tab_id)
    else {
        frame.render_widget(
            Paragraph::new(" Waiting for pane layout…").style(dim_style(&app.theme().tokens)),
            area,
        );
        return;
    };

    let zoomed = app
        .local_zoom()
        .map(str::to_owned)
        .or_else(|| layout.zoomed.clone())
        .or_else(|| {
            snapshot
                .tabs
                .iter()
                .find(|tab| tab.tab_id == snapshot.focused_tab_id)
                .and_then(|tab| tab.zoomed.clone())
        });
    let multi_pane = layout.panes.len() > 1;
    for (pane_index, pane) in layout.panes.iter().enumerate() {
        if zoomed.as_ref().is_some_and(|zoomed| zoomed != &pane.pane_id) {
            continue;
        }
        let mut pane_area = if zoomed.is_some() {
            area
        } else if mobile {
            let index = layout
                .panes
                .iter()
                .position(|candidate| candidate.pane_id == pane.pane_id)
                .unwrap_or_default() as u32;
            let count = layout.panes.len().max(1) as u32;
            let start = index * area.height as u32 / count;
            let end = (index + 1) * area.height as u32 / count;
            Rect {
                x: area.x,
                y: area.y + start as u16,
                width: area.width,
                height: end.saturating_sub(start).max(1) as u16,
            }
        } else {
            map_pane_rect(layout, pane.rect, area)
        };
        if app.config().ui.pane_gaps && multi_pane {
            let leading_x = zoomed.is_some() || mobile || pane.rect.x == layout.area.x;
            let leading_y = zoomed.is_some()
                || (mobile && pane_index == 0)
                || (!mobile && pane.rect.y == layout.area.y);
            apply_one_cell_gap(&mut pane_area, leading_x, leading_y);
        }
        render_pane(
            app,
            frame,
            &pane.pane_id,
            pane.focused,
            pane_area,
            app.config().ui.pane_borders && multi_pane,
        );
    }
}

fn apply_one_cell_gap(area: &mut Rect, leading_x: bool, leading_y: bool) {
    let left = u16::from(leading_x && area.width > 1);
    let top = u16::from(leading_y && area.height > 1);
    area.x = area.x.saturating_add(left);
    area.y = area.y.saturating_add(top);
    area.width = area.width.saturating_sub(left.saturating_add(1));
    area.height = area.height.saturating_sub(top.saturating_add(1));
}

fn map_pane_rect(
    layout: &PaneLayoutSnapshot,
    pane: starcil_protocol::types::PaneRect,
    target: Rect,
) -> Rect {
    if layout.area.width == 0 || layout.area.height == 0 {
        return target;
    }
    let relative_x = pane.x.saturating_sub(layout.area.x) as u32;
    let relative_y = pane.y.saturating_sub(layout.area.y) as u32;
    let start_x = target.x as u32
        + relative_x * target.width as u32 / layout.area.width as u32;
    let start_y = target.y as u32
        + relative_y * target.height as u32 / layout.area.height as u32;
    let end_x = target.x as u32
        + (relative_x + pane.width as u32) * target.width as u32 / layout.area.width as u32;
    let end_y = target.y as u32
        + (relative_y + pane.height as u32) * target.height as u32 / layout.area.height as u32;
    Rect {
        x: start_x as u16,
        y: start_y as u16,
        width: end_x.saturating_sub(start_x).max(1) as u16,
        height: end_y.saturating_sub(start_y).max(1) as u16,
    }
}

fn render_pane<L: ServerLink>(
    app: &App<L>,
    frame: &mut Frame<'_>,
    pane_id: &str,
    focused: bool,
    area: Rect,
    draw_border: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let tokens = &app.theme().tokens;
    let border_color = if focused {
        tokens.accent
    } else {
        tokens.pane_border_inactive
    };
    let title = pane_title(app, pane_id);
    let mut content = area;
    if draw_border {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ratatui_color(border_color)))
            .title(Span::styled(
                format!(" {title} "),
                if focused {
                    accent_style(tokens)
                } else {
                    dim_style(tokens)
                },
            ));
        content = block.inner(area);
        frame.render_widget(block, area);
    }
    let Some(mirror) = app.mirror(pane_id) else {
        return;
    };
    let rows = content.height.min(mirror.rows());
    let cols = content.width.min(mirror.cols());
    for row in 0..rows {
        for col in 0..cols {
            let Some(source) = mirror.cell(row, col) else {
                continue;
            };
            let style = source.style;
            let target = &mut frame.buffer_mut()[(content.x + col, content.y + row)];
            target.set_char(source.ch);
            target.set_style(if app.selection().is_selected(pane_id, row, col) {
                style
                    .fg(ratatui_color(tokens.bg))
                    .bg(ratatui_color(tokens.selection))
            } else {
                style
            });
        }
    }
    if should_draw_cursor(app.config().ui.host_cursor) {
        if let Some(cursor) = mirror.cursor().filter(|cursor| cursor.visible) {
            if cursor.row < rows
                && cursor.col < cols
                && !app.selection().is_selected(pane_id, cursor.row, cursor.col)
            {
                frame.buffer_mut()[(content.x + cursor.col, content.y + cursor.row)].set_style(
                    Style::default()
                        .fg(ratatui_color(tokens.bg))
                        .bg(ratatui_color(tokens.cursor)),
                );
            }
        }
    }
    if let Some(copy) = app
        .scrollback()
        .copy_state()
        .filter(|copy| copy.pane_id == pane_id)
    {
        if copy.cursor.row < rows && copy.cursor.col < cols {
            frame.buffer_mut()[(content.x + copy.cursor.col, content.y + copy.cursor.row)]
                .set_style(
                    Style::default()
                        .fg(ratatui_color(tokens.bg))
                        .bg(ratatui_color(tokens.accent))
                        .add_modifier(Modifier::BOLD),
                );
        }
    }
    let offset = app.scrollback().offset(pane_id);
    if offset > 0 && content.width > 0 && content.height > 0 {
        let badge = format!("[{offset} lines up]");
        let width = (badge.chars().count() as u16).min(content.width);
        let rect = Rect {
            x: content.x + content.width.saturating_sub(width),
            y: content.y,
            width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(badge).style(
                Style::default()
                    .fg(ratatui_color(tokens.bg))
                    .bg(ratatui_color(tokens.accent))
                    .add_modifier(Modifier::BOLD),
            ),
            rect,
        );
    }
}

fn pane_title<L: ServerLink>(app: &App<L>, pane_id: &str) -> String {
    let pane = app
        .snapshot()
        .and_then(|snapshot| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id));
    pane.and_then(|pane| pane.label.clone())
        .or_else(|| {
            app.config()
                .ui
                .show_agent_labels_on_pane_borders
                .then(|| pane.and_then(agent_label))
                .flatten()
        })
        .unwrap_or_else(|| pane_id.to_owned())
}

fn agent_label(pane: &PaneInfo) -> Option<String> {
    // The agent kind ("claude", "codex", "opencode", …) identifies which CLI
    // runs in the pane at a glance; a user-given name is only a fallback.
    pane.agent.clone().or_else(|| pane.agent_name.clone())
}

fn render_modal<L: ServerLink>(app: &App<L>, frame: &mut Frame<'_>, area: Rect, modal: &Modal) {
    let tokens = &app.theme().tokens;
    let modal_area = modal_rect(app, area, modal);
    match modal {
        Modal::ContextMenu { target, selected, .. } => {
            render_context_menu(frame, modal_area, target, *selected, tokens);
            return;
        }
        Modal::Menu { selected } => {
            render_menu(app, frame, modal_area, *selected, tokens);
            return;
        }
        _ => {}
    }
    // Everything under a centered modal dims so the overlay reads as a layer.
    dim_background(frame, area);
    frame.render_widget(Clear, modal_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ratatui_color(tokens.accent)))
        .style(
            Style::default()
                .fg(ratatui_color(tokens.fg))
                .bg(ratatui_color(tokens.panel_bg)),
        );
    match modal {
        Modal::Help { filter } => {
            let mut lines = vec![
                Line::styled(" EFFECTIVE KEYMAP", accent_style(tokens).add_modifier(Modifier::BOLD)),
                Line::styled(
                    if filter.is_empty() {
                        " / filter · Esc close".to_owned()
                    } else {
                        format!(" filter: {filter}")
                    },
                    dim_style(tokens),
                ),
            ];
            let filter = filter.to_ascii_lowercase();
            for (chord, binding) in app
                .keymap()
                .terminal
                .iter()
                .chain(app.keymap().navigate.iter())
            {
                let action = binding.action.name();
                if !filter.is_empty()
                    && !action.contains(&filter)
                    && !chord.to_string().contains(&filter)
                {
                    continue;
                }
                lines.push(Line::from(vec![
                    Span::styled(format!(" {:<22}", chord), accent_style(tokens)),
                    Span::styled(action.to_owned(), Style::default().fg(ratatui_color(tokens.fg))),
                ]));
            }
            frame.render_widget(
                Paragraph::new(lines).block(block.title(" Help ")).wrap(Wrap { trim: false }),
                modal_area,
            );
        }
        Modal::WorkspacePicker { selected } => {
            let mut lines = vec![Line::styled(" Select a workspace", dim_style(tokens))];
            if let Some(snapshot) = app.snapshot() {
                for (index, workspace) in snapshot.workspaces.iter().enumerate() {
                    let style = if index == *selected {
                        Style::default()
                            .fg(ratatui_color(tokens.selection))
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(ratatui_color(tokens.fg))
                    };
                    lines.push(Line::styled(format!(" {} {}", state_icon(&workspace_state(snapshot, &workspace.workspace_id)), workspace.label), style));
                }
            }
            frame.render_widget(
                Paragraph::new(lines).block(block.title(" Workspaces ")),
                modal_area,
            );
        }
        Modal::Confirm { action, .. } => frame.render_widget(
            Paragraph::new(vec![
                Line::styled(format!(" Run {}?", action.name()), accent_style(tokens)),
                Line::styled(" y / Enter confirm · n / Esc cancel", dim_style(tokens)),
            ])
            .block(block.title(" Confirm ")),
            modal_area,
        ),
        Modal::UpdatePrompt { version } => frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!(" Starcil {version} is ready to install."),
                    Style::default().fg(ratatui_color(tokens.fg)),
                ),
                Line::styled(" y / Enter update now · n / Esc later", dim_style(tokens)),
            ])
            .block(block.title(" Update ")),
            modal_area,
        ),
        Modal::Prompt { kind, value } => frame.render_widget(
            Paragraph::new(vec![
                Line::styled(prompt_label(*kind), dim_style(tokens)),
                Line::styled(format!(" {value}▏"), accent_style(tokens)),
                Line::styled(" Enter save · Esc cancel", dim_style(tokens)),
            ])
            .block(block.title(" Name ")),
            modal_area,
        ),
        Modal::Settings => {
            let editor = app.settings();
            let mut lines = vec![Line::styled(
                " settings",
                Style::default()
                    .fg(ratatui_color(tokens.fg))
                    .add_modifier(Modifier::BOLD),
            )];
            lines.resize_with(SETTINGS_SECTION_INNER_LINE, Line::default);
            let mut bar = vec![Span::styled(" ", Style::default())];
            for (index, name) in SECTION_NAMES.iter().enumerate() {
                let style = if index == editor.section_index() {
                    Style::default()
                        .fg(ratatui_color(tokens.bg))
                        .bg(ratatui_color(tokens.accent))
                        .add_modifier(Modifier::BOLD)
                } else {
                    dim_style(tokens)
                };
                bar.push(Span::styled(format!(" {name} "), style));
                bar.push(Span::raw(" "));
            }
            lines.push(Line::from(bar));
            lines.resize_with(SETTINGS_ROWS_INNER_LINE, Line::default);
            for (index, row) in editor.rows().into_iter().enumerate() {
                let selected = index == editor.selected_index();
                let marker = if selected { " ► " } else { "   " };
                match row {
                    SettingsRow::Theme(name) => {
                        let active = app.config().theme.name == name;
                        let check = if active { " ✓" } else { "" };
                        let mut style = if selected {
                            accent_style(tokens)
                        } else {
                            Style::default().fg(ratatui_color(tokens.fg))
                        };
                        if active || selected {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        lines.push(Line::styled(format!("{marker}{name}{check}"), style));
                    }
                    SettingsRow::Setting(setting) => {
                        let value = if selected {
                            editor
                                .editing()
                                .map(|buffer| format!("{buffer}▏"))
                                .unwrap_or_else(|| editor.value(setting, app.config()))
                        } else {
                            editor.value(setting, app.config())
                        };
                        let style = if selected {
                            Style::default()
                                .fg(ratatui_color(tokens.bg))
                                .bg(ratatui_color(tokens.selection))
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(ratatui_color(tokens.fg))
                        };
                        lines.push(Line::styled(
                            format!("{marker}{:<26}  {value}", setting.label()),
                            style,
                        ));
                    }
                }
            }
            if let Some(error) = editor.error() {
                lines.push(Line::styled(format!(" ! {error}"), state_style(tokens, "blocked")));
            } else {
                lines.push(Line::default());
            }
            lines.push(Line::from(vec![
                Span::styled(" ↑↓ select   tab section  ", dim_style(tokens)),
                Span::styled(
                    " ↵ apply ",
                    Style::default()
                        .fg(ratatui_color(tokens.bg))
                        .bg(ratatui_color(tokens.accent))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  esc close", dim_style(tokens)),
            ]));
            frame.render_widget(
                Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
                modal_area,
            );
        }
        Modal::Menu { .. } | Modal::ContextMenu { .. } => {
            unreachable!("popup menus render before centered modals")
        }
        Modal::Onboarding => frame.render_widget(
            Paragraph::new(vec![
                Line::styled(" Welcome to Starcil", accent_style(tokens).add_modifier(Modifier::BOLD)),
                Line::styled(" Choose how background agents notify you:", Style::default().fg(ratatui_color(tokens.fg))),
                Line::styled(" 1  Starcil toast", accent_style(tokens)),
                Line::styled(" 2  Terminal notification", Style::default().fg(ratatui_color(tokens.fg))),
                Line::styled(" 3  System notification", Style::default().fg(ratatui_color(tokens.fg))),
                Line::styled(" 4  Off", dim_style(tokens)),
            ])
            .block(block.title(" First run ")),
            modal_area,
        ),
    }
}

fn render_menu<L: ServerLink>(
    app: &App<L>,
    frame: &mut Frame<'_>,
    rect: Rect,
    selected: usize,
    tokens: &ThemeTokens,
) {
    let lines = app
        .menu_actions()
        .into_iter()
        .enumerate()
        .map(|(index, action)| {
            let is_selected = index == selected;
            let style = if is_selected {
                Style::default()
                    .fg(ratatui_color(tokens.bg))
                    .bg(ratatui_color(tokens.selection))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ratatui_color(tokens.fg))
            };
            if action == MenuAction::ApplyUpdate {
                let mut bullet = accent_style(tokens);
                if is_selected {
                    bullet = bullet
                        .bg(ratatui_color(tokens.selection))
                        .add_modifier(Modifier::BOLD);
                }
                Line::from(vec![
                    Span::styled(" ●", bullet),
                    Span::styled(format!(" {}", action.label()), style),
                ])
            } else {
                Line::styled(format!(" {}", action.label()), style)
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" Menu ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ratatui_color(tokens.accent)))
                .style(
                    Style::default()
                        .fg(ratatui_color(tokens.fg))
                        .bg(ratatui_color(tokens.panel_bg)),
                ),
        ),
        rect,
    );
}

fn render_context_menu(
    frame: &mut Frame<'_>,
    rect: Rect,
    target: &ContextTarget,
    selected: usize,
    tokens: &ThemeTokens,
) {
    let lines = ContextMenuAction::items(target)
        .iter()
        .copied()
        .enumerate()
        .map(|(index, action)| {
            let style = if index == selected {
                Style::default()
                    .fg(ratatui_color(tokens.bg))
                    .bg(ratatui_color(tokens.selection))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ratatui_color(tokens.fg))
            };
            Line::styled(format!(" {}", action.label()), style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(target.title())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ratatui_color(tokens.accent)))
                .style(
                    Style::default()
                        .fg(ratatui_color(tokens.fg))
                        .bg(ratatui_color(tokens.panel_bg)),
                ),
        ),
        rect,
    );
}

fn prompt_label(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::NewWorkspace => "New workspace name",
        PromptKind::RenameWorkspace => "Rename workspace",
        PromptKind::NewTab => "New tab name",
        PromptKind::RenameTab => "Rename tab",
        PromptKind::RenamePane => "Rename pane",
    }
}

fn render_toast<L: ServerLink>(
    app: &App<L>,
    frame: &mut Frame<'_>,
    area: Rect,
    toast: &crate::app::ToastMessage,
) {
    let tokens = &app.theme().tokens;
    let width = (toast.message.chars().count() as u16 + 4).min(area.width).max(1);
    let height = 3.min(area.height).max(1);
    let x = match toast.position {
        ToastPosition::TopLeft | ToastPosition::BottomLeft => area.x,
        ToastPosition::TopCenter | ToastPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastPosition::TopRight | ToastPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match toast.position {
        ToastPosition::TopLeft | ToastPosition::TopCenter | ToastPosition::TopRight => area.y,
        ToastPosition::BottomLeft
        | ToastPosition::BottomCenter
        | ToastPosition::BottomRight => {
            area.y + area.height.saturating_sub(height)
        }
    };
    let rect = Rect { x, y, width, height };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(toast.message.clone())
            .style(
                Style::default()
                    .fg(ratatui_color(tokens.toast_fg))
                    .bg(ratatui_color(tokens.toast_bg)),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ratatui_color(tokens.accent))),
            ),
        rect,
    );
}

fn render_mouse_debug(frame: &mut Frame<'_>, area: Rect, line: String, tokens: &ThemeTokens) {
    let width = (line.chars().count() as u16 + 2).min(area.width).max(1);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width),
        y: area.y + area.height.saturating_sub(1),
        width,
        height: 1,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(format!(" {line} ")).style(
            Style::default()
                .fg(ratatui_color(tokens.accent))
                .bg(ratatui_color(tokens.panel_bg))
                .add_modifier(Modifier::BOLD),
        ),
        rect,
    );
}

fn dim_background(frame: &mut Frame<'_>, area: Rect) {
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            cell.fg = dimmed_color(cell.fg);
            cell.bg = dimmed_color(cell.bg);
            cell.modifier.insert(Modifier::DIM);
        }
    }
}

fn dimmed_color(color: RatatuiColor) -> RatatuiColor {
    match color {
        // Truecolor cells darken directly; palette cells rely on the DIM
        // attribute because their exact values belong to the host terminal.
        RatatuiColor::Rgb(red, green, blue) => RatatuiColor::Rgb(red / 2, green / 2, blue / 2),
        other => other,
    }
}

fn workspace_state(snapshot: &starcil_protocol::types::SessionSnapshot, workspace_id: &str) -> String {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.workspace_id == workspace_id)
        .map(|agent| status_name(&agent.agent_status))
        .min_by_key(|state| state_priority(state))
        .unwrap_or_else(|| "idle".to_owned())
}

fn status_name(status: &starcil_domain::AgentStatus) -> String {
    status.as_str().to_owned()
}

fn state_priority(state: &str) -> u8 {
    match state {
        "blocked" => 0,
        "working" => 1,
        "done" => 2,
        "idle" => 3,
        _ => 4,
    }
}

fn state_icon(state: &str) -> &'static str {
    workspace_state_icon(state)
}

fn workspace_state_icon(state: &str) -> &'static str {
    match state {
        "blocked" | "done" | "working" => "●",
        "idle" => "○",
        "unknown" => "·",
        _ => "·",
    }
}

/// Braille spinner frames for a working agent; the app ticks
/// the frame index, so the sidebar animates while anything is working.
pub(crate) const SPINNER_FRAMES: [&str; 10] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn agent_state_icon(state: &str, spinner_frame: usize) -> &'static str {
    match state {
        "blocked" => "◉",
        "done" => "●",
        "working" => SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()],
        "idle" => "✓",
        _ => "·",
    }
}

fn state_style(tokens: &ThemeTokens, state: &str) -> Style {
    Style::default().fg(ratatui_color(state_color(tokens, state)))
}

fn state_color(tokens: &ThemeTokens, state: &str) -> Color {
    match state {
        "blocked" => tokens.state_blocked,
        "done" => tokens.state_done,
        "working" => tokens.state_working,
        "unknown" => tokens.state_unknown,
        _ => tokens.state_idle,
    }
}

fn accent_style(tokens: &ThemeTokens) -> Style {
    Style::default().fg(ratatui_color(tokens.accent))
}

fn dim_style(tokens: &ThemeTokens) -> Style {
    Style::default().fg(ratatui_color(tokens.dim_fg))
}

fn should_draw_cursor(policy: HostCursor) -> bool {
    policy == HostCursor::Drawn || policy == HostCursor::Auto && cfg!(windows)
}

pub fn ratatui_color(color: Color) -> RatatuiColor {
    match color {
        Color::Rgb(red, green, blue) => RatatuiColor::Rgb(red, green, blue),
        Color::Reset => RatatuiColor::Reset,
        Color::Named(named) => match named {
            NamedColor::Black => RatatuiColor::Black,
            NamedColor::DarkGray => RatatuiColor::DarkGray,
            NamedColor::Gray => RatatuiColor::Gray,
            NamedColor::White => RatatuiColor::White,
            NamedColor::Red => RatatuiColor::Red,
            NamedColor::LightRed => RatatuiColor::LightRed,
            NamedColor::Green => RatatuiColor::Green,
            NamedColor::LightGreen => RatatuiColor::LightGreen,
            NamedColor::Yellow => RatatuiColor::Yellow,
            NamedColor::LightYellow => RatatuiColor::LightYellow,
            NamedColor::Blue => RatatuiColor::Blue,
            NamedColor::LightBlue => RatatuiColor::LightBlue,
            NamedColor::Magenta => RatatuiColor::Magenta,
            NamedColor::LightMagenta => RatatuiColor::LightMagenta,
            NamedColor::Cyan => RatatuiColor::Cyan,
            NamedColor::LightCyan => RatatuiColor::LightCyan,
        },
    }
}

pub fn protocol_style(style: StyleDef) -> Style {
    let mut result = Style::default();
    if let Some(fg) = packed_color(style.fg) {
        result = result.fg(fg);
    }
    if let Some(bg) = packed_color(style.bg) {
        result = result.bg(bg);
    }
    let mut modifiers = Modifier::empty();
    if style.attrs & attr_bits::BOLD != 0 {
        modifiers |= Modifier::BOLD;
    }
    if style.attrs & attr_bits::DIM != 0 {
        modifiers |= Modifier::DIM;
    }
    if style.attrs & attr_bits::ITALIC != 0 {
        modifiers |= Modifier::ITALIC;
    }
    if style.attrs & attr_bits::UNDERLINE != 0 {
        modifiers |= Modifier::UNDERLINED;
    }
    if style.attrs & attr_bits::INVERSE != 0 {
        modifiers |= Modifier::REVERSED;
    }
    if style.attrs & attr_bits::HIDDEN != 0 {
        modifiers |= Modifier::HIDDEN;
    }
    if style.attrs & attr_bits::STRIKETHROUGH != 0 {
        modifiers |= Modifier::CROSSED_OUT;
    }
    if style.attrs & attr_bits::BLINK != 0 {
        modifiers |= Modifier::SLOW_BLINK;
    }
    result.add_modifier(modifiers)
}

fn packed_color(packed: u32) -> Option<RatatuiColor> {
    match packed >> 24 {
        0x01 => Some(RatatuiColor::Rgb(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        )),
        0x02 => Some(RatatuiColor::Indexed((packed & 0xff) as u8)),
        _ => None,
    }
}
