//! Inline permission dialog overlay.
//!
//! Renders a centered modal dialog when the agent requests permission to
//! invoke a tool.  The dialog shows the tool name, the available options,
//! and keyboard hints for navigating / confirming.
//!
//! Phase A: softened border (DarkGray), title simplified to
//! `Permission Request` in default fg (no ⚠, no yellow).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::app::PermissionDialog;

/// Render the permission dialog as a centered overlay on top of the frame.
///
/// The dialog is ~60% wide × ~40% tall, centered in the given `area`.
/// A `Clear` widget is drawn first so the underlying content is hidden
/// behind the dialog.
pub fn render_permission_dialog(dialog: &PermissionDialog, area: Rect, frame: &mut Frame<'_>) {
    // Centre a ~60% × ~40% block inside `area`.
    let vertical = Layout::vertical([
        Constraint::Percentage(30),
        Constraint::Percentage(40),
        Constraint::Percentage(30),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .split(vertical[1]);

    let dialog_area = horizontal[1];

    // Clear underlying content.
    frame.render_widget(Clear, dialog_area);

    // Softened border: DarkGray instead of Yellow.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" Permission Request ", Style::default()));

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Tool name.
    lines.push(Line::from(vec![
        Span::styled("Tool: ", Style::default().fg(Color::Gray)),
        Span::styled(
            dialog.tool_name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    lines.push(Line::default()); // blank

    // Options list — selected item uses background highlight (no `> <` markers).
    for (i, opt) in dialog.options.iter().enumerate() {
        let icon = match opt.id.as_str() {
            "allow" | "accept" | "yes" => "✓",
            "deny" | "reject" | "no" => "✗",
            "allow_all" | "accept_all" => "⚡",
            _ => "•",
        };

        let is_selected = i == dialog.selected;
        let fg = if is_selected {
            Color::Black
        } else {
            Color::White
        };
        let bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{icon} {}", opt.label),
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
        ]);
        lines.push(line);
    }

    lines.push(Line::default()); // blank

    // Keyboard hints.
    lines.push(Line::from(vec![
        Span::styled("[↑↓]", Style::default().fg(Color::DarkGray)),
        Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[Enter]", Style::default().fg(Color::DarkGray)),
        Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[Esc]", Style::default().fg(Color::DarkGray)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, dialog_area);
}
