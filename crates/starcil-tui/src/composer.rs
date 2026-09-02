//! The in-pane composer's line editor: cursor editing, shell-style history
//! (seeded from the shell's own history file) and Tab completion against the
//! pane's live cwd. Pure logic — `App` owns the state and routes the keys.
//!
//! The composer never round-trips through the PTY for any of this: the shell
//! prompt above stays empty until Enter, so what the row shows is exactly
//! what runs (a PSReadLine "mirror" would drift the moment a completion
//! rewrote the line).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Line editor
// ---------------------------------------------------------------------------

/// One line of input with a cursor, addressed in chars (what the row shows),
/// never bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    chars: Vec<char>,
    cursor: usize,
}

impl LineEditor {
    pub fn from_text(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Replace the whole line; the cursor lands at the end.
    pub fn set_text(&mut self, text: &str) {
        *self = Self::from_text(text);
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.chars.len());
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn insert_char(&mut self, character: char) {
        self.chars.insert(self.cursor, character);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        for character in text.chars() {
            self.insert_char(character);
        }
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.chars.remove(self.cursor);
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.chars.remove(self.cursor);
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Start of the previous word (ctrl+left / alt+b).
    pub fn word_left(&mut self) {
        self.cursor = self.previous_word_start();
    }

    /// Start of the next word (ctrl+right / alt+f), PSReadLine's `NextWord`.
    pub fn word_right(&mut self) {
        self.cursor = self.next_word_start();
    }

    /// ctrl+backspace / ctrl+w / alt+backspace.
    pub fn delete_word_back(&mut self) -> bool {
        let start = self.previous_word_start();
        if start == self.cursor {
            return false;
        }
        self.chars.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    /// ctrl+delete / alt+d: to the end of the current (or next) word.
    pub fn delete_word_forward(&mut self) -> bool {
        let end = self.next_word_end();
        if end == self.cursor {
            return false;
        }
        self.chars.drain(self.cursor..end);
        true
    }

    /// ctrl+u / ctrl+home.
    pub fn kill_to_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.chars.drain(..self.cursor);
        self.cursor = 0;
        true
    }

    /// ctrl+k / ctrl+end.
    pub fn kill_to_end(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.chars.truncate(self.cursor);
        true
    }

    /// Replace the char range `start..end` with `with`, leaving the cursor
    /// `cursor_back` chars before the end of the inserted text (inside a
    /// closing quote, for instance).
    pub fn replace(&mut self, start: usize, end: usize, with: &str, cursor_back: usize) {
        let start = start.min(self.chars.len());
        let end = end.clamp(start, self.chars.len());
        let inserted: Vec<char> = with.chars().collect();
        let inserted_len = inserted.len();
        self.chars.splice(start..end, inserted);
        self.cursor = (start + inserted_len).saturating_sub(cursor_back);
    }

    fn previous_word_start(&self) -> usize {
        let mut index = self.cursor;
        while index > 0 && !is_word_char(self.chars[index - 1]) {
            index -= 1;
        }
        while index > 0 && is_word_char(self.chars[index - 1]) {
            index -= 1;
        }
        index
    }

    fn next_word_start(&self) -> usize {
        let len = self.chars.len();
        let mut index = self.cursor;
        while index < len && is_word_char(self.chars[index]) {
            index += 1;
        }
        while index < len && !is_word_char(self.chars[index]) {
            index += 1;
        }
        index
    }

    fn next_word_end(&self) -> usize {
        let len = self.chars.len();
        let mut index = self.cursor;
        while index < len && !is_word_char(self.chars[index]) {
            index += 1;
        }
        while index < len && is_word_char(self.chars[index]) {
            index += 1;
        }
        index
    }
}

/// Word = letters, digits, `_`. Path separators, dots and dashes split
/// words, so ctrl+left walks `C:\dev\Starcil` a segment at a time (PSReadLine
/// does the same).
fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Most entries kept in memory (the seed file is trimmed to the newest).
pub const HISTORY_CAP: usize = 5000;

/// Command history shared by every composer of the client: up/down walk it
/// filtered by the prefix typed so far (fish / PSReadLine
/// `HistorySearchBackward`), ctrl+r searches it. Seeded from the shell's own
/// history file, so the first launch already knows yesterday's commands; the
/// shell keeps writing that file itself since it receives every submitted
/// line as keystrokes.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    nav: Option<Nav>,
}

#[derive(Debug, Clone)]
struct Nav {
    index: usize,
    prefix: String,
    /// What the user had typed before walking the history.
    stash: String,
}

impl History {
    pub fn seed(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            self.push_entry(&line);
        }
        self.nav = None;
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Record a submitted line (consecutive duplicates collapse, blanks are
    /// dropped) and leave any navigation.
    pub fn push(&mut self, entry: &str) {
        self.nav = None;
        self.push_entry(entry);
    }

    fn push_entry(&mut self, entry: &str) {
        let entry = entry.trim_end();
        if entry.trim().is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| last == entry) {
            return;
        }
        self.entries.push(entry.to_owned());
        if self.entries.len() > HISTORY_CAP {
            let excess = self.entries.len() - HISTORY_CAP;
            self.entries.drain(..excess);
        }
    }

