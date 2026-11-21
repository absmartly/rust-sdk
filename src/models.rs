use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

fn deserialize_null_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// `assignmentRules` arrives as a JSON string. An object or array in its
/// place is ignored with a warning, so only that experiment's rules are
/// skipped instead of failing the whole context; other scalars are kept in
/// their string form.
fn deserialize_assignment_rules<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s,
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => {
            log::warn!(
                "Ignoring assignmentRules: expected a JSON string but got {}",
                if other.is_object() {
                    "an object"
                } else {
                    "an array"
                }
            );
            String::new()
        }
    })
}

/// The backend serializes seed fields as signed 32-bit integers (their raw
/// two's-complement bit pattern), so values are frequently negative. Decode as
/// i32 and reinterpret the bits as u32 rather than rejecting negative values.
/// Unsigned values (as this crate serializes the field) are accepted too, so
/// serialized context data can be loaded again.
fn deserialize_seed<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let v = i64::deserialize(deserializer)?;
    if v < i64::from(i32::MIN) || v > i64::from(u32::MAX) {
        return Err(serde::de::Error::custom(format!(
            "seed {} is out of the 32-bit range",
            v
        )));
    }
    Ok(v as u32)
}

/// `holdouts` is lenient: `null` or a non-array means none. Each entry is
/// read field by field, keeping only what holdout resolution uses (`id`,
/// `name`, `unitType`, `iteration`, `seedHi`, `seedLo`, `split` and
/// `holdoutType`), so a malformed field the SDK never reads cannot drop the
/// holdout and let its units into the experiments it covers. An entry
/// without an integral `id` is dropped.
fn deserialize_holdouts<'de, D>(deserializer: D) -> Result<Vec<ExperimentData>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Array(items) => items.iter().filter_map(holdout_from_value).collect(),
        _ => Vec::new(),
    })
}

fn integral(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15)
            .map(|f| f as i64)
    })
}

/// Numeric seeds use JS's ToUint32: truncate and wrap modulo 2^32.
fn seed(value: &serde_json::Value) -> u32 {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .map_or(0, |v| v.trunc().rem_euclid(4_294_967_296.0) as u32)
}

fn holdout_from_value(item: &serde_json::Value) -> Option<ExperimentData> {
    let string = |key: &str| item.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let field = |key: &str| item.get(key).unwrap_or(&serde_json::Value::Null);
    Some(ExperimentData {
        id: integral(field("id"))?,
        name: string("name").unwrap_or_default(),
        unit_type: string("unitType"),
        iteration: integral(field("iteration")).unwrap_or(0),
        full_on_variant: 0,
        traffic_split: Vec::new(),
        traffic_seed_hi: 0,
        traffic_seed_lo: 0,
        audience: String::new(),
        audience_strict: false,
        split: field("split")
            .as_array()
            .and_then(|split| split.iter().map(|v| v.as_f64()).collect())
            .unwrap_or_default(),
        seed_hi: seed(field("seedHi")),
        seed_lo: seed(field("seedLo")),
        variants: Vec::new(),
        variables: HashMap::new(),
        custom_field_values: None,
        assignment_rules: String::new(),
        holdout_ids: Vec::new(),
        holdout_type: string("holdoutType"),
        ..Default::default()
    })
}

/// `holdoutIds` keeps the integral ids (`11` or `11.0`) and ignores
/// anything else, so a malformed list only loses its bad entries.
fn deserialize_holdout_ids<'de, D>(deserializer: D) -> Result<Vec<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Array(items) => items.iter().filter_map(integral).collect(),
        _ => Vec::new(),
    })
}

fn deserialize_lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
}

/// An integral number (`3600000` or `3600000.0`) within the `i64` range;
/// anything else reads as `None`.
fn deserialize_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(integral(&serde_json::Value::deserialize(deserializer)?))
}

/// `switchbackClusters` keeps the JSON object's key order, which decides the
/// serving unit. It is read entry by entry rather than through a
/// `serde_json::Value`, whose map is sorted unless serde_json's
/// `preserve_order` feature is on. A repeated key keeps its first position
/// and its last path, as a `LinkedHashMap` does. A path that is not a
/// string makes the whole map unreadable, which (like an empty map) takes the
/// legacy path, so it reads as an empty map; a non-object reads as `None`.
fn deserialize_switchback_clusters<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<(String, String)>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ClustersVisitor;

    impl<'de> serde::de::Visitor<'de> for ClustersVisitor {
        type Value = Option<Vec<(String, String)>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object of unit type names to attribute paths")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut clusters: Vec<(String, String)> = Vec::new();
            let mut readable = true;
            while let Some((unit_type, path)) = map.next_entry::<String, serde_json::Value>()? {
                let serde_json::Value::String(path) = path else {
                    readable = false;
                    continue;
                };
                match clusters.iter_mut().find(|(t, _)| *t == unit_type) {
                    Some(entry) => entry.1 = path,
                    None => clusters.push((unit_type, path)),
                }
            }
            Ok(Some(if readable { clusters } else { Vec::new() }))
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(self)
        }
    }

    deserializer.deserialize_any(ClustersVisitor)
}

