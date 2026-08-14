//! Application state and message types for the TUI.

use ratatui::style::{Modifier, Style};
use serde_json::Value;
use tui_textarea::TextArea;

use super::input::complete::{Completion, SlashCompleter};
use super::input::history::InputHistory;
use super::theme;
use super::widgets::agents::AgentStatus;

// Phase 3: re-export diff types for convenience.
pub use crate::ui::widgets::diff::DiffLine;

/// Default context window (tokens) — used when the model's true limit is
/// unknown. Claude 3.5/4 Sonnet and Opus have 200k; we use this as a safe
/// default for percentage calculations.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// A message displayed in the messages pane.
#[derive(Debug, Clone)]
pub enum Message {
    /// A message sent by the user.
    User { text: String },
    /// A streamed reply from the agent. `in_progress` is true while the turn
    /// is still running.
    Assistant {
        text: String,
        thinking: String,
        tool_calls: Vec<ToolCall>,
        in_progress: bool,
    },
    /// Informational system message (session opened, errors, help text…).
    System { text: String },
}

/// A tool invocation by the agent.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub output: Option<ToolOutput>,
    pub status: ToolStatus,
    pub expanded: bool,
    /// Phase 3: precomputed diff lines for edit/write tools (populated lazily).
    pub diff_lines: Option<Vec<DiffLine>>,
}

/// Output from a completed tool call.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

/// Status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Running,
    Success,
    Failed,
    /// Tool call was denied by the user (retained for future permission flow).
    #[allow(dead_code)]
    Denied,
}

/// State of the slash-completion popup.
#[derive(Debug, Clone)]
pub struct CompletionPopup {
    /// Current matching completions (always non-empty when popup is Some).
    pub items: Vec<Completion>,
    /// Index of the highlighted item.
    pub selected: usize,
}

impl CompletionPopup {
    /// Rebuild the popup from the current input. Returns `None` when there
    /// are no matches (caller should clear the popup).
    pub fn for_input(input: &str) -> Option<Self> {
        let items = SlashCompleter::complete(input);
        if items.is_empty() {
            None
        } else {
            Some(Self { items, selected: 0 })
        }
    }

    /// Move the highlight down by one.
    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// Move the highlight up by one.
    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = if self.selected == 0 {
                self.items.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Return the currently highlighted label (e.g. `/help`).
    pub fn selected_label(&self) -> Option<&str> {
        self.items.get(self.selected).map(|c| c.label.as_str())
    }
}

/// Token usage / cost tracker.
///
/// Updated on every `usage_update` ACP event. Pricing is model-specific; we
/// hard-code common Claude models and fall back to zero for unknowns.
#[derive(Debug, Clone, Default)]
pub struct UsageTracker {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub model: String,
}

impl UsageTracker {
    /// Apply a `usage_update` event's JSON data.
    ///
    /// The payload may arrive in several shapes depending on the ACP runtime.
    /// We defensively look for `input_tokens`, `output_tokens`,
    /// `cache_read_tokens`, `cache_write_tokens` (and a few camelCase aliases).
    pub fn apply(&mut self, data: &Value) {
        if let Some(n) = data["input_tokens"]
            .as_u64()
            .or_else(|| data["inputTokens"].as_u64())
        {
            self.input_tokens = self.input_tokens.saturating_add(n);
        }
        if let Some(n) = data["output_tokens"]
            .as_u64()
            .or_else(|| data["outputTokens"].as_u64())
        {
            self.output_tokens = self.output_tokens.saturating_add(n);
        }
        if let Some(n) = data["cache_read_tokens"]
            .as_u64()
            .or_else(|| data["cacheReadTokens"].as_u64())
        {
            self.cache_read_tokens = self.cache_read_tokens.saturating_add(n);
        }
        if let Some(n) = data["cache_write_tokens"]
            .as_u64()
            .or_else(|| data["cacheWriteTokens"].as_u64())
        {
            self.cache_write_tokens = self.cache_write_tokens.saturating_add(n);
        }
        if let Some(m) = data["model"].as_str() {
            if !m.is_empty() {
                self.model = m.to_string();
            }
        }
    }

