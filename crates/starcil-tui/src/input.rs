use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use starcil_config::{Key, KeyChord, MetaModifier, Modifiers, NamedKey};

/// Normalize only at the pane boundary, after modal and shell-composer routing.
/// ConPTY synthesizes Ctrl+Enter for a terminal's raw LF (Ctrl+J). A physical
/// Ctrl+Enter is indistinguishable; on Windows both become the agent newline key.
pub fn pane_key_chord(chord: KeyChord) -> KeyChord {
    normalize_pane_chord(chord, cfg!(windows))
}

fn normalize_pane_chord(mut chord: KeyChord, windows: bool) -> KeyChord {
    if windows
        && chord.key == Key::Named(NamedKey::Enter)
        && chord.mods.ctrl
        && !chord.mods.alt
        && !chord.mods.shift
        && chord.mods.meta.is_none()
    {
        chord.key = Key::Character('j');
    }
    chord
}

pub fn key_event_to_chord(event: &KeyEvent) -> Option<KeyChord> {
    if event.kind == KeyEventKind::Release {
        return None;
    }

    let mut mods = Modifiers {
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
        meta: event
            .modifiers
            .contains(KeyModifiers::SUPER)
            .then_some(MetaModifier::Super),
    };

    let key = match event.code {
        KeyCode::Char(character) => normalize_character(character, &mut mods),
        KeyCode::F(number) if (1..=24).contains(&number) => Key::Function(number),
        KeyCode::Enter => Key::Named(NamedKey::Enter),
        KeyCode::Tab => Key::Named(NamedKey::Tab),
        KeyCode::BackTab => {
            mods.shift = true;
            Key::Named(NamedKey::Tab)
        }
        KeyCode::Esc => Key::Named(NamedKey::Esc),
        KeyCode::Left => Key::Named(NamedKey::Left),
        KeyCode::Right => Key::Named(NamedKey::Right),
        KeyCode::Up => Key::Named(NamedKey::Up),
        KeyCode::Down => Key::Named(NamedKey::Down),
        KeyCode::Backspace => Key::Named(NamedKey::Backspace),
        KeyCode::Delete => Key::Named(NamedKey::Delete),
        KeyCode::Home => Key::Named(NamedKey::Home),
        KeyCode::End => Key::Named(NamedKey::End),
        KeyCode::PageUp => Key::Named(NamedKey::PageUp),
        KeyCode::PageDown => Key::Named(NamedKey::PageDown),
        _ => return None,
    };

    Some(KeyChord {
        mods,
        key,
        requires_prefix: false,
    })
}

fn normalize_character(character: char, mods: &mut Modifiers) -> Key {
    if mods.shift && (character.is_ascii_uppercase() || character.is_ascii_punctuation()) {
        // Crossterm has already resolved Shift into the printable glyph. Keeping
        // the modifier would turn bindings such as `prefix+?` into
        // `prefix+shift+?`, which is a different chord in the config keymap.
        mods.shift = false;
    }

    match character {
        ' ' => Key::Named(NamedKey::Space),
        '-' | '_' => Key::Named(NamedKey::Minus),
        ',' | '<' => Key::Named(NamedKey::Comma),
        '&' => Key::Named(NamedKey::Ampersand),
        '+' => Key::Named(NamedKey::Plus),
        '`' | '~' => Key::Named(NamedKey::Backtick),
        character if character.is_ascii_alphabetic() => {
            Key::Character(character.to_ascii_lowercase())
        }
        character => Key::Character(character),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_newline_mapping_is_windows_only_and_preserves_other_modifiers() {
        for (modifiers, windows_chord, unix_chord) in [
            (KeyModifiers::NONE, "enter", "enter"),
            (KeyModifiers::CONTROL, "ctrl+j", "ctrl+enter"),
            (KeyModifiers::SHIFT, "shift+enter", "shift+enter"),
            (KeyModifiers::ALT, "alt+enter", "alt+enter"),
            (KeyModifiers::CONTROL | KeyModifiers::SHIFT, "ctrl+shift+enter", "ctrl+shift+enter"),
            (KeyModifiers::CONTROL | KeyModifiers::ALT, "ctrl+alt+enter", "ctrl+alt+enter"),
        ] {
            let event = KeyEvent::new(KeyCode::Enter, modifiers);
            let chord = key_event_to_chord(&event).unwrap();
            assert_eq!(normalize_pane_chord(chord.clone(), true).to_string(), windows_chord);
            assert_eq!(normalize_pane_chord(chord, false).to_string(), unix_chord);
        }
        let ctrl_j = key_event_to_chord(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(normalize_pane_chord(ctrl_j, true).to_string(), "ctrl+j");
    }

    #[test]
    fn normalizes_prefix_letters_and_printable_punctuation() {
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(key_event_to_chord(&prefix).unwrap().to_string(), "ctrl+b");

        for (character, expected) in [
            ('?', "?"),
            ('+', "plus"),
            ('_', "minus"),
            ('{', "{"),
            ('}', "}"),
            ('A', "a"),
            ('Z', "z"),
        ] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::SHIFT);
            let chord = key_event_to_chord(&event).expect("printable key chord");
            assert_eq!(chord.to_string(), expected, "character {character:?}");
            assert!(!chord.mods.shift, "Shift survived for {character:?}");
        }
    }
}
