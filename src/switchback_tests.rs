//! Ported from java-sdk's `SwitchbackAssignerGoldenVectorsTest`,
//! `SwitchbackAttributePathTest` and `SwitchbackServingTest`.

use super::*;
use serde_json::json;

// Copied byte for byte from absmartly/abs 9bd2b5ca35
// lib/utils/src/test/resources/switchback/golden-vectors.json (git blob b314abeb9e97).
const GOLDEN_VECTORS: &str = include_str!("../tests/data/switchback/golden-vectors.json");
// Copied byte for byte from absmartly/abs 9bd2b5ca35
// collector/src/test/resources/switchback/attribute-path-cases.json (git blob 7b1d3fed5985).
const PATH_CASES: &str = include_str!("../tests/data/switchback/attribute-path-cases.json");

/// `git hash-object`: SHA-1 over `"blob <len>\0"` and the bytes.
fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut data = format!("blob {}\0", bytes.len()).into_bytes();
    data.extend_from_slice(bytes);
    sha1_hex(&data)
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in message.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{:08x}", x)).collect()
}

#[test]
fn vendored_fixtures_are_the_platform_blobs() {
    assert_eq!(
        git_blob_sha1(GOLDEN_VECTORS.as_bytes()),
        "b314abeb9e97b35f911c7caacec90b8ff73b9d77"
    );
    assert_eq!(
        git_blob_sha1(PATH_CASES.as_bytes()),
        "7b1d3fed59856fcbc296ed68a820b43359ae1932"
    );
}

fn vectors() -> Value {
    serde_json::from_str(GOLDEN_VECTORS).unwrap()
}

/// The platform keys a numeric cluster unit as its decimal text.
fn cluster_unit(node: &Value) -> String {
    match node {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() => n.to_string(),
        other => panic!("clusterUnit must be a string or integer: {}", other),
    }
}

/// 64-bit fields are decimal strings; a number would be a lossy fixture.
fn exact_i64(node: &Value) -> i64 {
    node.as_str()
        .expect("64-bit field must be a decimal string")
        .parse()
        .unwrap()
}

#[test]
fn assign_switchback_matches_every_golden_vector() {
    let cases = vectors()["assignSwitchback"].as_array().unwrap().clone();
    assert_eq!(cases.len(), 882);
    let mut arm_counts = std::collections::BTreeSet::new();
    let mut boundary_cases = 0;
    for c in &cases {
        let period_index = exact_i64(&c["periodIndex"]);
        let seed = exact_i64(&c["seed"]);
        let arm_count = c["armCount"].as_u64().unwrap() as usize;
        arm_counts.insert(arm_count);
        if [period_index, seed]
            .iter()
            .any(|v| *v == i64::MIN || *v == i64::MAX)
        {
            boundary_cases += 1;
        }
        assert_eq!(
            assign_switchback(
                &cluster_unit(&c["clusterUnit"]),
                period_index,
                seed,
                arm_count
            ),
            Some(c["expectedVariant"].as_u64().unwrap() as usize),
            "{}",
            c
        );
    }
    assert_eq!(arm_counts.into_iter().collect::<Vec<_>>(), vec![2, 3, 10]);
    assert!(boundary_cases > 0);
}

