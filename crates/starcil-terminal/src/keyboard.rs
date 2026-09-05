//! Key chords → PTY bytes, in whichever encoding the pane's program asked for.
//!
//! Three encodings coexist; [`TerminalKeyboardMode`] (tracked by the screen
//! from what the program wrote) picks between them per key:
//!
//! * **legacy** (xterm): what every terminal sends by default. A modified
//!   Enter has no spelling here (`\r` is `\r`), so shift+enter becomes the
//!   readline "meta+enter" (`ESC CR`) that composers read as "insert a
//!   newline" (Claude Code, aider, ink apps), and ctrl+enter becomes LF — the
//!   ctrl+j newline of those same composers, and what conhost itself produces.
//! * **kitty keyboard protocol** (`CSI code;mods u`): pushed by the program
//!   with `CSI > flags u` (crossterm apps such as codex, Claude Code ≥ 2.x,
//!   gemini-cli). Esc and every modified Enter/Tab/Backspace are unambiguous.
//! * **win32-input-mode** (`CSI Vk;Sc;Uc;Kd;Cs;Rc _`): ConPTY asks for it with
//!   `CSI ? 9001 h`. conhost turns each record into an exact
//!   `KEY_EVENT_RECORD`, so a program that reads the console (crossterm on
//!   Windows: codex) sees a real shift+enter. It is also the ONLY road through
//!   ConPTY for kitty sequences: conhost drops a `CSI 13;2u` written raw
//!   (measured 2026-09-04, `keyprobe.py`), but forwards every byte wrapped as
//!   a keyless character record.

use serde::{Deserialize, Serialize};

/// Kitty flag 0b1: disambiguate escape codes (the one every client sets).
pub const KITTY_DISAMBIGUATE: u8 = 0b0000_0001;
/// Kitty flag 0b1000: report all keys (even plain text) as escape codes.
pub const KITTY_REPORT_ALL_KEYS: u8 = 0b0000_1000;
const KITTY_FLAG_MASK: u8 = 0b0001_1111;
const KITTY_STACK_LIMIT: usize = 16;

/// What the pane's program negotiated for keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TerminalKeyboardMode {
    /// Kitty keyboard protocol flags in force for the current screen
    /// (0 = legacy encoding).
    pub kitty_flags: u8,
    /// ConPTY requested win32-input-mode: modified Enter travels as a
    /// `KEY_EVENT_RECORD` and kitty sequences as character records.
    pub win32_input: bool,
    /// DECCKM: unmodified cursor keys as `SS3 A` instead of `CSI A`.
    pub application_cursor: bool,
}

impl TerminalKeyboardMode {
    pub fn kitty(&self) -> bool {
        self.kitty_flags != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid key: {0}")]
pub struct InvalidKey(pub String);

/// Kitty flag stacks (one per screen, as the protocol demands) plus the
/// ConPTY win32-input-mode switch. Pure state, driven by the interceptor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct KeyboardState {
    pub(crate) win32_input: bool,
    main_stack: Vec<u8>,
    alternate_stack: Vec<u8>,
}

impl KeyboardState {
    pub(crate) fn flags(&self, alternate_screen: bool) -> u8 {
        self.stack(alternate_screen).last().copied().unwrap_or(0)
    }

    pub(crate) fn push(&mut self, alternate_screen: bool, flags: u8) {
        let stack = self.stack_mut(alternate_screen);
        if stack.len() >= KITTY_STACK_LIMIT {
            stack.remove(0);
        }
        stack.push(flags & KITTY_FLAG_MASK);
    }

    pub(crate) fn pop(&mut self, alternate_screen: bool, count: u8) {
        let stack = self.stack_mut(alternate_screen);
        for _ in 0..count.max(1) {
            stack.pop();
        }
    }

    /// `CSI = flags ; mode u`: 1 = set, 2 = OR in, 3 = AND out. With an empty
    /// stack the implicit base entry is what gets edited.
    pub(crate) fn set(&mut self, alternate_screen: bool, flags: u8, mode: u8) {
        let stack = self.stack_mut(alternate_screen);
        if stack.is_empty() {
            stack.push(0);
        }
        let current = stack.last_mut().expect("non-empty stack");
        let flags = flags & KITTY_FLAG_MASK;
        *current = match mode {
            2 => *current | flags,
            3 => *current & !flags,
            _ => flags,
        };
    }

