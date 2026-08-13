//! Render pipeline for the TUI. Each sub-module owns one region of the screen.

pub mod footer;
pub mod input;
pub mod message;
pub mod messages;
pub mod root;
pub mod status;

use anyhow::Result;
use crossterm::event::Event as CtEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::app::{AppState, PermissionDialog, PermissionOption};

/// Redraw the entire screen from `app` state.
pub fn render_frame(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &AppState<'_>,
) -> Result<()> {
    terminal.draw(|frame| {
        root::render(frame, frame.area(), app);
    })?;
    Ok(())
}

/// Handle a crossterm event by mutating `app`. Returns `true` if the runner
/// should attempt to send the user's input to the agent after handling.
pub fn handle_term_event(app: &mut AppState<'_>, ev: CtEvent) -> bool {
    use crossterm::event::Event;

    match ev {
        Event::Key(key) => {
            // Phase 3: permission dialog takes priority over all other keys.
            if app.permission_dialog.is_some() {
                return handle_permission_key(app, key);
            }
            handle_key(app, key)
        }
        Event::Resize(_, _) => {
            // ratatui redraws on the next tick automatically.
            false
        }
        _ => false,
    }
}

/// Key-event dispatch. Returns `true` if `app.should_send` was populated and
/// the runner should dispatch the prompt to the agent.
fn handle_key(app: &mut AppState<'_>, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Global shortcuts first (only when completion popup is NOT shown).
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                // Ctrl-C → quit cleanly.
                app.running = false;
                return false;
            }
            KeyCode::Char('l') => {
                // Ctrl-L → clear messages (keep session).
                app.messages.clear();
                app.scroll_offset = 0;
                return false;
            }
            KeyCode::Char('t') => {
                // Ctrl-T → toggle thinking block visibility.
                app.toggle_thinking();
                return false;
            }
            KeyCode::Char('n') => {
                // Phase 4: Ctrl-N → toggle multi-agent panel.
                app.show_agents_panel = !app.show_agents_panel;
                return false;
            }
            _ => {}
        }
    }

    // Phase 4: when the completion popup is visible, intercept navigation keys.
    if app.completion_popup.is_some() {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                if let Some(popup) = app.completion_popup.as_mut() {
                    popup.next();
                }
                return false;
            }
            KeyCode::BackTab | KeyCode::Up => {
                // Only intercept Up when popup is visible; otherwise we'd
                // break history browsing.
                if let Some(popup) = app.completion_popup.as_mut() {
                    popup.prev();
                }
                return false;
            }
            KeyCode::Enter => {
                // Accept completion instead of sending.
                if app.accept_completion() {
                    // After accepting, the textarea now contains the command
                    // token. User needs to add args and press Enter again.
                    return false;
                }
            }
            KeyCode::Esc => {
                app.completion_popup = None;
                return false;
            }
            _ => {}
        }
    }

    // Phase 4: ↑/↓ history navigation when cursor at top/bottom.
    match key.code {
        KeyCode::Up if app.is_cursor_at_top() && app.completion_popup.is_none() => {
            let current = app.input.lines().join("\n");
            let prev_text = app.input_history.prev(&current).map(|s| s.to_string());
            if let Some(text) = prev_text {
                app.replace_input(&text);
            }
            return false;
        }
        KeyCode::Down if app.is_cursor_at_bottom() && app.completion_popup.is_none() => {
            let next_text = app.input_history.next().map(|s| s.to_string());
            if let Some(text) = next_text {
                app.replace_input(&text);
            }
            return false;
        }
        _ => {}
    }

    // Input-area shortcuts.
    if key.code == KeyCode::Enter {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            // Shift-Enter → insert newline (let textarea handle it).
            app.input.input(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ));
            // Update completion popup after input change.
            app.update_completion_popup();
            return false;
        }
        // Plain Enter → send (if non-empty).
        let text: String = app.input.lines().join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return false;
        }
        // Record into history before clearing.
        app.input_history.add(&trimmed);
        app.completion_popup = None;
        app.should_send = Some(trimmed);
        return true;
    }

    // Everything else → let the textarea handle it.
    app.input.input(key);
    // Phase 4: refresh completion popup after every input change.
    app.update_completion_popup();
    false
}

