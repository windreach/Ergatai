//! Orchestration module for multi-agent collaboration
//!
//! Provides DAG-based task topology for AI-friendly workflow management.

pub mod dag_topology;
pub mod dag_parser;
pub mod template;
pub mod context;

// Legacy tree modules (deprecated, kept for backward compatibility)
// pub mod tree_topology;
// pub mod markdown_parser;

// Re-export DAG types as primary
pub use dag_topology::{TaskGraph, TaskNode, TaskStatus};
pub use dag_parser::parse_dag_markdown;
pub use template::{render_template, extract_references};
pub use context::DagContext;

// Legacy tree exports (deprecated) - commented out to avoid conflicts
// pub use tree_topology::{TaskTree, TaskNode as TreeNode, TaskStatus as TreeStatus};
// pub use markdown_parser::parse_markdown_tree;
