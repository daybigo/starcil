//! Windows console input reader for the TUI client.
//!
//! crossterm parses `MOUSE_EVENT_RECORD`s by button *transitions* and gives the
//! right button priority on motion. Under hosts that swallow the right-button
//! release (Warp) conhost keeps reporting the right bit as held forever, so
//! crossterm emits exactly one right click per process lifetime and turns every
//! later left drag into `Drag(Right)`. This reader peeks the console input
//! queue, consumes mouse records itself (translated by
//! `starcil_tui::winmouse`, which tolerates stuck bits) and hands every other
//! record to `crossterm::event::read()`. Negotiated kitty input is received as
//! keyless UTF-16 records and decoded by our incremental VT parser.
//!
//! crossterm reads one record per produced event; the records it would silently
//! discard (bare modifier keys, Alt+numpad digits, menu events) are discarded
//! here first so it never reads past the head record into a mouse record.

use crossterm::event::Event as CtEvent;
use starcil_tui::winmouse::{MouseRecordTranslator, RawMouseRecord};
use std::sync::mpsc::Sender;
use std::collections::VecDeque;
use std::io::Write;
use std::time::{Duration, Instant};
use crate::vtinput::{Decoded, Parser};

type HANDLE = isize;
type BOOL = i32;
type DWORD = u32;
type WORD = u16;

const STD_INPUT_HANDLE: DWORD = -10i32 as DWORD;
const STD_OUTPUT_HANDLE: DWORD = -11i32 as DWORD;
const INVALID_HANDLE_VALUE: HANDLE = -1;
const INFINITE: DWORD = 0xFFFF_FFFF;
const WAIT_OBJECT_0: DWORD = 0;
const WAIT_TIMEOUT: DWORD = 258;
const ENABLE_VIRTUAL_TERMINAL_INPUT: DWORD = 0x0200;
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(70);

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
    fn GetConsoleMode(handle: HANDLE, mode: *mut DWORD) -> BOOL;
    fn SetConsoleMode(handle: HANDLE, mode: DWORD) -> BOOL;
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
    parser: Parser,
    surrogate: Option<u16>,
    pending_since: Option<Instant>,
    queued: VecDeque<CtEvent>,
    kitty_reply: Option<u16>,
    device_attributes: bool,
    last_raw: Option<String>,
}

/// Lives on the client main thread, so early returns restore input mode and
/// pop the terminal's flags before crossterm restores the original raw mode.
pub struct KeyboardMode {
    input: HANDLE,
    saved: DWORD,
    pub enhanced: bool,
}

