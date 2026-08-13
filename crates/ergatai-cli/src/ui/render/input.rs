//! Input area (bottom rows): borderless tui-textarea with a single-line
//! horizontal separator above it (drawn by the caller in `root.rs`) acting as
//! the input box's top border.
//!
//! Phase A+ redesign:
//! - Textarea itself is borderless, with placeholder `"Ask agent anything"`.
//! - The row above the textarea is rendered as a `─` separator line by the
//!   caller (root.rs), giving the input region a visible top edge.
//! - Completion popup: no border, no title. Selected item uses background
//!   highlight (Black on Cyan) without the `▸ ` marker.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::ui::app::AppState;

/// The minimum height reserved for the input area (matches the layout in
/// `root.rs`): 1 row separator + 2 rows textarea. When the completion popup is
/// shown the caller should hand us a taller area.
pub const BASE_INPUT_HEIGHT: u16 = 3;

/// Render the input area. The top row is the separator line; the rest is the
/// textarea (with the completion popup overlayed just above the textarea).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    if area.height < BASE_INPUT_HEIGHT {
        return;
    }
    // Top row: horizontal separator acting as the input box's top border.
    let sep_height: u16 = 1;
    let sep_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: sep_height,
    };
    render_separator(frame, sep_area);

    // Remaining rows: textarea (+ optional completion popup above it).
    let content_area = Rect {
        x: area.x,
        y: area.y + sep_height,
        width: area.width,
        height: area.height - sep_height,
    };

    if let Some(popup) = &app.completion_popup {
        if !popup.items.is_empty() {
            // Popup gets enough rows for its items (capped).
            let popup_rows = popup.items.len().min(6) as u16;
            let popup_area = Rect {
                x: content_area.x,
                y: content_area.y,
                width: content_area.width,
                height: popup_rows.min(content_area.height),
            };
            let input_area = Rect {
                x: content_area.x,
                y: content_area.y + popup_area.height,
                width: content_area.width,
                height: content_area.height.saturating_sub(popup_area.height),
            };
            render_popup(frame, popup_area, app);
            render_textarea(frame, input_area, app);
            return;
        }
    }
    render_textarea(frame, content_area, app);
}

/// Render the single-line horizontal separator that forms the input box's
/// top edge. Uses `─` characters in DarkGray.
fn render_separator(frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    let line = "─".repeat(width);
    let paragraph = Paragraph::new(Span::styled(
        line,
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(paragraph, area);
}

/// Render just the textarea (no border, no title).
fn render_textarea(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    if area.height == 0 {
        return;
    }
    // The placeholder is configured on the TextArea itself (see AppState).
    frame.render_widget(&app.input, area);
}

/// Render the slash-completion popup as a borderless list.
fn render_popup(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    let popup = match &app.completion_popup {
        Some(p) => p,
        None => return,
    };
    if popup.items.is_empty() {
        return;
    }

    let items: Vec<ListItem> = popup
        .items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let is_selected = i == popup.selected;
            // No marker — selected item uses background highlight only.
            let label_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let line = Line::from(vec![
                Span::raw("  "), // consistent indent (replaces old marker column)
                Span::styled(format!("{:<10}", c.label), label_style),
                Span::styled(c.description.clone(), desc_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, area);
}
