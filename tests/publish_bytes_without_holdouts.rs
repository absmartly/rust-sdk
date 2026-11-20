//! A payload without holdouts publishes exactly what 0.2.0 did before
//! holdouts were added. The golden body below was recorded on main
//! (`7c26910`) with this same test; only timestamps are normalised.

use absmartly::{Context, ContextData, PublishParams};
use serde_json::{json, Value};

const GOLDEN: &str = include_str!("data/publish_without_holdouts.json");

fn experiment(id: i64, name: &str, full_on_variant: i64, audience: Value) -> Value {
    json!({
        "id": id, "name": name, "unitType": "session_id", "iteration": 1,
        "seedHi": 100 + id, "seedLo": 200 + id, "split": [0.34, 0.33, 0.33],
        "trafficSeedHi": 1, "trafficSeedLo": 2, "trafficSplit": [0.2, 0.8],
        "fullOnVariant": full_on_variant, "audience": audience, "audienceStrict": true,
        "variants": [
            {"name": "A", "config": null},
            {"name": "B", "config": "{\"color\":\"blue\"}"},
            {"name": "C", "config": "{\"color\":\"red\"}"}
        ]
    })
}

fn normalised(mut params: PublishParams) -> PublishParams {
    params.published_at = 0;
    for e in params.exposures.iter_mut().flatten() {
        e.exposed_at = 0;
    }
    for g in params.goals.iter_mut().flatten() {
        g.achieved_at = 0;
    }
    for a in params.attributes.iter_mut().flatten() {
        a.set_at = 0;
    }
    params
}

#[test]
fn publish_body_without_holdouts_is_unchanged() {
    let data: ContextData = serde_json::from_value(json!({"experiments": [
        experiment(1, "exp_a", 0, Value::Null),
        experiment(2, "exp_b", 2, Value::Null),
        experiment(3, "exp_c", 0, json!("{\"filter\":[{\"gte\":[{\"var\":\"age\"},{\"value\":20}]}]}")),
        experiment(4, "exp_d", 0, Value::Null)
    ]}))
    .unwrap();

    let mut bodies = Vec::new();
    for uid in [
        "ascii",
        "  padded  ",
        "façade",
        "日本語",
        "🚀",
        "0",
        "-1",
        "12345678901234567",
    ] {
        let mut ctx = Context::new(data.clone());
        ctx.set_unit("session_id", uid).unwrap();
        ctx.set_attribute("age", 25).unwrap();
        ctx.set_override("exp_d", 1).unwrap();
        for name in ["exp_a", "exp_b", "exp_c", "exp_d", "missing"] {
            ctx.treatment(name).unwrap();
        }
        ctx.variable_value("color", "none").unwrap();
        ctx.track("goal", json!({"amount": 1})).unwrap();
        let params = ctx.get_publish_params();
        bodies.push(normalised(params));
    }
    let actual = serde_json::to_string_pretty(&bodies).unwrap() + "\n";

    if std::env::var_os("RECORD_GOLDEN").is_some() {
        std::fs::write("tests/data/publish_without_holdouts.json", &actual).unwrap();
    }
    // Byte-for-byte, so field order and formatting are pinned too. Only CR
    // is dropped, for a CRLF checkout of the golden file (Windows).
    assert_eq!(actual, GOLDEN.replace('\r', ""));
}
