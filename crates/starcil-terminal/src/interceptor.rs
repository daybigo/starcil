#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryKind {
    CursorPosition,
    PrimaryDeviceAttributes,
    SecondaryDeviceAttributes,
    DeviceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterceptEvent {
    Query(QueryKind),
    BracketedPaste(bool),
    Title(Option<String>),
    /// The shell announced its working directory: OSC `9;9;<path>` (ConEmu /
    /// Windows Terminal, what Starcil's own PowerShell prompt hook emits) or
    /// OSC `7;file://host/path` (xterm, iTerm2, most unix prompt setups).
    Cwd(String),
}

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

            self.candidate.push(byte);
            if self.candidate.starts_with(b"\x1b]") {
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

            if let Some(event) = exact_event(&self.candidate) {
                events.push((index + 1, event));
                self.candidate.clear();
            } else if !is_known_prefix(&self.candidate) {
                let restart = byte == 0x1b;
                self.candidate.clear();
                if restart {
                    self.candidate.push(byte);
                }
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

fn exact_event(candidate: &[u8]) -> Option<InterceptEvent> {
    match candidate {
        b"\x1b[6n" => Some(InterceptEvent::Query(QueryKind::CursorPosition)),
        b"\x1b[c" => Some(InterceptEvent::Query(QueryKind::PrimaryDeviceAttributes)),
        b"\x1b[>c" => Some(InterceptEvent::Query(QueryKind::SecondaryDeviceAttributes)),
        b"\x1b[5n" => Some(InterceptEvent::Query(QueryKind::DeviceStatus)),
        b"\x1b[?2004h" => Some(InterceptEvent::BracketedPaste(true)),
        b"\x1b[?2004l" => Some(InterceptEvent::BracketedPaste(false)),
        _ => None,
    }
}

fn is_known_prefix(candidate: &[u8]) -> bool {
    const PATTERNS: [&[u8]; 6] = [
        b"\x1b[6n",
        b"\x1b[c",
        b"\x1b[>c",
        b"\x1b[5n",
        b"\x1b[?2004h",
        b"\x1b[?2004l",
    ];
    candidate == b"\x1b]"
        || PATTERNS
        .iter()
        .any(|pattern| pattern.starts_with(candidate))
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
}
