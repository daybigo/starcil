#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryKind {
    CursorPosition,
    PrimaryDeviceAttributes,
    SecondaryDeviceAttributes,
    DeviceStatus,
    /// Kitty keyboard protocol `CSI ? u`: the program asks which flags are in
    /// force (crossterm's `supports_keyboard_enhancement`, Claude Code's
    /// startup probe). Answering is what lets it switch shift+enter on.
    KeyboardFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterceptEvent {
    Query(QueryKind),
    BracketedPaste(bool),
    /// ConPTY's `CSI ? 9001 h/l`: win32-input-mode wanted on the PTY input.
    Win32Input(bool),
    /// Kitty `CSI > flags u` / `CSI < n u` / `CSI = flags ; mode u`.
    KittyPush(u8),
    KittyPop(u8),
    KittySet { flags: u8, mode: u8 },
    /// DECSET/DECRST 1049, 1047, 47.
    AlternateScreen(bool),
    /// RIS (`ESC c`).
    Reset,
    Title(Option<String>),
    /// The shell announced its working directory: OSC `9;9;<path>` (ConEmu /
    /// Windows Terminal, what Starcil's own PowerShell prompt hook emits) or
    /// OSC `7;file://host/path` (xterm, iTerm2, most unix prompt setups).
    Cwd(String),
}

/// Longest CSI we keep buffering (ESC `[` included) before giving up on it.
const MAX_CSI_LEN: usize = 64;

#[derive(Debug, Default)]
pub(crate) struct EscapeInterceptor {
    candidate: Vec<u8>,
}

impl EscapeInterceptor {
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> Vec<(usize, InterceptEvent)> {
        let mut events = Vec::new();
        for (index, byte) in bytes.iter().copied().enumerate() {
            if self.candidate.is_empty() {
                if byte == 0x1b {
                    self.candidate.push(byte);
                }
                continue;
            }
            if self.candidate.len() == 1 {
                match byte {
                    b'[' | b']' => self.candidate.push(byte),
                    b'c' => {
                        events.push((index + 1, InterceptEvent::Reset));
                        self.candidate.clear();
                    }
                    // ESC ESC: the second one is the candidate now.
                    0x1b => {}
                    _ => self.candidate.clear(),
                }
                continue;
            }

            self.candidate.push(byte);
            if self.candidate[1] == b']' {
                if osc_complete(&self.candidate) {
                    if let Some(title) = osc_title(&self.candidate) {
                        events.push((index + 1, InterceptEvent::Title(title)));
                    } else if let Some(cwd) = osc_cwd(&self.candidate) {
                        events.push((index + 1, InterceptEvent::Cwd(cwd)));
                    }
                    self.candidate.clear();
                } else if self.candidate.len() > 4096 {
                    self.candidate.clear();
                }
                continue;
            }

            // CSI: parameter bytes 0x30–0x3F, intermediates 0x20–0x2F, one
            // final byte 0x40–0x7E. Anything else aborts the sequence.
            match byte {
                0x40..=0x7e => {
                    if let Some(event) = classify_csi(&self.candidate[2..]) {
                        events.push((index + 1, event));
                    }
                    self.candidate.clear();
                }
                0x20..=0x3f if self.candidate.len() <= MAX_CSI_LEN => {}
                0x1b => {
                    self.candidate.clear();
                    self.candidate.push(0x1b);
                }
                _ => self.candidate.clear(),
            }
        }
        events
    }
}

fn osc_complete(candidate: &[u8]) -> bool {
    candidate.last() == Some(&0x07) || candidate.ends_with(b"\x1b\\")
}

fn osc_title(candidate: &[u8]) -> Option<Option<String>> {
    let terminator_len = if candidate.last() == Some(&0x07) { 1 } else { 2 };
    let body = candidate.get(2..candidate.len().checked_sub(terminator_len)?)?;
    let separator = body.iter().position(|byte| *byte == b';')?;
    let code = &body[..separator];
    if code != b"0" && code != b"2" {
        return None;
    }

    let decoded = String::from_utf8_lossy(&body[separator + 1..]);
    let normalized: String = decoded
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let normalized: String = normalized.trim().chars().take(80).collect();
    Some((!normalized.is_empty()).then_some(normalized))
}

