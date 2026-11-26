//! Several experiments defining the same variable key, as in java-sdk's
//! `Context.getVariableAssignment`: the candidates are the experiments with
//! the key in any variant, in id order. The first one the unit is in
//! (assigned, overridden or set by a rule) wins and is the only experiment
//! exposed, even when its variant does not define the key. Failing that, the
//! first held-out candidate gives its control value. Otherwise the caller's
//! default is returned.

use absmartly::{Context, ContextData};
use serde_json::{json, Value};

const UID: &str = "e791e240fcd3df7d238cfc285f475e8152fcc0ec";

/// Assigns variant 1 to every unit; variant 0 has `{key: "control"}`.
fn experiment(id: i64, name: &str, key: &str, value: Value) -> Value {
    experiment_with_unit(id, name, "session_id", key, value)
}

fn experiment_with_unit(id: i64, name: &str, unit: &str, key: &str, value: Value) -> Value {
    json!({
        "id": id, "name": name, "unitType": unit, "iteration": 1,
        "seedHi": 1, "seedLo": 2, "split": [0.0, 1.0],
        "trafficSeedHi": 3, "trafficSeedLo": 4, "trafficSplit": [0.0, 1.0],
        "fullOnVariant": 0, "audienceStrict": false, "audience": "",
        "variants": [
            {"config": json!({key: "control"}).to_string()},
            {"config": json!({key: value}).to_string()}
        ]
    })
}

/// Only US visitors match; a strict mismatch leaves the unit unassigned.
fn strict_us_only(mut experiment: Value) -> Value {
    experiment["audienceStrict"] = json!(true);
    experiment["audience"] = json!(r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#);
    experiment
}

fn variant_config(mut experiment: Value, variant: usize, config: Value) -> Value {
    experiment["variants"][variant]["config"] = json!(config.to_string());
    experiment
}

fn context(experiments: Vec<Value>) -> Context {
    context_with_holdouts(vec![], experiments)
}

fn context_with_holdouts(holdouts: Vec<Value>, experiments: Vec<Value>) -> Context {
    let data: ContextData =
        serde_json::from_value(json!({"experiments": experiments, "holdouts": holdouts})).unwrap();
    let mut ctx = Context::new(data);
    ctx.set_unit("session_id", UID).unwrap();
    ctx.set_attribute("country", "UK").unwrap();
    ctx
}

/// The queued exposures as `(name, variant, assigned, overridden, rule_override)`.
fn exposed(ctx: &mut Context) -> Vec<(String, i32, bool, bool, bool)> {
    ctx.get_publish_params()
        .exposures
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.name, e.variant, e.assigned, e.overridden, e.rule_override))
        .collect()
}

fn assigned(name: &str) -> (String, i32, bool, bool, bool) {
    (name.to_string(), 1, true, false, false)
}

#[test]
fn unassigned_lower_id_candidate_by_strict_audience_is_not_exposed() {
    let mut ctx = context(vec![
        strict_us_only(experiment(1, "exp_us_only", "color", json!("red"))),
        experiment(2, "exp_everyone", "color", json!("blue")),
    ]);

    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("blue")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_everyone")]);
}

#[test]
fn lower_id_candidate_without_its_unit_is_not_exposed() {
    let mut ctx = context(vec![
        experiment_with_unit(1, "exp_by_user", "user_id", "color", json!("red")),
        experiment(2, "exp_by_session", "color", json!("blue")),
    ]);

    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("blue")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_by_session")]);
    ctx.treatment("exp_by_user").unwrap();
    assert_eq!(
        exposed(&mut ctx),
        vec![
            assigned("exp_by_session"),
            ("exp_by_user".to_string(), 0, false, false, false)
        ]
    );
}

#[test]
fn candidates_are_ordered_by_experiment_id_not_data_order() {
    let mut ctx = context(vec![
        experiment(9, "exp_high", "color", json!("high")),
        experiment(3, "exp_low", "color", json!("low")),
    ]);

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("low")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("low")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_low")]);
}

