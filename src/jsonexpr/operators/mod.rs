use log::error;
use regex::RegexBuilder;
use serde_json::Value;

use super::evaluator::Evaluator;

/// `and`: false at the first falsy operand, else true. Null when the
/// arguments are not an array.
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
        Value::Null
    }
}

/// `or`: true at the first truthy operand; an empty list is true. Null when
/// the arguments are not an array.
pub fn or_op(evaluator: &Evaluator, args: &Value) -> Value {
    if let Value::Array(exprs) = args {
        for expr in exprs {
            let result = evaluator.evaluate(expr);
            if evaluator.boolean_convert(&result) {
                return Value::Bool(true);
            }
        }
        Value::Bool(exprs.is_empty())
    } else {
        Value::Null
    }
}

pub fn value_op(_evaluator: &Evaluator, args: &Value) -> Value {
    args.clone()
}

/// `var`: the path is a string, or an object with a string `path`.
pub fn var_op(evaluator: &Evaluator, args: &Value) -> Value {
    let path = match args {
        Value::Object(map) => map.get("path").unwrap_or(&Value::Null),
        other => other,
    };
    if let Value::String(path) = path {
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

/// Evaluate a binary operator's two operands. Null (and `op` is not called)
/// when the arguments are not an array or either operand evaluates to null.
fn binary(
    evaluator: &Evaluator,
    args: &Value,
    op: impl FnOnce(&Value, &Value) -> Value,
) -> Value {
    let Value::Array(arr) = args else {
        return Value::Null;
    };
    let lhs = arr.first().map_or(Value::Null, |a| evaluator.evaluate(a));
    if lhs.is_null() {
        return Value::Null;
    }
    let rhs = arr.get(1).map_or(Value::Null, |a| evaluator.evaluate(a));
    if rhs.is_null() {
        return Value::Null;
    }
    op(&lhs, &rhs)
}

/// Map a comparison to a boolean, or null when the operands are not comparable.
fn compared(result: Option<i32>, test: impl FnOnce(i32) -> bool) -> Value {
    result.map_or(Value::Null, |r| Value::Bool(test(r)))
}

/// `in`: `[haystack, needle]`. An array haystack holds the needle when an item
/// compares equal to it; a string haystack contains the needle converted to a
/// string; an object haystack has the needle as a key. Any other haystack is
/// null.
pub fn in_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |haystack, needle| {
        haystack_contains(evaluator, haystack, needle)
    })
}

fn haystack_contains(evaluator: &Evaluator, haystack: &Value, needle: &Value) -> Value {
    match haystack {
        Value::Array(items) => Value::Bool(
            items
                .iter()
                .any(|item| evaluator.compare(item, needle) == Some(0)),
        ),
        Value::String(h) => Value::Bool(
            evaluator
                .string_convert(needle)
                .is_some_and(|n| h.contains(n.as_str())),
        ),
        Value::Object(map) => Value::Bool(
            evaluator
                .string_convert(needle)
                .is_some_and(|n| map.contains_key(&n)),
        ),
        _ => Value::Null,
    }
}

/// `contains`: the same operation as `in`, `[haystack, needle]`.
pub fn contains_op(evaluator: &Evaluator, args: &Value) -> Value {
    in_op(evaluator, args)
}

/// `match`: `[text, pattern]`, both converted to strings. Null when either
/// cannot be converted or the pattern is not a valid regex.
pub fn match_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |text, pattern| {
        let (Some(text), Some(pattern)) =
            (evaluator.string_convert(text), evaluator.string_convert(pattern))
        else {
            return Value::Null;
        };
        match RegexBuilder::new(&pattern)
            .size_limit(100_000)
            .dfa_size_limit(1_000_000)
            .build()
        {
            Ok(regex) => Value::Bool(regex.is_match(&text)),
            Err(e) => {
                error!("Invalid regex pattern '{}': {}", pattern, e);
                Value::Null
            }
        }
    })
}

/// `eq`. A null operand gives null, so `eq(null, null)` is null, not true.
pub fn eq_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |lhs, rhs| {
        compared(evaluator.compare(lhs, rhs), |r| r == 0)
    })
}

pub fn gt_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |lhs, rhs| {
        compared(evaluator.compare(lhs, rhs), |r| r > 0)
    })
}

pub fn gte_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |lhs, rhs| {
        compared(evaluator.compare(lhs, rhs), |r| r >= 0)
    })
}

