//! Markdown rendering widget using tui-markdown.
//!
//! Phase 3 adds [`render_markdown_with_syntax_highlighting`] which
//! post-processes fenced code blocks through [`crate::ui::widgets::syntax::highlight_code`]
//! while delegating prose to `tui-markdown`.
//!
//! Phase B adds **heading-style grading**: when the tui-markdown output
//! contains a line whose joined text starts with `#`/`##`/`###`/…, we
//! restyle the whole line per heading level (H1 bold+underline, H2 bold,
//! H3 bold+italic, H4+ italic) and strip the leading `#` tokens. This
//! mirrors codex's markdown renderer.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::syntax::highlight_code;

/// Render markdown text with syntax-highlighted fenced code blocks.
pub fn render_markdown_with_syntax_highlighting(content: &str) -> Text<'static> {
    let mut result = Text::default();
    let mut prose_buf = String::new();

    let segments = split_prose_and_code_blocks(content);
    for segment in segments {
        match segment {
            Segment::Prose(text) => {
                prose_buf.push_str(&text);
            }
            Segment::CodeBlock { code, language } => {
                if !prose_buf.is_empty() {
                    let md = tui_markdown::from_str(&prose_buf);
                    for line in md.lines {
                        result.lines.push(apply_heading_style(own_line(line)));
                    }
                    prose_buf.clear();
                }
                let highlighted = highlight_code(&code, &language);
                result.lines.extend(highlighted);
            }
        }
    }

    if !prose_buf.is_empty() {
        let md = tui_markdown::from_str(&prose_buf);
        for line in md.lines {
            result.lines.push(apply_heading_style(own_line(line)));
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Heading style post-processing
// ---------------------------------------------------------------------------

/// Detect an ATX heading prefix in `line_text` and return
/// `(level, rest_of_line)` if found.
fn detect_heading(line_text: &str) -> Option<(u8, &str)> {
    let trimmed = line_text.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let mut level: u8 = 0;
    let mut rest = trimmed;
    while rest.starts_with('#') && level < 6 {
        level += 1;
        rest = &rest[1..];
    }
    // Require either end-of-line or whitespace after the `#`s.
    if let Some(c) = rest.chars().next() {
        if !c.is_whitespace() {
            return None;
        }
        rest = rest.trim_start();
    }
    if level == 0 {
        None
    } else {
        Some((level, rest))
    }
}

/// Restyle a heading line (or pass it through unchanged).
fn apply_heading_style(line: Line<'static>) -> Line<'static> {
    let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if let Some((level, rest)) = detect_heading(&line_text) {
        if rest.is_empty() {
            return Line::default();
        }
        let style = match level {
            1 => Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            2 => Style::default().add_modifier(Modifier::BOLD),
            3 => Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
            _ => Style::default().add_modifier(Modifier::ITALIC),
        };
        Line::from(Span::styled(rest.to_string(), style))
    } else {
        line
    }
}

/// Convert a borrowed tui-markdown line to an owned `Line<'static>`.
fn own_line(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|s| Span::styled(s.content.into_owned(), s.style))
            .collect::<Vec<_>>(),
    )
}

// ---------------------------------------------------------------------------
// Internal: split markdown into prose / code-block segments
// ---------------------------------------------------------------------------

enum Segment {
    Prose(String),
    CodeBlock { code: String, language: String },
}

/// Split `content` into alternating prose and fenced-code-block segments.
fn split_prose_and_code_blocks(content: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut in_code_block = false;
    let mut current_prose = String::new();
    let mut current_code = String::new();
    let mut current_lang = String::new();

    for line in content.lines() {
        if !in_code_block && line.trim_start().starts_with("```") {
            if !current_prose.is_empty() {
                segments.push(Segment::Prose(std::mem::take(&mut current_prose)));
            }
            current_lang = line.trim_start().trim_start_matches('`').trim().to_string();
            in_code_block = true;
        } else if in_code_block && line.trim_start().starts_with("```") {
            segments.push(Segment::CodeBlock {
                code: std::mem::take(&mut current_code),
                language: std::mem::take(&mut current_lang),
            });
            in_code_block = false;
        } else if in_code_block {
            current_code.push_str(line);
            current_code.push('\n');
        } else {
            current_prose.push_str(line);
            current_prose.push('\n');
        }
    }

    if !current_prose.is_empty() {
        segments.push(Segment::Prose(current_prose));
    }
    if in_code_block && !current_code.is_empty() {
        segments.push(Segment::CodeBlock {
            code: current_code,
            language: current_lang,
        });
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_code_blocks_returns_single_prose_segment() {
        let segs = split_prose_and_code_blocks("hello\nworld\n");
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], Segment::Prose(_)));
    }

    #[test]
    fn single_code_block_detected() {
        let input = "before\n```rust\nfn x() {}\n```\nafter\n";
        let segs = split_prose_and_code_blocks(input);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], Segment::Prose(_)));
        match &segs[1] {
            Segment::CodeBlock { language, code } => {
                assert_eq!(language, "rust");
                assert!(code.contains("fn x()"));
            }
            _ => panic!("expected code block"),
        }
        assert!(matches!(&segs[2], Segment::Prose(_)));
    }

    #[test]
    fn unclosed_fence_treated_as_code_block() {
        let input = "```python\nprint('hi')\n";
        let segs = split_prose_and_code_blocks(input);
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            Segment::CodeBlock { language, code } => {
                assert_eq!(language, "python");
                assert!(code.contains("print"));
            }
            _ => panic!("expected code block for unclosed fence"),
        }
    }

    #[test]
    fn render_with_highlighting_does_not_panic() {
        let text = "# Title\n\n```rust\nlet x = 1;\n```\n\nMore text.";
        let _ = render_markdown_with_syntax_highlighting(text);
    }

    #[test]
    fn detect_heading_levels() {
        assert_eq!(detect_heading("# Hello"), Some((1, "Hello")));
        assert_eq!(detect_heading("## Sub"), Some((2, "Sub")));
        assert_eq!(detect_heading("### Subsub"), Some((3, "Subsub")));
        assert_eq!(detect_heading("#### Deep"), Some((4, "Deep")));
        assert_eq!(detect_heading("Hello world"), None);
        // `#tag` with no space is not a heading.
        assert_eq!(detect_heading("#tag"), None);
    }

    #[test]
    fn apply_heading_style_strips_hashes() {
        let line = Line::from(Span::raw("## Title"));
        let out = apply_heading_style(line);
        let text: String = out.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Title");
    }
}
