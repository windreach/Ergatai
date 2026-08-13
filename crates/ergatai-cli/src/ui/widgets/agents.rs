//! Multi-agent status panel widget.
//!
//! Phase 4 groundwork: defines the `AgentStatus` type and a render function
//! for the agents panel. The panel is only visible when a DAG is running
//! (i.e. `agents.len() > 1`). For single-agent chat it is hidden.
//!
//! Population of `AgentStatus` values from DAG execution happens in a future
//! phase; this module provides the data model and the rendering plumbing.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

/// High-level state of an agent in the multi-agent panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AgentState {
    /// Agent is idle, waiting for a task.
    Idle,
    /// Agent is currently executing a task.
    Busy,
    /// Agent finished its task successfully.
    Done,
    /// Agent encountered an error.
    Error,
}

impl AgentState {
    /// Status icon shown in the panel.
    pub fn icon(self) -> &'static str {
        match self {
            AgentState::Idle => "○",
            AgentState::Busy => "●",
            AgentState::Done => "✓",
            AgentState::Error => "⚡",
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Busy => "busy",
            AgentState::Done => "done",
            AgentState::Error => "err",
        }
    }

    /// Colour used for the status icon/label.
    pub fn color(self) -> Color {
        match self {
            AgentState::Idle => Color::DarkGray,
            AgentState::Busy => Color::Yellow,
            AgentState::Done => Color::Green,
            AgentState::Error => Color::Red,
        }
    }
}

/// Status of a single agent in the multi-agent panel.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    /// Agent name (short identifier).
    pub name: String,
    /// Current state.
    pub status: AgentState,
    /// Optional one-line description of what the agent is doing.
    pub current_task: Option<String>,
}

/// Render the multi-agent panel into `area`.
///
/// The panel is a bordered list showing each agent's name, status icon, and
/// current task (if any). When `agents` is empty or has a single entry the
/// caller should skip rendering entirely (single-agent chat hides the panel).
pub fn render_agents_panel(frame: &mut Frame<'_>, area: Rect, agents: &[AgentStatus]) {
    if agents.is_empty() {
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Agents ");

    let items: Vec<ListItem> = agents
        .iter()
        .map(|a| {
            let task_suffix = match &a.current_task {
                Some(t) => format!(" — {}", truncate(t, 30)),
                None => String::new(),
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{} ", a.status.icon()),
                    Style::default().fg(a.status.color()),
                ),
                Span::styled(
                    format!("{:<10}", a.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    a.status.label().to_string(),
                    Style::default().fg(a.status.color()),
                ),
                Span::styled(task_suffix, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

/// Render a compact inline summary for the status bar (single line).
///
/// Returns a [`Line`] suitable for appending to the status bar. When there
/// are no agents, returns an empty line.
#[allow(dead_code)]
pub fn status_line_summary(agents: &[AgentStatus]) -> Line<'static> {
    if agents.is_empty() {
        return Line::default();
    }
    let spans: Vec<Span<'static>> = agents
        .iter()
        .flat_map(|a| {
            vec![
                Span::styled(
                    format!("{}{}", a.status.icon(), a.name),
                    Style::default().fg(a.status.color()),
                ),
                Span::raw(" "),
            ]
        })
        .collect();
    Line::from(spans)
}

/// Render a placeholder message when no agents are active (single-agent mode).
///
/// The panel is normally hidden in this case, but this helper is available
/// for debugging or forced-display scenarios.
#[allow(dead_code)]
pub fn render_placeholder(frame: &mut Frame<'_>, area: Rect) {
    let text = Paragraph::new("No active agents.")
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Agents "),
        )
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(text, area);
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_state_icons() {
        assert_eq!(AgentState::Idle.icon(), "○");
        assert_eq!(AgentState::Busy.icon(), "●");
        assert_eq!(AgentState::Done.icon(), "✓");
        assert_eq!(AgentState::Error.icon(), "⚡");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world this is long", 10), "hello worl…");
    }

    #[test]
    fn test_status_line_summary_empty() {
        let line = status_line_summary(&[]);
        assert!(line.spans.is_empty());
    }

    #[test]
    fn test_status_line_summary_with_agents() {
        let agents = vec![
            AgentStatus {
                name: "claude".to_string(),
                status: AgentState::Busy,
                current_task: None,
            },
            AgentStatus {
                name: "cursor".to_string(),
                status: AgentState::Done,
                current_task: None,
            },
        ];
        let line = status_line_summary(&agents);
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains("claude"));
        assert!(content.contains("cursor"));
    }
}
