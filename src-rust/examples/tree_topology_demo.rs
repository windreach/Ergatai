//! Example demonstrating tree-based task topology
//!
//! NOTE: Tree topology is deprecated. Use DAG-based TaskGraph instead.
//! See examples/dag_topology_demo.rs for current API.

// Tree topology module is currently disabled in orchestration/mod.rs
// To re-enable: uncomment tree_topology and markdown_parser modules,
// then uncomment the imports below.

/*
use ergatai::orchestration::{TaskNode, TaskStatus, TaskTree};

fn main() {
    // Build a task tree
    let root = TaskNode::new("pm", "pm-agent", "Analyze requirements")
        .with_child(
            TaskNode::new("dev-1", "dev-agent", "Implement login")
                .with_child(TaskNode::new("test-1", "test-agent", "Test login")),
        )
        .with_child(
            TaskNode::new("dev-2", "dev-agent", "Implement registration")
                .with_child(TaskNode::new("test-2", "test-agent", "Test registration")),
        );

    let mut tree = TaskTree::new(root);
    tree.description = Some("Build user authentication system".to_string());

    // Initial state
    println!("{}", tree.to_ai_prompt());
    println!();

    // Simulate execution
    println!("--- Starting execution ---\n");

    // PM completes analysis
    tree.update_status("pm", TaskStatus::Completed).unwrap();
    println!("PM completed:\n{}", tree.to_ai_prompt());
    println!();

    // Dev agents start working
    tree.update_status("dev-1", TaskStatus::Running).unwrap();
    tree.update_status("dev-2", TaskStatus::Running).unwrap();
    println!("Dev agents running:\n{}", tree.to_ai_prompt());
    println!();

    // Dev-1 completes
    tree.set_result("dev-1", ".ergatai/results/dev-1.md".to_string()).unwrap();
    println!("Dev-1 completed:\n{}", tree.to_ai_prompt());
    println!();

    // Check what's ready now
    let ready = tree.ready_tasks();
    println!("Ready to execute next:");
    for node in ready {
        println!("  - [{}] {}", node.id, node.task);
    }
}
*/

fn main() {
    println!("Tree topology example is deprecated. Use DAG-based TaskGraph instead.");
    println!("See orchestration/dag_topology.rs and orchestration/dag_parser.rs for current API.");
}
