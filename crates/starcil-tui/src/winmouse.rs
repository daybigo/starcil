//! Host-tolerant translation of Windows console `MOUSE_EVENT_RECORD`s into
//! crossterm mouse events.
//!
//! Why this exists: ConPTY (conhost / OpenConsole) keeps a cumulative button
//! mask for the SGR mouse reports it receives from the host terminal. Some hosts
//! reserve the right button for their own UI and forward the PRESS but never the
//! RELEASE (Warp on Windows does exactly this). From then on every record that
//! conhost emits carries a stale "right button held" bit:
//!
//! * a transition-based parser (crossterm's) sees no change on the next right
//!   press and emits nothing — the context menu "works once";
//! * a left drag arrives as `MOUSE_MOVED` with both bits set and a parser that
//!   gives the right button priority reports `Drag(Right)` — divider resize and
//!   drag selection go dead.
//!
//! This translator derives events from the record itself plus a small amount of
//! state that cannot be confused by a stuck bit: a repeated press of a button
//! that never released is a new click, and drags are attributed to the button
//! that was pressed most recently.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// `dwButtonState` bits (wincon.h).
pub const FROM_LEFT_1ST_BUTTON_PRESSED: u32 = 0x0001;
pub const RIGHTMOST_BUTTON_PRESSED: u32 = 0x0002;
pub const FROM_LEFT_2ND_BUTTON_PRESSED: u32 = 0x0004;
const BUTTON_MASK: u32 =
    FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED | FROM_LEFT_2ND_BUTTON_PRESSED;

/// `dwEventFlags` values (wincon.h). `0` is a plain press or release.
pub const MOUSE_MOVED: u32 = 0x0001;
pub const DOUBLE_CLICK: u32 = 0x0002;
pub const MOUSE_WHEELED: u32 = 0x0004;
pub const MOUSE_HWHEELED: u32 = 0x0008;

/// `dwControlKeyState` bits (wincon.h).
const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_ALT_PRESSED: u32 = 0x0002;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const SHIFT_PRESSED: u32 = 0x0010;

/// The fields of a `MOUSE_EVENT_RECORD`, free of any Win32 types so the
/// translator is testable everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMouseRecord {
    pub x: i16,
    /// Absolute screen-buffer row; callers pass the window top so it becomes
    /// viewport-relative (the alternate screen has top 0, the main buffer may not).
    pub y: i16,
    pub button_state: u32,
    pub control_key_state: u32,
    pub event_flags: u32,
}

#[derive(Debug, Default)]
pub struct MouseRecordTranslator {
    held: u32,
    last_pressed: Option<MouseButton>,
}