    /// Any edit ends the walk; the text shown stays as the new draft.
    pub fn reset(&mut self) {
        self.nav = None;
    }

    pub fn is_navigating(&self) -> bool {
        self.nav.is_some()
    }

    /// Up: the newest older entry starting with what was typed before the
    /// walk began (`current` is what the row shows now).
    pub fn previous(&mut self, current: &str) -> Option<String> {
        let (mut index, prefix, stash) = match &self.nav {
            Some(nav) => (nav.index, nav.prefix.clone(), nav.stash.clone()),
            None => (self.entries.len(), current.to_owned(), current.to_owned()),
        };
        while index > 0 {
            index -= 1;
            let entry = &self.entries[index];
            if entry.starts_with(&prefix) && entry != current {
                let entry = entry.clone();
                self.nav = Some(Nav { index, prefix, stash });
                return Some(entry);
            }
        }
        None
    }

    /// Down: the next newer match; past the newest the stashed draft returns
    /// and the walk ends.
    pub fn next(&mut self, current: &str) -> Option<String> {
        let nav = self.nav.clone()?;
        let mut index = nav.index;
        while index + 1 < self.entries.len() {
            index += 1;
            let entry = &self.entries[index];
            if entry.starts_with(&nav.prefix) && entry != current {
                let entry = entry.clone();
                self.nav = Some(Nav { index, ..nav });
                return Some(entry);
            }
        }
        self.nav = None;
        Some(nav.stash)
    }

    /// ctrl+r: the newest entry containing `query` (case-insensitive) older
    /// than `before` (an index from a previous hit; `None` = start from the
    /// newest). An empty query matches nothing.
    pub fn search_backward(&self, query: &str, before: Option<usize>) -> Option<(usize, String)> {
        if query.is_empty() {
            return None;
        }
        let query = query.to_lowercase();
        let end = before.unwrap_or(self.entries.len()).min(self.entries.len());
        self.entries[..end]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| entry.to_lowercase().contains(&query))
            .map(|(index, entry)| (index, entry.clone()))
    }

    /// First words of the history, for command-name completion: what the
    /// user actually runs, aliases and functions the PATH scan cannot see.
    pub fn command_words(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        let mut words = Vec::new();
        for entry in &self.entries {
            let Some(word) = entry.split_whitespace().next() else {
                continue;
            };
            let looks_like_command = word
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if looks_like_command && seen.insert(word.to_owned()) {
                words.push(word.to_owned());
            }
        }
        words
    }
}

/// How the shell writes its history file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFormat {
    /// `ConsoleHost_history.txt`: one command per line, a trailing backtick
    /// continues on the next line.
    PsReadLine,
    /// `.zsh_history`, extended (`: <epoch>:<secs>;<command>`) or plain; a
    /// trailing backslash continues.
    Zsh,
    /// `.bash_history`: one command per line, `#<epoch>` timestamp lines.
    Bash,
}

/// The shell history file this platform's default shell keeps, if any.
pub fn shell_history_path() -> Option<(PathBuf, HistoryFormat)> {
    if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA")?;
        return Some((
            PathBuf::from(appdata)
                .join("Microsoft")
                .join("Windows")
                .join("PowerShell")
                .join("PSReadLine")
                .join("ConsoleHost_history.txt"),
            HistoryFormat::PsReadLine,
        ));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if let Some(file) = std::env::var_os("HISTFILE") {
        let path = PathBuf::from(file);
        let format = if path.to_string_lossy().contains("zsh") {
            HistoryFormat::Zsh
        } else {
            HistoryFormat::Bash
        };
        return Some((path, format));
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("zsh") {
        Some((home.join(".zsh_history"), HistoryFormat::Zsh))
    } else {
        Some((home.join(".bash_history"), HistoryFormat::Bash))
    }
}

/// Read and parse the shell's history file; empty when there is none.
pub fn load_shell_history() -> Vec<String> {
    let Some((path, format)) = shell_history_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    parse_history(&String::from_utf8_lossy(&bytes), format)
}

pub fn parse_history(text: &str, format: HistoryFormat) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(mut open) = pending.take() {
            open.push('\n');
            open.push_str(line);
            if let Some(again) = continued(&open, format) {
                pending = Some(again);
            } else {
                entries.push(open);
            }
            continue;
        }
        let line = match format {
            HistoryFormat::Zsh => line
                .strip_prefix(": ")
                .and_then(|rest| rest.split_once(';'))
                .map(|(_, command)| command)
                .unwrap_or(line),
            HistoryFormat::Bash => {
                if line.starts_with('#')
                    && line[1..].chars().all(|c| c.is_ascii_digit())
                    && line.len() > 1
                {
                    continue;
                }
                line
            }
            HistoryFormat::PsReadLine => line,
        };
        if line.trim().is_empty() {
            continue;
        }
        match continued(line, format) {
            Some(open) => pending = Some(open),
            None => entries.push(line.to_owned()),
        }
    }
    if let Some(open) = pending {
        entries.push(open);
    }
    if entries.len() > HISTORY_CAP {
        let excess = entries.len() - HISTORY_CAP;
        entries.drain(..excess);
    }
    entries
}

