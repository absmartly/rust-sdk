use log::{error, warn};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assigner::VariantAssigner;
use crate::context_publisher::ContextPublisher;
use crate::matcher::{AudienceMatcher, RuleAction};
use crate::models::*;
use crate::utils::{array_equals_shallow, hash_unit_value};

pub type EventLogger = Box<dyn Fn(&Context, &str, Option<Value>) + Send + Sync>;

struct Experiment {
    data: ExperimentData,
    variables: Vec<HashMap<String, Value>>,
    /// `data.assignment_rules` parsed once when the data is installed; `None`
    /// when there are none or they are not valid JSON.
    rules: Option<Arc<Vec<Value>>>,
    /// Changes only when a data load changes the experiment's
    /// `assignment_rules`, so cached assignments compare it instead of the
    /// rule text.
    rules_rev: u64,
    /// Sorted by id and unique, as in JS.
    holdouts: Vec<Arc<ExperimentData>>,
}

/// A unit's arm in one holdout epoch, shared by every assignment that
/// pinned it so the holdout's exposure is queued once per context.
#[derive(Debug)]
struct HoldoutAssignment {
    id: i64,
    iteration: i64,
    variant: i32,
    /// `split.len()` when `variant` was drawn: arm 1 means something else
    /// for a 3-arm holdout, and a same-iteration refresh must not change it.
    arm_count: usize,
    unit_type: String,
    exposed: AtomicBool,
}

#[derive(Debug, Clone)]
struct PinnedHoldout {
    data: Arc<ExperimentData>,
    assignment: Option<Arc<HoldoutAssignment>>,
}

/// Arm 0 holds a unit out of every covered experiment. Arm 1 of a 3-arm
/// (`all_full_on`) holdout only allows full-on experiments.
fn is_held_out_by(variant: i32, arm_count: usize, full_on_variant: i64) -> bool {
    variant == 0 || (arm_count == 3 && variant == 1 && full_on_variant == 0)
}

/// What a cached assignment's rule outcome was resolved against. Kept beside
/// the public [`Assignment`] rather than in it, so it stays an internal
/// detail of the cache.
#[derive(Debug, Clone, Copy, Default)]
struct RuleState {
    /// The variant was set by a matching assignment rule.
    rule_override: bool,
    /// A split rule matched but the unit was missing, so the full-on variant
    /// was used; setting the unit later re-randomizes it while unexposed.
    missing_unit: bool,
    /// The experiment's `rules_rev` when the rules were resolved.
    rules_rev: u64,
    /// The experiment's variant count when the rules were resolved; a change
    /// can put a fixed variant out of range or invalidate a split.
    variant_count: usize,
}

#[derive(Debug, Default)]
struct CachedAssignment {
    assignment: Assignment,
    rule: RuleState,
    /// A covering holdout holds the unit out: control values, and no
    /// exposure of its own unless an override or a rule set the variant.
    suppressed: bool,
    holdouts: Vec<PinnedHoldout>,
    /// A refresh changed the experiment's holdout coverage. Checked on the
    /// next read, as in JS, so a change that a later refresh undoes keeps
    /// the assignment and its exposure.
    coverage_stale: AtomicBool,
}

pub type DataFetcher = Box<dyn Fn() -> Result<ContextData, String> + Send + Sync>;

pub struct Context {
    units: HashMap<String, Value>,
    attrs: Vec<Attribute>,
    data: ContextData,
    assignments: HashMap<String, CachedAssignment>,
    exposures: Vec<Exposure>,
    goals: Vec<Goal>,
    overrides: HashMap<String, i32>,
    cassignments: HashMap<String, i32>,
    state: ContextState,
    failed: bool,
    failed_error: Option<String>,
    pending: usize,
    attrs_seq: u64,
    rules_rev: u64,
    index: HashMap<String, Experiment>,
    index_variables: HashMap<String, Vec<String>>,
    /// Holdout arms by (holdout id, covered experiment's unit type); kept
    /// across refreshes and replaced only when the iteration changes.
    holdout_assignments: HashMap<(i64, String), Arc<HoldoutAssignment>>,
    assigners: HashMap<String, VariantAssigner>,
    hashes: HashMap<String, String>,
    audience_matcher: AudienceMatcher,
    event_logger: Option<crate::models::EventLogger>,
    publish_delay: i64,
    refresh_period: i64,
    data_fetcher: Option<DataFetcher>,
    publisher: Option<Box<dyn ContextPublisher>>,
    /// The client's `application` and `environment` names, seeded under
    /// user attributes when evaluating audiences and rules.
    system_attrs: HashMap<String, Value>,
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
            rules_rev: 0,
            index: HashMap::new(),
            index_variables: HashMap::new(),
            holdout_assignments: HashMap::new(),
            assigners: HashMap::new(),
            hashes: HashMap::new(),
            audience_matcher: AudienceMatcher::new(),
            event_logger: None,
            publish_delay: 0,
            refresh_period: 0,
            data_fetcher: None,
            publisher: None,
            system_attrs: HashMap::new(),
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

    /// Record the client's `application` and `environment` names so audiences
    /// can target them. These are not stored as user attributes: they seed the
    /// evaluation map and any user attribute of the same name overrides them.
    /// Empty names are ignored, so the key is simply absent when nothing is
    /// configured. Changing a stored value bumps the attribute sequence, so a
    /// previously cached assignment is re-evaluated against the new value.
    pub fn set_system_attributes(&mut self, application: Option<&str>, environment: Option<&str>) {
        let system_attrs: HashMap<String, Value> =
            [("application", application), ("environment", environment)]
                .into_iter()
                .filter_map(|(name, value)| {
                    value
                        .filter(|v| !v.is_empty())
                        .map(|v| (name.to_string(), Value::from(v)))
                })
                .collect();
        if system_attrs != self.system_attrs {
            self.system_attrs = system_attrs;
            self.attrs_seq += 1;
        }
    }

    pub fn publish_delay(&self) -> i64 {
        self.publish_delay
    }

    pub fn refresh_period(&self) -> i64 {
        self.refresh_period
    }

    fn init(&mut self, data: ContextData) {
        self.data = data;
        let mut previous = std::mem::take(&mut self.index);
        self.index_variables.clear();
        let holdouts_by_id: HashMap<i64, Arc<ExperimentData>> = self
            .data
            .holdouts
            .iter()
            .filter(|holdout| !holdout.split.is_empty())
            .map(|holdout| (holdout.id, Arc::new(holdout.clone())))
            .collect();

        // Each key's candidates, by experiment id and once each, as in Java.
        let mut candidates: HashMap<String, Vec<(i64, String)>> = HashMap::new();
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
                    candidates
                        .entry(key.clone())
                        .or_default()
                        .push((experiment.id, experiment.name.clone()));
                }

