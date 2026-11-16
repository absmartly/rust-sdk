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
