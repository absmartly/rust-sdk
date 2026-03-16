//! Experiment context for managing assignments, exposures, and goal tracking.

use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assigner::VariantAssigner;
use crate::matcher::AudienceMatcher;
use crate::models::{
    Assignment, Attribute, ContextData, ContextState, ExperimentData, Exposure, Goal,
    PublishParams, Unit,
};
use crate::utils::{array_equals_shallow, hash_unit};

/// Callback type for logging context events.
pub type EventLogger = Box<dyn Fn(&Context, &str, Option<Value>) + Send + Sync>;

struct Experiment {
    data: ExperimentData,
    variables: Vec<HashMap<String, Value>>,
}

impl std::fmt::Debug for Experiment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Experiment")
            .field("data", &self.data)
            .field("variables", &self.variables)
            .finish()
    }
}

/// The main context for interacting with ABsmartly experiments.
///
/// A context holds unit assignments, tracks exposures and goals, and resolves
/// experiment variables. Create one via [`SDK::create_context`] or
/// [`SDK::create_context_with`].
///
/// [`SDK::create_context`]: crate::sdk::SDK::create_context
/// [`SDK::create_context_with`]: crate::sdk::SDK::create_context_with
pub struct Context {
    units: HashMap<String, String>,
    attrs: Vec<Attribute>,
    data: ContextData,
    assignments: HashMap<String, Assignment>,
    exposures: Vec<Exposure>,
    goals: Vec<Goal>,
    overrides: HashMap<String, i32>,
    cassignments: HashMap<String, i32>,
    state: ContextState,
    pending: usize,
    attrs_seq: u64,
    index: HashMap<String, Experiment>,
    index_variables: HashMap<String, Vec<String>>,
    assigners: HashMap<String, VariantAssigner>,
    hashes: HashMap<String, String>,
    audience_matcher: AudienceMatcher,
    event_logger: Option<EventLogger>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("units", &self.units)
            .field("state", &self.state)
            .field("pending", &self.pending)
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

impl Context {
    /// Creates a new context initialized with the given experiment data.
    pub fn new(data: ContextData) -> Self {
        let mut ctx = Self {
            units: HashMap::new(),
            attrs: Vec::new(),
            data: ContextData::default(),
            assignments: HashMap::new(),
            exposures: Vec::new(),
            goals: Vec::new(),
            overrides: HashMap::new(),
            cassignments: HashMap::new(),
            state: ContextState::Loading,
            pending: 0,
            attrs_seq: 0,
            index: HashMap::new(),
            index_variables: HashMap::new(),
            assigners: HashMap::new(),
            hashes: HashMap::new(),
            audience_matcher: AudienceMatcher::new(),
            event_logger: None,
        };
        ctx.init(data);
        ctx.state = ContextState::Ready;
        ctx
    }

    /// Sets an event logger callback for observing context events.
    pub fn set_event_logger(&mut self, logger: EventLogger) {
        self.event_logger = Some(logger);
    }

    fn init(&mut self, data: ContextData) {
        self.data = data;
        self.index.clear();
        self.index_variables.clear();

        for experiment in &self.data.experiments {
            let mut variables: Vec<HashMap<String, Value>> = Vec::new();

            for variant in &experiment.variants {
                let parsed: HashMap<String, Value> = variant
                    .config
                    .as_ref()
                    .and_then(|c| {
                        if c.is_empty() {
                            None
                        } else {
                            serde_json::from_str(c).ok()
                        }
                    })
                    .unwrap_or_default();

                for key in parsed.keys() {
                    self.index_variables
                        .entry(key.clone())
                        .or_default()
                        .push(experiment.name.clone());
                }

                variables.push(parsed);
            }

            let _ = self.index.insert(
                experiment.name.clone(),
                Experiment {
                    data: experiment.clone(),
                    variables,
                },
            );
        }
    }

    /// Returns true if the context is ready for use.
    pub fn is_ready(&self) -> bool {
        self.state == ContextState::Ready
    }

    /// Returns true if the context failed to initialize.
    pub fn is_failed(&self) -> bool {
        self.state == ContextState::Failed
    }

    /// Returns true if the context has been finalized.
    pub fn is_finalized(&self) -> bool {
        self.state == ContextState::Finalized
    }