/// A line whose last char says "the command goes on": returns it without
/// the continuation marker.
fn continued(line: &str, format: HistoryFormat) -> Option<String> {
    let marker = match format {
        HistoryFormat::PsReadLine => '`',
        HistoryFormat::Zsh | HistoryFormat::Bash => '\\',
    };
    let stripped = line.strip_suffix(marker)?;
    // An escaped marker (`\\` / ``` `` ```) ends the line for real.
    if stripped.ends_with(marker) {
        return None;
    }
    Some(stripped.to_owned())
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

/// What completion reads from the machine; `App` injects the real
/// filesystem, tests a fake.
pub trait CompletionSource {
    /// `(name, is_directory)` for every entry of `dir`; empty when unreadable.
    fn list_dir(&self, dir: &Path) -> Vec<(String, bool)>;
    /// Bare names of the executables on PATH (no extension on Windows).
    fn path_commands(&self) -> Vec<String>;
    fn home_dir(&self) -> Option<PathBuf>;
}

/// The real filesystem. The PATH scan runs once per client (first Tab in
/// command position) and is cached.
#[derive(Default)]
pub struct FsCompletionSource {
    commands: std::cell::OnceCell<Vec<String>>,
}

impl CompletionSource for FsCompletionSource {
    fn list_dir(&self, dir: &Path) -> Vec<(String, bool)> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| {
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir())
                    || entry.path().is_dir();
                (entry.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect()
    }

    fn path_commands(&self) -> Vec<String> {
        self.commands
            .get_or_init(|| {
                let path = std::env::var("PATH").unwrap_or_default();
                let pathext = std::env::var("PATHEXT").unwrap_or_default();
                scan_path_commands(&path, &pathext, &|dir| self.list_dir_with_exec(dir))
            })
            .clone()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }
}

impl FsCompletionSource {
    /// `(name, executable)` for a PATH directory.
    fn list_dir_with_exec(&self, dir: &Path) -> Vec<(String, bool)> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
            .map(|entry| {
                let executable = is_executable(&entry.path());
                (entry.file_name().to_string_lossy().into_owned(), executable)
            })
            .collect()
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Executable names across PATH. Windows: files with a PATHEXT extension,
/// reported without it (`code.cmd` → `code`); unix: files with an exec bit.
/// `list` yields `(name, executable)` per directory.
pub fn scan_path_commands(
    path: &str,
    pathext: &str,
    list: &dyn Fn(&Path) -> Vec<(String, bool)>,
) -> Vec<String> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let extensions: Vec<String> = if cfg!(windows) {
        let configured: Vec<String> = pathext
            .split(';')
            .map(|extension| extension.trim().to_ascii_lowercase())
            .filter(|extension| !extension.is_empty())
            .collect();
        if configured.is_empty() {
            [".exe", ".cmd", ".bat", ".com"]
                .iter()
                .map(|extension| (*extension).to_owned())
                .collect()
        } else {
            configured
        }
    } else {
        Vec::new()
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut names = Vec::new();
    for directory in path.split(separator).map(str::trim).filter(|d| !d.is_empty()) {
        for (name, executable) in list(Path::new(directory)) {
            let bare = if extensions.is_empty() {
                if !executable {
                    continue;
                }
                name
            } else {
                let lower = name.to_ascii_lowercase();
                let Some(extension) = extensions.iter().find(|ext| lower.ends_with(ext.as_str()))
                else {
                    continue;
                };
                name[..name.len() - extension.len()].to_owned()
            };
            if bare.is_empty() {
                continue;
            }
            let key = if cfg!(windows) { bare.to_ascii_lowercase() } else { bare.clone() };
            if seen.insert(key) {
                names.push(bare);
            }
        }
    }
    names
}

/// One replacement Tab can make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// Chars from the end where the cursor lands (1 = inside a closing
    /// quote, so typing continues the path).
    pub cursor_back: usize,
}

/// Everything Tab computed for the token under the cursor; `App` cycles
/// through `candidates` on repeated presses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Char range of the line to replace.
    pub start: usize,
    pub end: usize,
    pub candidates: Vec<Candidate>,
}