impl MouseRecordTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Buttons conhost currently reports as held (may include stuck bits).
    pub fn held_mask(&self) -> u32 {
        self.held
    }

    /// Keep the tracked mask honest when a mouse event was parsed elsewhere
    /// (crossterm consumed a record we did not get to see).
    pub fn observe_foreign(&mut self, event: &MouseEvent) {
        match event.kind {
            MouseEventKind::Down(button) => {
                self.held |= button_bit(button);
                self.last_pressed = Some(button);
            }
            MouseEventKind::Up(button) => self.held &= !button_bit(button),
            _ => {}
        }
    }

    pub fn translate(&mut self, record: RawMouseRecord, window_top: i16) -> Vec<MouseEvent> {
        let modifiers = modifiers_from(record.control_key_state);
        let column = record.x.max(0) as u16;
        let row = record.y.saturating_sub(window_top).max(0) as u16;
        let make = |kind: MouseEventKind| MouseEvent {
            kind,
            column,
            row,
            modifiers,
        };
        let buttons = record.button_state & BUTTON_MASK;

        if record.event_flags & MOUSE_WHEELED != 0 {
            // The wheel delta lives in the high word; forward (away from the
            // user) is positive.
            let delta = (record.button_state >> 16) as i16;
            return match delta.signum() {
                1 => vec![make(MouseEventKind::ScrollUp)],
                -1 => vec![make(MouseEventKind::ScrollDown)],
                _ => Vec::new(),
            };
        }
        if record.event_flags & MOUSE_HWHEELED != 0 {
            let delta = (record.button_state >> 16) as i16;
            return match delta.signum() {
                1 => vec![make(MouseEventKind::ScrollRight)],
                -1 => vec![make(MouseEventKind::ScrollLeft)],
                _ => Vec::new(),
            };
        }
        if record.event_flags & MOUSE_MOVED != 0 {
            // Motion: keep the mask in sync (hosts may drop a release and the
            // next motion shows the real state) but never synthesize clicks
            // from motion records.
            self.held = buttons;
            let Some(button) = self.drag_button(buttons) else {
                return vec![make(MouseEventKind::Moved)];
            };
            return vec![make(MouseEventKind::Drag(button))];
        }

        // Plain press/release or DOUBLE_CLICK.
        let previous = self.held;
        let released = previous & !buttons;
        let pressed = buttons & !previous;
        self.held = buttons;
        let mut events = Vec::new();
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            if released & button_bit(button) != 0 {
                events.push(make(MouseEventKind::Up(button)));
            }
        }
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            if pressed & button_bit(button) != 0 {
                self.last_pressed = Some(button);
                events.push(make(MouseEventKind::Down(button)));
            }
        }
        if events.is_empty() && buttons != 0 {
            // No transition yet a press record arrived: the host lost a release,
            // so conhost still reports the button as held. Treat it as the click
            // it is. With several bits set the right button is the stuck one in
            // practice (hosts reserve it for their own menus).
            let button = if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
                MouseButton::Right
            } else if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
                MouseButton::Left
            } else {
                MouseButton::Middle
            };
            self.last_pressed = Some(button);
            events.push(make(MouseEventKind::Down(button)));
        }
        events
    }

    fn drag_button(&self, buttons: u32) -> Option<MouseButton> {
        if buttons == 0 {
            return None;
        }
        if let Some(last) = self.last_pressed {
            if buttons & button_bit(last) != 0 {
                return Some(last);
            }
        }
        // Unknown history: prefer the left button — a stuck bit is almost
        // always the right one, and left is what people drag with.
        if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
            Some(MouseButton::Left)
        } else if buttons & FROM_LEFT_2ND_BUTTON_PRESSED != 0 {
            Some(MouseButton::Middle)
        } else {
            Some(MouseButton::Right)
        }
    }
}

fn button_bit(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => FROM_LEFT_1ST_BUTTON_PRESSED,
        MouseButton::Right => RIGHTMOST_BUTTON_PRESSED,
        MouseButton::Middle => FROM_LEFT_2ND_BUTTON_PRESSED,
    }
}