#[test]
fn fisher_yates_permutation_matches_every_golden_vector() {
    let cases = vectors()["fisherYatesPermutation"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cases.len(), 504);
    for c in &cases {
        let arm_count = c["armCount"].as_u64().unwrap() as usize;
        let expected: Vec<usize> = c["expectedPermutation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();
        let hash = base_hash(
            &cluster_unit(&c["clusterUnit"]),
            exact_i64(&c["cycleIndex"]),
            exact_i64(&c["seed"]),
        );
        assert_eq!(
            fisher_yates_permutation(arm_count, hash),
            Some(expected),
            "{}",
            c
        );
    }
}

#[test]
fn every_cycle_covers_every_arm_exactly_once() {
    for arm_count in [2usize, 3, 10] {
        for cycle in -3i64..20 {
            let mut seen = vec![false; arm_count];
            for slot in 0..arm_count as i64 {
                let variant = assign_switchback(
                    "store_42",
                    cycle * arm_count as i64 + slot,
                    0x1234_5678_9abc,
                    arm_count,
                )
                .unwrap();
                assert!(!seen[variant], "arm repeated within a cycle");
                seen[variant] = true;
            }
        }
    }
}

#[test]
fn rejects_fewer_than_two_arms() {
    assert_eq!(assign_switchback("u", 0, 0, 1), None);
    assert_eq!(assign_switchback("u", 0, 0, 0), None);
    assert_eq!(fisher_yates_permutation(0, 0), None);
    assert_eq!(fisher_yates_permutation(1, 0), None);
}

#[test]
fn floor_div_and_mod_at_the_i64_bounds() {
    // The period index is floor-divided by the arm count, so a negative
    // index lands in the previous cycle at the slot floorMod gives.
    assert_eq!(
        assign_switchback("market_1", -1, 0, 2),
        assign_switchback("market_1", -1, 0, 2)
    );
    for (period, arms, cycle, slot) in [
        (0i64, 3i64, 0i64, 0i64),
        (2, 3, 0, 2),
        (3, 3, 1, 0),
        (-1, 3, -1, 2),
        (-3, 3, -1, 0),
        (-4, 3, -2, 2),
        (i64::MIN, 3, i64::MIN.div_euclid(3), 1),
        (i64::MAX, 10, i64::MAX / 10, 7),
    ] {
        let permutation =
            fisher_yates_permutation(arms as usize, base_hash("k", cycle, 5)).unwrap();
        assert_eq!(
            assign_switchback("k", period, 5, arms as usize),
            Some(permutation[slot as usize]),
            "{} / {}",
            period,
            arms
        );
    }
}

#[test]
fn matches_every_path_case() {
    let fixture: Value = serde_json::from_str(PATH_CASES).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 7);
    for c in cases {
        let path = AttributePath::parse(c["path"].as_str().unwrap());
        if !c["supported"].as_bool().unwrap() {
            assert_eq!(path, None, "{}", c);
            continue;
        }
        let path = path.unwrap_or_else(|| panic!("{}", c));
        let expected = Some(&c["value"]).filter(|v| !v.is_null());
        assert_eq!(path.resolve(&fixture["attributes"]), expected, "{}", c);
    }
}

#[test]
fn quoted_segments_are_literal_keys() {
    let attributes = json!({"geo": {"0": "zero", "a/b": "slash"}});
    assert_eq!(
        AttributePath::parse("geo/'0'")
            .unwrap()
            .resolve(&attributes),
        Some(&json!("zero"))
    );
    assert_eq!(
        AttributePath::parse("geo/\"a/b\"")
            .unwrap()
            .resolve(&attributes),
        Some(&json!("slash"))
    );
}

#[test]
fn rejects_malformed_paths() {
    for path in [
        "geo/'0",
        "'geo",
        "\"geo/city",
        "geo/\"",
        "geo.city",
        "geo/a.b",
        "a./b",
        ".",
        "items/0.5/city",
        "'geo.city'",
        "\"geo.city\"",
        "geo/'a.b'",
    ] {
        assert_eq!(AttributePath::parse(path), None, "{}", path);
    }
}

#[test]
fn separators_are_collapsed() {
    let attributes = json!({"geo": {"city": "Porto"}});
    assert_eq!(
        AttributePath::parse("/geo//city/")
            .unwrap()
            .resolve(&attributes),
        Some(&json!("Porto"))
    );
    assert_eq!(
        AttributePath::parse("geo").unwrap().resolve(&attributes),
        Some(&attributes["geo"])
    );
}

#[test]
fn rejects_unsupported_paths() {
    for path in [
        "",
        "/",
        "//",
        "0/city",
        "items/0",
        "items/0/1/city",
        "items/-1/city",
        "'",
    ] {
        assert_eq!(AttributePath::parse(path), None, "{}", path);
    }
}

