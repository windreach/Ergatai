//! Inline permission dialog overlay.
//!
//! Renders a centered modal dialog when the agent requests permission to
//! invoke a tool.  The dialog shows the tool name, the available options,
//! and keyboard hints for navigating / confirming.
//!
//! Uses `tui_widgets::popup::Popup` (re-export of `tui-popup`) for
//! auto-centering, background clearing, and border rendering.  The content
//! (tool name + option list + hints) is built as a `Text` and passed as the
//! popup body.  Keyboard navigation (j/k/Enter/Esc) is handled by
//! `handle_permission_key` in `render/mod.rs`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::Frame;
use tui_widgets::popup::Popup;

use crate::ui::app::PermissionDialog;
use crate::ui::theme;

/// Render the permission dialog as a centered overlay on top of the frame.
///
/// `tui-popup` handles centering, clearing the underlying content, and
/// drawing the border + title.  We just build the body text.
pub fn render_permission_dialog(dialog: &PermissionDialog, area: Rect, frame: &mut Frame<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Tool name.
    lines.push(Line::from(vec![
        Span::styled("Tool: ", Style::default().fg(theme::dim())),
        Span::styled(
            dialog.tool_name.clone(),
            Style::default()
                .fg(theme::accent())
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
            theme::highlight_fg()
        } else {
            theme::default_fg()
        };
        let bg = if is_selected {
            theme::accent()
        } else {
            ratatui::style::Color::Reset
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
        Span::styled("[↑↓]", Style::default().fg(theme::muted())),
        Span::styled(" navigate  ", Style::default().fg(theme::muted())),
        Span::styled("[Enter]", Style::default().fg(theme::muted())),
        Span::styled(" confirm  ", Style::default().fg(theme::muted())),
        Span::styled("[Esc]", Style::default().fg(theme::muted())),
        Span::styled(" cancel", Style::default().fg(theme::muted())),
    ]));

    let body = Text::from(lines);
    let popup = Popup::new(body)
        .title(" Permission Request ")
        .border_style(Style::default().fg(theme::muted()));
    frame.render_widget(popup, area);
}
