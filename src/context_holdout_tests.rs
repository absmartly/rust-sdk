//! Holdouts, ported from java-sdk's `ContextHoldoutTest` (same case names in
//! snake_case) and checked against javascript-sdk's `context.ts`.
//!
//! A holdout arrives as an experiment-shaped entry in `ContextData.holdouts`;
//! its variant 0 holds the unit out. A covered experiment lists the holdout
//! ids in `holdoutIds`. A held-out unit gets control values and no exposure of
//! its own for the covered experiment, and every covering holdout queues its
//! own exposure once per context.
//!
//! Java's detached-assignment and exposure-settlement cases guard a window
//! between two threads. `Context` takes `&mut self`, so an assignment can't be
//! held across `set_unit` or a refresh; those cases are ported as the same
//! call sequence through the public API, with the same expected events.

use super::*;
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};

const UNIT_TYPE: &str = "session_id";
const UID: &str = "e791e240fcd3df7d238cfc285f475e8152fcc0ec";
const UID_NOT_HELD_OUT: &str = "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3";

// split [0.5,0.5], seeds 100/200: variant 1 for UID, 0 for UID_NOT_HELD_OUT.
const NORMAL_VARIANT: i32 = 1;

// split [0.1,0.9], seeds 13/111: variant 0 (held out) for UID, 1 for UID_NOT_HELD_OUT.
const HOLDOUT_A: (i64, i64) = (13, 111);
// split [0.1,0.9], seeds 1/222: variant 1 (not held out) for both UIDs.
const HOLDOUT_B: (i64, i64) = (1, 222);

// A 3-arm holdout's split has length 3, which is what selects the arm-1
// rule. Seeds for UID: seedLo 1 -> arm 0, 3 -> arm 1, 0 -> arm 2.
const THREE_ARM_0: (i64, i64) = (0, 1);
const THREE_ARM_1: (i64, i64) = (0, 3);
const THREE_ARM_2: (i64, i64) = (0, 0);

const US_RULE: &str = r#"{"rules":[{"name":"US Internal Users","type":"assign","conditions":{"and":[{"eq":[{"var":"country"},{"value":"US"}]}]},"environments":[],"variant":1}]}"#;

fn experiment(id: i64, name: &str) -> Value {
    experiment_with(id, name, UNIT_TYPE, 0)
}

fn experiment_full_on(id: i64, name: &str, full_on_variant: i64) -> Value {
    experiment_with(id, name, UNIT_TYPE, full_on_variant)
}

fn experiment_with(id: i64, name: &str, unit_type: &str, full_on_variant: i64) -> Value {
    let variants: Vec<Value> = (0..=full_on_variant.max(1))
        .map(|_| json!({"name": "v", "config": null}))
        .collect();
    json!({
        "id": id,
        "name": name,
        "unitType": unit_type,
        "iteration": 1,
        "seedHi": 100,
        "seedLo": 200,
        "split": [0.5, 0.5],
        "trafficSeedHi": 1,
        "trafficSeedLo": 2,
        "trafficSplit": [0.0, 1.0],
        "fullOnVariant": full_on_variant,
        "applications": [{"name": "website"}],
        "variants": variants,
        "audienceStrict": false,
        "audience": null
    })
}

fn holdout(id: i64, name: &str, seeds: (i64, i64)) -> Value {
    holdout_with(id, name, UNIT_TYPE, seeds, "full")
}

fn holdout_with(id: i64, name: &str, unit_type: &str, seeds: (i64, i64), kind: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "unitType": unit_type,
        "iteration": 1,
        "seedHi": seeds.0,
        "seedLo": seeds.1,
        "split": [0.1, 0.9],
        "trafficSplit": [0.0, 1.0],
        "fullOnVariant": 0,
        "applications": [],
        "variants": [{"name": "A", "config": null}, {"name": "B", "config": null}],
        "audienceStrict": false,
        "audience": null,
        "holdoutType": kind
    })
}

fn three_arm_holdout(id: i64, name: &str, seeds: (i64, i64)) -> Value {
    let mut h = holdout_with(id, name, UNIT_TYPE, seeds, "all_full_on");
    h["split"] = json!([0.3, 0.3, 0.4]);
    h["variants"] = json!([{"name": "A"}, {"name": "B"}, {"name": "C"}]);
    h
}

fn covered_by(mut experiment: Value, ids: &[i64]) -> Value {
    experiment["holdoutIds"] = json!(ids);
    experiment
}

fn with_config(mut experiment: Value, variant: usize, config: Value) -> Value {
    experiment["variants"][variant]["config"] = json!(config.to_string());
    experiment
}

fn with_variables(mut experiment: Value, key: &str) -> Value {
    experiment["variants"] = json!([
        {"name": "A", "config": json!({key: "control"}).to_string()},
        {"name": "B", "config": json!({key: "treatment"}).to_string()}
    ]);
    experiment
}

fn data(holdouts: Vec<Value>, experiments: Vec<Value>) -> ContextData {
    serde_json::from_value(json!({"experiments": experiments, "holdouts": holdouts})).unwrap()
}

fn context(uid: &str, data: ContextData) -> Context {
    let mut ctx = Context::new(data);
    ctx.set_unit(UNIT_TYPE, uid).unwrap();
    ctx
}

type Seen = (i64, String, Option<String>, i32, [bool; 7]);

#[allow(clippy::too_many_arguments)]
fn exposure(
    id: i64,
    name: &str,
    unit: &str,
    variant: i32,
    assigned: bool,
    eligible: bool,
    overridden: bool,
    full_on: bool,
    custom: bool,
    audience_mismatch: bool,
    rule_override: bool,
) -> Seen {
    (
        id,
        name.to_string(),
        Some(unit.to_string()),
        variant,
        [
            assigned,
            eligible,
            overridden,
            full_on,
            custom,
            audience_mismatch,
            rule_override,
        ],
    )
}

fn normal(id: i64, name: &str, variant: i32) -> Seen {
    exposure(
        id, name, UNIT_TYPE, variant, true, true, false, false, false, false, false,
    )
}

fn holdout_exposure(id: i64, name: &str, variant: i32) -> Seen {
    holdout_exposure_unit(UNIT_TYPE, id, name, variant)
}

fn holdout_exposure_unit(unit: &str, id: i64, name: &str, variant: i32) -> Seen {
    exposure(
        id, name, unit, variant, true, true, false, false, false, false, false,
    )
}

/// The queued exposures, in order, without their timestamps.
fn queued(ctx: &mut Context) -> Vec<Seen> {
    ctx.get_publish_params()
        .exposures
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            (
                e.id,
                e.name,
                e.unit,
                e.variant,
                [
                    e.assigned,
                    e.eligible,
                    e.overridden,
                    e.full_on,
                    e.custom,
                    e.audience_mismatch,
                    e.rule_override,
                ],
            )
        })
        .collect()
}

fn single_holdout(seeds: (i64, i64), experiments: Vec<Value>) -> ContextData {
    data(vec![holdout(11, "holdout_a", seeds)], experiments)
}

#[test]
fn held_out_unit_gets_control_values_and_emits_no_exposure_for_covered_experiment() {
    let exp = covered_by(experiment(1, "exp_holdout_in"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));

    assert_eq!(ctx.peek("exp_holdout_in").unwrap(), 0);
    ctx.treatment("exp_holdout_in").unwrap();

    assert_eq!(queued(&mut ctx), vec![holdout_exposure(11, "holdout_a", 0)]);
}

#[test]
fn not_held_out_unit_emits_holdout_exposure_variant_one_and_normal_exposure() {
    let exp = covered_by(experiment(1, "exp_holdout_out"), &[11]);
    let mut ctx = context(UID_NOT_HELD_OUT, single_holdout(HOLDOUT_A, vec![exp]));

    assert_eq!(ctx.treatment("exp_holdout_out").unwrap(), 0);

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_holdout_out", 0),
            holdout_exposure(11, "holdout_a", 1)
        ]
    );
}

#[test]
fn experiment_without_holdout_ids_emits_normally_even_for_held_out_unit() {
    let covered = covered_by(experiment(1, "exp_holdout_in"), &[11]);
    let not_covered = experiment(2, "exp_not_covered");
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![covered, not_covered]));

    assert_eq!(ctx.treatment("exp_holdout_in").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_not_covered").unwrap(), NORMAL_VARIANT);

    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_a", 0),
            normal(2, "exp_not_covered", NORMAL_VARIANT)
        ]
    );
}

#[test]
fn union_of_applicable_holdouts_suppresses_experiment_and_both_emit_own_exposure() {
    let exp = covered_by(experiment(1, "exp_multi_holdout"), &[11, 12]);
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_a", HOLDOUT_A),
                holdout(12, "holdout_b", HOLDOUT_B),
            ],
            vec![exp],
        ),
    );

    assert_eq!(ctx.treatment("exp_multi_holdout").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_a", 0),
            holdout_exposure(12, "holdout_b", 1)
        ]
    );
}

// The low-id holdout does not hold the unit out and the higher-id one does,
// so an implementation that only reads the first holdout fails here.
#[test]
fn union_rule_applies_when_the_higher_id_holdout_is_the_only_one_holding_unit_out() {
    let exp = covered_by(experiment(1, "exp_union_high_id_decides"), &[11, 12]);
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_low_id", HOLDOUT_B),
                holdout(12, "holdout_high_id", HOLDOUT_A),
            ],
            vec![exp],
        ),
    );

    assert_eq!(ctx.treatment("exp_union_high_id_decides").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_low_id", 1),
            holdout_exposure(12, "holdout_high_id", 0)
        ]
    );
}

#[test]
fn unit_not_held_out_by_either_holdout_is_not_suppressed() {
    let exp = covered_by(experiment(1, "exp_multi_holdout_miss"), &[11, 12]);
    let mut ctx = context(
        UID_NOT_HELD_OUT,
        data(
            vec![
                holdout(11, "holdout_a", HOLDOUT_A),
                holdout(12, "holdout_b", HOLDOUT_B),
            ],
            vec![exp],
        ),
    );

    assert_eq!(ctx.treatment("exp_multi_holdout_miss").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_multi_holdout_miss", 0),
            holdout_exposure(11, "holdout_a", 1),
            holdout_exposure(12, "holdout_b", 1)
        ]
    );
}

#[test]
fn holdout_suppresses_full_on_experiment_regardless_of_full_on_variant() {
    let exp = covered_by(experiment_full_on(1, "exp_fullon_holdout", 2), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));

    assert_eq!(ctx.treatment("exp_fullon_holdout").unwrap(), 0);
}

fn three_arm_context(seeds: (i64, i64), prefix: &str) -> Context {
    let non_full_on = covered_by(experiment(1, &format!("{prefix}_non_fullon")), &[21]);
    let full_on = covered_by(experiment_full_on(2, &format!("{prefix}_fullon"), 2), &[21]);
    context(
        UID,
        data(
            vec![three_arm_holdout(21, "holdout_three_arm", seeds)],
            vec![non_full_on, full_on],
        ),
    )
}

