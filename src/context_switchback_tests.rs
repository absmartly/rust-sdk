//! Switchbacks through `Context`, ported from java-sdk's
//! `ContextSwitchbackTest`, `ContextSwitchbackClusterTest`,
//! `ContextSwitchbackClusterRuleTest` and `SwitchbackServerParityTest` (same
//! case names in snake_case).
//!
//! Java's concurrency cases (two threads resolving one period, a refresh
//! from inside the audience matcher) guard lock windows. `Context` takes
//! `&mut self`, so they have no counterpart; their single-threaded sequences
//! are covered by the cases below.

use super::*;
use crate::switchback::assign_switchback;
use serde_json::json;
use std::sync::atomic::AtomicI64;
use std::sync::Mutex;

const UNIT_TYPE: &str = "store_id";
const UID: &str = "e791e240fcd3df7d238cfc285f475e8152fcc0ec";
// hash_unit(UID).
const UID_HASH: &str = "pAE3a1i5Drs5mKRNq56adA";

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
// 2026-01-01T00:00:00Z, hour aligned.
const ANCHOR: i64 = 1_767_225_600_000;

const SEED_HI: i64 = -5;
const SEED_LO: i64 = -7;
const SEED: i64 = (SEED_HI << 32) | (SEED_LO & 0xffff_ffff);

const CLUSTER_PATH: &str = "store_key";
const CLUSTER_KEY: &str = "store-42";
// Periods 0..11 of "store-42" under (SEED_HI, SEED_LO), from java-sdk's
// independent Python reference.
const EXPECTED_2_ARMS: [i32; 12] = [0, 1, 0, 1, 1, 0, 1, 0, 1, 0, 0, 1];
const EXPECTED_3_ARMS: [i32; 12] = [0, 2, 1, 2, 1, 0, 1, 2, 0, 1, 0, 2];
const EXPECTED_10_ARMS: [i32; 12] = [8, 3, 7, 4, 2, 0, 5, 1, 9, 6, 3, 7];
// The same reference keyed by UID_HASH and by UID.
const UID_HASH_3_ARMS: [i32; 12] = [1, 2, 0, 2, 1, 0, 2, 0, 1, 1, 2, 0];
const UID_3_ARMS: [i32; 12] = [2, 0, 1, 0, 1, 2, 1, 2, 0, 0, 1, 2];

const STRICT_GOLD_AUDIENCE: &str = r#"{"filter":[{"eq":[{"var":"tier"},{"value":"gold"}]}]}"#;
const ASSIGN_VARIANT_1: &str = r#"{"rules":[{"type":"assign","environments":[],"variant":1}]}"#;
const ASSIGN_VARIANT_2: &str = r#"{"rules":[{"type":"assign","environments":[],"variant":2}]}"#;

/// A clock the test moves. `next_read`, when set, is returned once.
#[derive(Clone)]
struct Clock {
    now: Arc<AtomicI64>,
    next_read: Arc<Mutex<Option<i64>>>,
}

impl Clock {
    fn new(now: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(now)),
            next_read: Arc::new(Mutex::new(None)),
        }
    }

    fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }

    fn next_read(&self, value: i64) {
        *self.next_read.lock().unwrap() = Some(value);
    }

    fn install(&self, ctx: &mut Context) {
        let clock = self.clone();
        ctx.clock = Some(Arc::new(move || {
            clock
                .next_read
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| clock.now.load(Ordering::SeqCst))
        }));
    }
}

fn switchback(id: i64, name: &str, arms: usize, period_ms: i64) -> Value {
    let variants: Vec<Value> = (0..arms)
        .map(|i| json!({"name": ((b'A' + i as u8) as char).to_string(), "config": json!({"sb.arm": i}).to_string()}))
        .collect();
    json!({
        "id": id,
        "name": name,
        "unitType": UNIT_TYPE,
        "iteration": 1,
        "seedHi": SEED_HI,
        "seedLo": SEED_LO,
        "split": vec![1.0 / arms as f64; arms],
        "trafficSeedHi": 1,
        "trafficSeedLo": 2,
        "trafficSplit": [0.0, 1.0],
        "fullOnVariant": 0,
        "applications": [{"name": "website"}],
        "variants": variants,
        "audience": null,
        "audienceStrict": false,
        "randomizationType": "switchback",
        "switchbackClusters": {UNIT_TYPE: CLUSTER_PATH},
        "switchbackStartAt": ANCHOR,
        "switchbackPeriodMs": period_ms,
        "switchbackCarryoverDurationMs": 0,
        "switchbackBurnInDurationMs": 0,
        "switchbackBurnOutDurationMs": 0,
        "switchbackDesignType": "balanced_block",
        "switchbackEstimand": "global_average_treatment_effect"
    })
}

fn sb(arms: usize, period_ms: i64) -> Value {
    switchback(1, "exp_sb", arms, period_ms)
}

/// The same experiment as a plain unit experiment.
fn unit_twin(mut experiment: Value) -> Value {
    let map = experiment.as_object_mut().unwrap();
    for key in [
        "randomizationType",
        "switchbackClusters",
        "switchbackStartAt",
        "switchbackPeriodMs",
    ] {
        map.remove(key);
    }
    experiment
}

fn data_of(experiments: Vec<Value>) -> ContextData {
    serde_json::from_value(json!({ "experiments": experiments })).unwrap()
}

fn data_with_holdouts(experiments: Vec<Value>, holdouts: Vec<Value>) -> ContextData {
    serde_json::from_value(json!({ "experiments": experiments, "holdouts": holdouts })).unwrap()
}

/// A context at the clock's reading with `units`, the cluster attribute set
/// then.
fn context_with(clock: &Clock, units: &[(&str, &str)], data: ContextData) -> Context {
    let mut ctx = Context::new(data);
    clock.install(&mut ctx);
    for (unit_type, uid) in units {
        ctx.set_unit(unit_type, *uid).unwrap();
    }
    ctx.set_attribute(CLUSTER_PATH, CLUSTER_KEY).unwrap();
    ctx
}

fn context(clock: &Clock, data: ContextData) -> Context {
    context_with(clock, &[(UNIT_TYPE, UID)], data)
}

/// A context with no attributes set.
fn bare_context(clock: &Clock, units: &[(&str, Value)], data: ContextData) -> Context {
    let mut ctx = Context::new(data);
    clock.install(&mut ctx);
    for (unit_type, uid) in units {
        ctx.set_unit(unit_type, uid.clone()).unwrap();
    }
    ctx
}

type Seen = (i64, String, Option<String>, i32, i64, [bool; 7]);

#[allow(clippy::too_many_arguments)]
fn seen(
    id: i64,
    name: &str,
    unit: &str,
    variant: i32,
    exposed_at: i64,
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
        exposed_at,
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

fn exposure(
    id: i64,
    name: &str,
    variant: i32,
    exposed_at: i64,
    assigned: bool,
    eligible: bool,
) -> Seen {
    seen(
        id, name, UNIT_TYPE, variant, exposed_at, assigned, eligible, false, false, false, false,
        false,
    )
}

fn rule_exposure(name: &str, variant: i32, exposed_at: i64) -> Seen {
    seen(
        1, name, UNIT_TYPE, variant, exposed_at, false, true, false, false, false, false, true,
    )
}

fn strict_mismatch_exposure(exposed_at: i64) -> Seen {
    seen(
        1, "exp_sb", UNIT_TYPE, 0, exposed_at, false, true, false, false, false, true, false,
    )
}

fn as_seen(e: &Exposure) -> Seen {
    (
        e.id,
        e.name.clone(),
        e.unit.clone(),
        e.variant,
        e.exposed_at,
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
}

/// The queued exposures, in order.
fn queued(ctx: &mut Context) -> Vec<Seen> {
    ctx.get_publish_params()
        .exposures
        .unwrap_or_default()
        .iter()
        .map(as_seen)
        .collect()
}

fn peek_schedule(
    ctx: &mut Context,
    clock: &Clock,
    name: &str,
    period_ms: i64,
    periods: usize,
) -> Vec<i32> {
    (0..periods)
        .map(|i| {
            clock.set(ANCHOR + i as i64 * period_ms);
            ctx.peek(name).unwrap()
        })
        .collect()
}

fn cached_slots(ctx: &Context) -> usize {
    ctx.assignments.len()
        + ctx
            .switchbacks
            .values()
            .map(|e| e.periods.len() + usize::from(e.pre_start.is_some()))
            .sum::<usize>()
}

/// The slot the clock's reading resolves to: its serving, eligibility and
/// whether it records nothing.
fn resolution(
    ctx: &Context,
    name: &str,
    now: i64,
) -> Option<(Serving, Option<String>, bool, bool)> {
    let exp = ctx.index.get(name)?;
    let period = switchback_period(exp.switchback.as_ref()?, &exp.data, now);
    let cached = ctx.switchbacks.get(name)?.slot(period)?;
    let state = cached.switchback.as_ref()?;
    Some((
        state.serving.clone(),
        cached.assignment.unit_type.clone(),
        cached.assignment.eligible,
        state.silent,
    ))
}

// The cluster identity is the raw attribute value, neither hashed nor the unit uid.
#[test]
fn cluster_identity_is_the_raw_cluster_value() {
    assert_eq!(crate::utils::hash_unit(UID), UID_HASH);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(3, HOUR)]));
    let served = peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12);

    assert_eq!(served, EXPECTED_3_ARMS);
    assert_ne!(served, UID_HASH_3_ARMS);
    assert_ne!(served, UID_3_ARMS);
    let hashed_key = crate::utils::hash_unit(CLUSTER_KEY);
    let mut hashed_differs = false;
    for (i, &variant) in served.iter().enumerate() {
        assert_eq!(
            assign_switchback(CLUSTER_KEY, i as i64, SEED, 3),
            Some(variant as usize)
        );
        hashed_differs |=
            assign_switchback(&hashed_key, i as i64, SEED, 3) != Some(variant as usize);
    }
    assert!(hashed_differs);
}

#[test]
fn serves_two_three_and_ten_arm_permutations() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(
        &clock,
        data_of(vec![
            switchback(1, "exp_2", 2, HOUR),
            switchback(2, "exp_3", 3, HOUR),
            switchback(3, "exp_10", 10, HOUR),
        ]),
    );
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_2", HOUR, 12),
        EXPECTED_2_ARMS
    );
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_3", HOUR, 12),
        EXPECTED_3_ARMS
    );
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_10", HOUR, 12),
        EXPECTED_10_ARMS
    );
}

#[test]
fn one_hour_boundary_at_exact_edge_milliseconds() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    for (now, expected) in [
        (ANCHOR - 1, 0),
        (ANCHOR, EXPECTED_2_ARMS[0]),
        (ANCHOR + HOUR - 1, EXPECTED_2_ARMS[0]),
        (ANCHOR + HOUR, EXPECTED_2_ARMS[1]),
        (ANCHOR + 3 * HOUR - 1, EXPECTED_2_ARMS[2]),
        (ANCHOR + 3 * HOUR, EXPECTED_2_ARMS[3]),
    ] {
        clock.set(now);
        assert_eq!(ctx.peek("exp_sb").unwrap(), expected, "{}", now - ANCHOR);
    }
}

#[test]
fn twenty_four_hour_boundary_at_exact_edge_milliseconds() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, DAY)]));
    for (now, expected) in [
        (ANCHOR - 1, 0),
        (ANCHOR, EXPECTED_2_ARMS[0]),
        (ANCHOR + HOUR, EXPECTED_2_ARMS[0]),
        (ANCHOR + DAY - 1, EXPECTED_2_ARMS[0]),
        (ANCHOR + DAY, EXPECTED_2_ARMS[1]),
        (ANCHOR + 4 * DAY - 1, EXPECTED_2_ARMS[3]),
        (ANCHOR + 4 * DAY, EXPECTED_2_ARMS[4]),
    ] {
        clock.set(now);
        assert_eq!(ctx.peek("exp_sb").unwrap(), expected, "{}", now - ANCHOR);
    }
}

// The anchor is served as-is, never rounded; before it the experiment is a
// silent control, and an anchor that is not hour aligned is unservable.
#[test]
fn unaligned_start_is_unservable_and_pre_anchor_is_ineligible() {
    let mut aligned = switchback(1, "exp_aligned", 2, HOUR);
    aligned["switchbackStartAt"] = json!(ANCHOR + HOUR);
    let mut unaligned = switchback(2, "exp_unaligned", 2, HOUR);
    let start = ANCHOR + 1_234_567;
    unaligned["switchbackStartAt"] = json!(start);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![aligned, unaligned]));

    for now in [ANCHOR, start - 1, start, ANCHOR + HOUR - 1] {
        clock.set(now);
        assert_eq!(ctx.treatment("exp_aligned").unwrap(), 0);
        assert_eq!(ctx.treatment("exp_unaligned").unwrap(), 0);
    }
    assert_eq!(ctx.pending(), 0);
    for i in 1..4 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(
            ctx.treatment("exp_aligned").unwrap(),
            EXPECTED_2_ARMS[i as usize - 1]
        );
        assert_eq!(ctx.treatment("exp_unaligned").unwrap(), 0);
        clock.set(start + i * HOUR);
        assert_eq!(ctx.treatment("exp_unaligned").unwrap(), 0);
    }
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(
                1,
                "exp_aligned",
                EXPECTED_2_ARMS[0],
                ANCHOR + HOUR,
                true,
                true
            ),
            exposure(
                1,
                "exp_aligned",
                EXPECTED_2_ARMS[1],
                ANCHOR + 2 * HOUR,
                true,
                true
            ),
            exposure(
                1,
                "exp_aligned",
                EXPECTED_2_ARMS[2],
                ANCHOR + 3 * HOUR,
                true,
                true
            ),
        ]
    );
}

// The Registry's anchor bounds: hour aligned and in [0, 10000-01-01).
#[test]
fn anchor_not_on_an_hour_boundary_is_unservable_on_get_and_post() {
    let max_anchor: i64 = 253_402_300_800_000;
    for start_at in [
        ANCHOR + 1,
        ANCHOR - 1,
        ANCHOR + 30 * 60_000,
        ANCHOR + HOUR - 1,
        -HOUR,
        -1,
        max_anchor,
        max_anchor + HOUR,
        i64::MAX - (i64::MAX % HOUR),
    ] {
        let mut experiment = sb(2, HOUR);
        experiment["switchbackStartAt"] = json!(start_at);
        let clock = Clock::new(0);
        let mut ctx = context(&clock, data_of(vec![experiment]));
        for now in [
            0,
            ANCHOR,
            ANCHOR + 2 * HOUR + 1,
            start_at,
            start_at.wrapping_add(HOUR),
            max_anchor + DAY,
        ] {
            clock.set(now);
            assert_eq!(
                ctx.treatment("exp_sb").unwrap(),
                0,
                "startAt {} now {}",
                start_at,
                now
            );
            assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
        }
        assert_eq!(ctx.pending(), 0, "startAt {}", start_at);
    }

    for start_at in [0, ANCHOR, max_anchor - HOUR] {
        let mut experiment = sb(2, HOUR);
        experiment["switchbackStartAt"] = json!(start_at);
        let clock = Clock::new(start_at + HOUR);
        let mut ctx = context(&clock, data_of(vec![experiment]));
        assert_eq!(
            ctx.treatment("exp_sb").unwrap(),
            EXPECTED_2_ARMS[1],
            "startAt {}",
            start_at
        );
        assert_eq!(ctx.pending(), 1);
    }
}

#[test]
fn null_start_at_is_ineligible_and_never_falls_back_to_unit_assignment() {
    let mut experiment = sb(2, HOUR);
    experiment["switchbackStartAt"] = Value::Null;
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    for i in 0..4 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
    }
    assert_eq!(ctx.pending(), 0);
}

#[test]
fn same_period_is_cached_and_next_period_rolls_over_with_one_exposure_each() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));

    clock.set(ANCHOR + 10);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);
    clock.set(ANCHOR + HOUR - 1);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.peek("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);

    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.peek("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    assert_eq!(ctx.pending(), 1);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    assert_eq!(ctx.pending(), 2);
    clock.set(ANCHOR + HOUR + 30 * 60_000);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    assert_eq!(ctx.pending(), 2);

    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(1, "exp_sb", EXPECTED_2_ARMS[0], ANCHOR + 10, true, true),
            exposure(1, "exp_sb", EXPECTED_2_ARMS[1], ANCHOR + HOUR, true, true),
        ]
    );
}

#[test]
fn variable_lookups_follow_the_period_and_expose_once_per_period() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(3, HOUR)]));
    // Listed once per variant that defines the key, as on main.
    assert_eq!(
        ctx.variable_keys().unwrap(),
        HashMap::from([("sb.arm".to_string(), vec!["exp_sb".to_string(); 3])])
    );
    assert_eq!(ctx.experiments().unwrap(), vec!["exp_sb".to_string()]);
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        let expected = json!(EXPECTED_3_ARMS[i as usize]);
        assert_eq!(ctx.peek_variable_value("sb.arm", -1).unwrap(), expected);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), expected);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), expected);
        assert_eq!(
            ctx.treatment("exp_sb").unwrap(),
            EXPECTED_3_ARMS[i as usize]
        );
    }
    assert_eq!(ctx.pending(), 3);
}

