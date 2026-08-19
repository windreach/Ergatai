//! Condition expression evaluator for DAG conditional edges
//!
//! Supports simple comparison and logical expressions:
//! - Comparisons: `==`, `!=`, `<`, `>`, `<=`, `>=`
//! - Logical: `&&`, `||`
//! - Values: strings, numbers, booleans, null
//!
//! Template variables (e.g., `{{node.output}}`) are resolved before evaluation.

use crate::context::DagContext;

/// A condition expression that can be evaluated against a DagContext
#[derive(Debug, Clone)]
pub struct Condition {
    expr: String,
}

impl Condition {
    /// Create a new condition from an expression string
    pub fn new(expr: impl Into<String>) -> Self {
        Self { expr: expr.into() }
    }

    /// Evaluate the condition against the given context
    ///
    /// Returns true if the condition is satisfied, false otherwise.
    /// Template variables are resolved from the context before evaluation.
    pub fn evaluate(&self, context: &DagContext) -> bool {
        // First, render any template variables
        let rendered = context.render_template(&self.expr);

        // Then evaluate the expression
        Self::eval_expression(&rendered)
    }

    /// Evaluate a rendered expression (no template variables)
    fn eval_expression(expr: &str) -> bool {
        let expr = expr.trim();

        // Handle logical OR (lowest precedence)
        if let Some(pos) = Self::find_operator(expr, "||") {
            let left = Self::eval_expression(&expr[..pos]);
            let right = Self::eval_expression(&expr[pos + 2..]);
            return left || right;
        }

        // Handle logical AND
        if let Some(pos) = Self::find_operator(expr, "&&") {
            let left = Self::eval_expression(&expr[..pos]);
            let right = Self::eval_expression(&expr[pos + 2..]);
            return left && right;
        }

        // Handle comparison operators
        for op in &["==", "!=", "<=", ">=", "<", ">"] {
            if let Some(pos) = Self::find_operator(expr, op) {
                let left = expr[..pos].trim();
                let right = expr[pos + op.len()..].trim();
                return Self::eval_comparison(left, right, op);
            }
        }

        // If no operator found, treat as boolean value
        Self::parse_bool(expr)
    }

    /// Find the position of an operator, respecting parentheses
    fn find_operator(expr: &str, op: &str) -> Option<usize> {
        let mut depth = 0;

        for (i, ch) in expr.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0 => {
                    // Check if this position starts the operator
                    if expr[i..].starts_with(op) {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Evaluate a comparison expression
    fn eval_comparison(left: &str, right: &str, op: &str) -> bool {
        let left_val = Self::parse_value(left);
        let right_val = Self::parse_value(right);

        match op {
            "==" => left_val == right_val,
            "!=" => left_val != right_val,
            "<" => Self::compare_values(&left_val, &right_val) == Some(std::cmp::Ordering::Less),
            ">" => {
                Self::compare_values(&left_val, &right_val) == Some(std::cmp::Ordering::Greater)
            }
            "<=" => matches!(
                Self::compare_values(&left_val, &right_val),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ),
            ">=" => matches!(
                Self::compare_values(&left_val, &right_val),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            ),
            _ => false,
        }
    }

    /// Parse a value string into a comparable Value
    fn parse_value(s: &str) -> Value {
        let s = s.trim();

        // Remove quotes if present
        if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))
        {
            return Value::String(s[1..s.len() - 1].to_string());
        }

        // Try parsing as number
        if let Ok(n) = s.parse::<i64>() {
            return Value::Number(n);
        }
        if let Ok(n) = s.parse::<f64>() {
            return Value::Float(n);
        }

        // Try parsing as boolean
        match s.to_lowercase().as_str() {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            "null" | "none" => return Value::Null,
            _ => {}
        }

        // Default to string
        Value::String(s.to_string())
    }

    /// Parse a boolean value from string
    fn parse_bool(s: &str) -> bool {
        match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" | "" => false,
            _ => false,
        }
    }

    /// Compare two values, returning None if they're not comparable
    fn compare_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
        match (left, right) {
            (Value::Number(a), Value::Number(b)) => Some(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Number(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
            (Value::Float(a), Value::Number(b)) => a.partial_cmp(&(*b as f64)),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

/// Comparable value type for condition evaluation
#[derive(Debug, Clone, PartialEq)]
enum Value {
    String(String),
    Number(i64),
    Float(f64),
    Bool(bool),
    Null,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_equality() {
        let ctx = DagContext::empty();
        assert!(Condition::new("1 == 1").evaluate(&ctx));
        assert!(!Condition::new("1 == 2").evaluate(&ctx));
    }

    #[test]
    fn test_string_equality() {
        let ctx = DagContext::empty();
        assert!(Condition::new("\"hello\" == \"hello\"").evaluate(&ctx));
        assert!(!Condition::new("\"hello\" == \"world\"").evaluate(&ctx));
    }

    #[test]
    fn test_comparison_operators() {
        let ctx = DagContext::empty();
        assert!(Condition::new("1 < 2").evaluate(&ctx));
        assert!(Condition::new("2 > 1").evaluate(&ctx));
        assert!(Condition::new("1 <= 1").evaluate(&ctx));
        assert!(Condition::new("2 >= 1").evaluate(&ctx));
    }

    #[test]
    fn test_logical_and() {
        let ctx = DagContext::empty();
        assert!(Condition::new("1 == 1 && 2 == 2").evaluate(&ctx));
        assert!(!Condition::new("1 == 1 && 2 == 3").evaluate(&ctx));
    }

    #[test]
    fn test_logical_or() {
        let ctx = DagContext::empty();
        assert!(Condition::new("1 == 1 || 2 == 3").evaluate(&ctx));
        assert!(!Condition::new("1 == 2 || 3 == 4").evaluate(&ctx));
    }

    #[test]
    fn test_template_variables() {
        let mut ctx = DagContext::empty();
        ctx.set_global("status", "success");
        assert!(Condition::new("{{global.status}} == \"success\"").evaluate(&ctx));
        assert!(!Condition::new("{{global.status}} == \"failure\"").evaluate(&ctx));
    }
}