    /// Compute the cost in USD based on the current model.
    ///
    /// Prices are per 1M tokens (August 2026 published rates).
    pub fn cost_usd(&self) -> f64 {
        let (in_price, out_price, cache_read_price, cache_write_price) = match self.model.as_str() {
            "claude-sonnet-4-20250514" | "claude-sonnet-4" => (3.0, 15.0, 0.30, 3.75),
            "claude-opus-4-20250514" | "claude-opus-4" => (15.0, 75.0, 1.50, 18.75),
            "claude-3-5-sonnet-20241022" | "claude-3-5-sonnet" | "claude-3-5-sonnet-v2" => {
                (3.0, 15.0, 0.30, 3.75)
            }
            "claude-3-5-haiku-20241022" | "claude-3-5-haiku" => (0.80, 4.0, 0.08, 1.0),
            "claude-3-opus-20240229" | "claude-3-opus" => (15.0, 75.0, 1.50, 18.75),
            _ => return 0.0,
        };
        (self.input_tokens as f64 * in_price / 1_000_000.0)
            + (self.output_tokens as f64 * out_price / 1_000_000.0)
            + (self.cache_read_tokens as f64 * cache_read_price / 1_000_000.0)
            + (self.cache_write_tokens as f64 * cache_write_price / 1_000_000.0)
    }

    /// Estimate context usage as a percentage (0..=100).
    pub fn context_percent(&self, context_window: u64) -> u8 {
        if context_window == 0 {
            return 0;
        }
        let total = self.input_tokens.saturating_add(self.output_tokens);
        ((total as f64 / context_window as f64) * 100.0).min(100.0) as u8
    }

    /// Format a human-readable cost string.
    pub fn format_cost(&self) -> String {
        let c = self.cost_usd();
        if c < 0.01 && self.input_tokens == 0 && self.output_tokens == 0 {
            "$0.00".to_string()
        } else if c < 0.01 {
            format!("${c:.4}")
        } else {
            format!("${c:.2}")
        }
    }

    /// Format a full breakdown string for `/cost`.
    pub fn format_breakdown(&self) -> String {
        format!(
            "Model: {}\n\
             Tokens — in: {}, out: {}, cache read: {}, cache write: {}\n\
             Context: ~{}% of {}k\n\
             Cost: {}",
            if self.model.is_empty() {
                "<unknown>"
            } else {
                &self.model
            },
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.context_percent(DEFAULT_CONTEXT_WINDOW),
            DEFAULT_CONTEXT_WINDOW / 1_000,
            self.format_cost(),
        )
    }
}

/// Top-level TUI state.
pub struct AppState<'a> {
    /// ACP session id (shown in the status bar).
    pub session_id: String,
    /// Human-readable agent name (shown in the status bar).
    pub agent_name: String,
    /// Messages in chronological order.
    pub messages: Vec<Message>,
    /// Number of lines to scroll the messages pane from the bottom.
    /// 0 means "show the latest content" (auto-scroll).
    pub scroll_offset: usize,
    /// Whether the messages pane should track the bottom as new content arrives.
    pub auto_scroll: bool,
    /// Multi-line input area.
    pub input: TextArea<'a>,
    /// Main-loop flag. Set to `false` to exit the TUI cleanly.
    pub running: bool,
    /// Set to `Some(text)` when the user presses Enter with non-empty input.
    /// The runner drains this and sends it to the agent.
    pub should_send: Option<String>,
    /// True while the agent is producing a streamed reply (used by renderer).
    pub agent_busy: bool,
    /// Tick counter snapshot when `agent_busy` last transitioned `false → true`.
    /// Used by the status bar to render a compact `(Ns)` elapsed indicator.
    pub busy_start_tick: u64,
    /// Phase 2: global toggle for thinking block visibility. True = collapsed.
    pub collapsed_thinking: bool,
    /// Phase 3: inline permission dialog (shown when agent requests permission).
    pub permission_dialog: Option<PermissionDialog>,
    /// Phase 3: pending permission response. The runner drains this after each
    /// event-loop iteration and sends `SessionCommand::PermissionResponse`.
    pub pending_permission_response: Option<(String, Option<String>)>,

    // ---- Phase 4 fields ----
    /// Persistent input history (↑/↓ browsing).
    pub input_history: InputHistory,
    /// Slash-completion popup (shown when input starts with `/`).
    pub completion_popup: Option<CompletionPopup>,
    /// Active agents (populated during DAG execution; empty in single-agent chat).
    pub active_agents: Vec<AgentStatus>,
    /// Whether the multi-agent panel is visible (toggled with Ctrl-N).
    pub show_agents_panel: bool,
    /// Current model name (set via `/model` or from `usage_update`).
    pub model: String,
    /// Token usage / cost tracker.
    pub usage: UsageTracker,
    /// Tick counter — incremented by the runner on every `Event::Tick`. Used
    /// to drive spinner animation.
    pub tick: u64,
}

