use serde_json::Value;
use std::collections::HashMap;

use super::operators;

pub struct Evaluator {
    vars: HashMap<String, Value>,
}

impl Evaluator {
    pub fn new(vars: HashMap<String, Value>) -> Self {
        Self { vars }
    }

    pub fn evaluate(&self, expr: &Value) -> Value {
        match expr {
            Value::Array(arr) => operators::and_op(self, &Value::Array(arr.clone())),
            Value::Object(map) => {
                for (key, value) in map.iter() {
                    match key.as_str() {
                        "and" => return operators::and_op(self, value),
                        "or" => return operators::or_op(self, value),
                        "value" => return operators::value_op(self, value),
                        "var" => return operators::var_op(self, value),
                        "null" => return operators::null_op(self, value),
                        "not" => return operators::not_op(self, value),
                        "in" => return operators::in_op(self, value),
                        "match" => return operators::match_op(self, value),
                        "eq" => return operators::eq_op(self, value),
                        "gt" => return operators::gt_op(self, value),
                        "gte" => return operators::gte_op(self, value),
                        "lt" => return operators::lt_op(self, value),
                        "lte" => return operators::lte_op(self, value),
                        _ => {}
                    }
                    break;
                }
                Value::Null
            }
            _ => Value::Null,
        }
    }

