use log::{error, warn};
use serde_json::Value;
use std::collections::HashMap;

use crate::jsonexpr::JsonExpr;

/// Same check as the backend's experiment split: the gap from 100 is rounded
/// to two decimals before being compared with this tolerance.
const SPLIT_PERCENTAGES_SUM_TOLERANCE: f64 = 0.01;

/// The action of the first matching assignment rule.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleAction {
    /// An `assign` rule: a fixed variant.
    Assign(i32),
    /// A `split` rule: fractions (one per variant, summing to 1) to randomize
    /// the unit over.
    Split(Vec<f64>),
}

/// JavaScript `parseFloat`: leading whitespace is skipped and the longest
/// valid decimal prefix is parsed, so `"50abc"` is 50 and `"abc"` is NaN.
fn parse_float(s: &str) -> f64 {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    if s[i..].starts_with("Infinity") {
        return if bytes.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }

    let mut digits = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
    }
    if !digits {
        return f64::NAN;
    }

    let mut end = i;
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            end = j;
        }
    }

    s[..end].parse().unwrap_or(f64::NAN)
}

/// Parse a split rule's `"50/30/20"` percentages into fractions. `None` when
/// it is not a string, a part is not a non-negative number, or the parts do
/// not sum to 100 (within the backend's tolerance).
pub(crate) fn parse_split_percentages(percentages: &Value) -> Option<Vec<f64>> {
    let Value::String(percentages) = percentages else {
        return None;
    };

    let values: Vec<f64> = percentages.split('/').map(parse_float).collect();
    if values.iter().any(|v| v.is_nan() || *v < 0.0) {
        return None;
    }

    let gap = (100.0 - values.iter().sum::<f64>()).abs();
    if !gap.is_finite() {
        return None;
    }
    // JavaScript's toFixed(2) on the gap, as in the other SDKs.
    let rounded_gap: f64 = format!("{:.2}", gap).parse().ok()?;
    if rounded_gap > SPLIT_PERCENTAGES_SUM_TOLERANCE {
        return None;
    }

    Some(values.iter().map(|v| v / 100.0).collect())
}

/// An integral number (including `2.0`) within `i32` range, like JavaScript's
/// `Number.isInteger`.
fn as_variant(value: &Value) -> Option<i32> {
    let d = value.as_f64()?;
    if d.fract() != 0.0 || d < f64::from(i32::MIN) || d > f64::from(i32::MAX) {
        return None;
    }
    Some(d as i32)
}

fn parse_rule_action(rule: &serde_json::Map<String, Value>) -> Option<RuleAction> {
    match rule.get("type").and_then(Value::as_str) {
        Some("assign") => rule
            .get("variant")
            .and_then(as_variant)
            .map(RuleAction::Assign),
        Some("split") => rule
            .get("percentages")
            .and_then(parse_split_percentages)
            .map(RuleAction::Split),
        _ => None,
    }
}

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
                    } else {
                        warn!("Audience filter is not an array or object: {:?}", filter);
                    }
                } else {
                    warn!("No 'filter' field found in audience JSON");
                }
                None
            }
            Err(e) => {
                error!(
                    "Failed to parse audience JSON: {}. Input: '{}'",
                    e, audience_string
                );
                None
            }
        }
    }
}

impl AudienceMatcher {
    /// Parse an experiment's `assignmentRules` JSON string into its list of
    /// rules. `Ok(None)` when it is empty or has no `rules` list; `Err` when
    /// it is not valid JSON.
    pub fn parse_rules(&self, assignment_rules: &str) -> Result<Option<Vec<Value>>, String> {
        if assignment_rules.is_empty() {
            return Ok(None);
        }
        let parsed: Value = serde_json::from_str(assignment_rules).map_err(|e| e.to_string())?;
        match parsed.get("rules") {
            Some(Value::Array(rules)) => Ok(Some(rules.clone())),
            _ => Ok(None),
        }
    }

