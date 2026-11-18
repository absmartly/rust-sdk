//! Warmed cached reads must not copy the assignment or the experiment's
//! assignment rules: once an assignment is current, `peek`/`treatment` do no
//! heap work, with or without rules and after a rule revalidation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use absmartly::{Context, ContextData};
use serde_json::json;

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

fn context(assignment_rules: &str) -> Context {
    let large_config = json!({"payload": "x".repeat(10_000), "list": vec![1; 500]}).to_string();
    let data: ContextData = serde_json::from_value(json!({"experiments": [{
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
        "variants": [{"config": large_config}, {"config": large_config}],
        "assignmentRules": assignment_rules
    }]}))
    .unwrap();
    let mut ctx = Context::new(data);
    ctx.set_system_attributes(Some("website"), Some("production"));
    ctx.set_unit("session_id", "user").unwrap();
    ctx
}

fn large_rules() -> String {
    let mut rules: Vec<_> = (0..200)
        .map(|i| json!({"type": "assign", "variant": 1, "conditions": {"eq": [{"var": "tier"}, {"value": format!("t{}", i)}]}}))
        .collect();
    rules.push(json!({"type": "split", "percentages": "50/50"}));
    json!({ "rules": rules }).to_string()
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
