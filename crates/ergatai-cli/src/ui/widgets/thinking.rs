//! Thinking block rendering widget.
//!
//! Phase A: removed `(Ctrl-T to expand)` help hint — the footer handles
//! shortcut discovery.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};

/// Render a thinking block into the given Text.
///
/// When collapsed, shows a single-line indicator.
/// When expanded, shows the full thinking text in dim color.
pub fn render_thinking_into(text: &mut Text<'static>, thinking: &str, collapsed: bool) {
    if thinking.is_empty() {
        return;
    }

    if collapsed {
        // Collapsed: single line indicator (no help hint — footer covers shortcuts).
        text.lines.push(Line::from(Span::styled(
            "💭 Thinking…",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Expanded: show full thinking text with dim styling.
        for line in thinking.lines() {
            text.lines.push(Line::from(Span::styled(
                format!("│ {line}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
}