#[test]
fn three_arm_variant_zero_holds_out_both_full_on_and_non_full_on_experiments() {
    let mut ctx = three_arm_context(THREE_ARM_0, "exp_three_arm_0");

    assert_eq!(ctx.treatment("exp_three_arm_0_non_fullon").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_three_arm_0_fullon").unwrap(), 0);

    assert_eq!(
        queued(&mut ctx),
        vec![holdout_exposure(21, "holdout_three_arm", 0)]
    );
}

#[test]
fn three_arm_variant_one_forces_non_full_on_experiment_to_control_but_assigns_full_on_experiment_normally(
) {
    let mut ctx = three_arm_context(THREE_ARM_1, "exp_three_arm_1");

    assert_eq!(ctx.treatment("exp_three_arm_1_non_fullon").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_three_arm_1_fullon").unwrap(), 2);

    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(21, "holdout_three_arm", 1),
            exposure(
                2,
                "exp_three_arm_1_fullon",
                UNIT_TYPE,
                2,
                true,
                true,
                false,
                true,
                false,
                false,
                false
            )
        ]
    );
}

#[test]
fn three_arm_variant_two_evaluates_both_experiment_kinds_normally() {
    let mut ctx = three_arm_context(THREE_ARM_2, "exp_three_arm_2");

    assert_eq!(
        ctx.treatment("exp_three_arm_2_non_fullon").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(ctx.treatment("exp_three_arm_2_fullon").unwrap(), 2);

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_three_arm_2_non_fullon", NORMAL_VARIANT),
            holdout_exposure(21, "holdout_three_arm", 2),
            exposure(
                2,
                "exp_three_arm_2_fullon",
                UNIT_TYPE,
                2,
                true,
                true,
                false,
                true,
                false,
                false,
                false
            )
        ]
    );
}

#[test]
fn two_arm_holdout_wins_union_even_when_three_arm_would_not_hold_out() {
    let exp = covered_by(experiment(1, "exp_union_two_arm_wins"), &[11, 22]);
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_two_arm", HOLDOUT_A),
                three_arm_holdout(22, "holdout_three_arm", THREE_ARM_2),
            ],
            vec![exp],
        ),
    );

    assert_eq!(ctx.treatment("exp_union_two_arm_wins").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_two_arm", 0),
            holdout_exposure(22, "holdout_three_arm", 2)
        ]
    );
}

#[test]
fn three_arm_holdout_wins_union_even_when_two_arm_would_not_hold_out() {
    let exp = covered_by(experiment(1, "exp_union_three_arm_wins"), &[11, 22]);
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_two_arm", HOLDOUT_B),
                three_arm_holdout(22, "holdout_three_arm", THREE_ARM_1),
            ],
            vec![exp],
        ),
    );

    assert_eq!(ctx.treatment("exp_union_three_arm_wins").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_two_arm", 1),
            holdout_exposure(22, "holdout_three_arm", 1)
        ]
    );
}

// Arm 1 does not short-circuit a full-on experiment to its full-on variant:
// a strict audience mismatch still gives control, exactly as under arm 2.
#[test]
fn three_arm_arm_one_defers_to_normal_path_so_audience_mismatch_still_wins_for_full_on_experiment()
{
    let audience = r#"{"filter":[{"gte":[{"var":"age"},{"value":20}]}]}"#;
    for (seeds, arm) in [(THREE_ARM_1, 1), (THREE_ARM_2, 2)] {
        let mut exp = covered_by(experiment_full_on(1, "exp_audience_fullon", 2), &[21]);
        exp["audienceStrict"] = json!(true);
        exp["audience"] = json!(audience);
        let mut ctx = context(
            UID,
            data(
                vec![three_arm_holdout(21, "holdout_three_arm", seeds)],
                vec![exp],
            ),
        );
        ctx.set_attribute("age", 5).unwrap();

        assert_eq!(ctx.treatment("exp_audience_fullon").unwrap(), 0);
        assert_eq!(
            queued(&mut ctx),
            vec![
                exposure(
                    1,
                    "exp_audience_fullon",
                    UNIT_TYPE,
                    0,
                    false,
                    true,
                    false,
                    false,
                    false,
                    true,
                    false
                ),
                holdout_exposure(21, "holdout_three_arm", arm)
            ]
        );
    }
}

#[test]
fn unconfigured_unit_type_gets_control_values_and_emits_no_exposure_at_all() {
    let exp = covered_by(experiment_with(1, "exp_user_holdout", "user_id", 0), &[11]);
    let h = holdout_with(11, "holdout_user", "user_id", HOLDOUT_A, "full");
    let mut ctx = context(UID, data(vec![h], vec![exp]));

    assert_eq!(ctx.peek("exp_user_holdout").unwrap(), 0);
    ctx.treatment("exp_user_holdout").unwrap();
    ctx.publish().unwrap();

    assert_eq!(ctx.pending(), 0);
}

#[test]
fn assigns_normally_when_holdouts_key_is_absent() {
    let data: ContextData =
        serde_json::from_value(json!({"experiments": [experiment(1, "exp_no_holdouts_key")]}))
            .unwrap();
    let mut ctx = context(UID, data);

    assert_eq!(ctx.peek("exp_no_holdouts_key").unwrap(), NORMAL_VARIANT);
}

#[test]
fn holdout_id_absent_from_holdouts_array_is_ignored_and_experiment_assigns_normally() {
    let exp = covered_by(experiment(1, "exp_dangling_holdout_id"), &[999]);
    let mut ctx = context(UID, data(vec![], vec![exp]));

    assert_eq!(ctx.peek("exp_dangling_holdout_id").unwrap(), NORMAL_VARIANT);
}

