//! TUI main loop: terminal setup/teardown, event dispatch, prompt sending.

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use ergatai_core::acp::manager::SessionCommand;

use super::app::AppState;
use super::event::{drain_acp_events, spawn_crossterm_pump, Event};
use super::render;
use crate::ui::input::{parse_input, ChatCommand};

/// RAII guard that puts the terminal into alt-screen + raw mode and restores
/// it on drop — including panics inside the TUI loop.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run the TUI. Blocks until the user quits or the session closes.
pub async fn run(
    app: &mut AppState<'static>,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    initial_message: Option<String>,
) -> Result<()> {
    let _guard = TerminalGuard::enter().context("Failed to enter TUI mode")?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to init ratatui terminal")?;
    terminal.clear().ok();

    // Welcome message.
    app.push_system(format!(
        "Connected to {}. /help for commands. Ctrl-C to quit. ↑/↓ history · / autocomplete · Ctrl-N agents",
        app.agent_name
    ));

    // Event pump (crossterm → channel).
    let term_rx = spawn_crossterm_pump();

    // Build a unified channel so we can drain ACP events into the same stream.
    let (unified_tx, mut unified_rx) = mpsc::unbounded_channel::<Event>();

    // Forward terminal events from the pump into the unified channel.
    let forward_tx = unified_tx.clone();
    tokio::spawn(async move {
        let mut rx = term_rx;
        while let Some(ev) = rx.recv().await {
            if forward_tx.send(ev).is_err() {
                return;
            }
        }
    });

    // Send initial message if provided.
    if let Some(msg) = initial_message {
        dispatch_prompt(app, &cmd_tx, &msg);
    }

    // Main loop.
    loop {
        // Render on every iteration.
        if let Err(e) = render::render_frame(&mut terminal, app) {
            app.push_system(format!("Render error: {e}"));
        }

        if !app.running {
            break;
        }

        // Drain pending ACP events into the unified channel.
        drain_acp_events(&unified_tx);

        // Wait for the next event (terminal, ACP, or tick).
        let ev = unified_rx.recv().await;
        match ev {
            Some(Event::Term(ct_ev)) => {
                let should_send = render::handle_term_event(app, ct_ev);
                if should_send {
                    if let Some(text) = app.should_send.take() {
                        dispatch_user_input(app, &cmd_tx, &text);
                    }
                }
            }
            Some(Event::Acp(acp_ev)) => {
                render::handle_acp_event(app, acp_ev);
            }
            Some(Event::Tick) => {
                // Phase 4: advance the tick counter for spinner animation.
                app.tick = app.tick.wrapping_add(1);
            }
            None => {
                // Channel closed — exit.
                break;
            }
        }

        // Phase 3: drain any pending permission response from the app state
        // and forward it to the ACP session.
        if let Some((request_id, option_id)) = app.pending_permission_response.take() {
            let _ = cmd_tx.send(SessionCommand::PermissionResponse {
                request_id,
                option_id,
            });
        }
    }

    // Clean up: close the ACP session.
    let _ = cmd_tx.send(SessionCommand::Close);

    Ok(())
}

/// Handle a user-typed string: either a slash command or a prompt to send.
fn dispatch_user_input(
    app: &mut AppState<'_>,
    cmd_tx: &mpsc::UnboundedSender<SessionCommand>,
    text: &str,
) {
    match parse_input(text) {
        ChatCommand::Quit => {
            app.running = false;
        }
        ChatCommand::Help => {
            app.push_system(help_text());
        }
        ChatCommand::Clear => {
            app.messages.clear();
            app.scroll_offset = 0;
        }
        ChatCommand::Agents => {
            app.push_system(
                "Listing agents is not yet supported inside the TUI. Exit and run `ergatai agents list`.",
            );
        }
        ChatCommand::Status => {
            app.push_system("Status is not yet supported inside the TUI.");
        }
        ChatCommand::Switch(target) => {
            app.push_system(format!(
                "Switching agents is not yet supported inside the TUI. Exit and re-run with --agent {target}."
            ));
        }
        // Phase 4: new commands.
        ChatCommand::Model(name) => handle_model(app, name),
        ChatCommand::Cost => handle_cost(app),
        ChatCommand::Compact => handle_compact(app),
        ChatCommand::SendPrompt(prompt) => {
            dispatch_prompt(app, cmd_tx, &prompt);
        }
    }
}

/// Phase 4: handle `/model` — show or set the model name.
fn handle_model(app: &mut AppState<'_>, name: Option<String>) {
    match name {
        Some(n) => {
            app.model = n.clone();
            app.usage.model = n.clone();
            app.push_system(format!("Model set to '{n}'. Note: this is a display label only — it does not change the agent's actual model."));
        }
        None => {
            let display = if app.model.is_empty() {
                "<not set>".to_string()
            } else {
                app.model.clone()
            };
            app.push_system(format!("Current model: {display}"));
        }
    }
}

/// Phase 4: handle `/cost` — show session cost + token breakdown.
fn handle_cost(app: &mut AppState<'_>) {
    app.push_system(app.usage.format_breakdown());
}

/// Phase 4: handle `/compact` — placeholder.
fn handle_compact(app: &mut AppState<'_>) {
    app.push_system("Compaction is not yet implemented. This will eventually compact the conversation context to free up the context window.");
}

/// Push a user message, start an assistant message placeholder, and send the
/// prompt over the ACP channel.
fn dispatch_prompt(
    app: &mut AppState<'_>,
    cmd_tx: &mpsc::UnboundedSender<SessionCommand>,
    text: &str,
) {
    app.push_user_message(text.to_string());
    app.start_assistant_message();

    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    if let Err(e) = cmd_tx.send(SessionCommand::SendPrompt {
        text: text.to_string(),
        reply_tx,
    }) {
        app.finish_assistant_message();
        app.push_system(format!("Failed to send prompt: {e}"));
    }
}

fn help_text() -> String {
    [
        "Available commands:",
        "  /help, /h      — show this help",
        "  /quit, /q      — exit the chat",
        "  /clear         — clear the messages pane",
        "  /agents        — list agents (not in TUI yet)",
        "  /switch <name> — switch agents (not in TUI yet)",
        "  /status        — show status (not in TUI yet)",
        "  /model [name]  — show or set the model (display only)",
        "  /cost          — show session cost and token usage",
        "  /compact       — compact conversation context (not yet implemented)",
        "",
        "Type a message and press Enter to send. Shift-Enter for newline.",
        "↑/↓ to browse history. / to autocomplete commands. Ctrl-N for agents panel.",
    ]
    .join("\n")
}
