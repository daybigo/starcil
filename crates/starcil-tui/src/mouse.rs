//! Mouse hit-testing, divider dragging, wheel routing, and pane-menu intent.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use starcil_config::{AgentPanelSort, TabBarPosition};
use starcil_protocol::attach::{PaneMouseEncoding, PaneMouseMode, PaneMouseTracking};
use starcil_protocol::types::{PaneLayoutSnapshot, PaneRect};

use crate::app::{App, ContextMenuAction, ContextTarget, Modal, Mode, SidebarState};
use crate::link::ServerLink;
use crate::settings::{SECTION_NAMES, SettingsRow};

pub(crate) const SETTINGS_SECTION_INNER_LINE: usize = 1;
pub(crate) const SETTINGS_ROWS_INNER_LINE: usize = 3;

/// Tab bar height. One text row: a cell row is indivisible, so this is the
/// only height where the labels sit vertically centered in the band —
/// Cesar's "centrado". The Workspaces header matches.
pub(crate) const TAB_BAR_ROWS: u16 = 1;
/// Sidebar header height (the ◫ Workspaces button).
pub(crate) const SIDEBAR_HEADER_ROWS: u16 = 1;
/// Composer chrome under the dock stack: brand line + input + brand line +
/// the status row that carries the cwd (Claude Code's own double-line layout).
pub(crate) const COMPOSER_BASE_ROWS: u16 = 4;
/// At most this many dock buttons stack inside the composer.
pub(crate) const COMPOSER_MAX_DOCK: usize = 4;
/// Cells the ` 📁 open folder` row needs (the emoji is two cells wide).
pub(crate) const FOLDER_BUTTON_WIDTH: u16 = 16;
/// Rows between the workspaces section and the agents header: the
/// `new · menu` controls row and the draggable divider.
pub(crate) const SIDEBAR_SPLIT_TAIL: u16 = 2;