    /// Returns true if the context is currently finalizing.
    pub fn is_finalizing(&self) -> bool {
        self.state == ContextState::Finalizing
    }

    /// Returns the number of pending events waiting to be published.
    pub fn pending(&self) -> usize {
        self.pending
    }

    fn check_not_finalized(&self) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABSmartly Context is finalized.".to_owned());
        }
        if self.is_finalizing() {
            return Err("ABSmartly Context is finalizing.".to_owned());
        }
        Ok(())
    }

    /// Returns a reference to the context's experiment data.
    pub fn data(&self) -> &ContextData {
        &self.data
    }

    /// Sets a unit identifier for the given unit type.
    ///
    /// # Errors
    ///
    /// Returns an error if the context is finalized/finalizing, the UID is blank,
    /// or a different UID is already set for this unit type.
    pub fn set_unit(&mut self, unit_type: &str, uid: &str) -> Result<(), String> {
        self.check_not_finalized()?;

        let uid = uid.trim();
        if uid.is_empty() {
            return Err(format!("Unit '{unit_type}' UID must not be blank."));
        }

        if let Some(existing) = self.units.get(unit_type) {
            if existing != uid {
                return Err(format!("Unit '{unit_type}' UID already set."));
            }
        }

        let _ = self.units.insert(unit_type.to_owned(), uid.to_owned());
        Ok(())
    }

    /// Sets multiple unit identifiers at once.
    ///
    /// # Errors
    ///
    /// Returns an error if any individual `set_unit` call fails.
    pub fn set_units<I, K, V>(&mut self, units: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (unit_type, uid) in units {
            self.set_unit(&unit_type.into(), &uid.into())?;
        }
        Ok(())
    }

    /// Returns the UID for the given unit type, if set.
    pub fn get_unit(&self, unit_type: &str) -> Option<&String> {
        self.units.get(unit_type)
    }

    /// Returns all unit type to UID mappings.
    pub fn get_units(&self) -> &HashMap<String, String> {
        &self.units
    }

    /// Sets a context attribute.
    ///
    /// # Errors
    ///
    /// Returns an error if the context is finalized or finalizing.
    pub fn set_attribute(&mut self, name: &str, value: impl Into<Value>) -> Result<(), String> {
        self.check_not_finalized()?;

        self.attrs.push(Attribute {
            name: name.to_owned(),
            value: value.into(),
            set_at: now_millis(),
        });
        self.attrs_seq += 1;
        Ok(())
    }

    /// Sets multiple context attributes at once.
    ///
    /// # Errors
    ///
    /// Returns an error if the context is finalized or finalizing.
    pub fn set_attributes<I, K, V>(&mut self, attrs: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Value>,
    {
        self.check_not_finalized()?;

        let set_at = now_millis();
        for (name, value) in attrs {
            self.attrs.push(Attribute {
                name: name.into(),
                value: value.into(),
                set_at,
            });
            self.attrs_seq += 1;
        }
        Ok(())
    }

    /// Returns the most recently set value for the given attribute name.
    pub fn get_attribute(&self, name: &str) -> Option<&Value> {
        self.attrs
            .iter()
            .rev()
            .find(|a| a.name == name)
            .map(|a| &a.value)
    }

    /// Returns all attributes as a map (last-write-wins for duplicate names).
    pub fn get_attributes(&self) -> HashMap<String, Value> {
        let mut attrs = HashMap::new();
        for attr in &self.attrs {
            let _ = attrs.insert(attr.name.clone(), attr.value.clone());
        }
        attrs
    }

    /// Sets an override for the given experiment, forcing a specific variant.
    pub fn set_override(&mut self, experiment_name: &str, variant: i32) {
        let _ = self.overrides.insert(experiment_name.to_owned(), variant);
    }

    /// Sets multiple experiment overrides at once.
    pub fn set_overrides<I, K>(&mut self, overrides: I)
    where
        I: IntoIterator<Item = (K, i32)>,
        K: Into<String>,
    {
        for (experiment_name, variant) in overrides {
            let _ = self.overrides.insert(experiment_name.into(), variant);
        }
    }

    /// Sets a custom assignment for the given experiment.
    ///
    /// # Errors
    ///
    /// Returns an error if the context is finalized or finalizing.
    pub fn set_custom_assignment(
        &mut self,
        experiment_name: &str,
        variant: i32,
    ) -> Result<(), String> {
        self.check_not_finalized()?;
        let _ = self
            .cassignments
            .insert(experiment_name.to_owned(), variant);
        Ok(())
    }

    /// Sets multiple custom assignments at once.
    ///
    /// # Errors
    ///
    /// Returns an error if any individual assignment fails.
    pub fn set_custom_assignments<I, K>(&mut self, assignments: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, i32)>,
        K: Into<String>,
    {
        for (experiment_name, variant) in assignments {
            self.set_custom_assignment(&experiment_name.into(), variant)?;
        }
        Ok(())
    }

    /// Returns the assigned variant for an experiment without recording an exposure.
    pub fn peek(&mut self, experiment_name: &str) -> i32 {
        self.assign(experiment_name).variant
    }

    /// Returns the assigned variant for an experiment and records an exposure.
    pub fn treatment(&mut self, experiment_name: &str) -> i32 {
        let assignment = self.assign(experiment_name);
        let variant = assignment.variant;

        if !assignment.exposed {
            if let Some(assignment) = self.assignments.get_mut(experiment_name) {
                assignment.exposed = true;
            }
            self.queue_exposure(experiment_name);
        }

        variant
    }

    /// Records a goal achievement event.
    ///
    /// # Errors
    ///
    /// Returns an error if the context is finalized or finalizing.
    pub fn track(&mut self, goal_name: &str, properties: impl Into<Value>) -> Result<(), String> {
        self.check_not_finalized()?;

        let properties_map: Option<HashMap<String, Value>> = match properties.into() {
            Value::Object(map) => Some(map.into_iter().collect()),
            _ => None,
        };

        let goal = Goal {
            name: goal_name.to_owned(),
            properties: properties_map,
            achieved_at: now_millis(),
        };

        self.log_event(
            "goal",
            Some(serde_json::to_value(&goal).unwrap_or_default()),
        );
        self.goals.push(goal);
        self.pending += 1;

        Ok(())
    }

    /// Returns the variable value for the given key, falling back to the default.
    /// Records an exposure for the experiment that provides the variable.
    pub fn variable_value(&mut self, key: &str, default_value: impl Into<Value>) -> Value {
        if let Some(experiment_names) = self.index_variables.get(key).cloned() {
            for exp_name in experiment_names {
                let assignment = self.assign(&exp_name);
                if let Some(variables) = &assignment.variables {
                    if !assignment.exposed {
                        if let Some(a) = self.assignments.get_mut(&exp_name) {
                            a.exposed = true;
                        }
                        self.queue_exposure(&exp_name);
                    }

                    if let Some(value) = variables.get(key) {
                        if assignment.assigned || assignment.overridden {
                            return value.clone();
                        }
                    }
                }
            }
        }
        default_value.into()
    }

    /// Returns the variable value for the given key without recording an exposure.
    pub fn peek_variable_value(&mut self, key: &str, default_value: impl Into<Value>) -> Value {
        if let Some(experiment_names) = self.index_variables.get(key).cloned() {
            for exp_name in experiment_names {
                let assignment = self.assign(&exp_name);
                if let Some(variables) = &assignment.variables {
                    if let Some(value) = variables.get(key) {
                        if assignment.assigned || assignment.overridden {
                            return value.clone();
                        }
                    }
                }
            }
        }
        default_value.into()
    }

    /// Returns a map of variable keys to the experiment names that provide them.
    pub fn variable_keys(&self) -> HashMap<String, Vec<String>> {
        self.index_variables.clone()
    }

    /// Returns a custom field value for the given experiment and field name.
    pub fn custom_field_value(&self, experiment_name: &str, field_name: &str) -> Option<Value> {
        let exp = self.index.get(experiment_name)?;
        let custom_fields = exp.data.custom_field_values.as_ref()?;
        let field = custom_fields.iter().find(|f| f.name == field_name)?;

        match field.field_type.as_str() {
            "text" | "string" => Some(Value::String(field.value.clone())),
            "number" => field
                .value
                .parse::<f64>()
                .ok()
                .map(|n| serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)),
            "json" => {
                if field.value == "null" {
                    Some(Value::Null)
                } else if field.value.is_empty() {
                    Some(Value::String(String::new()))
                } else {
                    serde_json::from_str(&field.value).ok()
                }
            }
            "boolean" => Some(Value::Bool(field.value == "true")),
            _ => None,
        }
    }

    /// Returns the type of a custom field for the given experiment and field name.
    pub fn custom_field_value_type(
        &self,
        experiment_name: &str,
        field_name: &str,
    ) -> Option<String> {
        let exp = self.index.get(experiment_name)?;
        let custom_fields = exp.data.custom_field_values.as_ref()?;
        let field = custom_fields.iter().find(|f| f.name == field_name)?;
        Some(field.field_type.clone())
    }

    /// Returns all unique custom field names across all experiments.
    pub fn custom_field_keys(&self) -> Vec<String> {
        let mut keys = std::collections::HashSet::new();
        for exp in &self.data.experiments {
            if let Some(ref custom_fields) = exp.custom_field_values {
                for field in custom_fields {
                    let _ = keys.insert(field.name.clone());
                }
            }
        }
        keys.into_iter().collect()
    }

    /// Returns the names of all experiments in the context.
    pub fn experiments(&self) -> Vec<String> {
        self.data
            .experiments
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    /// Refreshes the context with new experiment data, clearing all cached assignments.
    pub fn refresh(&mut self, new_data: ContextData) {
        self.assignments.clear();
        self.init(new_data);
        self.log_event(
            "refresh",
            Some(serde_json::to_value(&self.data).unwrap_or_default()),
        );
    }

    /// Publishes all pending events (exposures, goals, attributes).
    pub fn publish(&mut self) {
        if self.pending == 0 {
            return;
        }

        let params = self.build_publish_params();
        self.log_event(
            "publish",
            Some(serde_json::to_value(&params).unwrap_or_default()),
        );

        self.pending = 0;
        self.exposures.clear();
        self.goals.clear();
    }

    /// Finalizes the context, publishing any pending events and preventing further mutations.
    pub fn finalize(&mut self) {
        if self.is_finalized() {
            return;
        }

        self.state = ContextState::Finalizing;

        if self.pending > 0 {
            self.publish();
        }

        self.state = ContextState::Finalized;
        self.log_event("finalize", None);
    }

    #[allow(clippy::too_many_lines)]
    fn assign(&mut self, experiment_name: &str) -> Assignment {
        let has_custom = self.cassignments.contains_key(experiment_name);
        let has_override = self.overrides.contains_key(experiment_name);
        let has_experiment = self.index.contains_key(experiment_name);

        if let Some(cached) = self.assignments.get(experiment_name) {
            if has_override {
                if cached.overridden && cached.variant == self.overrides[experiment_name] {
                    return cached.clone();
                }
            } else if !has_experiment {
                if !cached.assigned {
                    return cached.clone();
                }
            } else if !has_custom || self.cassignments[experiment_name] == cached.variant {
                if let Some(exp) = self.index.get(experiment_name) {
                    if Self::experiment_matches(&exp.data, cached)
                        && self.audience_matches(&exp.data, cached)
                    {
                        return cached.clone();
                    }
                }
            }
        }

        let exp_data_opt = self.index.get(experiment_name).map(|e| e.data.clone());

        let mut assignment = Assignment {
            eligible: true,
            ..Default::default()
        };

        if has_override {
            if let Some(ref exp_data) = exp_data_opt {
                assignment.id = exp_data.id;
                assignment.unit_type.clone_from(&exp_data.unit_type);
            }

            assignment.overridden = true;
            assignment.variant = self.overrides[experiment_name];
        } else if let Some(ref exp_data) = exp_data_opt {
            if !exp_data.audience.is_empty() {
                let attrs = self.get_attributes();
                let result = self.audience_matcher.evaluate(&exp_data.audience, &attrs);
                if let Some(matched) = result {
                    assignment.audience_mismatch = !matched;
                }
            }

            if exp_data.audience_strict && assignment.audience_mismatch {
                assignment.variant = 0;
            } else if exp_data.full_on_variant == 0 {
                if let Some(ref unit_type) = exp_data.unit_type {
                    if self.units.contains_key(unit_type) {
                        let unit_hash = self.unit_hash(unit_type);

                        if let Some(ref hash) = unit_hash {
                            let assigner = self
                                .assigners
                                .entry(unit_type.clone())
                                .or_insert_with(|| VariantAssigner::new(hash));

                            let eligible = assigner.assign(
                                &exp_data.traffic_split,
                                exp_data.traffic_seed_hi,
                                exp_data.traffic_seed_lo,
                            ) == 1;

                            assignment.assigned = true;
                            assignment.eligible = eligible;

                            if eligible {
                                if has_custom {
                                    assignment.variant = self.cassignments[experiment_name];
                                    assignment.custom = true;
                                } else {
                                    #[allow(
                                        clippy::cast_possible_truncation,
                                        clippy::cast_possible_wrap
                                    )]
                                    let v = assigner.assign(
                                        &exp_data.split,
                                        exp_data.seed_hi,
                                        exp_data.seed_lo,
                                    ) as i32;
                                    assignment.variant = v;
                                }
                            } else {
                                assignment.variant = 0;
                            }
                        }
                    }
                }
            } else {
                assignment.assigned = true;
                assignment.eligible = true;
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let v = exp_data.full_on_variant as i32;
                assignment.variant = v;
                assignment.full_on = true;
            }

            assignment.unit_type.clone_from(&exp_data.unit_type);
            assignment.id = exp_data.id;
            assignment.iteration = exp_data.iteration;
            assignment.traffic_split = Some(exp_data.traffic_split.clone());
            assignment.full_on_variant = exp_data.full_on_variant;
            assignment.attrs_seq = self.attrs_seq;
        }

        if let Some(exp) = self.index.get(experiment_name) {
            #[allow(clippy::cast_sign_loss)]
            let variant_idx = assignment.variant as usize;
            if variant_idx < exp.variables.len() {
                assignment.variables = Some(exp.variables[variant_idx].clone());
            }
        }

        let _ = self
            .assignments
            .insert(experiment_name.to_owned(), assignment.clone());
        assignment
    }

    fn experiment_matches(experiment: &ExperimentData, assignment: &Assignment) -> bool {
        experiment.id == assignment.id
            && experiment.unit_type == assignment.unit_type
            && experiment.iteration == assignment.iteration
            && experiment.full_on_variant == assignment.full_on_variant
            && assignment
                .traffic_split
                .as_ref()
                .is_some_and(|ts| array_equals_shallow(&experiment.traffic_split, ts))
    }

    fn audience_matches(&self, experiment: &ExperimentData, assignment: &Assignment) -> bool {
        if !experiment.audience.is_empty() && self.attrs_seq > assignment.attrs_seq {
            let attrs = self.get_attributes();
            let result = self.audience_matcher.evaluate(&experiment.audience, &attrs);
            if let Some(matched) = result {
                return matched != assignment.audience_mismatch;
            }
        }
        true
    }

    fn queue_exposure(&mut self, experiment_name: &str) {
        if let Some(assignment) = self.assignments.get(experiment_name) {
            let exposure = Exposure {
                id: assignment.id,
                name: experiment_name.to_owned(),
                exposed_at: now_millis(),
                unit: assignment.unit_type.clone(),
                variant: assignment.variant,
                assigned: assignment.assigned,
                eligible: assignment.eligible,
                overridden: assignment.overridden,
                full_on: assignment.full_on,
                custom: assignment.custom,
                audience_mismatch: assignment.audience_mismatch,
            };

            self.log_event(
                "exposure",
                Some(serde_json::to_value(&exposure).unwrap_or_default()),
            );
            self.exposures.push(exposure);
            self.pending += 1;
        }
    }

    fn unit_hash(&mut self, unit_type: &str) -> Option<String> {
        if let Some(hash) = self.hashes.get(unit_type) {
            return Some(hash.clone());
        }

        if let Some(unit) = self.units.get(unit_type) {
            let hash = hash_unit(unit);
            let _ = self.hashes.insert(unit_type.to_owned(), hash.clone());
            return Some(hash);
        }

        None
    }

    fn build_publish_params(&self) -> PublishParams {
        let units: Vec<Unit> = self
            .units
            .keys()
            .map(|unit_type| Unit {
                unit_type: unit_type.clone(),
                uid: self.hashes.get(unit_type).cloned(),
            })
            .collect();

        PublishParams {
            published_at: now_millis(),
            units,
            hashed: true,
            exposures: if self.exposures.is_empty() {
                None
            } else {
                Some(self.exposures.clone())
            },
            goals: if self.goals.is_empty() {
                None
            } else {
                Some(self.goals.clone())
            },
            attributes: if self.attrs.is_empty() {
                None
            } else {
                Some(self.attrs.clone())
            },
        }
    }

    fn log_event(&self, event_name: &str, data: Option<Value>) {
        if let Some(ref logger) = self.event_logger {
            logger(self, event_name, data);
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Variant;
    use serde_json::json;

    fn make_experiment(name: &str, variants: Vec<&str>, split: Vec<f64>) -> ExperimentData {
        ExperimentData {
            id: 1,
            name: name.to_string(),
            unit_type: Some("session_id".to_string()),
            iteration: 1,
            seed_hi: 0,
            seed_lo: 0,
            split,
            traffic_seed_hi: 0,
            traffic_seed_lo: 0,
            traffic_split: vec![0.0, 1.0],
            full_on_variant: 0,
            audience: String::new(),
            audience_strict: false,
            variants: variants
                .iter()
                .map(|c| Variant {
                    config: if c.is_empty() {
                        None
                    } else {
                        Some((*c).to_string())
                    },
                })
                .collect(),
            variables: HashMap::new(),
            custom_field_values: None,
        }
    }

    fn make_context_data(experiments: Vec<ExperimentData>) -> ContextData {
        ContextData { experiments }
    }

    #[test]
    fn test_context_is_ready_after_creation() {
        let data = make_context_data(vec![]);
        let context = Context::new(data);
        assert!(context.is_ready());
        assert!(!context.is_failed());
        assert!(!context.is_finalized());
    }

    #[test]
    fn test_context_set_unit() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_unit("session_id", "user123").is_ok());
        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
    }

    #[test]
    fn test_context_set_unit_cannot_change() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_unit("session_id", "user123").is_ok());
        assert!(context.set_unit("session_id", "user456").is_err());
    }

    #[test]
    fn test_context_set_unit_same_value_ok() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_unit("session_id", "user123").is_ok());
        assert!(context.set_unit("session_id", "user123").is_ok());
    }

    #[test]
    fn test_context_set_unit_blank_not_allowed() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_unit("session_id", "").is_err());
        assert!(context.set_unit("session_id", "   ").is_err());
    }

    #[test]
    fn test_context_set_attribute() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_attribute("country", json!("US")).is_ok());
        assert_eq!(context.get_attribute("country"), Some(&json!("US")));
    }

    #[test]
    fn test_context_peek_returns_zero_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert_eq!(context.peek("nonexistent_experiment"), 0);
    }

    #[test]
    fn test_context_treatment_returns_zero_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert_eq!(context.treatment("nonexistent_experiment"), 0);
    }

    #[test]
    fn test_context_treatment_with_experiment() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        let variant = context.treatment("test_exp");
        assert!(variant == 0 || variant == 1);
    }

    #[test]
    fn test_context_peek_does_not_queue_exposure() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.peek("test_exp");
        assert_eq!(context.pending(), 0);
    }

    #[test]
    fn test_context_treatment_queues_exposure() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.treatment("test_exp");
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_context_treatment_only_queues_once() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.treatment("test_exp");
        context.treatment("test_exp");
        context.treatment("test_exp");
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_context_set_override() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_override("test_exp", 1);
        context.set_unit("session_id", "test_user").unwrap();

        assert_eq!(context.treatment("test_exp"), 1);
    }

    #[test]
    fn test_context_set_custom_assignment() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_custom_assignment("test_exp", 1).unwrap();
        context.set_unit("session_id", "test_user").unwrap();

        let variant = context.treatment("test_exp");
        assert_eq!(variant, 1);
    }

    #[test]
    fn test_context_track() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.track("purchase", json!({"amount": 99.99})).is_ok());

        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_context_track_without_properties() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.track("click", ()).is_ok());
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_context_publish_clears_pending() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.treatment("test_exp");
        context.track("click", ()).unwrap();

        assert_eq!(context.pending(), 2);
        context.publish();
        assert_eq!(context.pending(), 0);
    }

    #[test]
    fn test_context_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize();
        assert!(context.is_finalized());
    }

    #[test]
    fn test_context_cannot_track_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize();
        assert!(context.track("click", ()).is_err());
    }

    #[test]
    fn test_context_cannot_set_unit_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize();
        assert!(context.set_unit("session_id", "user123").is_err());
    }

    #[test]
    fn test_context_cannot_set_attribute_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize();
        assert!(context.set_attribute("country", json!("US")).is_err());
    }

    #[test]
    fn test_context_variable_value_returns_default_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.variable_value("nonexistent", json!("default"));
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_context_peek_variable_value_returns_default_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.peek_variable_value("nonexistent", json!(42));
        assert_eq!(value, json!(42));
    }

    #[test]
    fn test_context_experiments_list() {
        let exp1 = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let exp2 = make_experiment("exp2", vec!["{}"], vec![1.0]);
        let data = make_context_data(vec![exp1, exp2]);
        let context = Context::new(data);

        let experiments = context.experiments();
        assert!(experiments.contains(&"exp1".to_string()));
        assert!(experiments.contains(&"exp2".to_string()));
    }

    #[test]
    fn test_context_full_on_variant() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.full_on_variant = 1;
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "any_user").unwrap();
        assert_eq!(context.treatment("test_exp"), 1);
    }

    #[test]
    fn test_context_audience_mismatch_strict() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.set_attribute("country", json!("UK")).unwrap();

        assert_eq!(context.treatment("test_exp"), 0);
    }

    #[test]
    fn test_context_audience_match() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.set_attribute("country", json!("US")).unwrap();

        assert_eq!(context.treatment("test_exp"), 1);
    }

    #[test]
    fn test_context_refresh() {
        let exp1 = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let data1 = make_context_data(vec![exp1]);
        let mut context = Context::new(data1);

        let exp2 = make_experiment("exp2", vec!["{}"], vec![1.0]);
        let data2 = make_context_data(vec![exp2]);
        context.refresh(data2);

        let experiments = context.experiments();
        assert!(experiments.contains(&"exp2".to_string()));
        assert!(!experiments.contains(&"exp1".to_string()));
    }

    #[test]
    fn test_ergonomic_set_attribute_with_string() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_attribute("country", "US").is_ok());
        assert_eq!(context.get_attribute("country"), Some(&json!("US")));
    }

    #[test]
    fn test_ergonomic_set_attribute_with_number() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_attribute("age", 25).is_ok());
        assert_eq!(context.get_attribute("age"), Some(&json!(25)));
    }

    #[test]
    fn test_ergonomic_set_attribute_with_bool() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_attribute("premium", true).is_ok());
        assert_eq!(context.get_attribute("premium"), Some(&json!(true)));
    }

    #[test]
    fn test_set_attributes_with_array() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context
            .set_attributes([
                ("country", json!("US")),
                ("age", json!(25)),
                ("premium", json!(true)),
            ])
            .is_ok());
        assert_eq!(context.get_attribute("country"), Some(&json!("US")));
        assert_eq!(context.get_attribute("age"), Some(&json!(25)));
        assert_eq!(context.get_attribute("premium"), Some(&json!(true)));
    }

    #[test]
    fn test_set_attributes_with_hashmap() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let attrs = HashMap::from([
            ("country".to_string(), json!("UK")),
            ("tier".to_string(), json!("gold")),
        ]);
        assert!(context.set_attributes(attrs).is_ok());
        assert_eq!(context.get_attribute("country"), Some(&json!("UK")));
        assert_eq!(context.get_attribute("tier"), Some(&json!("gold")));
    }

    #[test]
    fn test_ergonomic_variable_value_with_string_default() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.variable_value("nonexistent", "default_value");
        assert_eq!(value, json!("default_value"));
    }

    #[test]
    fn test_ergonomic_variable_value_with_number_default() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.variable_value("nonexistent", 42);
        assert_eq!(value, json!(42));
    }

    #[test]
    fn test_ergonomic_peek_variable_value_with_bool_default() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.peek_variable_value("nonexistent", false);
        assert_eq!(value, json!(false));
    }

    #[test]
    fn test_ergonomic_track_with_json_properties() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context
            .track(
                "purchase",
                json!({
                    "item_count": 1,
                    "total_amount": 99.99
                })
            )
            .is_ok());
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_ergonomic_track_with_unit_no_properties() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.track("click", ()).is_ok());
        assert_eq!(context.pending(), 1);
    }
}
