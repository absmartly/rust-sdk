use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assigner::VariantAssigner;
use crate::matcher::AudienceMatcher;
use crate::models::*;
use crate::utils::{array_equals_shallow, hash_unit};

pub type EventLogger = Box<dyn Fn(&Context, &str, Option<Value>) + Send + Sync>;

struct Experiment {
    data: ExperimentData,
    variables: Vec<HashMap<String, Value>>,
}

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

impl Context {
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

    pub fn new_loading() -> Self {
        Self {
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
        }
    }

    pub fn become_ready(&mut self, data: ContextData) {
        if self.state == ContextState::Loading {
            self.init(data);
            self.state = ContextState::Ready;
        }
    }

    pub fn become_failed(&mut self) {
        if self.state == ContextState::Loading {
            self.state = ContextState::Failed;
        }
    }

    pub fn set_event_logger(&mut self, logger: EventLogger) {
        self.event_logger = Some(logger);
    }

    fn init(&mut self, data: ContextData) {
        self.data = data;
        self.index.clear();
        self.index_variables.clear();

        for experiment in &self.data.experiments {
            let mut variables: Vec<HashMap<String, Value>> = Vec::new();

            for (variant_idx, variant) in experiment.variants.iter().enumerate() {
                let parsed: HashMap<String, Value> = variant
                    .config
                    .as_ref()
                    .and_then(|c| {
                        if c.is_empty() {
                            None
                        } else {
                            match serde_json::from_str(c) {
                                Ok(v) => Some(v),
                                Err(e) => {
                                    eprintln!(
                                        "ERROR: Failed to parse variant config for experiment '{}', variant {}: {}. Config: '{}'",
                                        experiment.name, variant_idx, e, c
                                    );
                                    None
                                }
                            }
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

            self.index.insert(
                experiment.name.clone(),
                Experiment {
                    data: experiment.clone(),
                    variables,
                },
            );
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == ContextState::Ready
    }

    pub fn is_failed(&self) -> bool {
        self.state == ContextState::Failed
    }

    pub fn is_finalized(&self) -> bool {
        self.state == ContextState::Finalized
    }

    pub fn is_finalizing(&self) -> bool {
        self.state == ContextState::Finalizing
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn data(&self) -> &ContextData {
        &self.data
    }

    pub fn set_unit(&mut self, unit_type: &str, uid: &str) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }

        let uid = uid.trim();
        if uid.is_empty() {
            return Err(format!("Unit '{}' UID must not be blank.", unit_type));
        }

        if let Some(existing) = self.units.get(unit_type) {
            if existing != uid {
                return Err(format!("Unit '{}' UID already set.", unit_type));
            }
        }

        self.units.insert(unit_type.to_string(), uid.to_string());
        Ok(())
    }

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

    pub fn get_unit(&self, unit_type: &str) -> Option<&String> {
        self.units.get(unit_type)
    }

    pub fn get_units(&self) -> &HashMap<String, String> {
        &self.units
    }

    pub fn set_attribute(&mut self, name: &str, value: impl Into<Value>) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }

        self.attrs.push(Attribute {
            name: name.to_string(),
            value: value.into(),
            set_at: now_millis(),
        });
        self.attrs_seq += 1;
        Ok(())
    }

    pub fn set_attributes<I, K, V>(&mut self, attrs: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Value>,
    {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }

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

    pub fn get_attribute(&self, name: &str) -> Option<&Value> {
        self.attrs
            .iter()
            .rev()
            .find(|a| a.name == name)
            .map(|a| &a.value)
    }

    pub fn get_attributes(&self) -> HashMap<String, Value> {
        let mut attrs = HashMap::new();
        for attr in &self.attrs {
            attrs.insert(attr.name.clone(), attr.value.clone());
        }
        attrs
    }

    pub fn set_override(&mut self, experiment_name: &str, variant: i32) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }
        self.overrides.insert(experiment_name.to_string(), variant);
        Ok(())
    }

    pub fn set_overrides<I, K>(&mut self, overrides: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, i32)>,
        K: Into<String>,
    {
        for (experiment_name, variant) in overrides {
            self.set_override(&experiment_name.into(), variant)?;
        }
        Ok(())
    }

    pub fn set_custom_assignment(&mut self, experiment_name: &str, variant: i32) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }
        self.cassignments
            .insert(experiment_name.to_string(), variant);
        Ok(())
    }

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

    pub fn peek(&mut self, experiment_name: &str) -> i32 {
        self.assign(experiment_name).variant
    }

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

    pub fn track(&mut self, goal_name: &str, properties: impl Into<Value>) -> Result<(), String> {
        if self.is_finalized() {
            return Err("ABsmartly Context is finalized.".to_string());
        }
        if self.is_finalizing() {
            return Err("ABsmartly Context is finalizing.".to_string());
        }

        let properties_map: Option<HashMap<String, Value>> = match properties.into() {
            Value::Object(map) => Some(map.into_iter().collect()),
            Value::Null => None,
            _ => None,
        };

        let goal = Goal {
            name: goal_name.to_string(),
            properties: properties_map,
            achieved_at: now_millis(),
        };

        match serde_json::to_value(&goal) {
            Ok(value) => self.log_event("goal", Some(value)),
            Err(e) => {
                eprintln!("ERROR: Failed to serialize goal '{}': {}", goal_name, e);
            }
        }
        self.goals.push(goal);
        self.pending += 1;

        Ok(())
    }

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

    pub fn variable_keys(&self) -> HashMap<String, Vec<String>> {
        let mut result = HashMap::new();
        for (key, exp_names) in &self.index_variables {
            result.insert(key.clone(), exp_names.clone());
        }
        result
    }

    pub fn custom_field_value(&self, experiment_name: &str, field_name: &str) -> Option<Value> {
        if let Some(exp) = self.index.get(experiment_name) {
            if let Some(ref custom_fields) = exp.data.custom_field_values {
                if let Some(field) = custom_fields.iter().find(|f| f.name == field_name) {
                    return match field.field_type.as_str() {
                        "text" | "string" => Some(Value::String(field.value.clone())),
                        "number" => match field.value.parse::<f64>() {
                            Ok(n) => serde_json::Number::from_f64(n)
                                .map(Value::Number)
                                .or_else(|| {
                                    eprintln!("WARNING: Custom field '{}' number out of range: {}", field_name, n);
                                    Some(Value::Null)
                                }),
                            Err(e) => {
                                eprintln!("ERROR: Failed to parse custom field '{}' as number: {}. Value: '{}'", field_name, e, field.value);
                                None
                            }
                        },
                        "json" => {
                            if field.value == "null" {
                                Some(Value::Null)
                            } else if field.value.is_empty() {
                                Some(Value::String(String::new()))
                            } else {
                                match serde_json::from_str(&field.value) {
                                    Ok(v) => Some(v),
                                    Err(e) => {
                                        eprintln!("ERROR: Failed to parse custom field '{}' JSON: {}. Value: '{}'", field_name, e, field.value);
                                        None
                                    }
                                }
                            }
                        }
                        "boolean" => Some(Value::Bool(field.value == "true")),
                        _ => {
                            eprintln!("WARNING: Unknown custom field type '{}' for field '{}'", field.field_type, field_name);
                            None
                        }
                    };
                }
            }
        }
        None
    }

    pub fn custom_field_value_type(&self, experiment_name: &str, field_name: &str) -> Option<String> {
        if let Some(exp) = self.index.get(experiment_name) {
            if let Some(ref custom_fields) = exp.data.custom_field_values {
                if let Some(field) = custom_fields.iter().find(|f| f.name == field_name) {
                    return Some(field.field_type.clone());
                }
            }
        }
        None
    }

    pub fn custom_field_keys(&self) -> Vec<String> {
        let mut keys = std::collections::HashSet::new();
        for exp in &self.data.experiments {
            if let Some(ref custom_fields) = exp.custom_field_values {
                for field in custom_fields {
                    keys.insert(field.name.clone());
                }
            }
        }
        keys.into_iter().collect()
    }

    pub fn experiments(&self) -> Vec<String> {
        self.data.experiments.iter().map(|e| e.name.clone()).collect()
    }

    pub fn refresh(&mut self, new_data: ContextData) {
        self.assignments.clear();
        self.init(new_data);
        match serde_json::to_value(&self.data) {
            Ok(value) => self.log_event("refresh", Some(value)),
            Err(e) => {
                eprintln!("ERROR: Failed to serialize context data for refresh: {}", e);
            }
        }
    }

    pub fn publish(&mut self) {
        if self.pending == 0 {
            return;
        }

        let params = self.build_publish_params();

        match serde_json::to_value(&params) {
            Ok(value) => self.log_event("publish", Some(value)),
            Err(e) => {
                eprintln!("ERROR: Failed to serialize publish params: {}", e);
                return;
            }
        }

        self.pending = 0;
        self.exposures.clear();
        self.goals.clear();
    }

    pub fn get_publish_params(&self) -> PublishParams {
        self.build_publish_params()
    }

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
                    if self.experiment_matches(&exp.data, cached)
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
                assignment.unit_type = exp_data.unit_type.clone();
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
                                    assignment.variant = assigner.assign(
                                        &exp_data.split,
                                        exp_data.seed_hi,
                                        exp_data.seed_lo,
                                    ) as i32;
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
                assignment.variant = exp_data.full_on_variant as i32;
                assignment.full_on = true;
            }

            assignment.unit_type = exp_data.unit_type.clone();
            assignment.id = exp_data.id;
            assignment.iteration = exp_data.iteration;
            assignment.traffic_split = Some(exp_data.traffic_split.clone());
            assignment.full_on_variant = exp_data.full_on_variant;
            assignment.attrs_seq = self.attrs_seq;
        }

        if let Some(exp) = self.index.get(experiment_name) {
            if (assignment.variant as usize) < exp.variables.len() {
                assignment.variables = Some(exp.variables[assignment.variant as usize].clone());
            }
        }

        self.assignments
            .insert(experiment_name.to_string(), assignment.clone());
        assignment
    }

    fn experiment_matches(&self, experiment: &ExperimentData, assignment: &Assignment) -> bool {
        experiment.id == assignment.id
            && experiment.unit_type == assignment.unit_type
            && experiment.iteration == assignment.iteration
            && experiment.full_on_variant == assignment.full_on_variant
            && assignment
                .traffic_split
                .as_ref()
                .map_or(false, |ts| array_equals_shallow(&experiment.traffic_split, ts))
    }

    fn audience_matches(&self, experiment: &ExperimentData, assignment: &Assignment) -> bool {
        if !experiment.audience.is_empty() && self.attrs_seq > assignment.attrs_seq {
            let attrs = self.get_attributes();
            let result = self.audience_matcher.evaluate(&experiment.audience, &attrs);
            if let Some(matched) = result {
                return matched == !assignment.audience_mismatch;
            }
        }
        true
    }

    fn queue_exposure(&mut self, experiment_name: &str) {
        if let Some(assignment) = self.assignments.get(experiment_name) {
            let exposure = Exposure {
                id: assignment.id,
                name: experiment_name.to_string(),
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

            match serde_json::to_value(&exposure) {
                Ok(value) => self.log_event("exposure", Some(value)),
                Err(e) => {
                    eprintln!("ERROR: Failed to serialize exposure for experiment '{}': {}", experiment_name, e);
                }
            }
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
            self.hashes.insert(unit_type.to_string(), hash.clone());
            return Some(hash);
        }

        None
    }

    fn build_publish_params(&self) -> PublishParams {
        let units: Vec<Unit> = self
            .units
            .iter()
            .map(|(unit_type, _)| Unit {
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

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|e| {
            eprintln!("WARNING: System time error, returning 0: {}", e);
            0
        })
}

#[cfg(test)]
mod tests {
    use super::*;
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
                    config: if c.is_empty() { None } else { Some(c.to_string()) },
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
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        let variant = context.treatment("test_exp");
        assert!(variant == 0 || variant == 1);
    }

    #[test]
    fn test_context_peek_does_not_queue_exposure() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.peek("test_exp");
        assert_eq!(context.pending(), 0);
    }

    #[test]
    fn test_context_treatment_queues_exposure() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "test_user").unwrap();
        context.treatment("test_exp");
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_context_treatment_only_queues_once() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
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
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        let _ = context.set_override("test_exp", 1);
        context.set_unit("session_id", "test_user").unwrap();

        assert_eq!(context.treatment("test_exp"), 1);
    }

    #[test]
    fn test_context_set_custom_assignment() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
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
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
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
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.full_on_variant = 1;
        let data = make_context_data(vec![exp]);
        let mut context = Context::new(data);

        context.set_unit("session_id", "any_user").unwrap();
        assert_eq!(context.treatment("test_exp"), 1);
    }

    #[test]
    fn test_context_audience_mismatch_strict() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
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
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
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

        let attrs = std::collections::HashMap::from([
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

        assert!(context.track("purchase", json!({
            "item_count": 1,
            "total_amount": 99.99
        })).is_ok());
        assert_eq!(context.pending(), 1);
    }

    #[test]
    fn test_ergonomic_track_with_unit_no_properties() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.track("click", ()).is_ok());
        assert_eq!(context.pending(), 1);
    }

    fn make_experiment_with_id(name: &str, id: i64, variants: Vec<&str>, split: Vec<f64>) -> ExperimentData {
        ExperimentData {
            id,
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
                    config: if c.is_empty() { None } else { Some(c.to_string()) },
                })
                .collect(),
            variables: HashMap::new(),
            custom_field_values: None,
        }
    }

    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct LogEntry {
        event: String,
        data: Option<Value>,
    }

    fn make_logging_context(data: ContextData) -> (Context, Arc<Mutex<Vec<LogEntry>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let mut ctx = Context::new(data);
        ctx.set_event_logger(Box::new(move |_ctx, event, data| {
            log_clone.lock().unwrap().push(LogEntry {
                event: event.to_string(),
                data,
            });
        }));
        (ctx, log)
    }

    #[test]
    fn test_event_logger_on_exposure() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "exposure"));
    }

    #[test]
    fn test_event_logger_on_goal() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.track("purchase", json!({"amount": 9.99})).unwrap();

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "goal"));
    }

    #[test]
    fn test_event_logger_on_publish() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");
        ctx.publish();

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "publish"));
    }

    #[test]
    fn test_event_logger_on_finalize() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.finalize();

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "finalize"));
    }

    #[test]
    fn test_event_logger_on_refresh() {
        let exp = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);

        let exp2 = make_experiment("exp2", vec!["{}"], vec![1.0]);
        let data2 = make_context_data(vec![exp2]);
        ctx.refresh(data2);

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "refresh"));
    }

    #[test]
    fn test_loading_state_become_ready() {
        let mut ctx = Context::new_loading();
        assert!(!ctx.is_ready());
        assert!(!ctx.is_failed());

        let data = make_context_data(vec![]);
        ctx.become_ready(data);
        assert!(ctx.is_ready());
    }

    #[test]
    fn test_loading_state_become_failed() {
        let mut ctx = Context::new_loading();
        assert!(!ctx.is_ready());

        ctx.become_failed();
        assert!(ctx.is_failed());
        assert!(!ctx.is_ready());
    }

    #[test]
    fn test_become_ready_only_once() {
        let mut ctx = Context::new_loading();
        let data1 = make_context_data(vec![]);
        ctx.become_ready(data1);
        assert!(ctx.is_ready());

        let exp = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let data2 = make_context_data(vec![exp]);
        ctx.become_ready(data2);

        assert!(ctx.experiments().is_empty());
    }

    #[test]
    fn test_become_failed_only_from_loading() {
        let mut ctx = Context::new_loading();
        let data = make_context_data(vec![]);
        ctx.become_ready(data);
        assert!(ctx.is_ready());

        ctx.become_failed();
        assert!(!ctx.is_failed());
        assert!(ctx.is_ready());
    }

    #[test]
    fn test_set_unit_callable_before_ready() {
        let mut ctx = Context::new_loading();
        assert!(ctx.set_unit("session_id", "user123").is_ok());
        assert_eq!(ctx.get_unit("session_id"), Some(&"user123".to_string()));
    }

    #[test]
    fn test_set_attribute_callable_before_ready() {
        let mut ctx = Context::new_loading();
        assert!(ctx.set_attribute("country", json!("US")).is_ok());
        assert_eq!(ctx.get_attribute("country"), Some(&json!("US")));
    }

    #[test]
    fn test_set_override_callable_before_ready() {
        let mut ctx = Context::new_loading();
        let _ = ctx.set_override("exp1", 1);

        let exp = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        ctx.become_ready(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("exp1"), 1);
    }

    #[test]
    fn test_set_custom_assignment_callable_before_ready() {
        let mut ctx = Context::new_loading();
        ctx.set_custom_assignment("exp1", 1).unwrap();

        let exp = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        ctx.become_ready(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("exp1"), 1);
    }

    #[test]
    fn test_set_custom_assignment_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize();

        assert!(ctx.set_custom_assignment("exp1", 1).is_err());
    }

    #[test]
    fn test_treatment_queues_exposure_after_peek() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek("test_exp");
        assert_eq!(ctx.pending(), 0);

        ctx.treatment("test_exp");
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_treatment_queues_exposure_with_override_variant() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        let _ = ctx.set_override("test_exp", 1);

        ctx.treatment("test_exp");
        assert_eq!(ctx.pending(), 1);

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["overridden"], json!(true));
        assert_eq!(exposure_data["variant"], json!(1));
    }

    #[test]
    fn test_treatment_queues_exposure_with_custom_assignment_variant() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_custom_assignment("test_exp", 1).unwrap();

        ctx.treatment("test_exp");
        assert_eq!(ctx.pending(), 1);

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["custom"], json!(true));
        assert_eq!(exposure_data["variant"], json!(1));
    }

    #[test]
    fn test_treatment_base_variant_on_unknown_experiment() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("unknown_exp"), 0);
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_treatment_not_requeue_on_unknown_experiment() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("unknown_exp");
        ctx.treatment("unknown_exp");
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_peek_returns_override_variant() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        let _ = ctx.set_override("test_exp", 1);

        assert_eq!(ctx.peek("test_exp"), 1);
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_peek_audience_mismatch_non_strict() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let variant = ctx.peek("test_exp");
        assert_eq!(variant, 1);
    }

    #[test]
    fn test_peek_audience_mismatch_strict() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        assert_eq!(ctx.peek("test_exp"), 0);
    }

    #[test]
    fn test_treatment_audience_match_queues_with_audience_mismatch_false() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("US")).unwrap();

        ctx.treatment("test_exp");

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(false));
    }

    #[test]
    fn test_treatment_audience_mismatch_queues_with_audience_mismatch_true() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        ctx.treatment("test_exp");

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(true));
    }

    #[test]
    fn test_treatment_audience_mismatch_strict_queues_control() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let variant = ctx.treatment("test_exp");
        assert_eq!(variant, 0);

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(true));
        assert_eq!(exposure_data["variant"], json!(0));
    }

    #[test]
    fn test_variable_value_returns_default_when_unassigned() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![1.0, 0.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let value = ctx.variable_value("button", json!("default"));
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_value_returns_override_values() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        let _ = ctx.set_override("test_exp", 1);

        let value = ctx.variable_value("button", json!("default"));
        assert_eq!(value, json!("red"));
    }

    #[test]
    fn test_variable_value_queues_exposure() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.variable_value("button", json!("default"));
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_variable_value_queues_exposure_after_peek() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek_variable_value("button", json!("default"));
        assert_eq!(ctx.pending(), 0);

        ctx.variable_value("button", json!("default"));
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_variable_value_queues_exposure_only_once() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.variable_value("button", json!("default"));
        ctx.variable_value("button", json!("default"));
        ctx.variable_value("button", json!("default"));
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_peek_variable_value_does_not_queue_exposure() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek_variable_value("button", json!("default"));
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_peek_variable_value_returns_override_values() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        let _ = ctx.set_override("test_exp", 1);

        let value = ctx.peek_variable_value("button", json!("default"));
        assert_eq!(value, json!("red"));
    }

    #[test]
    fn test_peek_variable_value_returns_default_when_unassigned() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![1.0, 0.0]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let value = ctx.peek_variable_value("button", json!("default"));
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_keys_returns_all_active_keys() {
        let exp1 = make_experiment("exp1", vec!["{}", r#"{"button":"red","header":"large"}"#], vec![0.5, 0.5]);
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let ctx = Context::new(data);

        let keys = ctx.variable_keys();
        assert!(keys.contains_key("button"));
        assert!(keys.contains_key("header"));
        assert!(keys.contains_key("color"));
    }

    #[test]
    fn test_track_with_null_properties() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        assert!(ctx.track("click", Value::Null).is_ok());
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_track_with_number_properties() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        assert!(ctx.track("purchase", json!({"amount": 99.99, "count": 1})).is_ok());
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_track_callable_before_ready() {
        let mut ctx = Context::new_loading();
        assert!(ctx.track("click", ()).is_ok());
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_track_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize();
        assert!(ctx.track("click", ()).is_err());
    }

    #[test]
    fn test_publish_does_not_call_when_queue_empty() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.publish();

        let entries = log.lock().unwrap();
        assert!(!entries.iter().any(|e| e.event == "publish"));
    }

    #[test]
    fn test_publish_clears_queue_on_success() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");
        ctx.track("click", ()).unwrap();
        assert!(ctx.pending() > 0);

        ctx.publish();
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_finalize_does_not_call_publish_when_queue_empty() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.finalize();

        let entries = log.lock().unwrap();
        assert!(!entries.iter().any(|e| e.event == "publish"));
        assert!(entries.iter().any(|e| e.event == "finalize"));
    }

    #[test]
    fn test_finalize_calls_publish_when_pending() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");
        assert!(ctx.pending() > 0);

        ctx.finalize();
        assert!(ctx.is_finalized());
        assert_eq!(ctx.pending(), 0);

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "publish"));
        assert!(entries.iter().any(|e| e.event == "finalize"));
    }

    #[test]
    fn test_finalize_is_idempotent() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.finalize();
        assert!(ctx.is_finalized());
        ctx.finalize();
        assert!(ctx.is_finalized());
    }

    #[test]
    fn test_refresh_keeps_overrides() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        let _ = ctx.set_override("test_exp", 1);

        assert_eq!(ctx.treatment("test_exp"), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.refresh(data2);
        assert_eq!(ctx.treatment("test_exp"), 1);
    }

    #[test]
    fn test_refresh_keeps_custom_assignments() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_custom_assignment("test_exp", 1).unwrap();

        assert_eq!(ctx.treatment("test_exp"), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.refresh(data2);
        assert_eq!(ctx.treatment("test_exp"), 1);
    }

    #[test]
    fn test_refresh_picks_up_fullon_change() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");

        let mut exp2 = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp2.full_on_variant = 1;
        let data2 = make_context_data(vec![exp2]);
        ctx.refresh(data2);

        assert_eq!(ctx.treatment("test_exp"), 1);
    }

    #[test]
    fn test_refresh_picks_up_traffic_split_change() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");

        let mut exp2 = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp2.traffic_split = vec![0.0, 1.0];
        let data2 = make_context_data(vec![exp2]);
        ctx.refresh(data2);

        let _variant = ctx.treatment("test_exp");
        assert!(ctx.pending() > 0);
    }

    #[test]
    fn test_refresh_picks_up_iteration_change() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");
        let pending_before = ctx.pending();

        let mut exp2 = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp2.iteration = 2;
        let data2 = make_context_data(vec![exp2]);
        ctx.refresh(data2);

        ctx.treatment("test_exp");
        assert!(ctx.pending() > pending_before);
    }

    #[test]
    fn test_refresh_picks_up_id_change() {
        let exp = make_experiment_with_id("test_exp", 1, vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp");
        let pending_before = ctx.pending();

        let exp2 = make_experiment_with_id("test_exp", 2, vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data2 = make_context_data(vec![exp2]);
        ctx.refresh(data2);

        ctx.treatment("test_exp");
        assert!(ctx.pending() > pending_before);
    }

    #[test]
    fn test_refresh_no_change_same_variant() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let variant1 = ctx.treatment("test_exp");
        assert_eq!(ctx.pending(), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.refresh(data2);

        let variant2 = ctx.treatment("test_exp");
        assert_eq!(variant1, variant2);
    }

    #[test]
    fn test_custom_field_keys() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![
            CustomFieldValue {
                name: "country".to_string(),
                value: "US".to_string(),
                field_type: "string".to_string(),
            },
            CustomFieldValue {
                name: "overrides".to_string(),
                value: r#"{"key":"value"}"#.to_string(),
                field_type: "json".to_string(),
            },
        ]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        let keys = ctx.custom_field_keys();
        assert!(keys.contains(&"country".to_string()));
        assert!(keys.contains(&"overrides".to_string()));
    }

    #[test]
    fn test_custom_field_value_string() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "country".to_string(),
            value: "US".to_string(),
            field_type: "string".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "country"), Some(json!("US")));
    }

    #[test]
    fn test_custom_field_value_text() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "description".to_string(),
            value: "A test experiment".to_string(),
            field_type: "text".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "description"), Some(json!("A test experiment")));
    }

    #[test]
    fn test_custom_field_value_json() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "overrides".to_string(),
            value: r#"{"key":"value"}"#.to_string(),
            field_type: "json".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "overrides"), Some(json!({"key": "value"})));
    }

    #[test]
    fn test_custom_field_value_number() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "priority".to_string(),
            value: "5".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "priority"), Some(json!(5.0)));
    }

    #[test]
    fn test_custom_field_value_decimal() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "weight".to_string(),
            value: "1.5".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "weight"), Some(json!(1.5)));
    }

    #[test]
    fn test_custom_field_value_boolean() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "enabled".to_string(),
            value: "true".to_string(),
            field_type: "boolean".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "enabled"), Some(json!(true)));
    }

    #[test]
    fn test_custom_field_value_boolean_false() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "enabled".to_string(),
            value: "false".to_string(),
            field_type: "boolean".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "enabled"), Some(json!(false)));
    }

    #[test]
    fn test_custom_field_value_null_for_nonexistent_field() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "country".to_string(),
            value: "US".to_string(),
            field_type: "string".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "nonexistent"), None);
    }

    #[test]
    fn test_custom_field_value_null_for_nonexistent_experiment() {
        let data = make_context_data(vec![]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("nonexistent_exp", "field"), None);
    }

    #[test]
    fn test_custom_field_value_null_for_experiment_without_custom_fields() {
        let exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "field"), None);
    }

    #[test]
    fn test_custom_field_value_type() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        exp.custom_field_values = Some(vec![
            CustomFieldValue {
                name: "country".to_string(),
                value: "US".to_string(),
                field_type: "string".to_string(),
            },
            CustomFieldValue {
                name: "priority".to_string(),
                value: "5".to_string(),
                field_type: "number".to_string(),
            },
            CustomFieldValue {
                name: "enabled".to_string(),
                value: "true".to_string(),
                field_type: "boolean".to_string(),
            },
        ]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value_type("test_exp", "country"), Some("string".to_string()));
        assert_eq!(ctx.custom_field_value_type("test_exp", "priority"), Some("number".to_string()));
        assert_eq!(ctx.custom_field_value_type("test_exp", "enabled"), Some("boolean".to_string()));
        assert_eq!(ctx.custom_field_value_type("test_exp", "nonexistent"), None);
    }

    #[test]
    fn test_get_attribute_returns_last_set_value() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_attribute("country", json!("US")).unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();
        assert_eq!(ctx.get_attribute("country"), Some(&json!("UK")));
    }

    #[test]
    fn test_get_units_returns_all() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_unit("session_id", "user123").unwrap();
        ctx.set_unit("device_id", "device456").unwrap();

        let units = ctx.get_units();
        assert_eq!(units.get("session_id"), Some(&"user123".to_string()));
        assert_eq!(units.get("device_id"), Some(&"device456".to_string()));
    }

    #[test]
    fn test_set_units_multiple() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_units([
            ("session_id", "user123"),
            ("device_id", "device456"),
        ]).unwrap();

        assert_eq!(ctx.get_unit("session_id"), Some(&"user123".to_string()));
        assert_eq!(ctx.get_unit("device_id"), Some(&"device456".to_string()));
    }

    #[test]
    fn test_set_overrides_multiple() {
        let exp1 = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let _ = ctx.set_overrides([("exp1", 1), ("exp2", 0)]);

        assert_eq!(ctx.treatment("exp1"), 1);
        assert_eq!(ctx.treatment("exp2"), 0);
    }

    #[test]
    fn test_set_custom_assignments_multiple() {
        let exp1 = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.set_custom_assignments([("exp1", 1), ("exp2", 0)]).unwrap();

        assert_eq!(ctx.treatment("exp1"), 1);
        assert_eq!(ctx.treatment("exp2"), 0);
    }

    #[test]
    fn test_set_attributes_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize();

        assert!(ctx.set_attributes([("country", json!("US"))]).is_err());
    }

    #[test]
    fn test_set_custom_assignments_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize();

        assert!(ctx.set_custom_assignments([("exp1", 1)]).is_err());
    }

    #[test]
    fn test_get_attributes_returns_all() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_attribute("country", json!("US")).unwrap();
        ctx.set_attribute("age", json!(25)).unwrap();

        let attrs = ctx.get_attributes();
        assert_eq!(attrs.get("country"), Some(&json!("US")));
        assert_eq!(attrs.get("age"), Some(&json!(25)));
    }

    #[test]
    fn test_data_returns_context_data() {
        let exp = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        let ctx_data = ctx.data();
        assert_eq!(ctx_data.experiments.len(), 1);
        assert_eq!(ctx_data.experiments[0].name, "exp1");
    }

    #[test]
    fn test_context_not_finalizing_initially() {
        let data = make_context_data(vec![]);
        let ctx = Context::new(data);
        assert!(!ctx.is_finalizing());
    }

    #[test]
    fn test_custom_field_value_json_null() {
        let mut exp = make_experiment("test_exp", vec!["{}"], vec![1.0]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "nullfield".to_string(),
            value: "null".to_string(),
            field_type: "json".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "nullfield"), Some(Value::Null));
    }

    #[test]
    fn test_variable_value_audience_mismatch_strict_returns_default() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let value = ctx.variable_value("button", json!("default"));
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_value_audience_match_returns_value() {
        let mut exp = make_experiment("test_exp", vec!["{}", r#"{"button":"red"}"#], vec![0.0, 1.0]);
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("US")).unwrap();

        let value = ctx.variable_value("button", json!("default"));
        assert_eq!(value, json!("red"));
    }
}