/// Which launcher row the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockHover {
    Item(usize),
    Folder,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MouseAction {
    Ignored,
    NewWorkspace,
    OpenMenu,
    NewTab,
    FocusPane(String),
    FocusWorkspace(String),
    FocusTab(String),
    /// A tab dragged along the bar crossed a neighbour: reorder it within its
    /// workspace (`insert_index` = its new position among the workspace tabs).
    MoveTab {
        tab_id: String,
        insert_index: usize,
    },
    FocusAgent(String),
    /// Click on the composer chrome (lines, status row): bring the live
    /// bottom back into view; the composer already owns the keyboard.
    FocusComposer,
    /// Click on the input row itself: put the cursor under the pointer
    /// (`offset` cells from the row's left edge, `width` cells in the row).
    PlaceComposerCursor {
        offset: u16,
        width: u16,
    },
    /// Click on dock item N (0-based).
    DockLaunch(usize),
    /// Click on the folder button or the cwd label.
    OpenFolderPicker,
    /// Click on the sidebar collapse toggle in the header.
    ToggleSidebar,
    BeginSelection {
        pane_id: String,
        row: u16,
        col: u16,
    },
    UpdateSelection {
        pane_id: String,
        row: u16,
        col: u16,
    },
    FinishSelection,
    Resize {
        pane_id: String,
        direction: &'static str,
        amount: f64,
    },
    ResizeSidebar {
        width: u16,
    },
    /// Dragging the divider between the workspaces and agents sections.
    ResizeSidebarSplit {
        percent: u8,
    },
    /// The divider drag ended: persist the split.
    SaveSidebarSplit,
    Scroll {
        pane_id: String,
        lines: i64,
        alternate_screen: bool,
    },
    ContextMenu {
        target: ContextTarget,
        x: u16,
        y: u16,
    },
    CloseModal,
    CloseModalAndContextMenu {
        target: ContextTarget,
        x: u16,
        y: u16,
    },
    ContextMenuItem {
        index: usize,
        activate: bool,
    },
    MenuItem {
        index: usize,
        activate: bool,
    },
    SettingsSection(usize),
    SettingsRow {
        index: usize,
        activate: bool,
    },
    Paste(String),
    Passthrough {
        pane_id: String,
        data_base64: String,
    },
    MouseTracking {
        pane_id: String,
        data_base64: String,
        focus: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRegion {
    pub pane_id: String,
    pub outer: Rect,
    pub content: Rect,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChromeTarget {
    NewWorkspace,
    Menu,
    NewTab,
    Workspace(String),
    Tab(String),
    Agent(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeRegion {
    pub rect: Rect,
    pub target: ChromeTarget,
}

/// The in-pane composer: launcher buttons + input drawn INSIDE the focused
/// shell pane (its PTY cedes these rows through `InputFrame::ReserveRows`).
/// Hidden whenever the focused pane hosts an agent CLI — a running agent has
/// its own composer and needs neither the input nor the shortcuts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerGeometry {
    pub pane_id: String,
    /// The launcher panel: a bordered, titled box (like the right-click
    /// menu) holding the dock rows and the folder row. Zero when no dock.
    pub dock_panel: Rect,
    /// Dock rows inside the panel, top to bottom.
    pub dock_items: Vec<Rect>,
    /// The open-folder button under the dock stack.
    pub folder_button: Rect,
    /// Brand line across the pane, above the input.
    pub border: Rect,
    pub input: Rect,
    /// Second brand line, below the input (double line like Claude Code).
    pub bottom_border: Rect,
    /// Status row under the bottom line; the clickable cwd sits on its left.
    pub spacer: Rect,
    pub cwd_label: Rect,
    /// Total pane-content rows the composer occupies (reserved server-side).
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiGeometry {
    pub area: Rect,
    pub sidebar: Rect,
    pub tab_bar: Option<Rect>,
    pub main: Rect,
    pub footer: Rect,
    pub panes: Vec<PaneRegion>,
    pub chrome: Vec<ChromeRegion>,
    /// The in-pane composer for the focused shell pane, when visible.
    pub composer: Option<ComposerGeometry>,
    /// The sidebar collapse toggle in the header (before "Workspaces").
    pub sidebar_toggle: Rect,
    /// The draggable divider row between the workspaces and agents sections
    /// (expanded sidebar only; zero otherwise).
    pub sidebar_split: Rect,
}

impl UiGeometry {
    pub fn calculate<L: ServerLink>(app: &App<L>, area: Rect) -> Self {
        let footer = Rect {
            x: area.x,
            y: area.y.saturating_add(area.height),
            width: area.width,
            height: 0,
        };
        let body = area;
        let mobile = area.width <= app.config().ui.mobile_width_threshold;
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
        if mobile && main.height > 0 {
            main.y += 1;
            main.height = main.height.saturating_sub(1);
        }
        let tab_rows = TAB_BAR_ROWS.min(main.height.saturating_sub(1));
        let tab_bar = if should_show_tab_bar(app) && tab_rows > 0 {
            Some(match app.config().ui.tab_bar_position {
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
            })
        } else {
            None
        };
        let mut panes = pane_regions(app, main, mobile);
        let zero = Rect::default();
        let composer = composer_geometry(app, &mut panes);
        let sidebar_toggle = match app.sidebar_state() {
            SidebarState::Expanded if sidebar.width > 3 => Rect {
                x: sidebar.x,
                y: sidebar.y,
                width: 4.min(sidebar.width),
                height: SIDEBAR_HEADER_ROWS.min(sidebar.height),
            },
            SidebarState::Compact if sidebar.width > 0 => Rect {
                x: sidebar.x,
                y: sidebar.y,
                width: sidebar.width,
                height: SIDEBAR_HEADER_ROWS.min(sidebar.height),
            },
            _ => zero,
        };
        let sidebar_split = match app.sidebar_state() {
            SidebarState::Expanded if sidebar.width > 1 => {
                let split = sidebar_split(app, sidebar.height);
                if split >= 1 {
                    Rect {
                        x: sidebar.x,
                        y: sidebar.y + split - 1,
                        width: sidebar.width - 1,
                        height: 1,
                    }
                } else {
                    zero
                }
            }
            _ => zero,
        };
        let mut geometry = Self {
            area,
            sidebar,
            tab_bar,
            main,
            footer,
            panes,
            chrome: Vec::new(),
            composer,
            sidebar_toggle,
            sidebar_split,
        };
        geometry.chrome = chrome_regions(app, &geometry);
        geometry
    }

    /// What a left click on the composer or the sidebar toggle does.
    pub fn bar_action_at(&self, x: u16, y: u16) -> Option<MouseAction> {
        if contains(self.sidebar_toggle, x, y) {
            return Some(MouseAction::ToggleSidebar);
        }
        let Some(composer) = &self.composer else {
            return None;
        };
        for (position, item) in composer.dock_items.iter().enumerate() {
            if contains(*item, x, y) {
                return Some(MouseAction::DockLaunch(position));
            }
        }
        if contains(composer.folder_button, x, y) || contains(composer.cwd_label, x, y) {
            return Some(MouseAction::OpenFolderPicker);
        }
        if contains(composer.input, x, y) {
            return Some(MouseAction::PlaceComposerCursor {
                offset: x - composer.input.x,
                width: composer.input.width,
            });
        }
        if contains(composer.border, x, y)
            || contains(composer.bottom_border, x, y)
            || contains(composer.spacer, x, y)
        {
            return Some(MouseAction::FocusComposer);
        }
        None
    }

    pub fn pane_at(&self, x: u16, y: u16) -> Option<&PaneRegion> {
        self.panes
            .iter()
            .find(|pane| contains(pane.outer, x, y))
    }

    pub fn content_at(&self, x: u16, y: u16) -> Option<(&PaneRegion, u16, u16)> {
        self.panes.iter().find_map(|pane| {
            contains(pane.content, x, y).then(|| {
                (
                    pane,
                    y.saturating_sub(pane.content.y),
                    x.saturating_sub(pane.content.x),
                )
            })
        })
    }

    pub fn chrome_at(&self, x: u16, y: u16) -> Option<&ChromeTarget> {
        self.chrome
            .iter()
            .find(|region| contains(region.rect, x, y))
            .map(|region| &region.target)
    }

    /// What a right click at (x, y) opens a menu for: a tab in the tab bar, a
    /// workspace in the sidebar, or the pane under the pointer.
    pub fn context_target_at(&self, x: u16, y: u16) -> Option<ContextTarget> {
        match self.chrome_at(x, y) {
            Some(ChromeTarget::Tab(id)) => return Some(ContextTarget::Tab(id.clone())),
            Some(ChromeTarget::Workspace(id)) => {
                return Some(ContextTarget::Workspace(id.clone()));
            }
            _ => {}
        }
        self.pane_at(x, y)
            .map(|pane| ContextTarget::Pane(pane.pane_id.clone()))
    }
}

/// The focused pane hosts the composer only while it is a plain shell: once
/// an agent CLI runs there (or anywhere the user launched one), the shortcuts
/// and the input are pointless and hide.
pub(crate) fn composer_pane_id<L: ServerLink>(app: &App<L>) -> Option<String> {
    let snapshot = app.snapshot()?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == snapshot.focused_pane_id)?;
    (pane.agent.is_none() && pane.agent_name.is_none()).then(|| pane.pane_id.clone())
}

/// Row (from the sidebar top) where the agents header sits: the workspaces
/// section takes `ui.sidebar_section_split` of the height, minus the
/// controls row and the divider just above the header. Clamped so both
/// sections keep at least one entry.
pub(crate) fn sidebar_split<L: ServerLink>(app: &App<L>, height: u16) -> u16 {
    let percent = u32::from(app.config().ui.sidebar_section_split_percent.min(100));
    let wanted = ((u32::from(height) * percent + 50) / 100) as u16;
    let min = SIDEBAR_HEADER_ROWS + 2 + SIDEBAR_SPLIT_TAIL;
    let max = height.saturating_sub(3);
    if max < min {
        return height / 2;
    }
    wanted.clamp(min, max)
}

fn composer_geometry<L: ServerLink>(
    app: &App<L>,
    panes: &mut [PaneRegion],
) -> Option<ComposerGeometry> {
    let pane_id = composer_pane_id(app)?;
    let region = panes.iter_mut().find(|region| region.pane_id == pane_id)?;
    let dock_count = app.dock_agents().len().min(COMPOSER_MAX_DOCK) as u16;
    // The launcher is a bordered panel like the right-click menu: title
    // border, one row per agent, the folder row, bottom border.
    let panel_rows = if dock_count > 0 { dock_count + 1 + 2 } else { 0 };
    let rows = panel_rows + COMPOSER_BASE_ROWS;
    // Keep at least a few PTY rows visible or skip the composer entirely.
    if region.content.height < rows + 4 || region.content.width < 12 {
        return None;
    }
    let content = region.content;
    let top = content.y + content.height - rows;
    let inner_width = (0..dock_count)
        .map(|index| {
            let agent = &app.dock_agents()[usize::from(index)];
            dock_button_width(app, usize::from(index), &agent.name)
        })
        .chain(std::iter::once(FOLDER_BUTTON_WIDTH))
        .max()
        .unwrap_or(0)
        .min(content.width.saturating_sub(4));
    let dock_panel = if dock_count > 0 {
        Rect {
            x: content.x + 1,
            y: top,
            width: inner_width + 2,
            height: panel_rows,
        }
    } else {
        Rect::default()
    };
    let mut dock_items = Vec::new();
    for index in 0..dock_count {
        dock_items.push(Rect {
            x: content.x + 2,
            y: top + 1 + index,
            width: inner_width,
            height: 1,
        });
    }
    let folder_button = if dock_count > 0 {
        Rect {
            x: content.x + 2,
            y: top + 1 + dock_count,
            width: inner_width,
            height: 1,
        }
    } else {
        Rect::default()
    };
    let border = Rect { x: content.x, y: top + panel_rows, width: content.width, height: 1 };
    let input = Rect { x: content.x, y: border.y + 1, width: content.width, height: 1 };
    let bottom_border = Rect { x: content.x, y: border.y + 2, width: content.width, height: 1 };
    let spacer = Rect { x: content.x, y: border.y + 3, width: content.width, height: 1 };
    // The cwd reads like Claude Code's status line: left-aligned under the
    // bottom line, one cell in from the edge.
    let cwd_text = cwd_label_text(app, spacer.width);
    let cwd_label = if cwd_text.is_empty() {
        Rect::default()
    } else {
        let width = (cwd_text.chars().count() as u16).min(spacer.width.saturating_sub(1));
        Rect {
            x: spacer.x + 1,
            y: spacer.y,
            width,
            height: 1,
        }
    };
    // The pane's own content ends where the composer begins.
    region.content.height = region.content.height.saturating_sub(rows);
    Some(ComposerGeometry {
        pane_id,
        dock_panel,
        dock_items,
        folder_button,
        border,
        input,
        bottom_border,
        spacer,
        cwd_label,
        rows,
    })
}

/// Button width: icon-only when the agent has a brand icon, text otherwise.
pub(crate) fn dock_button_width<L: ServerLink>(
    app: &App<L>,
    position: usize,
    name: &str,
) -> u16 {
    let _ = app;
    dock_item_label(position, name).chars().count() as u16
}

#[derive(Debug, Clone)]
struct DividerDrag {
    pane_id: String,
    click_pane_id: String,
    x: u16,
    y: u16,
    vertical: bool,
    span: u16,
    moved: bool,
}

#[derive(Debug, Clone)]
struct SidebarDividerDrag {
    x: u16,
    width: u16,
}

/// Dragging the divider between the workspaces and agents sections.
#[derive(Debug, Clone)]
struct SidebarSplitDrag {
    y: u16,
    split: u16,
    height: u16,
    moved: bool,
}

/// A tab grabbed on the tab bar: the press focused it, motion reorders it.
#[derive(Debug, Clone)]
struct TabDrag {
    tab_id: String,
    x: u16,
    moved: bool,
}

#[derive(Debug, Default)]
pub struct MouseController {
    divider: Option<DividerDrag>,
    sidebar_divider: Option<SidebarDividerDrag>,
    split_drag: Option<SidebarSplitDrag>,
    tab_drag: Option<TabDrag>,
    selection_pane: Option<String>,
    tracking_pane: Option<String>,
    /// The chrome element under the pointer (tab, `+`, sidebar row), for
    /// hover feedback. Motion may arrive as `Drag(Right)` on hosts that never
    /// forward the right-button release (Warp), so any motion counts.
    hovered: Option<ChromeTarget>,
    /// Launcher row under the pointer.
    hovered_dock: Option<DockHover>,
    /// Pointer over the sections divider.
    hovered_split: bool,
}

impl MouseController {
    /// The chrome element the pointer last moved over, if any.
    pub fn hovered(&self) -> Option<&ChromeTarget> {
        self.hovered.as_ref()
    }

    pub fn hovered_dock(&self) -> Option<DockHover> {
        self.hovered_dock
    }

    pub fn hovered_split(&self) -> bool {
        self.hovered_split
    }

    /// The tab being dragged along the bar (pointer moved since the press),
    /// for render feedback.
    pub fn dragging_tab(&self) -> Option<&str> {
        self.tab_drag
            .as_ref()
            .filter(|drag| drag.moved)
            .map(|drag| drag.tab_id.as_str())
    }

    pub fn route<L: ServerLink>(
        &mut self,
        app: &App<L>,
        area: Rect,
        event: MouseEvent,
    ) -> MouseAction {
        if !app.config().ui.mouse_capture || area.width == 0 || area.height == 0 {
            return MouseAction::Ignored;
        }
        if let Mode::Modal(modal) = app.mode() {
            self.divider = None;
            self.sidebar_divider = None;
            self.split_drag = None;
            self.tab_drag = None;
            self.selection_pane = None;
            self.tracking_pane = None;
            self.hovered = None;
            self.hovered_dock = None;
            self.hovered_split = false;
            return route_modal(app, area, event, modal);
        }
        let geometry = UiGeometry::calculate(app, area);
        self.hovered = geometry.chrome_at(event.column, event.row).cloned();
        self.hovered_dock = geometry.composer.as_ref().and_then(|composer| {
            composer
                .dock_items
                .iter()
                .position(|item| contains(*item, event.column, event.row))
                .map(DockHover::Item)
                .or_else(|| {
                    contains(composer.folder_button, event.column, event.row)
                        .then_some(DockHover::Folder)
                })
        });
        self.hovered_split = contains(geometry.sidebar_split, event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = geometry.bar_action_at(event.column, event.row) {
                    return action;
                }
                if contains(geometry.sidebar_split, event.column, event.row) {
                    self.split_drag = Some(SidebarSplitDrag {
                        y: event.row,
                        split: sidebar_split(app, geometry.sidebar.height),
                        height: geometry.sidebar.height,
                        moved: false,
                    });
                    return MouseAction::Ignored;
                }
                if let Some(divider) =
                    sidebar_divider_at(app, &geometry, event.column, event.row)
                {
                    self.sidebar_divider = Some(divider);
                    return MouseAction::Ignored;
                }
                if let Some(target) = geometry.chrome_at(event.column, event.row) {
                    return match target {
                        ChromeTarget::NewWorkspace => MouseAction::NewWorkspace,
                        ChromeTarget::Menu => MouseAction::OpenMenu,
                        ChromeTarget::NewTab => MouseAction::NewTab,
                        ChromeTarget::Workspace(id) => MouseAction::FocusWorkspace(id.clone()),
                        ChromeTarget::Tab(id) => {
                            // The press focuses the tab as before; keeping
                            // hold of it and moving along the bar reorders.
                            self.tab_drag = Some(TabDrag {
                                tab_id: id.clone(),
                                x: event.column,
                                moved: false,
                            });
                            MouseAction::FocusTab(id.clone())
                        }
                        ChromeTarget::Agent(id) => MouseAction::FocusAgent(id.clone()),
                    };
                }
                let slack = 1 + u16::from(app.config().ui.pane_gaps);
                if let Some(divider) = divider_at(&geometry, event.column, event.row, slack) {
                    self.divider = Some(divider);
                    return MouseAction::Ignored;
                }
                if let Some((pane, row, col)) = geometry.content_at(event.column, event.row) {
                    if let Some(mouse_mode) = mouse_mode(app, &pane.pane_id)
                        .filter(|mouse_mode| tracks_mouse(mouse_mode))
                    {
                        self.selection_pane = None;
                        self.tracking_pane = Some(pane.pane_id.clone());
                        return MouseAction::MouseTracking {
                            pane_id: pane.pane_id.clone(),
                            data_base64: pane_mouse_base64(mouse_mode, 0, event, pane.content, true),
                            focus: true,
                        };
                    }
                    self.selection_pane = Some(pane.pane_id.clone());
                    return MouseAction::BeginSelection {
                        pane_id: pane.pane_id.clone(),
                        row,
                        col,
                    };
                }
                geometry
                    .pane_at(event.column, event.row)
                    .map(|pane| MouseAction::FocusPane(pane.pane_id.clone()))
                    .unwrap_or(MouseAction::Ignored)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &mut self.split_drag {
                    let target = (i32::from(drag.split) + i32::from(event.row)
                        - i32::from(drag.y))
                    .max(0) as u32;
                    let height = u32::from(drag.height.max(1));
                    let percent = ((target * 100 + height / 2) / height).min(100) as u8;
                    if percent == app.config().ui.sidebar_section_split_percent {
                        return MouseAction::Ignored;
                    }
                    drag.moved = true;
                    return MouseAction::ResizeSidebarSplit { percent };
                }
                if let Some(drag) = &mut self.tab_drag {
                    if event.column != drag.x {
                        drag.moved = true;
                    }
                    let Some((current, target)) =
                        tab_drop_index(&geometry, &drag.tab_id, event.column)
                    else {
                        return MouseAction::Ignored;
                    };
                    if target == current {
                        return MouseAction::Ignored;
                    }
                    return MouseAction::MoveTab {
                        tab_id: drag.tab_id.clone(),
                        insert_index: target,
                    };
                }
                if let Some(divider) = &self.sidebar_divider {
                    let delta = i32::from(event.column) - i32::from(divider.x);
                    let minimum = i32::from(app.config().ui.sidebar_min_width);
                    let maximum = i32::from(app.config().ui.sidebar_max_width);
                    let width = (i32::from(divider.width) + delta).clamp(minimum, maximum) as u16;
                    let current = app
                        .config()
                        .ui
                        .sidebar_width
                        .clamp(
                            app.config().ui.sidebar_min_width,
                            app.config().ui.sidebar_max_width,
                        );
                    return if width == current {
                        MouseAction::Ignored
                    } else {
                        MouseAction::ResizeSidebar { width }
                    };
                }
                if let Some(divider) = &mut self.divider {
                    if event.column != divider.x || event.row != divider.y {
                        divider.moved = true;
                    }
                    let delta = if divider.vertical {
                        i32::from(event.column) - i32::from(divider.x)
                    } else {
                        i32::from(event.row) - i32::from(divider.y)
                    };
                    if delta == 0 {
                        return MouseAction::Ignored;
                    }
                    // Advance the drag origin so each event resizes by the
                    // cells moved since the last one, not the cumulative total.
                    if divider.vertical {
                        divider.x = event.column;
                    } else {
                        divider.y = event.row;
                    }
                    let amount = (f64::from(delta.unsigned_abs())
                        / f64::from(divider.span.max(1)))
                    .clamp(0.01, 0.5);
                    let direction = match (divider.vertical, delta.is_positive()) {
                        (true, true) => "right",
                        (true, false) => "left",
                        (false, true) => "down",
                        (false, false) => "up",
                    };
                    return MouseAction::Resize {
                        pane_id: divider.pane_id.clone(),
                        direction,
                        amount,
                    };
                }
                if let Some(pane_id) = &self.tracking_pane {
                    let Some(pane) = geometry.panes.iter().find(|pane| &pane.pane_id == pane_id)
                    else {
                        return MouseAction::Ignored;
                    };
                    let Some(mouse_mode) = mouse_mode(app, pane_id) else {
                        return MouseAction::Ignored;
                    };
                    if matches!(
                        mouse_mode.tracking,
                        PaneMouseTracking::ButtonMotion | PaneMouseTracking::AnyMotion
                    ) {
                        return MouseAction::MouseTracking {
                            pane_id: pane_id.clone(),
                            data_base64: pane_mouse_base64(
                                mouse_mode,
                                32,
                                event,
                                pane.content,
                                true,
                            ),
                            focus: false,
                        };
                    }
                    return MouseAction::Ignored;
                }
                if let Some(pane_id) = &self.selection_pane {
                    if let Some(pane) = geometry.panes.iter().find(|pane| &pane.pane_id == pane_id) {
                        let row = event
                            .row
                            .saturating_sub(pane.content.y)
                            .min(pane.content.height.saturating_sub(1));
                        let col = event
                            .column
                            .saturating_sub(pane.content.x)
                            .min(pane.content.width.saturating_sub(1));
                        return MouseAction::UpdateSelection {
                            pane_id: pane_id.clone(),
                            row,
                            col,
                        };
                    }
                }
                MouseAction::Ignored
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(drag) = self.split_drag.take() {
                    return if drag.moved {
                        MouseAction::SaveSidebarSplit
                    } else {
                        MouseAction::Ignored
                    };
                }
                if self.tab_drag.take().is_some() {
                    // Every reorder already went out during the drag and the
                    // press focused the tab: nothing left to do on release.
                    return MouseAction::Ignored;
                }
                let divider = self.divider.take();
                self.sidebar_divider = None;
                if let Some(pane_id) = self.tracking_pane.take() {
                    let Some(pane) = geometry.panes.iter().find(|pane| pane.pane_id == pane_id)
                    else {
                        return MouseAction::Ignored;
                    };
                    let Some(mouse_mode) = mouse_mode(app, &pane_id)
                        .filter(|mouse_mode| tracks_mouse(mouse_mode))
                    else {
                        return MouseAction::Ignored;
                    };
                    MouseAction::MouseTracking {
                        pane_id,
                        data_base64: pane_mouse_base64(mouse_mode, 0, event, pane.content, false),
                        focus: false,
                    }
                } else if self.selection_pane.take().is_some() {
                    MouseAction::FinishSelection
                } else if let Some(divider) = divider.filter(|divider| !divider.moved) {
                    MouseAction::FocusPane(divider.click_pane_id)
                } else {
                    MouseAction::Ignored
                }
            }
            // The release also opens the menu: some host terminals reserve
            // the right-button press for themselves and only forward the
            // release. When both arrive, the press opens the menu and the
            // release lands in modal routing, which ignores it.
            MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Up(MouseButton::Right) => {
                let Some(target) = geometry.context_target_at(event.column, event.row) else {
                    return MouseAction::Ignored;
                };
                let ContextTarget::Pane(_) = &target else {
                    return MouseAction::ContextMenu {
                        target,
                        x: event.column,
                        y: event.row,
                    };
                };
                let Some(pane) = geometry.pane_at(event.column, event.row) else {
                    return MouseAction::Ignored;
                };
                let passthrough = modifier_matches(
                    &app.config().ui.right_click_passthrough_modifier,
                    event.modifiers,
                ) && mouse_mode(app, &pane.pane_id).is_some_and(|mouse_mode| {
                    mouse_mode.alternate_screen
                });
                if passthrough {
                    let mouse_mode = mouse_mode(app, &pane.pane_id)
                        .expect("alternate-screen mouse mode is present");
                    MouseAction::Passthrough {
                        pane_id: pane.pane_id.clone(),
                        data_base64: pane_mouse_base64(
                            mouse_mode,
                            2,
                            event,
                            pane.content,
                            matches!(event.kind, MouseEventKind::Down(_)),
                        ),
                    }
                } else {
                    MouseAction::ContextMenu {
                        target,
                        x: event.column,
                        y: event.row,
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => geometry
                .pane_at(event.column, event.row)
                .map(|pane| MouseAction::Paste(pane.pane_id.clone()))
                .unwrap_or(MouseAction::Ignored),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let Some(pane) = geometry.pane_at(event.column, event.row) else {
                    return MouseAction::Ignored;
                };
                if let Some(mouse_mode) = mouse_mode(app, &pane.pane_id)
                    .filter(|mouse_mode| tracks_mouse(mouse_mode))
                {
                    let button = if event.kind == MouseEventKind::ScrollUp {
                        64
                    } else {
                        65
                    };
                    return MouseAction::Passthrough {
                        pane_id: pane.pane_id.clone(),
                        data_base64: pane_mouse_base64(mouse_mode, button, event, pane.content, true),
                    };
                }
                let direction = if event.kind == MouseEventKind::ScrollUp {
                    1
                } else {
                    -1
                };
                MouseAction::Scroll {
                    pane_id: pane.pane_id.clone(),
                    lines: i64::from(app.config().ui.mouse_scroll_lines) * direction,
                    alternate_screen: app
                        .mirror(&pane.pane_id)
                        .and_then(|mirror| mirror.mouse_mode())
                        .is_some_and(|mouse_mode| mouse_mode.alternate_screen),
                }
            }
            MouseEventKind::Moved => {
                let Some((pane, _, _)) = geometry.content_at(event.column, event.row) else {
                    return MouseAction::Ignored;
                };
                let Some(mouse_mode) = mouse_mode(app, &pane.pane_id)
                    .filter(|mouse_mode| mouse_mode.tracking == PaneMouseTracking::AnyMotion)
                else {
                    return MouseAction::Ignored;
                };
                MouseAction::Passthrough {
                    pane_id: pane.pane_id.clone(),
                    data_base64: pane_mouse_base64(mouse_mode, 67, event, pane.content, true),
                }
            }
            _ => MouseAction::Ignored,
        }
    }
}

pub(crate) fn modal_rect<L: ServerLink>(app: &App<L>, area: Rect, modal: &Modal) -> Rect {
    match modal {
        Modal::Help { .. } => sized_rect(area, 66, area.height.saturating_mul(3) / 4),
        Modal::WorkspacePicker { .. } => {
            let count = app
                .snapshot()
                .map_or(0, |snapshot| snapshot.workspaces.len()) as u16;
            sized_rect(area, 44, count.saturating_add(3))
        }
        Modal::Confirm { .. } => sized_rect(area, 46, 4),
        Modal::UpdatePrompt { .. } => sized_rect(area, 52, 4),
        Modal::Prompt { .. } => sized_rect(area, 48, 5),
        Modal::Menu { .. } => {
            let width = 20.min(area.width).max(1);
            let height = (app.menu_actions().len() as u16 + 2)
                .min(area.height)
                .max(1);
            let anchor = UiGeometry::calculate(app, area)
                .chrome
                .iter()
                .find(|region| matches!(region.target, ChromeTarget::Menu))
                .map(|region| region.rect);
            anchor.map_or_else(
                || sized_rect(area, width, height),
                |anchor| anchored_popup_rect(area, width, height, anchor),
            )
        }
        Modal::Settings => {
            let rows = app.settings().rows().len() as u16;
            sized_rect(area, 56, rows.saturating_add(7))
        }
        Modal::ContextMenu { target, x, y, .. } => {
            let width = 22.min(area.width).max(1);
            let height = (ContextMenuAction::items(target).len() as u16 + 2)
                .min(area.height)
                .max(1);
            Rect {
                x: (*x).min(area.x + area.width.saturating_sub(width)),
                y: (*y).min(area.y + area.height.saturating_sub(height)),
                width,
                height,
            }
        }
        Modal::Onboarding => sized_rect(area, 52, 8),
    }
}

fn route_modal<L: ServerLink>(
    app: &App<L>,
    area: Rect,
    event: MouseEvent,
    modal: &Modal,
) -> MouseAction {
    let rect = modal_rect(app, area, modal);
    if !matches!(modal, Modal::Onboarding)
        && matches!(
            event.kind,
            MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Up(MouseButton::Right)
        )
        && !contains(rect, event.column, event.row)
    {
        let geometry = UiGeometry::calculate(app, area);
        return geometry
            .context_target_at(event.column, event.row)
            .map(|target| MouseAction::CloseModalAndContextMenu {
                target,
                x: event.column,
                y: event.row,
            })
            .unwrap_or(MouseAction::CloseModal);
    }
    match modal {
        Modal::ContextMenu { target, selected, .. } => match event.kind {
            // Drag counts as hover: hosts that never forward the right-button
            // release (Warp) leave a stuck bit, so pointer motion over the
            // open menu arrives as Drag(Right) instead of Moved.
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                match popup_item_at(
                    rect,
                    event.column,
                    event.row,
                    ContextMenuAction::items(target).len(),
                ) {
                    Some(index) if index != *selected => MouseAction::ContextMenuItem {
                        index,
                        activate: false,
                    },
                    _ => MouseAction::Ignored,
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = popup_item_at(
                    rect,
                    event.column,
                    event.row,
                    ContextMenuAction::items(target).len(),
                ) {
                    MouseAction::ContextMenuItem {
                        index,
                        activate: true,
                    }
                } else if contains(rect, event.column, event.row) {
                    MouseAction::Ignored
                } else {
                    MouseAction::CloseModal
                }
            }
            MouseEventKind::Down(_) if !contains(rect, event.column, event.row) => {
                MouseAction::CloseModal
            }
            _ => MouseAction::Ignored,
        },
        Modal::Menu { selected } => {
            let item_count = app.menu_actions().len();
            match event.kind {
                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                    match popup_item_at(rect, event.column, event.row, item_count) {
                        Some(index) if index != *selected => MouseAction::MenuItem {
                            index,
                            activate: false,
                        },
                        _ => MouseAction::Ignored,
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        popup_item_at(rect, event.column, event.row, item_count)
                    {
                        MouseAction::MenuItem {
                            index,
                            activate: true,
                        }
                    } else if contains(rect, event.column, event.row) {
                        MouseAction::Ignored
                    } else {
                        MouseAction::CloseModal
                    }
                }
                MouseEventKind::Down(_) if !contains(rect, event.column, event.row) => {
                    MouseAction::CloseModal
                }
                _ => MouseAction::Ignored,
            }
        }
        Modal::Settings => match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !contains(rect, event.column, event.row) {
                    return MouseAction::CloseModal;
                }
                if let Some(index) = settings_section_at(rect, event.column, event.row) {
                    return MouseAction::SettingsSection(index);
                }
                let Some(index) = settings_row_at(app, rect, event.column, event.row) else {
                    return MouseAction::Ignored;
                };
                let activate = index == app.settings().selected_index()
                    || matches!(app.settings().rows().get(index), Some(SettingsRow::Theme(_)));
                MouseAction::SettingsRow { index, activate }
            }
            MouseEventKind::Down(_) if !contains(rect, event.column, event.row) => {
                MouseAction::CloseModal
            }
            _ => MouseAction::Ignored,
        },
        Modal::Onboarding => MouseAction::Ignored,
        _ => match event.kind {
            MouseEventKind::Down(_) if !contains(rect, event.column, event.row) => {
                MouseAction::CloseModal
            }
            _ => MouseAction::Ignored,
        },
    }
}

fn popup_item_at(rect: Rect, x: u16, y: u16, item_count: usize) -> Option<usize> {
    let inner_right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let inner_bottom = rect.y.saturating_add(rect.height).saturating_sub(1);
    if x <= rect.x || x >= inner_right || y <= rect.y || y >= inner_bottom {
        return None;
    }
    let index = y.saturating_sub(rect.y).saturating_sub(1) as usize;
    (index < item_count).then_some(index)
}

fn settings_section_at(rect: Rect, x: u16, y: u16) -> Option<usize> {
    if y
        != rect
            .y
            .saturating_add(1)
            .saturating_add(SETTINGS_SECTION_INNER_LINE as u16)
    {
        return None;
    }
    let mut start = rect.x.saturating_add(2);
    for (index, name) in SECTION_NAMES.iter().enumerate() {
        let width = name.chars().count() as u16 + 2;
        if x >= start && x < start.saturating_add(width) {
            return Some(index);
        }
        start = start.saturating_add(width).saturating_add(1);
    }
    None
}

fn settings_row_at<L: ServerLink>(app: &App<L>, rect: Rect, x: u16, y: u16) -> Option<usize> {
    let inner_right = rect.x.saturating_add(rect.width).saturating_sub(1);
    if x <= rect.x || x >= inner_right {
        return None;
    }
    let first_row = rect
        .y
        .saturating_add(1)
        .saturating_add(SETTINGS_ROWS_INNER_LINE as u16);
    let index = y.checked_sub(first_row)? as usize;
    (index < app.settings().rows().len()).then_some(index)
}

fn sized_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.max(1).min(area.width);
    let height = height.max(1).min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn anchored_popup_rect(area: Rect, width: u16, height: u16, anchor: Rect) -> Rect {
    let max_x = area
        .x
        .saturating_add(area.width)
        .saturating_sub(width);
    let x = anchor.x.clamp(area.x, max_x);
    let below = anchor.y.saturating_add(anchor.height);
    let area_bottom = area.y.saturating_add(area.height);
    let y = if below.saturating_add(height) <= area_bottom {
        below
    } else {
        anchor.y.saturating_sub(height).max(area.y)
    };
    Rect {
        x,
        y,
        width,
        height,
    }
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

fn pane_regions<L: ServerLink>(app: &App<L>, main: Rect, mobile: bool) -> Vec<PaneRegion> {
    let Some(snapshot) = app.snapshot() else {
        return Vec::new();
    };
    let Some(layout) = snapshot
        .layouts
        .iter()
        .find(|layout| layout.tab_id == snapshot.focused_tab_id)
    else {
        return Vec::new();
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
    layout
        .panes
        .iter()
        .filter(|pane| zoomed.as_ref().is_none_or(|zoomed| zoomed == &pane.pane_id))
        .enumerate()
        .map(|(pane_index, pane)| {
            let mut outer = if zoomed.is_some() {
                main
            } else if mobile {
                let index = layout
                    .panes
                    .iter()
                    .position(|candidate| candidate.pane_id == pane.pane_id)
                    .unwrap_or_default() as u32;
                let count = layout.panes.len().max(1) as u32;
                let start = index * main.height as u32 / count;
                let end = (index + 1) * main.height as u32 / count;
                Rect {
                    x: main.x,
                    y: main.y + start as u16,
                    width: main.width,
                    height: end.saturating_sub(start).max(1) as u16,
                }
            } else {
                map_pane_rect(layout, pane.rect, main)
            };
            if app.config().ui.pane_gaps && multi_pane {
                let leading_x = zoomed.is_some() || mobile || pane.rect.x == layout.area.x;
                let leading_y = zoomed.is_some()
                    || (mobile && pane_index == 0)
                    || (!mobile && pane.rect.y == layout.area.y);
                apply_one_cell_gap(&mut outer, leading_x, leading_y);
            }
            let content = if app.config().ui.pane_borders && multi_pane {
                inset(outer, 1)
            } else {
                outer
            };
            PaneRegion {
                pane_id: pane.pane_id.clone(),
                outer,
                content,
                focused: pane.focused,
            }
        })
        .collect()
}

fn map_pane_rect(layout: &PaneLayoutSnapshot, pane: PaneRect, target: Rect) -> Rect {
    if layout.area.width == 0 || layout.area.height == 0 {
        return target;
    }
    let relative_x = pane.x.saturating_sub(layout.area.x) as u32;
    let relative_y = pane.y.saturating_sub(layout.area.y) as u32;
    let start_x = target.x as u32 + relative_x * target.width as u32 / layout.area.width as u32;
    let start_y = target.y as u32 + relative_y * target.height as u32 / layout.area.height as u32;
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

fn chrome_regions<L: ServerLink>(app: &App<L>, geometry: &UiGeometry) -> Vec<ChromeRegion> {
    let Some(snapshot) = app.snapshot() else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    if geometry.sidebar.width > 0 {
        match app.sidebar_state() {
            SidebarState::Compact => {
                for (index, workspace) in snapshot.workspaces.iter().enumerate() {
                    regions.push(ChromeRegion {
                        rect: Rect {
                            x: geometry.sidebar.x,
                            y: geometry.sidebar.y + SIDEBAR_HEADER_ROWS + index as u16,
                            width: geometry.sidebar.width,
                            height: 1,
                        },
                        target: ChromeTarget::Workspace(workspace.workspace_id.clone()),
                    });
                }
            }
            SidebarState::Expanded => {
                let split = sidebar_split(app, geometry.sidebar.height);
                let spaces_end = geometry.sidebar.y + split.saturating_sub(SIDEBAR_SPLIT_TAIL);
                let mut row = geometry.sidebar.y + SIDEBAR_HEADER_ROWS;
                for workspace in &snapshot.workspaces {
                    let height = app.config().ui.sidebar.spaces.rows.len().max(1) as u16;
                    if row.saturating_add(height) <= spaces_end {
                        regions.push(ChromeRegion {
                            rect: Rect {
                                x: geometry.sidebar.x,
                                y: row,
                                width: geometry.sidebar.width,
                                height,
                            },
                            target: ChromeTarget::Workspace(workspace.workspace_id.clone()),
                        });
                    }
                    row = row
                        .saturating_add(height)
                        .saturating_add(u16::from(app.config().ui.sidebar.spaces.row_gap));
                }
                if split >= SIDEBAR_SPLIT_TAIL {
                    let controls_y = geometry.sidebar.y + split - SIDEBAR_SPLIT_TAIL;
                    let half = geometry.sidebar.width / 2;
                    regions.push(ChromeRegion {
                        rect: Rect {
                            x: geometry.sidebar.x,
                            y: controls_y,
                            width: half,
                            height: 1,
                        },
                        target: ChromeTarget::NewWorkspace,
                    });
                    regions.push(ChromeRegion {
                        rect: Rect {
                            x: geometry.sidebar.x + half,
                            y: controls_y,
                            width: geometry.sidebar.width.saturating_sub(half),
                            height: 1,
                        },
                        target: ChromeTarget::Menu,
                    });
                }
                row = geometry.sidebar.y + split + 1;
                let agents_end = geometry.sidebar.y + geometry.sidebar.height;
                let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
                match app.config().ui.agent_panel_sort {
                    AgentPanelSort::Priority => agents.sort_by(|left, right| {
                        right
                            .agent_status
                            .priority()
                            .cmp(&left.agent_status.priority())
                    }),
                    AgentPanelSort::Spaces => agents.sort_by(|left, right| {
                        left.workspace_id
                            .cmp(&right.workspace_id)
                            .then_with(|| left.agent.cmp(&right.agent))
                    }),
                }
                for agent in agents {
                    let height = app
                        .config()
                        .ui
                        .sidebar
                        .agents
                        .rows_by_agent
                        .get(&agent.agent)
                        .unwrap_or(&app.config().ui.sidebar.agents.rows)
                        .len()
                        .max(1) as u16;
                    if row.saturating_add(height) <= agents_end {
                        regions.push(ChromeRegion {
                            rect: Rect {
                                x: geometry.sidebar.x,
                                y: row,
                                width: geometry.sidebar.width,
                                height,
                            },
                            target: ChromeTarget::Agent(agent.pane_id.clone()),
                        });
                    }
                    row = row
                        .saturating_add(height)
                        .saturating_add(u16::from(app.config().ui.sidebar.agents.row_gap));
                }
            }
            SidebarState::Hidden => {}
        }
    }
    if let Some(tab_bar) = geometry.tab_bar {
        let mut x = tab_bar.x;
        for tab in snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == snapshot.focused_workspace_id)
        {
            let width = (tab.label.chars().count() as u16 + 2)
                .max(10)
                .min(tab_bar.x.saturating_add(tab_bar.width).saturating_sub(x));
            if width == 0 {
                break;
            }
            regions.push(ChromeRegion {
                rect: Rect {
                    x,
                    y: tab_bar.y,
                    width,
                    height: tab_bar.height,
                },
                target: ChromeTarget::Tab(tab.tab_id.clone()),
            });
            x = x.saturating_add(width);
        }
        if tab_bar.x.saturating_add(tab_bar.width).saturating_sub(x) >= 3 {
            regions.push(ChromeRegion {
                rect: Rect {
                    x,
                    y: tab_bar.y,
                    width: 3,
                    height: tab_bar.height,
                },
                target: ChromeTarget::NewTab,
            });
        }
    }
    regions
}

/// Where a dragged tab lands for a pointer at `column`: `(current, target)`
/// positions among the tab-bar tabs. The target is the number of OTHER tabs
/// whose centre lies at or left of the pointer, so a tab swaps with its
/// neighbour exactly when the pointer crosses that neighbour's midpoint —
/// and after the re-layout the neighbour's centre has moved away from the
/// pointer, so the swap never flickers back. `None` when the tab is not on
/// the bar (overflow, bar hidden).
fn tab_drop_index(geometry: &UiGeometry, tab_id: &str, column: u16) -> Option<(usize, usize)> {
    let tabs: Vec<(&str, &Rect)> = geometry
        .chrome
        .iter()
        .filter_map(|region| match &region.target {
            ChromeTarget::Tab(id) => Some((id.as_str(), &region.rect)),
            _ => None,
        })
        .collect();
    let current = tabs.iter().position(|(id, _)| *id == tab_id)?;
    let target = tabs
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != current)
        .filter(|(_, (_, rect))| rect.x.saturating_add(rect.width / 2) <= column)
        .count();
    Some((current, target))
}

fn divider_at(geometry: &UiGeometry, x: u16, y: u16, slack: u16) -> Option<DividerDrag> {
    // The grab zone spans the pane's trailing border plus the gap column and
    // the neighbor's leading border (`slack`), so any part of the visible
    // separator starts a drag instead of demanding the exact edge cell.
    for pane in &geometry.panes {
        let right = pane.outer.x + pane.outer.width.saturating_sub(1);
        if x >= right
            && x <= right.saturating_add(slack)
            && y >= pane.outer.y
            && y < pane.outer.y.saturating_add(pane.outer.height)
            && right < geometry.main.x.saturating_add(geometry.main.width).saturating_sub(1)
        {
            return Some(DividerDrag {
                pane_id: pane.pane_id.clone(),
                click_pane_id: geometry
                    .pane_at(x, y)
                    .map(|pane| pane.pane_id.clone())
                    .unwrap_or_else(|| pane.pane_id.clone()),
                x,
                y,
                vertical: true,
                span: geometry.main.width,
                moved: false,
            });
        }
        let bottom = pane.outer.y + pane.outer.height.saturating_sub(1);
        if y >= bottom
            && y <= bottom.saturating_add(slack)
            && x >= pane.outer.x
            && x < pane.outer.x.saturating_add(pane.outer.width)
            && bottom < geometry.main.y.saturating_add(geometry.main.height).saturating_sub(1)
        {
            return Some(DividerDrag {
                pane_id: pane.pane_id.clone(),
                click_pane_id: geometry
                    .pane_at(x, y)
                    .map(|pane| pane.pane_id.clone())
                    .unwrap_or_else(|| pane.pane_id.clone()),
                x,
                y,
                vertical: false,
                span: geometry.main.height,
                moved: false,
            });
        }
    }
    None
}

fn sidebar_divider_at<L: ServerLink>(
    app: &App<L>,
    geometry: &UiGeometry,
    x: u16,
    y: u16,
) -> Option<SidebarDividerDrag> {
    if app.sidebar_state() != SidebarState::Expanded || geometry.sidebar.width == 0 {
        return None;
    }
    let divider_x = geometry
        .sidebar
        .x
        .saturating_add(geometry.sidebar.width.saturating_sub(1));
    (x == divider_x
        && y >= geometry.sidebar.y
        && y < geometry.sidebar.y.saturating_add(geometry.sidebar.height))
    .then_some(SidebarDividerDrag {
        x: divider_x,
        width: geometry.sidebar.width,
    })
}

fn modifier_matches(configured: &str, actual: KeyModifiers) -> bool {
    let configured = configured.trim().to_ascii_lowercase();
    if configured.is_empty() || configured == "off" {
        return false;
    }
    let expected = configured.split('+').fold(KeyModifiers::empty(), |mut result, part| {
        match part.trim() {
            "ctrl" | "control" => result |= KeyModifiers::CONTROL,
            "alt" | "option" => result |= KeyModifiers::ALT,
            "cmd" | "super" | "meta" | "win" => result |= KeyModifiers::SUPER,
            "hyper" => {
                result |= KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER
            }
            _ => {}
        }
        result
    });
    !expected.is_empty() && actual.contains(expected)
}

fn mouse_mode<'a, L: ServerLink>(app: &'a App<L>, pane_id: &str) -> Option<&'a PaneMouseMode> {
    app.mirror(pane_id).and_then(|mirror| mirror.mouse_mode())
}

fn tracks_mouse(mouse_mode: &PaneMouseMode) -> bool {
    mouse_mode.tracking != PaneMouseTracking::None
}

fn pane_mouse_base64(
    mouse_mode: &PaneMouseMode,
    button: u8,
    event: MouseEvent,
    content: Rect,
    pressed: bool,
) -> String {
    match mouse_mode.encoding {
        PaneMouseEncoding::Sgr => sgr_mouse_base64(button, event, content, pressed),
        PaneMouseEncoding::Default | PaneMouseEncoding::Utf8 => {
            legacy_mouse_base64(button, event, content, pressed)
        }
    }
}

fn sgr_mouse_base64(button: u8, event: MouseEvent, content: Rect, pressed: bool) -> String {
    let (x, y) = content_coordinates(event, content);
    let suffix = if pressed { 'M' } else { 'm' };
    base64_encode(
        format!(
            "\u{1b}[<{};{x};{y}{suffix}",
            button.saturating_add(mouse_modifiers(event))
        )
        .as_bytes(),
    )
}

fn legacy_mouse_base64(button: u8, event: MouseEvent, content: Rect, pressed: bool) -> String {
    let (x, y) = content_coordinates(event, content);
    let button = if pressed { button } else { 3 };
    let bytes = [
        0x1b,
        b'[',
        b'M',
        32u8.saturating_add(button.saturating_add(mouse_modifiers(event))),
        32u8.saturating_add(x.min(223) as u8),
        32u8.saturating_add(y.min(223) as u8),
    ];
    base64_encode(&bytes)
}

fn mouse_modifiers(event: MouseEvent) -> u8 {
    u8::from(event.modifiers.contains(KeyModifiers::SHIFT)) * 4
        + u8::from(event.modifiers.contains(KeyModifiers::ALT)) * 8
        + u8::from(event.modifiers.contains(KeyModifiers::CONTROL)) * 16
}

fn content_coordinates(event: MouseEvent, content: Rect) -> (u16, u16) {
    let x = event
        .column
        .saturating_sub(content.x)
        .min(content.width.saturating_sub(1))
        .saturating_add(1);
    let y = event
        .row
        .saturating_sub(content.y)
        .min(content.height.saturating_sub(1))
        .saturating_add(1);
    (x, y)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        result.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        result.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        result.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    result
}

/// One dock row as rendered: ` N glyph name ` (` N name ` without an icon).
pub(crate) fn dock_item_label(position: usize, name: &str) -> String {
    match crate::dock::dock_glyph(name) {
        Some((glyph, _)) => format!(" {} {} {} ", position + 1, glyph, name),
        None => format!(" {} {} ", position + 1, name),
    }
}

/// The clickable cwd label: the folder new dock panes will start in (or the
/// focused pane's cwd), truncated from the left to fit the status row.
pub(crate) fn cwd_label_text<L: ServerLink>(app: &App<L>, available: u16) -> String {
    let Some(cwd) = app.dock_cwd_label() else {
        return String::new();
    };
    let budget = usize::from(available.saturating_sub(4)).min(60);
    if budget < 4 {
        return String::new();
    }
    let mut label = cwd;
    if label.chars().count() > budget {
        let keep: String = label
            .chars()
            .rev()
            .take(budget.saturating_sub(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        label = format!("…{keep}");
    }
    format!(" {label} ")
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn inset(rect: Rect, amount: u16) -> Rect {
    Rect {
        x: rect.x.saturating_add(amount),
        y: rect.y.saturating_add(amount),
        width: rect.width.saturating_sub(amount.saturating_mul(2)),
        height: rect.height.saturating_sub(amount.saturating_mul(2)),
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