#[test]
fn refresh_rebuilds_cached_override_when_experiment_becomes_available() {
    let mut ctx = context(vec![]);
    ctx.set_override("exp_new", 1).unwrap();
    assert_eq!(ctx.peek("exp_new").unwrap(), 1);

    let refreshed: ContextData = serde_json::from_value(json!({
        "experiments": [experiment(2, "exp_new", "color", json!("new"))],
        "holdouts": []
    }))
    .unwrap();
    ctx.refresh_with(refreshed);

    for _ in 0..3 {
        assert_eq!(
            ctx.variable_value("color", "default").unwrap(),
            json!("new")
        );
    }
    assert_eq!(
        exposed(&mut ctx),
        vec![("exp_new".to_string(), 1, false, true, false)]
    );
}

#[test]
fn override_on_a_present_experiment_is_exposed_once() {
    let mut ctx = context(vec![experiment(1, "exp_over", "color", json!("red"))]);
    ctx.set_override("exp_over", 1).unwrap();

    for _ in 0..3 {
        assert_eq!(ctx.treatment("exp_over").unwrap(), 1);
        assert_eq!(
            ctx.variable_value("color", "default").unwrap(),
            json!("red")
        );
    }
    assert_eq!(
        exposed(&mut ctx),
        vec![("exp_over".to_string(), 1, false, true, false)]
    );
}

/// `(id, unit)` of each queued exposure.
fn exposure_ids(ctx: &mut Context) -> Vec<(i64, Option<String>)> {
    ctx.get_publish_params()
        .exposures
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.id, e.unit))
        .collect()
}

fn refreshed(experiments: Vec<Value>) -> ContextData {
    serde_json::from_value(json!({"experiments": experiments, "holdouts": []})).unwrap()
}

#[test]
fn refresh_that_changes_an_overridden_experiments_id_rebuilds_the_override() {
    let mut ctx = context(vec![experiment(2, "exp_over", "color", json!("red"))]);
    ctx.set_override("exp_over", 1).unwrap();
    assert_eq!(ctx.peek("exp_over").unwrap(), 1);

    ctx.refresh_with(refreshed(vec![experiment(
        5,
        "exp_over",
        "color",
        json!("red"),
    )]));
    ctx.treatment("exp_over").unwrap();

    assert_eq!(
        exposure_ids(&mut ctx),
        vec![(5, Some("session_id".to_string()))]
    );
}

#[test]
fn refresh_that_changes_an_overridden_experiments_unit_type_rebuilds_the_override() {
    let mut ctx = context(vec![experiment(2, "exp_over", "color", json!("red"))]);
    ctx.set_unit("user_id", "user-1").unwrap();
    ctx.set_override("exp_over", 1).unwrap();
    assert_eq!(ctx.peek("exp_over").unwrap(), 1);

    ctx.refresh_with(refreshed(vec![experiment_with_unit(
        2,
        "exp_over",
        "user_id",
        "color",
        json!("red"),
    )]));
    ctx.treatment("exp_over").unwrap();

    assert_eq!(
        exposure_ids(&mut ctx),
        vec![(2, Some("user_id".to_string()))]
    );
}

#[test]
fn variable_keys_lists_each_candidate_once_in_id_order() {
    let ctx = context(vec![
        experiment(9, "exp_high", "color", json!("high")),
        experiment(3, "exp_low", "color", json!("low")),
    ]);

    assert_eq!(
        ctx.variable_keys().unwrap()["color"],
        vec!["exp_low".to_string(), "exp_high".to_string()]
    );
}

#[test]
fn winner_variant_without_the_key_returns_the_default_and_exposes_only_the_winner() {
    let winner = variant_config(
        experiment(1, "exp_winner", "color", json!("unused")),
        1,
        json!({"size": "large"}),
    );
    let mut ctx = context(vec![
        winner,
        experiment(2, "exp_later", "color", json!("blue")),
    ]);

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_winner")]);
}

#[test]
fn already_exposed_winner_without_the_key_still_does_not_fall_through() {
    let winner = variant_config(
        experiment(1, "exp_winner", "color", json!("unused")),
        1,
        json!({"size": "large"}),
    );
    let mut ctx = context(vec![
        winner,
        experiment(2, "exp_later", "color", json!("blue")),
    ]);
    ctx.treatment("exp_winner").unwrap();

    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_winner")]);
}

#[test]
fn no_candidate_assigned_returns_the_default_and_exposes_nothing() {
    let mut ctx = context(vec![
        strict_us_only(experiment(1, "exp_us_a", "color", json!("red"))),
        experiment_with_unit(2, "exp_by_user", "user_id", "color", json!("green")),
        strict_us_only(experiment(3, "exp_us_b", "color", json!("blue"))),
    ]);

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(ctx.pending(), 0);
    assert_eq!(exposed(&mut ctx), vec![]);
}

