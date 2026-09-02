use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use starcil_config::{Key, KeyChord, MetaModifier, Modifiers, NamedKey};

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
