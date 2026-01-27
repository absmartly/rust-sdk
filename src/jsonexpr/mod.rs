pub mod evaluator;
pub mod operators;

use evaluator::Evaluator;
use serde_json::Value;
use std::collections::HashMap;

pub struct JsonExpr;

impl JsonExpr {
    pub fn new() -> Self {
        Self
    }

    pub fn evaluate_boolean_expr(&self, expr: &Value, vars: &HashMap<String, Value>) -> Option<bool> {
        let evaluator = Evaluator::new(vars.clone());
        let result = evaluator.evaluate(expr);
        Some(evaluator.boolean_convert(&result))
    }

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