#[test]
fn index_segments_read_only_arrays_and_key_segments_read_only_objects() {
    let attributes = json!({
        "items": [{"city": "Lisbon"}],
        "geo": {"0": {"city": "zero"}},
        "name": "scalar"
    });
    for path in ["geo/0/city", "items/1/city", "name/x", "missing/x"] {
        assert_eq!(
            AttributePath::parse(path).unwrap().resolve(&attributes),
            None,
            "{}",
            path
        );
    }
    assert_eq!(
        AttributePath::parse("items/-0/city")
            .unwrap()
            .resolve(&attributes),
        Some(&json!("Lisbon"))
    );
}

#[test]
fn resolve_attribute_reads_below_the_root_key() {
    let path = AttributePath::parse("geo/city").unwrap();
    assert_eq!(path.root_key(), "geo");
    assert_eq!(
        path.resolve_attribute(&json!({"city": "Porto"})),
        Some(&json!("Porto"))
    );
    assert_eq!(path.resolve_attribute(&json!("Porto")), None);
    let root = AttributePath::parse("'geo'").unwrap();
    assert_eq!(
        root.resolve_attribute(&json!("Porto")),
        Some(&json!("Porto"))
    );
}

#[test]
fn try_parse_int_follows_guava() {
    assert_eq!(try_parse_int("0"), Some(0));
    assert_eq!(try_parse_int("-0"), Some(0));
    assert_eq!(try_parse_int("007"), Some(7));
    assert_eq!(try_parse_int("2147483647"), Some(i32::MAX));
    assert_eq!(try_parse_int("-2147483648"), Some(i32::MIN));
    for s in [
        "",
        "-",
        "+1",
        "1a",
        " 1",
        "2147483648",
        "-2147483649",
        "99999999999999999999",
        "\u{660}",
        "1.0",
        "--1",
    ] {
        assert_eq!(try_parse_int(s), None, "{}", s);
    }
}

#[test]
fn cluster_key_takes_non_empty_strings_and_integers_only() {
    for (value, expected) in [
        (json!("Lisboa"), Some("Lisboa")),
        (json!(" "), Some(" ")),
        (json!(42), Some("42")),
        (json!(-7), Some("-7")),
        (json!(0), Some("0")),
        (json!(4000000002u64), Some("4000000002")),
        (json!(i64::MIN), Some("-9223372036854775808")),
        (json!(u64::MAX), Some("18446744073709551615")),
        (json!(""), None),
        (json!(3.0), None),
        (json!(1.5), None),
        (json!(-0.0), None),
        (json!(true), None),
        (json!(false), None),
        (Value::Null, None),
        (json!({"a": "b"}), None),
        (json!(["x"]), None),
        (json!([]), None),
    ] {
        assert_eq!(cluster_key(&value).as_deref(), expected, "{}", value);
    }
    // Parsed from JSON text: `3` is an integer, `3.0` and `3e0` are not.
    for (text, expected) in [("3", Some("3")), ("3.0", None), ("3e0", None), ("-0", None)] {
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(cluster_key(&value).as_deref(), expected, "{}", text);
    }
}

fn sources_of(pairs: &[(&str, &str)]) -> Vec<(String, AttributePath)> {
    let clusters: Vec<(String, String)> = pairs
        .iter()
        .map(|(t, p)| (t.to_string(), p.to_string()))
        .collect();
    sources(Some(&clusters)).unwrap()
}

/// Serving over context `units` reading `attributes`.
fn serve(
    sources: &[(String, AttributePath)],
    allowed: Option<&[String]>,
    units: &[&str],
    attributes: &Value,
) -> Serving {
    resolve_serving(
        sources,
        allowed,
        |unit_type| units.contains(&unit_type),
        |path| cluster_key(path.resolve(attributes)?),
    )
}

fn eligible(unit_type: &str, key: &str) -> Serving {
    Serving::Eligible {
        unit_type: unit_type.to_string(),
        cluster_key: key.to_string(),
    }
}

fn ineligible(unit_type: &str) -> Serving {
    Serving::Ineligible {
        unit_type: unit_type.to_string(),
    }
}

