//! Footer (1 line): minimal command hints in dim text.
//!
//! Phase A redesign: fewer hints, all in `Color::DarkGray`. The footer is
//! secondary — users learn shortcuts; we don't advertise everything.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::app::AppState;

pub fn render(frame: &mut Frame<'_>, area: Rect, _app: &AppState<'_>) {
    let dim = Style::default().fg(Color::DarkGray);
    let line = Line::from(vec![
        Span::styled("↑↓ history", dim),
        Span::styled("  ·  ", dim),
        Span::styled("/ commands", dim),
        Span::styled("  ·  ", dim),
        Span::styled("Ctrl-C quit", dim),
    ]);

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}