/// A permission dialog awaiting user input.
#[derive(Debug, Clone)]
pub struct PermissionDialog {
    /// The ACP request id (used to correlate the response).
    pub request_id: String,
    /// Human-readable tool name (shown in the dialog header).
    pub tool_name: String,
    /// The options offered by the agent (e.g. Allow, Deny, Allow All).
    pub options: Vec<PermissionOption>,
    /// Index of the currently highlighted option.
    pub selected: usize,
}

/// A single option inside a [`PermissionDialog`].
#[derive(Debug, Clone)]
pub struct PermissionOption {
    /// Opaque option id forwarded to the agent in the response.
    pub id: String,
    /// Display label.
    pub label: String,
}

/// Create a new `TextArea` with the Phase A placeholder configured.
///
/// The placeholder is shown in `Color::DarkGray` italic when the textarea
/// is empty, giving users a hint about what the input is for without
/// requiring a border or title.
fn new_textarea_with_placeholder<'a>() -> TextArea<'a> {
    let mut textarea = TextArea::default();
    textarea.set_placeholder_text("Ask agent anything");
    textarea.set_placeholder_style(
        Style::default()
            .fg(theme::muted())
            .add_modifier(Modifier::ITALIC),
    );
    textarea
}

impl<'a> AppState<'a> {
    pub fn new(session_id: String, agent_name: String) -> Self {
        let input = new_textarea_with_placeholder();

        Self {
            session_id,
            agent_name,
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input,
            running: true,
            should_send: None,
            agent_busy: false,
            busy_start_tick: 0,
            collapsed_thinking: true,
            permission_dialog: None,
            pending_permission_response: None,
            // Phase 4
            input_history: InputHistory::load(),
            completion_popup: None,
            active_agents: Vec::new(),
            show_agents_panel: false,
            model: String::new(),
            usage: UsageTracker::default(),
            tick: 0,
        }
    }

    /// Push a user message and clear the input.
    pub fn push_user_message(&mut self, text: String) {
        self.messages.push(Message::User { text });
        self.reset_input();
        self.touch_scroll();
    }

    /// Transition to the busy state (agent is producing a streamed reply).
    /// Also records `busy_start_tick` so the status bar can show elapsed time.
    pub fn mark_busy(&mut self) {
        self.agent_busy = true;
        self.busy_start_tick = self.tick;
    }

    /// Start a new in-progress assistant message. Any subsequent
    /// `agent_message_chunk` events will append to it.
    pub fn start_assistant_message(&mut self) {
        self.messages.push(Message::Assistant {
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            in_progress: true,
        });
        self.mark_busy();
        self.touch_scroll();
    }