#[test]
fn peek_exposes_nothing() {
    let mut ctx = context(vec![
        strict_us_only(experiment(1, "exp_us_only", "color", json!("red"))),
        experiment(2, "exp_everyone", "color", json!("blue")),
    ]);

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("blue")
    );
    assert_eq!(ctx.pending(), 0);
}

#[test]
fn override_on_an_unassigned_lower_id_candidate_wins() {
    let mut ctx = context(vec![
        strict_us_only(experiment(1, "exp_us_only", "color", json!("red"))),
        experiment(2, "exp_everyone", "color", json!("blue")),
    ]);
    ctx.set_override("exp_us_only", 1).unwrap();

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("red")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("red")
    );
    assert_eq!(
        exposed(&mut ctx),
        vec![("exp_us_only".to_string(), 1, false, true, false)]
    );
}

#[test]
fn override_on_a_higher_id_candidate_does_not_beat_an_assigned_lower_id_one() {
    let mut ctx = context(vec![
        experiment(1, "exp_first", "color", json!("red")),
        experiment(2, "exp_second", "color", json!("blue")),
    ]);
    ctx.set_override("exp_second", 0).unwrap();

    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("red")
    );
    assert_eq!(exposed(&mut ctx), vec![assigned("exp_first")]);
}

#[test]
fn override_to_a_variant_without_the_key_returns_the_default() {
    let mut ctx = context(vec![
        experiment(1, "exp_first", "color", json!("red")),
        experiment(2, "exp_second", "color", json!("blue")),
    ]);
    ctx.set_override("exp_first", 5).unwrap();

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        exposed(&mut ctx),
        vec![("exp_first".to_string(), 5, false, true, false)]
    );
}

#[test]
fn rule_assignment_on_an_unassigned_lower_id_candidate_wins() {
    let mut ruled = strict_us_only(experiment(1, "exp_ruled", "color", json!("red")));
    ruled["assignmentRules"] = json!(
        r#"{"rules":[{"name":"UK","type":"assign","conditions":{"and":[{"eq":[{"var":"country"},{"value":"UK"}]}]},"environments":[],"variant":1}]}"#
    );
    let mut ctx = context(vec![
        ruled,
        experiment(2, "exp_everyone", "color", json!("blue")),
    ]);

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("red")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("red")
    );
    assert_eq!(
        exposed(&mut ctx),
        vec![("exp_ruled".to_string(), 1, false, false, true)]
    );
}

#[test]
fn falsy_values_are_returned_and_only_a_missing_key_gives_the_default() {
    for value in [
        json!(null),
        json!(false),
        json!(0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        let mut ctx = context(vec![
            experiment(1, "exp_falsy", "flag", value.clone()),
            experiment(2, "exp_later", "flag", json!("later")),
        ]);

        assert_eq!(ctx.peek_variable_value("flag", "default").unwrap(), value);
        assert_eq!(ctx.variable_value("flag", "default").unwrap(), value);
        assert_eq!(exposed(&mut ctx), vec![assigned("exp_falsy")], "{value}");
    }
}

#[test]
fn held_out_candidate_gives_its_control_value_when_no_candidate_is_assigned() {
    let holdout = json!({
        "id": 11, "name": "holdout_a", "unitType": "session_id", "iteration": 1,
        "seedHi": 13, "seedLo": 111, "split": [0.1, 0.9], "trafficSplit": [0.0, 1.0],
        "fullOnVariant": 0, "variants": [{"config": null}, {"config": null}],
        "holdoutType": "full"
    });
    let mut held_out = experiment(1, "exp_held_out", "color", json!("red"));
    held_out["holdoutIds"] = json!([11]);
    let mut ctx = context_with_holdouts(
        vec![holdout],
        vec![
            held_out,
            strict_us_only(experiment(2, "exp_us_only", "color", json!("blue"))),
        ],
    );

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("control")
    );
    assert_eq!(ctx.pending(), 0);
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("control")
    );
    assert_eq!(
        exposed(&mut ctx),
        vec![("holdout_a".to_string(), 0, true, false, false)]
    );
}