// No unit of a linked type: the experiment does not apply and records
// nothing. unitType is never a fallback.
#[test]
fn missing_cluster_unit_is_unassigned_without_fallback() {
    let mut experiment = sb(2, HOUR);
    experiment["unitType"] = json!("user_id");
    let clock = Clock::new(ANCHOR);
    let mut ctx = context_with(&clock, &[("user_id", UID)], data_of(vec![experiment]));
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.peek("exp_sb").unwrap(), 0);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(-1));
    }
    assert_eq!(ctx.pending(), 0);

    // Setting a linked unit later makes the experiment apply.
    ctx.set_unit(UNIT_TYPE, UID).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[2]);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            EXPECTED_2_ARMS[2],
            ANCHOR + 2 * HOUR,
            true,
            true
        )]
    );
}

#[test]
fn full_on_serves_the_full_on_variant_in_every_period() {
    let mut experiment = sb(3, HOUR);
    experiment["fullOnVariant"] = json!(2);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    for i in 0..4 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    }
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 4);
    for e in exposures {
        assert_eq!(e.3, 2);
        assert!(e.5[3], "fullOn");
    }
}

#[test]
fn override_and_custom_assignment_win_in_every_period() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(
        &clock,
        data_of(vec![
            switchback(1, "exp_override", 3, HOUR),
            switchback(2, "exp_custom", 3, HOUR),
        ]),
    );
    ctx.set_override("exp_override", 7).unwrap();
    ctx.set_custom_assignment("exp_custom", 2).unwrap();
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_override").unwrap(), 7);
        assert_eq!(ctx.treatment("exp_custom").unwrap(), 2);
    }
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 6);
    for e in exposures {
        if e.1 == "exp_override" {
            assert!(e.5[2] && e.3 == 7);
        } else {
            assert!(e.5[4] && e.3 == 2);
        }
    }
}

// Traffic eligibility uses the serving unit's traffic hash: it matches a
// unit twin with the same traffic seed and stays fixed across periods.
#[test]
fn traffic_eligibility_is_cluster_sticky() {
    let (mut eligible_count, mut ineligible_count) = (0, 0);
    for u in 0..24 {
        let mut experiment = switchback(1, "exp_sb", 2, HOUR);
        experiment["trafficSplit"] = json!([0.5, 0.5]);
        let mut twin = unit_twin(switchback(2, "exp_unit", 2, HOUR));
        twin["trafficSplit"] = json!([0.5, 0.5]);
        let clock = Clock::new(ANCHOR);
        let uid = format!("store_{}", u);
        let mut ctx = context_with(
            &clock,
            &[(UNIT_TYPE, &uid)],
            data_of(vec![experiment, twin]),
        );
        ctx.treatment("exp_unit").unwrap();
        for i in 0..4 {
            clock.set(ANCHOR + i * HOUR);
            ctx.treatment("exp_sb").unwrap();
        }
        let exposures = queued(&mut ctx);
        assert_eq!(exposures.len(), 5);
        let twin_eligible = exposures[0].5[1];
        for e in &exposures[1..] {
            assert_eq!(e.1, "exp_sb");
            assert_eq!(e.5[1], twin_eligible);
            if !twin_eligible {
                assert_eq!(e.3, 0);
            }
        }
        if twin_eligible {
            eligible_count += 1;
        } else {
            ineligible_count += 1;
        }
    }
    assert!(eligible_count > 0 && ineligible_count > 0);
}

#[test]
fn unit_experiments_are_unaffected() {
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let mut absent = switchback(2, "exp_absent", 2, HOUR);
    absent.as_object_mut().unwrap().remove("randomizationType");
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![unit, absent]));
    let variant = ctx.treatment("exp_sb").unwrap();
    let absent_variant = ctx.treatment("exp_absent").unwrap();
    for i in 1..6 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), variant);
        assert_eq!(ctx.treatment("exp_absent").unwrap(), absent_variant);
    }
    assert_eq!(ctx.pending(), 2);
}

#[test]
fn refresh_invalidates_when_grid_fields_change_but_not_for_analysis_only_fields() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    clock.set(ANCHOR + 10);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);

    let mut burns_changed = sb(2, HOUR);
    burns_changed["switchbackBurnInDurationMs"] = json!(600_000);
    burns_changed["switchbackBurnOutDurationMs"] = json!(300_000);
    burns_changed["switchbackCarryoverDurationMs"] = json!(900_000);
    burns_changed["switchbackEstimand"] = json!("other");
    ctx.refresh_with(data_of(vec![burns_changed]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);

    // Period 0 under both grids, so only the identity can invalidate.
    ctx.refresh_with(data_of(vec![sb(2, 2 * HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 2);

    let mut design_changed = sb(2, 2 * HOUR);
    design_changed["switchbackDesignType"] = json!("other_design");
    ctx.refresh_with(data_of(vec![design_changed]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 3);

    let mut to_unit = sb(2, 2 * HOUR);
    to_unit["randomizationType"] = json!("unit");
    ctx.refresh_with(data_of(vec![to_unit]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 4);

    ctx.refresh_with(data_of(vec![sb(2, 2 * HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 5);

    // The cluster map (types, paths and their order) and the allow-list are identity.
    let mut path_changed = sb(2, 2 * HOUR);
    path_changed["switchbackClusters"] = json!({UNIT_TYPE: "'store_key'"});
    ctx.refresh_with(data_of(vec![path_changed]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 6);

    let mut link_added = sb(2, 2 * HOUR);
    link_added["switchbackClusters"] =
        ordered_clusters(&[(UNIT_TYPE, "'store_key'"), ("device_id", "city")]);
    let link_added = parse_experiments(&[link_added]);
    assert_eq!(
        link_added.experiments[0]
            .switchback_clusters
            .as_deref()
            .unwrap()[0]
            .0,
        UNIT_TYPE
    );
    ctx.refresh_with(link_added);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 7);

    let mut reordered = sb(2, 2 * HOUR);
    reordered["switchbackClusters"] =
        ordered_clusters(&[("device_id", "city"), (UNIT_TYPE, "'store_key'")]);
    let reordered = parse_experiments(&[reordered]);
    assert_eq!(
        reordered.experiments[0]
            .switchback_clusters
            .as_deref()
            .unwrap()[0]
            .0,
        "device_id"
    );
    ctx.refresh_with(reordered.clone());
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 8);

    let mut values_set = reordered.clone();
    values_set.experiments[0].switchback_cluster_values = Some(vec![CLUSTER_KEY.to_string()]);
    ctx.refresh_with(values_set.clone());
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 9);

    // An unrelated unitType edit is not identity: switchbacks never read it.
    let mut unit_type_changed = values_set;
    unit_type_changed.experiments[0].unit_type = Some("anything".to_string());
    ctx.refresh_with(unit_type_changed);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 9);
}

#[test]
fn refresh_setting_start_at_starts_the_grid() {
    let mut pending = sb(2, HOUR);
    pending["switchbackStartAt"] = Value::Null;
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![pending]));
    clock.set(ANCHOR + 10);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);

    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);
}

/// Holdout 11 for UID under seeds (13, 111).
fn holdout(split: Value) -> Value {
    json!({
        "id": 11, "name": "holdout_a", "unitType": UNIT_TYPE, "iteration": 1,
        "seedHi": 13, "seedLo": 111, "split": split, "trafficSplit": [0.0, 1.0]
    })
}

// Holdouts never apply to a switchback: a holdout that would hold the unit
// out neither suppresses it nor records its own exposure through it.
#[test]
fn holdout_suppression_and_exposure_span_periods() {
    let mut covered = sb(2, HOUR);
    covered["holdoutIds"] = json!([11]);
    // split [0.1, 0.9] holds UID out (arm 0).
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(
        &clock,
        data_with_holdouts(vec![covered], vec![holdout(json!([0.1, 0.9]))]),
    );
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(
            ctx.treatment("exp_sb").unwrap(),
            EXPECTED_2_ARMS[i as usize]
        );
        assert_eq!(
            ctx.variable_value("sb.arm", -1).unwrap(),
            json!(EXPECTED_2_ARMS[i as usize])
        );
    }
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(1, "exp_sb", EXPECTED_2_ARMS[0], ANCHOR, true, true),
            exposure(1, "exp_sb", EXPECTED_2_ARMS[1], ANCHOR + HOUR, true, true),
            exposure(
                1,
                "exp_sb",
                EXPECTED_2_ARMS[2],
                ANCHOR + 2 * HOUR,
                true,
                true
            ),
        ]
    );
}

// A matching rule assigns its variant even before the anchor, recorded
// with ruleOverride (Collector assignmentRuleOverridesTheSchedule).
#[test]
fn assignment_rule_overrides_the_schedule() {
    let mut experiment = sb(2, HOUR);
    experiment["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    for u in 0..10 {
        let clock = Clock::new(ANCHOR - 1);
        let uid = format!("user-{}", u);
        let mut ctx = context_with(
            &clock,
            &[(UNIT_TYPE, &uid)],
            data_of(vec![experiment.clone()]),
        );
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(1));
        for i in 0..4 {
            clock.set(ANCHOR + i * HOUR);
            assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
        }
        let mut expected = vec![rule_exposure("exp_sb", 1, ANCHOR - 1)];
        expected.extend((0..4).map(|i| rule_exposure("exp_sb", 1, ANCHOR + i * HOUR)));
        assert_eq!(queued(&mut ctx), expected);
    }
}

// An unservable schedule only replaces the draw: full-on and a matching
// rule still apply and are recorded.
#[test]
fn unservable_only_replaces_the_variant_draw_not_full_on_or_rules() {
    let mut full_on = switchback(1, "sb_full_on", 2, HOUR);
    full_on["switchbackStartAt"] = Value::Null;
    full_on["fullOnVariant"] = json!(1);
    let mut rules = switchback(2, "sb_rules", 2, HOUR);
    rules["switchbackPeriodMs"] = json!(0);
    rules["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    let mut unaligned_rules = switchback(3, "sb_unaligned_rules", 2, HOUR);
    unaligned_rules["switchbackStartAt"] = json!(ANCHOR + 1);
    unaligned_rules["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context_with(
        &clock,
        &[(UNIT_TYPE, "15")],
        data_of(vec![full_on, rules, unaligned_rules]),
    );

    assert_eq!(ctx.treatment("sb_full_on").unwrap(), 1);
    assert_eq!(ctx.treatment("sb_rules").unwrap(), 1);
    assert_eq!(ctx.treatment("sb_unaligned_rules").unwrap(), 1);
    assert_eq!(
        queued(&mut ctx),
        vec![
            seen(
                1,
                "sb_full_on",
                UNIT_TYPE,
                1,
                ANCHOR,
                true,
                true,
                false,
                true,
                false,
                false,
                false
            ),
            seen(
                2, "sb_rules", UNIT_TYPE, 1, ANCHOR, false, true, false, false, false, false, true
            ),
            seen(
                3,
                "sb_unaligned_rules",
                UNIT_TYPE,
                1,
                ANCHOR,
                false,
                true,
                false,
                false,
                false,
                false,
                true
            ),
        ]
    );
}

// Rules are evaluated only for an eligible cluster.
#[test]
fn assignment_rules_do_not_apply_to_an_ineligible_cluster() {
    let mut experiment = sb(2, HOUR);
    experiment["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    experiment["switchbackClusterValues"] = json!(["other-store"]);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);
}

// A split rule randomizes over the serving unit, not over unitType.
#[test]
fn split_rule_randomizes_over_the_serving_unit() {
    let mut experiment = sb(2, HOUR);
    experiment["unitType"] = json!("user_id");
    experiment["assignmentRules"] =
        json!(r#"{"rules":[{"type":"split","environments":[],"percentages":"50/50"}]}"#);
    let mut ones = 0;
    for u in 0..32 {
        let uid = format!("store-{}", u);
        let expected = VariantAssigner::new(&crate::utils::hash_unit(&uid)).assign(
            &[0.5, 0.5],
            SEED_HI as u32,
            SEED_LO as u32,
        ) as i32;
        let clock = Clock::new(ANCHOR);
        let mut ctx = context_with(
            &clock,
            &[(UNIT_TYPE, &uid), ("user_id", "fixed-user")],
            data_of(vec![experiment.clone()]),
        );
        assert_eq!(ctx.treatment("exp_sb").unwrap(), expected, "{}", uid);
        assert_eq!(
            queued(&mut ctx),
            vec![rule_exposure("exp_sb", expected, ANCHOR)],
            "{}",
            uid
        );
        ones += expected;
    }
    assert!(ones > 0 && ones < 32);
}

#[test]
fn audience_strict_mismatch_is_recorded_as_control() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![strict_mismatch_exposure(ANCHOR + HOUR)]
    );
}

// Exact wire form of a strict mismatch, in period 1 whose draw is 1, so the
// variant 0 is the mismatch's control and not a draw of arm 0.
#[test]
fn audience_strict_mismatch_exposure_serializes_exactly() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(EXPECTED_2_ARMS[1], 1);
    clock.set(ANCHOR + HOUR + 5);
    ctx.treatment("exp_sb").unwrap();
    clock.set(ANCHOR + HOUR + 9);
    assert_eq!(
        serde_json::to_value(ctx.get_publish_params()).unwrap(),
        json!({
            "publishedAt": ANCHOR + HOUR + 9,
            "units": [{"type": "store_id", "uid": UID_HASH}],
            "hashed": true,
            "exposures": [{
                "id": 1, "name": "exp_sb", "exposedAt": ANCHOR + HOUR + 5, "unit": "store_id",
                "variant": 0, "assigned": false, "eligible": true, "overridden": false,
                "fullOn": false, "custom": false, "audienceMismatch": true, "ruleOverride": false
            }],
            "attributes": [{"name": CLUSTER_PATH, "value": CLUSTER_KEY, "setAt": ANCHOR}]
        })
    );
}

// The audience check comes before the period check, so a strict mismatch
// before the anchor or with an unservable schedule is recorded.
#[test]
fn audience_strict_mismatch_before_the_anchor_is_recorded() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let clock = Clock::new(ANCHOR - 1);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(queued(&mut ctx), vec![strict_mismatch_exposure(ANCHOR - 1)]);
}

#[test]
fn audience_strict_mismatch_with_an_unservable_schedule_is_recorded() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    experiment["switchbackStartAt"] = Value::Null;
    let clock = Clock::new(ANCHOR + HOUR + 3);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![strict_mismatch_exposure(ANCHOR + HOUR + 3)]
    );
}

// The legacy path returns before the audience is evaluated.
#[test]
fn audience_strict_mismatch_with_a_missing_or_unreadable_cluster_map_records_nothing() {
    for clusters in [Value::Null, json!({UNIT_TYPE: "'geo"})] {
        let mut experiment = sb(2, HOUR);
        experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
        experiment["audienceStrict"] = json!(true);
        experiment["switchbackClusters"] = clusters.clone();
        let clock = Clock::new(ANCHOR + HOUR + 3);
        let mut ctx = context(&clock, data_of(vec![experiment]));
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0, "{}", clusters);
        assert_eq!(ctx.pending(), 0, "{}", clusters);
    }
}

// A strict-mismatched switchback is not assigned, so on a shared variable
// key it gives way to the next experiment, which alone is exposed.
#[test]
fn strict_mismatched_switchback_gives_way_on_a_shared_variable_key() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let mut unit = unit_twin(switchback(2, "exp_unit", 2, HOUR));
    unit["variants"] = json!([
        {"name": "A", "config": r#"{"sb.arm":10}"#},
        {"name": "B", "config": r#"{"sb.arm":11}"#}
    ]);
    unit["fullOnVariant"] = json!(1);
    let clock = Clock::new(ANCHOR + HOUR + 3);
    let mut ctx = context(&clock, data_of(vec![experiment, unit]));
    assert_eq!(
        ctx.variable_keys().unwrap(),
        HashMap::from([(
            "sb.arm".to_string(),
            vec![
                "exp_sb".to_string(),
                "exp_sb".to_string(),
                "exp_unit".to_string(),
                "exp_unit".to_string()
            ]
        )])
    );

    assert_eq!(ctx.peek_variable_value("sb.arm", -1).unwrap(), json!(11));
    assert_eq!(ctx.pending(), 0);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(11));
    let exposures = queued(&mut ctx);
    // The strict-mismatched switchback is not assigned, so only the experiment
    // that supplied the value is exposed, as in java-sdk.
    assert_eq!(
        exposures,
        vec![seen(
            2,
            "exp_unit",
            UNIT_TYPE,
            1,
            ANCHOR + HOUR + 3,
            true,
            true,
            false,
            true,
            false,
            false,
            false
        ),]
    );
}

#[test]
fn strict_mismatched_switchback_alone_on_a_variable_key_returns_the_default() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let clock = Clock::new(ANCHOR + HOUR + 3);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.peek_variable_value("sb.arm", -1).unwrap(), json!(-1));
    assert_eq!(ctx.pending(), 0);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(-1));
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Branch {
    Override,
    OverrideOverStrictMismatch,
    Rule,
    RuleOverStrictMismatch,
    StrictMismatch,
    StrictMismatchOverFullOn,
    StrictMismatchOverTrafficMiss,
    NonStrictMismatch,
    FullOn,
    TrafficMiss,
    Custom,
    Draw,
}

