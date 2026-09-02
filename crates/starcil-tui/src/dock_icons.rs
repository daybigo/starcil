//! Brand glyph + color per known agent CLI, derived from the logo SVGs in
//! steipete/CodexBar (Sources/CodexBar/Resources, fetched 2026-08-24). Terminal
//! cells cannot draw SVGs; each entry keeps the logo's dominant color and a
//! single-width glyph that evokes its shape. Unknown names fall back to text.
//!
//! Most CodexBar SVGs are monochrome masks. For those, the RGB value comes from
//! the matching provider palette stored alongside the SVG in CodexBar. Aider
//! uses its official SVG because the fetched Resources listing has no Aider icon.

/// (glyph, (r, g, b)) for a known agent CLI name, `None` otherwise.
pub fn brand_icon(name: &str) -> Option<(char, (u8, u8, u8))> {
    Some(match name {
        "claude" => ('✳', (0xD9, 0x77, 0x57)),
        "codex" => ('⬡', (0x73, 0x6B, 0xD4)),
        "opencode" => ('◆', (0x21, 0x1E, 0x1E)),
        "aider" => ('✚', (0x14, 0xB0, 0x14)),
        "gemini" => ('✦', (0x42, 0x85, 0xF4)),
        "kimi" => ('◐', (0x4E, 0x6E, 0xF2)),
        "deepseek" => ('◈', (0x4D, 0x6B, 0xFE)),
        "cursor" | "cursor-agent" => ('△', (0x00, 0xBF, 0xA5)),
        "copilot" => ('◉', (0x85, 0x34, 0xF3)),
        "droid" | "factory" => ('✣', (0xEE, 0x60, 0x18)),
        "amp" => ('✧', (0xDC, 0x26, 0x26)),
        "qwen" | "qwencloud" => ('⬢', (0x61, 0x5C, 0xED)),
        "devin" => ('⊙', (0x46, 0xB4, 0x82)),
        "cline" | "clinepass" => ('▣', (0x61, 0xA3, 0xFA)),
        "kiro" => ('◒', (0x8F, 0x4A, 0xFF)),
        "grok" => ('╱', (0x10, 0xA3, 0x7F)),
        "kilo" => ('▥', (0xFA, 0x48, 0x3A)),
        "qoder" | "qodercli" => ('⊚', (0x2A, 0xDB, 0x5C)),
        "antigravity" => ('∩', (0x42, 0x85, 0xF4)),
        "augment" => ('{', (0xF9, 0x73, 0x16)),
        "codebuff" => ('◇', (0x44, 0xFF, 0x00)),
        "manus" => ('✥', (0x34, 0x32, 0x2D)),
        "mistral" => ('▦', (0xFA, 0x50, 0x0F)),
        "ollama" => ('♞', (0x88, 0x88, 0x88)),
        "opencodego" => ('▤', (0x21, 0x1E, 0x1E)),
        "warp" => ('▰', (0xC7, 0xAE, 0xFF)),
        "windsurf" => ('≋', (0x34, 0xE8, 0xBB)),
        "zed" => ('Z', (0x08, 0x4C, 0xCF)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN_NAMES: &[&str] = &[
        "claude",
        "codex",
        "opencode",
        "aider",
        "gemini",
        "kimi",
        "deepseek",
        "cursor",
        "cursor-agent",
        "copilot",
        "droid",
        "factory",
        "amp",
        "qwen",
        "qwencloud",
        "devin",
        "cline",
        "clinepass",
        "kiro",
        "grok",
        "kilo",
        "qoder",
        "qodercli",
        "antigravity",
        "augment",
        "codebuff",
        "manus",
        "mistral",
        "ollama",
        "opencodego",
        "warp",
        "windsurf",
        "zed",
    ];

    // Reviewed against Unicode's narrow/ambiguous geometric and dingbat ranges.
    // This keeps the module standalone without adding unicode-width to Cargo.toml.
    const REVIEWED_SINGLE_COLUMN_GLYPHS: &[char] = &[
        '✳', '⬡', '◆', '✚', '✦', '◐', '◈', '△', '◉', '✣', '✧', '⬢', '⊙', '▣', '◒',
        '╱', '▥', '⊚', '∩', '{', '◇', '✥', '▦', '♞', '▤', '▰', '≋', 'Z',
    ];

    #[test]
    fn claude_and_codex_keep_the_extracted_colors() {
        assert_eq!(brand_icon("claude"), Some(('✳', (0xD9, 0x77, 0x57))));
        assert_eq!(brand_icon("codex"), Some(('⬡', (0x73, 0x6B, 0xD4))));
    }

    #[test]
    fn unknown_name_has_no_brand_icon() {
        assert_eq!(brand_icon("unknown-agent"), None);
    }

    #[test]
    fn every_known_glyph_is_in_the_single_column_allowlist() {
        for name in KNOWN_NAMES {
            let (glyph, _) = brand_icon(name).expect("known agent must have an icon");
            assert!(
                REVIEWED_SINGLE_COLUMN_GLYPHS.contains(&glyph),
                "{name} uses an unreviewed glyph: {glyph}"
            );
        }
    }
}