    /// The action of the first matching rule, or `None` when no rule matches.
    ///
    /// A rule is skipped when it is not an object, its type is unknown, an
    /// `assign` rule has a non-integer variant, a `split` rule has invalid
    /// percentages, its `environments` are present but not a list, its
    /// non-empty `environments` list does not contain `environment` (no
    /// environment never matches a non-empty list), its `conditions` are not
    /// an object, or its conditions do not evaluate to true. A rule without
    /// conditions always matches, and an empty `environments` list matches
    /// every environment. Variant bounds are the caller's responsibility.
    pub fn evaluate_rules(
        &self,
        rules: &[Value],
        environment: Option<&str>,
        vars: &HashMap<String, Value>,
    ) -> Option<RuleAction> {
        for rule in rules {
            let Value::Object(rule) = rule else {
                continue;
            };
            let Some(action) = parse_rule_action(rule) else {
                continue;
            };

            match rule.get("environments") {
                None | Some(Value::Null) => {}
                Some(Value::Array(envs)) => {
                    let matches = envs.is_empty()
                        || environment.is_some_and(|env| envs.iter().any(|e| e == env));
                    if !matches {
                        continue;
                    }
                }
                Some(_) => continue,
            }

            match rule.get("conditions") {
                None | Some(Value::Null) => return Some(action),
                Some(conditions @ Value::Object(_)) => {
                    if self.json_expr.evaluate_boolean_expr(conditions, vars) == Some(true) {
                        return Some(action);
                    }
                }
                Some(_) => {}
            }
        }
        None
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

        assert_eq!(
            matcher.evaluate(r#"{"filter":[{"value":5}]}"#, &vars),
            Some(true)
        );
        assert_eq!(
            matcher.evaluate(r#"{"filter":[{"value":true}]}"#, &vars),
            Some(true)
        );
        assert_eq!(
            matcher.evaluate(r#"{"filter":[{"value":1}]}"#, &vars),
            Some(true)
        );
        assert_eq!(
            matcher.evaluate(r#"{"filter":[{"value":null}]}"#, &vars),
            Some(false)
        );
        assert_eq!(
            matcher.evaluate(r#"{"filter":[{"value":0}]}"#, &vars),
            Some(false)
        );
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

        let audience = r#"{"filter": {"in": [{"value": ["US", "CA", "MX"]}, {"var": "country"}]}}"#;
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

    fn rules(json: Value) -> Vec<Value> {
        AudienceMatcher::new()
            .parse_rules(&json.to_string())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn test_parse_rules() {
        let matcher = AudienceMatcher::new();
        assert_eq!(matcher.parse_rules(""), Ok(None));
        assert_eq!(matcher.parse_rules("{}"), Ok(None));
        assert_eq!(matcher.parse_rules(r#"{"rules":{}}"#), Ok(None));
        assert!(matcher.parse_rules("not json").is_err());
        assert_eq!(
            matcher.parse_rules(r#"{"rules":[{"type":"assign"}]}"#),
            Ok(Some(vec![json!({"type":"assign"})]))
        );
    }

    #[test]
    fn test_evaluate_rules_first_match_wins() {
        let matcher = AudienceMatcher::new();
        let mut vars = HashMap::new();
        vars.insert("country".to_string(), json!("US"));
        let rules = rules(json!({"rules": [
            {"type": "assign", "variant": 2, "conditions": {"eq": [{"var": "country"}, {"value": "UK"}]}},
            {"type": "assign", "variant": 1, "conditions": {"eq": [{"var": "country"}, {"value": "US"}]}},
            {"type": "assign", "variant": 0}
        ]}));
        assert_eq!(
            matcher.evaluate_rules(&rules, None, &vars),
            Some(RuleAction::Assign(1))
        );
        vars.insert("country".to_string(), json!("DE"));
        assert_eq!(
            matcher.evaluate_rules(&rules, None, &vars),
            Some(RuleAction::Assign(0))
        );
    }

    #[test]
    fn test_evaluate_rules_skips_invalid_rules() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();
        let rules = rules(json!({"rules": [
            null,
            "assign",
            {"type": "unknown", "variant": 1},
            {"type": "assign"},
            {"type": "assign", "variant": 1.5},
            {"type": "assign", "variant": "1"},
            {"type": "split", "percentages": "50/40"},
            {"type": "split", "percentages": [50, 50]},
            {"type": "assign", "variant": 1, "environments": "production"},
            {"type": "assign", "variant": 1, "conditions": [{"value": true}]},
            {"type": "assign", "variant": 1, "conditions": {"value": false}},
            {"type": "assign", "variant": 3.0}
        ]}));
        assert_eq!(
            matcher.evaluate_rules(&rules, None, &vars),
            Some(RuleAction::Assign(3))
        );
    }

    #[test]
    fn test_evaluate_rules_environments() {
        let matcher = AudienceMatcher::new();
        let vars = HashMap::new();
        let scoped = rules(json!({"rules": [
            {"type": "assign", "variant": 1, "environments": ["production"]}
        ]}));
        assert_eq!(
            matcher.evaluate_rules(&scoped, Some("production"), &vars),
            Some(RuleAction::Assign(1))
        );
        assert_eq!(
            matcher.evaluate_rules(&scoped, Some("staging"), &vars),
            None
        );
        assert_eq!(matcher.evaluate_rules(&scoped, None, &vars), None);

        let any = rules(json!({"rules": [
            {"type": "assign", "variant": 1, "environments": []}
        ]}));
        assert_eq!(
            matcher.evaluate_rules(&any, None, &vars),
            Some(RuleAction::Assign(1))
        );
    }

    #[test]
    fn test_evaluate_rules_split() {
        let matcher = AudienceMatcher::new();
        let rules = rules(json!({"rules": [
            {"type": "split", "percentages": "25/75"}
        ]}));
        assert_eq!(
            matcher.evaluate_rules(&rules, None, &HashMap::new()),
            Some(RuleAction::Split(vec![0.25, 0.75]))
        );
    }

    #[test]
    fn test_parse_split_percentages() {
        let p = |s: &str| parse_split_percentages(&json!(s));
        assert_eq!(p("50/50"), Some(vec![0.5, 0.5]));
        assert_eq!(p("100"), Some(vec![1.0]));
        assert_eq!(p("0/100"), Some(vec![0.0, 1.0]));
        assert_eq!(
            p(" 33.33/33.33/33.34"),
            Some(vec![33.33 / 100.0, 33.33 / 100.0, 33.34 / 100.0])
        );
        assert_eq!(p("33.33/33.33/33.33"), Some(vec![0.3333, 0.3333, 0.3333]));
        assert_eq!(p("50abc/50"), Some(vec![0.5, 0.5]));
        assert_eq!(p("5e1/50"), Some(vec![0.5, 0.5]));
        assert_eq!(p("33.3/33.3/33.3"), None);
        assert_eq!(p("50/40"), None);
        assert_eq!(p("60/50"), None);
        assert_eq!(p("-50/150"), None);
        assert_eq!(p("abc/100"), None);
        assert_eq!(p("50//50"), None);
        assert_eq!(p("Infinity/50"), None);
        assert_eq!(p(""), None);
        assert_eq!(parse_split_percentages(&json!(100)), None);
    }

    #[test]
    fn test_parse_float_matches_javascript() {
        assert_eq!(parse_float("  12.5px"), 12.5);
        assert_eq!(parse_float("+3"), 3.0);
        assert_eq!(parse_float("-3"), -3.0);
        assert_eq!(parse_float(".5"), 0.5);
        assert_eq!(parse_float("5."), 5.0);
        assert_eq!(parse_float("1e"), 1.0);
        assert_eq!(parse_float("1e+2x"), 100.0);
        assert_eq!(parse_float("-Infinity"), f64::NEG_INFINITY);
        assert!(parse_float("").is_nan());
        assert!(parse_float(".").is_nan());
        assert!(parse_float("x1").is_nan());
    }
}
