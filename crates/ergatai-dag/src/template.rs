//! Template engine for DAG data flow
//!
//! Supports `{{var}}` style variable references:
//! - `{{global.key}}` - global variables (user query, project info, etc.)
//! - `{{node_id.key}}` - outputs from completed upstream nodes
//!
//! Unresolved references are preserved as-is so the agent sees what's missing.

use std::collections::HashMap;

/// Render a template string by resolving all `{{key}}` references
///
/// Keys are looked up in `context`:
/// - `global.user_query` → `context["global.user_query"]`
/// - `TaskA.result`      → `context["TaskA.result"]`
///
/// Unknown variables are kept in the output (e.g. `{{unknown}}` stays).
pub fn render_template(template: &str, context: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        // Copy text before the opening `{{`
        result.push_str(&remaining[..start]);

        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let var_name = after_open[..end].trim();

            if !var_name.is_empty() {
                match context.get(var_name) {
                    Some(value) => result.push_str(value),
                    None => {
                        // Keep the unresolved reference so the agent can see it's missing
                        result.push_str("{{");
                        result.push_str(var_name);
                        result.push_str("}}");
                    }
                }
            } else {
                // Empty braces `{{}}` — preserve literally
                result.push_str("{{}}");
            }

            remaining = &after_open[end + 2..];
        } else {
            // No closing `}}` — treat the rest as literal text
            result.push_str(&remaining[start..]);
            remaining = "";
        }
    }

    // Append any trailing text after the last `}}`
    result.push_str(remaining);
    result
}

/// Extract all variable references from a template string.
///
/// Returns a deduplicated Vec of variable names found in `{{...}}` blocks.
pub fn extract_references(template: &str) -> Vec<String> {
    // Pre-allocate with a reasonable default
    let mut refs = Vec::with_capacity(4);
    let mut seen = std::collections::HashSet::new();
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let var_name = after_open[..end].trim().to_string();
            if !var_name.is_empty() && seen.insert(var_name.clone()) {
                refs.push(var_name);
            }
            remaining = &after_open[end + 2..];
        } else {
            break;
        }
    }

    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── render_template ──

    #[test]
    fn test_no_template_passthrough() {
        let context = ctx(&[]);
        assert_eq!(render_template("Hello, world!", &context), "Hello, world!");
    }

    #[test]
    fn test_global_variable() {
        let context = ctx(&[("global.user_query", "fix the bug")]);
        assert_eq!(
            render_template("Query: {{global.user_query}}", &context),
            "Query: fix the bug"
        );
    }

    #[test]
    fn test_node_output_reference() {
        let context = ctx(&[("TaskA.review_result", "LGTM, 3 issues found")]);
        assert_eq!(
            render_template("Upstream said: {{TaskA.review_result}}", &context),
            "Upstream said: LGTM, 3 issues found"
        );
    }

    #[test]
    fn test_multiple_references() {
        let context = ctx(&[
            ("global.project", "ergatai"),
            ("n1.output", "result1"),
            ("n2.output", "result2"),
        ]);
        assert_eq!(
            render_template(
                "Project {{global.project}}: {{n1.output}} + {{n2.output}}",
                &context
            ),
            "Project ergatai: result1 + result2"
        );
    }

    #[test]
    fn test_unresolved_preserved() {
        let context = ctx(&[("global.known", "value")]);
        assert_eq!(
            render_template("{{global.known}} and {{unknown}}", &context),
            "value and {{unknown}}"
        );
    }

    #[test]
    fn test_empty_braces() {
        let context = ctx(&[]);
        assert_eq!(
            render_template("text {{}} more", &context),
            "text {{}} more"
        );
    }

    #[test]
    fn test_unclosed_braces() {
        let context = ctx(&[]);
        // Unclosed `{{` is treated as literal
        assert_eq!(
            render_template("text {{unclosed", &context),
            "text {{unclosed"
        );
    }

    #[test]
    fn test_adjacent_references() {
        let context = ctx(&[("a", "X"), ("b", "Y")]);
        assert_eq!(render_template("{{a}}{{b}}", &context), "XY");
    }

    #[test]
    fn test_whitespace_in_reference() {
        // `{{ var }}` should be trimmed to `var`
        let context = ctx(&[("var", "resolved")]);
        assert_eq!(render_template("{{ var }}", &context), "resolved");
    }

    #[test]
    fn test_empty_template() {
        let context = ctx(&[("a", "b")]);
        assert_eq!(render_template("", &context), "");
    }

    // ── extract_references ──

    #[test]
    fn test_extract_basic() {
        let refs = extract_references("{{a.b}} and {{c.d}}");
        assert_eq!(refs, vec!["a.b", "c.d"]);
    }

    #[test]
    fn test_extract_dedup() {
        let refs = extract_references("{{x}} then {{y}} then {{x}}");
        assert_eq!(refs, vec!["x", "y"]); // second x is deduped
    }

    #[test]
    fn test_extract_none() {
        let refs = extract_references("no references here");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_skips_empty() {
        let refs = extract_references("{{}} and {{valid}}");
        assert_eq!(refs, vec!["valid"]);
    }

    // ── Additional edge case tests ──

    #[test]
    fn test_nested_braces_inner_preserved() {
        // `{{outer {{inner}} }}` — the first `}}` closes the first `{{`
        // so `{{outer {{inner}}` is resolved, then ` }}` remains
        let context = ctx(&[]);
        let result = render_template("{{outer {{inner}} }}", &context);
        // First match: `{{outer {{inner}}` -> var_name = "outer {{inner"
        // which won't be found, so preserved; then " }}" remains
        assert!(result.contains("outer {{inner") || result.contains("{{outer {{inner}} }}"));
    }

    #[test]
    fn test_unicode_content() {
        let context = ctx(&[("greeting", "こんにちは世界")]);
        assert_eq!(
            render_template("Hello: {{greeting}}", &context),
            "Hello: こんにちは世界"
        );
    }

    #[test]
    fn test_unicode_in_template() {
        let context = ctx(&[("名前", "太郎")]);
        assert_eq!(render_template("名前: {{名前}}", &context), "名前: 太郎");
    }

    #[test]
    fn test_very_long_variable_name() {
        let long_name = "a".repeat(1000);
        let mut context = HashMap::new();
        context.insert(long_name.clone(), "value".to_string());
        let template = format!("{{{{{}}}}}", long_name);
        assert_eq!(render_template(&template, &context), "value");
    }

    #[test]
    fn test_variable_with_dots_and_special_chars() {
        let context = ctx(&[
            ("global.user.name", "Alice"),
            ("node-1.result/path", "/tmp/out"),
        ]);
        assert_eq!(
            render_template(
                "User: {{global.user.name}}, Path: {{node-1.result/path}}",
                &context
            ),
            "User: Alice, Path: /tmp/out"
        );
    }

    #[test]
    fn test_multiple_adjacent_references_resolved() {
        let context = ctx(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(render_template("{{a}}{{b}}{{c}}", &context), "123");
    }

    #[test]
    fn test_extract_references_with_dots() {
        let refs = extract_references("{{global.x}} and {{node.y}}");
        assert_eq!(refs, vec!["global.x", "node.y"]);
    }

    #[test]
    fn test_render_template_only_braces() {
        let context = ctx(&[]);
        assert_eq!(render_template("{{}}", &context), "{{}}");
    }

    #[test]
    fn test_extract_references_empty_template() {
        let refs = extract_references("");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_references_only_unclosed() {
        let refs = extract_references("{{unclosed");
        assert!(refs.is_empty());
    }
}
