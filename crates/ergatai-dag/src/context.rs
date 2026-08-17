//! DAG execution context — tracks global variables and per-node outputs
//!
//! DagContext is the data store that powers the template engine.  When a DAG
//! starts, global variables are populated (e.g. `user_query`, `project_root`).
//! As nodes complete, their outputs are recorded.  Before a downstream node is
//! submitted, the scheduler calls `render_template()` on its input / task
//! description, and all `{{var}}` references are resolved from the context.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::template::render_template;
use ergatai_error::ErgataiResult;

/// DAG execution context
///
/// Stores global variables (set once before DAG starts) and per-node outputs
/// (recorded as each node completes).  Used to resolve `{{var}}` templates in
/// downstream node inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagContext {
    /// Global variables available as `{{global.key}}`
    pub global_vars: HashMap<String, String>,

    /// Per-node outputs: `node_id → { key → value }`
    /// Available as `{{node_id.key}}`
    #[serde(default)]
    node_outputs: HashMap<String, HashMap<String, String>>,
}

impl DagContext {
    /// Create a new context with the given global variables
    pub fn new(global_vars: HashMap<String, String>) -> Self {
        Self {
            global_vars,
            node_outputs: HashMap::new(),
        }
    }

    /// Create an empty context (no globals, no outputs)
    pub fn empty() -> Self {
        Self::new(HashMap::new())
    }

    // ── Global variables ──

    /// Set a global variable
    pub fn set_global(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.global_vars.insert(key.into(), value.into());
    }

    /// Get a global variable
    pub fn get_global(&self, key: &str) -> Option<&str> {
        self.global_vars.get(key).map(|s| s.as_str())
    }

    // ── Node outputs ──

    /// Record outputs from a completed node
    ///
    /// These become available as `{{node_id.key}}` in downstream templates.
    pub fn record_output(&mut self, node_id: impl Into<String>, outputs: HashMap<String, String>) {
        let id = node_id.into();
        debug!(node_id = id, keys = ?outputs.keys().collect::<Vec<_>>(), "Recording node outputs");
        self.node_outputs.insert(id, outputs);
    }

    /// Get the outputs of a specific node
    pub fn get_node_outputs(&self, node_id: &str) -> Option<&HashMap<String, String>> {
        self.node_outputs.get(node_id)
    }

    /// Check if outputs have been recorded for a node
    pub fn has_node_outputs(&self, node_id: &str) -> bool {
        self.node_outputs.contains_key(node_id)
    }

    // ── Template rendering ──

    /// Render a template string by resolving `{{var}}` references
    ///
    /// Builds a flat lookup map from both global vars and node outputs:
    /// - `global.user_query` → value from `self.global_vars["user_query"]`
    /// - `node_id.key`       → value from `self.node_outputs["node_id"]["key"]`
    ///
    /// Unknown references are preserved as-is (e.g. `{{missing}}` stays).
    pub fn render_template(&self, template: &str) -> String {
        let context = self.build_context_map();
        render_template(template, &context)
    }

    /// Build a flat `key → value` map from all sources
    fn build_context_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::with_capacity(
            self.global_vars.len() + self.node_outputs.values().map(|m| m.len()).sum::<usize>(),
        );

        // Global variables → `global.{key}`
        for (k, v) in &self.global_vars {
            map.insert(format!("global.{}", k), v.clone());
        }

        // Node outputs → `{node_id}.{key}`
        for (node_id, outputs) in &self.node_outputs {
            for (k, v) in outputs {
                map.insert(format!("{}.{}", node_id, k), v.clone());
            }
        }