pub struct CompletionContext<'a> {
    /// The pane's live cwd (relative paths resolve against it).
    pub cwd: &'a str,
    /// Windows path rules: `\`, case-insensitive prefixes, `.\` for cwd
    /// entries in command position, single-quote quoting.
    pub windows: bool,
    /// First words from the history, offered as commands.
    pub history_commands: &'a [String],
}

/// Commands whose argument can only be a directory.
const DIRECTORY_COMMANDS: &[&str] = &["cd", "pushd", "rmdir", "rd", "chdir", "sl", "set-location", "cd.."];

/// Names every shell knows that no PATH scan lists.
fn builtin_commands(windows: bool) -> &'static [&'static str] {
    if windows {
        &[
            "cd", "cls", "clear", "dir", "ls", "cat", "cp", "mv", "rm", "rmdir", "mkdir", "md",
            "echo", "pwd", "pushd", "popd", "type", "where", "set", "exit", "history", "sort",
            "select", "foreach", "measure", "tee", "Get-ChildItem", "Get-Content", "Set-Location",
            "Get-Location", "Get-Process", "Stop-Process", "Start-Process", "Get-Command",
            "Get-Help", "Get-Item", "Remove-Item", "Copy-Item", "Move-Item", "New-Item",
            "Rename-Item", "Test-Path", "Select-String", "Write-Output", "Write-Host",
            "Set-Content", "Add-Content", "Out-File", "Import-Module", "Get-Module", "Get-Date",
            "Measure-Object", "Sort-Object", "Select-Object", "ForEach-Object", "Where-Object",
            "Invoke-WebRequest", "Invoke-RestMethod", "Invoke-Expression", "Get-Service",
            "Set-ExecutionPolicy", "Get-Alias", "Set-Alias",
        ]
    } else {
        &[
            "cd", "ls", "cat", "cp", "mv", "rm", "rmdir", "mkdir", "echo", "pwd", "pushd", "popd",
            "export", "source", "alias", "unalias", "exit", "clear", "history", "type", "which",
            "jobs", "fg", "bg", "kill", "set", "unset", "read", "test", "true", "false", "eval",
            "exec",
        ]
    }
}

/// The token the cursor sits in (or the empty token at the cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    /// Char index where the token (its opening quote included) starts.
    start: usize,
    /// Char index where the replacement ends: the cursor, or one past a
    /// closing quote sitting right at the cursor.
    end: usize,
    /// The token's text without its quotes.
    content: String,
    quote: Option<char>,
    /// No word before it in its command (start of line, or after `|`, `;`,
    /// `&`).
    command_position: bool,
    /// The command this token is an argument of, lowercased, quotes dropped.
    command: Option<String>,
}

fn token_at(text: &[char], cursor: usize) -> Token {
    let cursor = cursor.min(text.len());
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut token_start = 0usize;
    let mut in_token = false;
    let mut quote: Option<char> = None;
    for (index, &character) in text.iter().enumerate().take(cursor) {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                }
            }
            None => {
                if character.is_whitespace() {
                    if in_token {
                        words.push((token_start, index));
                        in_token = false;
                    }
                } else if matches!(character, '|' | ';' | '&') {
                    if in_token {
                        words.push((token_start, index));
                        in_token = false;
                    }
                    // A new command starts after the separator.
                    words.clear();
                } else {
                    if !in_token {
                        in_token = true;
                        token_start = index;
                    }
                    if matches!(character, '\'' | '"') {
                        quote = Some(character);
                    }
                }
            }
        }
    }
    let start = if in_token { token_start } else { cursor };
    let mut raw: Vec<char> = text[start..cursor].to_vec();
    let opening = raw.first().copied().filter(|c| matches!(c, '\'' | '"'));
    if opening.is_some() {
        raw.remove(0);
    }
    let mut end = cursor;
    if let Some(open) = opening {
        // The token's own closing quote: part of what gets replaced,
        // whether it sits before the cursor or right under it.
        if raw.last() == Some(&open) && raw.len() > 0 && quote.is_none() {
            raw.pop();
        } else if text.get(cursor) == Some(&open) {
            end = cursor + 1;
        }
    }
    let command = words.first().map(|&(from, to)| {
        text[from..to]
            .iter()
            .filter(|c| !matches!(c, '\'' | '"'))
            .collect::<String>()
            .to_lowercase()
    });
    Token {
        start,
        end,
        content: raw.into_iter().collect(),
        quote: opening,
        command_position: words.is_empty(),
        command,
    }
}