fn serialize_switchback_clusters<S>(
    clusters: &Option<Vec<(String, String)>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    match clusters {
        None => serializer.serialize_none(),
        Some(clusters) => {
            let mut map = serializer.serialize_map(Some(clusters.len()))?;
            for (unit_type, path) in clusters {
                map.serialize_entry(unit_type, path)?;
            }
            map.end()
        }
    }
}

/// `switchbackClusterValues` is a list of strings. Numbers and booleans are
/// kept as their JSON text, as Jackson coerces them into a `String[]`;
/// `null`, objects and arrays are skipped. A non-array reads as `None`.
fn deserialize_cluster_values<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Array(items) => Some(
            items
                .into_iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextData {
    #[serde(default)]
    pub experiments: Vec<ExperimentData>,
    /// The holdouts that experiments reference through
    /// [`ExperimentData::holdout_ids`]. Each one is an experiment-shaped
    /// entry, of which only `id`, `name`, `unitType`, `iteration`, `seedHi`,
    /// `seedLo`, `split` and `holdoutType` are kept; an entry with an empty
    /// `split` is ignored.
    #[serde(
        default,
        deserialize_with = "deserialize_holdouts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub holdouts: Vec<ExperimentData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentData {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub unit_type: Option<String>,
    #[serde(default)]
    pub iteration: i64,
    #[serde(default)]
    pub full_on_variant: i64,
    #[serde(default)]
    pub traffic_split: Vec<f64>,
    #[serde(default, deserialize_with = "deserialize_seed")]
    pub traffic_seed_hi: u32,
    #[serde(default, deserialize_with = "deserialize_seed")]
    pub traffic_seed_lo: u32,
    #[serde(default, deserialize_with = "deserialize_null_string")]
    pub audience: String,
    #[serde(default)]
    pub audience_strict: bool,
    #[serde(default)]
    pub split: Vec<f64>,
    #[serde(default, deserialize_with = "deserialize_seed")]
    pub seed_hi: u32,
    #[serde(default, deserialize_with = "deserialize_seed")]
    pub seed_lo: u32,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub variables: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub custom_field_values: Option<Vec<CustomFieldValue>>,
    /// The experiment's custom assignment rules, as the JSON string the API
    /// sends (empty when there are none).
    #[serde(
        default,
        deserialize_with = "deserialize_assignment_rules",
        skip_serializing_if = "String::is_empty"
    )]
    pub assignment_rules: String,
    /// The ids of the [`ContextData::holdouts`] covering this experiment.
    /// Ids with no matching holdout are ignored.
    #[serde(
        default,
        deserialize_with = "deserialize_holdout_ids",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub holdout_ids: Vec<i64>,
    /// Set on holdout entries (`"full"` or `"all_full_on"`). Informational
    /// only: the number of arms is read from `split`.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub holdout_type: Option<String>,
    /// `"switchback"` makes this a switchback experiment; `"unit"` or
    /// anything else (or absent) is a unit experiment.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub randomization_type: Option<String>,
    /// A switchback's sources: unit type name and the attribute path holding
    /// its cluster value, **in the served order**. The first unit (in this
    /// order) whose path gives a value serves the experiment, so the order is
    /// kept exactly as received.
    ///
    /// The order survives decoding from JSON text or bytes
    /// (`serde_json::from_str`, `from_slice`, `from_reader`, and the
    /// [`crate::Client`] fetch). A `serde_json::Value` object has already
    /// sorted its keys (serde_json's `preserve_order` feature is off), so
    /// `serde_json::from_value` sees them sorted and can serve another unit.
    /// For the same reason the `ready` and `refresh` event data, which are
    /// `Value`s, list the map sorted.
    #[serde(
        default,
        deserialize_with = "deserialize_switchback_clusters",
        serialize_with = "serialize_switchback_clusters",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_clusters: Option<Vec<(String, String)>>,
    /// The cluster values a switchback serves; `None` or empty serves every
    /// value.
    #[serde(
        default,
        deserialize_with = "deserialize_cluster_values",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_cluster_values: Option<Vec<String>>,
    /// The switchback's anchor, in epoch milliseconds. It must be hour
    /// aligned and before year 10000, or the schedule is unservable.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_i64",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_start_at: Option<i64>,
    /// The switchback's period length in milliseconds.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_i64",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_period_ms: Option<i64>,
    /// Analysis-only; carried but not used for assignment.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_i64",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_carryover_duration_ms: Option<i64>,
    /// Analysis-only; carried but not used for assignment.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_i64",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_burn_in_duration_ms: Option<i64>,
    /// Analysis-only; carried but not used for assignment.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_i64",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_burn_out_duration_ms: Option<i64>,
    /// The switchback design, for example `"balanced_block"`.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_design_type: Option<String>,
    /// Analysis-only; carried but not used for assignment.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub switchback_estimand: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Variant {
    #[serde(default)]
    pub config: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFieldValue {
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub field_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct Assignment {
    pub id: i64,
    pub iteration: i64,
    pub full_on_variant: i64,
    pub unit_type: Option<String>,
    pub variant: i32,
    pub overridden: bool,
    pub assigned: bool,
    pub exposed: bool,
    pub eligible: bool,
    pub full_on: bool,
    pub custom: bool,
    pub audience_mismatch: bool,
    pub traffic_split: Option<Vec<f64>>,
    pub variables: Option<HashMap<String, serde_json::Value>>,
    pub attrs_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exposure {
    pub id: i64,
    pub name: String,
    pub exposed_at: i64,
    pub unit: Option<String>,
    pub variant: i32,
    pub assigned: bool,
    pub eligible: bool,
    pub overridden: bool,
    pub full_on: bool,
    pub custom: bool,
    pub audience_mismatch: bool,
    /// The variant was set by a matching assignment rule. Flagged so the
    /// backend can exclude it from analysis, as it does for overrides.
    #[serde(default)]
    pub rule_override: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribute {
    pub name: String,
    pub value: serde_json::Value,
    pub set_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub name: String,
    pub properties: Option<HashMap<String, serde_json::Value>>,
    pub achieved_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    #[serde(rename = "type")]
    pub unit_type: String,
    pub uid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishParams {
    pub published_at: i64,
    pub units: Vec<Unit>,
    pub hashed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposures: Option<Vec<Exposure>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goals: Option<Vec<Goal>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<Attribute>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextState {
    #[default]
    Loading,
    Ready,
    Failed,
    Finalizing,
    Finalized,
}

#[derive(Debug, Clone)]
pub struct ContextParams {
    pub units: HashMap<String, String>,
}

/// Event logger callback: invoked with the context, the event name, and optional data.
pub type EventLogger =
    std::sync::Arc<dyn Fn(&crate::context::Context, &str, Option<serde_json::Value>) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ContextOptions {
    pub publish_delay: i64,
    pub refresh_period: i64,
    pub event_logger: Option<EventLogger>,
}

impl std::fmt::Debug for ContextOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextOptions")
            .field("publish_delay", &self.publish_delay)
            .field("refresh_period", &self.refresh_period)
            .field(
                "event_logger",
                &self.event_logger.as_ref().map(|_| "<closure>"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negative_seed_round_trips_through_serialization() {
        let json = r#"{"experiments":[{"id":1,"name":"test","seedHi":-1,"seedLo":-2147483648,"trafficSeedHi":7,"trafficSeedLo":-5}]}"#;
        let data: ContextData = serde_json::from_str(json).unwrap();
        assert_eq!(data.experiments[0].seed_hi, u32::MAX);
        let again: ContextData =
            serde_json::from_str(&serde_json::to_string(&data).unwrap()).unwrap();
        let (a, b) = (&data.experiments[0], &again.experiments[0]);
        assert_eq!(
            (a.seed_hi, a.seed_lo, a.traffic_seed_hi, a.traffic_seed_lo),
            (b.seed_hi, b.seed_lo, b.traffic_seed_hi, b.traffic_seed_lo)
        );
        assert!(serde_json::from_str::<ContextData>(
            r#"{"experiments":[{"id":1,"name":"t","seedHi":4294967296}]}"#
        )
        .is_err());
    }

    #[test]
    fn test_assignment_rules_deserialization() {
        let parse = |rules: &str| -> String {
            let json = format!(
                r#"{{"experiments":[{{"id":1,"name":"t","assignmentRules":{}}}]}}"#,
                rules
            );
            serde_json::from_str::<ContextData>(&json)
                .unwrap()
                .experiments[0]
                .assignment_rules
                .clone()
        };
        assert_eq!(parse(r#""{\"rules\":[]}""#), r#"{"rules":[]}"#);
        assert_eq!(parse("null"), "");
        assert_eq!(parse(r#"{"rules":[]}"#), "");
        assert_eq!(parse("[]"), "");
        assert_eq!(parse("5"), "5");
        let missing: ContextData =
            serde_json::from_str(r#"{"experiments":[{"id":1,"name":"t"}]}"#).unwrap();
        assert_eq!(missing.experiments[0].assignment_rules, "");
    }

    #[test]
    fn test_context_options_debug_without_logger() {
        let opts = ContextOptions {
            publish_delay: 100,
            refresh_period: 200,
            event_logger: None,
        };
        let debug_str = format!("{:?}", opts);
        assert!(debug_str.contains("publish_delay: 100"));
        assert!(debug_str.contains("refresh_period: 200"));
        assert!(debug_str.contains("None"));
    }

    #[test]
    fn test_context_options_debug_with_logger() {
        let opts = ContextOptions {
            publish_delay: 100,
            refresh_period: 200,
            event_logger: Some(std::sync::Arc::new(|_ctx, _event, _data| {})),
        };
        let debug_str = format!("{:?}", opts);
        assert!(debug_str.contains("<closure>"));
    }
}