fn modifiers_from(state: u32) -> KeyModifiers {
    let mut modifiers = KeyModifiers::empty();
    if state & SHIFT_PRESSED != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    modifiers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(x: i16, y: i16, buttons: u32, flags: u32) -> RawMouseRecord {
        RawMouseRecord {
            x,
            y,
            button_state: buttons,
            control_key_state: 0,
            event_flags: flags,
        }
    }

    fn kinds(events: &[MouseEvent]) -> Vec<MouseEventKind> {
        events.iter().map(|event| event.kind).collect()
    }

    #[test]
    fn paired_press_release_behaves_like_a_normal_parser() {
        let mut t = MouseRecordTranslator::new();
        assert_eq!(
            kinds(&t.translate(record(5, 3, FROM_LEFT_1ST_BUTTON_PRESSED, 0), 0)),
            vec![MouseEventKind::Down(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(6, 3, FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Drag(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(6, 3, 0, 0), 0)),
            vec![MouseEventKind::Up(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(7, 3, 0, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Moved]
        );
        assert_eq!(t.held_mask(), 0);
    }

    #[test]
    fn right_press_without_release_still_clicks_every_time() {
        // Warp: the right release never reaches conhost, so every later press
        // record arrives with the right bit already set.
        let mut t = MouseRecordTranslator::new();
        let first = t.translate(record(40, 10, RIGHTMOST_BUTTON_PRESSED, 0), 0);
        assert_eq!(kinds(&first), vec![MouseEventKind::Down(MouseButton::Right)]);
        for (x, y) in [(42, 11), (44, 12), (10, 2)] {
            let again = t.translate(record(x, y, RIGHTMOST_BUTTON_PRESSED, 0), 0);
            assert_eq!(
                kinds(&again),
                vec![MouseEventKind::Down(MouseButton::Right)],
                "press at {x},{y}"
            );
            assert_eq!((again[0].column, again[0].row), (x as u16, y as u16));
        }
    }

    #[test]
    fn left_drag_with_a_stuck_right_bit_is_a_left_drag() {
        let mut t = MouseRecordTranslator::new();
        t.translate(record(40, 10, RIGHTMOST_BUTTON_PRESSED, 0), 0);
        let both = FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED;
        assert_eq!(
            kinds(&t.translate(record(60, 20, both, 0), 0)),
            vec![MouseEventKind::Down(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(61, 20, both, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Drag(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(63, 20, both, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Drag(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(63, 20, RIGHTMOST_BUTTON_PRESSED, 0), 0)),
            vec![MouseEventKind::Up(MouseButton::Left)]
        );
        // And the stuck right button still clicks afterwards.
        assert_eq!(
            kinds(&t.translate(record(20, 5, RIGHTMOST_BUTTON_PRESSED, 0), 0)),
            vec![MouseEventKind::Down(MouseButton::Right)]
        );
    }

    #[test]
    fn motion_with_unknown_history_prefers_the_left_button() {
        let mut t = MouseRecordTranslator::new();
        let both = FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED;
        assert_eq!(
            kinds(&t.translate(record(1, 1, both, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Drag(MouseButton::Left)]
        );
        assert_eq!(
            kinds(&t.translate(record(1, 1, RIGHTMOST_BUTTON_PRESSED, MOUSE_MOVED), 0)),
            vec![MouseEventKind::Drag(MouseButton::Right)]
        );
    }

    #[test]
    fn double_click_flag_is_a_press() {
        let mut t = MouseRecordTranslator::new();
        t.translate(record(1, 1, FROM_LEFT_1ST_BUTTON_PRESSED, 0), 0);
        t.translate(record(1, 1, 0, 0), 0);
        assert_eq!(
            kinds(&t.translate(record(1, 1, FROM_LEFT_1ST_BUTTON_PRESSED, DOUBLE_CLICK), 0)),
            vec![MouseEventKind::Down(MouseButton::Left)]
        );
    }

    #[test]
    fn wheel_records_map_to_scrolls_and_ignore_buttons() {
        let mut t = MouseRecordTranslator::new();
        let up = (120u32 << 16) | RIGHTMOST_BUTTON_PRESSED;
        let down = ((-120i16 as u16 as u32) << 16) | RIGHTMOST_BUTTON_PRESSED;
        assert_eq!(
            kinds(&t.translate(record(1, 1, up, MOUSE_WHEELED), 0)),
            vec![MouseEventKind::ScrollUp]
        );
        assert_eq!(
            kinds(&t.translate(record(1, 1, down, MOUSE_WHEELED), 0)),
            vec![MouseEventKind::ScrollDown]
        );
        assert_eq!(
            kinds(&t.translate(record(1, 1, up, MOUSE_HWHEELED), 0)),
            vec![MouseEventKind::ScrollRight]
        );
        assert_eq!(
            kinds(&t.translate(record(1, 1, down, MOUSE_HWHEELED), 0)),
            vec![MouseEventKind::ScrollLeft]
        );
    }

    #[test]
    fn rows_are_viewport_relative_and_modifiers_decode() {
        let mut t = MouseRecordTranslator::new();
        let events = t.translate(
            RawMouseRecord {
                x: 3,
                y: 2300,
                button_state: FROM_LEFT_1ST_BUTTON_PRESSED,
                control_key_state: SHIFT_PRESSED | LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED,
                event_flags: 0,
            },
            2295,
        );
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].column, events[0].row), (3, 5));
        assert_eq!(
            events[0].modifiers,
            KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT
        );
    }

    #[test]
    fn simultaneous_release_and_press_emit_both_in_order() {
        let mut t = MouseRecordTranslator::new();
        t.translate(record(1, 1, FROM_LEFT_1ST_BUTTON_PRESSED, 0), 0);
        let events = t.translate(record(1, 1, RIGHTMOST_BUTTON_PRESSED, 0), 0);
        assert_eq!(
            kinds(&events),
            vec![
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Down(MouseButton::Right)
            ]
        );
    }

    #[test]
    fn foreign_events_keep_the_mask_honest() {
        let mut t = MouseRecordTranslator::new();
        t.observe_foreign(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(t.held_mask(), FROM_LEFT_1ST_BUTTON_PRESSED);
        assert_eq!(
            kinds(&t.translate(record(2, 2, 0, 0), 0)),
            vec![MouseEventKind::Up(MouseButton::Left)]
        );
    }
}
