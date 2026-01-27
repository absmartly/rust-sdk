use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

fn deserialize_null_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
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
    #[serde(default)]
    pub traffic_seed_hi: u32,
    #[serde(default)]
    pub traffic_seed_lo: u32,
    #[serde(default, deserialize_with = "deserialize_null_string")]
    pub audience: String,
    #[serde(default)]
    pub audience_strict: bool,
    #[serde(default)]
    pub split: Vec<f64>,
    #[serde(default)]
    pub seed_hi: u32,
    #[serde(default)]
    pub seed_lo: u32,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub variables: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub custom_field_values: Option<Vec<CustomFieldValue>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextState {
    Loading,
    Ready,
    Failed,
    Finalizing,
    Finalized,
}

impl Default for ContextState {
    fn default() -> Self {
        Self::Loading
    }
}

#[derive(Debug, Clone)]
pub struct ContextParams {
    pub units: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct ContextOptions {
    pub publish_delay: i64,
    pub refresh_period: i64,
}