#[test]
fn custom_assignment_cannot_override_held_out_variant() {
    let exp = covered_by(experiment(1, "exp_holdout_custom"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    ctx.set_custom_assignment("exp_holdout_custom", 3).unwrap();

    assert_eq!(ctx.peek("exp_holdout_custom").unwrap(), 0);
}

#[test]
fn reuses_cached_held_out_assignment_with_custom_assignment() {
    let exp = covered_by(experiment(1, "exp_holdout_custom_cache"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    ctx.set_custom_assignment("exp_holdout_custom_cache", 3)
        .unwrap();

    assert_eq!(ctx.treatment("exp_holdout_custom_cache").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_holdout_custom_cache").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
    // The custom assignment never applies while held out, so it is not a
    // reason to rebuild (and forget the exposure of) the cached assignment.
    assert_eq!(ctx.peek("exp_holdout_custom_cache").unwrap(), 0);
    assert!(
        ctx.assignments["exp_holdout_custom_cache"]
            .assignment
            .exposed
    );
}

#[test]
fn override_takes_precedence_over_holdout() {
    let exp = covered_by(experiment(1, "exp_holdout_override"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    ctx.set_override("exp_holdout_override", 3).unwrap();

    assert_eq!(ctx.peek("exp_holdout_override").unwrap(), 3);
}

#[test]
fn get_treatment_on_overridden_experiment_triggers_applicable_holdout_exposure_both_arms() {
    for (uid, arm) in [(UID, 0), (UID_NOT_HELD_OUT, 1)] {
        let exp = covered_by(experiment(1, "exp_holdout_override"), &[11]);
        let mut ctx = context(uid, single_holdout(HOLDOUT_A, vec![exp]));
        ctx.set_override("exp_holdout_override", 3).unwrap();

        assert_eq!(ctx.treatment("exp_holdout_override").unwrap(), 3);
        assert_eq!(
            queued(&mut ctx),
            vec![
                exposure(
                    1,
                    "exp_holdout_override",
                    UNIT_TYPE,
                    3,
                    false,
                    true,
                    true,
                    false,
                    false,
                    false,
                    false
                ),
                holdout_exposure(11, "holdout_a", arm)
            ]
        );
    }
}

#[test]
fn holdout_takes_precedence_over_audience_mismatch() {
    let mut exp = covered_by(experiment(1, "exp_holdout_audience"), &[11]);
    exp["audienceStrict"] = json!(true);
    exp["audience"] = json!(r#"{"filter":[{"gte":[{"var":"age"},{"value":20}]}]}"#);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    ctx.set_attribute("age", 5).unwrap();

    assert_eq!(ctx.peek("exp_holdout_audience").unwrap(), 0);
    ctx.treatment("exp_holdout_audience").unwrap();
    assert_eq!(queued(&mut ctx), vec![holdout_exposure(11, "holdout_a", 0)]);
}

#[test]
fn holdout_exposure_emitted_once_per_context_not_per_suppressed_experiment() {
    let a = covered_by(experiment(1, "exp_shared_a"), &[11]);
    let b = covered_by(experiment(2, "exp_shared_b"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![a, b]));

    assert_eq!(ctx.treatment("exp_shared_a").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_shared_b").unwrap(), 0);

    assert_eq!(queued(&mut ctx), vec![holdout_exposure(11, "holdout_a", 0)]);
}

#[test]
fn holdout_exposure_fires_once_even_when_triggering_experiment_is_not_suppressed() {
    let a = covered_by(experiment(1, "exp_shared_a"), &[11]);
    let b = covered_by(experiment(2, "exp_shared_b"), &[11]);
    let mut ctx = context(UID_NOT_HELD_OUT, single_holdout(HOLDOUT_A, vec![a, b]));

    ctx.treatment("exp_shared_a").unwrap();
    ctx.treatment("exp_shared_b").unwrap();

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_shared_a", 0),
            holdout_exposure(11, "holdout_a", 1),
            normal(2, "exp_shared_b", 0)
        ]
    );
}

#[test]
fn reuses_cached_suppressed_assignment_across_repeated_calls() {
    let exp = covered_by(experiment(1, "exp_holdout_cache"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));

    assert_eq!(ctx.treatment("exp_holdout_cache").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_holdout_cache").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
}

// A same-iteration seed edit must not move an exposed unit to the other arm.
#[test]
fn refresh_with_seed_change_same_iteration_keeps_unit_in_original_arm_and_does_not_re_expose() {
    let exp = || covered_by(experiment(1, "exp_holdout_seed_thrash"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_seed_thrash").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    ctx.refresh_with(single_holdout(HOLDOUT_B, vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_seed_thrash").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
}

#[test]
fn warmed_reads_after_refresh_compare_holdout_coverage_only_once() {
    let exp = || covered_by(experiment(1, "exp_coverage_once"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp()]));
    ctx.treatment("exp_coverage_once").unwrap();
    ctx.refresh_with(single_holdout(HOLDOUT_A, vec![exp()]));

    let comparisons = || super::COVERAGE_COMPARISONS.with(|n| n.get());
    let before = comparisons();
    for _ in 0..100 {
        ctx.peek("exp_coverage_once").unwrap();
        ctx.treatment("exp_coverage_once").unwrap();
    }
    assert_eq!(comparisons() - before, 1);
}

#[test]
fn refresh_with_cosmetic_holdout_edit_does_not_re_expose_held_out_unit() {
    let exp = || covered_by(experiment(1, "exp_holdout_cosmetic"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_cosmetic").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    let mut cosmetic = holdout(11, "holdout_a_renamed", HOLDOUT_A);
    cosmetic["applications"] = json!([{"name": "website"}]);
    cosmetic["variants"] = json!([{"name": "Control"}, {"name": "HeldOut"}]);
    ctx.refresh_with(data(vec![cosmetic], vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_cosmetic").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
}

#[test]
fn refresh_with_cosmetic_holdout_edit_does_not_duplicate_covered_experiment_exposure() {
    let exp = || covered_by(experiment(1, "exp_holdout_cosmetic_covered"), &[11]);
    let mut ctx = context(UID_NOT_HELD_OUT, single_holdout(HOLDOUT_A, vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_cosmetic_covered").unwrap(), 0);
    assert_eq!(ctx.pending(), 2);

    ctx.refresh_with(data(
        vec![holdout(11, "holdout_a_renamed", HOLDOUT_A)],
        vec![exp()],
    ));

    assert_eq!(ctx.treatment("exp_holdout_cosmetic_covered").unwrap(), 0);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn refresh_with_iteration_bump_reassigns_to_exactly_one_new_arm() {
    let exp = || covered_by(experiment(1, "exp_holdout_iteration"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_B, vec![exp()]));

    assert_eq!(
        ctx.treatment("exp_holdout_iteration").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(ctx.pending(), 2);

    let mut new_epoch = holdout(11, "holdout_a", HOLDOUT_A);
    new_epoch["iteration"] = json!(2);
    ctx.refresh_with(data(vec![new_epoch], vec![exp()]));

    assert_eq!(ctx.treatment("exp_holdout_iteration").unwrap(), 0);
    assert_eq!(ctx.pending(), 3);
    assert_eq!(ctx.treatment("exp_holdout_iteration").unwrap(), 0);
    assert_eq!(ctx.pending(), 3);
}

// An experiment first evaluated after a same-iteration seed edit reads the
// pinned arm, not one drawn from the new seed.
#[test]
fn refreshed_seed_does_not_desync_suppression_from_pinned_holdout_arm_for_newly_evaluated_experiment(
) {
    let a = || covered_by(experiment(1, "exp_holdout_desync_a"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![a()]));

    assert_eq!(ctx.treatment("exp_holdout_desync_a").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    let b = covered_by(experiment(2, "exp_holdout_desync_b"), &[11]);
    ctx.refresh_with(single_holdout(HOLDOUT_B, vec![a(), b]));

    assert_eq!(ctx.treatment("exp_holdout_desync_b").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_holdout_desync_a").unwrap(), 0);
    assert_eq!(queued(&mut ctx), vec![holdout_exposure(11, "holdout_a", 0)]);
}

// Arm 1 means "defer" under 2 arms and "full-on only" under 3. A pinned arm
// keeps the meaning of the arm count it was drawn with.
#[test]
fn refresh_from_two_arm_to_three_arm_same_iteration_does_not_reinterpret_deferring_arm_as_full_on_only(
) {
    let a = || covered_by(experiment(1, "exp_arity_2to3_a"), &[31]);
    let mut ctx = context(
        UID,
        data(vec![holdout(31, "holdout_arity", HOLDOUT_B)], vec![a()]),
    );

    assert_eq!(ctx.treatment("exp_arity_2to3_a").unwrap(), NORMAL_VARIANT);

    let b = covered_by(experiment(2, "exp_arity_2to3_b"), &[31]);
    ctx.refresh_with(data(
        vec![three_arm_holdout(31, "holdout_arity", THREE_ARM_1)],
        vec![a(), b],
    ));

    assert_eq!(ctx.treatment("exp_arity_2to3_b").unwrap(), NORMAL_VARIANT);
    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_arity_2to3_a", NORMAL_VARIANT),
            holdout_exposure(31, "holdout_arity", 1),
            normal(2, "exp_arity_2to3_b", NORMAL_VARIANT)
        ]
    );
}

#[test]
fn refresh_from_three_arm_to_two_arm_same_iteration_keeps_full_on_only_arm_suppressing_non_full_on_experiment(
) {
    let a = || covered_by(experiment(1, "exp_arity_3to2_a"), &[41]);
    let mut ctx = context(
        UID,
        data(
            vec![three_arm_holdout(41, "holdout_arity_rev", THREE_ARM_1)],
            vec![a()],
        ),
    );

    assert_eq!(ctx.treatment("exp_arity_3to2_a").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    let b = covered_by(experiment(2, "exp_arity_3to2_b"), &[41]);
    ctx.refresh_with(data(
        vec![holdout(41, "holdout_arity_rev", HOLDOUT_B)],
        vec![a(), b],
    ));

    assert_eq!(ctx.treatment("exp_arity_3to2_b").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![holdout_exposure(41, "holdout_arity_rev", 1)]
    );
}

#[test]
fn experiments_evaluated_before_and_after_same_iteration_arity_change_agree_on_suppression() {
    let pre = || covered_by(experiment(1, "exp_arity_consistency_pre"), &[71]);
    let mut ctx = context(
        UID,
        data(
            vec![holdout(71, "holdout_arity_consistency", HOLDOUT_B)],
            vec![pre()],
        ),
    );

    assert_eq!(
        ctx.treatment("exp_arity_consistency_pre").unwrap(),
        NORMAL_VARIANT
    );

    let post = covered_by(experiment(2, "exp_arity_consistency_post"), &[71]);
    ctx.refresh_with(data(
        vec![three_arm_holdout(
            71,
            "holdout_arity_consistency",
            THREE_ARM_1,
        )],
        vec![pre(), post],
    ));

    assert_eq!(
        ctx.treatment("exp_arity_consistency_post").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        ctx.treatment("exp_arity_consistency_pre").unwrap(),
        NORMAL_VARIANT
    );
}

// Java refreshes from inside the event logger between the decision and the
// holdout trigger. Here the logger only gets `&Context`, so it cannot; the
// test installs the new data without the refresh's eviction pass, which is
// the state that race leaves, and checks the trigger fires the pinned arm.
#[test]
fn trigger_fires_holdout_exposure_from_decision_epoch_despite_refresh_racing_between_decision_and_trigger(
) {
    let exp = || covered_by(experiment(1, "exp_epoch_race"), &[11]);
    let mut ctx = context(
        UID,
        data(vec![holdout(11, "holdout_h", HOLDOUT_B)], vec![exp()]),
    );

    assert_eq!(ctx.peek("exp_epoch_race").unwrap(), NORMAL_VARIANT);
    let mut iteration2 = holdout(11, "holdout_h", HOLDOUT_A);
    iteration2["iteration"] = json!(2);
    ctx.init(data(vec![iteration2], vec![exp()]));
    ctx.trigger_exposures("exp_epoch_race");

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_epoch_race", NORMAL_VARIANT),
            holdout_exposure(11, "holdout_h", 1)
        ]
    );
}

fn late_unit_context(name: &str, seeds: (i64, i64), full_on: i64) -> Context {
    let exp = covered_by(experiment_with(1, name, "user_id", full_on), &[11]);
    let h = holdout_with(11, "holdout_user_id", "user_id", seeds, "full");
    Context::new(data(vec![h], vec![exp]))
}

fn full_on_exposure(unit: &str, id: i64, name: &str, variant: i32) -> Seen {
    exposure(
        id, name, unit, variant, true, true, false, true, false, false, false,
    )
}

#[test]
fn set_unit_invalidates_null_holdout_snapshot_so_the_holdout_exposure_is_not_lost_forever() {
    let mut ctx = late_unit_context("exp_null_snapshot_fullon", HOLDOUT_B, 2);

    assert_eq!(ctx.peek("exp_null_snapshot_fullon").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_null_snapshot_fullon").unwrap(), 2);
    assert_eq!(
        queued(&mut ctx),
        vec![
            full_on_exposure("user_id", 1, "exp_null_snapshot_fullon", 2),
            holdout_exposure_unit("user_id", 11, "holdout_user_id", 1)
        ]
    );
}

#[test]
fn set_unit_does_not_evict_already_exposed_assignment_so_no_duplicate_exposure() {
    let mut ctx = late_unit_context("exp_exposed_before_unit", HOLDOUT_B, 2);

    assert_eq!(ctx.treatment("exp_exposed_before_unit").unwrap(), 2);
    assert_eq!(ctx.pending(), 1);
    let before = ctx.assignments["exp_exposed_before_unit"].holdouts[0]
        .data
        .clone();

    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_exposed_before_unit").unwrap(), 2);
    assert_eq!(ctx.pending(), 1);
    let kept = &ctx.assignments["exp_exposed_before_unit"].holdouts[0];
    assert!(Arc::ptr_eq(&before, &kept.data) && kept.assignment.is_none());
}

#[test]
fn set_unit_for_unrelated_unit_type_does_not_evict_exposed_assignment() {
    let exp = covered_by(experiment(1, "exp_unrelated_unit"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_B, vec![exp]));

    assert_eq!(ctx.treatment("exp_unrelated_unit").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 2);

    ctx.set_unit("other_unit_type", "some-other-uid").unwrap();

    assert_eq!(ctx.treatment("exp_unrelated_unit").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn set_unit_evicts_only_the_affected_entry_and_leaves_the_other_untouched() {
    let late = covered_by(experiment_with(1, "exp_late_unit", "user_id", 2), &[11]);
    let late_holdout = holdout_with(11, "holdout_late", "user_id", HOLDOUT_B, "full");
    let present = covered_by(experiment(2, "exp_present_unit"), &[12]);
    let present_holdout = holdout(12, "holdout_present", HOLDOUT_B);
    let mut ctx = context(
        UID,
        data(vec![late_holdout, present_holdout], vec![late, present]),
    );

    assert_eq!(ctx.peek("exp_late_unit").unwrap(), 2);
    assert_eq!(ctx.treatment("exp_present_unit").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 2);

    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_late_unit").unwrap(), 2);
    assert_eq!(ctx.pending(), 4);
    assert_eq!(ctx.treatment("exp_present_unit").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 4);
}

// The holdout is drawn with the covered experiment's unit type, so eviction
// keys on that type, not on the holdout's declared one.
#[test]
fn set_unit_evicts_using_the_covered_experiments_unit_type_not_the_holdouts_declared_unit_type() {
    let exp = covered_by(
        experiment_with(1, "exp_mismatched_unit_type", "A", 2),
        &[11],
    );
    let h = holdout_with(11, "holdout_declares_b", "B", HOLDOUT_B, "full");
    let mut ctx = Context::new(data(vec![h], vec![exp]));

    assert_eq!(ctx.peek("exp_mismatched_unit_type").unwrap(), 2);
    ctx.set_unit("A", UID).unwrap();

    assert_eq!(ctx.treatment("exp_mismatched_unit_type").unwrap(), 2);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn set_unit_recomputes_suppression_coherently_when_late_resolved_holdout_suppresses() {
    let mut ctx = late_unit_context("exp_null_snapshot_suppresses", HOLDOUT_A, 2);

    assert_eq!(ctx.peek("exp_null_snapshot_suppresses").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_null_snapshot_suppresses").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![holdout_exposure_unit("user_id", 11, "holdout_user_id", 0)]
    );
}

#[test]
fn detached_treatment_assignment_cannot_publish_beside_suppressing_replacement() {
    let mut ctx = late_unit_context("exp_detached_suppressed", HOLDOUT_A, 2);

    assert_eq!(ctx.peek("exp_detached_suppressed").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();
    assert_eq!(ctx.treatment("exp_detached_suppressed").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_detached_suppressed").unwrap(), 0);

    assert_eq!(
        queued(&mut ctx),
        vec![holdout_exposure_unit("user_id", 11, "holdout_user_id", 0)]
    );
}

#[test]
fn detached_treatment_assignment_retries_current_entry_without_losing_exposure() {
    let mut ctx = late_unit_context("exp_detached_retry", HOLDOUT_B, 2);

    assert_eq!(ctx.peek("exp_detached_retry").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();
    assert_eq!(ctx.treatment("exp_detached_retry").unwrap(), 2);
    assert_eq!(ctx.pending(), 2);
    assert_eq!(ctx.treatment("exp_detached_retry").unwrap(), 2);

    assert_eq!(
        queued(&mut ctx),
        vec![
            full_on_exposure("user_id", 1, "exp_detached_retry", 2),
            holdout_exposure_unit("user_id", 11, "holdout_user_id", 1)
        ]
    );
}

#[test]
fn detached_treatment_returns_the_replacement_variant() {
    let mut ctx = late_unit_context("exp_detached_variant", HOLDOUT_A, 2);

    assert_eq!(ctx.peek("exp_detached_variant").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_detached_variant").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
}

// Ported from Java's treatmentExposureSettlesAfterTwoConsecutiveRetirements,
// whose retry loop has no Rust equivalent: a repeated treatment queues one
// exposure.
#[test]
fn repeated_treatment_queues_one_exposure() {
    let mut ctx = context(
        UID,
        data(
            vec![],
            vec![
                experiment_full_on(1, "exp_first", 2),
                experiment_full_on(2, "exp_second", 1),
                experiment_full_on(3, "exp_settled", 1),
            ],
        ),
    );

    assert_eq!(ctx.treatment("exp_settled").unwrap(), 1);
    assert_eq!(ctx.treatment("exp_settled").unwrap(), 1);
    assert_eq!(ctx.pending(), 1);
}

// Ported from Java's exposureSettlementStopsAfterTheBoundedNumberOfRetirements:
// a peek queues nothing.
#[test]
fn peek_queues_no_exposure() {
    let mut ctx = context(
        UID,
        data(vec![], vec![experiment_full_on(1, "exp_bounded_retry", 1)]),
    );

    assert_eq!(ctx.peek("exp_bounded_retry").unwrap(), 1);
    assert_eq!(ctx.pending(), 0);
}

#[test]
fn detached_variable_assignment_reresolves_the_key_to_a_new_winning_experiment() {
    let original = || {
        with_config(
            covered_by(
                experiment_with(2, "exp_detached_variable_old", "user_id", 1),
                &[11],
            ),
            1,
            json!({"detached_var": "old"}),
        )
    };
    let h = || {
        holdout_with(
            11,
            "holdout_detached_variable",
            "user_id",
            HOLDOUT_B,
            "full",
        )
    };
    let mut ctx = Context::new(data(vec![h()], vec![original()]));

    assert_eq!(
        ctx.peek_variable_value("detached_var", "default").unwrap(),
        json!("old")
    );
    ctx.set_unit("user_id", UID).unwrap();

    let winner = with_config(
        covered_by(
            experiment_with(1, "exp_detached_variable_winner", "user_id", 1),
            &[11],
        ),
        1,
        json!({"detached_var": "new winner"}),
    );
    ctx.refresh_with(data(vec![h()], vec![winner, original()]));

    assert_eq!(
        ctx.variable_value("detached_var", "default").unwrap(),
        json!("new winner")
    );
    assert_eq!(
        ctx.variable_value("detached_var", "default").unwrap(),
        json!("new winner")
    );
    assert_eq!(
        queued(&mut ctx),
        vec![
            full_on_exposure("user_id", 1, "exp_detached_variable_winner", 1),
            holdout_exposure_unit("user_id", 11, "holdout_detached_variable", 1)
        ]
    );
}

#[test]
fn trigger_applicable_holdout_exposures_never_live_resolves_a_pinned_null_entry_for_an_already_exposed_assignment(
) {
    let exp = with_config(
        covered_by(
            experiment_with(1, "exp_a_mutant_pinned_null", "user_id", 1),
            &[11],
        ),
        1,
        json!({"my_var": "value_from_variant"}),
    );
    let h = holdout_with(11, "holdout_pinned_null", "user_id", HOLDOUT_A, "full");
    let mut ctx = Context::new(data(vec![h], vec![exp]));

    assert_eq!(ctx.treatment("exp_a_mutant_pinned_null").unwrap(), 1);
    assert_eq!(ctx.pending(), 1);

    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(
        ctx.variable_value("my_var", "default").unwrap(),
        json!("value_from_variant")
    );
    ctx.trigger_exposures("exp_a_mutant_pinned_null");
    assert_eq!(ctx.pending(), 2);
    assert_eq!(
        queued(&mut ctx)
            .iter()
            .filter(|e| e.0 == 11)
            .collect::<Vec<_>>(),
        Vec::<&Seen>::new()
    );
}

#[test]
fn set_unit_evicts_assignment_with_multiple_holdouts_declaring_different_unit_types() {
    let exp = covered_by(
        experiment_with(1, "exp_multi_holdout_unit_types", "user_id", 2),
        &[11, 12, 13],
    );
    let holdouts = vec![
        holdout_with(11, "holdout_11", "type_x", HOLDOUT_B, "full"),
        holdout_with(12, "holdout_12", "type_y", HOLDOUT_B, "full"),
        holdout_with(13, "holdout_13", "user_id", HOLDOUT_B, "full"),
    ];
    let mut ctx = Context::new(data(holdouts, vec![exp]));

    assert_eq!(ctx.peek("exp_multi_holdout_unit_types").unwrap(), 2);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_multi_holdout_unit_types").unwrap(), 2);
    assert_eq!(ctx.pending(), 4);
}

#[test]
fn one_holdout_covering_two_unit_types_keeps_independent_exposure_state_per_unit_type() {
    let a1 = covered_by(experiment_with(1, "exp_unit_a1", "user_id", 0), &[11]);
    let b = covered_by(experiment_with(2, "exp_unit_b", "session_id", 0), &[11]);
    let a2 = covered_by(experiment_with(3, "exp_unit_a2", "user_id", 0), &[11]);
    let h = holdout_with(11, "holdout_shared", "user_id", HOLDOUT_B, "full");
    let mut ctx = context(UID, data(vec![h], vec![a1, b, a2]));
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_unit_a1").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.treatment("exp_unit_b").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.treatment("exp_unit_a2").unwrap(), NORMAL_VARIANT);

    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 5);
    let holdout_units: Vec<_> = exposures
        .iter()
        .filter(|e| e.0 == 11)
        .map(|e| e.2.clone().unwrap())
        .collect();
    assert_eq!(holdout_units, vec!["user_id", "session_id"]);
}

#[test]
fn set_unit_with_no_cached_assignment_does_not_throw_and_subsequent_assignment_is_normal() {
    let exp = covered_by(experiment(1, "exp_no_prior_assignment"), &[11]);
    let mut ctx = Context::new(single_holdout(HOLDOUT_B, vec![exp]));

    ctx.set_unit(UNIT_TYPE, UID).unwrap();

    assert_eq!(
        ctx.treatment("exp_no_prior_assignment").unwrap(),
        NORMAL_VARIANT
    );
}

#[test]
fn set_unit_called_twice_with_same_uid_is_a_no_op_and_does_not_throw() {
    let exp = covered_by(experiment(1, "exp_set_unit_twice"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_B, vec![exp]));

    assert_eq!(ctx.treatment("exp_set_unit_twice").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 2);

    ctx.set_unit(UNIT_TYPE, UID).unwrap();

    assert_eq!(ctx.treatment("exp_set_unit_twice").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 2);
}

// A redundant set_unit must not evict an assignment whose holdouts all
// resolved; a same-iteration split edit (ignored by the cache) shows it.
#[test]
fn set_unit_redundant_call_does_not_evict_fully_resolved_unexposed_holdout_snapshot() {
    let exp = covered_by(experiment(1, "exp_redundant_set_unit"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_B, vec![exp.clone()]));

    assert_eq!(ctx.peek("exp_redundant_set_unit").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 0);

    let mut refreshed = exp;
    refreshed["split"] = json!([1.0, 0.0]);
    ctx.refresh_with(single_holdout(HOLDOUT_B, vec![refreshed]));
    ctx.set_unit(UNIT_TYPE, UID).unwrap();

    assert_eq!(
        ctx.treatment("exp_redundant_set_unit").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_redundant_set_unit", NORMAL_VARIANT),
            holdout_exposure(11, "holdout_a", 1)
        ]
    );
}

#[test]
fn peek_variable_value_then_set_unit_then_get_variable_value_recomputes_under_late_suppressing_holdout(
) {
    let exp = with_config(
        covered_by(
            experiment_with(1, "exp_var_late_unit_suppress", "user_id", 1),
            &[11],
        ),
        1,
        json!({"my_var": "value_from_variant"}),
    );
    let h = holdout_with(11, "holdout_var_late", "user_id", HOLDOUT_A, "full");
    let mut ctx = Context::new(data(vec![h], vec![exp]));

    assert_eq!(
        ctx.peek_variable_value("my_var", "default").unwrap(),
        json!("value_from_variant")
    );
    assert_eq!(ctx.pending(), 0);

    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(
        ctx.variable_value("my_var", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        queued(&mut ctx),
        vec![holdout_exposure_unit("user_id", 11, "holdout_var_late", 0)]
    );
}

#[test]
fn set_unit_does_not_evict_override_assignment_and_holdout_resolves_live_at_exposure_time() {
    let exp = covered_by(
        experiment_with(1, "exp_override_late_unit", "user_id", 0),
        &[11],
    );
    let h = holdout_with(11, "holdout_override_late", "user_id", HOLDOUT_B, "full");
    let mut ctx = Context::new(data(vec![h], vec![exp]));
    ctx.set_override("exp_override_late_unit", 3).unwrap();

    assert_eq!(ctx.peek("exp_override_late_unit").unwrap(), 3);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_override_late_unit").unwrap(), 3);
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(
                1,
                "exp_override_late_unit",
                "user_id",
                3,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
            holdout_exposure_unit("user_id", 11, "holdout_override_late", 1)
        ]
    );
}

#[test]
fn set_unit_evicts_null_snapshot_for_custom_assignment_and_custom_applies_after_recompute() {
    let exp = covered_by(
        experiment_with(1, "exp_custom_late_unit", "user_id", 0),
        &[11],
    );
    let h = holdout_with(11, "holdout_custom_late", "user_id", HOLDOUT_B, "full");
    let mut ctx = Context::new(data(vec![h], vec![exp]));
    ctx.set_custom_assignment("exp_custom_late_unit", 3)
        .unwrap();

    assert_eq!(ctx.peek("exp_custom_late_unit").unwrap(), 0);
    ctx.set_unit("user_id", UID).unwrap();

    assert_eq!(ctx.treatment("exp_custom_late_unit").unwrap(), 3);
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(
                1,
                "exp_custom_late_unit",
                "user_id",
                3,
                true,
                true,
                false,
                false,
                true,
                false,
                false
            ),
            holdout_exposure_unit("user_id", 11, "holdout_custom_late", 1)
        ]
    );
}

// Cross-SDK verdict vectors, checked against the collector's verdicts for
// the same fixtures (java-sdk ContextHoldoutTest).
const VERDICT_UNIT_TYPE: &str = "verdict_unit_type";

fn verdict_context(uid: &str, holdouts: Vec<Value>) -> Context {
    let ids: Vec<i64> = holdouts.iter().map(|h| h["id"].as_i64().unwrap()).collect();
    let exp = covered_by(
        experiment_with(9, "verdict_vectors_experiment", VERDICT_UNIT_TYPE, 0),
        &ids,
    );
    let mut ctx = Context::new(data(holdouts, vec![exp]));
    ctx.set_unit(VERDICT_UNIT_TYPE, uid).unwrap();
    ctx
}

fn verdict_holdout(id: i64, seed_hi: i64, seed_lo: i64, split: [f64; 2]) -> Value {
    let mut h = holdout_with(
        id,
        &format!("holdout_{id}"),
        VERDICT_UNIT_TYPE,
        (seed_hi, seed_lo),
        "full",
    );
    h["split"] = json!(split);
    h
}

fn assert_held_out_by(mut ctx: Context, id: i64) {
    assert_eq!(ctx.treatment("verdict_vectors_experiment").unwrap(), 0);
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 1);
    assert_eq!(
        (exposures[0].0, exposures[0].1.as_str(), exposures[0].3),
        (id, format!("holdout_{id}").as_str(), 0)
    );
}

fn assert_not_held_out(mut ctx: Context, id: i64) {
    ctx.treatment("verdict_vectors_experiment").unwrap();
    let name = format!("holdout_{id}");
    assert!(queued(&mut ctx)
        .iter()
        .any(|e| e.0 == id && e.1 == name && e.3 == 1));
}

#[test]
fn unicode_unit_ids() {
    assert_held_out_by(
        verdict_context("façade", vec![verdict_holdout(107, 0, 42, [0.1, 0.9])]),
        107,
    );
    assert_not_held_out(
        verdict_context(
            "unicode_emoji_\u{1F680}_0",
            vec![verdict_holdout(107, 0, 42, [0.1, 0.9])],
        ),
        107,
    );
}

#[test]
fn signed_seed_halves() {
    // 0x8000000080000000 as a signed int64: the sign bit is set in both halves.
    let seed: i64 = -9_223_372_034_707_292_160;
    let (seed_hi, seed_lo) = ((seed >> 32) as i32 as i64, seed as i32 as i64);
    assert_eq!((seed_hi, seed_lo), (i32::MIN as i64, i32::MIN as i64));

    assert_held_out_by(
        verdict_context(
            "signed_unit_4",
            vec![verdict_holdout(106, seed_hi, seed_lo, [0.1, 0.9])],
        ),
        106,
    );
    assert_not_held_out(
        verdict_context(
            "signed_unit_b_1",
            vec![verdict_holdout(106, seed_hi, seed_lo, [0.1, 0.9])],
        ),
        106,
    );
}

#[test]
fn out_of_range_holdout_seeds_wrap_to_32_bits_as_in_js() {
    // JS reads seeds with `>>> 0`: 2^32 + 1 is 1, and -(2^31) - 1 is 2^31 - 1.
    let decoded = |seed_hi: i64, seed_lo: i64| -> (u32, u32) {
        let data = data(
            vec![holdout(11, "h", (seed_hi, seed_lo))],
            vec![experiment(1, "e")],
        );
        (data.holdouts[0].seed_hi, data.holdouts[0].seed_lo)
    };
    assert_eq!(decoded(4_294_967_297, -2_147_483_649), (1, 2_147_483_647));
    assert_eq!(decoded(-1, 4_294_967_295), (u32::MAX, u32::MAX));
}

#[test]
fn percentage_boundary_at_one_percent() {
    assert_held_out_by(
        verdict_context(
            "pct1_unit_188",
            vec![verdict_holdout(105, 0, 999_999, [0.01, 0.99])],
        ),
        105,
    );
    assert_not_held_out(
        verdict_context(
            "pct1_unit_0",
            vec![verdict_holdout(105, 0, 999_999, [0.01, 0.99])],
        ),
        105,
    );
}

#[test]
fn assignment_probability_boundary_around_ten_percent() {
    assert_held_out_by(
        verdict_context(
            "boundary_unit_175653",
            vec![verdict_holdout(107, 0, 42, [0.1, 0.9])],
        ),
        107,
    );
    assert_not_held_out(
        verdict_context(
            "boundary_unit_75792",
            vec![verdict_holdout(107, 0, 42, [0.1, 0.9])],
        ),
        107,
    );
}

#[test]
fn unit_held_out_by_two_holdouts_simultaneously() {
    let mut ctx = verdict_context(
        "dual_holdout_unit_5",
        vec![
            verdict_holdout(107, 0, 42, [0.1, 0.9]),
            verdict_holdout(108, 0, 55, [0.1, 0.9]),
        ],
    );

    assert_eq!(ctx.treatment("verdict_vectors_experiment").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure_unit(VERDICT_UNIT_TYPE, 107, "holdout_107", 0),
            holdout_exposure_unit(VERDICT_UNIT_TYPE, 108, "holdout_108", 0)
        ]
    );
}

#[test]
fn variable_lookup_fires_holdout_exposure_for_both_held_out_and_non_held_out_units() {
    for (uid, pending) in [(UID, 1), (UID_NOT_HELD_OUT, 2)] {
        let exp = with_config(
            covered_by(experiment(1, "exp_var"), &[11]),
            1,
            json!({"var_a": "value_a"}),
        );
        let mut ctx = context(uid, single_holdout(HOLDOUT_A, vec![exp]));

        assert_eq!(
            ctx.variable_value("var_a", "default").unwrap(),
            json!("default")
        );
        assert_eq!(ctx.pending(), pending);
    }
}

#[test]
fn variable_key_resolution_skips_suppressed_assignment_in_favor_of_assigned_one() {
    let suppressed = with_config(
        covered_by(experiment(1, "exp_var_key_suppressed"), &[11]),
        1,
        json!({"shared": "from_suppressed"}),
    );
    let assigned = with_config(
        experiment(2, "exp_var_key_assigned"),
        1,
        json!({"shared": "from_assigned"}),
    );
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![suppressed, assigned]));

    assert_eq!(
        ctx.variable_value("shared", "default").unwrap(),
        json!("from_assigned")
    );
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_a", 0),
            normal(2, "exp_var_key_assigned", NORMAL_VARIANT)
        ]
    );
}

#[test]
fn peek_variable_value_never_triggers_holdout_exposure_for_evaluated_candidates() {
    let suppressed = with_config(
        covered_by(experiment(1, "exp_var_key_peek_suppressed"), &[11]),
        1,
        json!({"shared_peek": "from_suppressed"}),
    );
    let assigned = with_config(
        experiment(2, "exp_var_key_peek_assigned"),
        1,
        json!({"shared_peek": "from_assigned"}),
    );
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![suppressed, assigned]));

    assert_eq!(
        ctx.peek_variable_value("shared_peek", "default").unwrap(),
        json!("from_assigned")
    );
    assert_eq!(ctx.pending(), 0);
}

/// A logger that panics on exposures of experiment `id` while `armed`.
fn panicking_logger(id: i64, armed: Arc<AtomicBool>) -> EventLogger {
    Box::new(move |_, event, data| {
        let hit = data
            .as_ref()
            .and_then(|d| d["id"].as_i64())
            .is_some_and(|logged| logged == id);
        if event == "exposure" && hit && armed.load(Ordering::Relaxed) {
            panic!("boom");
        }
    })
}

#[test]
fn throwing_logger_does_not_permanently_lose_sibling_holdout_exposure() {
    let exp = covered_by(
        experiment(1, "exp_multi_holdout_throwing_logger"),
        &[11, 12],
    );
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_a", HOLDOUT_A),
                holdout(12, "holdout_b", HOLDOUT_B),
            ],
            vec![exp],
        ),
    );
    ctx.set_event_logger(panicking_logger(11, Arc::new(AtomicBool::new(true))));

    let result = catch_unwind(AssertUnwindSafe(|| {
        ctx.treatment("exp_multi_holdout_throwing_logger")
    }));

    assert!(result.is_err());
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn throwing_logger_does_not_make_experiment_or_holdout_exposure_retryable() {
    let exp = covered_by(experiment(1, "exp_throwing_logger_terminal"), &[11]);
    let mut ctx = context(
        UID,
        data(vec![holdout(11, "holdout_terminal", HOLDOUT_B)], vec![exp]),
    );
    let armed = Arc::new(AtomicBool::new(true));
    ctx.set_event_logger(panicking_logger(1, armed.clone()));

    let result = catch_unwind(AssertUnwindSafe(|| {
        ctx.treatment("exp_throwing_logger_terminal")
    }));
    assert!(result.is_err());
    armed.store(false, Ordering::Relaxed);

    assert_eq!(
        ctx.treatment("exp_throwing_logger_terminal").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_throwing_logger_terminal", NORMAL_VARIANT),
            holdout_exposure(11, "holdout_terminal", 1)
        ]
    );
}

#[test]
fn throwing_logger_does_not_strand_later_variable_candidates_holdout_exposure() {
    let first = covered_by(
        with_variables(experiment(1, "exp_var_panic_first"), "k"),
        &[11],
    );
    let second = covered_by(
        with_variables(experiment(2, "exp_var_panic_second"), "k"),
        &[12],
    );
    let mut ctx = context(
        UID,
        data(
            vec![
                holdout(11, "holdout_a", HOLDOUT_A),
                holdout(12, "holdout_c", HOLDOUT_A),
            ],
            vec![first, second],
        ),
    );
    let armed = Arc::new(AtomicBool::new(true));
    ctx.set_event_logger(panicking_logger(11, armed.clone()));

    let result = catch_unwind(AssertUnwindSafe(|| ctx.variable_value("k", "default")));

    assert_eq!(
        result.unwrap_err().downcast_ref::<&str>(),
        Some(&"boom"),
        "the original panic is resumed"
    );
    armed.store(false, Ordering::Relaxed);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_a", 0),
            holdout_exposure(12, "holdout_c", 0)
        ]
    );
}

#[test]
fn throwing_logger_on_variable_winner_still_stops_the_candidate_walk() {
    let winner = with_config(
        experiment(1, "exp_var_panic_winner"),
        1,
        json!({"k": "won"}),
    );
    let later = covered_by(
        with_variables(experiment(2, "exp_var_panic_later"), "k"),
        &[11],
    );
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![winner, later]));
    ctx.set_event_logger(panicking_logger(1, Arc::new(AtomicBool::new(true))));

    assert!(catch_unwind(AssertUnwindSafe(|| ctx.variable_value("k", "default"))).is_err());
    ctx.set_event_logger(Box::new(|_, _, _| {}));
    assert_eq!(
        queued(&mut ctx),
        vec![normal(1, "exp_var_panic_winner", NORMAL_VARIANT)]
    );
}

// Java checks a flush is scheduled; this crate has no timer in `Context`,
// so the exposure must still be queued and publishable.
#[test]
fn throwing_logger_on_suppressed_experiments_only_holdout_still_schedules_flush() {
    let exp = covered_by(experiment(1, "exp_stranded_flush"), &[11]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    let armed = Arc::new(AtomicBool::new(true));
    ctx.set_event_logger(panicking_logger(11, armed.clone()));

    assert!(catch_unwind(AssertUnwindSafe(|| ctx.treatment("exp_stranded_flush"))).is_err());
    assert_eq!(ctx.pending(), 1);

    armed.store(false, Ordering::Relaxed);
    ctx.publish().unwrap();
    assert_eq!(ctx.pending(), 0);
}

#[test]
fn null_holdout_array_element_is_ignored_and_experiment_assigns_normally() {
    let data: ContextData = serde_json::from_value(json!({
        "experiments": [experiment(1, "exp_null_holdout_entry")],
        "holdouts": [null]
    }))
    .unwrap();
    let mut ctx = context(UID, data);

    assert_eq!(
        ctx.treatment("exp_null_holdout_entry").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        queued(&mut ctx),
        vec![normal(1, "exp_null_holdout_entry", NORMAL_VARIANT)]
    );
}

#[test]
fn holdout_with_null_split_is_ignored_and_experiment_assigns_normally() {
    let mut h = holdout(11, "holdout_null_split", HOLDOUT_A);
    h["split"] = Value::Null;
    let exp = covered_by(experiment(1, "exp_null_split_holdout"), &[11]);
    let mut ctx = context(UID, data(vec![h], vec![exp]));

    assert_eq!(
        ctx.treatment("exp_null_split_holdout").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        queued(&mut ctx),
        vec![normal(1, "exp_null_split_holdout", NORMAL_VARIANT)]
    );
}

#[test]
fn holdout_with_empty_split_is_ignored_and_experiment_assigns_normally() {
    let mut h = holdout(11, "holdout_empty_split", HOLDOUT_A);
    h["split"] = json!([]);
    let exp = covered_by(experiment(1, "exp_empty_split_holdout"), &[11]);
    let mut ctx = context(UID, data(vec![h], vec![exp]));

    assert_eq!(
        ctx.treatment("exp_empty_split_holdout").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(
        queued(&mut ctx),
        vec![normal(1, "exp_empty_split_holdout", NORMAL_VARIANT)]
    );
}

// Java feeds a stale holdout object into its trigger path. Here holdouts are
// always resolved from the current index, so the equivalent is an
// experiment first resolved after the iteration bump: it reuses the newer,
// already-exposed arm instead of drawing or exposing again.
#[test]
fn stale_holdout_reference_never_overwrites_newer_pinned_cache_entry() {
    let f = || covered_by(experiment(1, "exp_f"), &[11]);
    let mut ctx = context(
        UID,
        data(vec![holdout(11, "holdout_h", HOLDOUT_A)], vec![f()]),
    );

    assert_eq!(ctx.treatment("exp_f").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    let mut iteration2 = holdout(11, "holdout_h", HOLDOUT_B);
    iteration2["iteration"] = json!(2);
    let g = covered_by(experiment(2, "exp_g"), &[11]);
    let late = covered_by(experiment(3, "exp_late"), &[11]);
    ctx.refresh_with(data(vec![iteration2], vec![f(), g, late]));

    assert_eq!(ctx.treatment("exp_g").unwrap(), NORMAL_VARIANT);
    assert_eq!(ctx.pending(), 3);
    assert_eq!(ctx.peek("exp_late").unwrap(), NORMAL_VARIANT);
    let pinned = ctx.assignments["exp_late"].holdouts[0]
        .assignment
        .clone()
        .unwrap();
    assert_eq!((pinned.iteration, pinned.variant), (2, 1));
    assert!(pinned.exposed.load(Ordering::Relaxed));
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_h", 0),
            normal(2, "exp_g", NORMAL_VARIANT),
            holdout_exposure(11, "holdout_h", 1)
        ]
    );
}

#[test]
fn holdout_becoming_applicable_after_refresh_still_fires_for_overridden_experiment() {
    let mut ctx = context(
        UID,
        data(vec![], vec![experiment(1, "exp_override_late_holdout")]),
    );
    ctx.set_override("exp_override_late_holdout", 3).unwrap();

    assert_eq!(ctx.treatment("exp_override_late_holdout").unwrap(), 3);
    assert_eq!(ctx.pending(), 1);

    ctx.refresh_with(data(
        vec![holdout(11, "holdout_late", HOLDOUT_A)],
        vec![covered_by(
            experiment(1, "exp_override_late_holdout"),
            &[11],
        )],
    ));

    assert_eq!(ctx.treatment("exp_override_late_holdout").unwrap(), 3);
    let own = exposure(
        1,
        "exp_override_late_holdout",
        UNIT_TYPE,
        3,
        false,
        true,
        true,
        false,
        false,
        false,
        false,
    );
    assert_eq!(
        queued(&mut ctx),
        vec![own.clone(), own, holdout_exposure(11, "holdout_late", 0)]
    );
}

#[test]
fn variable_path_holdout_exposure_schedules_flush_without_any_other_event() {
    let suppressed = with_config(
        covered_by(experiment(1, "exp_var_flush_suppressed"), &[11]),
        1,
        json!({"flush_key": "from_suppressed"}),
    );
    let assigned = with_config(
        experiment(2, "exp_var_flush_assigned"),
        1,
        json!({"flush_key": "from_assigned"}),
    );
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![suppressed, assigned]));
    ctx.treatment("exp_var_flush_assigned").unwrap();
    assert_eq!(ctx.pending(), 1);

    assert_eq!(
        ctx.variable_value("flush_key", "default").unwrap(),
        json!("from_assigned")
    );
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn set_data_does_not_mutate_caller_supplied_holdout_ids_array() {
    let mut covered = experiment(1, "exp_holdout_no_mutate_in");
    covered["holdoutIds"] = json!([12, 11]);
    let not_covered = experiment(2, "exp_holdout_no_mutate_not_covered");
    let mut ctx = context(
        UID,
        data(
            vec![holdout(11, "holdout_no_mutate", HOLDOUT_A)],
            vec![covered, not_covered],
        ),
    );

    assert_eq!(ctx.treatment("exp_holdout_no_mutate_in").unwrap(), 0);
    assert_eq!(
        ctx.treatment("exp_holdout_no_mutate_not_covered").unwrap(),
        NORMAL_VARIANT
    );
    assert_eq!(ctx.data().unwrap().experiments[0].holdout_ids, vec![12, 11]);
}

fn rules_context(country: &str, rules: &str) -> Context {
    let mut exp = covered_by(experiment(1, "exp_holdout_rules"), &[11]);
    exp["assignmentRules"] = json!(rules);
    let mut ctx = context(
        UID,
        data(
            vec![holdout(11, "holdout_rules_suppression", HOLDOUT_A)],
            vec![exp],
        ),
    );
    ctx.set_attribute("country", country).unwrap();
    ctx
}

fn rule_exposure(name: &str, variant: i32) -> Seen {
    exposure(
        1, name, UNIT_TYPE, variant, false, true, false, false, false, false, true,
    )
}

#[test]
fn matching_assignment_rule_wins_over_holdout_suppression() {
    let mut ctx = rules_context("US", US_RULE);

    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 1);
    assert_eq!(
        queued(&mut ctx),
        vec![
            rule_exposure("exp_holdout_rules", 1),
            holdout_exposure(11, "holdout_rules_suppression", 0)
        ]
    );
}

#[test]
fn matching_split_rule_wins_over_holdout_suppression() {
    let mut ctx = rules_context(
        "GB",
        r#"{"rules":[{"name":"Everyone","type":"split","conditions":null,"environments":[],"percentages":"0/100"}]}"#,
    );

    assert_eq!(ctx.peek("exp_holdout_rules").unwrap(), 1);
    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 1);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn held_out_unit_is_suppressed_when_rule_does_not_match() {
    let mut ctx = rules_context("GB", US_RULE);

    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);

    ctx.set_attribute("country", "US").unwrap();
    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 1);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn held_out_unit_is_suppressed_again_when_rule_stops_matching() {
    let mut ctx = rules_context("US", US_RULE);

    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 1);
    assert_eq!(ctx.pending(), 2);

    ctx.set_attribute("country", "GB").unwrap();
    assert_eq!(ctx.treatment("exp_holdout_rules").unwrap(), 0);
    assert_eq!(ctx.peek("exp_holdout_rules").unwrap(), 0);
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn variable_value_on_held_out_rule_won_experiment() {
    let mut exp = with_variables(
        covered_by(experiment(1, "exp_holdout_rules"), &[11]),
        "color",
    );
    exp["assignmentRules"] = json!(US_RULE);
    let mut ctx = context(
        UID,
        data(
            vec![holdout(11, "holdout_rules_suppression", HOLDOUT_A)],
            vec![exp],
        ),
    );
    ctx.set_attribute("country", "US").unwrap();

    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("treatment")
    );
    assert_eq!(
        queued(&mut ctx),
        vec![
            rule_exposure("exp_holdout_rules", 1),
            holdout_exposure(11, "holdout_rules_suppression", 0)
        ]
    );
}

#[test]
fn shared_variable_key_prefers_rule_won_over_suppressed_experiment() {
    let held_out = with_variables(covered_by(experiment(1, "exp_held_out"), &[11]), "color");
    let mut rule_won = with_variables(covered_by(experiment(2, "exp_rule_won"), &[11]), "color");
    rule_won["assignmentRules"] = json!(US_RULE);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![held_out, rule_won]));

    ctx.set_attribute("country", "GB").unwrap();
    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("control")
    );

    ctx.set_attribute("country", "US").unwrap();
    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("treatment")
    );
}

