//! Syntax highlighting widget using `syntect`.
//!
//! Provides [`highlight_code`] which converts a fenced code block into a
//! sequence of ratatui [`Line`]s with per-token colouring driven by a
//! `syntect` theme.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Default syntax set (with newlines retained — required by `LinesWithEndings`).
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Default theme set.
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// The theme name used for code highlighting.
///
/// `syntect` ships several built-in themes; `base16-ocean.dark` is a
/// well-balanced dark palette that reads well on terminal backgrounds.
const THEME_NAME: &str = "base16-ocean.dark";

/// Highlight `code` as `language` and return ratatui-styled [`Line`]s.
///
/// Falls back to plain text when the language token is unknown or when the
/// configured theme is missing.  Never panics.
pub fn highlight_code(code: &str, language: &str) -> Vec<Line<'static>> {
    let ss = &*SYNTAX_SET;
    let ts = &*THEME_SET;

    let syntax = ss
        .find_syntax_by_token(language)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let theme = match ts.themes.get(THEME_NAME) {
        Some(t) => t,
        None => {
            // Theme not found — return code as plain monospace lines.
            return code
                .lines()
                .map(|l| Line::from(Span::raw(l.to_string())))
                .collect();
        }
    };

    let mut h = HighlightLines::new(syntax, theme);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, ss).unwrap_or_default();
        let spans: Vec<Span<'static>> = ranges
            .into_iter()
            .map(|(style, text)| {
                let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                Span::styled(text.to_string(), Style::default().fg(fg))
            })
            .collect();
        lines.push(Line::from(spans));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_unknown_language_returns_plain_lines() {
        let lines = highlight_code("let x = 1;\n", "no_such_language_xyz");
        assert!(!lines.is_empty());
    }

    #[test]
    fn highlight_rust_returns_lines() {
        let lines = highlight_code("fn main() {}\n", "rust");
        assert!(!lines.is_empty());
    }

    #[test]
    fn highlight_empty_code() {
        let lines = highlight_code("", "rust");
        // Empty input → empty output (LinesWithEndings yields nothing).
        assert!(lines.is_empty());
    }
}
