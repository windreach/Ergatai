//! Single-message rendering: append a `Message`'s styled lines to a `Text`.
//!
//! Phase A redesign:
//! - **User messages**: prefix first line with `▌` (cyan), continuation lines
//!   indented with 2 spaces. No `You >` header.
//! - **Assistant messages**: no header. Content rendered directly (markdown,
//!   thinking, tool cards). Continuation lines indented 2 spaces for visual
//!   separation between turns.
//! - **System messages**: `• ` prefix, `DarkGray`, unchanged.
//!
//! Phase B upgrade (codex-style):
//! - **User messages** get a subtle **tinted background** (`Color::Rgb(31,
//!   31, 31)` — a 12% white blend on a black terminal). Combined with the
//!   `▌` prefix this visually separates user turns without a border.

use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};

use crate::ui::app::Message;
use crate::ui::theme;
use crate::ui::widgets::{thinking, tool_card};

/// Append one message's content to `text`. Each message is followed by a
/// blank line for readability.
pub fn render_into(text: &mut Text<'static>, msg: &Message, collapsed_thinking: bool) {
    match msg {
        Message::User { text: content } => {
            let prefix_style = Style::default().fg(theme::accent()).bg(theme::user_msg_bg());
            let content_style = Style::default().bg(theme::user_msg_bg());
            let lines: Vec<&str> = content.lines().collect();
            if lines.is_empty() {
                text.lines
                    .push(Line::from(vec![Span::styled("▌ ", prefix_style)]));
            } else {
                for (i, raw_line) in lines.iter().enumerate() {
                    if i == 0 {
                        text.lines.push(Line::from(vec![
                            Span::styled("▌ ", prefix_style),
                            Span::styled(raw_line.to_string(), content_style),
                        ]));
                    } else {
                        // Continuation: 2-space indent to align with the first line's text.
                        text.lines.push(Line::from(vec![
                            Span::styled("  ", content_style),
                            Span::styled(raw_line.to_string(), content_style),
                        ]));
                    }
                }
            }
            text.lines.push(Line::default());
        }
        Message::Assistant {
            text: content,
            thinking: thinking_text,
            tool_calls,
            in_progress,
        } => {
            // No header in Phase A — render content directly.

            if !thinking_text.is_empty() {
                thinking::render_thinking_into(text, thinking_text, collapsed_thinking);
            }

            for tc in tool_calls {
                tool_card::render_tool_card_into(text, tc);
            }

            if content.is_empty() && *in_progress {
                text.lines.push(Line::from(Span::styled(
                    "…",
                    Style::default().fg(theme::muted()),
                )));
            } else if !content.is_empty() {
                let md =
                    crate::ui::widgets::markdown::render_markdown_with_syntax_highlighting(content);
                for line in md.lines {
                    let mut indented_spans = vec![Span::raw("  ")];
                    for span in line.spans {
                        indented_spans.push(Span::styled(span.content.into_owned(), span.style));
                    }
                    text.lines.push(Line::from(indented_spans));
                }
            }

            text.lines.push(Line::default());
        }
        Message::System { text: content } => {
            text.lines.push(Line::from(Span::styled(
                format!("• {content}"),
                Style::default().fg(theme::muted()),
            )));
            text.lines.push(Line::default());
        }
    }
}
