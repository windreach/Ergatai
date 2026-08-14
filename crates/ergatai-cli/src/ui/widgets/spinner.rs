//! Spinner states for thinking / tool execution, plus the shimmer animation
//! used on the status bar's "working" label.
//!
//! Phase 4 polish: provides animated indicator strings and status-line
//! helpers for showing progress while the agent is thinking or running a
//! tool. The TUI doesn't actually animate frame-by-frame (events are
//! tick-driven at ~50ms) so we expose a small set of static frames the
//! caller can cycle through based on a tick counter or timestamp.
//!
//! Phase B adds [`shimmer_spans`] — a codex-style sweep animation that
//! blends a highlight color across a run of characters. Falls back to a
//! bold/dim alternation when truecolor is unavailable.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme;

/// The set of spinner frames used for the "thinking" indicator.
///
/// Each frame is a short glyph string rendered next to the agent name.
pub const THINKING_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The set of spinner frames used for the "tool running" indicator.
pub const TOOL_FRAMES: &[&str] = &[
    "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄", "▃", "▂",
];

/// Pick a frame from a cycle based on a tick counter.
pub fn frame_at(frames: &[&'static str], tick: u64) -> &'static str {
    if frames.is_empty() {
        return "";
    }
    let idx = (tick as usize) % frames.len();
    frames[idx]
}

/// Current high-level spinner phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerPhase {
    /// Agent is thinking (producing text).
    Thinking,
    /// Agent is executing a tool.
    Tool,
    /// No spinner shown (idle).
    Idle,
}

impl SpinnerPhase {
    /// Return the spinner frame for this phase at the given tick.
    pub fn frame(self, tick: u64) -> &'static str {
        match self {
            SpinnerPhase::Thinking => frame_at(THINKING_FRAMES, tick),
            SpinnerPhase::Tool => frame_at(TOOL_FRAMES, tick),
            SpinnerPhase::Idle => "",
        }
    }

    /// Style applied to the spinner frame.
    pub fn style(self) -> Style {
        match self {
            SpinnerPhase::Thinking => Style::default().fg(theme::thinking()),
            SpinnerPhase::Tool => Style::default().fg(theme::warning()),
            SpinnerPhase::Idle => Style::default(),
        }
    }
}

/// Build a styled [`Line`] with the spinner frame + optional label.
///
/// Returns an empty line when phase is `Idle`.
pub fn spinner_line(phase: SpinnerPhase, tick: u64, label: &str) -> Line<'static> {
    if phase == SpinnerPhase::Idle {
        return Line::default();
    }
    let frame = phase.frame(tick);
    let mut spans = vec![Span::styled(format!("{frame} "), phase.style())];
    if !label.is_empty() {
        spans.push(Span::styled(
            label.to_string(),
            Style::default().fg(theme::muted()),
        ));
    }
    Line::from(spans)
}

/// Detect the current spinner phase from TUI state.
///
/// This is a convenience helper for the status bar. `busy` is the app's
/// `agent_busy` flag; `has_tool_running` is true if any tool call in the
/// latest assistant message has status `Running`.
pub fn detect_phase(busy: bool, has_tool_running: bool) -> SpinnerPhase {
    if !busy {
        return SpinnerPhase::Idle;
    }
    if has_tool_running {
        SpinnerPhase::Tool
    } else {
        SpinnerPhase::Thinking
    }
}

// ---------------------------------------------------------------------------
// Phase B: shimmer animation
// ---------------------------------------------------------------------------

/// Sweep period in seconds.
const SHIMMER_PERIOD_SECS: f32 = 2.0;
/// Width of the brightness band on each side of the sweep centre (in chars).
const SHIMMER_BAND_HALF_WIDTH: f32 = 5.0;
/// Padding of the sweep area beyond the text length.
const SHIMMER_PADDING: usize = 10;

/// Render `text` with a codex-style shimmer sweep.
///
/// `tick` is the app's 50ms tick counter; `truecolor` controls whether we
/// emit per-character RGB colors (smooth blend) or fall back to bold/dim
/// alternation (ANSI-16 friendly).
pub fn shimmer_spans(text: &str, tick: u64, truecolor: bool) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![];
    }
    let n = chars.len();
    let period = n + SHIMMER_PADDING * 2;
    // One tick = 50ms.
    let secs = (tick as f32) * 0.05;
    let pos_f = (secs % SHIMMER_PERIOD_SECS) / SHIMMER_PERIOD_SECS * (period as f32);

    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let ip = (i + SHIMMER_PADDING) as f32;
            let dist = (ip - pos_f).abs();
            let intensity = if dist <= SHIMMER_BAND_HALF_WIDTH {
                let x = std::f32::consts::PI * (dist / SHIMMER_BAND_HALF_WIDTH);
                0.5 * (1.0 + x.cos())
            } else {
                0.0
            };
            let style = if truecolor {
                // Blend from base (DarkGray ~80) to highlight (White 255).
                let base: f32 = 80.0;
                let peak: f32 = 255.0;
                let v = (base + (peak - base) * intensity * 0.9) as u8;
                Style::default()
                    .fg(ratatui::style::Color::Rgb(v, v, v))
                    .add_modifier(Modifier::BOLD)
            } else if intensity > 0.5 {
                Style::default()
                    .fg(ratatui::style::Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if intensity > 0.0 {
                Style::default().fg(theme::warning())
            } else {
                Style::default().fg(theme::muted())
            };
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

/// Format an elapsed-seconds count in compact `Ns` / `Nm Ns` / `Nh Nm Ns`
/// form (codex-style).
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_at_wraps() {
        assert_eq!(frame_at(THINKING_FRAMES, 0), "⠋");
        assert_eq!(frame_at(THINKING_FRAMES, THINKING_FRAMES.len() as u64), "⠋");
    }

    #[test]
    fn test_frame_at_empty() {
        assert_eq!(frame_at(&[], 0), "");
    }

    #[test]
    fn test_detect_phase() {
        assert_eq!(detect_phase(false, false), SpinnerPhase::Idle);
        assert_eq!(detect_phase(true, false), SpinnerPhase::Thinking);
        assert_eq!(detect_phase(true, true), SpinnerPhase::Tool);
        // Tool running but not busy shouldn't normally happen — but we
        // prioritise the idle flag.
        assert_eq!(detect_phase(false, true), SpinnerPhase::Idle);
    }

    #[test]
    fn test_spinner_line_idle_is_empty() {
        let line = spinner_line(SpinnerPhase::Idle, 0, "");
        assert!(line.spans.is_empty());
    }

    #[test]
    fn test_spinner_line_thinking_has_frame() {
        let line = spinner_line(SpinnerPhase::Thinking, 0, "thinking");
        assert_eq!(line.spans.len(), 2);
        assert!(line.spans[0].content.contains('⠋'));
    }

    #[test]
    fn test_fmt_elapsed_compact() {
        assert_eq!(fmt_elapsed_compact(0), "0s");
        assert_eq!(fmt_elapsed_compact(59), "59s");
        assert_eq!(fmt_elapsed_compact(60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(65), "1m 05s");
        assert_eq!(fmt_elapsed_compact(3600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3661), "1h 01m 01s");
    }

    #[test]
    fn test_shimmer_spans_count_matches_chars() {
        let spans = shimmer_spans("Working", 0, true);
        assert_eq!(spans.len(), "Working".chars().count());
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "Working"
        );
    }
}