#[test]
fn shared_variable_key_prefers_earlier_assigned_over_later_rule_won_experiment() {
    let mut assigned = with_variables(experiment(1, "exp_assigned"), "color");
    assigned["split"] = json!([1.0, 0.0]);
    let mut rule_won = with_variables(experiment(2, "exp_rule_won"), "color");
    rule_won["assignmentRules"] = json!(US_RULE);
    let mut ctx = context(UID, data(vec![], vec![assigned, rule_won]));
    ctx.set_attribute("country", "US").unwrap();

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("control")
    );
    assert_eq!(ctx.peek("exp_rule_won").unwrap(), 1);
}

#[test]
fn is_held_out_by_follows_the_arm_count() {
    assert!(is_held_out_by(0, 2, 0));
    assert!(is_held_out_by(0, 3, 2));
    assert!(!is_held_out_by(1, 2, 0));
    assert!(is_held_out_by(1, 3, 0));
    assert!(!is_held_out_by(1, 3, 2));
    assert!(!is_held_out_by(2, 3, 0));
    assert!(!is_held_out_by(1, 4, 0));
}

// The arm count comes from `split`, never from `holdoutType` (scenario 222).
#[test]
fn three_arm_arity_comes_from_split_without_holdout_type() {
    for kind in [Value::Null, json!("full"), json!(42)] {
        let mut h = three_arm_holdout(21, "holdout_arity_from_split", THREE_ARM_1);
        h["holdoutType"] = kind;
        let mut ctx = context(
            UID,
            data(
                vec![h],
                vec![covered_by(experiment(1, "exp_non_fullon"), &[21])],
            ),
        );
        assert_eq!(ctx.treatment("exp_non_fullon").unwrap(), 0);
    }
}

