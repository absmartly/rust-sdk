//! Data models for ABsmartly context, experiments, and event publishing.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

fn deserialize_null_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Top-level context data containing all experiment configurations.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextData {
    /// The list of experiments in this context.
    #[serde(default)]
    pub experiments: Vec<ExperimentData>,
}

/// Configuration data for a single experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentData {
    /// Unique experiment identifier.
    pub id: i64,
    /// Human-readable experiment name.
    pub name: String,
    /// The unit type used for assignment (e.g., `session_id`).
    #[serde(default)]
    pub unit_type: Option<String>,
    /// Current iteration of the experiment.
    #[serde(default)]
    pub iteration: i64,
    /// If non-zero, all users are assigned to this variant.
    #[serde(default)]
    pub full_on_variant: i64,
    /// Traffic allocation split weights.
    #[serde(default)]
    pub traffic_split: Vec<f64>,
    /// High bits of the traffic assignment seed.
    #[serde(default)]
    pub traffic_seed_hi: u32,
    /// Low bits of the traffic assignment seed.
    #[serde(default)]
    pub traffic_seed_lo: u32,
    /// JSON audience filter expression.
    #[serde(default, deserialize_with = "deserialize_null_string")]
    pub audience: String,
    /// When true, audience mismatches result in control assignment.
    #[serde(default)]
    pub audience_strict: bool,
    /// Variant assignment split weights.
    #[serde(default)]
    pub split: Vec<f64>,
    /// High bits of the variant assignment seed.
    #[serde(default)]
    pub seed_hi: u32,
    /// Low bits of the variant assignment seed.
    #[serde(default)]
    pub seed_lo: u32,
    /// Variant configurations.
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// Experiment-level variables.
    #[serde(default)]
    pub variables: HashMap<String, serde_json::Value>,
    /// Custom field values attached to the experiment.
    #[serde(default)]
    pub custom_field_values: Option<Vec<CustomFieldValue>>,
}

/// A variant's configuration payload.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Variant {
    /// JSON configuration string for this variant.
    #[serde(default)]
    pub config: Option<String>,
}

/// A custom field value associated with an experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFieldValue {
    /// Field name.
    pub name: String,
    /// Field value as a string.
    pub value: String,
    /// Field type (e.g., "text", "number", "json", "boolean").
    #[serde(rename = "type")]
    pub field_type: String,
}

/// Tracks the computed assignment for a unit in an experiment.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct Assignment {
    /// Experiment identifier.
    pub id: i64,
    /// Experiment iteration.
    pub iteration: i64,
    /// Full-on variant value from the experiment.
    pub full_on_variant: i64,
    /// The unit type used for this assignment.
    pub unit_type: Option<String>,
    /// The assigned variant index.
    pub variant: i32,
    /// Whether this assignment was overridden.
    pub overridden: bool,
    /// Whether the unit was successfully assigned.
    pub assigned: bool,
    /// Whether the assignment has been exposed (logged).
    pub exposed: bool,
    /// Whether the unit is eligible for the experiment.
    pub eligible: bool,
    /// Whether the experiment is in full-on mode.
    pub full_on: bool,
    /// Whether this is a custom assignment.
    pub custom: bool,
    /// Whether the audience filter did not match.
    pub audience_mismatch: bool,
    /// The traffic split at the time of assignment.
    pub traffic_split: Option<Vec<f64>>,
    /// Resolved variable values for the assigned variant.
    pub variables: Option<HashMap<String, serde_json::Value>>,
    /// Attribute sequence number at the time of assignment.
    pub attrs_seq: u64,
}

/// An exposure event recorded when a treatment is accessed.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exposure {
    /// Experiment identifier.
    pub id: i64,
    /// Experiment name.
    pub name: String,
    /// Timestamp of exposure in milliseconds since epoch.
    pub exposed_at: i64,
    /// The unit identifier.
    pub unit: Option<String>,
    /// The assigned variant index.
    pub variant: i32,
    /// Whether the unit was assigned.
    pub assigned: bool,
    /// Whether the unit was eligible.
    pub eligible: bool,
    /// Whether the assignment was overridden.
    pub overridden: bool,
    /// Whether the experiment is in full-on mode.
    pub full_on: bool,
    /// Whether this is a custom assignment.
    pub custom: bool,
    /// Whether the audience filter did not match.
    pub audience_mismatch: bool,
}

/// A user attribute with its value and timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribute {
    /// Attribute name.
    pub name: String,
    /// Attribute value.
    pub value: serde_json::Value,
    /// Timestamp when the attribute was set, in milliseconds since epoch.
    pub set_at: i64,
}

/// A goal achievement event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// Goal name.
    pub name: String,
    /// Optional properties associated with the goal.
    pub properties: Option<HashMap<String, serde_json::Value>>,
    /// Timestamp when the goal was achieved, in milliseconds since epoch.
    pub achieved_at: i64,
}

/// A unit identifier used in publish requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    /// The unit type (e.g., `session_id`).
    #[serde(rename = "type")]
    pub unit_type: String,
    /// The hashed unit identifier.
    pub uid: Option<String>,
}

/// Parameters sent when publishing context events to the ABsmartly API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishParams {
    /// Timestamp of publication in milliseconds since epoch.
    pub published_at: i64,
    /// Units involved in this publish.
    pub units: Vec<Unit>,
    /// Whether unit identifiers are hashed.
    pub hashed: bool,
    /// Exposure events to publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposures: Option<Vec<Exposure>>,
    /// Goal events to publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goals: Option<Vec<Goal>>,
    /// Attribute events to publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<Attribute>>,
}

/// The lifecycle state of a context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextState {
    /// Context is loading data.
    #[default]
    Loading,
    /// Context is ready for use.
    Ready,
    /// Context failed to initialize.
    Failed,
    /// Context is publishing final events.
    Finalizing,
    /// Context has been finalized and is no longer usable.
    Finalized,
}

/// Parameters for creating a new context.
#[derive(Debug, Clone)]
pub struct ContextParams {
    /// Map of unit type to unit identifier.
    pub units: HashMap<String, String>,
}

/// Options for context behavior.
#[derive(Debug, Clone, Default)]
pub struct ContextOptions {
    /// Delay in milliseconds before auto-publishing events.
    pub publish_delay: i64,
    /// Interval in milliseconds for automatic context refresh.
    pub refresh_period: i64,
}
