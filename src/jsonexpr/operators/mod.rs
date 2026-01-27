use regex::Regex;
use serde_json::Value;

use super::evaluator::{values_equal_deep, Evaluator};

pub fn and_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(exprs) = args {
        for expr in exprs {
            let result = evaluator.evaluate(expr);
            if !evaluator.boolean_convert(&result) {
                return Value::Bool(false);
            }
        }
        Value::Bool(true)
    } else {
        Value::Bool(false)
    }
}

pub fn or_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(exprs) = args {
        for expr in exprs {
            let result = evaluator.evaluate(expr);
            if evaluator.boolean_convert(&result) {
                return Value::Bool(true);
            }
        }
        Value::Bool(false)
    } else {
        Value::Bool(false)
    }
}

pub fn value_op(_evaluator: &Evaluator, args: &Value) -> Value {
    args.clone()
}

pub fn var_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::String(path) = args {
        evaluator.extract_var(path)
    } else {
        Value::Null
    }
}

pub fn null_op(evaluator: &Evaluator, args: &Value) -> Value {
    let result = evaluator.evaluate(args);
    Value::Bool(result.is_null())
}

pub fn not_op(evaluator: &Evaluator, args: &Value) -> Value {
    let result = evaluator.evaluate(args);
    Value::Bool(!evaluator.boolean_convert(&result))
}

pub fn in_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let needle = evaluator.evaluate(&arr[0]);
        let haystack = evaluator.evaluate(&arr[1]);

        match (&needle, &haystack) {
            (Value::String(n), Value::String(h)) => Value::Bool(h.contains(n.as_str())),
            (_, Value::Array(h_arr)) => {
                Value::Bool(h_arr.iter().any(|item| values_equal_deep(&needle, item)))
            }
            (Value::String(n), Value::Object(h_map)) => Value::Bool(h_map.contains_key(n)),
            _ => Value::Bool(false),
        }
    } else {
        Value::Bool(false)
    }
}

pub fn match_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let text = evaluator.evaluate(&arr[0]);
        let pattern = evaluator.evaluate(&arr[1]);

        if let (Some(text_str), Some(pattern_str)) = (
            evaluator.string_convert(&text),
            evaluator.string_convert(&pattern),
        ) {
            if let Ok(regex) = Regex::new(&pattern_str) {
                return Value::Bool(regex.is_match(&text_str));
            }
        }
        Value::Bool(false)
    } else {
        Value::Bool(false)
    }
}

pub fn eq_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let lhs = evaluator.evaluate(&arr[0]);
        let rhs = evaluator.evaluate(&arr[1]);
        let result = evaluator.compare(&lhs, &rhs);

        Value::Bool(result == Some(0))
    } else {
        Value::Bool(false)
    }
}

pub fn gt_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let lhs = evaluator.evaluate(&arr[0]);
        let rhs = evaluator.evaluate(&arr[1]);
        let result = evaluator.compare(&lhs, &rhs);

        Value::Bool(result.map_or(false, |r| r > 0))
    } else {
        Value::Bool(false)
    }
}

pub fn gte_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let lhs = evaluator.evaluate(&arr[0]);
        let rhs = evaluator.evaluate(&arr[1]);
        let result = evaluator.compare(&lhs, &rhs);

        Value::Bool(result.map_or(false, |r| r >= 0))
    } else {
        Value::Bool(false)
    }
}

pub fn lt_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let lhs = evaluator.evaluate(&arr[0]);
        let rhs = evaluator.evaluate(&arr[1]);
        let result = evaluator.compare(&lhs, &rhs);

        Value::Bool(result.map_or(false, |r| r < 0))
    } else {
        Value::Bool(false)
    }
}