impl Branch {
    const ALL: [Branch; 12] = [
        Branch::Override,
        Branch::OverrideOverStrictMismatch,
        Branch::Rule,
        Branch::RuleOverStrictMismatch,
        Branch::StrictMismatch,
        Branch::StrictMismatchOverFullOn,
        Branch::StrictMismatchOverTrafficMiss,
        Branch::NonStrictMismatch,
        Branch::FullOn,
        Branch::TrafficMiss,
        Branch::Custom,
        Branch::Draw,
    ];

    fn configure(self, experiment: &mut Value) {
        use Branch::*;
        if matches!(
            self,
            OverrideOverStrictMismatch
                | RuleOverStrictMismatch
                | StrictMismatch
                | StrictMismatchOverFullOn
                | StrictMismatchOverTrafficMiss
                | NonStrictMismatch
        ) {
            experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
            experiment["audienceStrict"] = json!(self != NonStrictMismatch);
        }
        if matches!(self, StrictMismatchOverFullOn | FullOn) {
            experiment["fullOnVariant"] = json!(2);
        }
        if matches!(self, StrictMismatchOverTrafficMiss | TrafficMiss) {
            experiment["trafficSplit"] = json!([1.0, 0.0]);
        }
        if matches!(self, Rule | RuleOverStrictMismatch) {
            experiment["assignmentRules"] = json!(ASSIGN_VARIANT_2);
        }
    }

    fn configure_context(self, ctx: &mut Context) {
        match self {
            Branch::Override | Branch::OverrideOverStrictMismatch => {
                ctx.set_override("exp_parity", 2).unwrap()
            }
            Branch::Custom => ctx.set_custom_assignment("exp_parity", 2).unwrap(),
            _ => {}
        }
    }

    /// assigned, eligible, overridden, fullOn, custom, audienceMismatch, ruleOverride.
    fn flags(self) -> [bool; 7] {
        use Branch::*;
        match self {
            Override | OverrideOverStrictMismatch => {
                [false, true, true, false, false, false, false]
            }
            Rule | RuleOverStrictMismatch => [false, true, false, false, false, false, true],
            StrictMismatch | StrictMismatchOverFullOn | StrictMismatchOverTrafficMiss => {
                [false, true, false, false, false, true, false]
            }
            NonStrictMismatch => [true, true, false, false, false, true, false],
            FullOn => [true, true, false, true, false, false, false],
            TrafficMiss => [true, false, false, false, false, false, false],
            Custom => [true, true, false, false, true, false, false],
            Draw => [true, true, false, false, false, false, false],
        }
    }

    /// At period 2, whose 3-arm draw is 1.
    fn variant(self) -> i32 {
        use Branch::*;
        match self {
            Override
            | OverrideOverStrictMismatch
            | Rule
            | RuleOverStrictMismatch
            | FullOn
            | Custom => 2,
            StrictMismatch
            | StrictMismatchOverFullOn
            | StrictMismatchOverTrafficMiss
            | TrafficMiss => 0,
            NonStrictMismatch | Draw => EXPECTED_3_ARMS[2],
        }
    }
}

fn expose_once(clock: &Clock, mut experiment: Value, branch: Branch) -> Seen {
    branch.configure(&mut experiment);
    let mut ctx = context(clock, data_of(vec![experiment]));
    branch.configure_context(&mut ctx);
    ctx.treatment("exp_parity").unwrap();
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 1, "{:?}", branch);
    exposures.into_iter().next().unwrap()
}

// The same branch on a unit experiment and on an eligible-cluster switchback
// publishes the same exposure apart from the variant, and both match the
// SWITCHBACK.md "Exposure flags" table.
#[test]
fn exposure_flags_match_a_unit_experiment_for_every_branch() {
    assert_eq!(EXPECTED_3_ARMS[2], 1);
    for branch in Branch::ALL {
        let clock = Clock::new(ANCHOR + 2 * HOUR + 3);
        let unit = expose_once(
            &clock,
            unit_twin(switchback(1, "exp_parity", 3, HOUR)),
            branch,
        );
        let sb = expose_once(&clock, switchback(1, "exp_parity", 3, HOUR), branch);
        assert_eq!(unit.5, branch.flags(), "unit {:?}", branch);
        assert_eq!(sb.5, branch.flags(), "switchback {:?}", branch);
        assert_eq!(sb.3, branch.variant(), "switchback variant {:?}", branch);
        if !matches!(branch, Branch::Draw | Branch::NonStrictMismatch) {
            assert_eq!(unit.3, branch.variant(), "unit variant {:?}", branch);
        }
        assert_eq!(
            (&unit.0, &unit.1, &unit.2, unit.4),
            (&sb.0, &sb.1, &sb.2, sb.4),
            "{:?}",
            branch
        );
    }
}

// A traffic miss on an eligible cluster is recorded under the serving unit;
// an ineligible cluster records nothing.
#[test]
fn traffic_miss_on_an_eligible_cluster_publishes_an_ineligible_exposure() {
    let mut miss = switchback(1, "exp_miss", 2, HOUR);
    miss["trafficSplit"] = json!([1.0, 0.0]);
    let mut ineligible_cluster = switchback(2, "exp_cluster_ineligible", 2, HOUR);
    ineligible_cluster["trafficSplit"] = json!([1.0, 0.0]);
    ineligible_cluster["switchbackClusterValues"] = json!(["other-store"]);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![miss, ineligible_cluster]));
    clock.set(ANCHOR + 2 * HOUR + 3);
    assert_eq!(ctx.treatment("exp_miss").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_cluster_ineligible").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_miss",
            0,
            ANCHOR + 2 * HOUR + 3,
            true,
            false
        )]
    );
}

#[test]
fn custom_field_values_and_unit_lookups_work() {
    let clock = Clock::new(ANCHOR);
    let ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.get_unit(UNIT_TYPE), Some(&json!(UID)));
    assert!(ctx.custom_field_keys().unwrap().is_empty());
    assert_eq!(ctx.custom_field_value("exp_sb", "missing").unwrap(), None);
}

// Each experiment keeps entries for at most its two most recent periods.
#[test]
fn per_period_cache_is_bounded() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    for i in 0..50 {
        clock.set(ANCHOR + i * HOUR);
        ctx.treatment("exp_sb").unwrap();
    }
    assert_eq!(cached_slots(&ctx), 2);
    assert_eq!(ctx.pending(), 50);
}

#[test]
fn unit_type_is_not_used_for_switchback_assignment() {
    let mut experiment = sb(2, HOUR);
    experiment["unitType"] = json!("user_id");
    let clock = Clock::new(ANCHOR);
    let mut ctx = context_with(
        &clock,
        &[(UNIT_TYPE, UID), ("user_id", "u1")],
        data_of(vec![experiment]),
    );
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(
            ctx.treatment("exp_sb").unwrap(),
            EXPECTED_2_ARMS[i as usize]
        );
    }
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 3);
    assert!(exposures.iter().all(|e| e.2.as_deref() == Some(UNIT_TYPE)));
}

// The publish payload has no switchback fields, and the cluster attribute is
// published with its timestamp.
#[test]
fn publish_payload_has_no_new_fields() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    clock.set(ANCHOR + 5);
    ctx.treatment("exp_sb").unwrap();
    clock.set(ANCHOR + HOUR + 7);
    ctx.treatment("exp_sb").unwrap();
    clock.set(ANCHOR + HOUR + 9);
    let exposure_json = |variant: i32, exposed_at: i64| {
        json!({
            "id": 1, "name": "exp_sb", "exposedAt": exposed_at, "unit": "store_id",
            "variant": variant, "assigned": true, "eligible": true, "overridden": false,
            "fullOn": false, "custom": false, "audienceMismatch": false, "ruleOverride": false
        })
    };
    assert_eq!(
        serde_json::to_value(ctx.get_publish_params()).unwrap(),
        json!({
            "publishedAt": ANCHOR + HOUR + 9,
            "units": [{"type": "store_id", "uid": UID_HASH}],
            "hashed": true,
            "exposures": [exposure_json(EXPECTED_2_ARMS[0], ANCHOR + 5), exposure_json(EXPECTED_2_ARMS[1], ANCHOR + HOUR + 7)],
            "attributes": [{"name": CLUSTER_PATH, "value": CLUSTER_KEY, "setAt": ANCHOR}]
        })
    );
    assert_ne!(EXPECTED_2_ARMS[0], EXPECTED_2_ARMS[1]);
}

#[test]
fn periods_published_across_flushes_stay_one_per_period() {
    let published = Arc::new(Mutex::new(Vec::new()));
    struct Capture(Arc<Mutex<Vec<PublishParams>>>);
    impl ContextPublisher for Capture {
        fn publish(&self, params: &PublishParams) -> Result<(), String> {
            self.0.lock().unwrap().push(params.clone());
            Ok(())
        }
    }
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    ctx.set_publisher(Box::new(Capture(published.clone())));
    ctx.treatment("exp_sb").unwrap();
    ctx.publish().unwrap();
    clock.set(ANCHOR + 1);
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 0);
    clock.set(ANCHOR + HOUR);
    ctx.treatment("exp_sb").unwrap();
    ctx.publish().unwrap();

    let published = published.lock().unwrap();
    assert_eq!(published.len(), 2);
    let variants: Vec<i32> = published
        .iter()
        .flat_map(|p| p.exposures.clone().unwrap_or_default())
        .map(|e| e.variant)
        .collect();
    assert_eq!(variants, vec![EXPECTED_2_ARMS[0], EXPECTED_2_ARMS[1]]);
}

// The clock crosses into period 1 between resolution and any later read;
// the exposure still carries the instant that selected period 0.
#[test]
fn treatment_exposure_is_stamped_with_the_resolution_instant() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    clock.set(ANCHOR + HOUR);
    clock.next_read(ANCHOR + HOUR - 1);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            EXPECTED_2_ARMS[0],
            ANCHOR + HOUR - 1,
            true,
            true
        )]
    );
}

#[test]
fn variable_exposure_is_stamped_with_the_resolution_instant() {
    let mut covered = sb(2, HOUR);
    covered["holdoutIds"] = json!([11]);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(
        &clock,
        data_with_holdouts(vec![covered], vec![holdout(json!([0.0, 1.0]))]),
    );
    clock.set(ANCHOR + HOUR);
    clock.next_read(ANCHOR + HOUR - 1);
    assert_eq!(
        ctx.variable_value("sb.arm", -1).unwrap(),
        json!(EXPECTED_2_ARMS[0])
    );
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            EXPECTED_2_ARMS[0],
            ANCHOR + HOUR - 1,
            true,
            true
        )]
    );
}

// A peek caches the period without exposing; the later treatment exposes
// at its own reading.
#[test]
fn exposure_after_peek_uses_the_exposing_calls_instant() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    clock.set(ANCHOR + 10);
    assert_eq!(ctx.peek("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    clock.set(ANCHOR + 20 * 60_000);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            EXPECTED_2_ARMS[0],
            ANCHOR + 20 * 60_000,
            true,
            true
        )]
    );
}

// Unit experiments keep stamping at enqueue time, and read the clock only
// then.
#[test]
fn unit_exposure_timing_is_unchanged() {
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![unit]));
    clock.set(ANCHOR + HOUR);
    clock.next_read(ANCHOR + HOUR - 1);
    let variant = ctx.treatment("exp_sb").unwrap();
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            variant,
            ANCHOR + HOUR - 1,
            true,
            true
        )]
    );
}

// A grid change that lowers the current index below the retained ones must
// not evict the entry it just installed.
#[test]
fn grid_refresh_to_a_lower_index_exposes_once() {
    let clock = Clock::new(ANCHOR + 99 * HOUR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    clock.set(ANCHOR + 100 * HOUR);
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 2);

    let mut restarted = sb(2, HOUR);
    restarted["switchbackStartAt"] = json!(ANCHOR + 100 * HOUR);
    ctx.refresh_with(data_of(vec![restarted]));
    for _ in 0..3 {
        assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    }
    assert_eq!(ctx.pending(), 3);
}