pub fn lt_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |lhs, rhs| {
        compared(evaluator.compare(lhs, rhs), |r| r < 0)
    })
}

pub fn lte_op(evaluator: &Evaluator, args: &Value) -> Value {
    binary(evaluator, args, |lhs, rhs| {
        compared(evaluator.compare(lhs, rhs), |r| r <= 0)
    })
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
        assert_eq!(
            and_op(&evaluator, &json!([{"value": true}, {"value": true}])),
            json!(true)
        );
        assert_eq!(
            and_op(
                &evaluator,
                &json!([{"value": true}, {"value": true}, {"value": true}])
            ),
            json!(true)
        );
    }

    #[test]
    fn test_and_any_false() {
        let evaluator = make_evaluator();
        assert_eq!(and_op(&evaluator, &json!([{"value": false}])), json!(false));
        assert_eq!(
            and_op(&evaluator, &json!([{"value": true}, {"value": false}])),
            json!(false)
        );
        assert_eq!(
            and_op(&evaluator, &json!([{"value": false}, {"value": true}])),
            json!(false)
        );
        assert_eq!(
            and_op(&evaluator, &json!([{"value": false}, {"value": false}])),
            json!(false)
        );
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
        assert_eq!(
            or_op(&evaluator, &json!([{"value": true}, {"value": true}])),
            json!(true)
        );
        assert_eq!(
            or_op(&evaluator, &json!([{"value": true}, {"value": false}])),
            json!(true)
        );
        assert_eq!(
            or_op(&evaluator, &json!([{"value": false}, {"value": true}])),
            json!(true)
        );
    }

    #[test]
    fn test_or_all_false() {
        let evaluator = make_evaluator();
        assert_eq!(or_op(&evaluator, &json!([{"value": false}])), json!(false));
        assert_eq!(
            or_op(&evaluator, &json!([{"value": false}, {"value": false}])),
            json!(false)
        );
        assert_eq!(
            or_op(
                &evaluator,
                &json!([{"value": false}, {"value": false}, {"value": false}])
            ),
            json!(false)
        );
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
        assert_eq!(
            not_op(&evaluator, &json!({"var": "returning"})),
            json!(false)
        );
    }

    #[test]
    fn test_eq_op_numbers() {
        let evaluator = make_evaluator();
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": 0}, {"value": 0}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": 1}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": 0}, {"value": 1}])),
            json!(false)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])),
            json!(false)
        );
    }

    #[test]
    fn test_eq_op_strings() {
        let evaluator = make_evaluator();
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": ""}, {"value": ""}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": "abc"}, {"value": "abc"}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": "abc"}, {"value": "def"}])),
            json!(false)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"var": "name"}, {"value": "John"}])),
            json!(true)
        );
    }

    #[test]
    fn test_eq_op_booleans() {
        let evaluator = make_evaluator();
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": true}, {"value": true}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": false}, {"value": false}])),
            json!(true)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": true}, {"value": false}])),
            json!(false)
        );
    }

    #[test]
    fn test_eq_op_null() {
        let evaluator = make_evaluator();
        // A null operand short-circuits to null (canonical behavior).
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": null}, {"value": null}])),
            json!(null)
        );
        assert_eq!(
            eq_op(&evaluator, &json!([{"value": null}, {"value": 0}])),
            json!(null)
        );
    }

    #[test]
    fn test_gt_op() {
        let evaluator = make_evaluator();
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": 1}, {"value": 0}])),
            json!(true)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": 0}, {"value": 1}])),
            json!(false)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": 1}, {"value": 1}])),
            json!(false)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])),
            json!(true)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])),
            json!(false)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])),
            json!(false)
        );
    }

    #[test]
    fn test_gt_op_strings() {
        let evaluator = make_evaluator();
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": "b"}, {"value": "a"}])),
            json!(true)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": "a"}, {"value": "b"}])),
            json!(false)
        );
        assert_eq!(
            gt_op(&evaluator, &json!([{"value": "a"}, {"value": "a"}])),
            json!(false)
        );
    }

    #[test]
    fn test_gte_op() {
        let evaluator = make_evaluator();
        assert_eq!(
            gte_op(&evaluator, &json!([{"value": 1}, {"value": 0}])),
            json!(true)
        );
        assert_eq!(
            gte_op(&evaluator, &json!([{"value": 1}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            gte_op(&evaluator, &json!([{"value": 0}, {"value": 1}])),
            json!(false)
        );
        assert_eq!(
            gte_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])),
            json!(true)
        );
        assert_eq!(
            gte_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])),
            json!(true)
        );
        assert_eq!(
            gte_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])),
            json!(false)
        );
    }

    #[test]
    fn test_lt_op() {
        let evaluator = make_evaluator();
        assert_eq!(
            lt_op(&evaluator, &json!([{"value": 0}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            lt_op(&evaluator, &json!([{"value": 1}, {"value": 0}])),
            json!(false)
        );
        assert_eq!(
            lt_op(&evaluator, &json!([{"value": 1}, {"value": 1}])),
            json!(false)
        );
        assert_eq!(
            lt_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])),
            json!(true)
        );
        assert_eq!(
            lt_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])),
            json!(false)
        );
        assert_eq!(
            lt_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])),
            json!(false)
        );
    }

    #[test]
    fn test_lte_op() {
        let evaluator = make_evaluator();
        assert_eq!(
            lte_op(&evaluator, &json!([{"value": 0}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            lte_op(&evaluator, &json!([{"value": 1}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            lte_op(&evaluator, &json!([{"value": 1}, {"value": 0}])),
            json!(false)
        );
        assert_eq!(
            lte_op(&evaluator, &json!([{"var": "age"}, {"value": 30}])),
            json!(true)
        );
        assert_eq!(
            lte_op(&evaluator, &json!([{"var": "age"}, {"value": 25}])),
            json!(true)
        );
        assert_eq!(
            lte_op(&evaluator, &json!([{"var": "age"}, {"value": 20}])),
            json!(false)
        );
    }

    #[test]
    fn test_in_op_is_haystack_first_only() {
        let evaluator = Evaluator::new(HashMap::new());
        let haystack_first = json!([{"value": ["US", "CA"]}, {"value": "US"}]);
        let miss = json!([{"value": ["US", "CA"]}, {"value": "FR"}]);
        // The old needle-first order is no longer recognised: a string
        // haystack does not contain the array needle.
        let needle_first = json!([{"value": "US"}, {"value": ["US", "CA"]}]);
        assert_eq!(in_op(&evaluator, &haystack_first), Value::Bool(true));
        assert_eq!(in_op(&evaluator, &miss), Value::Bool(false));
        assert_eq!(in_op(&evaluator, &needle_first), Value::Bool(false));
    }

    #[test]
    fn test_in_op_non_collection_haystack_is_null() {
        let evaluator = make_evaluator();
        assert_eq!(in_op(&evaluator, &json!([{"value": 5}, {"value": 5}])), Value::Null);
        assert_eq!(
            in_op(&evaluator, &json!([{"value": true}, {"value": true}])),
            Value::Null
        );
        assert_eq!(
            in_op(&evaluator, &json!([{"value": null}, {"value": "a"}])),
            Value::Null
        );
        assert_eq!(
            in_op(&evaluator, &json!([{"value": ["a"]}, {"value": null}])),
            Value::Null
        );
        assert_eq!(in_op(&evaluator, &json!({"value": 1})), Value::Null);
    }

    #[test]
    fn test_in_op_converts_needle() {
        let evaluator = make_evaluator();
        // Array items are compared with the evaluator's coercion.
        assert_eq!(
            in_op(&evaluator, &json!([{"value": [1, 2, 3]}, {"value": "2"}])),
            json!(true)
        );
        assert_eq!(
            in_op(&evaluator, &json!([{"value": ["1", "2"]}, {"value": 2}])),
            json!(true)
        );
        // String haystack: a number or boolean needle is converted to a string.
        assert_eq!(
            in_op(&evaluator, &json!([{"value": "abc123"}, {"value": 123}])),
            json!(true)
        );
        assert_eq!(
            in_op(&evaluator, &json!([{"value": "is true"}, {"value": true}])),
            json!(true)
        );
        // Object haystack: key lookup with the needle as a string.
        assert_eq!(
            in_op(&evaluator, &json!([{"value": {"1": "x"}}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            in_op(&evaluator, &json!([{"value": "abc"}, {"value": [1]}])),
            json!(false)
        );
    }

    #[test]
    fn test_binary_operators_null_operand_is_null() {
        let evaluator = make_evaluator();
        for op in [eq_op, gt_op, gte_op, lt_op, lte_op, match_op, in_op, contains_op] {
            assert_eq!(op(&evaluator, &json!([{"value": null}, {"value": 1}])), Value::Null);
            assert_eq!(op(&evaluator, &json!([{"value": "1"}, {"value": null}])), Value::Null);
            assert_eq!(op(&evaluator, &json!([{"value": "1"}])), Value::Null);
            assert_eq!(op(&evaluator, &json!("1")), Value::Null);
        }
    }

    #[test]
    fn test_compare_operators_not_comparable_is_null() {
        let evaluator = make_evaluator();
        for op in [eq_op, gt_op, gte_op, lt_op, lte_op] {
            assert_eq!(
                op(&evaluator, &json!([{"value": 1}, {"value": "abc"}])),
                Value::Null
            );
        }
    }

    #[test]
    fn test_and_or_non_array_and_empty() {
        let evaluator = make_evaluator();
        assert_eq!(and_op(&evaluator, &json!({"value": true})), Value::Null);
        assert_eq!(or_op(&evaluator, &json!({"value": true})), Value::Null);
        assert_eq!(and_op(&evaluator, &json!([])), json!(true));
        assert_eq!(or_op(&evaluator, &json!([])), json!(true));
    }

    #[test]
    fn test_var_op_object_path() {
        let evaluator = make_evaluator();
        assert_eq!(var_op(&evaluator, &json!({"path": "age"})), json!(25));
        assert_eq!(var_op(&evaluator, &json!({"path": 1})), Value::Null);
    }

    #[test]
    fn test_contains_op_string_contains() {
        let evaluator = make_evaluator();
        // [haystack, needle]
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "abc"}])
            ),
            json!(true)
        );
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "def"}])
            ),
            json!(true)
        );
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "xyz"}])
            ),
            json!(false)
        );
    }

    #[test]
    fn test_contains_op_array_contains() {
        let evaluator = make_evaluator();
        // [haystack, needle]
        assert_eq!(
            contains_op(&evaluator, &json!([{"value": [1, 2, 3]}, {"value": 1}])),
            json!(true)
        );
        assert_eq!(
            contains_op(&evaluator, &json!([{"value": [1, 2, 3]}, {"value": 2}])),
            json!(true)
        );
        assert_eq!(
            contains_op(&evaluator, &json!([{"value": [1, 2, 3]}, {"value": 4}])),
            json!(false)
        );
        assert_eq!(
            contains_op(&evaluator, &json!([{"value": []}, {"value": 1}])),
            json!(false)
        );
    }

    #[test]
    fn test_contains_op_object_contains_key() {
        let evaluator = make_evaluator();
        // [haystack, needle]
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": {"a": 1, "b": 2}}, {"value": "a"}])
            ),
            json!(true)
        );
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": {"a": 1, "b": 2}}, {"value": "b"}])
            ),
            json!(true)
        );
        assert_eq!(
            contains_op(
                &evaluator,
                &json!([{"value": {"a": 1, "b": 2}}, {"value": "c"}])
            ),
            json!(false)
        );
    }

    #[test]
    fn test_match_op() {
        let evaluator = make_evaluator();
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": ""}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "abc"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "ijk"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "^abc"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "ijk$"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "def"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "b.*j"}])
            ),
            json!(true)
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": "xyz"}])
            ),
            json!(false)
        );
    }

    #[test]
    fn test_match_op_with_null() {
        let evaluator = make_evaluator();
        assert_eq!(
            match_op(&evaluator, &json!([{"value": null}, {"value": "abc"}])),
            Value::Null
        );
        assert_eq!(
            match_op(
                &evaluator,
                &json!([{"value": "abcdefghijk"}, {"value": null}])
            ),
            Value::Null
        );
    }

    #[test]
    fn test_match_op_larger_pattern() {
        let evaluator = make_evaluator();
        let long_alternation: String = (0..200)
            .map(|i| format!("option{}", i))
            .collect::<Vec<_>>()
            .join("|");
        let result = match_op(
            &evaluator,
            &json!([{"value": "option150"}, {"value": long_alternation}]),
        );
        assert_eq!(result, json!(true));
    }

    #[test]
    fn test_match_op_invalid_pattern_returns_null() {
        let evaluator = make_evaluator();
        let result = match_op(&evaluator, &json!([{"value": "abc"}, {"value": "("}]));
        assert_eq!(result, Value::Null);
    }
}