                variables.push(parsed);
            }

            let unchanged = previous
                .remove(&experiment.name)
                .filter(|old| old.data.assignment_rules == experiment.assignment_rules);
            let (rules, rules_rev) = match unchanged {
                Some(old) => (old.rules, old.rules_rev),
                None => {
                    let rules = match self
                        .audience_matcher
                        .parse_rules(&experiment.assignment_rules)
                    {
                        Ok(rules) => rules.map(Arc::new),
                        Err(e) => {
                            warn!(
                                "Invalid assignmentRules JSON for experiment '{}'; its rules are ignored: {}",
                                experiment.name, e
                            );
                            None
                        }
                    };
                    self.rules_rev += 1;
                    (rules, self.rules_rev)
                }
            };

            let mut holdouts: Vec<Arc<ExperimentData>> = experiment
                .holdout_ids
                .iter()
                .filter_map(|id| holdouts_by_id.get(id).cloned())
                .collect();
            holdouts.sort_by_key(|holdout| holdout.id);
            holdouts.dedup_by_key(|holdout| holdout.id);

            self.index.insert(
                experiment.name.clone(),
                Experiment {
                    data: experiment.clone(),
                    variables,
                    rules,
                    rules_rev,
                    holdouts,
                },
            );
        }

        for (key, mut experiments) in candidates {
            experiments.sort_by_key(|(id, _)| *id);
            experiments.dedup_by_key(|(id, _)| *id);
            let names = experiments.into_iter().map(|(_, name)| name).collect();
            self.index_variables.insert(key, names);
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
            _ => {
                return Err(format!(
                    "Unit '{}' must be a string or a number.",
                    unit_type
                ))
            }
        };

        if let Some(existing) = self.units.get(unit_type) {
            if *existing != uid {
                return Err(format!("Unit '{}' UID already set.", unit_type));
            }
        }

        self.units.insert(unit_type.to_string(), uid);

        // A split rule or a covering holdout resolved without this unit is
        // resolved again now that it is available, but only while the
        // assignment is still unseen.
        self.assignments.retain(|_, a| {
            !((a.rule.missing_unit || a.holdouts.iter().any(|h| h.assignment.is_none()))
                && !a.assignment.exposed
                && a.assignment.unit_type.as_deref() == Some(unit_type))
        });
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

    /// The attribute map audiences are evaluated against: the client's
    /// `application` and `environment` names first (when configured and
    /// non-empty), then every user-set attribute overlaid on top so a user
    /// value of the same name always wins, including one set after the context
    /// was created. Because this folds in the current attributes, a later
    /// `set_attribute` bumps `attrs_seq` and invalidates any cached
    /// assignment, which is then re-evaluated against the new map.
    fn evaluation_attributes(&self) -> HashMap<String, Value> {
        let mut attrs = self.system_attrs.clone();
        for attr in &self.attrs {
            attrs.insert(attr.name.clone(), attr.value.clone());
        }
        attrs
    }

    fn system_environment(&self) -> Option<&str> {
        self.system_attrs.get("environment").and_then(Value::as_str)
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
        Ok(self.assign(experiment_name).assignment.variant)
    }

    /// The variant assigned to `experiment_name`; queues an exposure the first
    /// time. `Err` before the context is ready and once it is finalizing or
    /// finalized.
    pub fn treatment(&mut self, experiment_name: &str) -> Result<i32, String> {
        self.check_ready(true)?;
        let assignment = &self.assign(experiment_name).assignment;
        let (variant, exposed) = (assignment.variant, assignment.exposed);

        if !exposed {
            if let Some(entry) = self.assignments.get_mut(experiment_name) {
                entry.assignment.exposed = true;
            }
            self.trigger_exposures(experiment_name);
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
        Ok(self
            .resolve_variable_value(key)
            .unwrap_or_else(|| default_value.into()))
    }

    pub fn peek_variable_value(
        &mut self,
        key: &str,
        default_value: impl Into<Value>,
    ) -> Result<Value, String> {
        self.check_ready(true)?;
        Ok(self
            .peek_variable(key)
            .unwrap_or_else(|| default_value.into()))
    }

    /// [`Self::resolve_variable_value`] without exposures, kept as its own
    /// walk so a peek does not pay for the exposure bookkeeping.
    fn peek_variable(&mut self, key: &str) -> Option<Value> {
        let candidate_count = self.index_variables.get(key)?.len();
        let mut suppressed_fallback: Option<String> = None;
        for i in 0..candidate_count {
            let exp_name = self.index_variables[key][i].clone();
            let cached = self.assign(&exp_name);
            let a = &cached.assignment;
            if a.assigned || a.overridden || cached.rule.rule_override {
                return a.variables.as_ref()?.get(key).cloned();
            }
            if cached.suppressed && suppressed_fallback.is_none() {
                suppressed_fallback = Some(exp_name);
            }
        }
        self.control_value(suppressed_fallback?, key)
    }

    /// The value of `key` from the experiment that decides it: the first
    /// candidate, in id order, that the unit is in (assigned, overridden or
    /// set by a rule), else the first held-out candidate's control value.
    /// The winner's variant decides even when it lacks the key, so a later
    /// candidate never takes over because of the arm the unit got.
    ///
    /// Only the winner is exposed. As in Java, every candidate checked
    /// queues its holdouts' exposures, and a logger panic from those is
    /// resumed once the walk ends, before the winner is exposed.
    fn resolve_variable_value(&mut self, key: &str) -> Option<Value> {
        let candidate_count = self.index_variables.get(key)?.len();
        let mut suppressed_fallback: Option<String> = None;
        let mut deferred_panic = None;
        for i in 0..candidate_count {
            let exp_name = self.index_variables[key][i].clone();
            let cached = self.assign(&exp_name);
            let a = &cached.assignment;
            let wins = a.assigned || a.overridden || cached.rule.rule_override;
            let suppressed = cached.suppressed;
            let pending_holdout = Self::has_unexposed_holdout(cached);
            let winner = if wins {
                let value = a.variables.as_ref().and_then(|v| v.get(key)).cloned();
                if a.exposed && !pending_holdout && deferred_panic.is_none() {
                    #[cfg(test)]
                    WARMED_READS.with(|n| n.set(n.get() + 1));
                    return value;
                }
                Some((value, a.exposed))
            } else {
                None
            };
            if pending_holdout {
                if let Err(payload) = self.trigger_holdout_exposures_deferred(&exp_name) {
                    deferred_panic.get_or_insert(payload);
                }
            }
            if let Some((value, exposed)) = winner {
                if let Some(payload) = deferred_panic {
                    panic::resume_unwind(payload);
                }
                if !exposed {
                    if let Some(entry) = self.assignments.get_mut(&exp_name) {
                        entry.assignment.exposed = true;
                    }
                    self.trigger_exposures(&exp_name);
                }
                return value;
            }
            if suppressed && suppressed_fallback.is_none() {
                suppressed_fallback = Some(exp_name);
            }
        }
        if let Some(payload) = deferred_panic {
            panic::resume_unwind(payload);
        }
        self.control_value(suppressed_fallback?, key)
    }

    /// The held-out candidate's control value for `key`, cloned only once
    /// nothing else has won.
    fn control_value(&self, experiment_name: String, key: &str) -> Option<Value> {
        self.assignments[&experiment_name]
            .assignment
            .variables
            .as_ref()?
            .get(key)
            .cloned()
    }

    /// Whether a holdout pinned in `cached` still has an exposure to queue,
    /// so the common candidate skips the panic guard and the clock read.
    fn has_unexposed_holdout(cached: &CachedAssignment) -> bool {
        cached.holdouts.iter().any(|holdout| {
            holdout
                .assignment
                .as_ref()
                .is_some_and(|h| !h.exposed.load(Ordering::Relaxed))
        })
    }

    #[cold]
    #[inline(never)]
    fn trigger_holdout_exposures_deferred(
        &mut self,
        experiment_name: &str,
    ) -> Result<(), Box<dyn std::any::Any + Send>> {
        #[cfg(test)]
        HOLDOUT_TRIGGERS.with(|n| n.set(n.get() + 1));
        panic::catch_unwind(AssertUnwindSafe(|| {
            self.queue_exposures(experiment_name, false)
        }))
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

    fn custom_field_value_unchecked(
        &self,
        experiment_name: &str,
        field_name: &str,
    ) -> Option<Value> {
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
    /// valid. Returns `()` like 0.1's `refresh(data)`; if the context is not ready or is
    /// finalizing/finalized the data is left unchanged and an `error` event is
    /// emitted. Use [`Context::try_refresh_with`] to get the error back.
    pub fn refresh_with(&mut self, new_data: ContextData) {
        if let Err(e) = self.try_refresh_with(new_data) {
            self.log_error(&e);
        }
    }

    /// Like [`Context::refresh_with`] but returns the guard error. Same
    /// guards as [`Context::refresh`]: `Err` before ready and once finalizing
    /// or finalized, and a no-op `Ok` on a failed context.
    pub fn try_refresh_with(&mut self, new_data: ContextData) -> Result<(), String> {
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
                .filter_map(|(name, cached)| {
                    let assignment = &cached.assignment;
                    if assignment.overridden {
                        return None;
                    }
                    let should_remove = match index.get(name) {
                        None => assignment.assigned || cached.rule.rule_override,
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
        // Holdout coverage can only change here, so it is compared once per
        // refresh and the result is checked on the next read instead of the
        // pinned holdouts being compared on every read.
        for (name, cached) in self.assignments.iter_mut() {
            if let Some(exp) = self.index.get(name) {
                if !(cached.holdouts.is_empty() && exp.holdouts.is_empty()) {
                    cached.coverage_stale.store(true, Ordering::Relaxed);
                }
            }
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

    /// Make the cached assignment for `experiment_name` current, rebuilding
    /// it when stale. Afterwards `self.assignments` always has the entry;
    /// callers read it there, so a cache hit copies nothing.
    fn assign(&mut self, experiment_name: &str) -> &CachedAssignment {
        self.refresh_assignment(experiment_name);
        &self.assignments[experiment_name]
    }

    fn refresh_assignment(&mut self, experiment_name: &str) {
        let has_custom = self.cassignments.contains_key(experiment_name);
        let has_override = self.overrides.contains_key(experiment_name);
        let has_experiment = self.index.contains_key(experiment_name);

        // A refresh only flags a coverage change; it is compared here, on
        // the next read, so a change a later refresh undoes is no change.
        if let Some(cached) = self.assignments.get(experiment_name).filter(|cached| {
            !cached.coverage_stale.load(Ordering::Relaxed)
                || self.coverage_still_matches(experiment_name, cached)
        }) {
            let a = &cached.assignment;
            if has_override {
                if a.overridden
                    && a.variant == self.overrides[experiment_name]
                    && (!has_experiment
                        || Self::experiment_matches(&self.index[experiment_name].data, a))
                {
                    return;
                }
            } else if !has_experiment {
                // A rule-won assignment is not "assigned" either, but its
                // variant came from the experiment's rules, so it is rebuilt
                // once the experiment is gone.
                if !a.assigned && !cached.rule.rule_override {
                    return;
                }
            } else if cached.rule.rule_override
                || !has_custom
                || self.cassignments[experiment_name] == a.variant
                // A suppressed assignment is control whatever the custom
                // assignment says, so a custom mismatch is not staleness.
                || cached.suppressed
            {
                let exp = &self.index[experiment_name];
                if Self::experiment_matches(&exp.data, a)
                    && self.audience_matches(&exp.data, cached)
                {
                    let (rules_rev, variant_count) = (exp.rules_rev, exp.data.variants.len());
                    if cached.rule.rules_rev == rules_rev
                        && cached.rule.variant_count == variant_count
                        && self.attrs_seq <= a.attrs_seq
                    {
                        return;
                    }
                    if self.rule_still_resolves(experiment_name) {
                        let attrs_seq = self.attrs_seq;
                        let entry = self.assignments.get_mut(experiment_name).unwrap();
                        entry.rule.rules_rev = rules_rev;
                        entry.rule.variant_count = variant_count;
                        entry.assignment.attrs_seq = attrs_seq;
                        return;
                    }
                }
            }
        }

        self.rebuild_assignment(experiment_name, has_custom, has_override);
    }

    /// The slow path of [`Self::refresh_assignment`], kept out of line so
    /// the cache-hit check inlines into the read paths.
    #[cold]
    #[inline(never)]
    fn rebuild_assignment(&mut self, experiment_name: &str, has_custom: bool, has_override: bool) {
        let exp_data_opt = self.index.get(experiment_name).map(|e| e.data.clone());

        let mut assignment = Assignment {
            eligible: true,
            ..Default::default()
        };
        let mut rule_state = RuleState::default();

        let holdouts = self.resolve_holdouts(experiment_name);
        let suppressed = exp_data_opt.as_ref().is_some_and(|exp| {
            holdouts.iter().any(|h| {
                h.assignment
                    .as_ref()
                    .is_some_and(|a| is_held_out_by(a.variant, a.arm_count, exp.full_on_variant))
            })
        });

        if has_override {
            if let Some(ref exp_data) = exp_data_opt {
                assignment.id = exp_data.id;
                assignment.unit_type = exp_data.unit_type.clone();
            }

            assignment.overridden = true;
            assignment.variant = self.overrides[experiment_name];
        } else if let Some(ref exp_data) = exp_data_opt {
            // A matching rule is an explicit, author-specified assignment, like
            // an override: it wins over holdout suppression and the audience,
            // traffic and custom assignments. The rules are recorded for every
            // assignment so a refresh that changes them can be detected.
            rule_state.rules_rev = self.index[experiment_name].rules_rev;
            rule_state.variant_count = exp_data.variants.len();
            let rule = self.resolve_rule(experiment_name, false);

            if rule.is_none() && !suppressed && !exp_data.audience.is_empty() {
                let attrs = self.evaluation_attributes();
                let result = self.audience_matcher.evaluate(&exp_data.audience, &attrs);
                if let Some(matched) = result {
                    assignment.audience_mismatch = !matched;
                }
            }

            if let Some((variant, missing_unit)) = rule {
                rule_state.rule_override = true;
                rule_state.missing_unit = missing_unit;
                assignment.variant = variant;
            } else if suppressed || (exp_data.audience_strict && assignment.audience_mismatch) {
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

        // An already-exposed rule win with the same variant is kept, with only
        // its rule bookkeeping refreshed: a new assignment would publish the
        // same decision a second time.
        if let Some(previous) = self.assignments.get_mut(experiment_name) {
            let same_run = exp_data_opt
                .as_ref()
                .is_some_and(|exp| Self::experiment_matches(exp, &previous.assignment));
            // The read that got here settled the coverage flag.
            if same_run
                && !previous.coverage_stale.load(Ordering::Relaxed)
                && previous.assignment.exposed
                && previous.rule.rule_override
                && rule_state.rule_override
                && !previous.assignment.overridden
                && previous.assignment.variant == assignment.variant
            {
                previous.rule = rule_state;
                previous.assignment.attrs_seq = assignment.attrs_seq;
                return;
            }
        }

        self.assignments.insert(
            experiment_name.to_string(),
            CachedAssignment {
                assignment,
                rule: rule_state,
                suppressed,
                holdouts,
                coverage_stale: AtomicBool::new(false),
            },
        );
    }

    /// Pin the unit's arm in each holdout covering `experiment_name`. The
    /// holdouts are drawn with the covered experiment's unit type; an entry
    /// is `None` while that unit is missing.
    fn resolve_holdouts(&mut self, experiment_name: &str) -> Vec<PinnedHoldout> {
        let Some(exp) = self.index.get(experiment_name) else {
            return Vec::new();
        };
        if exp.holdouts.is_empty() {
            return Vec::new();
        }
        let holdouts = exp.holdouts.clone();
        let unit_type = exp.data.unit_type.clone();
        holdouts
            .into_iter()
            .map(|data| {
                let assignment = unit_type
                    .as_deref()
                    .and_then(|unit_type| self.holdout_assignment(&data, unit_type));
                PinnedHoldout { data, assignment }
            })
            .collect()
    }

    /// The unit's arm in `live` (the holdout as in the current data) for
    /// `unit_type`, drawn once per holdout iteration: a same-iteration seed or
    /// split edit keeps the arm, so an exposed unit never lands in a second
    /// arm.
    fn holdout_assignment(
        &mut self,
        live: &ExperimentData,
        unit_type: &str,
    ) -> Option<Arc<HoldoutAssignment>> {
        let key = (live.id, unit_type.to_string());
        if let Some(cached) = self.holdout_assignments.get(&key) {
            if cached.iteration == live.iteration {
                return Some(cached.clone());
            }
        }

        let hash = self.unit_hash(unit_type)?;
        let assigner = self
            .assigners
            .entry(unit_type.to_string())
            .or_insert_with(|| VariantAssigner::new(&hash));
        let assignment = Arc::new(HoldoutAssignment {
            id: live.id,
            iteration: live.iteration,
            variant: assigner.assign(&live.split, live.seed_hi, live.seed_lo) as i32,
            arm_count: live.split.len(),
            unit_type: unit_type.to_string(),
            exposed: AtomicBool::new(false),
        });
        self.holdout_assignments.insert(key, assignment.clone());
        Some(assignment)
    }

    /// Settle a refresh's coverage flag: `true` (and the flag cleared) when
    /// the experiment is still covered by the same holdouts.
    #[cold]
    fn coverage_still_matches(&self, experiment_name: &str, cached: &CachedAssignment) -> bool {
        #[cfg(test)]
        COVERAGE_COMPARISONS.with(|n| n.set(n.get() + 1));
        let matches = match self.index.get(experiment_name) {
            Some(exp) => Self::holdout_set_matches(&exp.holdouts, &cached.holdouts),
            None => true,
        };
        cached.coverage_stale.store(!matches, Ordering::Relaxed);
        matches
    }

    /// Holdout coverage is compared by (id, iteration) only: a seed or split
    /// edit must not re-resolve, and so re-expose, the experiment.
    fn holdout_set_matches(fresh: &[Arc<ExperimentData>], pinned: &[PinnedHoldout]) -> bool {
        fresh.len() == pinned.len()
            && fresh
                .iter()
                .zip(pinned)
                .all(|(f, p)| f.id == p.data.id && f.iteration == p.data.iteration)
    }

    /// Queue the experiment's exposure (unless it is held out) and the
    /// exposures of the holdouts pinned in its assignment that this unit
    /// has not seen yet. As in the JS SDK, each exposure is queued before
    /// the event logger sees it.
    fn trigger_exposures(&mut self, experiment_name: &str) {
        self.queue_exposures(experiment_name, true);
    }

    fn queue_exposures(&mut self, experiment_name: &str, own: bool) {
        let Some(cached) = self.assignments.get(experiment_name) else {
            return;
        };
        let now = now_millis();
        let first = self.exposures.len();
        if own && (!cached.suppressed || cached.assignment.overridden || cached.rule.rule_override)
        {
            let a = &cached.assignment;
            self.exposures.push(Exposure {
                id: a.id,
                name: experiment_name.to_string(),
                exposed_at: now,
                unit: a.unit_type.clone(),
                variant: a.variant,
                assigned: a.assigned,
                eligible: a.eligible,
                overridden: a.overridden,
                full_on: a.full_on,
                custom: a.custom,
                audience_mismatch: a.audience_mismatch,
                rule_override: cached.rule.rule_override,
            });
        }
        for holdout in &cached.holdouts {
            let Some(h) = &holdout.assignment else {
                continue;
            };
            if !h.exposed.swap(true, Ordering::Relaxed) {
                self.exposures.push(Exposure {
                    id: h.id,
                    name: holdout.data.name.clone(),
                    exposed_at: now,
                    unit: Some(h.unit_type.clone()),
                    variant: h.variant,
                    assigned: true,
                    eligible: true,
                    overridden: false,
                    full_on: false,
                    custom: false,
                    audience_mismatch: false,
                    rule_override: false,
                });
            }
        }

        self.pending += self.exposures.len() - first;
        for i in first..self.exposures.len() {
            match serde_json::to_value(&self.exposures[i]) {
                Ok(value) => self.log_event("exposure", Some(value)),
                Err(e) => {
                    error!(
                        "Failed to serialize exposure for experiment '{}': {}",
                        self.exposures[i].name, e
                    );
                }
            }
        }
    }

    /// Resolve the experiment's assignment rules to `(variant, missing_unit)`,
    /// or `None` when no rule applies. A fixed variant out of range, or a split
    /// whose length differs from the variant count, is ignored. A split with
    /// no unit to randomize over (or with `unit_pinned_missing`) gives the
    /// full-on variant and is flagged `missing_unit`.
    fn resolve_rule(
        &mut self,
        experiment_name: &str,
        unit_pinned_missing: bool,
    ) -> Option<(i32, bool)> {
        let exp = self.index.get(experiment_name)?;
        let rules = exp.rules.clone()?;
        let variant_count = exp.data.variants.len();
        let unit_type = exp.data.unit_type.clone();
        let (full_on_variant, seed_hi, seed_lo) =
            (exp.data.full_on_variant, exp.data.seed_hi, exp.data.seed_lo);

        let attrs = self.evaluation_attributes();
        let action =
            self.audience_matcher
                .evaluate_rules(&rules, self.system_environment(), &attrs)?;

        match action {
            RuleAction::Assign(variant) => {
                let in_bounds = usize::try_from(variant).is_ok_and(|v| v < variant_count);
                in_bounds.then_some((variant as i32, false))
            }
            RuleAction::Split(split) => {
                if split.len() != variant_count {
                    return None;
                }
                let hash = match unit_type {
                    Some(ref unit_type) if !unit_pinned_missing => self.unit_hash(unit_type),
                    _ => None,
                };
                let (Some(unit_type), Some(hash)) = (unit_type, hash) else {
                    return Some((full_on_variant as i32, true));
                };
                let assigner = self
                    .assigners
                    .entry(unit_type)
                    .or_insert_with(|| VariantAssigner::new(&hash));
                Some((assigner.assign(&split, seed_hi, seed_lo) as i32, false))
            }
        }
    }

    /// Whether re-resolving the experiment's rules still gives the cached
    /// assignment's rule outcome. Called only when the rules, the variant
    /// count or the attributes changed since the assignment was built.
    fn rule_still_resolves(&mut self, experiment_name: &str) -> bool {
        let cached = &self.assignments[experiment_name];
        // An exposed split resolved without its unit keeps resolving as if
        // the unit were still missing, so a unit set afterwards cannot
        // switch the variant the user already saw.
        let pinned = cached.rule.missing_unit && cached.assignment.exposed;
        let expected = cached
            .rule
            .rule_override
            .then_some((cached.assignment.variant, cached.rule.missing_unit));
        self.resolve_rule(experiment_name, pinned) == expected
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

    fn audience_matches(&self, experiment: &ExperimentData, cached: &CachedAssignment) -> bool {
        // A held-out unit never had its audience evaluated.
        if cached.rule.rule_override || cached.suppressed {
            return true;
        }
        let assignment = &cached.assignment;
        if !experiment.audience.is_empty() && self.attrs_seq > assignment.attrs_seq {
            let attrs = self.evaluation_attributes();
            let result = self.audience_matcher.evaluate(&experiment.audience, &attrs);
            if let Some(matched) = result {
                return matched != assignment.audience_mismatch;
            }
        }
        true
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

#[cfg(test)]
thread_local! {
    static COVERAGE_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HOLDOUT_TRIGGERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WARMED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
#[path = "context_holdout_tests.rs"]
mod holdout_tests;

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
            assignment_rules: String::new(),
            holdout_ids: vec![],
            holdout_type: None,
        }
    }

    fn make_context_data(experiments: Vec<ExperimentData>) -> ContextData {
        ContextData {
            experiments,
            holdouts: vec![],
        }
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
        assert_eq!(
            context.get_unit("session_id"),
            Some(&serde_json::json!("user123"))
        );
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

        let value = context
            .variable_value("nonexistent", json!("default"))
            .unwrap();
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn test_context_peek_variable_value_returns_default_for_nonexistent() {
        let data = make_context_data(vec![]);
        let mut context = Context::new(data);

        let value = context
            .peek_variable_value("nonexistent", json!(42))
            .unwrap();
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

        let value = context
            .variable_value("nonexistent", "default_value")
            .unwrap();
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
            assignment_rules: String::new(),
            holdout_ids: vec![],
            holdout_type: None,
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
    fn test_refresh_with_returns_unit_and_logs_error_when_finalized() {
        let mut ctx = Context::new(make_context_data(vec![]));
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        ctx.set_event_logger(Box::new(move |_, event, _| {
            sink.lock().unwrap().push(event.to_string());
        }));
        ctx.finalize().unwrap();
        let unit: () = ctx.refresh_with(make_context_data(vec![variable_experiment()]));
        assert_eq!(unit, ());
        assert!(ctx.experiments().unwrap().is_empty());
        assert!(log.lock().unwrap().iter().any(|e| e == "error"));
    }

    #[test]
    fn test_refresh_after_finalize_is_rejected() {
        let mut ctx = Context::new(make_context_data(vec![]));
        ctx.finalize().unwrap();
        assert_eq!(
            ctx.try_refresh_with(make_context_data(vec![])),
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
            (
                "variable_value",
                ctx.variable_value("color", "x").map(|_| ()),
            ),
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
                ctx.try_refresh_with(make_context_data(vec![variable_experiment()])),
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
            ctx.try_refresh_with(make_context_data(vec![variable_experiment()])),
            Ok(())
        );

        assert_eq!(
            *fetches.lock().unwrap(),
            0,
            "a failed context does not fetch"
        );
        assert!(ctx.is_failed());
        assert_eq!(
            ctx.experiments(),
            Ok(vec![]),
            "a failed context is not recovered"
        );
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
            Some(
                json!({"message": "Discarding 0 exposures and 1 goals because context failed to initialize"})
            )
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
        assert_eq!(
            ctx.get_unit("session_id"),
            Some(&serde_json::json!("user123"))
        );
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
        let float = params
            .units
            .iter()
            .find(|u| u.unit_type == "float_id")
            .unwrap();
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
            vec![0.0, 1.0],
        );
        let data = make_context_data(vec![exp]);
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "test_user").unwrap();

        assert_eq!(ctx.treatment("test_exp").unwrap(), 1);

        let mut exp2 = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp2.traffic_split = vec![1.0, 0.0];
        let data2 = make_context_data(vec![exp2]);
        ctx.set_data_fetcher(Box::new(move || Ok(data2.clone())));
        ctx.refresh().unwrap();

        // The unit is now out of traffic, so the cached assignment must be dropped.
        assert_eq!(ctx.treatment("test_exp").unwrap(), 0);
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

        assert_eq!(
            ctx.custom_field_value("test_exp", "nonexistent").unwrap(),
            None
        );
    }

    #[test]
    fn test_custom_field_value_null_for_nonexistent_experiment() {
        let data = make_context_data(vec![]);
        let ctx = Context::new(data);

        assert_eq!(
            ctx.custom_field_value("nonexistent_exp", "field").unwrap(),
            None
        );
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
        assert_eq!(
            ctx.custom_field_value_type("test_exp", "nonexistent")
                .unwrap(),
            None
        );
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
        assert_eq!(
            units.get("device_id"),
            Some(&serde_json::json!("device456"))
        );
    }

    #[test]
    fn test_set_units_multiple() {
        let data = make_context_data(vec![]);
        let mut ctx = Context::new(data);

        ctx.set_units([("session_id", "user123"), ("device_id", "device456")])
            .unwrap();

        assert_eq!(
            ctx.get_unit("session_id"),
            Some(&serde_json::json!("user123"))
        );
        assert_eq!(
            ctx.get_unit("device_id"),
            Some(&serde_json::json!("device456"))
        );
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

    /// A strict audience experiment with a 0/100 split to variant 1, so a
    /// matching audience yields variant 1 and a mismatch yields variant 0.
    fn make_audience_experiment(audience: &str) -> ExperimentData {
        let mut exp = make_experiment(
            "test_exp",
            vec!["{}", r#"{"button":"red"}"#],
            vec![0.0, 1.0],
        );
        exp.audience = audience.to_string();
        exp.audience_strict = true;
        exp
    }

    fn context_with_system_attrs(
        exp: ExperimentData,
        application: Option<&str>,
        environment: Option<&str>,
    ) -> Context {
        let mut context = Context::new(make_context_data(vec![exp]));
        context.set_system_attributes(application, environment);
        context.set_unit("session_id", "test_user").unwrap();
        context
    }

    const PRODUCTION_FILTER: &str =
        r#"{"filter":[{"eq":[{"var":"environment"},{"value":"production"}]}]}"#;

    /// A strict-audience context for the "website" app in "production".
    fn production_context(audience: &str) -> Context {
        context_with_system_attrs(
            make_audience_experiment(audience),
            Some("website"),
            Some("production"),
        )
    }

    fn audience_mismatch(context: &Context, name: &str) -> bool {
        context
            .assignments
            .get(name)
            .map(|a| a.assignment.audience_mismatch)
            .unwrap_or(false)
    }

    #[test]
    fn test_system_attributes_in_strict_audience() {
        for (audience, variant) in [
            (PRODUCTION_FILTER, 1),
            (
                r#"{"filter":[{"eq":[{"var":"application"},{"value":"website"}]}]}"#,
                1,
            ),
            (
                r#"{"filter":[{"eq":[{"var":"environment"},{"value":"staging"}]}]}"#,
                0,
            ),
            (
                r#"{"filter":[{"eq":[{"var":"app_version"},{"value":"0"}]}]}"#,
                0,
            ),
            (r#"{"filter":[{"null":{"var":"app_version"}}]}"#, 1),
            (r#"{"filter":[{"null":{"var":"environment"}}]}"#, 0),
        ] {
            let mut context = production_context(audience);
            assert_eq!(
                context.treatment("test_exp").unwrap(),
                variant,
                "{}",
                audience
            );
            assert_eq!(audience_mismatch(&context, "test_exp"), variant == 0);
        }
    }

    #[test]
    fn test_user_attribute_overrides_system_environment() {
        let staging_exp = make_audience_experiment(
            r#"{"filter":[{"eq":[{"var":"environment"},{"value":"staging"}]}]}"#,
        );
        let mut context =
            context_with_system_attrs(staging_exp, Some("website"), Some("production"));

        assert_eq!(context.peek("test_exp").unwrap(), 0);

        context.set_attribute("environment", "staging").unwrap();
        assert_eq!(context.treatment("test_exp").unwrap(), 1);
        assert!(!audience_mismatch(&context, "test_exp"));

        let mut context2 = production_context(PRODUCTION_FILTER);
        context2.set_attribute("environment", "staging").unwrap();
        assert_eq!(context2.treatment("test_exp").unwrap(), 0);
        assert!(audience_mismatch(&context2, "test_exp"));
    }

    #[test]
    fn test_missing_environment_config_leaves_key_absent() {
        let exp = make_audience_experiment(PRODUCTION_FILTER);
        let mut context = context_with_system_attrs(exp, Some(""), None);

        assert_eq!(context.treatment("test_exp").unwrap(), 0);
        assert!(audience_mismatch(&context, "test_exp"));

        for (application, environment) in [(Some(""), Some("")), (None, None)] {
            for name in ["application", "environment", "app_version"] {
                let audience = format!(r#"{{"filter":[{{"null":{{"var":"{}"}}}}]}}"#, name);
                let mut context = context_with_system_attrs(
                    make_audience_experiment(&audience),
                    application,
                    environment,
                );
                assert_eq!(context.treatment("test_exp").unwrap(), 1, "{}", name);
            }
        }
    }

    #[test]
    fn test_system_attributes_not_added_to_publish_payload() {
        let mut context = production_context(PRODUCTION_FILTER);
        context.treatment("test_exp").unwrap();

        let params = context.get_publish_params();
        let json = serde_json::to_value(&params).unwrap();
        let attributes = json.get("attributes");
        assert!(
            attributes.is_none() || attributes == Some(&Value::Null),
            "expected no attributes in publish payload, got: {:?}",
            attributes
        );
    }

    #[test]
    fn test_user_attribute_still_published_but_system_not() {
        let mut context = production_context(PRODUCTION_FILTER);
        context.set_attribute("country", "US").unwrap();
        context.treatment("test_exp").unwrap();

        let params = context.get_publish_params();
        let json = serde_json::to_value(&params).unwrap();
        let attributes = json
            .get("attributes")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let names: Vec<&str> = attributes
            .iter()
            .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"country"));
        assert!(!names.contains(&"application"));
        assert!(!names.contains(&"environment"));
    }

    #[test]
    fn test_changing_system_attributes_invalidates_cached_assignment() {
        let mut context = production_context(PRODUCTION_FILTER);

        assert_eq!(context.peek("test_exp").unwrap(), 1);
        assert!(!audience_mismatch(&context, "test_exp"));

        context.set_system_attributes(Some("website"), Some("staging"));
        assert_eq!(context.peek("test_exp").unwrap(), 0);
        assert!(audience_mismatch(&context, "test_exp"));

        context.set_system_attributes(Some("website"), Some("production"));
        assert_eq!(context.peek("test_exp").unwrap(), 1);
        assert!(!audience_mismatch(&context, "test_exp"));
    }

    #[test]
    fn test_system_attributes_build_context_wires_config() {
        let sdk = crate::sdk::Client::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "website",
            "production",
        )
        .unwrap();
        let exp = make_audience_experiment(PRODUCTION_FILTER);
        let mut context = sdk.create_context_with(
            [("session_id", "test_user")],
            make_context_data(vec![exp]),
            None,
        );
        assert_eq!(context.treatment("test_exp").unwrap(), 1);
    }

    fn rules_experiment(rules: Value) -> ExperimentData {
        let mut exp = make_experiment(
            "rules_exp",
            vec![
                r#"{"color":"red"}"#,
                r#"{"color":"green"}"#,
                r#"{"color":"blue"}"#,
            ],
            vec![1.0, 0.0, 0.0],
        );
        exp.assignment_rules = rules.to_string();
        exp
    }

    fn rules_context(rules: Value) -> Context {
        let mut ctx = Context::new(make_context_data(vec![rules_experiment(rules)]));
        ctx.set_system_attributes(Some("website"), Some("production"));
        ctx
    }

    fn exposures(ctx: &mut Context) -> Vec<Exposure> {
        ctx.get_publish_params().exposures.unwrap_or_default()
    }

    #[test]
    fn test_assign_rule_wins_and_flags_exposure() {
        let mut ctx = rules_context(json!({"rules": [
            {"type": "assign", "variant": 2, "conditions": {"eq": [{"var": "tier"}, {"value": "gold"}]}}
        ]}));
        ctx.set_unit("session_id", "user").unwrap();

        assert_eq!(ctx.peek("rules_exp").unwrap(), 0);
        ctx.set_attribute("tier", "gold").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);
        assert_eq!(ctx.variable_value("color", "none").unwrap(), json!("blue"));

        let exposures = exposures(&mut ctx);
        assert_eq!(exposures.len(), 1);
        assert!(exposures[0].rule_override);
        assert!(!exposures[0].assigned);
        assert!(!exposures[0].overridden);
        let wire = serde_json::to_value(&exposures[0]).unwrap();
        assert_eq!(wire["ruleOverride"], json!(true));
    }

    #[test]
    fn test_assign_rule_without_unit_still_applies() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1}]}));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        assert_eq!(ctx.variable_value("color", "none").unwrap(), json!("green"));
    }

    #[test]
    fn test_rule_beats_audience_strict_and_custom_assignment() {
        let mut exp = rules_experiment(json!({"rules": [{"type": "assign", "variant": 1}]}));
        exp.audience = r#"{"filter":[{"value":false}]}"#.to_string();
        exp.audience_strict = true;
        let mut ctx = Context::new(make_context_data(vec![exp]));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_custom_assignment("rules_exp", 2).unwrap();

        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        let exposure = &exposures(&mut ctx)[0];
        assert!(exposure.rule_override);
        assert!(!exposure.audience_mismatch);
        assert!(!exposure.custom);
    }

    #[test]
    fn test_override_beats_rule() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1}]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_override("rules_exp", 2).unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);
        let exposure = &exposures(&mut ctx)[0];
        assert!(exposure.overridden);
        assert!(!exposure.rule_override);
    }

    #[test]
    fn test_out_of_range_or_mismatched_split_falls_back_without_trying_later_rules() {
        for first in [
            json!({"type": "assign", "variant": 3}),
            json!({"type": "assign", "variant": -1}),
            // Integral but beyond i32: still the first match, as in JS, which
            // then fails the bounds check instead of moving on.
            json!({"type": "assign", "variant": 2147483648_i64}),
            json!({"type": "assign", "variant": -2147483649_i64}),
            // Would wrap to 1 if narrowed to i32 before the bounds check.
            json!({"type": "assign", "variant": 4294967297_i64}),
            json!({"type": "assign", "variant": 1e300}),
            json!({"type": "split", "percentages": "50/50"}),
        ] {
            let mut ctx =
                rules_context(json!({"rules": [first, {"type": "assign", "variant": 2}]}));
            ctx.set_unit("session_id", "user").unwrap();
            assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
            assert!(!exposures(&mut ctx)[0].rule_override);
        }
    }

    #[test]
    fn test_rule_scope_ignores_user_environment_attribute() {
        // Scope uses the configured environment; a condition still sees the
        // user's value.
        let mut ctx = rules_context(
            json!({"rules": [{"type": "assign", "variant": 1, "environments": ["staging"]}]}),
        );
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("environment", "staging").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 0);

        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1,
            "conditions": {"eq": [{"var": "environment"}, {"value": "staging"}]}}]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("environment", "staging").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 1);
    }

    #[test]
    fn test_rule_wins_over_zero_traffic() {
        let mut exp = rules_experiment(json!({"rules": [{"type": "assign", "variant": 2}]}));
        exp.traffic_split = vec![1.0, 0.0];
        let mut ctx = Context::new(make_context_data(vec![exp]));
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(
            ctx.peek_variable_value("color", "none").unwrap(),
            json!("blue")
        );
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);
        assert!(exposures(&mut ctx)[0].rule_override);
    }

    #[test]
    fn test_exposed_rule_win_rebuilt_to_same_variant_is_not_exposed_again() {
        // The exposed split resolved without its unit (full-on variant 0); the
        // refreshed rule assigns 0 directly. The outcome changed but the
        // variant did not, so the decision must not be published again.
        let mut ctx =
            rules_context(json!({"rules": [{"type": "split", "percentages": "0/0/100"}]}));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);

        let mut data = ctx.data().unwrap().clone();
        data.experiments[0].assignment_rules =
            json!({"rules": [{"type": "assign", "variant": 0}]}).to_string();
        ctx.refresh_with(data);
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        assert_eq!(ctx.variable_value("color", "none").unwrap(), json!("red"));
        let exposures = exposures(&mut ctx);
        assert_eq!(exposures.len(), 1);
        assert!(exposures[0].rule_override);
    }

    #[test]
    fn test_same_outcome_refresh_advances_the_rules_revision() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 2}]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.treatment("rules_exp").unwrap();

        let mut data = ctx.data().unwrap().clone();
        data.experiments[0].assignment_rules =
            json!({"rules": [{"name": "renamed", "type": "assign", "variant": 2}]}).to_string();
        ctx.refresh_with(data);
        assert_eq!(ctx.peek("rules_exp").unwrap(), 2);
        let current = ctx.index["rules_exp"].rules_rev;
        assert_eq!(ctx.assignments["rules_exp"].rule.rules_rev, current);

        let data = ctx.data().unwrap().clone();
        ctx.refresh_with(data);
        assert_eq!(ctx.index["rules_exp"].rules_rev, current);
    }

    #[test]
    fn test_user_app_version_is_evaluated_and_published() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1,
            "conditions": {"semver_gte": [{"var": "app_version"}, {"value": "2.0.0"}]}}]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("app_version", "2.1.0").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        let attributes = ctx.get_publish_params().attributes.unwrap_or_default();
        let names: Vec<&str> = attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["app_version"]);
    }

    #[test]
    fn test_every_exposure_serializes_rule_override() {
        let mut ctx = Context::new(make_context_data(vec![make_experiment(
            "plain",
            vec!["", ""],
            vec![0.5, 0.5],
        )]));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.treatment("plain").unwrap();
        let wire = serde_json::to_value(&exposures(&mut ctx)[0]).unwrap();
        assert_eq!(wire["ruleOverride"], json!(false));
    }

    #[test]
    fn test_unknown_payload_fields_are_ignored_at_every_level() {
        let data: ContextData = serde_json::from_value(json!({
            "experiments": [
                {
                    "id": 1, "name": "a", "unitType": "session_id", "iteration": 1,
                    "split": [0.0, 1.0], "trafficSplit": [0.0, 1.0],
                    "variants": [{"config": null, "future": [1]}, {"config": "{}", "x": {"y": 1}}],
                    "customFieldValues": [{"name": "f", "value": "v", "type": "string", "extra": true}],
                    "assignmentRules": "{\"rules\":[{\"type\":\"future\"}],\"version\":2}",
                    "holdouts": [{"id": 9}], "switchback": {"period": 60}, "newScalar": 1
                },
                {
                    "id": 2, "name": "b", "unitType": "session_id", "iteration": 1,
                    "split": [0.0, 1.0], "trafficSplit": [0.0, 1.0],
                    "variants": [{}, {}], "assignmentRules": {"rules": []}, "tags": ["t"]
                }
            ],
            "application": {"name": "x"}, "futureRoot": [1, {"a": null}], "flag": true
        }))
        .unwrap();
        let mut ctx = Context::new(data);
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("a").unwrap(), 1);
        assert_eq!(ctx.treatment("b").unwrap(), 1);
    }

    #[test]
    fn test_rule_environments_use_client_environment() {
        let rules =
            json!({"rules": [{"type": "assign", "variant": 1, "environments": ["staging"]}]});
        let mut ctx = rules_context(rules.clone());
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 0);

        let mut ctx = Context::new(make_context_data(vec![rules_experiment(rules)]));
        ctx.set_system_attributes(Some("website"), Some("staging"));
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 1);
    }

    #[test]
    fn test_rule_conditions_see_system_attributes_and_semver() {
        let mut ctx = rules_context(json!({"rules": [{
            "type": "assign",
            "variant": 1,
            "conditions": {"and": [
                {"eq": [{"var": "application"}, {"value": "website"}]},
                {"semver_gte": [{"var": "version"}, {"value": "2.0.0"}]}
            ]}
        }]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("version", "1.9.9").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 0);
        ctx.set_attribute("version", "v2.0.0").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 1);
    }

    #[test]
    fn test_split_rule_randomizes_over_its_percentages() {
        let mut ctx =
            rules_context(json!({"rules": [{"type": "split", "percentages": "0/50/50"}]}));
        let mut seen = std::collections::HashSet::new();
        for i in 0..50 {
            let mut c = Context::new(ctx.data().unwrap().clone());
            c.set_unit("session_id", format!("user-{}", i)).unwrap();
            let variant = c.treatment("rules_exp").unwrap();
            assert_ne!(variant, 0);
            seen.insert(variant);
            assert!(exposures(&mut c)[0].rule_override);
        }
        assert_eq!(seen.len(), 2);

        ctx.set_unit("session_id", "user-7").unwrap();
        let hash = crate::utils::hash_unit("user-7");
        let expected = VariantAssigner::new(&hash).assign(&[0.0, 0.5, 0.5], 0, 0) as i32;
        assert_eq!(ctx.treatment("rules_exp").unwrap(), expected);
    }

    #[test]
    fn test_split_rule_matches_javascript_per_uid_with_experiment_seeds() {
        // From javascript-sdk's VariantAssigner for a 20/30/50 split: seeds
        // 123/456 give these; the traffic seeds 789/1011 would give
        // [2, 1, 2, 1, 2, 2] and 0/0 [2, 2, 2, 2, 1, 2].
        let mut exp =
            rules_experiment(json!({"rules": [{"type": "split", "percentages": "20/30/50"}]}));
        (exp.seed_hi, exp.seed_lo) = (123, 456);
        (exp.traffic_seed_hi, exp.traffic_seed_lo) = (789, 1011);
        let data = make_context_data(vec![exp]);
        let variants: Vec<i32> = ["alice", "bob", "charlie", "dave", "eve", "user-7"]
            .into_iter()
            .map(|uid| {
                let mut ctx = Context::new(data.clone());
                ctx.set_unit("session_id", uid).unwrap();
                ctx.treatment("rules_exp").unwrap()
            })
            .collect();
        assert_eq!(variants, [1, 2, 1, 1, 2, 2]);
    }

    #[test]
    fn test_split_rule_without_unit_uses_full_on_and_rerandomizes_when_unexposed() {
        let mut ctx =
            rules_context(json!({"rules": [{"type": "split", "percentages": "0/0/100"}]}));
        assert_eq!(ctx.peek("rules_exp").unwrap(), 0);
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.peek("rules_exp").unwrap(), 2);
    }

    #[test]
    fn test_exposed_split_rule_without_unit_stays_pinned() {
        let mut ctx =
            rules_context(json!({"rules": [{"type": "split", "percentages": "0/0/100"}]}));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("anything", 1).unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        let exposures = exposures(&mut ctx);
        assert_eq!(exposures.len(), 1);
        assert!(exposures[0].rule_override);
    }

    #[test]
    fn test_attribute_change_reevaluates_rule() {
        let mut ctx = rules_context(json!({"rules": [
            {"type": "assign", "variant": 1, "conditions": {"eq": [{"var": "beta"}, {"value": true}]}}
        ]}));
        ctx.set_unit("session_id", "user").unwrap();
        ctx.set_attribute("beta", true).unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        ctx.set_attribute("beta", false).unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        assert_eq!(exposures(&mut ctx).len(), 2);
    }

    #[test]
    fn test_unrelated_attribute_change_keeps_exposed_rule_assignment() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1}]}));
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        ctx.set_attribute("other", "x").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        assert_eq!(exposures(&mut ctx).len(), 1);
    }

    #[test]
    fn test_refresh_changing_rules_reassigns() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1}]}));
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);

        let refreshed = rules_experiment(json!({"rules": [{"type": "assign", "variant": 2}]}));
        ctx.refresh_with(make_context_data(vec![refreshed]));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);

        ctx.refresh_with(make_context_data(vec![rules_experiment(
            json!({"rules": []}),
        )]));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        let variants: Vec<i32> = exposures(&mut ctx).iter().map(|e| e.variant).collect();
        assert_eq!(variants, vec![1, 2, 0]);
    }

    fn two_variant_rules_experiment(rules: Value) -> ExperimentData {
        let mut exp = make_experiment(
            "rules_exp",
            vec![r#"{"color":"red"}"#, r#"{"color":"green"}"#],
            vec![1.0, 0.0],
        );
        exp.assignment_rules = rules.to_string();
        exp
    }

    #[test]
    fn test_refresh_removing_variant_drops_out_of_range_assign_rule() {
        let rules = json!({"rules": [{"type": "assign", "variant": 2}]});
        let mut ctx = rules_context(rules.clone());
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);

        ctx.refresh_with(make_context_data(vec![two_variant_rules_experiment(rules)]));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        let exposures = exposures(&mut ctx);
        assert_eq!(exposures.len(), 2);
        assert!(!exposures[1].rule_override);
        assert!(exposures[1].assigned);
    }

    #[test]
    fn test_refresh_removing_variant_drops_mismatched_split_rule() {
        let rules = json!({"rules": [{"type": "split", "percentages": "0/0/100"}]});
        let mut ctx = rules_context(rules.clone());
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 2);

        ctx.refresh_with(make_context_data(vec![two_variant_rules_experiment(rules)]));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        let exposures = exposures(&mut ctx);
        assert_eq!(exposures.len(), 2);
        assert!(!exposures[1].rule_override);
        assert!(exposures[1].assigned);
    }

    #[test]
    fn test_refresh_removing_experiment_drops_rule_assignment() {
        let mut ctx = rules_context(json!({"rules": [{"type": "assign", "variant": 1}]}));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 1);
        ctx.refresh_with(make_context_data(vec![]));
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
    }

    #[test]
    fn test_invalid_rules_json_is_ignored() {
        let mut exp = rules_experiment(json!({}));
        exp.assignment_rules = "{not json".to_string();
        let mut ctx = Context::new(make_context_data(vec![exp]));
        ctx.set_unit("session_id", "user").unwrap();
        assert_eq!(ctx.treatment("rules_exp").unwrap(), 0);
        assert!(!exposures(&mut ctx)[0].rule_override);
    }
}