#[test]
fn unknown_fields_in_holdouts_are_ignored() {
    let mut h = holdout(11, "holdout_a", HOLDOUT_A);
    h["futureScalar"] = json!(7);
    h["futureObject"] = json!({"nested": {"deep": [1, 2]}});
    h["futureArray"] = json!([{"x": 1}, "y", null]);
    h["holdoutType"] = json!("some_future_type");
    let mut covered = covered_by(experiment(1, "exp_covered"), &[11]);
    covered["futureHoldoutField"] = json!({"a": [1]});
    let mut plain = experiment(2, "exp_plain");
    plain["futureArray"] = json!([]);
    let data: ContextData = serde_json::from_value(json!({
        "experiments": [covered, plain],
        "holdouts": [h],
        "futureRoot": {"holdoutsV2": [1, 2, 3]}
    }))
    .unwrap();
    let mut ctx = context(UID, data);

    assert_eq!(ctx.treatment("exp_covered").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_plain").unwrap(), NORMAL_VARIANT);
    assert_eq!(
        queued(&mut ctx),
        vec![
            holdout_exposure(11, "holdout_a", 0),
            normal(2, "exp_plain", NORMAL_VARIANT)
        ]
    );
}

#[test]
fn malformed_holdouts_degrade_only_themselves() {
    // The expected verdict comes from the input, not the decoder: only the
    // mixed list still carries a valid holdout 11, which must hold UID out.
    for (holdouts, expected) in [
        (json!({"not": "an array"}), NORMAL_VARIANT),
        (json!("nope"), NORMAL_VARIANT),
        (
            json!([{"id": "eleven"}, holdout(11, "holdout_a", HOLDOUT_A)]),
            0,
        ),
    ] {
        let data: ContextData = serde_json::from_value(json!({
            "experiments": [
                covered_by(experiment(1, "exp_covered"), &[11]),
                experiment(2, "exp_plain")
            ],
            "holdouts": holdouts
        }))
        .unwrap();
        let mut ctx = context(UID, data);
        assert_eq!(ctx.treatment("exp_covered").unwrap(), expected);
        assert_eq!(ctx.treatment("exp_plain").unwrap(), NORMAL_VARIANT);
    }

    let mut covered = experiment(1, "exp_covered");
    covered["holdoutIds"] = json!(["11", 11.0, null, {"id": 11}, 1.5]);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![covered]));
    assert_eq!(ctx.data().unwrap().experiments[0].holdout_ids, vec![11]);
    assert_eq!(ctx.treatment("exp_covered").unwrap(), 0);
}

