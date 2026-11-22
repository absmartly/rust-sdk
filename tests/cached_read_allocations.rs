//! Warmed cached reads must not copy the assignment or the experiment's
//! assignment rules: once an assignment is current, `peek`/`treatment` do no
//! heap work, with or without rules and after a rule revalidation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use absmartly::{Context, ContextData};
use serde_json::{json, Value};

struct CountingAlloc;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn allocations_during(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

fn context_data(assignment_rules: &str, variant_count: usize) -> ContextData {
    context_data_with_audience(assignment_rules, variant_count, "")
}

fn context_data_with_audience(
    assignment_rules: &str,
    variant_count: usize,
    audience: &str,
) -> ContextData {
    let large_config = json!({"payload": "x".repeat(10_000), "list": vec![1; 500]}).to_string();
    serde_json::from_value(json!({"experiments": [{
        "id": 1,
        "name": "exp",
        "unitType": "session_id",
        "iteration": 1,
        "trafficSplit": [0.0, 1.0],
        "trafficSeedHi": 1,
        "trafficSeedLo": 2,
        "split": [0.5, 0.5],
        "seedHi": 3,
        "seedLo": 4,
        "variants": vec![json!({"config": large_config}); variant_count],
        "assignmentRules": assignment_rules,
        "audience": audience
    }]}))
    .unwrap()
}

fn context(assignment_rules: &str) -> Context {
    context_with_audience(assignment_rules, "")
}

fn context_with_audience(assignment_rules: &str, audience: &str) -> Context {
    let mut ctx = Context::new(context_data_with_audience(assignment_rules, 2, audience));
    ctx.set_system_attributes(Some("website"), Some("production"));
    ctx.set_unit("session_id", "user").unwrap();
    ctx
}

fn large_rules_ending_with(last: serde_json::Value) -> String {
    let mut rules: Vec<_> = (0..200)
        .map(|i| json!({"type": "assign", "variant": 1, "conditions": {"eq": [{"var": "tier"}, {"value": format!("t{}", i)}]}}))
        .collect();
    rules.push(last);
    json!({ "rules": rules }).to_string()
}

fn large_rules() -> String {
    large_rules_ending_with(json!({"type": "split", "percentages": "50/50"}))
}

#[test]
fn warmed_reads_do_not_allocate() {
    for rules in [String::new(), large_rules()] {
        let mut ctx = context(&rules);
        let variant = ctx.treatment("exp").unwrap();
        let n = allocations_during(|| {
            for _ in 0..100 {
                assert_eq!(ctx.peek("exp").unwrap(), variant);
                assert_eq!(ctx.treatment("exp").unwrap(), variant);
            }
        });
        assert_eq!(n, 0, "rules: {} bytes", rules.len());
    }
}

#[test]
fn reads_after_rule_revalidation_do_not_allocate() {
    let mut ctx = context(&large_rules());
    let variant = ctx.treatment("exp").unwrap();
    ctx.set_attribute("unrelated", 1).unwrap();
    assert_eq!(ctx.peek("exp").unwrap(), variant);

    let n = allocations_during(|| {
        for _ in 0..100 {
            assert_eq!(ctx.treatment("exp").unwrap(), variant);
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn warmed_reads_of_covered_experiments_do_not_allocate() {
    for (holdout_seed_lo, uid) in [
        (111, "e791e240fcd3df7d238cfc285f475e8152fcc0ec"),
        (222, "u"),
    ] {
        let data: ContextData = serde_json::from_value(json!({
            "experiments": [{
                "id": 1, "name": "exp", "unitType": "session_id", "iteration": 1,
                "trafficSplit": [0.0, 1.0], "split": [0.5, 0.5], "seedHi": 3, "seedLo": 4,
                "variants": [{"config": null}, {"config": null}],
                "holdoutIds": [11, 12]
            }],
            "holdouts": [
                {"id": 11, "name": "h11", "iteration": 1, "seedHi": 13, "seedLo": holdout_seed_lo, "split": [0.1, 0.9]},
                {"id": 12, "name": "h12", "iteration": 1, "seedHi": 0, "seedLo": 3, "split": [0.3, 0.3, 0.4]}
            ]
        }))
        .unwrap();
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", uid).unwrap();
        let variant = ctx.treatment("exp").unwrap();
        let n = allocations_during(|| {
            for _ in 0..100 {
                assert_eq!(ctx.peek("exp").unwrap(), variant);
                assert_eq!(ctx.treatment("exp").unwrap(), variant);
            }
        });
        assert_eq!(n, 0, "uid {uid}");
    }
}

#[test]
fn warmed_reads_of_switchbacks_do_not_allocate() {
    let data: ContextData = serde_json::from_value(json!({"experiments": [{
        "id": 1, "name": "sb", "unitType": "store_id", "iteration": 1,
        "trafficSplit": [0.0, 1.0], "split": [0.5, 0.5], "seedHi": 3, "seedLo": 4,
        "variants": [{"config": "{\"k\":0}"}, {"config": "{\"k\":1}"}],
        "randomizationType": "switchback",
        "switchbackClusters": {"store_id": "city"},
        "switchbackStartAt": 1_704_067_200_000i64,
        "switchbackPeriodMs": 1_440_000_000_000i64
    }, {
        "id": 2, "name": "unit", "unitType": "store_id", "iteration": 1,
        "trafficSplit": [0.0, 1.0], "split": [0.5, 0.5], "seedHi": 3, "seedLo": 4,
        "variants": [{"config": null}, {"config": null}]
    }]}))
    .unwrap();
    let mut ctx = Context::new(data);
    ctx.set_unit("store_id", "s1").unwrap();
    ctx.set_attribute("city", "Lisboa").unwrap();
    let variant = ctx.treatment("sb").unwrap();
    let unit = ctx.treatment("unit").unwrap();
    let n = allocations_during(|| {
        for _ in 0..100 {
            assert_eq!(ctx.peek("sb").unwrap(), variant);
            assert_eq!(ctx.treatment("sb").unwrap(), variant);
            assert_eq!(ctx.treatment("unit").unwrap(), unit);
        }
    });
    assert_eq!(n, 0);
}

// A warmed variable read clones only the candidate it visits, not every
// experiment listed for the key.
#[test]
fn warmed_variable_reads_do_not_clone_every_candidate() {
    let experiments: Vec<_> = (1..=8)
        .map(|id| {
            json!({
                "id": id, "name": format!("exp_{id}"), "unitType": "session_id", "iteration": 1,
                "trafficSplit": [0.0, 1.0], "split": [0.0, 1.0], "seedHi": 3, "seedLo": 4,
                "variants": [{"config": "{\"k\":0}"}, {"config": "{\"k\":1}"}]
            })
        })
        .collect();
    let data: ContextData = serde_json::from_value(json!({ "experiments": experiments })).unwrap();
    let mut ctx = Context::new(data);
    ctx.set_unit("session_id", "u").unwrap();
    assert_eq!(ctx.variable_value("k", 9).unwrap(), json!(1));

    let n = allocations_during(|| {
        for _ in 0..100 {
            assert_eq!(ctx.peek_variable_value("k", 9).unwrap(), json!(1));
            assert_eq!(ctx.variable_value("k", 9).unwrap(), json!(1));
        }
    });
    assert!(n <= 200, "{n} allocations for 200 reads");
}

#[test]
fn reads_after_a_refresh_that_adds_a_variant_do_not_allocate() {
    let rules = large_rules_ending_with(json!({"type": "assign", "variant": 1}));
    let mut ctx = context(&rules);
    assert_eq!(ctx.treatment("exp").unwrap(), 1);
    ctx.refresh_with(context_data(&rules, 3));
    assert_eq!(ctx.peek("exp").unwrap(), 1);

    let n = allocations_during(|| {
        for _ in 0..100 {
            assert_eq!(ctx.treatment("exp").unwrap(), 1);
        }
    });
    assert_eq!(n, 0);
}

/// A switchback slot revalidated with the same outcome is stamped, so later
/// reads neither re-evaluate its rules nor allocate: after an unrelated
/// attribute, after a refresh that keeps the rule outcome, and after one that
/// adds a variant.
#[test]
fn switchback_reads_after_revalidation_do_not_allocate() {
    let experiment = |rules: &str, variants: usize| {
        json!({
            "id": 1, "name": "sb", "unitType": "store_id", "iteration": 1,
            "trafficSplit": [0.0, 1.0], "split": vec![1.0 / variants as f64; variants],
            "seedHi": 3, "seedLo": 4,
            "variants": vec![json!({"config": null}); variants],
            "assignmentRules": rules,
            "randomizationType": "switchback",
            "switchbackClusters": {"store_id": "city"},
            "switchbackStartAt": 1_704_067_200_000i64,
            "switchbackPeriodMs": 1_440_000_000_000i64
        })
    };
    // Large rules whose last, unconditional rule assigns variant 1, so adding
    // a variant keeps the outcome.
    let mut rules: Value = serde_json::from_str(&large_rules()).unwrap();
    *rules["rules"].as_array_mut().unwrap().last_mut().unwrap() =
        json!({"type": "assign", "variant": 1});
    let rules = rules.to_string();
    let data = |rules: &str, variants: usize| -> ContextData {
        serde_json::from_value(json!({ "experiments": [experiment(rules, variants)] })).unwrap()
    };
    let mut ctx = Context::new(data(&rules, 2));
    ctx.set_unit("store_id", "s1").unwrap();
    ctx.set_attribute("city", "Lisboa").unwrap();
    ctx.treatment("sb").unwrap();

    let other_rules = rules.replace("t199", "t-other");
    for step in 0..3 {
        match step {
            0 => ctx.set_attribute("unrelated", 1).unwrap(),
            1 => ctx.refresh_with(data(&other_rules, 2)),
            _ => ctx.refresh_with(data(&other_rules, 3)),
        }
        let variant = ctx.treatment("sb").unwrap();
        assert_eq!(variant, 1);
        let n = allocations_during(|| {
            for _ in 0..100 {
                assert_eq!(ctx.peek("sb").unwrap(), variant);
                assert_eq!(ctx.treatment("sb").unwrap(), variant);
            }
        });
        assert_eq!(n, 0, "step {}", step);
    }
}

#[test]
fn rule_revalidation_skips_the_audience() {
    let audience = json!({"filter": [{"eq": [{"var": "country"}, {"value": "US"}]}]}).to_string();
    let revalidation_allocations = |audience: &str| {
        let mut ctx = context_with_audience(&large_rules(), audience);
        ctx.set_attribute("country", "US").unwrap();
        let variant = ctx.treatment("exp").unwrap();
        ctx.set_attribute("country", "CA").unwrap();
        allocations_during(|| assert_eq!(ctx.peek("exp").unwrap(), variant))
    };
    assert_eq!(
        revalidation_allocations(&audience),
        revalidation_allocations("")
    );
}
