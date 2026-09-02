//! Agent dock: clickable launchers for AI CLIs found on PATH.
//!
//! Detection runs at TUI startup (and again on config reload): each configured
//! name (`ui.dock.agents`) is looked up on PATH the way the OS shell would —
//! on Windows every PATHEXT extension counts, elsewhere the bare name must be
//! an existing file. Order in the config is the dock order.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockAgent {
    /// Display name (the configured binary name).
    pub name: String,
    /// Command typed into the new pane's shell.
    pub command: String,
    /// Single-width icon shown before the name; only Claude Code and Codex
    /// carry one (Cesar's call), every other CLI shows its name alone.
    pub glyph: Option<char>,
}

/// Icon + brand color for a dock row. Only Claude Code (`✴`) and Codex
/// (`֎`) carry an icon — Cesar picked those two glyphs; the rest show their
/// name alone. Both are single-column glyphs (no emoji presentation selector,
/// which would make the terminal draw two cells and skew the panel).
pub fn dock_glyph(name: &str) -> Option<(char, starcil_config::Color)> {
    let glyph = match name {
        "claude" => '✴',
        "codex" => '֎',
        _ => return None,
    };
    let color = crate::dock_icons::brand_icon(name)
        .map(|(_, (red, green, blue))| starcil_config::Color::Rgb(red, green, blue))
        .unwrap_or(starcil_config::Color::Reset);
    Some((glyph, color))
}

/// Scan PATH for the configured agent binaries using the process environment.
pub fn detect_dock_agents(names: &[String]) -> Vec<DockAgent> {
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    detect_with(names, &path, &pathext, &|candidate| candidate.is_file())
}

/// Testable core: `is_file` abstracts the filesystem.
pub fn detect_with(
    names: &[String],
    path: &str,
    pathext: &str,
    is_file: &dyn Fn(&Path) -> bool,
) -> Vec<DockAgent> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let directories: Vec<&str> = path
        .split(separator)
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
        .collect();
    let extensions: Vec<String> = if cfg!(windows) {
        let configured: Vec<String> = pathext
            .split(';')
            .map(|extension| extension.trim().to_ascii_lowercase())
            .filter(|extension| !extension.is_empty())
            .collect();
        if configured.is_empty() {
            [".exe", ".cmd", ".bat", ".com", ".ps1"]
                .iter()
                .map(|extension| (*extension).to_owned())
                .collect()
        } else {
            configured
        }
    } else {
        Vec::new()
    };

    names
        .iter()
        .filter(|name| !name.trim().is_empty())
        .filter(|name| {
            directories.iter().any(|directory| {
                let base = PathBuf::from(directory).join(name.trim());
                if extensions.is_empty() {
                    is_file(&base)
                } else {
                    extensions.iter().any(|extension| {
                        let mut candidate = base.clone().into_os_string();
                        candidate.push(extension.as_str());
                        is_file(Path::new(&candidate))
                    })
                }
            })
        })
        .map(|name| {
            let name = name.trim().to_owned();
            DockAgent {
                glyph: dock_glyph(&name).map(|(glyph, _)| glyph),
                command: name.clone(),
                name,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_lookup_honors_pathext_and_order() {
        let names = vec![
            "claude".to_owned(),
            "codex".to_owned(),
            "missing".to_owned(),
            "custom-tool".to_owned(),
        ];
        let is_file = |path: &Path| {
            let path = path.to_string_lossy().replace('\\', "/");
            path == "C:/bin/claude.exe" || path == "D:/tools/codex.cmd" || path == "C:/bin/custom-tool.bat"
        };
        let found = detect_with(
            &names,
            if cfg!(windows) { "C:/bin;D:/tools; ;" } else { "C:/bin:D:/tools" },
            ".COM;.EXE;.BAT;.CMD",
            &is_file,
        );
        let found_names: Vec<&str> = found.iter().map(|agent| agent.name.as_str()).collect();
        if cfg!(windows) {
            assert_eq!(found_names, vec!["claude", "codex", "custom-tool"]);
            assert_eq!(found[0].glyph, Some('✴'));
            assert_eq!(found[1].glyph, Some('֎'));
            assert_eq!(found[2].glyph, None, "only claude and codex carry an icon");
        } else {
            assert!(found_names.is_empty(), "bare names have no extension on unix");
        }
    }

    #[test]
    fn empty_and_blank_names_are_skipped() {
        let names = vec!["".to_owned(), "  ".to_owned()];
        assert!(detect_with(&names, "C:/bin", ".exe", &|_| true).is_empty());
    }
}
