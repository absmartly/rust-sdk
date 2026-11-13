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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextData {
    #[serde(default)]
    pub experiments: Vec<ExperimentData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The variant was set by a matching assignment rule.
    pub rule_override: bool,
    /// A split rule matched but the unit was missing, so the full-on variant
    /// was used; setting the unit later re-randomizes it while unexposed.
    pub rule_missing_unit: bool,
    /// The `assignmentRules` this assignment was resolved against.
    pub rule_key: String,
    /// The experiment's variant count when the rules were resolved; a change
    /// can put a fixed variant out of range or invalidate a split.
    pub rule_variant_count: usize,
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
