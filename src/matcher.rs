use serde_json::Value;
use std::collections::HashMap;

use crate::jsonexpr::JsonExpr;

pub struct AudienceMatcher {
    json_expr: JsonExpr,
}

impl AudienceMatcher {
    pub fn new() -> Self {
        Self {
            json_expr: JsonExpr::new(),
        }
    }

    pub fn evaluate(&self, audience_string: &str, vars: &HashMap<String, Value>) -> Option<bool> {
        match serde_json::from_str::<Value>(audience_string) {
            Ok(audience) => {
                if let Some(filter) = audience.get("filter") {
                    if filter.is_array() || filter.is_object() {
                        return self.json_expr.evaluate_boolean_expr(filter, vars);
                    }
                }
                None
            }
            Err(_) => None,
        }
    }
}

impl Default for AudienceMatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_empty_audience_returns_none() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();

        assert_eq!(matcher.evaluate("", &vars), None);
        assert_eq!(matcher.evaluate("{}", &vars), None);
        assert_eq!(matcher.evaluate("null", &vars), None);
    }

    #[test]
    fn test_filter_not_object_or_array_returns_none() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();

        assert_eq!(matcher.evaluate(r#"{"filter":null}"#, &vars), None);
        assert_eq!(matcher.evaluate(r#"{"filter":false}"#, &vars), None);
        assert_eq!(matcher.evaluate(r#"{"filter":5}"#, &vars), None);
        assert_eq!(matcher.evaluate(r#"{"filter":"a"}"#, &vars), None);
    }

    #[test]
    fn test_filter_with_value_returns_boolean() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();

        assert_eq!(matcher.evaluate(r#"{"filter":[{"value":5}]}"#, &vars), Some(true));
        assert_eq!(matcher.evaluate(r#"{"filter":[{"value":true}]}"#, &vars), Some(true));
        assert_eq!(matcher.evaluate(r#"{"filter":[{"value":1}]}"#, &vars), Some(true));
        assert_eq!(matcher.evaluate(r#"{"filter":[{"value":null}]}"#, &vars), Some(false));
        assert_eq!(matcher.evaluate(r#"{"filter":[{"value":0}]}"#, &vars), Some(false));
    }

    #[test]
    fn test_filter_with_var_and_not() {
        let matcher = AudienceMatcher::new();

        let mut vars_true = HashMap::new();
        vars_true.insert("returning".to_string(), json!(true));

        let mut vars_false = HashMap::new();
        vars_false.insert("returning".to_string(), json!(false));

        let audience = r#"{"filter":[{"not":{"var":"returning"}}]}"#;
        assert_eq!(matcher.evaluate(audience, &vars_true), Some(false));
        assert_eq!(matcher.evaluate(audience, &vars_false), Some(true));
    }

    #[test]
    fn test_audience_match_eq() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("US"));

        let audience = r#"{"filter": {"eq": [{"var": "country"}, {"value": "US"}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));
    }

    #[test]
    fn test_audience_mismatch_eq() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("UK"));

        let audience = r#"{"filter": {"eq": [{"var": "country"}, {"value": "US"}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }

    #[test]
    fn test_audience_with_and_combinator() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("US"));
        vars.insert("age".to_string(), json!(25));

        let audience = r#"{"filter": {"and": [{"eq": [{"var": "country"}, {"value": "US"}]}, {"gte": [{"var": "age"}, {"value": 18}]}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));

        vars.insert("age".to_string(), json!(16));
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }

    #[test]
    fn test_audience_with_or_combinator() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("CA"));

        let audience = r#"{"filter": {"or": [{"eq": [{"var": "country"}, {"value": "US"}]}, {"eq": [{"var": "country"}, {"value": "CA"}]}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));

        vars.insert("country".to_string(), json!("UK"));
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }

    #[test]
    fn test_audience_with_in_operator() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("US"));

        let audience = r#"{"filter": {"in": [{"var": "country"}, {"value": ["US", "CA", "MX"]}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));

        vars.insert("country".to_string(), json!("UK"));
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }

    #[test]
    fn test_audience_with_match_operator() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("email".to_string(), json!("user@example.com"));

        let audience = r#"{"filter": {"match": [{"var": "email"}, {"value": "@example\\.com$"}]}}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));

        vars.insert("email".to_string(), json!("user@other.com"));
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }

    #[test]
    fn test_invalid_json() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();

        assert_eq!(matcher.evaluate("not valid json", &vars), None);
    }

    #[test]
    fn test_filter_as_array() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();

        let audience = r#"{"filter": [{"value": true}, {"value": true}]}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(true));

        let audience = r#"{"filter": [{"value": true}, {"value": false}]}"#;
        assert_eq!(matcher.evaluate(audience, &vars), Some(false));
    }
}
