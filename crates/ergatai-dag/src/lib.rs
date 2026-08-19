//! Orchestration module for multi-agent collaboration
//!
//! Provides DAG-based task topology for AI-friendly workflow management.

pub mod condition;
pub mod context;
pub mod critical_path;
pub mod dag_parser;
pub mod dag_topology;
pub mod template;
pub mod yaml_parser;

// Legacy tree modules (deprecated, kept for backward compatibility)
pub mod markdown_parser;
pub mod tree_topology;

// Re-export DAG types as primary
pub use condition::Condition;
pub use context::DagContext;
pub use dag_parser::parse_dag_markdown;
pub use dag_topology::{TaskGraph, TaskNode, TaskStatus};
pub use template::{extract_references, render_template};
pub use yaml_parser::{is_yaml_format, parse_dag_auto, parse_dag_yaml};

// Legacy tree exports (deprecated) - commented out to avoid conflicts
// pub use tree_topology::{TaskTree, TaskNode as TreeNode, TaskStatus as TreeStatus};
// pub use markdown_parser::parse_markdown_tree;
