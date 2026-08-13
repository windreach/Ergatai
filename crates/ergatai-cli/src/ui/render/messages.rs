//! Scrollable messages pane.

use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::AppState;

use super::message;

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    // Build a single `Text` from all messages.
    let mut text = Text::default();
    for msg in &app.messages {
        message::render_into(&mut text, msg, app.collapsed_thinking);
    }

    // Compute scroll so the bottom of `text` is visible.
    // No borders to account for in Phase A — full area is usable.
    let total_lines = text.lines.len().max(1);
    let visible = area.height as usize;
    let max_offset = total_lines.saturating_sub(visible);
    let y = if app.auto_scroll {
        max_offset
    } else {
        app.scroll_offset.min(max_offset)
    };

    let paragraph = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .scroll((y as u16, 0));

    frame.render_widget(paragraph, area);
}
