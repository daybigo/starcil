//! Windows console input reader for the TUI client.
//!
//! crossterm parses `MOUSE_EVENT_RECORD`s by button *transitions* and gives the
//! right button priority on motion. Under hosts that swallow the right-button
//! release (Warp) conhost keeps reporting the right bit as held forever, so
//! crossterm emits exactly one right click per process lifetime and turns every
//! later left drag into `Drag(Right)`. This reader peeks the console input
//! queue, consumes mouse records itself (translated by
//! `starcil_tui::winmouse`, which tolerates stuck bits) and hands every other
//! record to `crossterm::event::read()`, so keyboard parsing stays crossterm's.
//!
//! crossterm reads one record per produced event; the records it would silently
//! discard (bare modifier keys, Alt+numpad digits, menu events) are discarded
//! here first so it never reads past the head record into a mouse record.

use crossterm::event::Event as CtEvent;
use starcil_tui::winmouse::{MouseRecordTranslator, RawMouseRecord};
use std::sync::mpsc::Sender;

type HANDLE = isize;
type BOOL = i32;
type DWORD = u32;
type WORD = u16;

const STD_INPUT_HANDLE: DWORD = -10i32 as DWORD;
const STD_OUTPUT_HANDLE: DWORD = -11i32 as DWORD;
const INVALID_HANDLE_VALUE: HANDLE = -1;
const INFINITE: DWORD = 0xFFFF_FFFF;
const WAIT_OBJECT_0: DWORD = 0;

const KEY_EVENT: WORD = 0x0001;
const MOUSE_EVENT: WORD = 0x0002;
const WINDOW_BUFFER_SIZE_EVENT: WORD = 0x0004;
const FOCUS_EVENT: WORD = 0x0010;