    /// Leaving the alternate screen drops its stack (a crashed TUI must not
    /// leave the shell in kitty mode).
    pub(crate) fn leave_alternate_screen(&mut self) {
        self.alternate_stack.clear();
    }

    /// RIS.
    pub(crate) fn reset(&mut self) {
        self.main_stack.clear();
        self.alternate_stack.clear();
    }

    fn stack(&self, alternate_screen: bool) -> &Vec<u8> {
        if alternate_screen {
            &self.alternate_stack
        } else {
            &self.main_stack
        }
    }

    fn stack_mut(&mut self, alternate_screen: bool) -> &mut Vec<u8> {
        if alternate_screen {
            &mut self.alternate_stack
        } else {
            &mut self.main_stack
        }
    }
}

/// Reply to `CSI ? u`. Through ConPTY the bytes must ride as character
/// records: conhost drops a raw `CSI ? 0 u`.
pub fn kitty_flags_response(flags: u8, win32_input: bool) -> Vec<u8> {
    let response = format!("\x1b[?{flags}u").into_bytes();
    if win32_input {
        win32_passthrough(&response)
    } else {
        response
    }
}

/// Encode one chord (`shift+enter`, `ctrl+c`, `f5`, `alt+left`, `a`) for
/// the pane's negotiated keyboard mode.
pub fn encode_key(key: &str, mode: TerminalKeyboardMode) -> Result<Vec<u8>, InvalidKey> {
    let chord = parse_chord(key)?;
    let encoded = if mode.kitty() {
        encode_kitty(chord, mode.kitty_flags)
    } else {
        encode_legacy(chord, mode).ok_or_else(|| InvalidKey(key.to_owned()))?
    };
    Ok(match encoded {
        Encoded::Legacy(bytes) | Encoded::Win32(bytes) => bytes,
        Encoded::CsiU(bytes) if mode.win32_input => win32_passthrough(&bytes),
        Encoded::CsiU(bytes) => bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseKey {
    Char(char),
    Enter,
    Tab,
    Esc,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Space,
    Function(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    key: BaseKey,
}

enum Encoded {
    /// Bytes every terminal layer understands as-is.
    Legacy(Vec<u8>),
    /// A kitty `CSI … u` sequence: conhost needs it wrapped.
    CsiU(Vec<u8>),
    /// win32-input-mode records, already transport-ready.
    Win32(Vec<u8>),
}

fn parse_chord(key: &str) -> Result<Chord, InvalidKey> {
    let invalid = || InvalidKey(key.to_owned());
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(invalid());
    }
    // A trailing `+` is the plus key itself (`ctrl++`, `+`).
    let (head, base) = if trimmed == "+" {
        ("", "+")
    } else if let Some(head) = trimmed.strip_suffix("++") {
        (head, "+")
    } else {
        match trimmed.rfind('+') {
            Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
            None => ("", trimmed),
        }
    };
    let mut chord = Chord {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
        key: BaseKey::Enter,
    };
    for token in head.split('+').filter(|token| !token.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => chord.ctrl = true,
            "alt" | "opt" | "option" => chord.alt = true,
            "shift" => chord.shift = true,
            "super" | "cmd" | "meta" | "win" => chord.meta = true,
            _ => return Err(invalid()),
        }
    }
    chord.key = parse_base(base).ok_or_else(invalid)?;
    Ok(chord)
}

fn parse_base(token: &str) -> Option<BaseKey> {
    let mut characters = token.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Some(BaseKey::Char(character));
    }
    Some(match token.to_ascii_lowercase().as_str() {
        "enter" | "return" | "cr" => BaseKey::Enter,
        "tab" => BaseKey::Tab,
        "esc" | "escape" => BaseKey::Esc,
        "backspace" | "bs" => BaseKey::Backspace,
        "delete" | "del" => BaseKey::Delete,
        "insert" | "ins" => BaseKey::Insert,
        "home" => BaseKey::Home,
        "end" => BaseKey::End,
        "pageup" | "page_up" | "pgup" => BaseKey::PageUp,
        "pagedown" | "page_down" | "pgdn" => BaseKey::PageDown,
        "up" => BaseKey::Up,
        "down" => BaseKey::Down,
        "left" => BaseKey::Left,
        "right" => BaseKey::Right,
        "space" => BaseKey::Space,
        "minus" => BaseKey::Char('-'),
        "comma" => BaseKey::Char(','),
        "ampersand" => BaseKey::Char('&'),
        "plus" => BaseKey::Char('+'),
        "backtick" => BaseKey::Char('`'),
        name => {
            let number = name.strip_prefix('f')?.parse::<u8>().ok()?;
            if !(1..=24).contains(&number) {
                return None;
            }
            BaseKey::Function(number)
        }
    })
}

/// xterm/kitty modifier parameter: 1 + shift(1) + alt(2) + ctrl(4) + super(8).
fn modifier_param(chord: &Chord, with_meta: bool) -> u8 {
    1 + u8::from(chord.shift)
        + 2 * u8::from(chord.alt)
        + 4 * u8::from(chord.ctrl)
        + 8 * u8::from(chord.meta && with_meta)
}

/// Arrows, Home/End, the tilde keys and F-keys: shared by both VT encodings
/// (kitty keeps the legacy spellings for them).
fn encode_functional(key: BaseKey, mods: u8, application_cursor: bool) -> Option<Vec<u8>> {
    let letter = |final_byte: char| {
        if mods != 1 {
            format!("\x1b[1;{mods}{final_byte}")
        } else if application_cursor {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        }
    };
    let tilde = |number: u8| {
        if mods == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{mods}~")
        }
    };
    let ss3 = |final_byte: char| {
        if mods == 1 {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[1;{mods}{final_byte}")
        }
    };
    let encoded = match key {
        BaseKey::Up => letter('A'),
        BaseKey::Down => letter('B'),
        BaseKey::Right => letter('C'),
        BaseKey::Left => letter('D'),
        BaseKey::Home => letter('H'),
        BaseKey::End => letter('F'),
        BaseKey::Insert => tilde(2),
        BaseKey::Delete => tilde(3),
        BaseKey::PageUp => tilde(5),
        BaseKey::PageDown => tilde(6),
        BaseKey::Function(1) => ss3('P'),
        BaseKey::Function(2) => ss3('Q'),
        BaseKey::Function(3) => ss3('R'),
        BaseKey::Function(4) => ss3('S'),
        BaseKey::Function(number) => tilde(match number {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            12 => 24,
            13 => 25,
            14 => 26,
            15 => 28,
            16 => 29,
            17 => 31,
            18 => 32,
            19 => 33,
            20 => 34,
            _ => return None,
        }),
        _ => return None,
    };
    Some(encoded.into_bytes())
}

/// C0 byte for ctrl+`character` (xterm's table).
fn control_byte(character: char) -> Option<u8> {
    Some(match character.to_ascii_lowercase() {
        letter @ 'a'..='z' => (letter as u8) - b'a' + 1,
        '@' | '2' | ' ' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

fn encode_legacy(chord: Chord, mode: TerminalKeyboardMode) -> Option<Encoded> {
    let mods = modifier_param(&chord, false);
    let alt_prefixed = |mut bytes: Vec<u8>| {
        if chord.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    Some(match chord.key {
        BaseKey::Char(character) => {
            if chord.ctrl {
                let control = control_byte(character)?;
                // conhost reads the raw C0 byte back as Backspace / Tab /
                // ctrl+Enter, so a console reader (codex) never sees ctrl+j;
                // a key record names the letter that was actually pressed.
                let letter = character.to_ascii_uppercase();
                if mode.win32_input && !chord.alt && matches!(letter, 'H' | 'I' | 'J' | 'M') {
                    let scan = match letter {
                        'H' => 35,
                        'I' => 23,
                        'J' => 36,
                        _ => 50,
                    };
                    return Some(Encoded::Win32(win32_key_events(
                        u16::from(letter as u8),
                        scan,
                        u16::from(control),
                        &chord,
                    )));
                }
                Encoded::Legacy(alt_prefixed(vec![control]))
            } else {
                let character = if chord.shift {
                    character.to_ascii_uppercase()
                } else {
                    character
                };
                Encoded::Legacy(alt_prefixed(character.to_string().into_bytes()))
            }
        }
        BaseKey::Enter => {
            if mods == 1 {
                Encoded::Legacy(b"\r".to_vec())
            } else if mode.win32_input {
                let unicode = if chord.ctrl { b'\n' } else { b'\r' };
                Encoded::Win32(win32_key_events(VK_RETURN, SCAN_ENTER, u16::from(unicode), &chord))
            } else if chord.ctrl {
                // conhost's spelling of ctrl+enter, and ctrl+j for readline
                // composers.
                Encoded::Legacy(alt_prefixed(b"\n".to_vec()))
            } else {
                // shift+enter / alt+enter: meta+enter, the newline chord of
                // every readline-style composer.
                Encoded::Legacy(b"\x1b\r".to_vec())
            }
        }
        BaseKey::Tab => {
            if chord.ctrl && mode.win32_input {
                Encoded::Win32(win32_key_events(VK_TAB, SCAN_TAB, u16::from(b'\t'), &chord))
            } else if chord.shift {
                Encoded::Legacy(alt_prefixed(b"\x1b[Z".to_vec()))
            } else {
                Encoded::Legacy(alt_prefixed(b"\t".to_vec()))
            }
        }
        BaseKey::Esc => {
            if mode.win32_input && !chord.alt {
                // A raw ESC sits in conhost's escape-sequence timeout (and is
                // lost if more input follows within it); the key record is
                // what Windows Terminal itself sends.
                Encoded::Win32(win32_key_events(VK_ESCAPE, SCAN_ESCAPE, 0x1b, &chord))
            } else {
                Encoded::Legacy(alt_prefixed(vec![0x1b]))
            }
        }
        BaseKey::Backspace => {
            Encoded::Legacy(alt_prefixed(vec![if chord.ctrl { 0x08 } else { 0x7f }]))
        }
        BaseKey::Space => Encoded::Legacy(alt_prefixed(vec![if chord.ctrl { 0x00 } else { b' ' }])),
        functional => Encoded::Legacy(encode_functional(
            functional,
            mods,
            mode.application_cursor,
        )?),
    })
}

fn encode_kitty(chord: Chord, flags: u8) -> Encoded {
    let mods = modifier_param(&chord, true);
    let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
    let modified = mods != 1;
    let csi_u = |code: u32| {
        Encoded::CsiU(
            if mods == 1 {
                format!("\x1b[{code}u")
            } else {
                format!("\x1b[{code};{mods}u")
            }
            .into_bytes(),
        )
    };
    let plain_or = |bytes: &[u8], code: u32| {
        if modified || report_all {
            csi_u(code)
        } else {
            Encoded::Legacy(bytes.to_vec())
        }
    };
    match chord.key {
        BaseKey::Char(character) => {
            let text_only = !(chord.ctrl || chord.alt || chord.meta);
            if text_only && !report_all {
                let character = if chord.shift {
                    character.to_ascii_uppercase()
                } else {
                    character
                };
                return Encoded::Legacy(character.to_string().into_bytes());
            }
            // The protocol keys on the unshifted code point.
            let code = character.to_lowercase().next().unwrap_or(character) as u32;
            csi_u(code)
        }
        BaseKey::Enter => plain_or(b"\r", 13),
        BaseKey::Tab => plain_or(b"\t", 9),
        BaseKey::Backspace => plain_or(&[0x7f], 127),
        BaseKey::Space => plain_or(b" ", 32),
        // The whole point of "disambiguate": a lone ESC is never a prefix.
        BaseKey::Esc => csi_u(27),
        functional => match encode_functional(functional, mods, false) {
            Some(bytes) => Encoded::Legacy(bytes),
            // F21+: kitty-only numbers, nothing legacy to fall back to.
            None => csi_u(57376 + u32::from(match functional {
                BaseKey::Function(number) => number.saturating_sub(13),
                _ => 0,
            })),
        },
    }
}

// win32-input-mode (Windows Terminal / ConPTY): `CSI Vk;Sc;Uc;Kd;Cs;Rc _`.
const VK_RETURN: u16 = 0x0d;
const VK_TAB: u16 = 0x09;
const VK_ESCAPE: u16 = 0x1b;
const SCAN_ENTER: u16 = 28;
const SCAN_TAB: u16 = 15;
const SCAN_ESCAPE: u16 = 1;
const SHIFT_PRESSED: u32 = 0x0010;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const LEFT_ALT_PRESSED: u32 = 0x0002;

fn control_key_state(chord: &Chord) -> u32 {
    (if chord.shift { SHIFT_PRESSED } else { 0 })
        | (if chord.ctrl { LEFT_CTRL_PRESSED } else { 0 })
        | (if chord.alt { LEFT_ALT_PRESSED } else { 0 })
}

fn win32_record(vk: u16, scan: u16, unicode: u16, down: bool, control_state: u32) -> String {
    format!("\x1b[{vk};{scan};{unicode};{};{control_state};1_", u8::from(down))
}

/// Key down + key up, exactly what Windows Terminal sends for one press.
fn win32_key_events(vk: u16, scan: u16, unicode: u16, chord: &Chord) -> Vec<u8> {
    let state = control_key_state(chord);
    let mut events = win32_record(vk, scan, unicode, true, state);
    events.push_str(&win32_record(vk, scan, unicode, false, state));
    events.into_bytes()
}

/// Wrap ASCII bytes as keyless character records so conhost hands them to
/// the child untouched (it drops CSI sequences it does not know).
pub fn win32_passthrough(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|byte| win32_record(0, 0, u16::from(*byte), true, 0))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: TerminalKeyboardMode = TerminalKeyboardMode {
        kitty_flags: 0,
        win32_input: false,
        application_cursor: false,
    };
    const KITTY: TerminalKeyboardMode = TerminalKeyboardMode {
        kitty_flags: KITTY_DISAMBIGUATE,
        win32_input: false,
        application_cursor: false,
    };
    const CONPTY: TerminalKeyboardMode = TerminalKeyboardMode {
        kitty_flags: 0,
        win32_input: true,
        application_cursor: false,
    };
    const CONPTY_KITTY: TerminalKeyboardMode = TerminalKeyboardMode {
        kitty_flags: KITTY_DISAMBIGUATE,
        win32_input: true,
        application_cursor: false,
    };

    fn key(chord: &str, mode: TerminalKeyboardMode) -> Vec<u8> {
        encode_key(chord, mode).unwrap_or_else(|error| panic!("{chord}: {error}"))
    }

    #[test]
    fn legacy_matches_the_historical_table() {
        for (chord, expected) in [
            ("esc", &b"\x1b"[..]),
            ("enter", b"\r"),
            ("Return", b"\r"),
            ("tab", b"\t"),
            ("shift+tab", b"\x1b[Z"),
            ("backspace", &[0x7f]),
            ("space", b" "),
            ("up", b"\x1b[A"),
            ("home", b"\x1b[H"),
            ("insert", b"\x1b[2~"),
            ("pagedown", b"\x1b[6~"),
            ("f1", b"\x1bOP"),
            ("f5", b"\x1b[15~"),
            ("f12", b"\x1b[24~"),
            ("ctrl+space", &[0]),
            ("ctrl+c", &[3]),
            ("ctrl+[", &[0x1b]),
            ("ctrl+_", &[0x1f]),
            ("alt+x", b"\x1bx"),
            ("a", b"a"),
            ("+", b"+"),
            ("ctrl+-", &[0x1f]),
            ("shift++", b"+"),
        ] {
            assert_eq!(key(chord, LEGACY), expected, "{chord}");
        }
    }

    #[test]
    fn legacy_modified_enter_is_meta_enter_or_linefeed() {
        // No xterm spelling exists: meta+enter is what readline composers
        // (Claude Code, aider) take as "newline", LF is ctrl+j.
        assert_eq!(key("shift+enter", LEGACY), b"\x1b\r");
        assert_eq!(key("alt+enter", LEGACY), b"\x1b\r");
        assert_eq!(key("shift+alt+enter", LEGACY), b"\x1b\r");
        assert_eq!(key("ctrl+enter", LEGACY), b"\n");
        assert_eq!(key("ctrl+alt+enter", LEGACY), b"\x1b\n");
        assert_eq!(key("ctrl+j", LEGACY), b"\n");
    }

    #[test]
    fn legacy_modifiers_on_functional_keys_use_xterm_parameters() {
        assert_eq!(key("ctrl+left", LEGACY), b"\x1b[1;5D");
        assert_eq!(key("alt+right", LEGACY), b"\x1b[1;3C");
        assert_eq!(key("shift+up", LEGACY), b"\x1b[1;2A");
        assert_eq!(key("ctrl+shift+end", LEGACY), b"\x1b[1;6F");
        assert_eq!(key("ctrl+delete", LEGACY), b"\x1b[3;5~");
        assert_eq!(key("shift+f5", LEGACY), b"\x1b[15;2~");
        assert_eq!(key("alt+f1", LEGACY), b"\x1b[1;3P");
        assert_eq!(key("ctrl+backspace", LEGACY), &[0x08]);
        assert_eq!(key("alt+backspace", LEGACY), b"\x1b\x7f");
        assert_eq!(key("ctrl+alt+d", LEGACY), b"\x1b\x04");
        // Super has no legacy spelling: the rest of the chord still goes out.
        assert_eq!(key("super+left", LEGACY), b"\x1b[D");
    }

    #[test]
    fn application_cursor_mode_switches_unmodified_cursor_keys_to_ss3() {
        let mode = TerminalKeyboardMode {
            application_cursor: true,
            ..LEGACY
        };
        assert_eq!(key("up", mode), b"\x1bOA");
        assert_eq!(key("end", mode), b"\x1bOF");
        assert_eq!(key("ctrl+up", mode), b"\x1b[1;5A");
        assert_eq!(key("up", KITTY), b"\x1b[A", "kitty ignores DECCKM");
    }

    #[test]
    fn kitty_encodes_modified_enter_esc_and_control_chords_as_csi_u() {
        assert_eq!(key("shift+enter", KITTY), b"\x1b[13;2u");
        assert_eq!(key("alt+enter", KITTY), b"\x1b[13;3u");
        assert_eq!(key("ctrl+enter", KITTY), b"\x1b[13;5u");
        assert_eq!(key("shift+alt+enter", KITTY), b"\x1b[13;4u");
        assert_eq!(key("esc", KITTY), b"\x1b[27u");
        assert_eq!(key("ctrl+c", KITTY), b"\x1b[99;5u");
        assert_eq!(key("ctrl+j", KITTY), b"\x1b[106;5u");
        assert_eq!(key("alt+b", KITTY), b"\x1b[98;3u");
        assert_eq!(key("shift+tab", KITTY), b"\x1b[9;2u");
        assert_eq!(key("ctrl+backspace", KITTY), b"\x1b[127;5u");
        assert_eq!(key("ctrl+space", KITTY), b"\x1b[32;5u");
        assert_eq!(key("super+k", KITTY), b"\x1b[107;9u");
        // Unmodified text and Enter/Tab/Backspace keep the legacy bytes.
        assert_eq!(key("enter", KITTY), b"\r");
        assert_eq!(key("tab", KITTY), b"\t");
        assert_eq!(key("backspace", KITTY), &[0x7f]);
        assert_eq!(key("a", KITTY), b"a");
        assert_eq!(key("shift+a", KITTY), b"A");
        assert_eq!(key("ctrl+left", KITTY), b"\x1b[1;5D");
        assert_eq!(key("f5", KITTY), b"\x1b[15~");
    }

    #[test]
    fn kitty_report_all_keys_flag_turns_plain_keys_into_csi_u_too() {
        let mode = TerminalKeyboardMode {
            kitty_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_ALL_KEYS,
            ..KITTY
        };
        assert_eq!(key("enter", mode), b"\x1b[13u");
        assert_eq!(key("a", mode), b"\x1b[97u");
    }

    #[test]
    fn conpty_sends_modified_enter_as_win32_key_records() {
        assert_eq!(
            key("shift+enter", CONPTY),
            b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_"
        );
        assert_eq!(
            key("ctrl+enter", CONPTY),
            b"\x1b[13;28;10;1;8;1_\x1b[13;28;10;0;8;1_"
        );
        assert_eq!(
            key("alt+enter", CONPTY),
            b"\x1b[13;28;13;1;2;1_\x1b[13;28;13;0;2;1_"
        );
        assert_eq!(
            key("ctrl+shift+tab", CONPTY),
            b"\x1b[9;15;9;1;24;1_\x1b[9;15;9;0;24;1_"
        );
        // conhost reads raw LF / TAB / BS / CR / ESC back as ctrl+Enter, Tab,
        // Backspace, Enter and a timed-out escape: those travel as records too.
        assert_eq!(
            key("ctrl+j", CONPTY),
            b"\x1b[74;36;10;1;8;1_\x1b[74;36;10;0;8;1_"
        );
        assert_eq!(
            key("ctrl+shift+i", CONPTY),
            b"\x1b[73;23;9;1;24;1_\x1b[73;23;9;0;24;1_"
        );
        assert_eq!(
            key("esc", CONPTY),
            b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_"
        );
        // Everything conhost already understands stays as plain VT.
        assert_eq!(key("enter", CONPTY), b"\r");
        assert_eq!(key("ctrl+c", CONPTY), &[3]);
        assert_eq!(key("ctrl+alt+j", CONPTY), b"\x1b\n");
        assert_eq!(key("alt+esc", CONPTY), b"\x1b\x1b");
        assert_eq!(key("ctrl+left", CONPTY), b"\x1b[1;5D");
        assert_eq!(key("shift+tab", CONPTY), b"\x1b[Z");
    }

    #[test]
    fn conpty_wraps_kitty_sequences_as_character_records() {
        let expected: Vec<u8> = b"\x1b[13;2u"
            .iter()
            .map(|byte| format!("\x1b[0;0;{byte};1;0;1_"))
            .collect::<String>()
            .into_bytes();
        assert_eq!(key("shift+enter", CONPTY_KITTY), expected);
        assert_eq!(key("enter", CONPTY_KITTY), b"\r");
        assert_eq!(key("ctrl+left", CONPTY_KITTY), b"\x1b[1;5D");
        assert_eq!(
            kitty_flags_response(1, true),
            win32_passthrough(b"\x1b[?1u")
        );
        assert_eq!(kitty_flags_response(0, false), b"\x1b[?0u");
    }

    #[test]
    fn rejects_unknown_chords() {
        for chord in ["", "ctrl+not-a-key", "bogus+enter", "f25", "ctrl+1", "ctrl+"] {
            assert!(encode_key(chord, LEGACY).is_err(), "{chord:?}");
        }
        assert_eq!(
            encode_key("ctrl+not-a-key", LEGACY).unwrap_err(),
            InvalidKey("ctrl+not-a-key".to_owned())
        );
    }

    #[test]
    fn kitty_stacks_are_per_screen_and_drop_with_the_alternate_screen() {
        let mut state = KeyboardState::default();
        assert_eq!(state.flags(false), 0);
        state.push(true, KITTY_DISAMBIGUATE | 0b10);
        assert_eq!(state.flags(true), 0b11);
        assert_eq!(state.flags(false), 0, "main screen untouched");
        state.push(true, 0b1000);
        assert_eq!(state.flags(true), 0b1000);
        state.pop(true, 1);
        assert_eq!(state.flags(true), 0b11);
        state.pop(true, 0);
        assert_eq!(state.flags(true), 0, "count 0 pops one");
        state.push(false, 1);
        state.set(false, 0b100, 2);
        assert_eq!(state.flags(false), 0b101);
        state.set(false, 1, 3);
        assert_eq!(state.flags(false), 0b100);
        state.set(true, 0b1, 1);
        assert_eq!(state.flags(true), 1, "set on an empty stack edits the base entry");
        state.leave_alternate_screen();
        assert_eq!(state.flags(true), 0);
        assert_eq!(state.flags(false), 0b100);
        state.reset();
        assert_eq!(state.flags(false), 0);
        // Pushing past the limit evicts the oldest entry instead of failing.
        for flags in 0..20u8 {
            state.push(false, flags);
        }
        assert_eq!(state.flags(false), 19 & KITTY_FLAG_MASK);
        state.pop(false, 200);
        assert_eq!(state.flags(false), 0);
    }
}