/// Candidates for the token under the cursor: paths (against the cwd) or
/// command names in command position. `None` when there is nothing to offer.
pub fn complete(
    text: &[char],
    cursor: usize,
    context: &CompletionContext<'_>,
    source: &dyn CompletionSource,
) -> Option<Completion> {
    let token = token_at(text, cursor);
    let looks_like_path = token.content.contains('/')
        || (context.windows && token.content.contains('\\'))
        || token.content.starts_with('.')
        || token.content.starts_with('~');
    let candidates = if token.command_position && !looks_like_path {
        if token.content.is_empty() {
            return None;
        }
        complete_command(&token, context, source)
    } else {
        let dirs_only = token
            .command
            .as_deref()
            .is_some_and(|command| DIRECTORY_COMMANDS.contains(&command));
        complete_path(&token, context, source, dirs_only)
    };
    if candidates.is_empty() {
        return None;
    }
    Some(Completion {
        start: token.start,
        end: token.end,
        candidates,
    })
}

fn complete_command(
    token: &Token,
    context: &CompletionContext<'_>,
    source: &dyn CompletionSource,
) -> Vec<Candidate> {
    let prefix = &token.content;
    let matches = |name: &str| {
        if context.windows {
            name.to_lowercase().starts_with(&prefix.to_lowercase())
        } else {
            name.starts_with(prefix.as_str())
        }
    };
    // Case-insensitive on Windows: the first spelling seen wins.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let all = builtin_commands(context.windows)
        .iter()
        .map(|name| (*name).to_owned())
        .chain(source.path_commands())
        .chain(context.history_commands.iter().cloned());
    for name in all {
        if !matches(&name) {
            continue;
        }
        let key = if context.windows { name.to_lowercase() } else { name.clone() };
        names.entry(key).or_insert(name);
    }
    let mut candidates: Vec<Candidate> = names
        .into_values()
        .map(|name| Candidate {
            text: quoted(&name, token.quote, context.windows),
            cursor_back: 0,
        })
        .collect();
    // Programs and folders right here, spelled the way the shell runs them.
    let local_prefix = if context.windows { ".\\" } else { "./" };
    let mut local = list_matching(Path::new(context.cwd), prefix, context, source, false);
    local.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    for (name, is_dir) in local {
        let mut text = format!("{local_prefix}{name}");
        if is_dir {
            text.push(if context.windows { '\\' } else { '/' });
        }
        let quoted_text = quoted(&text, token.quote, context.windows);
        let cursor_back = usize::from(is_dir && quoted_text.ends_with(['\'', '"']));
        candidates.push(Candidate {
            text: quoted_text,
            cursor_back,
        });
    }
    candidates
}

fn complete_path(
    token: &Token,
    context: &CompletionContext<'_>,
    source: &dyn CompletionSource,
    dirs_only: bool,
) -> Vec<Candidate> {
    let content = token.content.as_str();
    let is_separator = |c: char| c == '/' || (context.windows && c == '\\');
    let (dir_part, prefix) = match content.rfind(is_separator) {
        Some(index) => (&content[..=index], &content[index + 1..]),
        None => ("", content),
    };
    let separator = if !context.windows {
        '/'
    } else if content.contains('/') && !content.contains('\\') {
        '/'
    } else {
        '\\'
    };
    let Some(directory) = resolve_directory(dir_part, context, source) else {
        return Vec::new();
    };
    let mut entries = list_matching(&directory, prefix, context, source, dirs_only);
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    entries
        .into_iter()
        .map(|(name, is_dir)| {
            let mut text = format!("{dir_part}{name}");
            if is_dir {
                text.push(separator);
            }
            let quoted_text = quoted(&text, token.quote, context.windows);
            let cursor_back = usize::from(is_dir && quoted_text.ends_with(['\'', '"']));
            Candidate {
                text: quoted_text,
                cursor_back,
            }
        })
        .collect()
}

