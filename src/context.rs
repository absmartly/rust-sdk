use log::{error, warn};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assigner::VariantAssigner;
use crate::context_publisher::ContextPublisher;
use crate::matcher::AudienceMatcher;
use crate::models::*;
use crate::utils::{array_equals_shallow, hash_unit_value};

pub type EventLogger = Box<dyn Fn(&Context, &str, Option<Value>) + Send + Sync>;

struct Experiment {
    data: ExperimentData,
    variables: Vec<HashMap<String, Value>>,
}

pub type DataFetcher = Box<dyn Fn() -> Result<ContextData, String> + Send + Sync>;

pub struct Context {
    units: HashMap<String, Value>,
    attrs: Vec<Attribute>,
    data: ContextData,
    assignments: HashMap<String, Assignment>,
    exposures: Vec<Exposure>,
    goals: Vec<Goal>,
    overrides: HashMap<String, i32>,
    cassignments: HashMap<String, i32>,
    state: ContextState,
    failed: bool,
    failed_error: Option<String>,
    pending: usize,
    attrs_seq: u64,
    index: HashMap<String, Experiment>,
    index_variables: HashMap<String, Vec<String>>,
    assigners: HashMap<String, VariantAssigner>,
    hashes: HashMap<String, String>,
    audience_matcher: AudienceMatcher,
    event_logger: Option<crate::models::EventLogger>,
    publish_delay: i64,
    refresh_period: i64,
    data_fetcher: Option<DataFetcher>,
    publisher: Option<Box<dyn ContextPublisher>>,
}