// An anchor change that keeps the period index is still a new epoch.
#[test]
fn anchor_change_with_the_same_period_index_reexposes() {
    let clock = Clock::new(ANCHOR + HOUR + 10);
    let mut ctx = context(&clock, data_of(vec![sb(2, 2 * HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    let mut moved = sb(2, 2 * HOUR);
    moved["switchbackStartAt"] = json!(ANCHOR + HOUR);
    ctx.refresh_with(data_of(vec![moved]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(
                1,
                "exp_sb",
                EXPECTED_2_ARMS[0],
                ANCHOR + HOUR + 10,
                true,
                true
            ),
            exposure(
                1,
                "exp_sb",
                EXPECTED_2_ARMS[0],
                ANCHOR + HOUR + 10,
                true,
                true
            ),
        ]
    );
}

// An exposed override resolved before any linked unit was set keeps its
// slot; when the override changes, the slot reads the serving unit again.
#[test]
fn changed_override_rereads_a_serving_unit_set_after_an_absent_resolution() {
    let mut experiment = sb(2, HOUR);
    experiment["switchbackClusters"] =
        ordered_clusters(&[("device_id", "geo/city"), (UNIT_TYPE, CLUSTER_PATH)]);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = bare_context(&clock, &[], parse_experiments(&[experiment]));
    ctx.set_attribute(CLUSTER_PATH, CLUSTER_KEY).unwrap();
    ctx.set_override("exp_sb", 1).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
    ctx.set_unit(UNIT_TYPE, UID).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
    ctx.set_override("exp_sb", 0).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![
            seen(
                1,
                "exp_sb",
                "device_id",
                1,
                ANCHOR + 10,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
            seen(
                1,
                "exp_sb",
                UNIT_TYPE,
                0,
                ANCHOR + 10,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
        ]
    );
}

#[test]
fn period_length_refresh_to_a_lower_index_exposes_once() {
    let clock = Clock::new(ANCHOR + 100 * HOUR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    clock.set(ANCHOR + 101 * HOUR);
    ctx.treatment("exp_sb").unwrap();

    ctx.refresh_with(data_of(vec![sb(2, 200 * HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 3);
}

// Rewinding the clock re-exposes each re-entered evicted period at most once.
#[test]
fn clock_rewind_reexposes_an_evicted_period_at_most_once() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        ctx.treatment("exp_sb").unwrap();
    }
    assert_eq!(ctx.pending(), 3);

    clock.set(ANCHOR);
    for _ in 0..3 {
        assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    }
    assert_eq!(ctx.pending(), 4);

    // Retained {0, 2}: period 2 is still deduplicated, period 1 was evicted.
    clock.set(ANCHOR + 2 * HOUR);
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 4);
    clock.set(ANCHOR + HOUR);
    for _ in 0..3 {
        assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    }
    assert_eq!(ctx.pending(), 5);

    let exposures = queued(&mut ctx);
    assert_eq!(
        exposures[3],
        exposure(1, "exp_sb", EXPECTED_2_ARMS[0], ANCHOR, true, true)
    );
    assert_eq!(
        exposures[4],
        exposure(1, "exp_sb", EXPECTED_2_ARMS[1], ANCHOR + HOUR, true, true)
    );
}

// An overridden switchback slot is re-resolved on a grid change; the
// override still wins and the holdout never fires.
#[test]
fn override_is_reresolved_when_the_grid_changes() {
    let mut original = sb(3, HOUR);
    original["holdoutIds"] = json!([11]);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(
        &clock,
        data_with_holdouts(vec![original], vec![holdout(json!([0.0, 1.0]))]),
    );
    ctx.set_override("exp_sb", 2).unwrap();

    clock.set(ANCHOR + 10);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(2));
    assert_eq!(ctx.pending(), 1);

    let mut regridded = sb(3, 2 * HOUR);
    regridded["holdoutIds"] = json!([11]);
    regridded["variants"][2]["config"] = json!(r#"{"sb.arm":22}"#);
    ctx.refresh_with(data_with_holdouts(
        vec![regridded],
        vec![holdout(json!([0.0, 1.0]))],
    ));

    assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(22));
    assert_eq!(
        queued(&mut ctx),
        vec![
            seen(
                1,
                "exp_sb",
                UNIT_TYPE,
                2,
                ANCHOR + 10,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
            seen(
                1,
                "exp_sb",
                UNIT_TYPE,
                2,
                ANCHOR + 10,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
        ]
    );
}

#[test]
fn switchback_to_unit_to_identical_switchback_reresolves() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);

    let mut to_unit = sb(2, HOUR);
    to_unit["randomizationType"] = json!("unit");
    ctx.refresh_with(data_of(vec![to_unit]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 2);

    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 3);
}

// Each transition starts again, as in java-sdk: the unit assignment goes when
// the experiment becomes a switchback, and the switchback's slots go when it
// is a unit experiment again, which is exposed again.
#[test]
fn unit_to_switchback_to_identical_unit_reexposes() {
    let clock = Clock::new(ANCHOR + HOUR + 10);
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let mut ctx = context(&clock, data_of(vec![unit.clone()]));
    let variant = ctx.treatment("exp_sb").unwrap();
    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    clock.set(ANCHOR + 2 * HOUR);
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.switchbacks["exp_sb"].periods.len(), 2);
    ctx.refresh_with(data_of(vec![unit]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), variant);
    assert_eq!(ctx.pending(), 4);
    assert_eq!(cached_slots(&ctx), 1);
}

// The traffic miss wins over a custom assignment (scenario 416).
#[test]
fn traffic_miss_wins_over_a_custom_assignment() {
    let mut experiment = sb(3, HOUR);
    experiment["trafficSplit"] = json!([1.0, 0.0]);
    let clock = Clock::new(ANCHOR + 2 * HOUR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    ctx.set_custom_assignment("exp_sb", 2).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(1, "exp_sb", 0, ANCHOR + 2 * HOUR, true, false)]
    );
}

#[test]
fn removed_and_restored_switchback_reresolves() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();

    ctx.refresh_with(data_of(vec![]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 2);

    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 3);
}

// The pre-anchor slot shares the lifecycle of the period slots.
#[test]
fn pre_anchor_slot_is_dropped_when_the_grid_changes() {
    let mut grid_a = sb(2, HOUR);
    grid_a["switchbackStartAt"] = json!(ANCHOR + 10 * HOUR);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![grid_a.clone()]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);

    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 1);

    ctx.refresh_with(data_of(vec![grid_a]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 1);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
    assert_eq!(ctx.pending(), 1);
}

// Epochs are reconciled lazily; an intermediate state that is never looked
// up still ends the epoch.
#[test]
fn transitions_without_an_intervening_lookup_still_reresolve() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 1);

    let mut to_unit = sb(2, HOUR);
    to_unit["randomizationType"] = json!("unit");
    ctx.refresh_with(data_of(vec![to_unit]));
    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 2);

    ctx.refresh_with(data_of(vec![]));
    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 3);

    let mut regridded = sb(2, HOUR);
    regridded["switchbackDesignType"] = json!("other");
    ctx.refresh_with(data_of(vec![regridded]));
    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[0]);
    assert_eq!(ctx.pending(), 4);

    // An unchanged identity keeps the epoch.
    ctx.refresh_with(data_of(vec![sb(2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.pending(), 4);
}

// A legacy switchback serves under its declared unitType, so a unitType
// change is identity for it.
#[test]
fn legacy_switchback_unit_type_change_is_identity() {
    let mut legacy = sb(2, HOUR);
    legacy["switchbackClusters"] = Value::Null;
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context_with(
        &clock,
        &[(UNIT_TYPE, UID), ("user_id", "u1")],
        data_of(vec![legacy.clone()]),
    );
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
    assert_eq!(
        resolution(&ctx, "exp_sb", ANCHOR + 10)
            .unwrap()
            .1
            .as_deref(),
        Some(UNIT_TYPE)
    );

    let mut other = legacy.clone();
    other["unitType"] = json!("other");
    ctx.refresh_with(data_of(vec![other]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(-1));

    let mut user = legacy;
    user["unitType"] = json!("user_id");
    ctx.refresh_with(data_of(vec![user]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
    let (serving, unit, eligible, silent) = resolution(&ctx, "exp_sb", ANCHOR + 10).unwrap();
    assert_eq!(
        serving,
        Serving::Ineligible {
            unit_type: "user_id".into()
        }
    );
    assert_eq!(unit.as_deref(), Some("user_id"));
    assert!(!eligible && silent);
    assert_eq!(ctx.pending(), 0);
}

/// Counts rule evaluations through an audience matcher that sees them.
fn rule_evaluations(ctx: &Context) -> usize {
    ctx.rule_evaluations
}

// An ineligible or absent slot never consulted its rules, so neither an
// attribute nor a rules refresh evaluates them.
#[test]
fn rules_are_not_reevaluated_for_an_ineligible_or_absent_slot() {
    let mut ineligible = switchback(1, "exp_ineligible", 2, HOUR);
    ineligible["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    ineligible["switchbackClusterValues"] = json!(["other-store"]);
    let mut absent = switchback(2, "exp_absent", 2, HOUR);
    absent["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    absent["switchbackClusters"] = json!({"device_id": CLUSTER_PATH});
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![ineligible.clone(), absent.clone()]));
    assert_eq!(ctx.treatment("exp_ineligible").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_absent").unwrap(), 0);
    assert_eq!(rule_evaluations(&ctx), 0);

    ctx.set_attribute("x", 1).unwrap();
    let changed = r#"{"rules":[{"type":"assign","environments":[],"variant":1,"name":"changed"}]}"#;
    ineligible["assignmentRules"] = json!(changed);
    absent["assignmentRules"] = json!(changed);
    ctx.refresh_with(data_of(vec![ineligible, absent]));

    assert_eq!(ctx.treatment("exp_ineligible").unwrap(), 0);
    assert_eq!(ctx.treatment("exp_absent").unwrap(), 0);
    assert_eq!(rule_evaluations(&ctx), 0);
    assert_eq!(ctx.pending(), 0);
}

// A rule-won switchback re-resolved in the same period keeps its variant and
// is exposed once.
#[test]
fn in_period_rule_refresh_keeps_the_variant_and_exposes_once() {
    let mut experiment = sb(2, HOUR);
    experiment["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    let clock = Clock::new(ANCHOR + 5);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);

    ctx.set_attribute("tier", "x").unwrap();
    let mut refreshed = sb(2, HOUR);
    refreshed["assignmentRules"] = json!(
        r#"{"rules":[{"type":"assign","environments":[],"variant":1,"conditions":{"and":[{"eq":[{"var":"tier"},{"value":"x"}]}]}}]}"#
    );
    ctx.refresh_with(data_of(vec![refreshed]));
    clock.set(ANCHOR + 6);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
    assert_eq!(
        queued(&mut ctx),
        vec![rule_exposure("exp_sb", 1, ANCHOR + 5)]
    );
}

// Before the anchor nothing is recorded, a traffic miss included; from the
// anchor the same miss is recorded.
#[test]
fn pre_anchor_traffic_miss_is_silent() {
    let mut experiment = sb(2, HOUR);
    experiment["trafficSplit"] = json!([1.0, 0.0]);
    let clock = Clock::new(ANCHOR - 1);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);

    clock.set(ANCHOR);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(1, "exp_sb", 0, ANCHOR, true, false)]
    );
}

// An override with no serving unit on a legacy switchback is exposed under
// the declared unitType.
#[test]
fn override_without_a_serving_unit_on_a_legacy_switchback_uses_the_declared_unit_type() {
    let mut missing = switchback(1, "exp_missing_map", 2, HOUR);
    missing["unitType"] = json!("user_id");
    missing["switchbackClusters"] = Value::Null;
    let mut unreadable = switchback(2, "exp_unreadable_map", 2, HOUR);
    unreadable["unitType"] = json!("user_id");
    unreadable["switchbackClusters"] = json!({UNIT_TYPE: "'unterminated"});
    let clock = Clock::new(ANCHOR + 5);
    let mut ctx = context(&clock, data_of(vec![missing, unreadable]));
    ctx.set_override("exp_missing_map", 1).unwrap();
    ctx.set_override("exp_unreadable_map", 1).unwrap();
    assert_eq!(ctx.treatment("exp_missing_map").unwrap(), 1);
    assert_eq!(ctx.treatment("exp_unreadable_map").unwrap(), 1);
    assert_eq!(
        queued(&mut ctx),
        vec![
            seen(
                1,
                "exp_missing_map",
                "user_id",
                1,
                ANCHOR + 5,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
            seen(
                2,
                "exp_unreadable_map",
                "user_id",
                1,
                ANCHOR + 5,
                false,
                true,
                true,
                false,
                false,
                false,
                false
            ),
        ]
    );
}

#[test]
fn rule_won_and_traffic_miss_exposures_serialize_exactly() {
    let mut rule = switchback(1, "exp_rule", 2, HOUR);
    rule["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    let mut miss = switchback(2, "exp_miss", 2, HOUR);
    miss["trafficSplit"] = json!([1.0, 0.0]);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![rule, miss]));
    clock.set(ANCHOR + 5);
    ctx.treatment("exp_rule").unwrap();
    ctx.treatment("exp_miss").unwrap();
    clock.set(ANCHOR + 9);
    assert_eq!(
        serde_json::to_value(ctx.get_publish_params()).unwrap(),
        json!({
            "publishedAt": ANCHOR + 9,
            "units": [{"type": "store_id", "uid": UID_HASH}],
            "hashed": true,
            "exposures": [
                {"id": 1, "name": "exp_rule", "exposedAt": ANCHOR + 5, "unit": "store_id", "variant": 1,
                 "assigned": false, "eligible": true, "overridden": false, "fullOn": false, "custom": false,
                 "audienceMismatch": false, "ruleOverride": true},
                {"id": 2, "name": "exp_miss", "exposedAt": ANCHOR + 5, "unit": "store_id", "variant": 0,
                 "assigned": true, "eligible": false, "overridden": false, "fullOn": false, "custom": false,
                 "audienceMismatch": false, "ruleOverride": false}
            ],
            "attributes": [{"name": CLUSTER_PATH, "value": CLUSTER_KEY, "setAt": ANCHOR}]
        })
    );
}

// The single-threaded sequence of Java's
// concurrentResolutionWithConcurrentSetAttributeExposesOnce: an audience
// change rebuilds the period slot but keeps the key it first read.
#[test]
fn audience_change_rebuild_keeps_the_period_key_and_exposes_once() {
    let mut experiment = sb(2, HOUR);
    experiment["audience"] = json!(r#"{"filter":[{"value":true}]}"#);
    experiment["switchbackClusters"] = json!({UNIT_TYPE: "city"});
    let clock = Clock::new(ANCHOR + HOUR + 10);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    let lisboa = assign_switchback("Lisboa", 1, SEED, 2).unwrap() as i32;
    let faro = assign_switchback("Faro", 1, SEED, 2).unwrap() as i32;
    assert_ne!(lisboa, faro);

    assert_eq!(ctx.peek("exp_sb").unwrap(), lisboa);
    ctx.set_attribute("country", "PT").unwrap();
    ctx.set_attribute("city", "Faro").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), lisboa);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), lisboa);
    assert_eq!(ctx.pending(), 1);
}

// ---- ContextSwitchbackClusterTest ----

const ARMS: usize = 3;

fn attribute_switchback(path: &str) -> Value {
    let mut experiment = sb(ARMS, HOUR);
    experiment["switchbackClusters"] = json!({ UNIT_TYPE: path });
    experiment
}

fn expected(key: &str, period: i64) -> i32 {
    assign_switchback(key, period, SEED, ARMS).unwrap() as i32
}

fn expected_schedule(key: &str) -> Vec<i32> {
    (0..12).map(|i| expected(key, i)).collect()
}

fn store_context(clock: &Clock, data: ContextData) -> Context {
    bare_context(clock, &[(UNIT_TYPE, json!(UID))], data)
}

#[test]
fn unit_uid_is_never_the_cluster_key() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    for i in 0..3 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    }
    assert_eq!(ctx.pending(), 0);
}

// A numeric uid serves and publishes like its canonical string; the cluster
// key is the attribute.
#[test]
fn numeric_unit_uid_clusters_like_its_canonical_string() {
    for (numeric, text) in [
        (json!(123456789), "123456789"),
        (json!(123456789.0), "123456789"),
        (json!(125.75), "126"),
    ] {
        let publish = |uid: Value| {
            let clock = Clock::new(ANCHOR);
            let mut ctx = bare_context(
                &clock,
                &[(UNIT_TYPE, uid)],
                data_of(vec![attribute_switchback("city")]),
            );
            ctx.set_attribute("city", "Lisboa").unwrap();
            ctx.treatment("exp_sb").unwrap();
            let params = ctx.get_publish_params();
            let schedule = peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12);
            (
                serde_json::to_value(params.units).unwrap(),
                queued(&mut ctx),
                schedule,
            )
        };
        let (numeric_units, numeric_exposures, numeric_schedule) = publish(numeric);
        let (text_units, text_exposures, text_schedule) = publish(json!(text));
        assert_eq!(numeric_schedule, expected_schedule("Lisboa"));
        assert_eq!(numeric_schedule, text_schedule);
        assert_eq!(numeric_units, text_units);
        assert_eq!(numeric_exposures, text_exposures);
        assert_eq!(
            numeric_units[0]["uid"],
            json!(crate::utils::hash_unit(text))
        );
    }
}

// Java's independent reference for the raw key "123456789" as an integer
// attribute value.
#[test]
fn numeric_unit_uid_matches_the_independent_reference() {
    let schedule = [0, 2, 1, 0, 1, 2, 0, 1, 2, 2, 1, 0];
    let clock = Clock::new(ANCHOR);
    let mut ctx = bare_context(
        &clock,
        &[(UNIT_TYPE, json!(123456789))],
        data_of(vec![attribute_switchback("region")]),
    );
    ctx.set_attribute("region", 123456789).unwrap();
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(
        ctx.get_publish_params().units[0].uid.as_deref(),
        Some("JfnnlDI7RTiF9RgfG2JNCw")
    );
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(1, "exp_sb", schedule[0], ANCHOR, true, true)]
    );
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12),
        schedule
    );
}

#[test]
fn attribute_source_serves_the_raw_attribute_value() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12),
        [0, 2, 1, 2, 1, 0, 1, 0, 2, 0, 1, 2]
    );
    assert_eq!(
        peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12),
        expected_schedule("Lisboa")
    );
}

#[test]
fn units_sharing_an_attribute_value_share_the_schedule() {
    let clock = Clock::new(ANCHOR);
    let schedule = |uid: &str, city: &str| {
        let mut ctx = bare_context(
            &clock,
            &[(UNIT_TYPE, json!(uid))],
            data_of(vec![attribute_switchback("city")]),
        );
        clock.set(ANCHOR);
        ctx.set_attribute("city", city).unwrap();
        peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12)
    };
    assert_eq!(schedule(UID, "Lisboa"), expected_schedule("Lisboa"));
    assert_eq!(
        schedule("another-store", "Lisboa"),
        expected_schedule("Lisboa")
    );
    assert_eq!(schedule(UID, "Porto"), expected_schedule("Porto"));
    assert_ne!(expected_schedule("Lisboa"), expected_schedule("Porto"));
}

#[test]
fn nested_and_array_paths_and_integer_values_resolve() {
    let clock = Clock::new(ANCHOR);
    for (path, name, value, key) in [
        ("geo/city", "geo", json!({"city": "Porto"}), "Porto"),
        (
            "items/1/city",
            "items",
            json!([{"city": "Lisbon"}, {"city": "Faro"}]),
            "Faro",
        ),
        ("region", "region", json!(42), "42"),
        ("geo/city", "geo", json!({"city": 42}), "42"),
    ] {
        clock.set(ANCHOR);
        let mut ctx = store_context(&clock, data_of(vec![attribute_switchback(path)]));
        ctx.set_attribute(name, value).unwrap();
        assert_eq!(
            peek_schedule(&mut ctx, &clock, "exp_sb", HOUR, 12),
            expected_schedule(key),
            "{}",
            path
        );
    }
}

// Values that give no cluster key serve variant 0 with no exposure. 3.0 is
// a float, so it gives none even though it is whole.
#[test]
fn values_without_a_cluster_key_are_ineligible() {
    for value in [
        Value::Null,
        json!(""),
        json!(true),
        json!(1.5),
        json!(2.0),
        json!(3.0),
        json!({"a": "b"}),
        json!(["x"]),
    ] {
        let clock = Clock::new(ANCHOR);
        let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
        ctx.set_attribute("city", value.clone()).unwrap();
        clock.set(ANCHOR + 10);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0, "{}", value);
        assert_eq!(
            ctx.variable_value("sb.arm", -1).unwrap(),
            json!(0),
            "{}",
            value
        );
        assert_eq!(ctx.pending(), 0, "{}", value);
    }

    let clock = Clock::new(ANCHOR);
    let mut missing = store_context(&clock, data_of(vec![attribute_switchback("geo/city")]));
    missing
        .set_attribute("geo", json!({"country": "PT"}))
        .unwrap();
    clock.set(ANCHOR + 10);
    assert_eq!(missing.treatment("exp_sb").unwrap(), 0);
    assert_eq!(missing.pending(), 0);
}

// Attribute values from JSON text keep the integer/float distinction: `3`
// keys the cluster "3", `3.0` gives none.
#[test]
fn integer_and_float_json_values_are_told_apart() {
    for (text, key) in [
        ("3", Some("3")),
        ("3.0", None),
        ("4000000002", Some("4000000002")),
        ("1e0", None),
    ] {
        let value: Value = serde_json::from_str(text).unwrap();
        let clock = Clock::new(ANCHOR);
        let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
        ctx.set_attribute("city", value).unwrap();
        let expected_variant = key.map_or(0, |k| expected(k, 0));
        assert_eq!(
            ctx.treatment("exp_sb").unwrap(),
            expected_variant,
            "{}",
            text
        );
        assert_eq!(ctx.pending(), usize::from(key.is_some()), "{}", text);
    }
}