/// Entries of `directory` starting with `prefix` (case-insensitively on
/// Windows). Unix hides dot-files unless the prefix asks for them.
fn list_matching(
    directory: &Path,
    prefix: &str,
    context: &CompletionContext<'_>,
    source: &dyn CompletionSource,
    dirs_only: bool,
) -> Vec<(String, bool)> {
    let lower_prefix = prefix.to_lowercase();
    source
        .list_dir(directory)
        .into_iter()
        .filter(|(name, is_dir)| {
            if dirs_only && !is_dir {
                return false;
            }
            if !context.windows && name.starts_with('.') && !prefix.starts_with('.') {
                return false;
            }
            if context.windows {
                name.to_lowercase().starts_with(&lower_prefix)
            } else {
                name.starts_with(prefix)
            }
        })
        .collect()
}

/// The directory `dir_part` names: `~` expands, absolute paths stand, the
/// rest joins the cwd.
fn resolve_directory(
    dir_part: &str,
    context: &CompletionContext<'_>,
    source: &dyn CompletionSource,
) -> Option<PathBuf> {
    if dir_part.is_empty() {
        return Some(PathBuf::from(context.cwd));
    }
    if dir_part == "~" || dir_part.starts_with("~/") || (context.windows && dir_part.starts_with("~\\")) {
        let home = source.home_dir()?;
        let rest = dir_part[1..].trim_start_matches(['/', '\\']);
        return Some(if rest.is_empty() { home } else { home.join(rest) });
    }
    let absolute = if context.windows {
        dir_part.starts_with('\\')
            || dir_part.starts_with('/')
            || (dir_part.len() >= 2 && dir_part.as_bytes()[1] == b':')
    } else {
        dir_part.starts_with('/')
    };
    Some(if absolute {
        PathBuf::from(dir_part)
    } else {
        Path::new(context.cwd).join(dir_part)
    })
}