// A payload without holdouts serializes exactly as before, so the `ready`
// and `refresh` event data and a re-loaded payload are unchanged.
#[test]
fn payload_without_holdouts_serializes_unchanged() {
    let raw = json!({"experiments": [experiment(1, "exp")]});
    let data: ContextData = serde_json::from_value(raw).unwrap();
    let value = serde_json::to_value(&data).unwrap();
    assert!(value.get("holdouts").is_none());
    assert!(value["experiments"][0].get("holdoutIds").is_none());
    assert!(value["experiments"][0].get("holdoutType").is_none());

    let covered: ContextData =
        serde_json::from_value(json!({"experiments": [covered_by(experiment(1, "exp"), &[11])], "holdouts": [holdout(11, "h", HOLDOUT_A)]})).unwrap();
    let again: ContextData =
        serde_json::from_value(serde_json::to_value(&covered).unwrap()).unwrap();
    assert_eq!(again.experiments[0].holdout_ids, vec![11]);
    assert_eq!(again.holdouts[0].holdout_type.as_deref(), Some("full"));
}

// Scenario 305: a holdout holding every unit out; the experiment whose rule
// matches serves the rule variant, the one without a rule is suppressed.
#[test]
fn rule_wins_over_holdout_suppression_scenario_305() {
    let mut h = holdout(9001, "parity_holdout", (24301, 5003));
    h["split"] = json!([1.0, 0.0]);
    let mut with_rule = covered_by(experiment(1, "exp_rules_holdout"), &[9001]);
    with_rule["assignmentRules"] = json!(
        r#"{"rules":[{"name":"all","type":"assign","conditions":null,"environments":[],"variant":1}]}"#
    );
    let plain = covered_by(experiment(2, "exp_rules_holdout_plain"), &[9001]);
    let mut ctx = context("svk-unit-1", data(vec![h], vec![with_rule, plain]));

    assert_eq!(ctx.treatment("exp_rules_holdout").unwrap(), 1);
    assert_eq!(ctx.treatment("exp_rules_holdout_plain").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            rule_exposure("exp_rules_holdout", 1),
            holdout_exposure(9001, "parity_holdout", 0)
        ]
    );
}