#[test]
fn unservable_configurations_are_ineligible() {
    let mut experiments: Vec<Value> = [
        "items/0",
        "items/-1/city",
        "",
        "geo.city",
        "'city",
        "\"city",
    ]
    .iter()
    .map(|path| attribute_switchback(path))
    .collect();
    for (field, value) in [
        ("switchbackPeriodMs", Value::Null),
        ("switchbackPeriodMs", json!(0)),
        ("switchbackPeriodMs", json!(-HOUR)),
        ("split", json!([1.0])),
        ("switchbackClusters", json!({UNIT_TYPE: null})),
    ] {
        let mut experiment = attribute_switchback("city");
        experiment[field] = value;
        experiments.push(experiment);
    }
    for experiment in experiments {
        let clock = Clock::new(ANCHOR);
        let mut ctx = store_context(&clock, data_of(vec![experiment.clone()]));
        ctx.set_attribute("city", "Lisboa").unwrap();
        ctx.set_attribute("items", json!([{"city": "Lisbon"}]))
            .unwrap();
        for i in 0..3 {
            clock.set(ANCHOR + i * HOUR);
            assert_eq!(ctx.treatment("exp_sb").unwrap(), 0, "{}", experiment);
        }
        assert_eq!(ctx.pending(), 0, "{}", experiment);
    }
}

#[test]
fn missing_or_empty_cluster_map_is_only_a_legacy_ineligible_control() {
    for clusters in [Value::Null, json!({})] {
        let mut experiment = attribute_switchback("city");
        experiment["switchbackClusters"] = clusters;
        let clock = Clock::new(ANCHOR);
        let mut ctx = store_context(&clock, data_of(vec![experiment.clone()]));
        ctx.set_attribute("city", "Lisboa").unwrap();
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
        assert_eq!(ctx.variable_value("sb.arm", -1).unwrap(), json!(0));
        assert_eq!(ctx.pending(), 0);

        let mut other = bare_context(
            &clock,
            &[("user_id", json!("u1"))],
            data_of(vec![experiment]),
        );
        other.set_attribute("city", "Lisboa").unwrap();
        assert_eq!(other.treatment("exp_sb").unwrap(), 0);
        assert_eq!(other.variable_value("sb.arm", -1).unwrap(), json!(-1));
        assert_eq!(other.pending(), 0);
    }
}

#[test]
fn latest_attribute_set_at_or_before_the_resolution_instant_is_used() {
    let clock = Clock::new(ANCHOR);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    clock.set(ANCHOR + 2 * HOUR);
    ctx.set_attribute("city", "Porto").unwrap();
    clock.set(ANCHOR + 4 * HOUR);
    ctx.set_attribute("city", "Lisboa").unwrap();

    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.peek("exp_sb").unwrap(), 0);
    clock.set(ANCHOR + 3 * HOUR);
    assert_eq!(ctx.peek("exp_sb").unwrap(), expected("Porto", 3));
    clock.set(ANCHOR + 4 * HOUR);
    assert_eq!(ctx.peek("exp_sb").unwrap(), expected("Lisboa", 4));
}

#[test]
fn set_attribute_mid_period_does_not_reassign_within_the_period() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));

    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Porto").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(ctx.peek("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(ctx.pending(), 1);

    let mut differing = 0;
    for i in 1..12 {
        clock.set(ANCHOR + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Porto", i));
        if expected("Porto", i) != expected("Lisboa", i) {
            differing += 1;
        }
    }
    assert_ne!(differing, 0);
    assert_eq!(ctx.pending(), 12);
}

#[test]
fn attribute_set_after_an_ineligible_resolution_waits_for_the_next_period() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    ctx.set_attribute("city", "Lisboa").unwrap();
    clock.set(ANCHOR + 20);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);

    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 1));
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            1,
            "exp_sb",
            expected("Lisboa", 1),
            ANCHOR + HOUR,
            true,
            true
        )]
    );
}

// A slot replaced through a traffic change keeps the period's first key.
#[test]
fn traffic_changes_within_the_period_keep_the_periods_bucket() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));

    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Porto").unwrap();
    let mut no_traffic = attribute_switchback("city");
    no_traffic["trafficSplit"] = json!([1.0, 0.0]);
    ctx.refresh_with(data_of(vec![no_traffic]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);

    ctx.refresh_with(data_of(vec![attribute_switchback("city")]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(ctx.pending(), 3);

    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Porto", 1));
}

#[test]
fn traffic_restored_within_the_period_keeps_the_key() {
    let mut no_traffic = attribute_switchback("city");
    no_traffic["trafficSplit"] = json!([1.0, 0.0]);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![no_traffic]));
    ctx.set_attribute("city", "Faro").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);

    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.refresh_with(data_of(vec![attribute_switchback("city")]));
    assert_eq!(expected("Faro", 0), 1);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Faro", 0));
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(1, "exp_sb", 0, ANCHOR + 10, true, false),
            // At ANCHOR + 20 the city is Lisboa, so the exposure is stamped
            // when Faro was read (java-sdk stamps ANCHOR + 20).
            exposure(1, "exp_sb", expected("Faro", 0), ANCHOR + 10, true, true),
        ]
    );
}

// An audience change re-resolves the slot (new exposure) but keeps its key.
// The city changed since it was read, so the new exposure is stamped when
// Lisboa was read (java-sdk stamps the later call).
#[test]
fn audience_revalidation_after_set_attribute_keeps_the_periods_bucket() {
    let mut experiment = attribute_switchback("city");
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![experiment]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.set_attribute("tier", "gold").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));

    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Porto").unwrap();
    ctx.set_attribute("tier", "silver").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(1, "exp_sb", expected("Lisboa", 0), ANCHOR + 10, true, true),
            seen(
                1,
                "exp_sb",
                UNIT_TYPE,
                expected("Lisboa", 0),
                ANCHOR + 10,
                true,
                true,
                false,
                false,
                false,
                true,
                false
            ),
        ]
    );
}

#[test]
fn audience_revalidation_after_an_ineligible_resolution_keeps_the_missing_key() {
    let mut experiment = attribute_switchback("city");
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![experiment]));
    ctx.set_attribute("tier", "gold").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);

    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Faro").unwrap();
    ctx.set_attribute("tier", "silver").unwrap();
    assert_eq!(expected("Faro", 0), 1);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);

    clock.set(ANCHOR + HOUR);
    assert_eq!(ctx.peek("exp_sb").unwrap(), expected("Faro", 1));
    clock.set(ANCHOR + 2 * HOUR);
    assert_eq!(expected("Faro", 2), 2);
    assert_eq!(ctx.peek("exp_sb").unwrap(), expected("Faro", 2));
}