/// Shell-safe spelling: the user's own quote when the token had one,
/// otherwise quotes only when the text needs them (PowerShell: single
/// quotes; unix: backslash escapes).
fn quoted(text: &str, quote: Option<char>, windows: bool) -> String {
    let needs = text
        .chars()
        .any(|c| c.is_whitespace() || "'\"$();,|&<>{}@#`".contains(c));
    match quote {
        Some('"') => format!("\"{}\"", text.replace('"', if windows { "`\"" } else { "\\\"" })),
        Some(_) => format!("'{}'", text.replace('\'', if windows { "''" } else { "'\\''" })),
        None if !needs => text.to_owned(),
        None if windows => format!("'{}'", text.replace('\'', "''")),
        None => text
            .chars()
            .flat_map(|c| {
                let escape = c.is_whitespace() || "'\"$();,|&<>{}@#`".contains(c);
                escape.then_some('\\').into_iter().chain(std::iter::once(c))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeFs {
        dirs: BTreeMap<String, Vec<(String, bool)>>,
        commands: Vec<String>,
    }

    impl FakeFs {
        fn new() -> Self {
            let mut dirs = BTreeMap::new();
            dirs.insert(
                norm("C:/repo"),
                vec![
                    ("tests".to_owned(), true),
                    ("target".to_owned(), true),
                    ("Cargo.toml".to_owned(), false),
                    ("README.md".to_owned(), false),
                    ("My Docs".to_owned(), true),
                    (".git".to_owned(), true),
                    ("build.ps1".to_owned(), false),
                ],
            );
            dirs.insert(
                norm("C:/repo/tests"),
                vec![("e2e".to_owned(), true), ("cli.rs".to_owned(), false)],
            );
            dirs.insert(
                norm("C:/home/cesar"),
                vec![("Desktop".to_owned(), true), ("notes.txt".to_owned(), false)],
            );
            dirs.insert(norm("D:/"), vec![("dev".to_owned(), true)]);
            Self {
                dirs,
                commands: vec!["git".to_owned(), "cargo".to_owned(), "claude".to_owned(), "code".to_owned()],
            }
        }
    }

    /// Lowercase, forward slashes, no `.` segments: `C:/repo/./` and
    /// `C:\repo` name the same fake directory.
    fn norm(path: &str) -> String {
        let replaced = path.replace('\\', "/");
        let parts: Vec<&str> = replaced
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        parts.join("/").to_lowercase()
    }

    impl CompletionSource for FakeFs {
        fn list_dir(&self, dir: &Path) -> Vec<(String, bool)> {
            self.dirs
                .get(&norm(&dir.to_string_lossy()))
                .cloned()
                .unwrap_or_default()
        }

        fn path_commands(&self) -> Vec<String> {
            self.commands.clone()
        }

        fn home_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from("C:/home/cesar"))
        }
    }

    fn complete_win(line: &str) -> Option<Completion> {
        let chars: Vec<char> = line.chars().collect();
        let history = vec!["cd".to_owned(), "npm".to_owned()];
        let context = CompletionContext {
            cwd: "C:/repo",
            windows: true,
            history_commands: &history,
        };
        complete(&chars, chars.len(), &context, &FakeFs::new())
    }

    fn texts(completion: &Completion) -> Vec<&str> {
        completion
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect()
    }

    #[test]
    fn editor_moves_edits_and_walks_words() {
        let mut editor = LineEditor::from_text("cd C:\\dev\\Starcil");
        assert_eq!(editor.cursor(), 17);
        editor.word_left();
        assert_eq!(editor.cursor(), 10, "to the start of `Starcil`");
        editor.word_left();
        assert_eq!(editor.cursor(), 6, "to `dev`");
        editor.word_right();
        assert_eq!(editor.cursor(), 10, "start of the next word");
        editor.delete_word_back();
        assert_eq!(editor.text(), "cd C:\\Starcil");
        editor.move_home();
        editor.delete_word_forward();
        assert_eq!(editor.text(), " C:\\Starcil");
        editor.insert_str("ls");
        assert_eq!(editor.text(), "ls C:\\Starcil");
        assert_eq!(editor.cursor(), 2);
        editor.move_right();
        editor.kill_to_end();
        assert_eq!(editor.text(), "ls ");
        editor.move_left();
        editor.delete();
        assert_eq!(editor.text(), "ls");
        editor.kill_to_start();
        assert_eq!(editor.text(), "");
        assert!(!editor.backspace());
        editor.set_text("héllo wörld");
        editor.set_cursor(1);
        editor.replace(0, 5, "hola", 0);
        assert_eq!(editor.text(), "hola wörld");
        assert_eq!(editor.cursor(), 4);
    }

    #[test]
    fn history_walks_by_prefix_and_restores_the_draft() {
        let mut history = History::default();
        history.seed(["git status", "cargo test", "git push", "cargo test", "ls"].map(String::from));
        assert_eq!(history.entries().len(), 5);

        // Plain up/down from an empty draft.
        assert_eq!(history.previous("").as_deref(), Some("ls"));
        assert_eq!(history.previous("ls").as_deref(), Some("cargo test"));
        assert_eq!(history.next("cargo test").as_deref(), Some("ls"));
        assert_eq!(history.next("ls").as_deref(), Some(""), "past the newest: the empty draft");
        assert!(!history.is_navigating());

        // Prefix filter, stash comes back at the end.
        assert_eq!(history.previous("git").as_deref(), Some("git push"));
        assert_eq!(history.previous("git push").as_deref(), Some("git status"));
        assert_eq!(history.previous("git status"), None, "nothing older");
        assert_eq!(history.next("git status").as_deref(), Some("git push"));
        assert_eq!(history.next("git push").as_deref(), Some("git"));

        // Submissions dedupe consecutive repeats and end any walk.
        history.push("ls");
        assert_eq!(history.entries().len(), 5);
        history.push("  ");
        assert_eq!(history.entries().len(), 5);
        history.push("cargo build");
        assert_eq!(history.entries().last().map(String::as_str), Some("cargo build"));

        // ctrl+r walks matches newest first, case-insensitively.
        let (index, hit) = history.search_backward("CARGO", None).unwrap();
        assert_eq!(hit, "cargo build");
        let (older, hit) = history.search_backward("cargo", Some(index)).unwrap();
        assert_eq!(hit, "cargo test");
        assert!(older < index);
        assert!(history.search_backward("", None).is_none());
        assert_eq!(history.command_words(), vec!["git", "cargo", "ls"]);
    }

    #[test]
    fn history_files_parse_per_shell() {
        let ps = "git status\r\nfoo `\r\n  bar\r\n\r\ncargo test\r\n";
        assert_eq!(
            parse_history(ps, HistoryFormat::PsReadLine),
            vec!["git status", "foo \n  bar", "cargo test"]
        );
        let zsh = ": 1700000000:0;ls -la\n: 1700000001:0;echo hi \\\nthere\nplain\n";
        assert_eq!(
            parse_history(zsh, HistoryFormat::Zsh),
            vec!["ls -la", "echo hi \nthere", "plain"]
        );
        let bash = "#1700000000\nls\n#1700000001\ncd ..\n";
        assert_eq!(parse_history(bash, HistoryFormat::Bash), vec!["ls", "cd .."]);
    }

    #[test]
    fn path_completion_follows_the_cwd_and_cycles_sorted() {
        let completion = complete_win("cd t").unwrap();
        assert_eq!((completion.start, completion.end), (3, 4));
        assert_eq!(texts(&completion), vec!["target\\", "tests\\"], "cd offers directories only");

        let completion = complete_win("cat c").unwrap();
        assert_eq!(texts(&completion), vec!["Cargo.toml"], "case-insensitive on Windows");

        let completion = complete_win("ls tests\\").unwrap();
        assert_eq!(texts(&completion), vec!["tests\\cli.rs", "tests\\e2e\\"]);

        let completion = complete_win("ls tests/e").unwrap();
        assert_eq!(texts(&completion), vec!["tests/e2e/"], "keeps the user's separator");

        let completion = complete_win("ls ~\\De").unwrap();
        assert_eq!(texts(&completion), vec!["~\\Desktop\\"]);

        let completion = complete_win("ls D:\\d").unwrap();
        assert_eq!(texts(&completion), vec!["D:\\dev\\"], "absolute paths stand alone");

        assert!(complete_win("ls zzz").is_none(), "no match, nothing to cycle");
        assert!(complete_win("").is_none(), "empty command position offers nothing");
    }

    #[test]
    fn spaces_get_quoted_with_the_cursor_inside_for_directories() {
        let completion = complete_win("cd my").unwrap();
        assert_eq!(texts(&completion), vec!["'My Docs\\'"]);
        assert_eq!(completion.candidates[0].cursor_back, 1);

        // An open quote is honoured and the closing one under the cursor is
        // part of the replacement.
        let line: Vec<char> = "cd 'my'".chars().collect();
        let context = CompletionContext { cwd: "C:/repo", windows: true, history_commands: &[] };
        let completion = complete(&line, 6, &context, &FakeFs::new()).unwrap();
        assert_eq!((completion.start, completion.end), (3, 7));
        assert_eq!(texts(&completion), vec!["'My Docs\\'"]);
        let completion = complete(&line, 7, &context, &FakeFs::new()).unwrap();
        assert_eq!((completion.start, completion.end), (3, 7), "after the closing quote too");
    }

    #[test]
    fn command_position_offers_commands_then_local_programs() {
        let completion = complete_win("c").unwrap();
        let names = texts(&completion);
        assert!(names.contains(&"cargo"), "{names:?}");
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"cd"), "builtins and history words count");
        assert!(names.contains(&".\\Cargo.toml"), "files here run as .\\name");
        assert!(!names.contains(&"git"));
        let position = |name: &str| names.iter().position(|n| *n == name).unwrap();
        assert!(position("cargo") < position(".\\Cargo.toml"), "commands before local files");

        let completion = complete_win("git status | c").unwrap();
        assert!(texts(&completion).contains(&"cargo"), "after a pipe a new command starts");

        let completion = complete_win("./b").unwrap();
        assert_eq!(texts(&completion), vec!["./build.ps1"], "a path in command position");
    }

    #[test]
    fn unix_rules_hide_dot_files_and_escape_instead_of_quoting() {
        let mut fake = FakeFs::new();
        fake.dirs.insert(
            norm("/home/cesar/repo"),
            vec![
                (".git".to_owned(), true),
                ("src".to_owned(), true),
                ("my file.txt".to_owned(), false),
                ("Makefile".to_owned(), false),
            ],
        );
        let history: Vec<String> = Vec::new();
        let context = CompletionContext {
            cwd: "/home/cesar/repo",
            windows: false,
            history_commands: &history,
        };
        let run = |line: &str| {
            let chars: Vec<char> = line.chars().collect();
            complete(&chars, chars.len(), &context, &fake)
        };
        assert_eq!(texts(&run("ls ").unwrap()), vec!["Makefile", "my\\ file.txt", "src/"]);
        assert_eq!(texts(&run("ls .").unwrap()), vec![".git/"]);
        assert!(run("ls m").unwrap().candidates.iter().all(|c| c.text.starts_with('m')), "case-sensitive");
        assert_eq!(texts(&run("s").unwrap()), vec!["set", "source", "./src/"]);
    }

    #[test]
    fn path_scan_strips_pathext_on_windows_and_needs_exec_bits_on_unix() {
        let list = |dir: &Path| -> Vec<(String, bool)> {
            match dir.to_string_lossy().as_ref() {
                "C:/bin" => vec![
                    ("code.cmd".to_owned(), true),
                    ("git.exe".to_owned(), true),
                    ("readme.txt".to_owned(), true),
                ],
                "/usr/bin" => vec![("ls".to_owned(), true), ("notes".to_owned(), false)],
                _ => vec![],
            }
        };
        if cfg!(windows) {
            let names = scan_path_commands("C:/bin;", ".COM;.EXE;.BAT;.CMD", &list);
            assert_eq!(names, vec!["code", "git"]);
        } else {
            let names = scan_path_commands("/usr/bin:/nowhere", "", &list);
            assert_eq!(names, vec!["ls"]);
        }
    }
}
