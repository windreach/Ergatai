//! Status bar (top line): compact text-first layout with shimmer.
//!
//! Phase A redesign: no background, no bold. Dim separators with typographic
//! hierarchy.
//!
//! Phase B (codex-style) upgrades:
//! - Busy indicator uses a **shimmer sweep** on the word `working` instead of
//!   a plain yellow bullet.
//! - Elapsed time is shown inline in compact `Ns` / `Nm Ns` / `Nh Nm Ns`
//!   form (derived from the app's tick counter since `agent_busy` was set).
//!
//! Format (busy):
//! ```text
//! ⠋ working (5s)  ·  claude  ·  a1b2c3d4  ·  sonnet  ·  45%  ·  $0.12
//! ```
//!
//! Format (idle):
//! ```text
//! claude  ·  a1b2c3d4  ·  sonnet  ·  45%  ·  $0.12
//! ```

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::app::AppState;
use crate::ui::widgets::spinner::{fmt_elapsed_compact, shimmer_spans, spinner_line};

/// Separator between segments.
const SEP: &str = "  ·  ";

/// One tick in the app loop ≈ 50ms.
const TICK_MS: u64 = 50;

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &AppState<'_>) {
    let dim = Style::default().fg(Color::DarkGray);
    let cyan = Style::default().fg(Color::Cyan);
    let bold_default = Style::default().add_modifier(Modifier::BOLD);

    let truecolor = is_truecolor();

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Busy indicator: spinner frame + shimmer "working" + compact elapsed.
    if app.agent_busy {
        // Spinner frame (animated braille/bar).
        let phase =
            crate::ui::widgets::spinner::detect_phase(app.agent_busy, app.has_running_tool());
        let spinner = spinner_line(phase, app.tick, "");
        let spinner_text: String = spinner.spans.iter().map(|s| s.content.as_ref()).collect();
        if !spinner_text.is_empty() {
            for s in spinner.spans {
                spans.push(s);
            }
            spans.push(Span::raw(" "));
        }
        // Shimmer "working".
        for s in shimmer_spans("working", app.tick, truecolor) {
            spans.push(s);
        }
        // Elapsed (compact).
        let elapsed_ticks = app.tick.saturating_sub(app.busy_start_tick);
        let elapsed_secs = (elapsed_ticks * TICK_MS) / 1000;
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("({})", fmt_elapsed_compact(elapsed_secs)),
            dim,
        ));
    }

    // Agent name (bold, default fg).
    if !spans.is_empty() {
        spans.push(Span::styled(SEP, dim));
    }
    spans.push(Span::styled(app.agent_name.clone(), bold_default));

    // Session id (cyan, NOT bold).
    spans.push(Span::styled(SEP, dim));
    spans.push(Span::styled(app.short_session_id().to_string(), cyan));

    // Model name (short form, dim).
    let model_short = short_model_name(&app.model);
    if !model_short.is_empty() {
        spans.push(Span::styled(SEP, dim));
        spans.push(Span::styled(model_short, dim));
    }

    // Context % (only when we have token data).
    let ctx = app
        .usage
        .context_percent(crate::ui::app::DEFAULT_CONTEXT_WINDOW);
    if !(app.usage.input_tokens == 0 && app.usage.output_tokens == 0) {
        spans.push(Span::styled(SEP, dim));
        spans.push(Span::styled(format!("{ctx}%"), dim));
    }

    // Cost.
    let cost = app.usage.format_cost();
    if !(app.usage.input_tokens == 0 && app.usage.output_tokens == 0) {
        spans.push(Span::styled(SEP, dim));
        spans.push(Span::styled(cost, dim));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Heuristic: does this terminal support truecolor (24-bit RGB)?
///
/// Crossterm exposes this via `capabilities().has(ColorCapabilities::TrueColor)`
/// in newer versions, but the cheap portable check is the `COLORTERM` env
/// variable (set by modern terminal emulators to `truecolor` or `24bit`).
fn is_truecolor() -> bool {
    match std::env::var("COLORTERM") {
        Ok(v) => v == "truecolor" || v == "24bit",
        Err(_) => false,
    }
}

/// Shorten a model id for compact display in the status bar.
///
/// - `claude-sonnet-4-20250514` → `sonnet`
/// - `claude-opus-4-20250514` → `opus`
/// - `claude-3-5-sonnet-20241022` → `sonnet`
/// - `claude-3-5-haiku-20241022` → `haiku`
/// - Anything with `gpt-4` → `gpt4`
/// - Otherwise returns the string unchanged (or empty if input is empty).
fn short_model_name(model: &str) -> String {
    if model.is_empty() {
        return String::new();
    }
    let m = model.to_lowercase();
    if m.contains("opus") {
        "opus".to_string()
    } else if m.contains("sonnet") {
        "sonnet".to_string()
    } else if m.contains("haiku") {
        "haiku".to_string()
    } else if m.contains("gpt-4") || m.contains("gpt4") {
        "gpt4".to_string()
    } else if m.contains("gpt-3") || m.contains("gpt3") {
        "gpt3".to_string()
    } else if m.contains("o1") {
        "o1".to_string()
    } else if m.contains("o3") {
        "o3".to_string()
    } else {
        // Return up to 16 chars.
        if m.len() > 16 {
            m[..16].to_string()
        } else {
            m
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_model_name() {
        assert_eq!(short_model_name("claude-sonnet-4-20250514"), "sonnet");
        assert_eq!(short_model_name("claude-opus-4"), "opus");
        assert_eq!(short_model_name("claude-3-5-haiku-20241022"), "haiku");
        assert_eq!(short_model_name("gpt-4-turbo"), "gpt4");
        assert_eq!(short_model_name(""), "");
        assert_eq!(
            short_model_name("some-unknown-model-xyz"),
            "some-unknown-mod"
        );
    }

    #[test]
    fn test_is_truecolor_runs() {
        // Just ensure the helper runs without panicking regardless of env.
        let _ = is_truecolor();
    }
}