#[test]
fn cluster_path_changes_invalidate_the_period() {
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.set_attribute("geo", json!({"city": "Porto"})).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(ctx.pending(), 1);

    ctx.refresh_with(data_of(vec![attribute_switchback("city")]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));
    assert_eq!(ctx.pending(), 1);

    ctx.refresh_with(data_of(vec![attribute_switchback("geo/city")]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Porto", 0));
    assert_eq!(ctx.pending(), 2);

    let mut filtered = attribute_switchback("geo/city");
    filtered["switchbackClusterValues"] = json!(["Porto", "Faro"]);
    ctx.refresh_with(data_of(vec![filtered]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Porto", 0));
    assert_eq!(ctx.pending(), 3);
}

#[test]
fn linked_unit_type_change_rekeys_the_period() {
    let clock = Clock::new(ANCHOR + 1);
    let mut ctx = bare_context(
        &clock,
        &[(UNIT_TYPE, json!(UID)), ("user_id", json!("user-0002"))],
        data_of(vec![attribute_switchback("city")]),
    );
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.set_attribute("ucity", "Faro").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Lisboa", 0));

    let mut by_user = attribute_switchback("ucity");
    by_user["switchbackClusters"] = json!({"user_id": "ucity"});
    ctx.refresh_with(data_of(vec![by_user]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), expected("Faro", 0));
    let exposures = queued(&mut ctx);
    assert_eq!(exposures.len(), 2);
    assert_eq!(exposures[0].2.as_deref(), Some(UNIT_TYPE));
    assert_eq!(exposures[1].2.as_deref(), Some("user_id"));
}

#[test]
fn custom_assignment_applies_only_to_units_with_a_cluster_key() {
    let clock = Clock::new(ANCHOR);
    let mut keyed = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    keyed.set_attribute("city", "Lisboa").unwrap();
    keyed.set_custom_assignment("exp_sb", 2).unwrap();
    assert_eq!(keyed.treatment("exp_sb").unwrap(), 2);

    let mut unkeyed = store_context(&clock, data_of(vec![attribute_switchback("city")]));
    unkeyed.set_custom_assignment("exp_sb", 2).unwrap();
    assert_eq!(unkeyed.treatment("exp_sb").unwrap(), 0);
}

// A custom assignment set after a period was resolved re-resolves it.
#[test]
fn custom_assignment_set_mid_period_reresolves_the_slot() {
    let clock = Clock::new(ANCHOR + 2 * HOUR);
    let mut ctx = context(&clock, data_of(vec![sb(3, HOUR)]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_3_ARMS[2]);
    ctx.set_custom_assignment("exp_sb", 2).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    assert_eq!(ctx.pending(), 2);
}

// Inertness: a unit experiment carrying every switchback field serves and
// publishes exactly what the same experiment without them does.
#[test]
fn unit_experiments_ignore_cluster_fields() {
    for kind in [Some("unit"), None] {
        let mut unit = attribute_switchback("city");
        match kind {
            Some(kind) => unit["randomizationType"] = json!(kind),
            None => {
                unit.as_object_mut().unwrap().remove("randomizationType");
            }
        }
        unit["switchbackClusters"] = json!({"user_id": "city", UNIT_TYPE: "'x"});
        unit["switchbackClusterValues"] = json!(["nothing"]);
        unit["switchbackPeriodMs"] = json!(0);
        unit["assignmentRules"] =
            json!(r#"{"rules":[{"type":"split","environments":[],"percentages":"34/33/33"}]}"#);
        let mut reference = unit.clone();
        for key in [
            "switchbackClusters",
            "switchbackClusterValues",
            "switchbackStartAt",
            "switchbackPeriodMs",
            "switchbackCarryoverDurationMs",
            "switchbackBurnInDurationMs",
            "switchbackBurnOutDurationMs",
            "switchbackDesignType",
            "switchbackEstimand",
        ] {
            reference.as_object_mut().unwrap().remove(key);
        }

        let mut published = Vec::new();
        for experiment in [unit, reference] {
            let clock = Clock::new(ANCHOR - HOUR);
            let mut ctx = bare_context(
                &clock,
                &[(UNIT_TYPE, json!(UID)), ("user_id", json!("u"))],
                data_of(vec![experiment]),
            );
            ctx.set_attribute("city", "Lisboa").unwrap();
            let first = ctx.treatment("exp_sb").unwrap();
            for i in 1..6 {
                clock.set(ANCHOR - HOUR + i * HOUR);
                assert_eq!(ctx.treatment("exp_sb").unwrap(), first);
            }
            published.push(queued(&mut ctx));
        }
        assert_eq!(published[0].len(), 1);
        assert!(published[0][0].5[1] && published[0][0].5[6]);
        assert_eq!(published[0][0].4, ANCHOR - HOUR);
        assert_eq!(published[0], published[1]);
    }
}

// ---- ContextSwitchbackClusterRuleTest (the Collector's ClusterRule section) ----

const RULE_SEED_HI: i64 = 0x1234;
const RULE_SEED_LO: i64 = 0x5678abcd;
const RULE_SEED: i64 = (RULE_SEED_HI << 32) | (RULE_SEED_LO & 0xffff_ffff);
const PERIOD: i64 = 5;
const NOW: i64 = ANCHOR + PERIOD * HOUR + 17 * 60_000;
const SESSION: &str = "session_id";
const USER: &str = "user_id";

/// "sbc" (id 900, "34/33/33", 1h), linked in declared order.
fn sbc(clusters: Value) -> Value {
    let variants: Vec<Value> = (0..3)
        .map(|i| json!({"name": i.to_string(), "config": json!({"sbc.config": format!("test config {}", i)}).to_string()}))
        .collect();
    json!({
        "id": 900, "name": "sbc", "unitType": USER, "iteration": 1,
        "seedHi": RULE_SEED_HI, "seedLo": RULE_SEED_LO, "split": [0.34, 0.33, 0.33],
        "trafficSeedHi": 7, "trafficSeedLo": 11, "trafficSplit": [0.0, 1.0],
        "variants": variants, "randomizationType": "switchback",
        "switchbackClusters": clusters, "switchbackStartAt": ANCHOR,
        "switchbackPeriodMs": HOUR, "switchbackDesignType": "balanced_block"
    })
}

/// The default links: session_id (unit type id 1) before user_id (2).
fn sbc_default() -> Value {
    sbc_clusters(&[(SESSION, "session_store"), (USER, "profile/store")])
}

fn sbc_clusters(clusters: &[(&str, &str)]) -> Value {
    let mut experiment = sbc(Value::Null);
    experiment["switchbackClusters"] = ordered_clusters(clusters);
    experiment
}

/// `json!` sorts object keys (serde_json without `preserve_order`), so an
/// ordered cluster map is carried as its JSON text until
/// [`parse_experiments`] splices it in.
fn ordered_clusters(clusters: &[(&str, &str)]) -> Value {
    let text = clusters
        .iter()
        .map(|(t, p)| format!("{}:{}", json!(t), json!(p)))
        .collect::<Vec<_>>()
        .join(",");
    json!(format!("{{{}}}", text))
}

/// Parse an experiment whose `switchbackClusters` holds ordered JSON text.
fn parse_experiments(experiments: &[Value]) -> ContextData {
    let items: Vec<String> = experiments
        .iter()
        .map(|e| {
            let mut e = e.clone();
            let clusters = e["switchbackClusters"].as_str().map(str::to_string);
            match clusters {
                Some(text) => {
                    e["switchbackClusters"] = json!("__CLUSTERS__");
                    e.to_string().replace("\"__CLUSTERS__\"", &text)
                }
                None => e.to_string(),
            }
        })
        .collect();
    serde_json::from_str(&format!(r#"{{"experiments":[{}]}}"#, items.join(","))).unwrap()
}

fn rule_scheduled(key: &str) -> i32 {
    assign_switchback(key, PERIOD, RULE_SEED, 3).unwrap() as i32
}

#[derive(Debug)]
struct Assigned {
    unit: Option<String>,
    eligible: bool,
    silent: bool,
    variant: i32,
    config: Value,
    exposures: Vec<Seen>,
}

/// Resolve and expose "sbc" after every other experiment; `None` when the
/// Collector lists no assignment (the switchback does not apply).
fn sbc_with(experiments: &[Value], attributes: Value, units: &[(&str, &str)]) -> Option<Assigned> {
    let clock = Clock::new(NOW);
    let mut ctx = Context::new(parse_experiments(experiments));
    clock.install(&mut ctx);
    for (unit_type, uid) in units {
        ctx.set_unit(unit_type, *uid).unwrap();
    }
    if let Value::Object(attributes) = attributes {
        ctx.set_attributes(attributes).unwrap();
    }
    for name in experiments.iter().filter_map(|e| e["name"].as_str()) {
        if name != "sbc" {
            ctx.treatment(name).unwrap();
        }
    }
    let variant = ctx.treatment("sbc").unwrap();
    let (serving, unit, eligible, silent) = resolution(&ctx, "sbc", NOW).unwrap();
    let config = ctx.variable_value("sbc.config", Value::Null).unwrap();
    let exposures: Vec<Seen> = queued(&mut ctx)
        .into_iter()
        .filter(|e| e.1 == "sbc")
        .collect();
    if serving == Serving::Absent {
        assert_eq!(variant, 0);
        assert_eq!(config, Value::Null);
        assert!(exposures.is_empty());
        return None;
    }
    Some(Assigned {
        unit,
        eligible,
        silent,
        variant,
        config,
        exposures,
    })
}

fn sbc_of(experiment: &Value, attributes: Value, units: &[(&str, &str)]) -> Option<Assigned> {
    sbc_with(std::slice::from_ref(experiment), attributes, units)
}

fn assert_ineligible(
    experiment: &Value,
    attributes: Value,
    expected_unit: &str,
    units: &[(&str, &str)],
) {
    let message = attributes.to_string();
    let a = sbc_of(experiment, attributes, units).unwrap_or_else(|| panic!("{}", message));
    assert_eq!(a.unit.as_deref(), Some(expected_unit), "{}", message);
    assert!(!a.eligible && a.silent, "{}", message);
    assert_eq!(a.variant, 0, "{}", message);
    assert_eq!(a.config, json!("test config 0"), "{}", message);
    assert!(a.exposures.is_empty(), "{}", message);
}

#[test]
fn missing_or_unreadable_cluster_configuration_returns_only_legacy_ineligible_control() {
    let attributes = json!({"geo": {"city": "Porto"}, "geo.city": "Faro", "session_store": "s-1"});
    for broken in [
        sbc(Value::Null),
        sbc(json!({})),
        sbc_clusters(&[(SESSION, "'geo")]),
        sbc_clusters(&[(SESSION, "geo.city")]),
        sbc_clusters(&[(SESSION, "'geo.city'")]),
        sbc_clusters(&[(SESSION, "\"geo.city\"")]),
    ] {
        // The declared unit (user_id) only identifies an ineligible control.
        assert_ineligible(
            &broken,
            attributes.clone(),
            USER,
            &[(SESSION, "x1"), (USER, "u1")],
        );
        assert!(sbc_of(&broken, attributes.clone(), &[(SESSION, "x1")]).is_none());
    }
    // A readable map whose only linked type is absent.
    let unknown = sbc_clusters(&[("device_id", "session_store")]);
    assert!(sbc_of(
        &unknown,
        json!({"session_store": "s-1"}),
        &[(SESSION, "x1"), (USER, "u1")]
    )
    .is_none());
}

#[test]
fn traffic_uses_serving_uid_not_experiment_uid_in_both_request_orders() {
    let mut experiment = sbc_default();
    experiment["trafficSplit"] = json!([0.6, 0.4]);
    let mut ordinary = unit_twin(sbc(Value::Null));
    ordinary["name"] = json!("ordinary");
    ordinary["id"] = json!(901);
    ordinary["variants"] = json!([{"name": "0", "config": null}, {"name": "1", "config": null}]);
    ordinary["split"] = json!([0.5, 0.5]);
    let mut witnesses: [Option<String>; 2] = [None, None];
    for i in 0..1000 {
        let uid = format!("traffic-{}", i);
        let slot = VariantAssigner::new(&crate::utils::hash_unit(&uid)).assign(&[0.6, 0.4], 7, 11);
        witnesses[slot.min(1)].get_or_insert(uid);
        if witnesses.iter().all(Option::is_some) {
            break;
        }
    }
    let [Some(out), Some(inside)] = witnesses else {
        panic!("no traffic witnesses");
    };
    let key = "s-18";
    assert_ne!(rule_scheduled(key), 0);
    for serving_eligible in [false, true] {
        let (session_uid, user_uid) = if serving_eligible {
            (&inside, &out)
        } else {
            (&out, &inside)
        };
        let a = sbc_with(
            &[experiment.clone(), ordinary.clone()],
            json!({"session_store": key, "profile": {"store": key}}),
            &[(SESSION, session_uid), (USER, user_uid)],
        )
        .unwrap();
        assert_eq!(a.unit.as_deref(), Some(SESSION));
        assert_eq!(a.eligible, serving_eligible);
        assert_eq!(
            a.variant,
            if serving_eligible {
                rule_scheduled(key)
            } else {
                0
            }
        );
        assert_eq!(a.exposures.len(), 1);
        assert_eq!(a.exposures[0].2.as_deref(), Some(SESSION));
        assert_eq!(a.exposures[0].5[1], serving_eligible);
    }
}

#[test]
fn one_matching_unit_serves_its_value() {
    let a = sbc_of(
        &sbc_default(),
        json!({"profile": {"store": "s-17"}}),
        &[(USER, "u1")],
    )
    .unwrap();
    assert!(a.eligible);
    assert_eq!(a.unit.as_deref(), Some(USER));
    assert_eq!(a.variant, rule_scheduled("s-17"));
}

#[test]
fn strict_audience_mismatch_on_an_eligible_cluster_is_listed_as_control_and_recorded() {
    let mut experiment = sbc_default();
    experiment["audience"] = json!(STRICT_GOLD_AUDIENCE);
    experiment["audienceStrict"] = json!(true);
    let a = sbc_of(
        &experiment,
        json!({"session_store": "s-17"}),
        &[(SESSION, "x1")],
    )
    .unwrap();
    assert_eq!(a.unit.as_deref(), Some(SESSION));
    assert!(a.eligible && !a.silent);
    assert_eq!(a.variant, 0);
    assert_eq!(a.exposures.len(), 1);
    assert!(a.exposures[0].5[5] && !a.exposures[0].5[0]);
}

#[test]
fn two_agreeing_units_serve_the_value() {
    let a = sbc_of(
        &sbc_default(),
        json!({"profile": {"store": "s-17"}, "session_store": "s-17"}),
        &[(USER, "u1"), (SESSION, "x1")],
    )
    .unwrap();
    assert!(a.eligible);
    assert_eq!(a.variant, rule_scheduled("s-17"));
}

#[test]
fn two_disagreeing_units_are_ineligible() {
    assert_ineligible(
        &sbc_default(),
        json!({"profile": {"store": "s-17"}, "session_store": "s-18"}),
        SESSION,
        &[(USER, "u1"), (SESSION, "x1")],
    );
}

// A context holds one uid per unit type, so only the single-unit path of the
// Collector's two-units-of-one-type case is reproducible.
#[test]
fn two_units_of_one_type_with_one_path_agree_on_the_same_value() {
    let key = "s-17";
    assert_ne!(rule_scheduled(key), 0);
    let mut ctx = Context::new(parse_experiments(&[sbc_default()]));
    ctx.set_unit(SESSION, "x1").unwrap();
    assert!(ctx.set_unit(SESSION, "x2").is_err());
    let a = sbc_of(
        &sbc_default(),
        json!({"session_store": key}),
        &[(SESSION, "x1")],
    )
    .unwrap();
    assert!(a.eligible);
    assert_eq!(a.variant, rule_scheduled(key));
}

#[test]
fn no_matching_unit_is_ineligible() {
    assert!(sbc_of(
        &sbc_default(),
        json!({"session_store": "s-1"}),
        &[("device_id", "d1")]
    )
    .is_none());
    assert!(sbc_of(&sbc_default(), json!({"session_store": "s-1"}), &[]).is_none());
}

#[test]
fn matching_unit_without_a_value_is_ineligible() {
    assert_ineligible(
        &sbc_default(),
        json!({"other": "s-1"}),
        USER,
        &[(USER, "u1")],
    );
    assert_ineligible(
        &sbc_default(),
        json!({"other": "s-1"}),
        SESSION,
        &[(USER, "u1"), (SESSION, "x1")],
    );
}

#[test]
fn non_string_non_integer_values_are_ineligible() {
    for value in [
        json!(1.5),
        json!(3.0),
        json!(true),
        json!({"a": 1}),
        json!(["a"]),
        json!(""),
    ] {
        assert_ineligible(
            &sbc_default(),
            json!({ "session_store": value }),
            SESSION,
            &[(SESSION, "x1")],
        );
    }
}

#[test]
fn whole_numbers_are_keyed_by_their_decimal_text() {
    assert_ne!(rule_scheduled("17"), 0);
    let a = sbc_of(
        &sbc_default(),
        json!({"session_store": 17}),
        &[(SESSION, "x1")],
    )
    .unwrap();
    assert!(a.eligible);
    assert_eq!(a.variant, rule_scheduled("17"));
    // Equal to the string "17" from another unit type, so the two agree.
    assert!(
        sbc_of(
            &sbc_default(),
            json!({"session_store": 17, "profile": {"store": "17"}}),
            &[(SESSION, "x1"), (USER, "u1")]
        )
        .unwrap()
        .eligible
    );
}

#[test]
fn nested_and_array_paths_are_read() {
    let a = sbc_of(
        &sbc_clusters(&[(SESSION, "items/1/store")]),
        json!({"items": [{"store": "a"}, {"store": "b"}]}),
        &[(SESSION, "x1")],
    )
    .unwrap();
    assert_eq!(a.variant, rule_scheduled("b"));
    assert_ne!(rule_scheduled("a"), rule_scheduled("b"));
}

#[test]
fn exposure_unit_is_the_first_matching_unit_in_declared_order_whatever_the_request_order() {
    let key = "s-18";
    assert_ne!(rule_scheduled(key), 0);
    let a = sbc_of(
        &sbc_default(),
        json!({"profile": {"store": key}, "session_store": key}),
        &[(USER, "u1"), (SESSION, "x1")],
    )
    .unwrap();
    assert_eq!(a.unit.as_deref(), Some(SESSION));
    assert_eq!(a.exposures[0].2.as_deref(), Some(SESSION));
    assert_eq!(a.variant, rule_scheduled(key));

    // Reversing the declared order moves the serving unit.
    let reversed = sbc_clusters(&[(USER, "profile/store"), (SESSION, "session_store")]);
    let a = sbc_of(
        &reversed,
        json!({"profile": {"store": key}, "session_store": key}),
        &[(USER, "u1"), (SESSION, "x1")],
    )
    .unwrap();
    assert_eq!(a.unit.as_deref(), Some(USER));
}

#[test]
fn ineligible_cluster_never_publishes_an_exposure() {
    let clock = Clock::new(NOW);
    let mut ctx = Context::new(parse_experiments(&[sbc_default()]));
    clock.install(&mut ctx);
    ctx.set_unit(USER, "u1").unwrap();
    ctx.set_unit(SESSION, "x1").unwrap();
    ctx.set_attributes(
        json!({"session_store": "a", "profile": {"store": "b"}})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(ctx.treatment("sbc").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);
}

#[test]
fn a_unit_without_a_value_does_not_serve_even_if_first_in_declared_order() {
    let key = "s-17";
    assert_ne!(rule_scheduled(key), 0);
    let a = sbc_of(
        &sbc_default(),
        json!({"profile": {"store": key}}),
        &[(SESSION, "x1"), (USER, "u1")],
    )
    .unwrap();
    assert_eq!(a.unit.as_deref(), Some(USER));
    assert_eq!(a.exposures[0].2.as_deref(), Some(USER));
    assert_eq!(a.variant, rule_scheduled(key));
}

#[test]
fn value_not_in_the_filter_is_ineligible() {
    let mut experiment = sbc_default();
    experiment["switchbackClusterValues"] = json!(["s-1", "s-2"]);
    assert_ineligible(
        &experiment,
        json!({"session_store": "s-3"}),
        SESSION,
        &[(SESSION, "x1")],
    );
    let allowed = sbc_of(
        &experiment,
        json!({"session_store": "s-2"}),
        &[(SESSION, "x1")],
    )
    .unwrap();
    assert!(allowed.eligible);
    assert_eq!(allowed.variant, rule_scheduled("s-2"));
}

#[test]
fn null_filter_assigns_every_value() {
    let mut experiment = sbc_default();
    experiment["switchbackClusterValues"] = Value::Null;
    for i in 0..20 {
        let key = format!("s-{}", i);
        let a = sbc_of(
            &experiment,
            json!({ "session_store": key }),
            &[(SESSION, "x1")],
        )
        .unwrap();
        assert!(a.eligible);
        assert_eq!(a.variant, rule_scheduled(&key));
    }
}

#[test]
fn valid_cluster_without_linked_request_unit_does_not_fall_back_to_experiment_unit() {
    let experiment = sbc_clusters(&[(SESSION, "session_store")]);
    let key = "s-17";
    assert_ne!(rule_scheduled(key), 0);
    assert!(sbc_of(
        &experiment,
        json!({"session_store": key, "profile": {"store": key}}),
        &[(USER, "u1")]
    )
    .is_none());
    let linked = sbc_of(
        &experiment,
        json!({ "session_store": key }),
        &[(USER, "u1"), (SESSION, "x1")],
    )
    .unwrap();
    assert_eq!(linked.unit.as_deref(), Some(SESSION));
    assert_eq!(linked.variant, rule_scheduled(key));
}

/// The Collector's wire-rule oracle SdkSwitchbackRule, transcribed:
/// `(variant, eligible)`.
fn oracle(
    experiment: &ExperimentData,
    uid: &str,
    cluster_key: Option<&str>,
    now: i64,
) -> (i32, bool) {
    let (Some(start), Some(period_ms)) = (
        experiment.switchback_start_at,
        experiment.switchback_period_ms,
    ) else {
        return (0, false);
    };
    let anchor = -(-start).div_euclid(HOUR) * HOUR;
    let Some(key) =
        cluster_key.filter(|_| period_ms > 0 && experiment.split.len() >= 2 && now >= anchor)
    else {
        return (0, false);
    };
    let eligible = VariantAssigner::new(&crate::utils::hash_unit(uid)).assign(
        &experiment.traffic_split,
        experiment.traffic_seed_hi,
        experiment.traffic_seed_lo,
    ) == 1;
    if !eligible {
        return (0, false);
    }
    let seed = ((u64::from(experiment.seed_hi) << 32) | u64::from(experiment.seed_lo)) as i64;
    let period = (now - anchor).div_euclid(period_ms);
    (
        assign_switchback(key, period, seed, experiment.split.len()).unwrap() as i32,
        true,
    )
}

#[derive(Debug, Clone, Copy)]
enum ClusterCase {
    Single,
    DisagreeingSecondSource,
    MissingValue,
    FilteredValue,
}

#[test]
fn context_matches_the_sdk_switchback_wire_rule_parity_grid() {
    let (mut eligible, mut ineligible, mut traffic_misses) = (0, 0, 0);
    for cluster in [
        ClusterCase::Single,
        ClusterCase::DisagreeingSecondSource,
        ClusterCase::MissingValue,
        ClusterCase::FilteredValue,
    ] {
        for anchor in [ANCHOR, ANCHOR + 7 * HOUR] {
            for period_ms in [HOUR, 2 * HOUR, 24 * HOUR] {
                let mut experiment = match cluster {
                    ClusterCase::DisagreeingSecondSource => {
                        sbc_clusters(&[(SESSION, "session_store"), (USER, "profile/store")])
                    }
                    _ => sbc_clusters(&[(SESSION, "session_store")]),
                };
                experiment["trafficSplit"] = json!([0.3, 0.7]);
                experiment["switchbackStartAt"] = json!(anchor);
                experiment["switchbackPeriodMs"] = json!(period_ms);
                for u in 0..6 {
                    let uid = format!("uid-{}", u);
                    for key in ["s-1", "s-2", "42", "Lisboa"] {
                        let (attributes, units): (Value, Vec<(&str, String)>) = match cluster {
                            ClusterCase::Single => (
                                json!({ "session_store": key }),
                                vec![(SESSION, uid.clone())],
                            ),
                            ClusterCase::DisagreeingSecondSource => (
                                json!({"session_store": key, "profile": {"store": format!("{}-other", key)}}),
                                vec![(SESSION, uid.clone()), (USER, format!("u-{}", uid))],
                            ),
                            ClusterCase::MissingValue => {
                                (json!({ "other": key }), vec![(SESSION, uid.clone())])
                            }
                            ClusterCase::FilteredValue => {
                                experiment["switchbackClusterValues"] =
                                    json!([format!("{}-allowed", key)]);
                                (
                                    json!({ "session_store": key }),
                                    vec![(SESSION, uid.clone())],
                                )
                            }
                        };
                        let data = parse_experiments(std::slice::from_ref(&experiment));
                        for now in [
                            anchor - HOUR,
                            anchor - 1,
                            anchor,
                            anchor + period_ms - 1,
                            anchor + period_ms,
                            anchor + 3 * period_ms + 5,
                            anchor + 11 * period_ms,
                        ] {
                            let oracle_key = matches!(cluster, ClusterCase::Single).then_some(key);
                            let (variant, oracle_eligible) =
                                oracle(&data.experiments[0], &uid, oracle_key, now);
                            let clock = Clock::new(now);
                            let mut ctx = Context::new(data.clone());
                            clock.install(&mut ctx);
                            for (unit_type, uid) in &units {
                                ctx.set_unit(unit_type, uid.as_str()).unwrap();
                            }
                            ctx.set_attributes(attributes.as_object().unwrap().clone())
                                .unwrap();
                            let message = format!(
                                "{:?}/{}/{}/{}/{}/{}",
                                cluster, anchor, period_ms, uid, key, now
                            );
                            assert_eq!(ctx.treatment("sbc").unwrap(), variant, "{}", message);
                            assert_eq!(
                                resolution(&ctx, "sbc", now).unwrap().2,
                                oracle_eligible,
                                "{}",
                                message
                            );
                            let exposures = queued(&mut ctx);
                            let expected_exposures =
                                usize::from(oracle_key.is_some() && now >= anchor);
                            assert_eq!(exposures.len(), expected_exposures, "{}", message);
                            if expected_exposures == 1 {
                                assert_eq!(exposures[0].5[1], oracle_eligible, "{}", message);
                            }
                            if matches!(cluster, ClusterCase::Single) {
                                if oracle_eligible {
                                    eligible += 1;
                                } else {
                                    ineligible += 1;
                                    if now >= anchor {
                                        traffic_misses += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(eligible > 100);
    assert!(ineligible > 100);
    assert!(traffic_misses > 0);

    // The deliberate divergence: an anchor off the hour is unservable in the
    // SDK, while the oracle would re-ceil it and draw.
    let mut experiment = sbc_clusters(&[(SESSION, "session_store")]);
    experiment["switchbackStartAt"] = json!(ANCHOR + 7 * HOUR + 1);
    let data = parse_experiments(&[experiment]);
    let now = ANCHOR + 8 * HOUR + 5;
    assert!(oracle(&data.experiments[0], "uid-0", Some("s-1"), now).1);
    let clock = Clock::new(now);
    let mut ctx = Context::new(data);
    clock.install(&mut ctx);
    ctx.set_unit(SESSION, "uid-0").unwrap();
    ctx.set_attribute("session_store", "s-1").unwrap();
    assert_eq!(ctx.treatment("sbc").unwrap(), 0);
    assert!(!resolution(&ctx, "sbc", now).unwrap().2);
    assert_eq!(ctx.pending(), 0);
}

// ---- SwitchbackServerParityTest ----

const PARITY_SEED: i64 = 1_234_567_890_123;
const PERIODS: i64 = 4;
const STORE: &str = "store_id";
const DEVICE: &str = "device_id";

fn parity_experiment(clusters: &[(&str, &str)], values: Option<Value>) -> Value {
    let mut experiment = sbc_clusters(clusters);
    experiment["name"] = json!("exp_sb");
    experiment["id"] = json!(1);
    experiment["unitType"] = json!(USER);
    experiment["seedHi"] = json!(PARITY_SEED >> 32);
    experiment["seedLo"] = json!(PARITY_SEED as i32);
    experiment["split"] = json!([1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]);
    experiment["variants"] = json!([{"name": "0"}, {"name": "1"}, {"name": "2"}]);
    if let Some(values) = values {
        experiment["switchbackClusterValues"] = values;
    }
    experiment
}

/// Serves PERIODS periods with one treatment each: (variants, exposures).
fn serve(experiment: &Value, units: &[(&str, &str)], attributes: Value) -> (Vec<i32>, Vec<Seen>) {
    let clock = Clock::new(ANCHOR);
    let mut ctx = Context::new(parse_experiments(std::slice::from_ref(experiment)));
    clock.install(&mut ctx);
    for (unit_type, uid) in units {
        ctx.set_unit(unit_type, *uid).unwrap();
    }
    if let Value::Object(attributes) = attributes {
        ctx.set_attributes(attributes).unwrap();
    }
    let variants = (0..PERIODS)
        .map(|p| {
            clock.set(ANCHOR + p * HOUR + 1);
            ctx.treatment("exp_sb").unwrap()
        })
        .collect();
    (variants, queued(&mut ctx))
}

fn collector_variant(key: &str, period: i64) -> i32 {
    assign_switchback(key, period, PARITY_SEED, 3).unwrap() as i32
}

fn assert_served(served: (Vec<i32>, Vec<Seen>), serving_type: &str, key: &str, message: &str) {
    let (variants, exposures) = served;
    assert_eq!(exposures.len(), PERIODS as usize, "{}", message);
    for p in 0..PERIODS {
        let expected = collector_variant(key, p);
        assert_eq!(variants[p as usize], expected, "{} period {}", message, p);
        let e = &exposures[p as usize];
        assert_eq!(e.2.as_deref(), Some(serving_type), "{}", message);
        assert_eq!(e.3, expected, "{}", message);
        assert!(e.5[0] && e.5[1], "{}", message);
        assert_eq!(e.4, ANCHOR + p * HOUR + 1, "{}", message);
    }
}

fn assert_silent_control(served: (Vec<i32>, Vec<Seen>), message: &str) {
    assert!(served.0.iter().all(|v| *v == 0), "{}", message);
    assert!(served.1.is_empty(), "{}: {:?}", message, served.1);
}

const TWO_SOURCES: &[(&str, &str)] = &[(STORE, "store_city"), (DEVICE, "geo/city")];
const TWO_UNITS: &[(&str, &str)] = &[(STORE, "s1"), (DEVICE, "d1")];

#[test]
fn raw_key_schedules_match_the_independent_reference() {
    assert_served(
        serve(
            &parity_experiment(&[(STORE, "city")], None),
            &[(STORE, "s1")],
            json!({"city": "Lisboa"}),
        ),
        STORE,
        "Lisboa",
        "Lisboa",
    );
    for p in 0..PERIODS {
        assert_eq!(collector_variant("Lisboa", p), [0, 1, 2, 2][p as usize]);
        assert_eq!(collector_variant("Porto", p), [2, 1, 0, 1][p as usize]);
        assert_eq!(collector_variant("42", p), [0, 2, 1, 2][p as usize]);
    }
}

#[test]
fn agreement_serves_the_first_source_unit() {
    assert_served(
        serve(
            &parity_experiment(TWO_SOURCES, None),
            TWO_UNITS,
            json!({"store_city": "Lisboa", "geo": {"city": "Lisboa"}}),
        ),
        STORE,
        "Lisboa",
        "agreement",
    );
}

#[test]
fn disagreement_is_ineligible_and_records_nothing() {
    assert_silent_control(
        serve(
            &parity_experiment(TWO_SOURCES, None),
            TWO_UNITS,
            json!({"store_city": "Lisboa", "geo": {"city": "Porto"}}),
        ),
        "disagreement",
    );
}

#[test]
fn missing_value_is_skipped_and_the_next_value_serves() {
    let experiment = parity_experiment(TWO_SOURCES, None);
    assert_served(
        serve(&experiment, TWO_UNITS, json!({"geo": {"city": "Porto"}})),
        DEVICE,
        "Porto",
        "first missing",
    );
    assert_served(
        serve(&experiment, TWO_UNITS, json!({"store_city": "Porto"})),
        STORE,
        "Porto",
        "second missing",
    );
    assert_silent_control(
        serve(&experiment, TWO_UNITS, json!({"other": "x"})),
        "no value at all",
    );
    assert_silent_control(serve(&experiment, TWO_UNITS, Value::Null), "no attributes");
}

#[test]
fn only_linked_units_present_in_the_context_take_part() {
    assert_served(
        serve(
            &parity_experiment(TWO_SOURCES, None),
            &[(DEVICE, "d1")],
            json!({"store_city": "Lisboa", "geo": {"city": "Porto"}}),
        ),
        DEVICE,
        "Porto",
        "store unit absent",
    );
}

#[test]
fn filtered_value_is_ineligible_and_allowed_value_serves() {
    let lisboa = json!({"store_city": "Lisboa"});
    assert_silent_control(
        serve(
            &parity_experiment(TWO_SOURCES, Some(json!(["Porto", "Faro"]))),
            TWO_UNITS,
            lisboa.clone(),
        ),
        "filtered",
    );
    for (values, message) in [
        (Some(json!(["Porto", "Lisboa"])), "allowed"),
        (Some(Value::Null), "null filter"),
        (Some(json!([])), "empty filter"),
    ] {
        assert_served(
            serve(
                &parity_experiment(TWO_SOURCES, values),
                TWO_UNITS,
                lisboa.clone(),
            ),
            STORE,
            "Lisboa",
            message,
        );
    }
    assert_served(
        serve(
            &parity_experiment(TWO_SOURCES, Some(json!(["42"]))),
            TWO_UNITS,
            json!({"store_city": 42}),
        ),
        STORE,
        "42",
        "numeric filter",
    );
    assert_silent_control(
        serve(
            &parity_experiment(TWO_SOURCES, Some(json!(["Porto"]))),
            TWO_UNITS,
            json!({"store_city": "Lisboa", "geo": {"city": "Porto"}}),
        ),
        "filtered first value",
    );
}

#[test]
fn lower_unit_type_id_source_serves_first() {
    let agreeing = json!({"store_city": "Lisboa", "geo": {"city": "Lisboa"}});
    assert_served(
        serve(
            &parity_experiment(&[(DEVICE, "geo/city"), (STORE, "store_city")], None),
            TWO_UNITS,
            agreeing.clone(),
        ),
        DEVICE,
        "Lisboa",
        "device declared first",
    );
    assert_served(
        serve(&parity_experiment(TWO_SOURCES, None), TWO_UNITS, agreeing),
        STORE,
        "Lisboa",
        "store first",
    );
}

#[test]
fn non_key_values_are_ignored() {
    for value in [
        json!(true),
        json!(false),
        json!(1.5),
        json!(2.0),
        json!({"a": "b"}),
        json!(["x"]),
        json!([]),
        Value::Null,
        json!(""),
    ] {
        let message = format!("ignored {}", value);
        assert_served(
            serve(
                &parity_experiment(TWO_SOURCES, None),
                TWO_UNITS,
                json!({"store_city": value, "geo": {"city": "Porto"}}),
            ),
            DEVICE,
            "Porto",
            &message,
        );
        assert_silent_control(
            serve(
                &parity_experiment(&[(STORE, "store_city")], None),
                &[(STORE, "s1")],
                json!({ "store_city": value }),
            ),
            &format!("{} alone", message),
        );
    }
}

#[test]
fn numeric_values_are_keyed_as_text() {
    for store in [json!(42), json!("42")] {
        for device in [json!(42), json!("42")] {
            assert_served(
                serve(
                    &parity_experiment(TWO_SOURCES, None),
                    TWO_UNITS,
                    json!({"store_city": store, "geo": {"city": device}}),
                ),
                STORE,
                "42",
                &format!("{}/{}", store, device),
            );
        }
    }
    assert_served(
        serve(
            &parity_experiment(&[(STORE, "n")], None),
            &[(STORE, "s1")],
            json!({"n": -7}),
        ),
        STORE,
        "-7",
        "negative",
    );
    assert_served(
        serve(
            &parity_experiment(&[(STORE, "n")], None),
            &[(STORE, "s1")],
            json!({ "n": u64::MAX }),
        ),
        STORE,
        "18446744073709551615",
        "u64",
    );
    assert_silent_control(
        serve(
            &parity_experiment(TWO_SOURCES, None),
            TWO_UNITS,
            json!({"store_city": 42, "geo": {"city": "042"}}),
        ),
        "42 vs 042 disagree",
    );
}

#[test]
fn malformed_paths_are_unservable() {
    let attributes = json!({"store_city": "Lisboa", "geo": {"city": "Porto"}, "geo.city": "Faro"});
    for path in [
        "'geo/city",
        "\"geo",
        "geo/'city",
        "geo.city",
        "geo/a.b",
        "'geo.city'",
        "\"geo.city\"",
    ] {
        assert_silent_control(
            serve(
                &parity_experiment(&[(STORE, "store_city"), (DEVICE, path)], None),
                TWO_UNITS,
                attributes.clone(),
            ),
            path,
        );
        assert_silent_control(
            serve(
                &parity_experiment(&[(DEVICE, path)], None),
                TWO_UNITS,
                attributes.clone(),
            ),
            &format!("{} alone", path),
        );
    }
}

#[test]
fn no_linked_unit_is_absent() {
    assert_silent_control(
        serve(
            &parity_experiment(TWO_SOURCES, None),
            &[(USER, "u1")],
            json!({"store_city": "Lisboa"}),
        ),
        "no linked unit",
    );
}

#[test]
fn attribute_path_cases_match_the_collector() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/data/switchback/attribute-path-cases.json"
    ))
    .unwrap();
    let mut eligible_cases = 0;
    for c in fixture["cases"].as_array().unwrap() {
        let path = c["path"].as_str().unwrap();
        let served = serve(
            &parity_experiment(&[(STORE, path)], None),
            &[(STORE, "s1")],
            fixture["attributes"].clone(),
        );
        match c["value"].as_str() {
            Some(value) if c["supported"].as_bool().unwrap() => {
                assert_served(served, STORE, value, &c.to_string());
                eligible_cases += 1;
            }
            _ => assert_silent_control(served, &c.to_string()),
        }
    }
    assert_eq!(eligible_cases, 3);
}

// Repeated names resolve to the latest; names are matched trimmed.
#[test]
fn repeated_attribute_names_resolve_to_the_latest() {
    assert_served(
        serve(
            &parity_experiment(&[(STORE, "cluster")], None),
            &[(STORE, "s1")],
            json!({" cluster ": "Porto"}),
        ),
        STORE,
        "Porto",
        "trimmed name",
    );
    let clock = Clock::new(ANCHOR);
    let mut ctx = Context::new(parse_experiments(&[parity_experiment(
        &[(STORE, "cluster")],
        None,
    )]));
    clock.install(&mut ctx);
    ctx.set_unit(STORE, "s1").unwrap();
    ctx.set_attribute("cluster", "Lisboa").unwrap();
    ctx.set_attribute("cluster", "Porto").unwrap();
    clock.set(ANCHOR + 1);
    assert_eq!(
        ctx.treatment("exp_sb").unwrap(),
        collector_variant("Porto", 0)
    );
}

// Payloads without switchbacks never look one up.
#[test]
fn unit_only_payloads_skip_the_switchback_lookup() {
    let clock = Clock::new(ANCHOR);
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let mut ctx = context(&clock, data_of(vec![unit.clone()]));
    ctx.treatment("exp_sb").unwrap();
    ctx.peek("exp_sb").unwrap();
    ctx.variable_value("sb.arm", -1).unwrap();
    assert_eq!(ctx.switchback_lookups, 0);

    ctx.refresh_with(data_of(vec![unit, switchback(2, "exp_other", 2, HOUR)]));
    ctx.treatment("exp_sb").unwrap();
    assert_eq!(ctx.switchback_lookups, 1);
}

// A refresh that removes every switchback puts reads back on the unit path.
#[test]
fn refresh_removing_every_switchback_skips_the_switchback_lookup_again() {
    let clock = Clock::new(ANCHOR);
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let mut ctx = context(
        &clock,
        data_of(vec![unit.clone(), switchback(2, "exp_other", 2, HOUR)]),
    );
    ctx.treatment("exp_sb").unwrap();
    let before = ctx.switchback_lookups;
    assert!(before > 0);
    ctx.refresh_with(data_of(vec![unit]));
    ctx.treatment("exp_sb").unwrap();
    ctx.peek("exp_sb").unwrap();
    ctx.variable_value("sb.arm", -1).unwrap();
    assert_eq!(ctx.switchback_lookups, before);
}

// A refresh drops the slots of a switchback it removes or replaces, without
// waiting for a read of the old name, and keeps the others.
#[test]
fn refresh_drops_slots_of_retired_switchbacks() {
    let clock = Clock::new(ANCHOR + 10);
    let kept = switchback(2, "exp_other", 2, HOUR);
    let mut ctx = context(&clock, data_of(vec![sb(2, HOUR), kept.clone()]));
    ctx.treatment("exp_sb").unwrap();
    ctx.treatment("exp_other").unwrap();
    assert_eq!(ctx.switchbacks.len(), 2);

    ctx.refresh_with(data_of(vec![kept.clone()]));
    assert!(!ctx.switchbacks.contains_key("exp_sb"));
    assert!(ctx.switchbacks.contains_key("exp_other"));
    assert_eq!(ctx.pending(), 2);

    let mut restarted = kept;
    restarted["iteration"] = json!(2);
    ctx.refresh_with(data_of(vec![restarted]));
    assert!(ctx.switchbacks.is_empty());
}

// Every read API takes the unit fast path, never the generic switchback-aware
// one, while the data has no switchback.
#[test]
fn unit_only_reads_skip_the_generic_path() {
    let clock = Clock::new(ANCHOR);
    let mut unit = sb(2, HOUR);
    unit["randomizationType"] = json!("unit");
    let mut ctx = context(&clock, data_of(vec![unit.clone()]));
    ctx.treatment("exp_sb").unwrap();
    ctx.peek("exp_sb").unwrap();
    ctx.variable_value("sb.arm", -1).unwrap();
    ctx.peek_variable_value("sb.arm", -1).unwrap();
    assert_eq!(ctx.generic_assigns, 0);

    ctx.refresh_with(data_of(vec![unit, switchback(2, "exp_other", 2, HOUR)]));
    ctx.peek("exp_sb").unwrap();
    assert!(ctx.generic_assigns > 0);
}

// A variable lookup reads the clock once: the switchback it walks past (not
// exposed, as in java-sdk) and the one that supplies the value share the
// reading, so the exposure is stamped with the reading that chose its period.
#[test]
fn variable_lookup_reads_the_clock_once_across_switchbacks() {
    let mut first = switchback(1, "exp_a", 2, HOUR);
    first["audience"] = json!(STRICT_GOLD_AUDIENCE);
    first["audienceStrict"] = json!(true);
    let second = switchback(2, "exp_b", 2, HOUR);
    let clock = Clock::new(ANCHOR);
    let mut ctx = context(&clock, data_of(vec![first, second]));
    clock.set(ANCHOR + HOUR);
    clock.next_read(ANCHOR + HOUR - 1);
    assert_eq!(
        ctx.variable_value("sb.arm", -1).unwrap(),
        json!(EXPECTED_2_ARMS[0])
    );
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(
            2,
            "exp_b",
            EXPECTED_2_ARMS[0],
            ANCHOR + HOUR - 1,
            true,
            true
        ),]
    );
}

// A legacy-path switchback has no period, so an override on it is exposed
// once, not once per period.
#[test]
fn override_on_a_legacy_switchback_is_exposed_once_across_periods() {
    let mut legacy = sb(2, HOUR);
    legacy["switchbackClusters"] = Value::Null;
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![legacy]));
    ctx.set_override("exp_sb", 1).unwrap();
    for i in 0..3 {
        clock.set(ANCHOR + 10 + i * HOUR);
        assert_eq!(ctx.treatment("exp_sb").unwrap(), 1);
    }
    assert_eq!(
        queued(&mut ctx),
        vec![seen(
            1,
            "exp_sb",
            UNIT_TYPE,
            1,
            ANCHOR + 10,
            false,
            true,
            true,
            false,
            false,
            false,
            false
        )]
    );
}

// An override peeked before any linked unit was set is resolved again once
// one is, so it is exposed under the serving unit, not the fallback.
#[test]
fn peeked_override_is_reresolved_when_a_linked_unit_is_set() {
    let mut experiment = sb(2, HOUR);
    experiment["switchbackClusters"] =
        ordered_clusters(&[("device_id", "city"), ("user_id", "city")]);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = bare_context(&clock, &[], parse_experiments(&[experiment]));
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.set_override("exp_sb", 2).unwrap();
    assert_eq!(ctx.peek("exp_sb").unwrap(), 2);
    ctx.set_unit("user_id", "u1").unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 2);
    assert_eq!(
        queued(&mut ctx),
        vec![seen(
            1,
            "exp_sb",
            "user_id",
            2,
            ANCHOR + 10,
            false,
            true,
            true,
            false,
            false,
            false,
            false
        )]
    );
}

// randomizationType is matched exactly, as in java-sdk.
#[test]
fn randomization_type_is_case_sensitive() {
    let mut upper = sb(2, HOUR);
    upper["randomizationType"] = json!("SWITCHBACK");
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![upper.clone()]));
    let variant = ctx.treatment("exp_sb").unwrap();
    assert!(ctx.switchbacks.is_empty());
    let mut twin = context(&clock, data_of(vec![unit_twin(upper)]));
    assert_eq!(twin.treatment("exp_sb").unwrap(), variant);
    assert_eq!(queued(&mut ctx), queued(&mut twin));
}

// An ineligible slot never consulted its rules, so an unrelated attribute
// does not make it re-evaluate them or rebuild.
#[test]
fn ineligible_slot_is_kept_after_an_unrelated_attribute() {
    let mut experiment = sb(2, HOUR);
    experiment["assignmentRules"] = json!(ASSIGN_VARIANT_1);
    experiment["switchbackClusterValues"] = json!(["other-store"]);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    ctx.set_attribute("unrelated", 1).unwrap();
    assert_eq!(ctx.peek("exp_sb").unwrap(), 0);
    // Kept, not rebuilt: a rebuilt slot would be unexposed.
    assert!(ctx.switchbacks["exp_sb"].periods[0].1.assignment.exposed);
    assert_eq!(ctx.rule_evaluations, 0);
}

// The Collector resolves the cluster again at exposedAt from the published
// attributes. When a cluster attribute changed after the slot was resolved
// in this period, the exposure is stamped with the resolving reading, so the
// Collector sees the cluster the variant came from.
#[test]
fn exposure_after_a_cluster_change_is_stamped_when_the_cluster_was_read() {
    let mut experiment = attribute_switchback("city");
    experiment["switchbackClusterValues"] = json!(["Faro"]);
    let clock = Clock::new(ANCHOR + 10);
    let mut ctx = store_context(&clock, data_of(vec![experiment]));
    ctx.set_attribute("city", "Faro").unwrap();
    let faro = expected("Faro", 0);
    assert_eq!(ctx.peek("exp_sb").unwrap(), faro);
    clock.set(ANCHOR + 20);
    ctx.set_attribute("city", "Lisboa").unwrap();
    clock.set(ANCHOR + 30);
    assert_eq!(ctx.treatment("exp_sb").unwrap(), faro);
    assert_eq!(
        queued(&mut ctx),
        vec![exposure(1, "exp_sb", faro, ANCHOR + 10, true, true)]
    );
    // At that exposedAt the published attributes give Faro, which is allowed.
    let params = ctx.get_publish_params();
    let at = params.exposures.as_ref().unwrap()[0].exposed_at;
    let city = params
        .attributes
        .unwrap()
        .into_iter()
        .filter(|a| a.name == "city" && a.set_at <= at)
        .max_by_key(|a| a.set_at)
        .unwrap();
    assert_eq!(city.value, json!("Faro"));
}

// A refresh that turns on strict targeting rebuilds a cached slot whose
// audience does not match, as a fresh context would serve it.
#[test]
fn refresh_enabling_strict_audience_rebuilds_the_slot() {
    let mut loose = sb(2, HOUR);
    loose["audience"] = json!(STRICT_GOLD_AUDIENCE);
    let clock = Clock::new(ANCHOR + HOUR + 3);
    let mut ctx = context(&clock, data_of(vec![loose.clone()]));
    assert_eq!(ctx.peek("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    let mut strict = loose.clone();
    strict["audienceStrict"] = json!(true);
    ctx.try_refresh_with(data_of(vec![strict.clone()])).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(
        queued(&mut ctx),
        vec![strict_mismatch_exposure(ANCHOR + HOUR + 3)]
    );

    // An audience edit alone also re-evaluates.
    let mut matching = strict;
    matching["audience"] = json!(r#"{"filter":[{"value":true}]}"#);
    ctx.try_refresh_with(data_of(vec![matching])).unwrap();
    assert_eq!(ctx.treatment("exp_sb").unwrap(), EXPECTED_2_ARMS[1]);
    assert_eq!(ctx.pending(), 2);
}

// A switchback whose served variant lacks the key does not supply it, so the
// lookup moves on; being assigned, it is still exposed, so control periods
// read only through variables are recorded.
#[test]
fn assigned_switchback_without_the_key_is_exposed_by_a_variable_lookup() {
    let mut first = switchback(1, "exp_a", 2, HOUR);
    first["variants"] = json!([{"name": "A", "config": r#"{"other":0}"#}, {"name": "B", "config": r#"{"other":1}"#}]);
    first["variants"][1]["config"] = json!(r#"{"sb.arm":1}"#);
    first["variants"][0]["config"] = json!(r#"{"other":0}"#);
    let second = switchback(2, "exp_b", 2, HOUR);
    let clock = Clock::new(ANCHOR + 5);
    let mut ctx = context(&clock, data_of(vec![first, second]));
    // Period 0 draws arm 0, whose variant has no sb.arm.
    assert_eq!(EXPECTED_2_ARMS[0], 0);
    assert_eq!(ctx.peek("exp_a").unwrap(), 0);
    assert_eq!(
        ctx.variable_value("sb.arm", -1).unwrap(),
        json!(EXPECTED_2_ARMS[0])
    );
    // exp_a is assigned (arm 0, a control without the key), so it is exposed
    // as java-sdk does; only an unassigned switchback is walked past silently.
    assert_eq!(
        queued(&mut ctx),
        vec![
            exposure(1, "exp_a", 0, ANCHOR + 5, true, true),
            exposure(2, "exp_b", EXPECTED_2_ARMS[0], ANCHOR + 5, true, true),
        ]
    );
}

// An overridden or rule-won switchback is not "assigned", but it applies, so
// a variable lookup that walks past its key-less variant still exposes it.
#[test]
fn overridden_or_rule_won_switchback_without_the_key_is_exposed_by_a_variable_lookup() {
    for won_by_rule in [false, true] {
        let mut first = switchback(1, "exp_a", 2, HOUR);
        first["variants"] = json!([{"name": "A", "config": r#"{"other":0}"#}, {"name": "B", "config": r#"{"sb.arm":1}"#}]);
        if won_by_rule {
            first["assignmentRules"] =
                json!(r#"{"rules":[{"type":"assign","environments":[],"variant":0}]}"#);
        }
        let second = switchback(2, "exp_b", 2, HOUR);
        let clock = Clock::new(ANCHOR + 5);
        let mut ctx = context(&clock, data_of(vec![first, second]));
        if !won_by_rule {
            ctx.set_override("exp_a", 0).unwrap();
        }
        assert_eq!(
            ctx.variable_value("sb.arm", -1).unwrap(),
            json!(EXPECTED_2_ARMS[0])
        );
        assert_eq!(
            queued(&mut ctx),
            vec![
                seen(
                    1,
                    "exp_a",
                    UNIT_TYPE,
                    0,
                    ANCHOR + 5,
                    false,
                    true,
                    !won_by_rule,
                    false,
                    false,
                    false,
                    won_by_rule
                ),
                exposure(2, "exp_b", EXPECTED_2_ARMS[0], ANCHOR + 5, true, true),
            ],
            "won by rule: {}",
            won_by_rule
        );
    }
}

// A split with more arms than variants never serves or exposes an arm that
// has no variant: the schedule is unservable, so a silent control.
#[test]
fn split_and_variant_arity_mismatch_is_a_silent_control() {
    for split in [json!([0.5, 0.5, 0.0]), json!([0.5, 0.25, 0.25])] {
        let mut experiment = sb(2, HOUR);
        experiment["split"] = split.clone();
        let clock = Clock::new(ANCHOR);
        let mut ctx = context(&clock, data_of(vec![experiment]));
        for i in 0..6 {
            clock.set(ANCHOR + i * HOUR);
            assert_eq!(ctx.treatment("exp_sb").unwrap(), 0, "{}", split);
            assert_eq!(
                ctx.variable_value("sb.arm", -1).unwrap(),
                json!(0),
                "{}",
                split
            );
        }
        assert_eq!(ctx.pending(), 0, "{}", split);
    }
    // Fewer split entries than variants is unservable too.
    let mut experiment = sb(3, HOUR);
    experiment["split"] = json!([0.5, 0.5]);
    let clock = Clock::new(ANCHOR + HOUR);
    let mut ctx = context(&clock, data_of(vec![experiment]));
    assert_eq!(ctx.treatment("exp_sb").unwrap(), 0);
    assert_eq!(ctx.pending(), 0);
}

// ---- wire model ----

// The cluster map keeps its JSON key order, which is neither sorted nor
// reverse sorted here, and round-trips through serialization.
#[test]
fn switchback_clusters_keep_their_declared_order() {
    let text = r#"{"experiments":[{"id":1,"name":"e","switchbackClusters":{"user_id":"a","device_id":"b","store_id":"c","account":"d"}}]}"#;
    let data: ContextData = serde_json::from_str(text).unwrap();
    let order: Vec<&str> = data.experiments[0]
        .switchback_clusters
        .as_deref()
        .unwrap()
        .iter()
        .map(|(t, _)| t.as_str())
        .collect();
    assert_eq!(order, ["user_id", "device_id", "store_id", "account"]);
    let again = serde_json::to_string(&data).unwrap();
    assert!(again.contains(
        r#""switchbackClusters":{"user_id":"a","device_id":"b","store_id":"c","account":"d"}"#
    ));
    let reparsed: ContextData = serde_json::from_str(&again).unwrap();
    assert_eq!(
        reparsed.experiments[0].switchback_clusters,
        data.experiments[0].switchback_clusters
    );
}

// Text decoding keeps the declared order; from_value serves whichever unit
// the Value's map lists first (sorted without `preserve_order`).
#[test]
fn cluster_order_survives_text_decoding_but_not_a_value() {
    let text = format!(
        r#"{{"experiments":[{}]}}"#,
        sbc_clusters(&[(USER, "city"), ("device_id", "city")])
            .to_string()
            .replace(
                &json!(ordered_clusters(&[(USER, "city"), ("device_id", "city")])
                    .as_str()
                    .unwrap())
                .to_string(),
                r#"{"user_id":"city","device_id":"city"}"#
            )
    );
    let from_text: ContextData = serde_json::from_str(&text).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    // The Value's own order: sorted, unless a dependency turns on
    // serde_json's `preserve_order`.
    let value_first = value["experiments"][0]["switchbackClusters"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let from_value: ContextData = serde_json::from_value(value).unwrap();
    let serving_unit = |data: ContextData| {
        let clock = Clock::new(NOW);
        let mut ctx = bare_context(
            &clock,
            &[(USER, json!("u1")), ("device_id", json!("d1"))],
            data,
        );
        ctx.set_attribute("city", "Lisboa").unwrap();
        ctx.treatment("sbc").unwrap();
        queued(&mut ctx)[0].2.clone()
    };
    assert_eq!(serving_unit(from_text).as_deref(), Some(USER));
    assert_eq!(serving_unit(from_value), Some(value_first));
}

// A repeated key keeps its first position and its last path, as Jackson's
// LinkedHashMap does; a non-string path makes the map unreadable (legacy).
#[test]
fn switchback_clusters_decode_like_a_linked_hash_map() {
    let parse = |clusters: &str| -> Option<Vec<(String, String)>> {
        let text = format!(
            r#"{{"experiments":[{{"id":1,"name":"e","switchbackClusters":{}}}]}}"#,
            clusters
        );
        serde_json::from_str::<ContextData>(&text)
            .unwrap()
            .experiments[0]
            .switchback_clusters
            .clone()
    };
    let pairs = |p: &[(&str, &str)]| {
        Some(
            p.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        parse(r#"{"b":"1","a":"2","b":"3"}"#),
        pairs(&[("b", "3"), ("a", "2")])
    );
    assert_eq!(parse(r#"{"b":"1","a":5}"#), pairs(&[]));
    assert_eq!(parse(r#"{"b":"1","a":null}"#), pairs(&[]));
    assert_eq!(parse("{}"), pairs(&[]));
    for other in ["null", "[]", "[1]", "\"x\"", "5", "true"] {
        assert_eq!(parse(other), None, "{}", other);
    }
}

// Every switchback field is optional and lenient; unknown fields (Phase 1E
// ones included) are ignored, and a malformed switchback field degrades only
// its own experiment.
#[test]
fn switchback_fields_decode_leniently_and_unknown_fields_are_ignored() {
    let text = r#"{"experiments":[
        {"id":1,"name":"sb","unitType":"store_id","split":[0.5,0.5],"seedHi":1,"seedLo":2,
         "trafficSplit":[0,1],"variants":[{"config":null},{"config":null}],
         "randomizationType":"switchback","switchbackClusters":{"store_id":"city","device_id":"geo/city"},
         "switchbackClusterValues":["porto",7,true,null,{"x":1},[2]],
         "switchbackStartAt":1767225600000.0,"switchbackPeriodMs":3600000,
         "switchbackCarryoverDurationMs":"soon","switchbackBurnInDurationMs":600000,
         "switchbackBurnOutDurationMs":null,"switchbackDesignType":5,"switchbackEstimand":"g",
         "switchbackClusterUnitType":"store_id","switchbackClusterCount":50,
         "switchbackClusterAttribute":"geo/city","switchbackFutureField":{"nested":[1,2,3]}},
        {"id":2,"name":"unit","unitType":"store_id","split":[0.5,0.5],"trafficSplit":[0,1],
         "randomizationType":7,"switchbackClusters":"nope","switchbackClusterValues":"nope",
         "switchbackStartAt":"later","switchbackPeriodMs":1.5,"variants":[{"config":null},{"config":null}]}
    ]}"#;
    let data: ContextData = serde_json::from_str(text).unwrap();
    let (a, b) = (&data.experiments[0], &data.experiments[1]);
    assert_eq!(a.randomization_type.as_deref(), Some("switchback"));
    assert_eq!(
        a.switchback_cluster_values,
        Some(vec![
            "porto".to_string(),
            "7".to_string(),
            "true".to_string()
        ])
    );
    assert_eq!(a.switchback_start_at, Some(1_767_225_600_000));
    assert_eq!(a.switchback_period_ms, Some(3_600_000));
    assert_eq!(a.switchback_carryover_duration_ms, None);
    assert_eq!(a.switchback_burn_in_duration_ms, Some(600_000));
    assert_eq!(a.switchback_burn_out_duration_ms, None);
    assert_eq!(a.switchback_design_type, None);
    assert_eq!(a.switchback_estimand.as_deref(), Some("g"));
    assert_eq!(b.randomization_type, None);
    assert_eq!(b.switchback_clusters, None);
    assert_eq!(b.switchback_cluster_values, None);
    assert_eq!(b.switchback_start_at, None);
    assert_eq!(b.switchback_period_ms, None);

    // Both still assign: the switchback from its cluster, the other as a unit.
    let clock = Clock::new(ANCHOR);
    let mut ctx = bare_context(&clock, &[(UNIT_TYPE, json!(UID))], data);
    ctx.set_attribute("city", "porto").unwrap();
    let seed = (1i64 << 32) | 2;
    assert_eq!(
        ctx.treatment("sb").unwrap(),
        assign_switchback("porto", 0, seed, 2).unwrap() as i32
    );
    let unit_variant = VariantAssigner::new(UID_HASH).assign(&[0.5, 0.5], 0, 0) as i32;
    assert_eq!(ctx.treatment("unit").unwrap(), unit_variant);
    assert_eq!(ctx.pending(), 2);
}

// Experiments without switchback fields serialize exactly as before.
#[test]
fn unit_experiments_serialize_without_switchback_fields() {
    let data = data_of(vec![json!({"id": 1, "name": "e", "split": [1.0]})]);
    let text = serde_json::to_string(&data).unwrap();
    assert!(
        !text.contains("switchback") && !text.contains("randomization"),
        "{}",
        text
    );
}