// Scenario 225.
#[test]
fn refresh_adding_or_removing_coverage_re_resolves_experiment() {
    let h = || holdout(1, "holdout_coverage_change", HOLDOUT_A);
    let mut ctx = context(
        UID,
        data(vec![h()], vec![experiment(1, "exp_coverage_change")]),
    );

    assert_eq!(
        ctx.treatment("exp_coverage_change").unwrap(),
        NORMAL_VARIANT
    );
    ctx.refresh_with(data(
        vec![h()],
        vec![covered_by(experiment(1, "exp_coverage_change"), &[1])],
    ));
    assert_eq!(ctx.treatment("exp_coverage_change").unwrap(), 0);
    ctx.refresh_with(data(vec![h()], vec![experiment(1, "exp_coverage_change")]));
    assert_eq!(
        ctx.treatment("exp_coverage_change").unwrap(),
        NORMAL_VARIANT
    );

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_coverage_change", NORMAL_VARIANT),
            holdout_exposure(1, "holdout_coverage_change", 0),
            normal(1, "exp_coverage_change", NORMAL_VARIANT)
        ]
    );
}

// Scenario 226: a held-out unit reads the control value, not the default.
#[test]
fn variable_value_of_suppressed_experiment_returns_control() {
    let exp = with_variables(
        covered_by(experiment(1, "exp_holdout_variables"), &[11]),
        "banner_color",
    );
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));

    assert_eq!(
        ctx.peek_variable_value("banner_color", "caller_default")
            .unwrap(),
        json!("control")
    );
    assert_eq!(ctx.pending(), 0);
    assert_eq!(
        ctx.variable_value("banner_color", "caller_default")
            .unwrap(),
        json!("control")
    );
    ctx.treatment("exp_holdout_variables").unwrap();
    assert_eq!(queued(&mut ctx), vec![holdout_exposure(11, "holdout_a", 0)]);
}

// The first suppressed candidate's control value is kept even when it lacks
// the key, so a later suppressed candidate cannot supply it (JS
// `_resolveVariableValue`).
#[test]
fn first_suppressed_candidate_decides_the_control_fallback() {
    let mut first = covered_by(experiment(1, "exp_first"), &[11]);
    first["variants"] = json!([
        {"config": "{\"other\":\"x\"}"},
        {"config": "{\"color\":\"treatment\"}"}
    ]);
    let second = with_variables(covered_by(experiment(2, "exp_second"), &[11]), "color");
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![first, second]));

    assert_eq!(
        ctx.peek_variable_value("color", "default").unwrap(),
        json!("default")
    );
    assert_eq!(
        ctx.variable_value("color", "default").unwrap(),
        json!("default")
    );
}

// JS sorts and de-duplicates the covering holdouts by id: exposures go out
// in id order, and a repeated id counts once, so a refresh listing the same
// set without the repeat is not a coverage change (no second exposure).
#[test]
fn holdout_ids_are_sorted_and_deduplicated() {
    let holdouts = || {
        vec![
            holdout(12, "holdout_b", HOLDOUT_B),
            holdout(11, "holdout_a", HOLDOUT_A),
        ]
    };
    let mut ctx = context(
        UID_NOT_HELD_OUT,
        data(
            holdouts(),
            vec![covered_by(experiment(1, "exp_ordered"), &[12, 11, 12])],
        ),
    );

    assert_eq!(ctx.treatment("exp_ordered").unwrap(), 0);
    ctx.refresh_with(data(
        holdouts(),
        vec![covered_by(experiment(1, "exp_ordered"), &[11, 12])],
    ));
    assert_eq!(ctx.treatment("exp_ordered").unwrap(), 0);

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_ordered", 0),
            holdout_exposure(11, "holdout_a", 1),
            holdout_exposure(12, "holdout_b", 1)
        ]
    );
}

// Moving coverage to another holdout id, same iteration, re-resolves.
#[test]
fn refresh_moving_coverage_to_another_holdout_re_resolves_experiment() {
    let holdouts = || {
        vec![
            holdout(11, "holdout_b", HOLDOUT_B),
            holdout(12, "holdout_a", HOLDOUT_A),
        ]
    };
    let mut ctx = context(
        UID,
        data(
            holdouts(),
            vec![covered_by(experiment(1, "exp_moved"), &[11])],
        ),
    );

    assert_eq!(ctx.treatment("exp_moved").unwrap(), NORMAL_VARIANT);
    ctx.refresh_with(data(
        holdouts(),
        vec![covered_by(experiment(1, "exp_moved"), &[12])],
    ));
    assert_eq!(ctx.treatment("exp_moved").unwrap(), 0);

    assert_eq!(
        queued(&mut ctx),
        vec![
            normal(1, "exp_moved", NORMAL_VARIANT),
            holdout_exposure(11, "holdout_b", 1),
            holdout_exposure(12, "holdout_a", 0)
        ]
    );
}

