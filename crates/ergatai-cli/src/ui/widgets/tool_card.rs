//! Tool call card rendering widget (codex-style).
//!
//! Phase B redesign (mirrors codex's `exec_cell/render.rs`):
//!
//! - **Collapsed card**: a colored bullet `•` encodes status, followed by
//!   `Ran`/`Running` as a bold verb, then the tool name and a one-line
//!   summary.
//!
//!   ```text
//!   • Ran bash: ls -la
//!     └ total 42
//!       -rw-r--r-- 1 ...
//!   ```
//!
//! - **Output truncation**: when the tool is expanded and its output exceeds
//!   [`TOOL_OUTPUT_PREVIEW_LINES`] lines, we show the first few lines then
//!   a dim `… +N lines (ctrl+t to view transcript)` hint.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::diff::render_diff;
use crate::ui::app::{ToolCall, ToolStatus};
use crate::ui::theme;

/// Maximum number of output lines shown when a tool card is expanded.
const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;

/// Render a tool call card into the given Text.
pub fn render_tool_card_into(text: &mut Text<'static>, tc: &ToolCall) {
    let summary = summarize_tool_input(&tc.name, &tc.input);

    // Collapsed header line: status bullet + verb + tool name + summary.
    text.lines.push(render_header(tc, &summary));

    if tc.expanded {
        if let Some(diff_lines) = &tc.diff_lines {
            text.lines.push(Line::from(Span::styled(
                "  ── diff ──",
                Style::default().fg(theme::muted()),
            )));
            let diff_text = render_diff(diff_lines);
            for line in diff_text.lines {
                let mut indented_spans = vec![Span::raw("  ")];
                for span in line.spans {
                    indented_spans.push(Span::styled(span.content.into_owned(), span.style));
                }
                text.lines.push(Line::from(indented_spans));
            }
        } else if let Ok(pretty) = serde_json::to_string_pretty(&tc.input) {
            for line in pretty.lines() {
                text.lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(theme::dim()),
                )));
            }
        }
        // Output preview (head + ellipsis hint when truncated).
        if let Some(out) = &tc.output {
            let color = if out.is_error {
                theme::error()
            } else {
                ratatui::style::Color::Reset
            };
            let all_lines: Vec<&str> = out.text.lines().collect();
            for line in all_lines.iter().take(TOOL_OUTPUT_PREVIEW_LINES) {
                text.lines.push(Line::from(vec![
                    Span::styled("  └ ", Style::default().fg(theme::muted())),
                    Span::styled(line.to_string(), Style::default().fg(color)),
                ]));
            }
            if all_lines.len() > TOOL_OUTPUT_PREVIEW_LINES {
                text.lines.push(output_ellipsis_line(all_lines.len() - TOOL_OUTPUT_PREVIEW_LINES));
            }
        }
    }
}

fn render_header(tc: &ToolCall, summary: &str) -> Line<'static> {
    let verb = match tc.status {
        ToolStatus::Running => "Running",
        ToolStatus::Success => "Ran",
        ToolStatus::Failed => "Failed",
        ToolStatus::Denied => "Denied",
    };
    let icon = tool_icon(&tc.name);
    Line::from(vec![
        status_bullet(tc.status),
        Span::raw(" "),
        Span::styled(verb, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(format!("{icon} "), Style::default().fg(theme::accent())),
        Span::styled(
            tc.name.clone(),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ),
        Span::raw(": "),
        Span::styled(summary.to_string(), Style::default().fg(theme::muted())),
    ])
}

fn status_bullet(status: ToolStatus) -> Span<'static> {
    let style = match status {
        ToolStatus::Running => Style::default().fg(theme::warning()),
        ToolStatus::Success => Style::default().fg(theme::success()).add_modifier(Modifier::BOLD),
        ToolStatus::Failed => Style::default().fg(theme::error()).add_modifier(Modifier::BOLD),
        ToolStatus::Denied => Style::default().fg(theme::muted()),
    };
    Span::styled("•".to_string(), style)
}

fn output_ellipsis_line(omitted: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled("  └ ", Style::default().fg(theme::muted())),
        Span::styled(
            format!("… +{omitted} lines (ctrl+t to view transcript)"),
            Style::default().fg(theme::muted()),
        ),
    ])
}

pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "bash" | "shell" | "run_command" => "⚡",
        "read" | "file_read" | "read_file" => "📖",
        "write" | "file_write" | "edit" | "write_to_file" | "replace_in_file" => "✏️",
        "search" | "grep" | "find" | "list_dir" => "🔍",
        "task" | "agent" => "🤖",
        _ => "🔧",
    }
}

fn summarize_tool_input(name: &str, input: &serde_json::Value) -> String {
    let summary = match name {
        "bash" | "shell" | "run_command" => {
            input.get("command").and_then(|v| v.as_str()).unwrap_or("")
        }
        "read" | "write" | "edit" | "file_read" | "file_write" | "write_to_file"
        | "replace_in_file" => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "search" | "grep" | "find" => input
            .get("query")
            .or_else(|| input.get("pattern"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        _ => "",
    };
    if summary.is_empty() {
        truncate(&input.to_string(), 60)
    } else {
        truncate(summary, 60)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
