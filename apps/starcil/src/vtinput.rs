//! Incremental VT keyboard parser for the Windows console's keyless UTF-16
//! records. Ordinary Windows KEY_EVENTs still use crossterm's layout handling.
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Key(KeyEvent),
    KittyFlags(u16),
    DeviceAttributes,
    SgrMouse { button: u16, x: i16, y: i16, pressed: bool },
}

#[derive(Default)]
pub struct Parser {
    pending: String,
}

impl Parser {
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn push(&mut self, ch: char) -> Vec<Decoded> {
        if self.pending.is_empty() {
            if ch == '\x1b' {
                self.pending.push(ch);
                return Vec::new();
            }
            return vec![Decoded::Key(plain_key(ch))];
        }
        if ch == '\x1b' {
            let out = self.expire();
            self.pending.push(ch);
            return out;
        }
        if self.pending == "\x1b" && !matches!(ch, '[' | 'O') {
            self.pending.clear();
            let mut key = plain_key(ch);
            key.modifiers |= KeyModifiers::ALT;
            return vec![Decoded::Key(key)];
        }
        self.pending.push(ch);
        if self.pending.len() == 2 {
            return Vec::new();
        }
        if !ch.is_ascii() || ch.is_control() || self.pending.len() > 128 {
            self.pending.clear();
            return Vec::new();
        }
        if ('@'..='~').contains(&ch) {
            let sequence = std::mem::take(&mut self.pending);
            return parse_sequence(&sequence).into_iter().collect();
        }
        Vec::new()
    }

    /// A lone Escape must not wait for another key forever. Incomplete CSI is
    /// discarded, never turned into text or Enter in an agent's composer.
    pub fn expire(&mut self) -> Vec<Decoded> {
        let esc = self.pending == "\x1b";
        self.pending.clear();
        if esc { vec![Decoded::Key(plain_key('\x1b'))] } else { Vec::new() }
    }
}

fn plain_key(ch: char) -> KeyEvent {
    let (code, mods) = match ch {
        '\r' => (KeyCode::Enter, KeyModifiers::NONE),
        '\n' => (KeyCode::Char('j'), KeyModifiers::CONTROL),
        '\t' => (KeyCode::Tab, KeyModifiers::NONE),
        '\x08' | '\x7f' => (KeyCode::Backspace, KeyModifiers::NONE),
        '\x1b' => (KeyCode::Esc, KeyModifiers::NONE),
        '\0' => (KeyCode::Char(' '), KeyModifiers::CONTROL),
        '\x01'..='\x1a' => (KeyCode::Char(char::from_u32(ch as u32 + 96).unwrap()), KeyModifiers::CONTROL),
        '\x1c'..='\x1f' => (KeyCode::Char(char::from_u32(ch as u32 + 64).unwrap()), KeyModifiers::CONTROL),
        ch => (KeyCode::Char(ch), KeyModifiers::NONE),
    };
    KeyEvent::new(code, mods)
}

fn key_code(code: u32) -> Option<KeyCode> {
    Some(match code {
        27 | 57344 => KeyCode::Esc,
        13 | 57345 => KeyCode::Enter,
        9 | 57346 => KeyCode::Tab,
        127 | 57347 => KeyCode::Backspace,
        57348 => KeyCode::Insert,
        57349 => KeyCode::Delete,
        57350 => KeyCode::Left,
        57351 => KeyCode::Right,
        57352 => KeyCode::Up,
        57353 => KeyCode::Down,
        57354 => KeyCode::PageUp,
        57355 => KeyCode::PageDown,
        57356 => KeyCode::Home,
        57357 => KeyCode::End,
        57364..=57387 => KeyCode::F((code - 57363) as u8),
        // Unsupported functional keys are not printable private-use text.
        57358..=63743 => return None,
        value => KeyCode::Char(char::from_u32(value)?),
    })
}