/// Apply one ACP event to `app`.
pub fn handle_acp_event(app: &mut AppState<'_>, ev: ergatai_core::acp::manager::NapiSessionEvent) {
    match ev.event_type.as_str() {
        "agent_message_chunk" => {
            let text = extract_text(&ev.data);
            if !text.is_empty() {
                app.append_assistant_chunk(&text);
            }
        }
        "agent_thought_chunk" => {
            let text = extract_text(&ev.data);
            if !text.is_empty() {
                app.append_thinking_chunk(&text);
            }
        }
        "tool_call" => {
            if let Ok(tc) = parse_tool_call(&ev.data) {
                app.add_tool_call(tc);
            }
        }
        "tool_call_update" => {
            if let Some((id, result, is_error)) = parse_tool_call_update(&ev.data) {
                app.update_tool_call(&id, result, is_error);
            }
        }
        "usage_update" => {
            // Phase 4: feed the usage tracker.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                app.usage.apply(&v);
                // Sync the model field if the event reported one.
                if !app.usage.model.is_empty() && app.model.is_empty() {
                    app.model = app.usage.model.clone();
                }
            }
        }
        "permission_request" => {
            // Phase 3: build an inline permission dialog.
            if let Some(dialog) = parse_permission_request(&ev.data) {
                app.permission_dialog = Some(dialog);
            }
        }
        "closed" => {
            app.finish_assistant_message();
            app.push_system("Session closed by agent.");
        }
        _ => {}
    }
}