pub fn lte_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(arr) = args {
        if arr.len() != 2 {
            return Value::Bool(false);
        }

        let lhs = evaluator.evaluate(&arr[0]);
        let rhs = evaluator.evaluate(&arr[1]);
        let result = evaluator.compare(&lhs, &rhs);

        Value::Bool(result.map_or(false, |r| r <= 0))
    } else {
        Value::Bool(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn make_evaluator() -> Evaluator {
        let mut vars = HashMap::new();
        vars.insert("age".to_string(), json!(25));
        vars.insert("name".to_string(), json!("John"));
        vars.insert("country".to_string(), json!("US"));
        vars.insert("returning".to_string(), json!(true));
        Evaluator::new(vars)
    }

    #[test]
    fn test_and_all_true() {
        let evaluator = make_evaluator();
        assert_eq!(and_op(&evaluator, &json!([{"value": true}])), json!(true));
        assert_eq!(and_op(&evaluator, &json!([{"value": true}, {"value": true}])), json!(true));
        assert_eq!(and_op(&evaluator, &json!([{"value": true}, {"value": true}, {"value": true}])), json!(true));
    }

    #[test]
    fn test_and_any_false() {
        let evaluator = make_evaluator();
        assert_eq!(and_op(&evaluator, &json!([{"value": false}])), json!(false));
        assert_eq!(and_op(&evaluator, &json!([{"value": true}, {"value": false}])), json!(false));
        assert_eq!(and_op(&evaluator, &json!([{"value": false}, {"value": true}])), json!(false));
        assert_eq!(and_op(&evaluator, &json!([{"value": false}, {"value": false}])), json!(false));
    }

    #[test]
    fn test_and_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(and_op(&evaluator, &json!([{"value": null}])), json!(false));
    }

    #[test]
    fn test_or_any_true() {
        let evaluator = make_evaluator();
        assert_eq!(or_op(&evaluator, &json!([{"value": true}])), json!(true));
        assert_eq!(or_op(&evaluator, &json!([{"value": true}, {"value": true}])), json!(true));
        assert_eq!(or_op(&evaluator, &json!([{"value": true}, {"value": false}])), json!(true));
        assert_eq!(or_op(&evaluator, &json!([{"value": false}, {"value": true}])), json!(true));
    }

    #[test]
    fn test_or_all_false() {
        let evaluator = make_evaluator();
        assert_eq!(or_op(&evaluator, &json!([{"value": false}])), json!(false));
        assert_eq!(or_op(&evaluator, &json!([{"value": false}, {"value": false}])), json!(false));
        assert_eq!(or_op(&evaluator, &json!([{"value": false}, {"value": false}, {"value": false}])), json!(false));
    }

    #[test]
    fn test_or_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(or_op(&evaluator, &json!([{"value": null}])), json!(false));
    }

    #[test]
    fn test_value_op() {
        let evaluator = make_evaluator();
        assert_eq!(value_op(&evaluator, &json!(5)), json!(5));
        assert_eq!(value_op(&evaluator, &json!(true)), json!(true));
        assert_eq!(value_op(&evaluator, &json!("abc")), json!("abc"));
        assert_eq!(value_op(&evaluator, &json!([1, 2, 3])), json!([1, 2, 3]));
        assert_eq!(value_op(&evaluator, &Value::Null), Value::Null);
    }

    #[test]
    fn test_var_op() {
        let evaluator = make_evaluator();
        assert_eq!(var_op(&evaluator, &json!("age")), json!(25));
        assert_eq!(var_op(&evaluator, &json!("name")), json!("John"));
        assert_eq!(var_op(&evaluator, &json!("country")), json!("US"));
        assert_eq!(var_op(&evaluator, &json!("nonexistent")), Value::Null);
    }

    #[test]
    fn test_null_op() {
        let evaluator = make_evaluator();
        assert_eq!(null_op(&evaluator, &json!({"value": null})), json!(true));
        assert_eq!(null_op(&evaluator, &json!({"value": 0})), json!(false));
        assert_eq!(null_op(&evaluator, &json!({"value": false})), json!(false));
        assert_eq!(null_op(&evaluator, &json!({"value": ""})), json!(false));
    }

    #[test]
    fn test_not_op() {
        let evaluator = make_evaluator();
        assert_eq!(not_op(&evaluator, &json!({"value": true})), json!(false));
        assert_eq!(not_op(&evaluator, &json!({"value": false})), json!(true));
        assert_eq!(not_op(&evaluator, &json!({"value": 1})), json!(false));
        assert_eq!(not_op(&evaluator, &json!({"value": 0})), json!(true));
        assert_eq!(not_op(&evaluator, &json!({"value": null})), json!(true));
        assert_eq!(not_op(&evaluator, &json!({"var": "returning"})), json!(false));
    }

    #[test]
    fn test_eq_op_numbers() {
        let evaluator = make_evaluator();
        assert_eq!(eq_op(&evaluator, &json!([{"value": 0}, {"value": 0}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": 1}, {"value": 1}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": 0}, {"value": 1}])), json!(false));
        assert_eq!(eq_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])), json!(false));
    }

    #[test]
    fn test_eq_op_strings() {
        let evaluator = make_evaluator();
        assert_eq!(eq_op(&evaluator, &json!([{"value": ""}, {"value": ""}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": "abc"}, {"value": "abc"}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": "abc"}, {"value": "def"}])), json!(false));
        assert_eq!(eq_op(&evaluator, &json!([{"var": "name"}, {"value": "John"}])), json!(true));
    }

    #[test]
    fn test_eq_op_booleans() {
        let evaluator = make_evaluator();
        assert_eq!(eq_op(&evaluator, &json!([{"value": true}, {"value": true}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": false}, {"value": false}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": true}, {"value": false}])), json!(false));
    }

    #[test]
    fn test_eq_op_null() {
        let evaluator = make_evaluator();
        assert_eq!(eq_op(&evaluator, &json!([{"value": null}, {"value": null}])), json!(true));
        assert_eq!(eq_op(&evaluator, &json!([{"value": null}, {"value": 0}])), json!(false));
    }

    #[test]
    fn test_gt_op() {
        let evaluator = make_evaluator();
        assert_eq!(gt_op(&evaluator, &json!([{"value": 1}, {"value": 0}])), json!(true));
        assert_eq!(gt_op(&evaluator, &json!([{"value": 0}, {"value": 1}])), json!(false));
        assert_eq!(gt_op(&evaluator, &json!([{"value": 1}, {"value": 1}])), json!(false));
        assert_eq!(gt_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])), json!(true));
        assert_eq!(gt_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])), json!(false));
        assert_eq!(gt_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])), json!(false));
    }

    #[test]
    fn test_gt_op_strings() {
        let evaluator = make_evaluator();
        assert_eq!(gt_op(&evaluator, &json!([{"value": "b"}, {"value": "a"}])), json!(true));
        assert_eq!(gt_op(&evaluator, &json!([{"value": "a"}, {"value": "b"}])), json!(false));
        assert_eq!(gt_op(&evaluator, &json!([{"value": "a"}, {"value": "a"}])), json!(false));
    }

    #[test]
    fn test_gte_op() {
        let evaluator = make_evaluator();
        assert_eq!(gte_op(&evaluator, &json!([{"value": 1}, {"value": 0}])), json!(true));
        assert_eq!(gte_op(&evaluator, &json!([{"value": 1}, {"value": 1}])), json!(true));
        assert_eq!(gte_op(&evaluator, &json!([{"value": 0}, {"value": 1}])), json!(false));
        assert_eq!(gte_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])), json!(true));
        assert_eq!(gte_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])), json!(true));
        assert_eq!(gte_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])), json!(false));
    }

    #[test]
    fn test_lt_op() {
        let evaluator = make_evaluator();
        assert_eq!(lt_op(&evaluator, &json!([{"value": 0}, {"value": 1}])), json!(true));
        assert_eq!(lt_op(&evaluator, &json!([{"value": 1}, {"value": 0}])), json!(false));
        assert_eq!(lt_op(&evaluator, &json!([{"value": 1}, {"value": 1}])), json!(false));
        assert_eq!(lt_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])), json!(true));
        assert_eq!(lt_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])), json!(false));
        assert_eq!(lt_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])), json!(false));
    }

    #[test]
    fn test_lte_op() {
        let evaluator = make_evaluator();
        assert_eq!(lte_op(&evaluator, &json!([{"value": 0}, {"value": 1}])), json!(true));
        assert_eq!(lte_op(&evaluator, &json!([{"value": 1}, {"value": 1}])), json!(true));
        assert_eq!(lte_op(&evaluator, &json!([{"value": 1}, {"value": 0}])), json!(false));
        assert_eq!(lte_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])), json!(true));
        assert_eq!(lte_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])), json!(true));
        assert_eq!(lte_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])), json!(false));
    }

    #[test]
    fn test_in_op_string_contains() {
        let evaluator = make_evaluator();
        assert_eq!(in_op(&evaluator, &json!([{"value": "abc"}, {"value": "abcdefghijk"}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": "def"}, {"value": "abcdefghijk"}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": "xyz"}, {"value": "abcdefghijk"}])), json!(false));
    }

    #[test]
    fn test_in_op_array_contains() {
        let evaluator = make_evaluator();
        assert_eq!(in_op(&evaluator, &json!([{"value": 1}, {"value": [1, 2, 3]}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": 2}, {"value": [1, 2, 3]}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": 4}, {"value": [1, 2, 3]}])), json!(false));
        assert_eq!(in_op(&evaluator, &json!([{"value": 1}, {"value": []}])), json!(false));
    }

    #[test]
    fn test_in_op_object_contains_key() {
        let evaluator = make_evaluator();
        assert_eq!(in_op(&evaluator, &json!([{"value": "a"}, {"value": {"a": 1, "b": 2}}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": "b"}, {"value": {"a": 1, "b": 2}}])), json!(true));
        assert_eq!(in_op(&evaluator, &json!([{"value": "c"}, {"value": {"a": 1, "b": 2}}])), json!(false));
    }

    #[test]
    fn test_match_op() {
        let evaluator = make_evaluator();
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": ""}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "abc"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "ijk"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "^abc"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "ijk$"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "def"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "b.*j"}])), json!(true));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": "xyz"}])), json!(false));
    }

    #[test]
    fn test_match_op_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(match_op(&evaluator, &json!([{"value": null}, {"value": "abc"}])), json!(false));
        assert_eq!(match_op(&evaluator, &json!([{"value": "abcdefghijk"}, {"value": null}])), json!(false));
    }
}