    /// Append a chunk of text to the current assistant message (if any).
    pub fn append_assistant_chunk(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Message::Assistant {
            text: buf,
            in_progress,
            ..
        }) = self.messages.last_mut()
        {
            if !*in_progress {
                // Last assistant message is finalized — start a new one.
                self.messages.push(Message::Assistant {
                    text: text.to_string(),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    in_progress: true,
                });
                self.mark_busy();
            } else {
                buf.push_str(text);
            }
        } else {
            // No assistant message yet — start one.
            self.messages.push(Message::Assistant {
                text: text.to_string(),
                thinking: String::new(),
                tool_calls: Vec::new(),
                in_progress: true,
            });
            self.mark_busy();
        }
        self.touch_scroll();
    }

    /// Append a chunk of thinking text to the current assistant message.
    pub fn append_thinking_chunk(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Message::Assistant {
            thinking,
            in_progress,
            ..
        }) = self.messages.last_mut()
        {
            if !*in_progress {
                // Last assistant message is finalized — start a new one.
                self.messages.push(Message::Assistant {
                    text: String::new(),
                    thinking: text.to_string(),
                    tool_calls: Vec::new(),
                    in_progress: true,
                });
                self.mark_busy();
            } else {
                thinking.push_str(text);
            }
        } else {
            // No assistant message yet — start one with just thinking.
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: text.to_string(),
                tool_calls: Vec::new(),
                in_progress: true,
            });
            self.mark_busy();
        }
        self.touch_scroll();
    }

    /// Add a tool call to the current assistant message.
    pub fn add_tool_call(&mut self, tc: ToolCall) {
        if let Some(Message::Assistant {
            tool_calls,
            in_progress,
            ..
        }) = self.messages.last_mut()
        {
            if !*in_progress {
                // Last assistant message is finalized — start a new one.
                self.messages.push(Message::Assistant {
                    text: String::new(),
                    thinking: String::new(),
                    tool_calls: vec![tc],
                    in_progress: true,
                });
                self.mark_busy();
            } else {
                tool_calls.push(tc);
            }
        } else {
            // No assistant message yet — start one.
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: String::new(),
                tool_calls: vec![tc],
                in_progress: true,
            });
            self.mark_busy();
        }
        self.touch_scroll();
    }

    /// Update a tool call's output by its id.
    pub fn update_tool_call(&mut self, id: &str, result: String, is_error: bool) {
        // Search all messages for a tool call with the matching id.
        for msg in self.messages.iter_mut().rev() {
            if let Message::Assistant { tool_calls, .. } = msg {
                for tc in tool_calls.iter_mut().rev() {
                    if tc.id == id {
                        tc.output = Some(ToolOutput {
                            text: result,
                            is_error,
                        });
                        tc.status = if is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Success
                        };
                        return;
                    }
                }
            }
        }
    }

    /// Mark the current assistant message as finished.
    pub fn finish_assistant_message(&mut self) {
        self.agent_busy = false;
        if let Some(Message::Assistant { in_progress, .. }) = self.messages.last_mut() {
            *in_progress = false;
        }
    }

    /// Toggle the global thinking block visibility.
    pub fn toggle_thinking(&mut self) {
        self.collapsed_thinking = !self.collapsed_thinking;
    }

    /// Push a system message (info / error / help).
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.messages.push(Message::System { text: text.into() });
        self.touch_scroll();
    }

    /// Clear the input area without adding its contents to the message list.
    pub fn reset_input(&mut self) {
        // tui-textarea has no built-in clear; replace the TextArea with a fresh one.
        self.input = new_textarea_with_placeholder();
        self.completion_popup = None;
        self.input_history.reset();
    }

    /// Phase 4: replace the entire textarea content with `text`.
    ///
    /// Used when recalling history entries or accepting a completion.
    pub fn replace_input(&mut self, text: &str) {
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        if lines.is_empty() {
            self.input = new_textarea_with_placeholder();
        } else {
            let last_line_idx = lines.len() - 1;
            let last_col = lines[last_line_idx].len();
            self.input = TextArea::new(lines);
            // Move cursor to the end of the last line.
            self.input.move_cursor(tui_textarea::CursorMove::Bottom);
            self.input.move_cursor(tui_textarea::CursorMove::End);
            // Suppress "unused" warnings if TextArea happens to use different
            // cursor APIs in a future version — the two moves above are
            // belt-and-braces; we don't actually need both locals.
            let _ = (last_line_idx, last_col);
        }
    }

    /// Phase 4: true when the cursor is on the first line of the input.
    pub fn is_cursor_at_top(&self) -> bool {
        let (row, _col) = self.input.cursor();
        row == 0
    }

    /// Phase 4: true when the cursor is on the last line of the input.
    pub fn is_cursor_at_bottom(&self) -> bool {
        let (row, _col) = self.input.cursor();
        let lines = self.input.lines();
        row + 1 >= lines.len()
    }

    /// Phase 4: true if any tool call in the latest assistant message is
    /// still running. Used to pick the spinner phase.
    pub fn has_running_tool(&self) -> bool {
        for msg in self.messages.iter().rev() {
            if let Message::Assistant { tool_calls, .. } = msg {
                return tool_calls.iter().any(|tc| tc.status == ToolStatus::Running);
            }
        }
        false
    }

    /// Phase 4: update the completion popup state based on current input.
    ///
    /// Called after every input change. When the input starts with `/` and
    /// there are matching commands, the popup is shown; otherwise it is
    /// hidden.
    pub fn update_completion_popup(&mut self) {
        let text: String = self.input.lines().join("\n");
        self.completion_popup = CompletionPopup::for_input(&text);
    }

    /// Phase 4: accept the currently highlighted completion.
    ///
    /// Replaces the input with the completed label (plus a trailing space)
    /// and hides the popup. Returns true if a completion was accepted.
    pub fn accept_completion(&mut self) -> bool {
        let label = match self
            .completion_popup
            .as_ref()
            .and_then(|p| p.selected_label())
        {
            Some(l) => l.to_string(),
            None => return false,
        };
        self.replace_input(&label);
        // Move cursor to end + insert a trailing space for ergonomics.
        self.input.move_cursor(tui_textarea::CursorMove::End);
        self.completion_popup = None;
        true
    }

    /// Reset scroll so the bottom of the pane is visible.
    fn touch_scroll(&mut self) {
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }

    /// Short session id for the status bar (first 8 chars).
    pub fn short_session_id(&self) -> &str {
        if self.session_id.len() > 8 {
            &self.session_id[..8]
        } else {
            &self.session_id
        }
    }
}