// The declared source order decides, not the order of the units.
#[test]
fn source_order_decides_whatever_the_unit_order() {
    let sources = sources_of(&[("session_id", "session_store"), ("user_id", "user_store")]);
    let attributes = json!({"session_store": "s-1", "user_store": "s-1"});
    assert_eq!(
        serve(&sources, None, &["user_id", "session_id"], &attributes),
        eligible("session_id", "s-1")
    );
    let reversed = sources_of(&[("user_id", "user_store"), ("session_id", "session_store")]);
    assert_eq!(
        serve(&reversed, None, &["session_id", "user_id"], &attributes),
        eligible("user_id", "s-1")
    );
}

#[test]
fn serving_skips_units_without_a_value_and_flags_disagreement() {
    let sources = sources_of(&[("store_id", "store_city"), ("device_id", "geo/city")]);
    let both = ["store_id", "device_id"];
    assert_eq!(
        serve(&sources, None, &both, &json!({"geo": {"city": "Porto"}})),
        eligible("device_id", "Porto")
    );
    assert_eq!(
        serve(
            &sources,
            None,
            &both,
            &json!({"store_city": "Lisboa", "geo": {"city": "Porto"}})
        ),
        ineligible("store_id")
    );
    assert_eq!(
        serve(&sources, None, &both, &json!({})),
        ineligible("store_id")
    );
    assert_eq!(
        serve(
            &sources,
            None,
            &["device_id"],
            &json!({"store_city": "Lisboa", "geo": {"city": "Porto"}})
        ),
        eligible("device_id", "Porto")
    );
    assert_eq!(
        serve(&sources, None, &["user_id"], &json!({"store_city": "x"})),
        Serving::Absent
    );
    // 42 and "42" agree; "042" does not.
    assert_eq!(
        serve(
            &sources,
            None,
            &both,
            &json!({"store_city": 42, "geo": {"city": "42"}})
        ),
        eligible("store_id", "42")
    );
    assert_eq!(
        serve(
            &sources,
            None,
            &both,
            &json!({"store_city": 42, "geo": {"city": "042"}})
        ),
        ineligible("store_id")
    );
}

#[test]
fn allow_list_filters_the_cluster_key() {
    let sources = sources_of(&[("store_id", "store_city"), ("device_id", "geo/city")]);
    let both = ["store_id", "device_id"];
    let lisboa = json!({"store_city": "Lisboa"});
    let list = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    assert_eq!(
        serve(&sources, Some(&list(&["Porto", "Faro"])), &both, &lisboa),
        ineligible("store_id")
    );
    assert_eq!(
        serve(&sources, Some(&list(&["Porto", "Lisboa"])), &both, &lisboa),
        eligible("store_id", "Lisboa")
    );
    assert_eq!(
        serve(&sources, Some(&[]), &both, &lisboa),
        eligible("store_id", "Lisboa")
    );
    assert_eq!(
        serve(
            &sources,
            Some(&list(&["42"])),
            &both,
            &json!({"store_city": 42})
        ),
        eligible("store_id", "42")
    );
    // A filtered first value is not replaced by a later allowed one.
    assert_eq!(
        serve(
            &sources,
            Some(&list(&["Porto"])),
            &both,
            &json!({"store_city": "Lisboa", "geo": {"city": "Porto"}})
        ),
        ineligible("store_id")
    );
}

#[test]
fn sources_are_none_for_a_missing_empty_or_unreadable_map() {
    assert!(sources(None).is_none());
    assert!(sources(Some(&[])).is_none());
    let unreadable = vec![
        ("store_id".to_string(), "city".to_string()),
        ("device_id".to_string(), "'geo".to_string()),
    ];
    assert!(sources(Some(&unreadable)).is_none());
}

#[test]
fn legacy_serving_uses_the_declared_unit_type_only_for_an_ineligible_control() {
    let units = ["user_id"];
    let has = |t: &str| units.contains(&t);
    assert_eq!(legacy_serving(Some("user_id"), has), ineligible("user_id"));
    assert_eq!(legacy_serving(Some("store_id"), has), Serving::Absent);
    assert_eq!(legacy_serving(None, has), Serving::Absent);
}