        map
    }

    // ── Persistence ──

    /// Save context to a JSON file
    pub async fn save_to_file(&self, path: &Path) -> ErgataiResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(path, json).await?;
        Ok(())
    }

    /// Load context from a JSON file
    pub async fn load_from_file(path: &Path) -> ErgataiResult<Self> {
        let content = tokio::fs::read_to_string(path).await?;
        let ctx: Self = serde_json::from_str(&content)?;
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globals(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── Global variables ──

    #[test]
    fn test_set_and_get_global() {
        let mut ctx = DagContext::empty();
        ctx.set_global("user_query", "fix the bug");
        assert_eq!(ctx.get_global("user_query"), Some("fix the bug"));
        assert_eq!(ctx.get_global("missing"), None);
    }

    #[test]
    fn test_new_with_globals() {
        let ctx = DagContext::new(globals(&[("project", "ergatai"), ("branch", "main")]));
        assert_eq!(ctx.get_global("project"), Some("ergatai"));
        assert_eq!(ctx.get_global("branch"), Some("main"));
    }

    // ── Node outputs ──

    #[test]
    fn test_record_and_get_outputs() {
        let mut ctx = DagContext::empty();

        let mut outputs = HashMap::new();
        outputs.insert("result".to_string(), "LGTM".to_string());
        outputs.insert("issues".to_string(), "3".to_string());
        ctx.record_output("node-a", outputs);

        let got = ctx.get_node_outputs("node-a").unwrap();
        assert_eq!(got.get("result"), Some(&"LGTM".to_string()));
        assert_eq!(got.get("issues"), Some(&"3".to_string()));
        assert!(ctx.has_node_outputs("node-a"));
        assert!(!ctx.has_node_outputs("node-b"));
    }

    // ── Template rendering ──

    #[test]
    fn test_render_global() {
        let ctx = DagContext::new(globals(&[("user_query", "fix bug")]));
        assert_eq!(
            ctx.render_template("Task: {{global.user_query}}"),
            "Task: fix bug"
        );
    }

    #[test]
    fn test_render_node_output() {
        let mut ctx = DagContext::empty();
        let mut outputs = HashMap::new();
        outputs.insert("review".to_string(), "approved".to_string());
        ctx.record_output("TaskA", outputs);

        assert_eq!(
            ctx.render_template("Result: {{TaskA.review}}"),
            "Result: approved"
        );
    }

    #[test]
    fn test_render_mixed() {
        let mut ctx = DagContext::new(globals(&[("project", "ergatai")]));
        let mut outputs = HashMap::new();
        outputs.insert("summary".to_string(), "all tests pass".to_string());
        ctx.record_output("n1", outputs);

        assert_eq!(
            ctx.render_template(
                "Project {{global.project}}: {{n1.summary}}, status: {{unknown.key}}"
            ),
            "Project ergatai: all tests pass, status: {{unknown.key}}"
        );
    }

    #[test]
    fn test_render_no_references() {
        let ctx = DagContext::empty();
        assert_eq!(ctx.render_template("plain text"), "plain text");
    }

    // ── Serialization roundtrip ──

    #[test]
    fn test_serde_roundtrip() {
        let mut ctx = DagContext::new(globals(&[("q", "hello")]));
        let mut outputs = HashMap::new();
        outputs.insert("k".to_string(), "v".to_string());
        ctx.record_output("n1", outputs);

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: DagContext = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.get_global("q"), Some("hello"));
        assert_eq!(
            restored.get_node_outputs("n1").unwrap().get("k"),
            Some(&"v".to_string())
        );
    }

    // ── Additional tests ──

    #[test]
    fn test_overwrite_global() {
        let mut ctx = DagContext::empty();
        ctx.set_global("key", "value1");
        assert_eq!(ctx.get_global("key"), Some("value1"));
        ctx.set_global("key", "value2");
        assert_eq!(ctx.get_global("key"), Some("value2"));
    }

    #[test]
    fn test_empty_context() {
        let ctx = DagContext::empty();
        assert!(ctx.global_vars.is_empty());
        assert!(ctx.node_outputs.is_empty());
        assert_eq!(ctx.render_template("{{unknown}}"), "{{unknown}}");
    }

    #[test]
    fn test_record_output_overwrites() {
        let mut ctx = DagContext::empty();
        let mut outputs1 = HashMap::new();
        outputs1.insert("k".to_string(), "v1".to_string());
        ctx.record_output("node", outputs1);

        let mut outputs2 = HashMap::new();
        outputs2.insert("k".to_string(), "v2".to_string());
        ctx.record_output("node", outputs2);

        let got = ctx.get_node_outputs("node").unwrap();
        assert_eq!(got.get("k"), Some(&"v2".to_string()));
    }

    #[test]
    fn test_nested_key_patterns_in_template() {
        let mut ctx = DagContext::empty();
        ctx.set_global("project.name", "ergatai");
        let mut outputs = HashMap::new();
        outputs.insert("result.status".to_string(), "ok".to_string());
        ctx.record_output("node.1", outputs);

        // Global with dot in key name
        assert_eq!(ctx.get_global("project.name"), Some("ergatai"));
    }

    #[test]
    fn test_context_clone() {
        let mut ctx = DagContext::new(globals(&[("q", "original")]));
        let mut outputs = HashMap::new();
        outputs.insert("k".to_string(), "v".to_string());
        ctx.record_output("n1", outputs);

        let cloned = ctx.clone();
        assert_eq!(cloned.get_global("q"), Some("original"));
        assert_eq!(
            cloned.get_node_outputs("n1").unwrap().get("k"),
            Some(&"v".to_string())
        );
    }

    #[test]
    fn test_render_template_with_empty_global_vars() {
        let ctx = DagContext::empty();
        assert_eq!(
            ctx.render_template("{{global.missing}}"),
            "{{global.missing}}"
        );
    }

    #[test]
    fn test_has_node_outputs_false_when_missing() {
        let ctx = DagContext::empty();
        assert!(!ctx.has_node_outputs("nonexistent"));
    }

    #[test]
    fn test_render_template_multiple_node_outputs() {
        let mut ctx = DagContext::empty();
        let mut o1 = HashMap::new();
        o1.insert("result".to_string(), "A done".to_string());
        ctx.record_output("TaskA", o1);
        let mut o2 = HashMap::new();
        o2.insert("result".to_string(), "B done".to_string());
        ctx.record_output("TaskB", o2);

        let rendered = ctx.render_template("{{TaskA.result}} then {{TaskB.result}}");
        assert_eq!(rendered, "A done then B done");
    }

    #[test]
    fn test_build_context_map_keys() {
        let mut ctx = DagContext::new(globals(&[("q", "query")]));
        let mut outputs = HashMap::new();
        outputs.insert("k".to_string(), "v".to_string());
        ctx.record_output("n1", outputs);

        let map = ctx.build_context_map();
        assert_eq!(map.get("global.q"), Some(&"query".to_string()));
        assert_eq!(map.get("n1.k"), Some(&"v".to_string()));
    }

    #[test]
    fn test_record_output_empty_outputs() {
        let mut ctx = DagContext::empty();
        ctx.record_output("node", HashMap::new());
        assert!(ctx.has_node_outputs("node"));
        let got = ctx.get_node_outputs("node").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn test_multiple_globals_in_render() {
        let ctx = DagContext::new(globals(&[("a", "alpha"), ("b", "beta"), ("c", "gamma")]));
        let rendered = ctx.render_template("{{global.a}} {{global.b}} {{global.c}}");
        assert_eq!(rendered, "alpha beta gamma");
    }

    #[test]
    fn test_get_node_outputs_returns_none_when_missing() {
        let ctx = DagContext::empty();
        assert!(ctx.get_node_outputs("nonexistent").is_none());
    }

    #[test]
    fn test_render_template_preserves_literal_text() {
        let ctx = DagContext::new(globals(&[("x", "value")]));
        let rendered = ctx.render_template("before {{global.x}} after");
        assert_eq!(rendered, "before value after");
    }
}
