//! JSON expression evaluator for audience filter matching.

pub mod evaluator;
pub mod operators;

use evaluator::Evaluator;
use serde_json::Value;
use std::collections::HashMap;

/// Entry point for evaluating JSON-based filter expressions.
#[derive(Debug)]
pub struct JsonExpr;

impl JsonExpr {
    /// Creates a new JSON expression evaluator.
    pub fn new() -> Self {
        Self
    }

    /// Evaluates a JSON expression as a boolean in the context of the given variables.
    pub fn evaluate_boolean_expr(
        &self,
        expr: &Value,
        vars: &HashMap<String, Value>,
    ) -> Option<bool> {
        let evaluator = Evaluator::new(vars.clone());
        let result = evaluator.evaluate(expr);
        Some(evaluator.boolean_convert(&result))
    }

    /// Evaluates a JSON expression and returns the resulting value.
    pub fn evaluate_expr(&self, expr: &Value, vars: &HashMap<String, Value>) -> Value {
        let evaluator = Evaluator::new(vars.clone());
        evaluator.evaluate(expr)
    }
}

impl Default for JsonExpr {
    fn default() -> Self {
        Self::new()
    }
}