    pub fn boolean_convert(&self, x: &Value) -> bool {
        match x {
            Value::Bool(b) => *b,
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    i != 0
                } else if let Some(f) = n.as_f64() {
                    f != 0.0
                } else {
                    false
                }
            }
            Value::String(s) => s != "false" && s != "0" && !s.is_empty(),
            Value::Null => false,
            Value::Array(_) => true,
            Value::Object(_) => true,
        }
    }

    pub fn number_convert(&self, x: &Value) -> Option<f64> {
        match x {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn string_convert(&self, x: &Value) -> Option<String> {
        match x {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    let formatted = format!("{:.15}", f);
                    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
                    Some(trimmed.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn extract_var(&self, path: &str) -> Value {
        let frags: Vec<&str> = path.split('/').collect();
        let mut target: &Value = &Value::Object(
            self.vars
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );

        for frag in frags {
            match target {
                Value::Object(map) => {
                    if let Some(val) = map.get(frag) {
                        target = val;
                    } else {
                        return Value::Null;
                    }
                }
                Value::Array(arr) => {
                    if let Ok(idx) = frag.parse::<usize>() {
                        if let Some(val) = arr.get(idx) {
                            target = val;
                        } else {
                            return Value::Null;
                        }
                    } else {
                        return Value::Null;
                    }
                }
                _ => return Value::Null,
            }
        }

        target.clone()
    }

    pub fn compare(&self, lhs: &Value, rhs: &Value) -> Option<i32> {
        if lhs.is_null() {
            return if rhs.is_null() { Some(0) } else { None };
        }
        if rhs.is_null() {
            return None;
        }

        match lhs {
            Value::Number(ln) => {
                if let Some(lval) = ln.as_f64() {
                    if let Some(rval) = self.number_convert(rhs) {
                        return Some(if (lval - rval).abs() < f64::EPSILON {
                            0
                        } else if lval > rval {
                            1
                        } else {
                            -1
                        });
                    }
                }
                None
            }
            Value::String(ls) => {
                if let Some(rs) = self.string_convert(rhs) {
                    return Some(ls.cmp(&rs) as i32);
                }
                None
            }
            Value::Bool(lb) => {
                let rb = self.boolean_convert(rhs);
                Some(match (lb, &rb) {
                    (true, true) | (false, false) => 0,
                    (true, false) => 1,
                    (false, true) => -1,
                })
            }
            _ => {
                if values_equal_deep(lhs, rhs) {
                    Some(0)
                } else {
                    None
                }
            }
        }
    }
}

pub fn values_equal_deep(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(ab), Value::Bool(bb)) => ab == bb,
        (Value::Number(an), Value::Number(bn)) => {
            an.as_f64().zip(bn.as_f64()).map_or(false, |(a, b)| {
                if a.is_nan() && b.is_nan() {
                    true
                } else {
                    (a - b).abs() < f64::EPSILON
                }
            })
        }
        (Value::String(as_), Value::String(bs)) => as_ == bs,
        (Value::Array(aa), Value::Array(ba)) => {
            aa.len() == ba.len() && aa.iter().zip(ba.iter()).all(|(x, y)| values_equal_deep(x, y))
        }
        (Value::Object(ao), Value::Object(bo)) => {
            ao.len() == bo.len()
                && ao.iter().all(|(k, v)| bo.get(k).map_or(false, |bv| values_equal_deep(v, bv)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_evaluator() -> Evaluator {
        Evaluator::new(HashMap::new())
    }

    fn make_evaluator_with_vars() -> Evaluator {
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), json!(1));
        vars.insert("b".to_string(), json!(true));
        vars.insert("c".to_string(), json!(false));
        vars.insert("d".to_string(), json!([1, 2, 3]));
        vars.insert("e".to_string(), json!([1, {"z": 2}, 3]));
        vars.insert("f".to_string(), json!({"y": {"x": 3, "0": 10}}));
        Evaluator::new(vars)
    }

    #[test]
    fn test_boolean_convert_truthy() {
        let evaluator = make_evaluator();
        assert!(evaluator.boolean_convert(&json!({})));
        assert!(evaluator.boolean_convert(&json!([])));
        assert!(evaluator.boolean_convert(&json!(true)));
        assert!(evaluator.boolean_convert(&json!(1)));
        assert!(evaluator.boolean_convert(&json!(2)));
        assert!(evaluator.boolean_convert(&json!("abc")));
        assert!(evaluator.boolean_convert(&json!("1")));
    }

    #[test]
    fn test_boolean_convert_falsy() {
        let evaluator = make_evaluator();
        assert!(!evaluator.boolean_convert(&Value::Null));
        assert!(!evaluator.boolean_convert(&json!(false)));
        assert!(!evaluator.boolean_convert(&json!(0)));
        assert!(!evaluator.boolean_convert(&json!("")));
        assert!(!evaluator.boolean_convert(&json!("0")));
        assert!(!evaluator.boolean_convert(&json!("false")));
    }

    #[test]
    fn test_number_convert_null_values() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.number_convert(&Value::Null), None);
        assert_eq!(evaluator.number_convert(&json!({})), None);
        assert_eq!(evaluator.number_convert(&json!([])), None);
        assert_eq!(evaluator.number_convert(&json!("")), None);
        assert_eq!(evaluator.number_convert(&json!("abc")), None);
        assert_eq!(evaluator.number_convert(&json!("x1234")), None);
    }

    #[test]
    fn test_number_convert_booleans() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.number_convert(&json!(true)), Some(1.0));
        assert_eq!(evaluator.number_convert(&json!(false)), Some(0.0));
    }

    #[test]
    fn test_number_convert_numbers() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.number_convert(&json!(-1.0)), Some(-1.0));
        assert_eq!(evaluator.number_convert(&json!(0.0)), Some(0.0));
        assert_eq!(evaluator.number_convert(&json!(1.0)), Some(1.0));
        assert_eq!(evaluator.number_convert(&json!(1.5)), Some(1.5));
        assert_eq!(evaluator.number_convert(&json!(2.0)), Some(2.0));
        assert_eq!(evaluator.number_convert(&json!(3.0)), Some(3.0));
        assert_eq!(evaluator.number_convert(&json!(2147483647)), Some(2147483647.0));
        assert_eq!(evaluator.number_convert(&json!(-2147483647)), Some(-2147483647.0));
    }

    #[test]
    fn test_number_convert_strings() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.number_convert(&json!("-1")), Some(-1.0));
        assert_eq!(evaluator.number_convert(&json!("0")), Some(0.0));
        assert_eq!(evaluator.number_convert(&json!("1")), Some(1.0));
        assert_eq!(evaluator.number_convert(&json!("1.5")), Some(1.5));
        assert_eq!(evaluator.number_convert(&json!("2")), Some(2.0));
        assert_eq!(evaluator.number_convert(&json!("3.0")), Some(3.0));
    }

    #[test]
    fn test_string_convert_null_values() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.string_convert(&Value::Null), None);
        assert_eq!(evaluator.string_convert(&json!({})), None);
        assert_eq!(evaluator.string_convert(&json!([])), None);
    }

    #[test]
    fn test_string_convert_booleans() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.string_convert(&json!(true)), Some("true".to_string()));
        assert_eq!(evaluator.string_convert(&json!(false)), Some("false".to_string()));
    }

    #[test]
    fn test_string_convert_strings() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.string_convert(&json!("")), Some("".to_string()));
        assert_eq!(evaluator.string_convert(&json!("abc")), Some("abc".to_string()));
    }

    #[test]
    fn test_string_convert_numbers() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.string_convert(&json!(-1.0)), Some("-1".to_string()));
        assert_eq!(evaluator.string_convert(&json!(0.0)), Some("0".to_string()));
        assert_eq!(evaluator.string_convert(&json!(1.0)), Some("1".to_string()));
        assert_eq!(evaluator.string_convert(&json!(2.0)), Some("2".to_string()));
        assert_eq!(evaluator.string_convert(&json!(3.0)), Some("3".to_string()));
        assert_eq!(evaluator.string_convert(&json!(2147483647.0)), Some("2147483647".to_string()));
        assert_eq!(evaluator.string_convert(&json!(-2147483647.0)), Some("-2147483647".to_string()));
    }

    #[test]
    fn test_extract_var_top_level() {
        let evaluator = make_evaluator_with_vars();
        assert_eq!(evaluator.extract_var("a"), json!(1));
        assert_eq!(evaluator.extract_var("b"), json!(true));
        assert_eq!(evaluator.extract_var("c"), json!(false));
        assert_eq!(evaluator.extract_var("d"), json!([1, 2, 3]));
        assert_eq!(evaluator.extract_var("e"), json!([1, {"z": 2}, 3]));
        assert_eq!(evaluator.extract_var("f"), json!({"y": {"x": 3, "0": 10}}));
    }

    #[test]
    fn test_extract_var_invalid_path_from_primitive() {
        let evaluator = make_evaluator_with_vars();
        assert_eq!(evaluator.extract_var("a/0"), Value::Null);
        assert_eq!(evaluator.extract_var("a/b"), Value::Null);
        assert_eq!(evaluator.extract_var("b/0"), Value::Null);
        assert_eq!(evaluator.extract_var("b/e"), Value::Null);
    }

    #[test]
    fn test_extract_var_array_index() {
        let evaluator = make_evaluator_with_vars();
        assert_eq!(evaluator.extract_var("d/0"), json!(1));
        assert_eq!(evaluator.extract_var("d/1"), json!(2));
        assert_eq!(evaluator.extract_var("d/2"), json!(3));
        assert_eq!(evaluator.extract_var("d/3"), Value::Null);
    }

    #[test]
    fn test_extract_var_nested() {
        let evaluator = make_evaluator_with_vars();
        assert_eq!(evaluator.extract_var("e/0"), json!(1));
        assert_eq!(evaluator.extract_var("e/1/z"), json!(2));
        assert_eq!(evaluator.extract_var("e/2"), json!(3));
        assert_eq!(evaluator.extract_var("e/1/0"), Value::Null);
    }

    #[test]
    fn test_extract_var_object_nested() {
        let evaluator = make_evaluator_with_vars();
        assert_eq!(evaluator.extract_var("f/y/x"), json!(3));
        assert_eq!(evaluator.extract_var("f/y/0"), json!(10));
    }

    #[test]
    fn test_compare_null_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&Value::Null, &Value::Null), Some(0));
    }

    #[test]
    fn test_compare_null_with_other() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&Value::Null, &json!(0)), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!(1)), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!(true)), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!(false)), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!("")), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!("abc")), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!({})), None);
        assert_eq!(evaluator.compare(&Value::Null, &json!([])), None);
    }

    #[test]
    fn test_compare_other_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!(0), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!(1), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!(true), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!(false), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!(""), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!("abc"), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!({}), &Value::Null), None);
        assert_eq!(evaluator.compare(&json!([]), &Value::Null), None);
    }

    #[test]
    fn test_compare_objects() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!({}), &json!({})), Some(0));
        assert_eq!(evaluator.compare(&json!({"a": 1}), &json!({"a": 1})), Some(0));
        assert_eq!(evaluator.compare(&json!({"a": 1}), &json!({"b": 2})), None);
        assert_eq!(evaluator.compare(&json!({}), &json!([])), None);
    }

    #[test]
    fn test_compare_arrays() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!([]), &json!([])), Some(0));
        assert_eq!(evaluator.compare(&json!([1, 2]), &json!([1, 2])), Some(0));
        assert_eq!(evaluator.compare(&json!([1, 2]), &json!([3, 4])), None);
        assert_eq!(evaluator.compare(&json!([]), &json!({})), None);
    }

    #[test]
    fn test_compare_booleans() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!(false), &json!(0)), Some(0));
        assert_eq!(evaluator.compare(&json!(false), &json!(1)), Some(-1));
        assert_eq!(evaluator.compare(&json!(false), &json!(true)), Some(-1));
        assert_eq!(evaluator.compare(&json!(false), &json!(false)), Some(0));
        assert_eq!(evaluator.compare(&json!(false), &json!("")), Some(0));
        assert_eq!(evaluator.compare(&json!(false), &json!("abc")), Some(-1));

        assert_eq!(evaluator.compare(&json!(true), &json!(0)), Some(1));
        assert_eq!(evaluator.compare(&json!(true), &json!(1)), Some(0));
        assert_eq!(evaluator.compare(&json!(true), &json!(true)), Some(0));
        assert_eq!(evaluator.compare(&json!(true), &json!(false)), Some(1));
        assert_eq!(evaluator.compare(&json!(true), &json!("")), Some(1));
        assert_eq!(evaluator.compare(&json!(true), &json!("abc")), Some(0));
    }

    #[test]
    fn test_compare_numbers() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!(0), &json!(0)), Some(0));
        assert_eq!(evaluator.compare(&json!(0), &json!(1)), Some(-1));
        assert_eq!(evaluator.compare(&json!(0), &json!(true)), Some(-1));
        assert_eq!(evaluator.compare(&json!(0), &json!(false)), Some(0));

        assert_eq!(evaluator.compare(&json!(1), &json!(0)), Some(1));
        assert_eq!(evaluator.compare(&json!(1), &json!(1)), Some(0));
        assert_eq!(evaluator.compare(&json!(1), &json!(true)), Some(0));
        assert_eq!(evaluator.compare(&json!(1), &json!(false)), Some(1));

        assert_eq!(evaluator.compare(&json!(1.0), &json!(1)), Some(0));
        assert_eq!(evaluator.compare(&json!(1.5), &json!(1)), Some(1));
        assert_eq!(evaluator.compare(&json!(2.0), &json!(1)), Some(1));

        assert_eq!(evaluator.compare(&json!(1), &json!(1.0)), Some(0));
        assert_eq!(evaluator.compare(&json!(1), &json!(1.5)), Some(-1));
        assert_eq!(evaluator.compare(&json!(1), &json!(2.0)), Some(-1));
    }

    #[test]
    fn test_compare_strings() {
        let evaluator = make_evaluator();
        assert_eq!(evaluator.compare(&json!(""), &json!("")), Some(0));
        assert_eq!(evaluator.compare(&json!("abc"), &json!("abc")), Some(0));
        assert_eq!(evaluator.compare(&json!("0"), &json!(0)), Some(0));
        assert_eq!(evaluator.compare(&json!("1"), &json!(1)), Some(0));
        assert_eq!(evaluator.compare(&json!("true"), &json!(true)), Some(0));
        assert_eq!(evaluator.compare(&json!("false"), &json!(false)), Some(0));

        assert_eq!(evaluator.compare(&json!("abc"), &json!("bcd")), Some(-1));
        assert_eq!(evaluator.compare(&json!("bcd"), &json!("abc")), Some(1));
        assert_eq!(evaluator.compare(&json!("0"), &json!("1")), Some(-1));
        assert_eq!(evaluator.compare(&json!("1"), &json!("0")), Some(1));
    }

    #[test]
    fn test_values_equal_deep_primitives() {
        assert!(values_equal_deep(&json!(0), &json!(0)));
        assert!(values_equal_deep(&json!(1), &json!(1)));
        assert!(values_equal_deep(&json!(1.5), &json!(1.5)));
        assert!(values_equal_deep(&json!(""), &json!("")));
        assert!(values_equal_deep(&json!("abc"), &json!("abc")));
        assert!(values_equal_deep(&json!(true), &json!(true)));
        assert!(values_equal_deep(&json!(false), &json!(false)));

        assert!(!values_equal_deep(&json!(0), &json!(1)));
        assert!(!values_equal_deep(&json!(1), &json!(1.5)));
        assert!(!values_equal_deep(&json!(""), &json!("abc")));
        assert!(!values_equal_deep(&json!(true), &json!(false)));
        assert!(!values_equal_deep(&json!(false), &json!(0)));
    }

    #[test]
    fn test_values_equal_deep_arrays() {
        assert!(values_equal_deep(&json!([1, 2, 3]), &json!([1, 2, 3])));
        assert!(!values_equal_deep(&json!([1, 2, 3]), &json!([3, 2, 1])));
        assert!(!values_equal_deep(&json!([]), &json!([1])));
        assert!(!values_equal_deep(&json!([1]), &json!([])));

        assert!(values_equal_deep(
            &json!([1, 2, [3, 4, [5, 6]]]),
            &json!([1, 2, [3, 4, [5, 6]]])
        ));
        assert!(!values_equal_deep(
            &json!([1, 2, [3, 4, [5, 6]]]),
            &json!([1, 2, [3, 4, [5, 9]]])
        ));
    }

    #[test]
    fn test_values_equal_deep_objects() {
        assert!(values_equal_deep(&json!({"a": 1, "b": 2}), &json!({"a": 1, "b": 2})));
        assert!(values_equal_deep(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1})));
        assert!(!values_equal_deep(&json!({"a": 1}), &json!({"b": 2})));
        assert!(!values_equal_deep(&json!({}), &json!({"a": 1})));
        assert!(!values_equal_deep(&json!({"a": 1}), &json!({})));

        assert!(values_equal_deep(
            &json!({"a": 1, "b": {"c": 2, "d": {"e": 3}}}),
            &json!({"a": 1, "b": {"c": 2, "d": {"e": 3}}})
        ));
        assert!(!values_equal_deep(
            &json!({"a": 1, "b": {"c": 2, "d": {"e": 3}}}),
            &json!({"a": 1, "b": {"c": 2, "d": {"e": 9}}})
        ));
    }
}