/// Extract the text payload from an ACP event's JSON `data` field.
fn extract_text(data: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    if let Some(s) = v["content"].as_str() {
        return s.to_string();
    }
    if let Some(s) = v["ContentBlock"]["text"].as_str() {
        return s.to_string();
    }
    if let Some(obj) = v["content"].as_object() {
        if let Some(s) = obj.get("text").and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

/// Parse a `tool_call` ACP event into a ToolCall struct.
///
/// Phase 3: for `edit` / `write` / `replace_in_file` tools, pre-compute
/// `diff_lines` from the input's `old_str` / `new_str` (or `content` vs
/// existing file) so the tool card can render an inline diff.
fn parse_tool_call(data: &str) -> anyhow::Result<super::app::ToolCall> {
    let v: serde_json::Value = serde_json::from_str(data)?;

    let name = v["tool_name"]
        .as_str()
        .or_else(|| v["toolName"].as_str())
        .unwrap_or("unknown")
        .to_string();

    let id = v["tool_call_id"]
        .as_str()
        .or_else(|| v["toolCallId"].as_str())
        .unwrap_or("")
        .to_string();

    let input = if !v["tool_input"].is_null() {
        v["tool_input"].clone()
    } else {
        v["toolInput"].clone()
    };

    // Phase 3: compute diff lines for edit-like tools.
    let diff_lines = compute_tool_diff(&name, &input);

    Ok(super::app::ToolCall {
        id,
        name,
        input,
        output: None,
        status: super::app::ToolStatus::Running,
        expanded: false,
        diff_lines,
    })
}

/// Compute diff lines for edit/write tools, if possible.
///
/// - `edit` / `replace_in_file`: diff between `old_str` and `new_str`.
/// - `write` / `write_to_file`: diff between existing file content (if
///   readable) and `content`.
///
/// Returns `None` for other tools or when no diff can be derived.
fn compute_tool_diff(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<Vec<super::app::DiffLine>> {
    use crate::ui::widgets::diff::compute_diff;

    match tool_name {
        "edit" | "replace_in_file" => {
            let old = input
                .get("old_str")
                .and_then(|v| v.as_str())
                .or_else(|| input.get("oldString").and_then(|v| v.as_str()))?;
            let new = input
                .get("new_str")
                .and_then(|v| v.as_str())
                .or_else(|| input.get("newString").and_then(|v| v.as_str()))?;
            let lines = compute_diff(old, new);
            if lines.is_empty() {
                None
            } else {
                Some(lines)
            }
        }
        "write" | "write_to_file" | "file_write" => {
            let content = input.get("content").and_then(|v| v.as_str())?;
            let path = input
                .get("file_path")
                .or_else(|| input.get("path"))
                .and_then(|v| v.as_str())?;
            // If we can read the current file, diff against it.
            let old = std::fs::read_to_string(path).ok()?;
            let lines = compute_diff(&old, content);
            if lines.is_empty() {
                None
            } else {
                Some(lines)
            }
        }
        _ => None,
    }
}

/// Parse a `tool_call_update` ACP event into (id, result, is_error).
fn parse_tool_call_update(data: &str) -> Option<(String, String, bool)> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;

    let id = v["tool_call_id"]
        .as_str()
        .or_else(|| v["toolCallId"].as_str())?
        .to_string();

    let result = v["result"]
        .as_str()
        .or_else(|| v["output"].as_str())
        .unwrap_or("")
        .to_string();

    let is_error = v["is_error"].as_bool().unwrap_or(false);

    Some((id, result, is_error))
}

/// Parse a `permission_request` ACP event into a [`PermissionDialog`].
///
/// The event payload is a serialised [`ergatai_core::acp::manager::NapiPermissionRequest`]
/// with `session_id`, `request_id`, `tool_name` (optional), and `options`
/// (each with `option_id` + `label`).
fn parse_permission_request(data: &str) -> Option<PermissionDialog> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;

    let request_id = v["request_id"]
        .as_str()
        .or_else(|| v["requestId"].as_str())?
        .to_string();

    let tool_name = v["tool_name"]
        .as_str()
        .or_else(|| v["toolName"].as_str())
        .unwrap_or("tool")
        .to_string();

    let options: Vec<PermissionOption> = v["options"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    let id = o["option_id"]
                        .as_str()
                        .or_else(|| o["optionId"].as_str())?
                        .to_string();
                    let label = o["label"].as_str().unwrap_or(&id).to_string();
                    Some(PermissionOption { id, label })
                })
                .collect()
        })
        .unwrap_or_default();

    // Fallback: if no options came through, expose Allow / Deny so the
    // dialog is still usable.
    let options = if options.is_empty() {
        vec![
            PermissionOption {
                id: "allow".to_string(),
                label: "Allow".to_string(),
            },
            PermissionOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
            },
        ]
    } else {
        options
    };

    Some(PermissionDialog {
        request_id,
        tool_name,
        options,
        selected: 0,
    })
}

/// Key handler used while the permission dialog is open.  Returns `false`
/// because the user is not sending a chat prompt — they are responding to
/// the dialog.
fn handle_permission_key(app: &mut AppState<'_>, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;

    let dialog = match app.permission_dialog.as_mut() {
        Some(d) => d,
        None => return false,
    };

    let max_idx = dialog.options.len().saturating_sub(1);

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            dialog.selected = dialog.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            dialog.selected = (dialog.selected + 1).min(max_idx);
        }
        KeyCode::Enter => {
            let option_id = dialog.options.get(dialog.selected).map(|o| o.id.clone());
            let request_id = dialog.request_id.clone();
            app.permission_dialog = None;
            app.pending_permission_response = Some((request_id, option_id));
        }
        KeyCode::Esc => {
            let request_id = dialog.request_id.clone();
            app.permission_dialog = None;
            // Esc → cancel: respond with no option selected.
            app.pending_permission_response = Some((request_id, None));
        }
        _ => {}
    }
    false
}
