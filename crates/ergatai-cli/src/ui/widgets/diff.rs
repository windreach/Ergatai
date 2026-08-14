//! Inline diff widget using `imara-diff`.
//!
//! Computes line-level diffs between two strings and renders them in a
//! codex-style layout: per-line line-number gutter, `+`/`-`/` ` sign column,
//! background-tinted rows, and `⋮` separators between hunks.

use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::ui::theme;

/// A single line in a diff output.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub content: String,
    /// 1-based line number in the source (old for Removed, new for Added).
    /// `None` for hunk separators.
    pub line_number: Option<usize>,
}

/// The kind of a diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// Unchanged context line (reserved for future context expansion).
    #[allow(dead_code)]
    Context,
    /// Added line (green).
    Added,
    /// Removed line (red).
    Removed,
    /// Separator between hunks (rendered as `⋮`). Replaces the old
    /// `@@ -a,b +c,d @@` header — codex-style.
    HunkSeparator,
}

/// Compute a line-level diff between `old` and `new`.
pub fn compute_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let input = InternedInput::new(old, new);
    let mut lines: Vec<DiffLine> = Vec::new();
    let mut first_hunk = true;

    diff(
        Algorithm::Histogram,
        &input,
        |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
            if !first_hunk {
                lines.push(DiffLine {
                    kind: DiffKind::HunkSeparator,
                    content: String::new(),
                    line_number: None,
                });
            }
            first_hunk = false;

            for (offset, &tok) in input.before[before.start as usize..before.end as usize]
                .iter()
                .enumerate()
            {
                lines.push(DiffLine {
                    kind: DiffKind::Removed,
                    content: input.interner[tok].to_string(),
                    line_number: Some(before.start as usize + offset + 1),
                });
            }

            for (offset, &tok) in input.after[after.start as usize..after.end as usize]
                .iter()
                .enumerate()
            {
                lines.push(DiffLine {
                    kind: DiffKind::Added,
                    content: input.interner[tok].to_string(),
                    line_number: Some(after.start as usize + offset + 1),
                });
            }
        },
    );

    lines
}

/// Render a slice of [`DiffLine`]s into ratatui [`Text`] with codex-style
/// line numbers, gutter signs, and background colors.
pub fn render_diff(lines: &[DiffLine]) -> Text<'static> {
    let mut text = Text::default();

    let max_ln = lines.iter().filter_map(|l| l.line_number).max().unwrap_or(0);
    let gutter_width = max_ln.to_string().len().max(1);

    for line in lines {
        match line.kind {
            DiffKind::HunkSeparator => {
                let spacer = format!("{:width$} ", "", width = gutter_width);
                text.lines.push(Line::from(vec![
                    Span::styled(spacer, Style::default().add_modifier(Modifier::DIM)),
                    Span::styled("⋮", Style::default().add_modifier(Modifier::DIM)),
                ]));
            }
            DiffKind::Context => {
                let ln_str = line
                    .line_number
                    .map(|n| format!("{n:>gutter_width$}"))
                    .unwrap_or_else(|| " ".repeat(gutter_width));
                text.lines.push(Line::from(vec![
                    Span::styled(format!("{ln_str} "), Style::default().add_modifier(Modifier::DIM)),
                    Span::styled(" ", Style::default()),
                    Span::raw(line.content.clone()),
                ]));
            }
            DiffKind::Added => {
                let ln_str = line
                    .line_number
                    .map(|n| format!("{n:>gutter_width$}"))
                    .unwrap_or_else(|| " ".repeat(gutter_width));
                text.lines.push(Line::from(vec![
                    Span::styled(
                        format!("{ln_str} "),
                        Style::default()
                            .fg(theme::diff_add_fg())
                            .bg(theme::diff_add_bg())
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        "+",
                        Style::default().fg(theme::diff_add_fg()).bg(theme::diff_add_bg()),
                    ),
                    Span::styled(
                        format!(" {}", line.content),
                        Style::default().bg(theme::diff_add_bg()),
                    ),
                ]));
            }
            DiffKind::Removed => {
                let ln_str = line
                    .line_number
                    .map(|n| format!("{n:>gutter_width$}"))
                    .unwrap_or_else(|| " ".repeat(gutter_width));
                text.lines.push(Line::from(vec![
                    Span::styled(
                        format!("{ln_str} "),
                        Style::default()
                            .fg(theme::diff_del_fg())
                            .bg(theme::diff_del_bg())
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        "-",
                        Style::default().fg(theme::diff_del_fg()).bg(theme::diff_del_bg()),
                    ),
                    Span::styled(
                        format!(" {}", line.content),
                        Style::default().bg(theme::diff_del_bg()),
                    ),
                ]));
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_empty_diff() {
        let lines = compute_diff("hello\nworld\n", "hello\nworld\n");
        assert!(lines
            .iter()
            .all(|l| l.kind != DiffKind::Added && l.kind != DiffKind::Removed));
    }

    #[test]
    fn added_line_detected() {
        let old = "a\nb\n";
        let new = "a\nb\nc\n";
        let lines = compute_diff(old, new);
        assert!(lines.iter().any(|l| l.kind == DiffKind::Added));
    }

    #[test]
    fn removed_line_detected() {
        let old = "a\nb\nc\n";
        let new = "a\nb\n";
        let lines = compute_diff(old, new);
        assert!(lines.iter().any(|l| l.kind == DiffKind::Removed));
    }

    #[test]
    fn render_diff_produces_text() {
        let lines = compute_diff("foo\n", "bar\n");
        let text = render_diff(&lines);
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn line_numbers_assigned() {
        let old = "a\nb\n";
        let new = "a\nx\nb\n";
        let lines = compute_diff(old, new);
        for l in &lines {
            if matches!(l.kind, DiffKind::Added | DiffKind::Removed) {
                assert!(l.line_number.is_some());
            }
        }
    }
}