impl Drop for KeyboardMode {
    fn drop(&mut self) {
        if self.enhanced {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[<1u");
            let _ = out.flush();
        }
        unsafe { SetConsoleMode(self.input, self.saved); }
    }
}

pub fn console_mode() -> std::io::Result<u32> {
    let mut mode = 0;
    if unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(mode)
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
            parser: Parser::default(),
            surrogate: None,
            pending_since: None,
            queued: VecDeque::new(),
            kitty_reply: None,
            device_attributes: false,
            last_raw: None,
        })
    }

    pub fn negotiate(&mut self) -> std::io::Result<KeyboardMode> {
        let saved = console_mode()?;
        let mut mode = KeyboardMode { input: self.input, saved, enhanced: false };
        // ConPTY drops an unknown CSI reply before ReadConsoleInputW can see
        // it unless VT input is enabled. Enable it ONLY for this bounded probe;
        // keep it afterwards ONLY if the terminal answered the kitty query.
        // Continue reading records, not ReadFile: retain the native mouse path.
        if unsafe { SetConsoleMode(self.input, saved | ENABLE_VIRTUAL_TERMINAL_INPUT) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?u\x1b[c")?;
        out.flush()?;
        let deadline = Instant::now() + Duration::from_millis(350);
        let mut deferred = VecDeque::new();
        // Conhost can answer DA1 itself before the OUTER terminal's kitty
        // reply returns. DA1 alone is not a negative answer on Windows; allow
        // the bounded window to finish before falling back to console input.
        while Instant::now() < deadline {
            if let Some(Step::Events(events)) = self.step_timeout(deadline.saturating_duration_since(Instant::now()))? {
                deferred.extend(events);
            }
            if self.kitty_reply.is_some() && self.device_attributes { break; }
        }
        self.queued.extend(deferred);
        mode.enhanced = self.kitty_reply.is_some();
        if mode.enhanced {
            out.write_all(b"\x1b[>1u")?;
            out.flush()?;
        } else if unsafe { SetConsoleMode(self.input, saved & !ENABLE_VIRTUAL_TERMINAL_INPUT) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        crate::keytrace::record(&format!("MODE kitty={} flags={:?} input={:#x}", mode.enhanced, self.kitty_reply, console_mode()?));
        Ok(mode)
    }

    /// Blocks until the head of the console input queue produced something.
    pub fn step(&mut self) -> std::io::Result<Step> {
        loop {
            if let Some(step) = self.step_timeout(Duration::from_millis(INFINITE as u64 - 1))? {
                return Ok(step);
            }
        }
    }

    pub fn step_timeout(&mut self, timeout: Duration) -> std::io::Result<Option<Step>> {
        self.last_raw = None;
        if let Some(event) = self.queued.pop_front() {
            return Ok(Some(Step::Events(vec![event])));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let remaining = self.pending_since.map(|start| remaining.min(ESCAPE_TIMEOUT.saturating_sub(start.elapsed()))).unwrap_or(remaining);
            let millis = remaining.as_millis().min(u32::MAX as u128 - 1) as u32;
            let wait = unsafe { WaitForSingleObject(self.input, millis) };
            if wait == WAIT_TIMEOUT {
                if self.pending_since.is_some_and(|start| start.elapsed() >= ESCAPE_TIMEOUT) {
                    let decoded = self.parser.expire();
                    self.pending_since = None;
                    return Ok(Some(self.decoded_step(decoded)));
                }
                if Instant::now() >= deadline { return Ok(None); }
                continue;
            }
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
                        return Ok(Some(Step::Dropped(format!(
                            "MOUSE pos=({},{}) buttons={:#x} flags={:#x} -> no event",
                            raw.x, raw.y, raw.button_state, raw.event_flags
                        ))));
                    }
                    return Ok(Some(Step::Events(events.into_iter().map(CtEvent::Mouse).collect())));
                }
                KEY_EVENT => {
                    let key = unsafe { head.event.key };
                    let raw = format!("RAW KEY vk={:#04x} scan={:#04x} char={:#06x} control={:#06x} down={} repeat={}",
                        key.virtual_key_code, key.virtual_scan_code, key.unicode_char,
                        key.control_key_state, key.key_down, key.repeat_count);
                    crate::keytrace::record(&raw);
                    self.last_raw = Some(raw);
                    // Unknown VT sequences and win32 passthrough replies are
                    // keyless records. Leave native VK records to crossterm.
                    if key.virtual_key_code == 0 && key.unicode_char != 0 {
                        self.read_one()?;
                        let mut decoded = Vec::new();
                        if key.key_down != 0 {
                            for _ in 0..key.repeat_count.max(1) {
                                if let Some(ch) = decode_unit(&mut self.surrogate, key.unicode_char) {
                                    decoded.extend(self.parser.push(ch));
                                }
                            }
                        }
                        if self.parser.is_pending() {
                            self.pending_since.get_or_insert_with(Instant::now);
                        } else {
                            self.pending_since = None;
                        }
                        return Ok(Some(self.decoded_step(decoded)));
                    }
                    if crossterm_discards(&key) {
                        self.read_one()?;
                        return Ok(Some(Step::Dropped(format!(
                            "KEY vk={:#x} down={} (modifier-only, discarded)",
                            key.virtual_key_code, key.key_down
                        ))));
                    }
                    return self.read_via_crossterm().map(Some);
                }
                WINDOW_BUFFER_SIZE_EVENT | FOCUS_EVENT => return self.read_via_crossterm().map(Some),
                other => {
                    self.read_one()?;
                    return Ok(Some(Step::Dropped(format!("EVENT type={other} (discarded)"))));
                }
            }
        }
    }

    fn decoded_step(&mut self, decoded: Vec<Decoded>) -> Step {
        let mut events = Vec::new();
        for item in decoded {
            match item {
                Decoded::Key(key) => events.push(CtEvent::Key(key)),
                Decoded::KittyFlags(flags) => self.kitty_reply = Some(flags),
                Decoded::DeviceAttributes => self.device_attributes = true,
                Decoded::SgrMouse { button, x, y, pressed } => {
                    let raw = sgr_record(button, x, y, pressed, self.translator.held_mask());
                    // SGR already uses viewport coordinates; native console
                    // MOUSE_EVENTs above still subtract the screen-buffer top.
                    events.extend(self.translator.translate(raw, 0).into_iter().map(CtEvent::Mouse));
                }
            }
        }
        if events.is_empty() { Step::Dropped("VT fragment/reply/release".into()) }
        else { Step::Events(events) }
    }

    pub fn describe(&self, step: &Step) -> String {
        let mut lines = self.last_raw.iter().cloned().collect::<Vec<_>>();
        match step {
            Step::Events(events) => lines.extend(events.iter().map(crate::keytrace::describe)),
            Step::Dropped(reason) => lines.push(format!("DROPPED {reason}")),
        }
        lines.join("\n")
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
pub fn spawn_input_thread(tx: Sender<CtEvent>, reader: std::io::Result<ConsoleReader>) {
    std::thread::spawn(move || {
        let mut reader = match reader {
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
                        crate::keytrace::record(&crate::keytrace::describe(&event));
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

fn decode_unit(pending: &mut Option<u16>, unit: u16) -> Option<char> {
    if (0xd800..=0xdbff).contains(&unit) {
        *pending = Some(unit);
        return None;
    }
    if let Some(high) = pending.take() {
        if (0xdc00..=0xdfff).contains(&unit) {
            return char::from_u32(0x10000 + (((high as u32 - 0xd800) << 10) | (unit as u32 - 0xdc00)));
        }
    }
    char::from_u32(unit as u32)
}

fn sgr_record(button: u16, x: i16, y: i16, pressed: bool, held: u32) -> RawMouseRecord {
    use starcil_tui::winmouse::{MOUSE_MOVED, MOUSE_WHEELED, MOUSE_HWHEELED};
    let mut control = 0;
    if button & 4 != 0 { control |= SHIFT_PRESSED; }
    if button & 8 != 0 { control |= LEFT_ALT_PRESSED; }
    if button & 16 != 0 { control |= LEFT_CTRL_PRESSED; }
    let (buttons, flags) = if button & 64 != 0 {
        let horizontal = button & 2 != 0;
        let positive = (button & 1 == 0) != horizontal;
        let delta: i16 = if positive { 120 } else { -120 };
        (held | ((delta as u16 as u32) << 16), if horizontal { MOUSE_HWHEELED } else { MOUSE_WHEELED })
    } else {
        let bit = match button & 3 { 0 => 1, 1 => 4, 2 => 2, _ => 0 };
        let buttons = if bit == 0 { 0 } else if pressed { held | bit } else { held & !bit };
        (buttons, if button & 32 != 0 { MOUSE_MOVED } else { 0 })
    };
    RawMouseRecord { x, y, button_state: buttons, control_key_state: control, event_flags: flags }
}

pub fn probe_keys(args: &[String]) -> std::io::Result<()> {
    let mut seconds = 20;
    let mut path = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--seconds" {
            seconds = args.next().and_then(|s| s.parse::<u64>().ok()).filter(|s| (1..=120).contains(s))
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "--seconds requires 1..120"))?;
        } else if path.is_none() && !arg.starts_with('-') { path = Some(arg); }
        else { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "use __probe-keys --help")); }
    }
    let original = console_mode()?;
    crossterm::terminal::enable_raw_mode()?;
    let result: std::io::Result<String> = (|| {
        let mut out = std::io::stdout();
        crossterm::execute!(out, crossterm::event::EnableMouseCapture)?;
        let mut reader = ConsoleReader::new()?;
        let mode = reader.negotiate()?;
        let mut log = format!("READY kitty={} input={:#x} original={original:#x} seconds={seconds}\n", mode.enhanced, console_mode()?);
        print!("{}\r", log);
        out.flush()?;
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if let Some(step) = reader.step_timeout(Duration::from_millis(100))? {
                let line = reader.describe(&step);
                if let Step::Events(events) = &step {
                    for event in events { crate::keytrace::record(&crate::keytrace::describe(event)); }
                }
                for line in line.lines() { write!(out, "{line}\r\n")?; }
                out.flush()?;
                log.push_str(&line);
                log.push('\n');
            }
        }
        drop(mode);
        Ok(log)
    })();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    let _ = crossterm::terminal::disable_raw_mode();
    // Mouse capture modifies flags beyond raw mode; restore the complete mode.
    unsafe { SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), original); }
    let mut log: String = result?;
    let restored = format!("RESTORED input={:#x} original={original:#x}\n", console_mode()?);
    print!("{restored}");
    log.push_str(&restored);
    if let Some(path) = path { std::fs::write(path, log)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyless_utf16_surrogates_and_recovery() {
        for (units, expected) in [
            (vec![0x00f1], vec!['ñ']),
            (vec![0xd83d, 0xde80], vec!['🚀']),
            (vec![0xd83d, 0x61], vec!['a']),
            (vec![0xde80, 0x62], vec!['b']),
        ] {
            let mut pending = None;
            let chars: Vec<char> = units.into_iter().filter_map(|unit| decode_unit(&mut pending, unit)).collect();
            assert_eq!(chars, expected);
        }
    }
    #[test]
    fn native_modifier_filter_keeps_modified_enter_and_alt_codes() {
        for (vk, down, ch, state, discard) in [
            (VK_SHIFT, 1, 0, SHIFT_PRESSED, true),
            (VK_MENU, 0, 233, 0, false),
            (VK_NUMPAD0, 1, 0, LEFT_ALT_PRESSED, true),
            (13, 1, 10, LEFT_CTRL_PRESSED, false),
            (13, 1, 13, LEFT_ALT_PRESSED, false),
            (13, 1, 13, SHIFT_PRESSED, false),
        ] {
            let key = KeyEventRecord { key_down: down, repeat_count: 1, virtual_key_code: vk, virtual_scan_code: 0, unicode_char: ch, control_key_state: state };
            assert_eq!(crossterm_discards(&key), discard);
        }
    }

    #[test]
    fn vt_mouse_uses_the_stuck_button_translator_and_preserves_wheel_modifiers() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let mut translator = MouseRecordTranslator::new();
        for (button, pressed, expected) in [
            (2, true, MouseEventKind::Down(MouseButton::Right)),
            (2, true, MouseEventKind::Down(MouseButton::Right)),
            (0, true, MouseEventKind::Down(MouseButton::Left)),
            (32, true, MouseEventKind::Drag(MouseButton::Left)),
            (0, false, MouseEventKind::Up(MouseButton::Left)),
            (2, false, MouseEventKind::Up(MouseButton::Right)),
            (35, true, MouseEventKind::Moved),
            (1, true, MouseEventKind::Down(MouseButton::Middle)),
            (1, false, MouseEventKind::Up(MouseButton::Middle)),
            (64, true, MouseEventKind::ScrollUp),
            (65, true, MouseEventKind::ScrollDown),
            (66, true, MouseEventKind::ScrollLeft),
            (67, true, MouseEventKind::ScrollRight),
        ] {
            let raw = sgr_record(button | 4 | 8 | 16, 39, 9, pressed, translator.held_mask());
            let events = translator.translate(raw, 0);
            assert_eq!(events.len(), 1, "button={button}");
            assert_eq!(events[0].kind, expected, "button={button}");
            assert_eq!((events[0].column, events[0].row), (39, 9));
            assert_eq!(events[0].modifiers, KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        }
    }
}