fn modifiers(field: Option<&str>) -> Option<(KeyModifiers, KeyEventKind, KeyEventState)> {
    let mut fields = field.unwrap_or("1").split(':');
    let value: u16 = fields.next()?.parse().ok()?;
    let bits = value.checked_sub(1)?;
    if bits > 255 { return None; }
    let kind = match fields.next().unwrap_or("1") {
        "1" => KeyEventKind::Press,
        "2" => KeyEventKind::Repeat,
        "3" => KeyEventKind::Release,
        _ => return None,
    };
    if fields.next().is_some() { return None; }
    let mut mods = KeyModifiers::NONE;
    for (bit, modifier) in [(1, KeyModifiers::SHIFT), (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL), (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER), (32, KeyModifiers::META)] {
        if bits & bit != 0 { mods |= modifier; }
    }
    let mut state = KeyEventState::NONE;
    if bits & 64 != 0 { state |= KeyEventState::CAPS_LOCK; }
    if bits & 128 != 0 { state |= KeyEventState::NUM_LOCK; }
    Some((mods, kind, state))
}

fn parse_sequence(sequence: &str) -> Option<Decoded> {
    let body = sequence.strip_prefix("\x1b[").or_else(|| sequence.strip_prefix("\x1bO"))?;
    let (params, final_char) = body.split_at(body.len().checked_sub(1)?);
    if let Some(mouse) = params.strip_prefix('<') {
        if !matches!(final_char, "M" | "m") { return None; }
        let mut fields = mouse.split(';');
        let button: u16 = fields.next()?.parse().ok()?;
        let x: i16 = fields.next()?.parse().ok()?;
        let y: i16 = fields.next()?.parse().ok()?;
        if fields.next().is_some() || button > 127 || x < 1 || y < 1 { return None; }
        return Some(Decoded::SgrMouse { button, x: x - 1, y: y - 1, pressed: final_char == "M" });
    }
    if let Some(reply) = params.strip_prefix('?') {
        return match final_char {
            "u" => Some(Decoded::KittyFlags(reply.parse().ok()?)),
            "c" if !reply.is_empty() && reply.split(';').all(|p| p.parse::<u16>().is_ok()) => Some(Decoded::DeviceAttributes),
            _ => None,
        };
    }
    let fields: Vec<&str> = params.split(';').collect();
    let (mut mods, kind, state) = modifiers(fields.get(1).copied())?;
    let code = match final_char {
        "u" => {
            if fields.len() > 3 { return None; }
            let codes: Vec<&str> = fields[0].split(':').collect();
            if codes.len() > 3 { return None; }
            let mut value = codes[0].parse().ok()?;
            if mods.contains(KeyModifiers::SHIFT) {
                if let Some(shifted) = codes.get(1).filter(|s| !s.is_empty()) {
                    value = shifted.parse().ok()?;
                }
            }
            key_code(value)?
        }
        "~" => match fields[0] {
            "1" | "7" => KeyCode::Home,
            "2" => KeyCode::Insert,
            "3" => KeyCode::Delete,
            "4" | "8" => KeyCode::End,
            "5" => KeyCode::PageUp,
            "6" => KeyCode::PageDown,
            "11" => KeyCode::F(1), "12" => KeyCode::F(2), "13" => KeyCode::F(3),
            "14" => KeyCode::F(4), "15" => KeyCode::F(5), "17" => KeyCode::F(6),
            "18" => KeyCode::F(7), "19" => KeyCode::F(8), "20" => KeyCode::F(9),
            "21" => KeyCode::F(10), "23" => KeyCode::F(11), "24" => KeyCode::F(12),
            _ => return None,
        },
        _ => {
            if !matches!(fields[0], "" | "1") || fields.len() > 2 { return None; }
            match final_char {
                "A" => KeyCode::Up, "B" => KeyCode::Down, "C" => KeyCode::Right, "D" => KeyCode::Left,
                "H" => KeyCode::Home, "F" => KeyCode::End,
                "P" => KeyCode::F(1), "Q" => KeyCode::F(2), "R" => KeyCode::F(3), "S" => KeyCode::F(4),
                "Z" => { mods |= KeyModifiers::SHIFT; KeyCode::BackTab },
                _ => return None,
            }
        }
    };
    Some(Decoded::Key(KeyEvent { code, modifiers: mods, kind, state }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(input: &str) -> Vec<Decoded> {
        let mut parser = Parser::default();
        input.chars().flat_map(|ch| parser.push(ch)).collect()
    }

    #[test]
    fn fragmented_vt_keys_and_legacy_text() {
        for (input, code, mods) in [
            ("\x1b[13;2u", KeyCode::Enter, KeyModifiers::SHIFT),
            ("\x1b[13;3u", KeyCode::Enter, KeyModifiers::ALT),
            ("\x1b[106;5u", KeyCode::Char('j'), KeyModifiers::CONTROL),
            ("\x1b[1;5D", KeyCode::Left, KeyModifiers::CONTROL),
            ("\x1bOA", KeyCode::Up, KeyModifiers::NONE),
            ("\x1bOP", KeyCode::F(1), KeyModifiers::NONE),
            ("\x1b[3;2~", KeyCode::Delete, KeyModifiers::SHIFT),
            ("\x1b[Z", KeyCode::BackTab, KeyModifiers::SHIFT),
            ("\x1b\r", KeyCode::Enter, KeyModifiers::ALT),
            ("\x1bx", KeyCode::Char('x'), KeyModifiers::ALT),
            ("\r", KeyCode::Enter, KeyModifiers::NONE),
            ("\n", KeyCode::Char('j'), KeyModifiers::CONTROL),
            ("\x03", KeyCode::Char('c'), KeyModifiers::CONTROL),
            ("\t", KeyCode::Tab, KeyModifiers::NONE),
            ("\x7f", KeyCode::Backspace, KeyModifiers::NONE),
            ("ñ", KeyCode::Char('ñ'), KeyModifiers::NONE),
            ("🚀", KeyCode::Char('🚀'), KeyModifiers::NONE),
            ("\x1b[97:65;2u", KeyCode::Char('A'), KeyModifiers::SHIFT),
            ("\x1b[57376u", KeyCode::F(13), KeyModifiers::NONE),
        ] {
            assert_eq!(decode(input), vec![Decoded::Key(KeyEvent::new(code, mods))], "{input:?}");
        }
    }

    #[test]
    fn kitty_events_modifiers_and_replies() {
        for (suffix, kind) in [("1", KeyEventKind::Press), ("2", KeyEventKind::Repeat), ("3", KeyEventKind::Release)] {
            let input = format!("\x1b[106;5:{suffix}u");
            assert_eq!(decode(&input), vec![Decoded::Key(KeyEvent::new_with_kind(KeyCode::Char('j'), KeyModifiers::CONTROL, kind))]);
        }
        let (mods, _, state) = modifiers(Some("256")).unwrap();
        assert!(mods.contains(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META));
        assert!(state.contains(KeyEventState::CAPS_LOCK | KeyEventState::NUM_LOCK));
        assert_eq!(decode("\x1b[?0u\x1b[?1;2c"), vec![Decoded::KittyFlags(0), Decoded::DeviceAttributes]);
        assert_eq!(decode("\x1b[?1;2c\x1b[?0u"), vec![Decoded::DeviceAttributes, Decoded::KittyFlags(0)]);
    }

    #[test]
    fn sgr_mouse_reports_survive_fragmentation_and_validate_coordinates() {
        for (input, button, pressed) in [("\x1b[<2;40;10M", 2, true), ("\x1b[<0;40;10m", 0, false),
            ("\x1b[<32;40;10M", 32, true), ("\x1b[<65;40;10M", 65, true)] {
            assert_eq!(decode(input), vec![Decoded::SgrMouse { button, x: 39, y: 9, pressed }]);
        }
        for input in ["\x1b[<0;0;1M", "\x1b[<0;1;32768M", "\x1b[<128;1;1M", "\x1b[<0;1;1;1M"] {
            assert!(decode(input).is_empty(), "{input:?}");
        }
    }

    #[test]
    fn escape_timeout_invalid_sequences_and_recovery() {
        let mut parser = Parser::default();
        assert!(parser.push('\x1b').is_empty());
        assert!(parser.is_pending());
        assert_eq!(parser.expire(), vec![Decoded::Key(plain_key('\x1b'))]);
        assert!(!parser.is_pending());
        for input in ["\x1b[13;0u", "\x1b[13;257u", "\x1b[13;5:4u", "\x1b[1114112u", "\x1b[55296u", "\x1b[99999~", "\x1b[?;c", "\x1b[200~", "\x1b[?9001h"] {
            assert!(decode(input).is_empty(), "{input:?}");
        }
        for ch in "\x1b[13;".chars() { assert!(parser.push(ch).is_empty()); }
        assert!(parser.expire().is_empty());
        assert_eq!(parser.push('x'), vec![Decoded::Key(plain_key('x'))]);
        assert_eq!(decode("\x1b[13;\x1b[106;5u"), decode("\x1b[106;5u"));
    }
}
