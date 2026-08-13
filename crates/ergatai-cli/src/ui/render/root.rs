//! Top-level layout: status bar | messages | footer + input + overlays.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::ui::app::AppState;
use crate::ui::widgets::agents::render_agents_panel;
use crate::ui::widgets::permission::render_permission_dialog;

use super::{footer, input, messages, status};

/// Render the regions of the TUI.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    // Compute how much vertical space the input area needs.
    // Base is 3 rows; the popup adds rows + 1 separator row.
    let input_height = input_height_for(app);

    // When the agents panel is visible and we have multiple agents, split
    // the messages region horizontally to carve out a side panel.
    let show_agents = app.show_agents_panel && app.active_agents.len() > 1;
    let agents_width: u16 = if show_agents { 28 } else { 0 };

    // Vertical split:
    //   status (1) | messages (flex) | footer (1) | input (variable)
    //
    // The input area's *first* row is drawn as a `─` separator (see
    // `input.rs`), so together with the footer above it the user sees a
    // clearly-bounded input box even though the textarea itself is borderless.
    // No gap rows — footer sits flush against the separator for a tight,
    // codex-style composition.
    let vchunks = Layout::vertical([
        Constraint::Length(1),            // status bar
        Constraint::Min(3),               // messages pane (and agents side panel)
        Constraint::Length(1),            // footer (flush with input separator)
        Constraint::Length(input_height), // input area: separator row + textarea rows
    ])
    .split(area);

    status::render(frame, vchunks[0], app);

    // Optionally split the middle region horizontally for the agents panel.
    if show_agents {
        let hchunks = Layout::horizontal([Constraint::Min(10), Constraint::Length(agents_width)])
            .split(vchunks[1]);
        messages::render(frame, hchunks[0], app);
        render_agents_panel(frame, hchunks[1], &app.active_agents);
    } else {
        messages::render(frame, vchunks[1], app);
    }

    footer::render(frame, vchunks[2], app);
    input::render(frame, vchunks[3], app);

    // Phase 3: overlay the permission dialog (if any) on top of everything.
    if let Some(dialog) = &app.permission_dialog {
        render_permission_dialog(dialog, area, frame);
    }
}

/// Compute the input area height based on whether the completion popup is shown.
///
/// Base is 3 rows: 1 separator + 2 textarea rows. When the popup is shown it
/// sits inside the input area just above the textarea, so we add its row count
/// to the base. (The textarea's own top separator doubles as the popup's
/// bottom visual boundary, so no extra row is needed.)
fn input_height_for(app: &AppState<'_>) -> u16 {
    let base: u16 = input::BASE_INPUT_HEIGHT;
    if let Some(popup) = &app.completion_popup {
        if !popup.items.is_empty() {
            let popup_rows = popup.items.len().min(6) as u16;
            return base + popup_rows;
        }
    }
    base
}