impl Context {
    fn base() -> Self {
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
            failed: false,
            failed_error: None,
            pending: 0,
            attrs_seq: 0,
            index: HashMap::new(),
            index_variables: HashMap::new(),
            assigners: HashMap::new(),
            hashes: HashMap::new(),
            audience_matcher: AudienceMatcher::new(),
            event_logger: None,
            publish_delay: 0,
            refresh_period: 0,
            data_fetcher: None,
            publisher: None,
        }
    }

    pub fn new(data: ContextData) -> Self {
        let mut ctx = Self::base();
        ctx.init(data);
        ctx.state = ContextState::Ready;
        ctx
    }

    /// Create a ready context. The `event_logger` in `options` receives the
    /// `ready` event before this returns.
    pub fn new_with_options(data: ContextData, options: ContextOptions) -> Self {
        let mut ctx = Self::new_loading_with_options(options);
        ctx.become_ready(data);
        ctx
    }

    pub fn new_loading() -> Self {
        Self::base()
    }

    /// Create a context that is still loading. Resolve it with
    /// [`Context::become_ready`] or [`Context::become_failed_with_error`]; the
    /// `event_logger` in `options` then receives the `ready` or `error` event.
    pub fn new_loading_with_options(options: ContextOptions) -> Self {
        let mut ctx = Self::base();
        ctx.event_logger = options.event_logger;
        ctx.publish_delay = options.publish_delay;
        ctx.refresh_period = options.refresh_period;
        ctx
    }

    /// Move a loading context to ready and emit `ready` with the data.
    /// Does nothing if the context is not loading.
    pub fn become_ready(&mut self, data: ContextData) {
        if self.state == ContextState::Loading {
            self.init(data);
            self.state = ContextState::Ready;
            match serde_json::to_value(&self.data) {
                Ok(value) => self.log_event("ready", Some(value)),
                Err(e) => {
                    error!("Failed to serialize context data for ready: {}", e);
                    self.log_event("ready", None);
                }
            }
        }
    }

    /// Move a loading context to failed and emit `error` with no data.
    /// Does nothing if the context is not loading.
    pub fn become_failed(&mut self) {
        if self.state == ContextState::Loading {
            self.init(ContextData::default());
            self.state = ContextState::Failed;
            self.failed = true;
            self.log_event("error", None);
        }
    }

    /// Move a loading context to failed, keep `error` for
    /// [`Context::ready_error`], and emit `error` with `{"message": error}`.
    /// Does nothing if the context is not loading.
    pub fn become_failed_with_error(&mut self, error: String) {
        if self.state == ContextState::Loading {
            self.init(ContextData::default());
            self.state = ContextState::Failed;
            self.failed = true;
            self.failed_error = Some(error.clone());
            self.log_error(&error);
        }
    }

    pub fn ready_error(&self) -> Option<&String> {
        self.failed_error.as_ref()
    }

    pub fn set_event_logger(&mut self, logger: EventLogger) {
        self.event_logger = Some(std::sync::Arc::from(logger));
    }

    pub fn set_publisher(&mut self, publisher: Box<dyn ContextPublisher>) {
        self.publisher = Some(publisher);
    }

    pub fn publish_delay(&self) -> i64 {
        self.publish_delay
    }

    pub fn refresh_period(&self) -> i64 {
        self.refresh_period
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
                                    error!(
                                        "Failed to parse variant config for experiment '{}', variant {}: {}. Config: '{}'",
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

    /// Like the JS SDK, a context whose load failed is still ready (with empty
    /// data), so only a pending load is "not ready".
    pub fn is_ready(&self) -> bool {
        self.state != ContextState::Loading
    }

    /// Whether the context data failed to load. Stays `true` once the
    /// context is finalized.
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    pub fn is_finalized(&self) -> bool {
        self.state == ContextState::Finalized
    }

    pub fn is_finalizing(&self) -> bool {
        self.state == ContextState::Finalizing
    }

    pub fn is_closed(&self) -> bool {
        self.is_finalized()
    }

    pub fn is_closing(&self) -> bool {
        self.is_finalizing()
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    /// The context data. `Err` before the context is ready; still available
    /// once finalized.
    pub fn data(&self) -> Result<&ContextData, String> {
        self.check_ready(false)?;
        Ok(&self.data)
    }

    /// Set the UID for `unit_type`. Like the JS SDK, a UID is a string or a
    /// number and keeps its type: `get_unit` returns it as given. Strings are
    /// trimmed and must not be blank; numbers are hashed as their rounded
    /// integer (see [`crate::utils::hash_unit_value`]).
    pub fn set_unit(&mut self, unit_type: &str, uid: impl Into<Value>) -> Result<(), String> {
        self.check_not_finalized()?;

        let uid = match uid.into() {
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(format!("Unit '{}' UID must not be blank.", unit_type));
                }
                Value::String(trimmed.to_string())
            }
            Value::Number(n) => Value::Number(n),
            _ => return Err(format!("Unit '{}' must be a string or a number.", unit_type)),
        };

        if let Some(existing) = self.units.get(unit_type) {
            if *existing != uid {
                return Err(format!("Unit '{}' UID already set.", unit_type));
            }
        }

        self.units.insert(unit_type.to_string(), uid);
        Ok(())
    }

    pub fn set_units<I, K, V>(&mut self, units: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Value>,
    {
        for (unit_type, uid) in units {
            self.set_unit(&unit_type.into(), uid)?;
        }
        Ok(())
    }

    pub fn get_unit(&self, unit_type: &str) -> Option<&Value> {
        self.units.get(unit_type)
    }

    pub fn get_units(&self) -> &HashMap<String, Value> {
        &self.units
    }

    pub fn set_attribute(&mut self, name: &str, value: impl Into<Value>) -> Result<(), String> {
        self.check_not_finalized()?;

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

    pub fn set_custom_assignment(
        &mut self,
        experiment_name: &str,
        variant: i32,
    ) -> Result<(), String> {
        self.check_not_finalized()?;
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

    /// The variant assigned to `experiment_name`, without queuing an exposure.
    /// `Err` before the context is ready and once it is finalizing or finalized.
    pub fn peek(&mut self, experiment_name: &str) -> Result<i32, String> {
        self.check_ready(true)?;
        Ok(self.assign(experiment_name).variant)
    }

    /// The variant assigned to `experiment_name`; queues an exposure the first
    /// time. `Err` before the context is ready and once it is finalizing or
    /// finalized.
    pub fn treatment(&mut self, experiment_name: &str) -> Result<i32, String> {
        self.check_ready(true)?;
        let assignment = self.assign(experiment_name);
        let variant = assignment.variant;

        if !assignment.exposed {
            if let Some(assignment) = self.assignments.get_mut(experiment_name) {
                assignment.exposed = true;
            }
            self.queue_exposure(experiment_name);
        }

        Ok(variant)
    }

    /// Queue a goal. `properties` must be a JSON object or null; anything
    /// else is rejected, as in the JS SDK.
    pub fn track(&mut self, goal_name: &str, properties: impl Into<Value>) -> Result<(), String> {
        self.check_not_finalized()?;

        let properties = properties.into();
        let properties_map: Option<HashMap<String, Value>> = match properties {
            Value::Null => None,
            Value::Object(map) => Some(map.into_iter().collect()),
            _ => {
                return Err(format!(
                    "Goal '{}' properties must be of type object.",
                    goal_name
                ));
            }
        };

        let goal = Goal {
            name: goal_name.to_string(),
            properties: properties_map,
            achieved_at: now_millis(),
        };

        match serde_json::to_value(&goal) {
            Ok(value) => self.log_event("goal", Some(value)),
            Err(e) => {
                error!("Failed to serialize goal '{}': {}", goal_name, e);
            }
        }
        self.goals.push(goal);
        self.pending += 1;

        Ok(())
    }

    pub fn variable_value(
        &mut self,
        key: &str,
        default_value: impl Into<Value>,
    ) -> Result<Value, String> {
        self.check_ready(true)?;
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
                            return Ok(value.clone());
                        }
                    }
                }
            }
        }
        Ok(default_value.into())
    }

    pub fn peek_variable_value(
        &mut self,
        key: &str,
        default_value: impl Into<Value>,
    ) -> Result<Value, String> {
        self.check_ready(true)?;
        if let Some(experiment_names) = self.index_variables.get(key).cloned() {
            for exp_name in experiment_names {
                let assignment = self.assign(&exp_name);
                if let Some(variables) = &assignment.variables {
                    if let Some(value) = variables.get(key) {
                        if assignment.assigned || assignment.overridden {
                            return Ok(value.clone());
                        }
                    }
                }
            }
        }
        Ok(default_value.into())
    }

    pub fn variable_keys(&self) -> Result<HashMap<String, Vec<String>>, String> {
        self.check_ready(true)?;
        Ok(self.index_variables.clone())
    }

    pub fn custom_field_value(
        &self,
        experiment_name: &str,
        field_name: &str,
    ) -> Result<Option<Value>, String> {
        self.check_ready(true)?;
        Ok(self.custom_field_value_unchecked(experiment_name, field_name))
    }

    fn custom_field_value_unchecked(&self, experiment_name: &str, field_name: &str) -> Option<Value> {
        if let Some(exp) = self.index.get(experiment_name) {
            if let Some(ref custom_fields) = exp.data.custom_field_values {
                if let Some(field) = custom_fields.iter().find(|f| f.name == field_name) {
                    return match field.field_type.as_str() {
                        "text" | "string" => Some(Value::String(field.value.clone())),
                        "number" => {
                            match field.value.parse::<f64>() {
                                Ok(n) => serde_json::Number::from_f64(n)
                                    .map(Value::Number)
                                    .or_else(|| {
                                        warn!(
                                            "Custom field '{}' number out of range: {}",
                                            field_name, n
                                        );
                                        None
                                    }),
                                Err(e) => {
                                    error!("Failed to parse custom field '{}' as number: {}. Value: '{}'", field_name, e, field.value);
                                    None
                                }
                            }
                        }
                        "json" => {
                            if field.value == "null" {
                                Some(Value::Null)
                            } else if field.value.is_empty() {
                                Some(Value::String(String::new()))
                            } else {
                                match serde_json::from_str(&field.value) {
                                    Ok(v) => Some(v),
                                    Err(e) => {
                                        error!("Failed to parse custom field '{}' JSON: {}. Value: '{}'", field_name, e, field.value);
                                        None
                                    }
                                }
                            }
                        }
                        "boolean" => Some(Value::Bool(field.value == "true")),
                        _ => {
                            warn!(
                                "Unknown custom field type '{}' for field '{}'",
                                field.field_type, field_name
                            );
                            None
                        }
                    };
                }
            }
        }
        None
    }

    pub fn custom_field_value_type(
        &self,
        experiment_name: &str,
        field_name: &str,
    ) -> Result<Option<String>, String> {
        self.check_ready(true)?;
        if let Some(exp) = self.index.get(experiment_name) {
            if let Some(ref custom_fields) = exp.data.custom_field_values {
                if let Some(field) = custom_fields.iter().find(|f| f.name == field_name) {
                    return Ok(Some(field.field_type.clone()));
                }
            }
        }
        Ok(None)
    }

    pub fn custom_field_keys(&self) -> Result<Vec<String>, String> {
        self.check_ready(true)?;
        let mut keys = std::collections::HashSet::new();
        for exp in &self.data.experiments {
            if let Some(ref custom_fields) = exp.custom_field_values {
                for field in custom_fields {
                    keys.insert(field.name.clone());
                }
            }
        }
        Ok(keys.into_iter().collect())
    }

    /// The names of the experiments in the context data. `Err` before the
    /// context is ready; still available once finalized.
    pub fn experiments(&self) -> Result<Vec<String>, String> {
        self.check_ready(false)?;
        Ok(self
            .data
            .experiments
            .iter()
            .map(|e| e.name.clone())
            .collect())
    }

    pub fn set_data_fetcher(&mut self, fetcher: DataFetcher) {
        self.data_fetcher = Some(fetcher);
    }

    /// Fetch fresh data with the configured data fetcher and apply it.
    ///
    /// `Err` before the context is ready and once it is finalizing or
    /// finalized. On a failed context (data never loaded) this is a no-op that
    /// returns `Ok`, as in the JS SDK: a refresh does not recover it. A fetch
    /// failure is returned and also emitted as an `error` event.
    pub fn refresh(&mut self) -> Result<(), String> {
        self.check_ready(true)?;
        if self.failed {
            return Ok(());
        }
        let Some(fetcher) = self.data_fetcher.as_ref() else {
            let e = "Cannot refresh: no data fetcher configured".to_string();
            self.log_error(&e);
            return Err(e);
        };
        match fetcher() {
            Ok(new_data) => {
                self.apply_refresh(new_data);
                Ok(())
            }
            Err(e) => {
                self.log_error(&e);
                Err(e)
            }
        }
    }

    /// Apply `new_data` as a refresh, keeping assignments that are still
    /// valid. Same guards as [`Context::refresh`]: `Err` before ready and once
    /// finalizing or finalized, and a no-op `Ok` on a failed context.
    pub fn refresh_with(&mut self, new_data: ContextData) -> Result<(), String> {
        self.check_ready(true)?;
        if !self.failed {
            self.apply_refresh(new_data);
        }
        Ok(())
    }

    fn apply_refresh(&mut self, new_data: ContextData) {
        self.init(new_data);
        let to_remove: Vec<String> = {
            let assignments = &self.assignments;
            let index = &self.index;
            assignments
                .iter()
                .filter_map(|(name, assignment)| {
                    if assignment.overridden {
                        return None;
                    }
                    let should_remove = match index.get(name) {
                        None => assignment.assigned,
                        Some(exp) => !Self::experiment_matches(&exp.data, assignment),
                    };
                    if should_remove {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        for name in to_remove {
            self.assignments.remove(&name);
        }
        match serde_json::to_value(&self.data) {
            Ok(value) => self.log_event("refresh", Some(value)),
            Err(e) => {
                error!("Failed to serialize context data for refresh: {}", e);
            }
        }
    }

    /// Send the pending exposures and goals to the publisher.
    ///
    /// `Err` before the context is ready and once it is finalizing or
    /// finalized. On success the queue is cleared and `publish` is emitted. If
    /// the publisher returns `Err`, the events stay pending (so a later
    /// `publish()` retries them), `error` is emitted, and the error is returned.
    ///
    /// A failed context (data never loaded) does not publish: its queue is
    /// discarded with an `error` event, as in the JS SDK.
    pub fn publish(&mut self) -> Result<(), String> {
        self.check_ready(true)?;
        self.flush()
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.pending == 0 {
            return Ok(());
        }

        if self.failed {
            let message = format!(
                "Discarding {} exposures and {} goals because context failed to initialize",
                self.exposures.len(),
                self.goals.len()
            );
            warn!("{}", message);
            self.pending = 0;
            self.exposures.clear();
            self.goals.clear();
            self.log_error(&message);
            return Ok(());
        }

        let params = self.build_publish_params();

        if let Some(publisher) = &self.publisher {
            if let Err(e) = publisher.publish(&params) {
                self.log_error(&e);
                return Err(e);
            }
        }

        self.pending = 0;
        self.exposures.clear();
        self.goals.clear();

        match serde_json::to_value(&params) {
            Ok(value) => self.log_event("publish", Some(value)),
            Err(e) => {
                error!("Failed to serialize publish params: {}", e);
            }
        }

        Ok(())
    }

    pub fn get_publish_params(&mut self) -> PublishParams {
        self.build_publish_params()
    }

    /// Publish any pending events, then seal the context and emit `finalize`.
    ///
    /// If that publish fails, the context is not finalized: it returns to its
    /// previous state with the events still pending, and the error is returned
    /// so the caller can retry.
    pub fn finalize(&mut self) -> Result<(), String> {
        if self.is_finalized() {
            return Ok(());
        }

        let previous_state = self.state;
        self.state = ContextState::Finalizing;

        if let Err(e) = self.flush() {
            self.state = previous_state;
            return Err(e);
        }

        self.state = ContextState::Finalized;
        self.log_event("finalize", None);
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), String> {
        self.finalize()
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
                                    )
                                        as i32;
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
                    error!(
                        "Failed to serialize exposure for experiment '{}': {}",
                        experiment_name, e
                    );
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
            let hash = hash_unit_value(unit)?;
            self.hashes.insert(unit_type.to_string(), hash.clone());
            return Some(hash);
        }

        None
    }

    fn build_publish_params(&mut self) -> PublishParams {
        let unit_types: Vec<String> = self.units.keys().cloned().collect();
        for unit_type in &unit_types {
            self.unit_hash(unit_type);
        }

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

    fn check_not_finalized(&self) -> Result<(), String> {
        match self.state {
            ContextState::Finalized => Err("ABsmartly Context is finalized.".to_string()),
            ContextState::Finalizing => Err("ABsmartly Context is finalizing.".to_string()),
            _ => Ok(()),
        }
    }

    /// `Err("ABsmartly Context is not yet ready.")` while loading and, with
    /// `expect_not_finalized`, once finalizing or finalized. A failed context
    /// counts as ready (its data is empty), as in the JS SDK.
    fn check_ready(&self, expect_not_finalized: bool) -> Result<(), String> {
        if self.state == ContextState::Loading {
            return Err("ABsmartly Context is not yet ready.".to_string());
        }
        if expect_not_finalized {
            self.check_not_finalized()?;
        }
        Ok(())
    }

    fn log_event(&self, event_name: &str, data: Option<Value>) {
        if let Some(ref logger) = self.event_logger {
            logger(self, event_name, data);
        }
    }

    fn log_error(&self, message: &str) {
        self.log_event("error", Some(json!({ "message": message })));
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|e| {
            warn!("System time error, returning 0: {}", e);
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
                    config: if c.is_empty() {
                        None
                    } else {
                        Some(c.to_string())
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
    fn test_ready_error_is_none_when_ready() {
        let data = make_context_data(vec![]);
        let context = Context::new(data);
        assert!(context.ready_error().is_none());
    }

    #[test]
    fn test_ready_error_is_none_after_become_failed_without_error() {
        let mut context = Context::new_loading();
        context.become_failed();
        assert!(context.is_failed());
        assert!(context.ready_error().is_none());
    }

    #[test]
    fn test_ready_error_returns_error_after_become_failed_with_error() {
        let mut context = Context::new_loading();
        context.become_failed_with_error("data fetch failed".to_string());
        assert!(context.is_failed());
        assert_eq!(
            context.ready_error(),
            Some(&"data fetch failed".to_string())
        );
    }

    #[test]
    fn test_context_set_unit() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert!(context.set_unit("session_id", "user123").is_ok());
        assert_eq!(context.get_unit("session_id"), Some(&serde_json::json!("user123")));
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

        assert_eq!(context.peek("nonexistent_experiment").unwrap(), 0);
    }

    #[test]
    fn test_context_treatment_returns_zero_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        assert_eq!(context.treatment("nonexistent_experiment").unwrap(), 0);
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
        let variant = context.treatment("test_exp").unwrap();
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
        context.peek("test_exp").unwrap();
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
        context.treatment("test_exp").unwrap();
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
        context.treatment("test_exp").unwrap();
        context.treatment("test_exp").unwrap();
        context.treatment("test_exp").unwrap();
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

        context.set_override("test_exp", 1).unwrap();
        context.set_unit("session_id", "test_user").unwrap();

        assert_eq!(context.treatment("test_exp").unwrap(), 1);
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

        let variant = context.treatment("test_exp").unwrap();
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
        context.treatment("test_exp").unwrap();
        context.track("click", ()).unwrap();

        assert_eq!(context.pending(), 2);
        context.publish().unwrap();
        assert_eq!(context.pending(), 0);
    }

    #[test]
    fn test_context_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize().unwrap();
        assert!(context.is_finalized());
    }

    #[test]
    fn test_context_cannot_track_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize().unwrap();
        assert!(context.track("click", ()).is_err());
    }

    #[test]
    fn test_context_cannot_set_unit_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize().unwrap();
        assert!(context.set_unit("session_id", "user123").is_err());
    }

    #[test]
    fn test_context_cannot_set_attribute_after_finalize() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        context.finalize().unwrap();
        assert!(context.set_attribute("country", json!("US")).is_err());
    }

    #[test]
    fn test_context_variable_value_returns_default_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.variable_value("nonexistent", json!("default")).unwrap();
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_context_peek_variable_value_returns_default_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.peek_variable_value("nonexistent", json!(42)).unwrap();
        assert_eq!(value, json!(42));
    }

    #[test]
    fn test_context_experiments_list() {
        let exp1 = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let exp2 = make_experiment("exp2", vec!["{}"], vec![1.0]);
        let data = make_context_data(vec![exp1, exp2]);
        let context = Context::new(data);

        let experiments = context.experiments().unwrap();
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
        assert_eq!(context.treatment("test_exp").unwrap(), 1);
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

        assert_eq!(context.treatment("test_exp").unwrap(), 0);
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

        assert_eq!(context.treatment("test_exp").unwrap(), 1);
    }

    #[test]
    fn test_context_refresh() {
        let exp1 = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let data1 = make_context_data(vec![exp1]);
        let mut context = Context::new(data1);

        let exp2 = make_experiment("exp2", vec!["{}"], vec![1.0]);
        let data2 = make_context_data(vec![exp2]);
        context.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        context.refresh().unwrap();

        let experiments = context.experiments().unwrap();
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

        let value = context.variable_value("nonexistent", "default_value").unwrap();
        assert_eq!(value, json!("default_value"));
    }

    #[test]
    fn test_ergonomic_variable_value_with_number_default() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.variable_value("nonexistent", 42).unwrap();
        assert_eq!(value, json!(42));
    }

    #[test]
    fn test_ergonomic_peek_variable_value_with_bool_default() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context.peek_variable_value("nonexistent", false).unwrap();
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

    fn make_experiment_with_id(
        name: &str,
        id: i64,
        variants: Vec<&str>,
        split: Vec<f64>,
    ) -> ExperimentData {
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
                    config: if c.is_empty() {
                        None
                    } else {
                        Some(c.to_string())
                    },
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
    fn test_refresh_without_fetcher_emits_error_event() {
        let (mut ctx, log) = make_logging_context(make_context_data(vec![]));
        assert!(ctx.refresh().is_err());
        assert!(log.lock().unwrap().iter().any(|e| e.event == "error"));
    }

    #[test]
    fn test_refresh_after_finalize_is_rejected() {
        let mut ctx = Context::new(make_context_data(vec![]));
        ctx.finalize().unwrap();
        assert_eq!(
            ctx.refresh_with(make_context_data(vec![])),
            Err("ABsmartly Context is finalized.".to_string())
        );
        assert_eq!(
            ctx.refresh(),
            Err("ABsmartly Context is finalized.".to_string())
        );
    }

    const NOT_READY: &str = "ABsmartly Context is not yet ready.";
    const FINALIZED: &str = "ABsmartly Context is finalized.";

    fn variable_experiment() -> ExperimentData {
        let mut exp = make_experiment("exp", vec!["{}", r#"{"color":"red"}"#], vec![0.0, 1.0]);
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "field".to_string(),
            value: "x".to_string(),
            field_type: "string".to_string(),
        }]);
        exp
    }

    /// Every read guarded like JS `_checkReady(true)`, as `(name, result)`.
    fn guarded_reads(ctx: &mut Context) -> Vec<(&'static str, Result<(), String>)> {
        vec![
            ("peek", ctx.peek("exp").map(|_| ())),
            ("treatment", ctx.treatment("exp").map(|_| ())),
            ("variable_value", ctx.variable_value("color", "x").map(|_| ())),
            (
                "peek_variable_value",
                ctx.peek_variable_value("color", "x").map(|_| ()),
            ),
            ("variable_keys", ctx.variable_keys().map(|_| ())),
            (
                "custom_field_value",
                ctx.custom_field_value("exp", "field").map(|_| ()),
            ),
            (
                "custom_field_value_type",
                ctx.custom_field_value_type("exp", "field").map(|_| ()),
            ),
            ("custom_field_keys", ctx.custom_field_keys().map(|_| ())),
            ("publish", ctx.publish()),
            ("refresh", ctx.refresh()),
            (
                "refresh_with",
                ctx.refresh_with(make_context_data(vec![variable_experiment()])),
            ),
        ]
    }

    #[test]
    fn test_reads_before_ready_return_not_ready_error() {
        let mut ctx = Context::new_loading();
        ctx.set_unit("session_id", "u").unwrap();
        for (name, result) in guarded_reads(&mut ctx) {
            assert_eq!(result, Err(NOT_READY.to_string()), "{}", name);
        }
        assert_eq!(ctx.experiments(), Err(NOT_READY.to_string()));
        assert!(ctx.data().is_err());
        assert_eq!(ctx.pending(), 0, "no exposure queued while loading");
    }

    #[test]
    fn test_reads_after_finalize_return_finalized_error() {
        let mut ctx = Context::new(make_context_data(vec![variable_experiment()]));
        ctx.set_unit("session_id", "u").unwrap();
        ctx.finalize().unwrap();
        for (name, result) in guarded_reads(&mut ctx) {
            assert_eq!(result, Err(FINALIZED.to_string()), "{}", name);
        }
        assert_eq!(ctx.pending(), 0, "no exposure queued after finalize");
        // As in JS, experiments() and data() only require the context to be ready.
        assert_eq!(ctx.experiments(), Ok(vec!["exp".to_string()]));
        assert!(ctx.data().is_ok());
    }

    #[test]
    fn test_reads_while_finalizing_return_finalizing_error() {
        struct OkPublisher;
        impl ContextPublisher for OkPublisher {
            fn publish(&self, _: &PublishParams) -> Result<(), String> {
                Ok(())
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_in_logger = seen.clone();
        let mut ctx = Context::new(make_context_data(vec![variable_experiment()]));
        ctx.set_unit("session_id", "u").unwrap();
        ctx.set_publisher(Box::new(OkPublisher));
        ctx.treatment("exp").unwrap();
        ctx.set_event_logger(Box::new(move |c, event, _| {
            if event == "publish" {
                seen_in_logger
                    .lock()
                    .unwrap()
                    .push((c.is_finalizing(), c.variable_keys().err()));
            }
        }));
        ctx.finalize().unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(true, Some("ABsmartly Context is finalizing.".to_string()))]
        );
    }

    #[test]
    fn test_failed_context_reads_return_defaults() {
        let mut ctx = Context::new_loading();
        ctx.become_failed_with_error("boom".to_string());
        assert_eq!(ctx.treatment("exp"), Ok(0));
        assert_eq!(ctx.peek("exp"), Ok(0));
        assert_eq!(ctx.variable_value("color", "blue"), Ok(json!("blue")));
        assert_eq!(ctx.variable_keys(), Ok(HashMap::new()));
        assert_eq!(ctx.experiments(), Ok(vec![]));
    }

    #[test]
    fn test_refresh_on_failed_context_is_noop_ok() {
        let (mut ctx, log) = {
            let log: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
            let log_clone = log.clone();
            let mut ctx = Context::new_loading();
            ctx.set_event_logger(Box::new(move |_ctx, event, data| {
                log_clone.lock().unwrap().push(LogEntry {
                    event: event.to_string(),
                    data,
                });
            }));
            (ctx, log)
        };
        ctx.become_failed_with_error("boom".to_string());
        let fetches = Arc::new(Mutex::new(0));
        let fetches_in_fetcher = fetches.clone();
        ctx.set_data_fetcher(Box::new(move || {
            *fetches_in_fetcher.lock().unwrap() += 1;
            Ok(make_context_data(vec![variable_experiment()]))
        }));
        log.lock().unwrap().clear();

        assert_eq!(ctx.refresh(), Ok(()));
        assert_eq!(
            ctx.refresh_with(make_context_data(vec![variable_experiment()])),
            Ok(())
        );

        assert_eq!(*fetches.lock().unwrap(), 0, "a failed context does not fetch");
        assert!(ctx.is_failed());
        assert_eq!(ctx.experiments(), Ok(vec![]), "a failed context is not recovered");
        assert!(log.lock().unwrap().is_empty(), "no refresh or error event");
    }

    #[test]
    fn test_failed_context_publish_discards_with_error_event() {
        let log: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let mut ctx = Context::new_loading();
        ctx.set_event_logger(Box::new(move |_ctx, event, data| {
            log_clone.lock().unwrap().push(LogEntry {
                event: event.to_string(),
                data,
            });
        }));
        ctx.become_failed();
        ctx.track("goal", json!(null)).unwrap();
        log.lock().unwrap().clear();

        assert_eq!(ctx.publish(), Ok(()));
        assert_eq!(ctx.pending(), 0);
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].event, "error");
        assert_eq!(
            log[0].data,
            Some(json!({"message": "Discarding 0 exposures and 1 goals because context failed to initialize"}))
        );
    }

    #[test]
    fn test_failed_context_stays_failed_after_finalize() {
        let mut ctx = Context::new_loading();
        ctx.become_failed();
        ctx.finalize().unwrap();
        assert!(ctx.is_finalized());
        assert!(ctx.is_failed());
    }

    #[test]
    fn test_event_logger_on_exposure() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();

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
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        ctx.publish().unwrap();

        let entries = log.lock().unwrap();
        assert!(entries.iter().any(|e| e.event == "publish"));
    }

    #[test]
    fn test_event_logger_on_finalize() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.finalize().unwrap();

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
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

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
        // A failed context is ready with empty data so reads return defaults.
        assert!(ctx.is_ready());
        assert!(ctx.data().unwrap().experiments.is_empty());
        assert_eq!(ctx.treatment("exp1").unwrap(), 0);
        assert_eq!(
            ctx.variable_value("button", json!("default")).unwrap(),
            json!("default")
        );
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

        assert!(ctx.experiments().unwrap().is_empty());
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
        assert_eq!(ctx.get_unit("session_id"), Some(&serde_json::json!("user123")));
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
        ctx.set_override("exp1", 1).unwrap();

        let exp = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        ctx.become_ready(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("exp1").unwrap(), 1);
    }

    #[test]
    fn test_set_custom_assignment_callable_before_ready() {
        let mut ctx = Context::new_loading();
        ctx.set_custom_assignment("exp1", 1).unwrap();

        let exp = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp]);
        ctx.become_ready(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("exp1").unwrap(), 1);
    }

    #[test]
    fn test_set_custom_assignment_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize().unwrap();

        assert!(ctx.set_custom_assignment("exp1", 1).is_err());
    }

    #[test]
    fn test_treatment_queues_exposure_after_peek() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek("test_exp").unwrap();
        assert_eq!(ctx.pending(), 0);

        ctx.treatment("test_exp").unwrap();
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_treatment_queues_exposure_with_override_variant() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_override("test_exp", 1).unwrap();

        ctx.treatment("test_exp").unwrap();
        assert_eq!(ctx.pending(), 1);

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["overridden"], json!(true));
        assert_eq!(exposure_data["variant"], json!(1));
    }

    #[test]
    fn test_treatment_queues_exposure_with_custom_assignment_variant() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_custom_assignment("test_exp", 1).unwrap();

        ctx.treatment("test_exp").unwrap();
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

        assert_eq!(ctx.treatment("unknown_exp").unwrap(), 0);
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_treatment_not_requeue_on_unknown_experiment() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("unknown_exp").unwrap();
        ctx.treatment("unknown_exp").unwrap();
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_peek_returns_override_variant() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_override("test_exp", 1).unwrap();

        assert_eq!(ctx.peek("test_exp").unwrap(), 1);
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_peek_audience_mismatch_non_strict() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let variant = ctx.peek("test_exp").unwrap();
        assert_eq!(variant, 1);
    }

    #[test]
    fn test_peek_audience_mismatch_strict() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        assert_eq!(ctx.peek("test_exp").unwrap(), 0);
    }

    #[test]
    fn test_treatment_audience_match_queues_with_audience_mismatch_false() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("US")).unwrap();

        ctx.treatment("test_exp").unwrap();

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(false));
    }

    #[test]
    fn test_treatment_audience_mismatch_queues_with_audience_mismatch_true() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = false;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        ctx.treatment("test_exp").unwrap();

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(true));
    }

    #[test]
    fn test_treatment_audience_mismatch_strict_queues_control() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let variant = ctx.treatment("test_exp").unwrap();
        assert_eq!(variant, 0);

        let entries = log.lock().unwrap();
        let exposure_entry = entries.iter().find(|e| e.event == "exposure").unwrap();
        let exposure_data = exposure_entry.data.as_ref().unwrap();
        assert_eq!(exposure_data["audienceMismatch"], json!(true));
        assert_eq!(exposure_data["variant"], json!(0));
    }

    #[test]
    fn test_variable_value_returns_default_when_unassigned() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![1.0, 0.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let value = ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_value_returns_override_values() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_override("test_exp", 1).unwrap();

        let value = ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("red"));
    }

    #[test]
    fn test_variable_value_queues_exposure() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_variable_value_queues_exposure_after_peek() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek_variable_value("button", json!("default")).unwrap();
        assert_eq!(ctx.pending(), 0);

        ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_variable_value_queues_exposure_only_once() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.variable_value("button", json!("default")).unwrap();
        ctx.variable_value("button", json!("default")).unwrap();
        ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_peek_variable_value_does_not_queue_exposure() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.peek_variable_value("button", json!("default")).unwrap();
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_peek_variable_value_returns_override_values() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_override("test_exp", 1).unwrap();

        let value = ctx.peek_variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("red"));
    }

    #[test]
    fn test_peek_variable_value_returns_default_when_unassigned() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![1.0, 0.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let value = ctx.peek_variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_keys_returns_all_active_keys() {
        let exp1 = make_experiment(
            "exp1",
            vec!["{}", r#"{"button":"red","header":"large"}"#],
            vec![0.5, 0.5],
        );
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let ctx = Context::new(data);

        let keys = ctx.variable_keys().unwrap();
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

        assert!(ctx
            .track("purchase", json!({"amount": 99.99, "count": 1}))
            .is_ok());
        assert_eq!(ctx.pending(), 1);
    }

    #[test]
    fn test_track_rejects_non_object_properties() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        for props in [json!(5), json!("str"), json!([1, 2]), json!(true)] {
            assert_eq!(
                ctx.track("purchase", props),
                Err("Goal 'purchase' properties must be of type object.".to_string())
            );
        }
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_numeric_unit_keeps_type_and_hashes_like_js() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.set_unit("user_id", 125).unwrap();
        ctx.set_unit("float_id", 125.75).unwrap();
        assert_eq!(ctx.get_unit("user_id"), Some(&json!(125)));
        assert_eq!(ctx.get_unit("float_id"), Some(&json!(125.75)));
        assert_eq!(
            ctx.set_unit("user_id", "125"),
            Err("Unit 'user_id' UID already set.".to_string())
        );
        assert_eq!(
            ctx.set_unit("bad", json!(true)),
            Err("Unit 'bad' must be a string or a number.".to_string())
        );
        ctx.track("g", Value::Null).unwrap();
        let params = ctx.get_publish_params();
        let float = params.units.iter().find(|u| u.unit_type == "float_id").unwrap();
        assert_eq!(float.uid, Some(crate::utils::hash_unit("126")));
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
        ctx.finalize().unwrap();
        assert!(ctx.track("click", ()).is_err());
    }

    #[test]
    fn test_publish_does_not_call_when_queue_empty() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.publish().unwrap();

        let entries = log.lock().unwrap();
        assert!(!entries.iter().any(|e| e.event == "publish"));
    }

    #[test]
    fn test_publish_clears_queue_on_success() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        ctx.track("click", ()).unwrap();
        assert!(ctx.pending() > 0);

        ctx.publish().unwrap();
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_finalize_does_not_call_publish_when_queue_empty() {
        let data = make_context_data(vec![]);
        let (mut ctx, log) = make_logging_context(data);

        ctx.finalize().unwrap();

        let entries = log.lock().unwrap();
        assert!(!entries.iter().any(|e| e.event == "publish"));
        assert!(entries.iter().any(|e| e.event == "finalize"));
    }

    #[test]
    fn test_finalize_calls_publish_when_pending() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let (mut ctx, log) = make_logging_context(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        assert!(ctx.pending() > 0);

        ctx.finalize().unwrap();
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

        ctx.finalize().unwrap();
        assert!(ctx.is_finalized());
        ctx.finalize().unwrap();
        assert!(ctx.is_finalized());
    }

    #[test]
    fn test_refresh_keeps_overrides() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_override("test_exp", 1).unwrap();

        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();
        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);
    }

    #[test]
    fn test_refresh_keeps_custom_assignments() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_custom_assignment("test_exp", 1).unwrap();

        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();
        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);
    }

    #[test]
    fn test_refresh_picks_up_fullon_change() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();

        let mut exp2 = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp2.full_on_variant = 1;
        let data2 = make_context_data(vec![exp2]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);
    }

    #[test]
    fn test_refresh_picks_up_traffic_split_change() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();

        let mut exp2 = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp2.traffic_split = vec![0.0, 1.0];
        let data2 = make_context_data(vec![exp2]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        let _variant = ctx.treatment("test_exp").unwrap();
        assert!(ctx.pending() > 0);
    }

    #[test]
    fn test_refresh_picks_up_iteration_change() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        let pending_before = ctx.pending();

        let mut exp2 = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp2.iteration = 2;
        let data2 = make_context_data(vec![exp2]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        ctx.treatment("test_exp").unwrap();
        assert!(ctx.pending() > pending_before);
    }

    #[test]
    fn test_refresh_picks_up_id_change() {
        let exp = make_experiment_with_id(
            "test_exp",
            1,
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        let pending_before = ctx.pending();

        let exp2 = make_experiment_with_id(
            "test_exp",
            2,
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data2 = make_context_data(vec![exp2]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        ctx.treatment("test_exp").unwrap();
        assert!(ctx.pending() > pending_before);
    }

    #[test]
    fn test_refresh_no_change_same_variant() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp.clone()]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        let variant1 = ctx.treatment("test_exp").unwrap();
        assert_eq!(ctx.pending(), 1);

        let data2 = make_context_data(vec![exp]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        let variant2 = ctx.treatment("test_exp").unwrap();
        assert_eq!(variant1, variant2);
    }

    #[test]
    fn test_custom_field_keys() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
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

        let keys = ctx.custom_field_keys().unwrap();
        assert!(keys.contains(&"country".to_string()));
        assert!(keys.contains(&"overrides".to_string()));
    }

    #[test]
    fn test_custom_field_value_string() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "country".to_string(),
            value: "US".to_string(),
            field_type: "string".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "country").unwrap(),
            Some(json!("US"))
        );
    }

    #[test]
    fn test_custom_field_value_text() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "description".to_string(),
            value: "A test experiment".to_string(),
            field_type: "text".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "description").unwrap(),
            Some(json!("A test experiment"))
        );
    }

    #[test]
    fn test_custom_field_value_json() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "overrides".to_string(),
            value: r#"{"key":"value"}"#.to_string(),
            field_type: "json".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "overrides").unwrap(),
            Some(json!({"key": "value"}))
        );
    }

    #[test]
    fn test_custom_field_value_number() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "priority".to_string(),
            value: "5".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "priority").unwrap(),
            Some(json!(5.0))
        );
    }

    #[test]
    fn test_custom_field_value_decimal() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "weight".to_string(),
            value: "1.5".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "weight").unwrap(),
            Some(json!(1.5))
        );
    }

    #[test]
    fn test_custom_field_value_boolean() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "enabled".to_string(),
            value: "true".to_string(),
            field_type: "boolean".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "enabled").unwrap(),
            Some(json!(true))
        );
    }

    #[test]
    fn test_custom_field_value_boolean_false() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "enabled".to_string(),
            value: "false".to_string(),
            field_type: "boolean".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("test_exp", "enabled").unwrap(),
            Some(json!(false))
        );
    }

    #[test]
    fn test_custom_field_value_null_for_nonexistent_field() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "country".to_string(),
            value: "US".to_string(),
            field_type: "string".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "nonexistent").unwrap(), None);
    }

    #[test]
    fn test_custom_field_value_null_for_nonexistent_experiment() {
        let data = make_context_data(vec![]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("nonexistent_exp", "field").unwrap(), None);
    }

    #[test]
    fn test_custom_field_value_null_for_experiment_without_custom_fields() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "field").unwrap(), None);
    }

    #[test]
    fn test_custom_field_value_type() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
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

        assert_eq!(
            ctx.custom_field_value_type("test_exp", "country").unwrap(),
            Some("string".to_string())
        );
        assert_eq!(
            ctx.custom_field_value_type("test_exp", "priority").unwrap(),
            Some("number".to_string())
        );
        assert_eq!(
            ctx.custom_field_value_type("test_exp", "enabled").unwrap(),
            Some("boolean".to_string())
        );
        assert_eq!(ctx.custom_field_value_type("test_exp", "nonexistent").unwrap(), None);
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
        assert_eq!(units.get("session_id"), Some(&serde_json::json!("user123")));
        assert_eq!(units.get("device_id"), Some(&serde_json::json!("device456")));
    }

    #[test]
    fn test_set_units_multiple() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_units([("session_id", "user123"), ("device_id", "device456")])
            .unwrap();

        assert_eq!(ctx.get_unit("session_id"), Some(&serde_json::json!("user123")));
        assert_eq!(ctx.get_unit("device_id"), Some(&serde_json::json!("device456")));
    }

    #[test]
    fn test_set_overrides_multiple() {
        let exp1 = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.set_overrides([("exp1", 1), ("exp2", 0)]).unwrap();

        assert_eq!(ctx.treatment("exp1").unwrap(), 1);
        assert_eq!(ctx.treatment("exp2").unwrap(), 0);
    }

    #[test]
    fn test_set_custom_assignments_multiple() {
        let exp1 = make_experiment("exp1", vec!["{}", r#"{"button":"red"}"#], vec![0.5, 0.5]);
        let exp2 = make_experiment("exp2", vec!["{}", r#"{"color":"blue"}"#], vec![0.5, 0.5]);
        let data = make_context_data(vec![exp1, exp2]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.set_custom_assignments([("exp1", 1), ("exp2", 0)])
            .unwrap();

        assert_eq!(ctx.treatment("exp1").unwrap(), 1);
        assert_eq!(ctx.treatment("exp2").unwrap(), 0);
    }

    #[test]
    fn test_set_attributes_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize().unwrap();

        assert!(ctx.set_attributes([("country", json!("US"))]).is_err());
    }

    #[test]
    fn test_set_custom_assignments_throws_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);
        ctx.finalize().unwrap();

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

        let ctx_data = ctx.data().unwrap();
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

        assert_eq!(
            ctx.custom_field_value("test_exp", "nullfield").unwrap(),
            Some(Value::Null)
        );
    }

    #[test]
    fn test_variable_value_audience_mismatch_strict_returns_default() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("UK")).unwrap();

        let value = ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_variable_value_audience_match_returns_value() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = r#"{"filter":[{"eq":[{"var":"country"},{"value":"US"}]}]}"#.to_string();
        exp.audience_strict = true;
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.set_attribute("country", json!("US")).unwrap();

        let value = ctx.variable_value("button", json!("default")).unwrap();
        assert_eq!(value, json!("red"));
    }

    #[test]
    fn test_base_constructor_deduplication() {
        let ctx1 = Context::new(make_context_data(vec![]));
        let ctx2 = Context::new_loading();
        assert!(ctx1.is_ready());
        assert!(!ctx2.is_ready());
        assert_eq!(ctx1.pending(), 0);
        assert_eq!(ctx2.pending(), 0);
    }

    #[test]
    fn test_new_with_options_sets_fields() {
        let options = ContextOptions {
            publish_delay: 100,
            refresh_period: 200,
            event_logger: None,
        };
        let ctx = Context::new_with_options(make_context_data(vec![]), options);
        assert!(ctx.is_ready());
        assert_eq!(ctx.publish_delay(), 100);
        assert_eq!(ctx.refresh_period(), 200);
    }

    #[test]
    fn test_custom_field_number_out_of_range_returns_none() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "bad_num".to_string(),
            value: "NaN".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "bad_num").unwrap(), None);
    }

    #[test]
    fn test_custom_field_infinity_returns_none() {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        exp.custom_field_values = Some(vec![CustomFieldValue {
            name: "inf_num".to_string(),
            value: "inf".to_string(),
            field_type: "number".to_string(),
        }]);
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        assert_eq!(ctx.custom_field_value("test_exp", "inf_num").unwrap(), None);
    }

    #[test]
    fn test_publish_clears_pending_even_on_serialization_path() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        ctx.treatment("test_exp").unwrap();
        ctx.track("click", ()).unwrap();
        assert_eq!(ctx.pending(), 2);

        ctx.publish().unwrap();
        assert_eq!(ctx.pending(), 0);

        ctx.publish().unwrap();
        assert_eq!(ctx.pending(), 0);
    }

    #[test]
    fn test_variable_keys_returns_clone() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red","header":"large"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let ctx = Context::new(data);

        let keys = ctx.variable_keys().unwrap();
        assert!(keys.contains_key("button"));
        assert!(keys.contains_key("header"));
    }

    #[test]
    fn test_publish_params_include_all_unit_hashes() {
        let exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.5, 0.5],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);

        ctx.set_unit("session_id", "user123").unwrap();
        ctx.set_unit("device_id", "device456").unwrap();

        ctx.track("click", ()).unwrap();

        let params = ctx.get_publish_params();
        for unit in &params.units {
            assert!(
                unit.uid.is_some(),
                "Unit '{}' should have a hashed uid",
                unit.unit_type
            );
        }
        assert_eq!(params.units.len(), 2);
    }

    #[test]
    fn test_set_override_succeeds_after_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.finalize().unwrap();
        assert!(ctx.is_finalized());

        let result = ctx.set_override("exp_test", 2);
        assert!(result.is_ok(), "set_override should succeed after finalize");
    }

    #[test]
    fn test_close_is_alias_for_finalize() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        assert!(!ctx.is_closed());
        ctx.close().unwrap();
        assert!(ctx.is_closed());
        assert!(ctx.is_finalized());
    }

    #[test]
    fn test_is_closed_matches_is_finalized() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        assert_eq!(ctx.is_closed(), ctx.is_finalized());
        ctx.finalize().unwrap();
        assert_eq!(ctx.is_closed(), ctx.is_finalized());
    }

    #[test]
    fn test_is_closing_matches_is_finalizing() {
        let data = make_context_data(vec![]);
        let ctx = Context::new(data);

        assert_eq!(ctx.is_closing(), ctx.is_finalizing());
    }

    #[test]
    fn test_finalized_error_message() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.finalize().unwrap();
        let result = ctx.set_unit("session_id", "uid");
        assert_eq!(result.unwrap_err(), "ABsmartly Context is finalized.");
    }

    #[test]
    fn test_unit_uid_blank_error_message() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        let result = ctx.set_unit("session_id", "");
        assert_eq!(
            result.unwrap_err(),
            "Unit 'session_id' UID must not be blank."
        );
    }

    #[test]
    fn test_unit_uid_already_set_error_message() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_unit("session_id", "uid1").unwrap();
        let result = ctx.set_unit("session_id", "uid2");
        assert_eq!(result.unwrap_err(), "Unit 'session_id' UID already set.");
    }

    fn logging_options() -> (ContextOptions, Arc<Mutex<Vec<LogEntry>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let options = ContextOptions {
            event_logger: Some(Arc::new(move |_ctx, event, data| {
                log_clone.lock().unwrap().push(LogEntry {
                    event: event.to_string(),
                    data,
                });
            })),
            ..Default::default()
        };
        (options, log)
    }

    fn event_names(log: &Arc<Mutex<Vec<LogEntry>>>) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|e| e.event.clone())
            .collect()
    }

    /// Publisher whose next `fail_next` calls fail; counts every call.
    struct FlakyPublisher {
        fail_next: Arc<Mutex<usize>>,
        calls: Arc<Mutex<usize>>,
    }

    impl ContextPublisher for FlakyPublisher {
        fn publish(&self, _params: &PublishParams) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            let mut fail_next = self.fail_next.lock().unwrap();
            if *fail_next > 0 {
                *fail_next -= 1;
                return Err("Publish failed".to_string());
            }
            Ok(())
        }
    }

    fn flaky_publisher(failures: usize) -> (Box<FlakyPublisher>, Arc<Mutex<usize>>) {
        let calls = Arc::new(Mutex::new(0));
        let publisher = Box::new(FlakyPublisher {
            fail_next: Arc::new(Mutex::new(failures)),
            calls: calls.clone(),
        });
        (publisher, calls)
    }

    fn exposed_context(options: ContextOptions) -> Context {
        let exp = make_experiment("test_exp", vec!["{}", "{}"], vec![0.5, 0.5]);
        let mut ctx = Context::new_with_options(make_context_data(vec![exp]), options);
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.treatment("test_exp").unwrap();
        ctx.track("click", ()).unwrap();
        ctx
    }

    #[test]
    fn test_new_with_options_emits_ready_with_data() {
        let exp = make_experiment("exp1", vec!["{}"], vec![1.0]);
        let (options, log) = logging_options();
        let ctx = Context::new_with_options(make_context_data(vec![exp]), options);

        assert!(ctx.is_ready());
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event, "ready");
        let data = entries[0]
            .data
            .as_ref()
            .expect("ready carries context data");
        assert_eq!(data["experiments"][0]["name"], "exp1");
    }

    #[test]
    fn test_become_ready_emits_ready_once() {
        let (options, log) = logging_options();
        let mut ctx = Context::new_loading_with_options(options);
        assert!(event_names(&log).is_empty());

        ctx.become_ready(make_context_data(vec![]));
        ctx.become_ready(make_context_data(vec![]));

        assert_eq!(event_names(&log), vec!["ready"]);
    }

    #[test]
    fn test_become_failed_with_error_emits_error_and_sets_ready_error() {
        let (options, log) = logging_options();
        let mut ctx = Context::new_loading_with_options(options);

        ctx.become_failed_with_error("fetch failed".to_string());

        assert!(ctx.is_failed());
        assert_eq!(ctx.ready_error(), Some(&"fetch failed".to_string()));
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event, "error");
        assert_eq!(entries[0].data, Some(json!({"message": "fetch failed"})));
    }

    #[test]
    fn test_become_failed_emits_error_once() {
        let (options, log) = logging_options();
        let mut ctx = Context::new_loading_with_options(options);

        ctx.become_failed();
        ctx.become_failed();
        ctx.become_ready(make_context_data(vec![]));

        assert!(ctx.is_failed());
        assert_eq!(event_names(&log), vec!["error"]);
    }

    #[test]
    fn test_failed_context_discards_pending_on_publish() {
        let (publisher, calls) = flaky_publisher(0);
        let mut ctx = Context::new_loading();
        ctx.set_publisher(publisher);
        ctx.track("click", ()).unwrap();
        ctx.become_failed_with_error("fetch failed".to_string());

        assert!(ctx.publish().is_ok());
        assert_eq!(ctx.pending(), 0);
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn test_publish_failure_preserves_pending_and_emits_error() {
        let (options, log) = logging_options();
        let mut ctx = exposed_context(options);
        let (publisher, calls) = flaky_publisher(1);
        ctx.set_publisher(publisher);
        assert_eq!(ctx.pending(), 2);
        log.lock().unwrap().clear();

        let result = ctx.publish();

        assert_eq!(result, Err("Publish failed".to_string()));
        assert_eq!(ctx.pending(), 2);
        assert_eq!(*calls.lock().unwrap(), 1);
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event, "error");
        assert_eq!(entries[0].data, Some(json!({"message": "Publish failed"})));
    }

    #[test]
    fn test_publish_retry_after_failure_sends_same_events() {
        let exp = make_experiment("test_exp", vec!["{}", "{}"], vec![0.5, 0.5]);
        let mut ctx = Context::new(make_context_data(vec![exp]));
        ctx.set_unit("session_id", "test_user").unwrap();
        ctx.treatment("test_exp").unwrap();
        ctx.track("click", ()).unwrap();

        let sent = Arc::new(Mutex::new(Vec::<PublishParams>::new()));
        struct Recorder {
            attempts: Mutex<usize>,
            sent: Arc<Mutex<Vec<PublishParams>>>,
        }
        impl ContextPublisher for Recorder {
            fn publish(&self, params: &PublishParams) -> Result<(), String> {
                let mut attempts = self.attempts.lock().unwrap();
                *attempts += 1;
                if *attempts == 1 {
                    return Err("Publish failed".to_string());
                }
                self.sent.lock().unwrap().push(params.clone());
                Ok(())
            }
        }
        ctx.set_publisher(Box::new(Recorder {
            attempts: Mutex::new(0),
            sent: sent.clone(),
        }));

        assert!(ctx.publish().is_err());
        assert!(ctx.publish().is_ok());
        assert_eq!(ctx.pending(), 0);

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].exposures.as_ref().map(Vec::len), Some(1));
        assert_eq!(sent[0].goals.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn test_publish_success_emits_publish_and_clears_pending() {
        let (options, log) = logging_options();
        let mut ctx = exposed_context(options);
        let (publisher, calls) = flaky_publisher(0);
        ctx.set_publisher(publisher);
        log.lock().unwrap().clear();

        assert!(ctx.publish().is_ok());

        assert_eq!(ctx.pending(), 0);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(event_names(&log), vec!["publish"]);
    }

    #[test]
    fn test_finalize_failure_keeps_context_open_and_pending() {
        let (options, log) = logging_options();
        let mut ctx = exposed_context(options);
        let (publisher, _calls) = flaky_publisher(1);
        ctx.set_publisher(publisher);
        log.lock().unwrap().clear();

        assert_eq!(ctx.finalize(), Err("Publish failed".to_string()));
        assert!(!ctx.is_finalized());
        assert!(!ctx.is_finalizing());
        assert!(ctx.is_ready());
        assert_eq!(ctx.pending(), 2);
        assert_eq!(event_names(&log), vec!["error"]);

        assert!(ctx.finalize().is_ok());
        assert!(ctx.is_finalized());
        assert_eq!(ctx.pending(), 0);
        assert_eq!(event_names(&log), vec!["error", "publish", "finalize"]);
    }

    #[test]
    fn test_refresh_fetch_failure_emits_error() {
        let (options, log) = logging_options();
        let mut ctx = Context::new_with_options(make_context_data(vec![]), options);
        ctx.set_data_fetcher(Box::new(|| Err("Refresh fetch failed: boom".to_string())));
        log.lock().unwrap().clear();

        assert_eq!(ctx.refresh(), Err("Refresh fetch failed: boom".to_string()));
        assert_eq!(event_names(&log), vec!["error"]);
    }
}
