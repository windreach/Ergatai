//! Markdown outline parser for task trees
//!
//! Parses Markdown-style outlines into TaskTree structures.
//! AI generates Markdown (which it's good at), system converts to tree.
//!
//! Format:
//! # Agent - Task description
//! ## Agent - Sub-task
//! ### Agent - Sub-sub-task

use ergatai_error::{ErgataiError, ErgataiResult};
use crate::{TaskNode, TaskStatus, TaskTree};

/// Parse a Markdown outline into a TaskTree
///
/// # Example
///
/// ```markdown
/// # PM - Analyze requirements
/// ## Dev-1 - Implement API
/// ### Test-1 - Write API tests
/// ## Dev-2 - Implement UI
/// # QA - Integration testing
/// ```
pub fn parse_markdown_tree(content: &str) -> ErgataiResult<TaskTree> {
    let lines: Vec<&str> = content.lines().collect();
    let mut iter = lines.into_iter().enumerate().peekable();

    // Pre-allocate with a reasonable default for root nodes
    let mut root_nodes = Vec::with_capacity(4);

    while let Some((_, line)) = iter.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('#') {
            continue;
        }

        let (level, task_text) = parse_heading(trimmed)?;
        let node = parse_node_recursive(&mut iter, level, &task_text)?;
        root_nodes.push(node);
    }

    if root_nodes.is_empty() {
        return Err(ErgataiError::InvalidArgument("No tasks found in markdown outline".to_string()));
    }

    // If only one root node, use it directly
    // Otherwise, create a virtual root
    let root = if root_nodes.len() == 1 {
        root_nodes.pop().expect("len == 1 guarantees pop() returns Some")
    } else {
        TaskNode {
            id: "root".to_string(),
            agent: "system".to_string(),
            task: "Execute task tree".to_string(),
            status: TaskStatus::Completed, // Virtual root is already done
            children: root_nodes,
            result_path: None,
            sibling_links: Vec::new(),
            metadata: std::collections::HashMap::new(),
        }
    };

    Ok(TaskTree::new(root))
}

/// Parse a heading line: "# Agent - Task" -> (level, "Agent - Task")
fn parse_heading(line: &str) -> ErgataiResult<(usize, String)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if level == 0 {
        return Err(ErgataiError::InvalidArgument(format!("Invalid heading: {}", line)));
    }

    let task_text = line
        .chars()
        .skip(level)
        .skip_while(|c| c.is_whitespace())
        .collect::<String>()
        .trim()
        .to_string();

    if task_text.is_empty() {
        return Err(ErgataiError::InvalidArgument(format!("Empty heading: {}", line)));
    }

    Ok((level, task_text))
}

/// Recursively parse a node and its children
fn parse_node_recursive<'a, I>(
    iter: &mut std::iter::Peekable<I>,
    parent_level: usize,
    task_text: &str,
) -> ErgataiResult<TaskNode>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    // Parse "Agent - Task" format
    let (agent, task_desc) = parse_task_format(task_text)?;

    let mut node = TaskNode::new(generate_id(&agent, &task_desc), agent, task_desc);

    // Look for children (headings with level > parent_level)
    while let Some(&(_, next_line)) = iter.peek() {
        let trimmed = next_line.trim();

        if trimmed.is_empty() || !trimmed.starts_with('#') {
            iter.next(); // Skip non-heading lines
            continue;
        }

        let (level, _) = parse_heading(trimmed)?;

        if level > parent_level {
            // This is a child node
            iter.next(); // Consume this line
            let (_, child_text) = parse_heading(trimmed)?;
            let child = parse_node_recursive(iter, level, &child_text)?;
            node.children.push(child);
        } else {
            // Back to parent level or higher, stop
            break;
        }
    }

    Ok(node)
}

/// Parse "Agent - Task" or "Agent: Task" format
fn parse_task_format(text: &str) -> ErgataiResult<(String, String)> {
    // Try "Agent - Task" format
    if let Some((agent, task)) = text.split_once(" - ") {
        return Ok((agent.trim().to_string(), task.trim().to_string()));
    }

    // Try "Agent: Task" format
    if let Some((agent, task)) = text.split_once(": ") {
        return Ok((agent.trim().to_string(), task.trim().to_string()));
    }

    // No separator found, use default agent
    Ok(("agent".to_string(), text.to_string()))
}

/// Generate a simple ID from agent and task
/// Uses full task description to avoid collisions
fn generate_id(agent: &str, task: &str) -> String {
    let agent_part = agent.to_lowercase().replace(' ', "-");
    let task_part = task
        .to_lowercase()
        .split_whitespace()
        .take(5) // Use up to 5 words for better uniqueness
        .collect::<Vec<_>>()
        .join("-");

    if task_part.is_empty() {
        agent_part
    } else {
        format!("{}-{}", agent_part, task_part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tree() {
        let markdown = r#"
# PM - Analyze requirements
## Dev - Implement feature
### Test - Write tests
"#;

        let tree = parse_markdown_tree(markdown).unwrap();
        assert_eq!(tree.root.agent, "PM"); // Single root, use it directly
        assert_eq!(tree.root.children.len(), 1);
        assert_eq!(tree.root.children[0].agent, "Dev");
        assert_eq!(tree.root.children[0].children.len(), 1);
    }

    #[test]
    fn test_multiple_roots() {
        let markdown = r#"
# PM - Analyze
# QA - Test
"#;

        let tree = parse_markdown_tree(markdown).unwrap();
        assert_eq!(tree.root.agent, "system"); // Virtual root
        assert_eq!(tree.root.children.len(), 2);
    }

    #[test]
    fn test_parallel_tasks() {
        let markdown = r#"
# PM - Plan
## Dev-1 - Task A
## Dev-2 - Task B
## Dev-3 - Task C
"#;

        let tree = parse_markdown_tree(markdown).unwrap();
        // PM is the root (single # heading)
        assert_eq!(tree.root.agent, "PM");
        assert_eq!(tree.root.children.len(), 3); // 3 parallel tasks
    }

    #[test]
    fn test_task_format() {
        let (agent, task) = parse_task_format("Dev - Implement API").unwrap();
        assert_eq!(agent, "Dev");
        assert_eq!(task, "Implement API");

        let (agent, task) = parse_task_format("Test: Write tests").unwrap();
        assert_eq!(agent, "Test");
        assert_eq!(task, "Write tests");
    }

    #[test]
    fn test_id_generation() {
        let id = generate_id("Dev-1", "Implement login");
        assert!(id.contains("dev-1"));
        assert!(id.contains("implement-login"));
    }
}