// As in JS, a held-out unit's audience is not evaluated, so it is not
// flagged as a mismatch, and attribute changes don't rebuild (and forget the
// exposure of) the cached suppressed assignment.
#[test]
fn held_out_unit_skips_the_audience() {
    let mut exp = covered_by(experiment(1, "exp_held_out_audience"), &[11]);
    exp["audienceStrict"] = json!(true);
    exp["audience"] = json!(r#"{"filter":[{"gte":[{"var":"age"},{"value":20}]}]}"#);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp]));
    ctx.set_attribute("age", 5).unwrap();

    assert_eq!(ctx.treatment("exp_held_out_audience").unwrap(), 0);
    assert!(
        !ctx.assignments["exp_held_out_audience"]
            .assignment
            .audience_mismatch
    );

    ctx.set_attribute("age", 30).unwrap();
    ctx.set_attribute("age", 1).unwrap();
    assert_eq!(ctx.peek("exp_held_out_audience").unwrap(), 0);
    assert!(ctx.assignments["exp_held_out_audience"].assignment.exposed);
    assert_eq!(ctx.pending(), 1);
}

#[test]
fn refresh_with_seed_change_same_iteration_does_not_re_expose_not_held_out_unit() {
    let exp = || covered_by(experiment(1, "exp_seed_edit_normal"), &[11]);
    let mut ctx = context(UID_NOT_HELD_OUT, single_holdout(HOLDOUT_A, vec![exp()]));

    let variant = ctx.treatment("exp_seed_edit_normal").unwrap();
    assert_eq!(ctx.pending(), 2);

    ctx.refresh_with(single_holdout(HOLDOUT_B, vec![exp()]));

    assert_eq!(ctx.treatment("exp_seed_edit_normal").unwrap(), variant);
    assert_eq!(ctx.pending(), 2);
}

// As in JS, coverage is compared when the experiment is next read, so a
// refresh that removes coverage and a later one that restores it, with no
// read in between, keep the exposed assignment.
#[test]
fn coverage_change_undone_before_the_next_read_does_not_re_expose() {
    let covered = || covered_by(experiment(1, "exp_flip"), &[11]);
    for uid in [UID, UID_NOT_HELD_OUT] {
        let mut ctx = context(uid, single_holdout(HOLDOUT_A, vec![covered()]));
        let variant = ctx.treatment("exp_flip").unwrap();
        let pending = ctx.pending();

        ctx.refresh_with(single_holdout(HOLDOUT_A, vec![experiment(1, "exp_flip")]));
        ctx.refresh_with(single_holdout(HOLDOUT_A, vec![covered()]));

        assert_eq!(ctx.treatment("exp_flip").unwrap(), variant);
        assert_eq!(ctx.pending(), pending, "uid {uid}");
        assert!(!ctx.assignments["exp_flip"]
            .coverage_stale
            .load(Ordering::Relaxed));
    }
}

// A coverage change noticed on read rebuilds an exposed rule win even when
// the rule gives the same variant, so the newly covering holdout fires.
#[test]
fn coverage_added_to_exposed_rule_win_fires_the_holdout() {
    let mut exp = experiment(1, "exp_rule_cov");
    exp["assignmentRules"] = json!(US_RULE);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp.clone()]));
    ctx.set_attribute("country", "US").unwrap();

    assert_eq!(ctx.treatment("exp_rule_cov").unwrap(), 1);
    ctx.refresh_with(single_holdout(HOLDOUT_A, vec![covered_by(exp, &[11])]));
    assert_eq!(ctx.treatment("exp_rule_cov").unwrap(), 1);

    let exposures = queued(&mut ctx);
    assert_eq!(
        exposures.last(),
        Some(&holdout_exposure(11, "holdout_a", 0))
    );
}

// A holdout keeps holding units out when a field the SDK never reads is
// null or the wrong type, as in JS; only the fields it uses matter.
#[test]
fn malformed_unused_holdout_fields_keep_the_holdout() {
    let cases = [
        ("name", Value::Null),
        ("iteration", Value::Null),
        ("variants", json!(null)),
        ("variants", json!("x")),
        ("trafficSplit", json!({"a": 1})),
        ("audienceStrict", json!("yes")),
        ("fullOnVariant", json!("2")),
        ("audience", json!(5)),
        ("unitType", json!(7)),
        ("customFieldValues", json!([{"name": "x"}])),
        ("applications", Value::Null),
        ("id", json!(11.0)),
    ];
    for (key, value) in cases {
        let mut h = holdout(11, "holdout_a", HOLDOUT_A);
        h[key] = value.clone();
        let exp = covered_by(experiment(1, "exp_covered"), &[11]);
        let mut ctx = context(UID, data(vec![h], vec![exp]));
        assert_eq!(ctx.treatment("exp_covered").unwrap(), 0, "{key}: {value}");
    }

    // Seeds are read like experiment seeds: signed or unsigned 32-bit.
    let mut signed = holdout(11, "holdout_a", HOLDOUT_A);
    signed["seedHi"] = json!(-1);
    let unsigned = {
        let mut h = signed.clone();
        h["seedHi"] = json!(u32::MAX);
        h
    };
    let arm = |h: Value| {
        let exp = covered_by(experiment(1, "exp_covered"), &[11]);
        let mut ctx = context(UID, data(vec![h], vec![exp]));
        ctx.treatment("exp_covered").unwrap();
        queued(&mut ctx)
    };
    assert_eq!(arm(signed), arm(unsigned));
}

// A holdout iteration bump re-resolves an exposed rule win, so the new
// iteration's holdout exposure fires along with the rule exposure.
#[test]
fn iteration_bump_on_exposed_rule_win_fires_the_new_holdout_iteration() {
    let mut exp = covered_by(experiment(1, "exp_rule_iter"), &[11]);
    exp["assignmentRules"] = json!(US_RULE);
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![exp.clone()]));
    ctx.set_attribute("country", "US").unwrap();
    assert_eq!(ctx.treatment("exp_rule_iter").unwrap(), 1);
    let before = ctx.pending();

    let mut h = holdout(11, "holdout_a", HOLDOUT_A);
    h["iteration"] = json!(2);
    ctx.refresh_with(data(vec![h], vec![exp]));

    assert_eq!(ctx.treatment("exp_rule_iter").unwrap(), 1);
    assert_eq!(ctx.pending(), before + 2);
}

// An override flagged by a coverage change and then removed from the data
// keeps its exposed assignment, as in JS.
#[test]
fn override_kept_when_experiment_disappears_after_coverage_flag() {
    let exp = covered_by(experiment(1, "exp_gone"), &[11]);
    let mut ctx = context(
        UID_NOT_HELD_OUT,
        single_holdout(HOLDOUT_A, vec![exp.clone()]),
    );
    ctx.set_override("exp_gone", 1).unwrap();
    ctx.treatment("exp_gone").unwrap();
    let pending = ctx.pending();

    ctx.refresh_with(single_holdout(HOLDOUT_A, vec![exp]));
    ctx.refresh_with(single_holdout(HOLDOUT_A, vec![]));

    assert_eq!(ctx.treatment("exp_gone").unwrap(), 1);
    assert_eq!(ctx.pending(), pending);
}

// Only a held-out unit reads control values: an experiment that is not
// assigned for another reason (strict audience mismatch, missing unit)
// still returns the caller's default, as on main and in JS.
#[test]
fn not_assigned_uncovered_experiment_variables_return_the_default() {
    let mut strict = with_config(experiment(1, "exp_strict"), 0, json!({"color": "red"}));
    strict["audienceStrict"] = json!(true);
    strict["audience"] = json!(r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#);
    let no_unit = with_config(
        experiment_with(2, "exp_nounit", "user_id", 0),
        0,
        json!({"size": "big"}),
    );
    let mut ctx = context(UID, data(vec![], vec![strict, no_unit]));
    ctx.set_attribute("country", "FR").unwrap();

    for key in ["color", "size"] {
        assert_eq!(ctx.peek_variable_value(key, "dflt").unwrap(), json!("dflt"));
        assert_eq!(ctx.variable_value(key, "dflt").unwrap(), json!("dflt"));
    }
}

#[test]
fn refresh_flags_coverage_only_for_covered_entries() {
    let exps = || {
        vec![
            covered_by(experiment(1, "exp_covered"), &[11]),
            experiment(2, "exp_plain"),
        ]
    };
    let mut ctx = context(UID, single_holdout(HOLDOUT_A, exps()));
    ctx.treatment("exp_covered").unwrap();
    ctx.treatment("exp_plain").unwrap();

    ctx.refresh_with(single_holdout(HOLDOUT_A, exps()));

    let stale =
        |ctx: &Context, name: &str| ctx.assignments[name].coverage_stale.load(Ordering::Relaxed);
    assert!(stale(&ctx, "exp_covered"));
    assert!(!stale(&ctx, "exp_plain"));
}

#[test]
fn overridden_or_rule_won_experiment_is_not_the_control_fallback() {
    for rule_won in [false, true] {
        let mut first = covered_by(experiment(1, "exp_first"), &[11]);
        first["variants"] = json!([
            {"config": "{\"other\":\"x\"}"},
            {"config": "{\"other\":\"y\"}"}
        ]);
        if rule_won {
            first["assignmentRules"] = json!(US_RULE);
        }
        let second = with_variables(covered_by(experiment(2, "exp_second"), &[11]), "color");
        let mut ctx = context(UID, single_holdout(HOLDOUT_A, vec![first, second]));
        if rule_won {
            ctx.set_attribute("country", "US").unwrap();
        } else {
            ctx.set_override("exp_first", 1).unwrap();
        }
        // Make exp_first a candidate for `color` too, without the key.
        ctx.index_variables
            .get_mut("color")
            .unwrap()
            .insert(0, "exp_first".to_string());

        assert_eq!(
            ctx.peek_variable_value("color", "default").unwrap(),
            json!("control"),
            "rule_won {rule_won}"
        );
        assert_eq!(
            ctx.variable_value("color", "default").unwrap(),
            json!("control"),
            "rule_won {rule_won}"
        );
    }
}

#[test]
fn numeric_holdout_seeds_follow_js_touint32() {
    for (input, expected) in [
        (json!(1.9), 1),
        (json!(-1.9), u32::MAX),
        (json!(1e20), 1_661_992_960),
        (json!(9007199254740993u64), 0),
    ] {
        let mut h = holdout(11, "h", (0, 0));
        h["seedLo"] = input.clone();
        let decoded = data(vec![h], vec![]);
        assert_eq!(decoded.holdouts[0].seed_lo, expected, "{input}");
    }
}