const VK_SHIFT: WORD = 0x10;
const VK_CONTROL: WORD = 0x11;
const VK_MENU: WORD = 0x12;
const VK_NUMPAD0: WORD = 0x60;
const VK_NUMPAD9: WORD = 0x69;
const RIGHT_ALT_PRESSED: DWORD = 0x0001;
const LEFT_ALT_PRESSED: DWORD = 0x0002;
const RIGHT_CTRL_PRESSED: DWORD = 0x0004;
const LEFT_CTRL_PRESSED: DWORD = 0x0008;
const SHIFT_PRESSED: DWORD = 0x0010;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ConsoleScreenBufferInfo {
    size: Coord,
    cursor_position: Coord,
    attributes: WORD,
    window: SmallRect,
    maximum_window_size: Coord,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEventRecord {
    key_down: BOOL,
    repeat_count: WORD,
    virtual_key_code: WORD,
    virtual_scan_code: WORD,
    unicode_char: WORD,
    control_key_state: DWORD,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseEventRecord {
    mouse_position: Coord,
    button_state: DWORD,
    control_key_state: DWORD,
    event_flags: DWORD,
}

#[repr(C)]
#[derive(Clone, Copy)]
union EventUnion {
    key: KeyEventRecord,
    mouse: MouseEventRecord,
    raw: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputRecord {
    event_type: WORD,
    event: EventUnion,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(std_handle: DWORD) -> HANDLE;
    fn WaitForSingleObject(handle: HANDLE, milliseconds: DWORD) -> DWORD;
    fn PeekConsoleInputW(
        console_input: HANDLE,
        buffer: *mut InputRecord,
        length: DWORD,
        number_read: *mut DWORD,
    ) -> BOOL;
    fn ReadConsoleInputW(
        console_input: HANDLE,
        buffer: *mut InputRecord,
        length: DWORD,
        number_read: *mut DWORD,
    ) -> BOOL;
    fn GetConsoleScreenBufferInfo(
        console_output: HANDLE,
        info: *mut ConsoleScreenBufferInfo,
    ) -> BOOL;
}

/// What the reader produces per step: events ready for the app, or a raw
/// record it consumed silently (for the diagnostic probe).
pub enum Step {
    Events(Vec<CtEvent>),
    Dropped(String),
}

pub struct ConsoleReader {
    input: HANDLE,
    output: HANDLE,
    translator: MouseRecordTranslator,
}

impl ConsoleReader {
    pub fn new() -> std::io::Result<Self> {
        let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if input == INVALID_HANDLE_VALUE || input == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        Ok(Self {
            input,
            output,
            translator: MouseRecordTranslator::new(),
        })
    }

    /// Blocks until the head of the console input queue produced something.
    pub fn step(&mut self) -> std::io::Result<Step> {
        loop {
            let wait = unsafe { WaitForSingleObject(self.input, INFINITE) };
            if wait != WAIT_OBJECT_0 {
                return Err(std::io::Error::last_os_error());
            }
            let Some(head) = self.peek()? else {
                continue;
            };
            match head.event_type {
                MOUSE_EVENT => {
                    let record = self.read_one()?;
                    let mouse = unsafe { record.event.mouse };
                    let raw = RawMouseRecord {
                        x: mouse.mouse_position.x,
                        y: mouse.mouse_position.y,
                        button_state: mouse.button_state,
                        control_key_state: mouse.control_key_state,
                        event_flags: mouse.event_flags,
                    };
                    let top = self.window_top();
                    let events = self.translator.translate(raw, top);
                    tracing::trace!(
                        x = raw.x,
                        y = raw.y,
                        buttons = format_args!("{:#x}", raw.button_state),
                        flags = format_args!("{:#x}", raw.event_flags),
                        held = format_args!("{:#x}", self.translator.held_mask()),
                        produced = events.len(),
                        "mouse record"
                    );
                    if events.is_empty() {
                        return Ok(Step::Dropped(format!(
                            "MOUSE pos=({},{}) buttons={:#x} flags={:#x} -> no event",
                            raw.x, raw.y, raw.button_state, raw.event_flags
                        )));
                    }
                    return Ok(Step::Events(events.into_iter().map(CtEvent::Mouse).collect()));
                }
                KEY_EVENT => {
                    let key = unsafe { head.event.key };
                    if crossterm_discards(&key) {
                        self.read_one()?;
                        return Ok(Step::Dropped(format!(
                            "KEY vk={:#x} down={} (modifier-only, discarded)",
                            key.virtual_key_code, key.key_down
                        )));
                    }
                    return self.read_via_crossterm();
                }
                WINDOW_BUFFER_SIZE_EVENT | FOCUS_EVENT => return self.read_via_crossterm(),
                other => {
                    self.read_one()?;
                    return Ok(Step::Dropped(format!("EVENT type={other} (discarded)")));
                }
            }
        }
    }

    fn read_via_crossterm(&mut self) -> std::io::Result<Step> {
        let event = crossterm::event::read()?;
        if let CtEvent::Mouse(mouse) = &event {
            // crossterm read past the head record into a mouse record; keep
            // the held mask honest for the records we translate next.
            self.translator.observe_foreign(mouse);
        }
        Ok(Step::Events(vec![event]))
    }

    fn peek(&self) -> std::io::Result<Option<InputRecord>> {
        let mut record = InputRecord {
            event_type: 0,
            event: EventUnion { raw: [0; 16] },
        };
        let mut count: DWORD = 0;
        let ok = unsafe { PeekConsoleInputW(self.input, &mut record, 1, &mut count) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((count > 0).then_some(record))
    }

    fn read_one(&self) -> std::io::Result<InputRecord> {
        let mut record = InputRecord {
            event_type: 0,
            event: EventUnion { raw: [0; 16] },
        };
        let mut count: DWORD = 0;
        let ok = unsafe { ReadConsoleInputW(self.input, &mut record, 1, &mut count) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(record)
    }

    fn window_top(&self) -> i16 {
        let mut info = ConsoleScreenBufferInfo {
            size: Coord { x: 0, y: 0 },
            cursor_position: Coord { x: 0, y: 0 },
            attributes: 0,
            window: SmallRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            maximum_window_size: Coord { x: 0, y: 0 },
        };
        let ok = unsafe { GetConsoleScreenBufferInfo(self.output, &mut info) };
        if ok == 0 {
            0
        } else {
            info.window.top
        }
    }
}

/// Mirrors the records crossterm's Windows key parser turns into `None`, so it
/// never loops past the head record looking for the next one.
fn crossterm_discards(key: &KeyEventRecord) -> bool {
    let vk = key.virtual_key_code;
    let down = key.key_down != 0;
    // An Alt release carrying a character is an Alt code — crossterm reports it.
    if vk == VK_MENU && !down && key.unicode_char != 0 {
        return false;
    }
    if matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU) {
        return true;
    }
    let state = key.control_key_state;
    let alt = state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0;
    let shift_or_ctrl =
        state & (SHIFT_PRESSED | LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
    alt && !shift_or_ctrl && (VK_NUMPAD0..=VK_NUMPAD9).contains(&vk)
}

/// Input thread for the client loop: blocking reads, zero polling latency.
pub fn spawn_input_thread(tx: Sender<CtEvent>) {
    std::thread::spawn(move || {
        let mut reader = match ConsoleReader::new() {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(%error, "console input unavailable; falling back to crossterm");
                loop {
                    match crossterm::event::read() {
                        Ok(event) => {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            }
        };
        loop {
            match reader.step() {
                Ok(Step::Events(events)) => {
                    for event in events {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                }
                Ok(Step::Dropped(_)) => {}
                Err(error) => {
                    tracing::warn!(%error, "console input read failed");
                    return;
                }
            }
        }
    });
}