/// OSC 9;9 (raw path) and OSC 7 (`file://` URL) working-directory reports.
fn osc_cwd(candidate: &[u8]) -> Option<String> {
    let terminator_len = if candidate.last() == Some(&0x07) { 1 } else { 2 };
    let body = candidate.get(2..candidate.len().checked_sub(terminator_len)?)?;
    let separator = body.iter().position(|byte| *byte == b';')?;
    let code = &body[..separator];
    let payload = String::from_utf8_lossy(&body[separator + 1..]);
    let path = match code {
        b"9" => {
            // ConEmu family: `9;9;<path>`; other `9;<n>` codes are progress
            // bars and notifications.
            let rest = payload.strip_prefix("9;")?;
            rest.trim_matches('"').to_owned()
        }
        b"7" => file_url_to_path(payload.trim())?,
        _ => return None,
    };
    let path: String = path.chars().filter(|character| !character.is_control()).collect();
    let path = path.trim();
    (!path.is_empty() && path.len() <= 4096).then(|| path.to_owned())
}

/// `file://host/C:/dir` → `C:\dir`, `file:///home/u` → `/home/u`; percent
/// escapes decoded.
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(slash) => &rest[slash..],
        None => return None,
    };
    let decoded = percent_decode(path);
    let bytes = decoded.as_bytes();
    // `/C:/...` is a Windows drive path in URL clothing.
    let windows_drive = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':';
    Some(if windows_drive {
        decoded[1..].replace('/', "\\")
    } else {
        decoded
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &text[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `body` is everything after `ESC [`, final byte included.
fn classify_csi(body: &[u8]) -> Option<InterceptEvent> {
    match body {
        b"6n" => return Some(InterceptEvent::Query(QueryKind::CursorPosition)),
        b"c" | b"0c" => return Some(InterceptEvent::Query(QueryKind::PrimaryDeviceAttributes)),
        b">c" | b">0c" => {
            return Some(InterceptEvent::Query(QueryKind::SecondaryDeviceAttributes))
        }
        b"5n" => return Some(InterceptEvent::Query(QueryKind::DeviceStatus)),
        b"?2004h" => return Some(InterceptEvent::BracketedPaste(true)),
        b"?2004l" => return Some(InterceptEvent::BracketedPaste(false)),
        b"?9001h" => return Some(InterceptEvent::Win32Input(true)),
        b"?9001l" => return Some(InterceptEvent::Win32Input(false)),
        b"?1049h" | b"?1047h" | b"?47h" => return Some(InterceptEvent::AlternateScreen(true)),
        b"?1049l" | b"?1047l" | b"?47l" => return Some(InterceptEvent::AlternateScreen(false)),
        b"?u" => return Some(InterceptEvent::Query(QueryKind::KeyboardFlags)),
        _ => {}
    }
    let (final_byte, params) = body.split_last()?;
    if *final_byte != b'u' {
        return None;
    }
    let (marker, rest) = params.split_first()?;
    match marker {
        b'>' => Some(InterceptEvent::KittyPush(decimal(rest).unwrap_or(0))),
        b'<' => Some(InterceptEvent::KittyPop(decimal(rest).unwrap_or(1))),
        b'=' => {
            let mut fields = rest.split(|byte| *byte == b';');
            let flags = fields.next().and_then(decimal).unwrap_or(0);
            let mode = fields.next().and_then(decimal).unwrap_or(1);
            Some(InterceptEvent::KittySet { flags, mode })
        }
        _ => None,
    }
}

/// A decimal parameter; `None` when empty or malformed, saturating on overflow.
fn decimal(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        digits
            .iter()
            .fold(0u8, |value, digit| value.saturating_mul(10).saturating_add(digit - b'0')),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_queries_split_across_chunks() {
        let mut interceptor = EscapeInterceptor::default();
        assert!(interceptor.scan(b"text\x1b[").is_empty());
        assert_eq!(
            interceptor.scan(b"6n"),
            vec![(2, InterceptEvent::Query(QueryKind::CursorPosition))]
        );
    }

    #[test]
    fn tracks_bracketed_paste_enable_and_disable() {
        let mut interceptor = EscapeInterceptor::default();
        let events = interceptor.scan(b"\x1b[?2004hready\x1b[?2004l");
        assert_eq!(
            events,
            vec![
                (8, InterceptEvent::BracketedPaste(true)),
                (21, InterceptEvent::BracketedPaste(false)),
            ]
        );
    }

    #[test]
    fn captures_split_osc_zero_and_two_titles() {
        let mut interceptor = EscapeInterceptor::default();
        assert!(interceptor.scan(b"\x1b]0;  Build").is_empty());
        assert_eq!(
            interceptor.scan(b"\x07"),
            vec![(1, InterceptEvent::Title(Some("Build".to_owned())))]
        );
        assert_eq!(
            interceptor.scan(b"\x1b]2;Deploy\x1b\\"),
            vec![(12, InterceptEvent::Title(Some("Deploy".to_owned())))]
        );
    }

    #[test]
    fn captures_working_directory_reports() {
        let mut interceptor = EscapeInterceptor::default();
        let mut one = |bytes: &[u8]| {
            let events = interceptor.scan(bytes);
            assert!(events.len() <= 1, "{events:?}");
            events.into_iter().next().map(|(at, event)| {
                assert_eq!(at, bytes.len());
                event
            })
        };
        assert_eq!(
            one(b"\x1b]9;9;C:\\Users\\cesar\\dev\x07"),
            Some(InterceptEvent::Cwd("C:\\Users\\cesar\\dev".to_owned()))
        );
        // Windows Terminal quotes the path; the quotes are not part of it.
        assert_eq!(
            one(b"\x1b]9;9;\"D:\\My Dir\"\x1b\\"),
            Some(InterceptEvent::Cwd("D:\\My Dir".to_owned()))
        );
        assert_eq!(
            one(b"\x1b]7;file://laptop/home/cesar/my%20repo\x07"),
            Some(InterceptEvent::Cwd("/home/cesar/my repo".to_owned()))
        );
        assert_eq!(
            one(b"\x1b]7;file:///C:/dev/Starcil\x07"),
            Some(InterceptEvent::Cwd("C:\\dev\\Starcil".to_owned()))
        );
        // Progress bars (9;4) and empty reports are not directories.
        assert_eq!(one(b"\x1b]9;4;3;50\x07"), None);
        assert_eq!(one(b"\x1b]9;9;\x07"), None);
    }

    #[test]
    fn empty_title_clears_and_other_osc_is_ignored() {
        let mut interceptor = EscapeInterceptor::default();
        assert_eq!(
            interceptor.scan(b"\x1b]2; \x07"),
            vec![(6, InterceptEvent::Title(None))]
        );
        assert!(interceptor.scan(b"\x1b]52;c;ignored\x07").is_empty());
    }

    #[test]
    fn tracks_kitty_keyboard_protocol_and_conpty_win32_input_requests() {
        let mut interceptor = EscapeInterceptor::default();
        // crossterm's support probe: flags query immediately followed by DA1.
        assert_eq!(
            interceptor.scan(b"\x1b[?u\x1b[c"),
            vec![
                (4, InterceptEvent::Query(QueryKind::KeyboardFlags)),
                (7, InterceptEvent::Query(QueryKind::PrimaryDeviceAttributes)),
            ]
        );
        assert_eq!(
            interceptor.scan(b"\x1b[>1u\x1b[>u\x1b[<u\x1b[<2u\x1b[=5;2u\x1b[=3u"),
            vec![
                (5, InterceptEvent::KittyPush(1)),
                (9, InterceptEvent::KittyPush(0)),
                (13, InterceptEvent::KittyPop(1)),
                (18, InterceptEvent::KittyPop(2)),
                (25, InterceptEvent::KittySet { flags: 5, mode: 2 }),
                (30, InterceptEvent::KittySet { flags: 3, mode: 1 }),
            ]
        );
        assert_eq!(
            interceptor.scan(b"\x1b[?9001h\x1b[?9001l\x1b[?1049h\x1b[?1049l\x1bc"),
            vec![
                (8, InterceptEvent::Win32Input(true)),
                (16, InterceptEvent::Win32Input(false)),
                (24, InterceptEvent::AlternateScreen(true)),
                (32, InterceptEvent::AlternateScreen(false)),
                (34, InterceptEvent::Reset),
            ]
        );
        // Split anywhere inside the sequence.
        assert!(interceptor.scan(b"\x1b[>").is_empty());
        assert!(interceptor.scan(b"1").is_empty());
        assert_eq!(interceptor.scan(b"u"), vec![(1, InterceptEvent::KittyPush(1))]);
    }

    #[test]
    fn other_csi_sequences_are_ignored_and_never_swallow_a_following_query() {
        let mut interceptor = EscapeInterceptor::default();
        assert!(interceptor
            .scan(b"\x1b[38;2;10;20;30m\x1b[?25l\x1b[2J\x1b[1;1H\x1b[?1000h\x1b[27;2;13~x")
            .is_empty());
        assert_eq!(
            interceptor.scan(b"\x1b[0m\x1b[6n"),
            vec![(8, InterceptEvent::Query(QueryKind::CursorPosition))]
        );
        // An overlong or broken CSI is abandoned; the next ESC starts fresh.
        let long = [b"\x1b[".to_vec(), vec![b'1'; 80], b"m".to_vec()].concat();
        assert!(interceptor.scan(&long).is_empty());
        assert_eq!(
            interceptor.scan(b"\x1b[12\n\x1b[?u"),
            vec![(9, InterceptEvent::Query(QueryKind::KeyboardFlags))]
        );
        assert_eq!(
            interceptor.scan(b"\x1b\x1b[?u"),
            vec![(5, InterceptEvent::Query(QueryKind::KeyboardFlags))]
        );
    }
}
