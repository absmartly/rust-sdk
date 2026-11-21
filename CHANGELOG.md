# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate follows
semantic versioning (under 0.x, a minor bump is a breaking release).

## [Unreleased] - 0.2.0

### Added

- Assignment rules: an experiment's `assignmentRules` are evaluated in order
  and the first matching rule decides the variant. `assign` gives a fixed
  variant, and `split` randomizes the unit over its own percentages. A
  matching rule wins over the audience, traffic and custom assignments.
  `set_override()` still wins over every rule.
- `semver_eq`, `semver_gt`, `semver_gte`, `semver_lt` and `semver_lte`
  operators for audiences and rule conditions, ordered as in javascript-sdk
  (UTF-16 order for pre-release identifiers; numbers converted like JS,
  including `1e+21`).
- `SDK_NAME`, `SDK_VERSION` and `SDK_AGENT` constants.
- Holdouts, as in javascript-sdk: `ContextData.holdouts` and
  `ExperimentData.holdout_ids` are read, and a unit that a covering holdout
  holds out gets variant 0 and control variables, with no exposure for the
  experiment. Every covering holdout queues its own exposure once per
  context and unit type. A three-arm holdout's arm 1 holds units out of non-full-on
  experiments only; the number of arms comes from the holdout's `split`.
  `set_override()` and a matching assignment rule win over a holdout.
  Holdout support itself does not change how payloads without holdouts
  assign or publish (exposures still gain `ruleOverride`; see Changed).

### Changed

Breaking changes are listed against the released 0.1.0.

- Audiences and rule conditions always see the client's `application` and
  `environment`. Only non-empty values are used, and a user attribute with
  the same name overrides them. They are not added to the publish payload,
  and no `app_version` is injected. **This can change assignment** for
  strict-audience experiments that filter on `application` or `environment`:
  before 0.2.0 they fell back to variant 0 unless a user attribute of the
  same name matched the audience.
- An exposure is queued before the event logger receives it, as in
  javascript-sdk, so `pending()` read inside an `exposure` callback already
  counts it, and a panicking logger no longer loses the exposure. When one
  call queues an experiment's exposure and its holdouts' exposures, all of
  them are queued before the first callback, so `pending()` in that
  callback already counts every one (javascript-sdk counts them one at a
  time). Payloads and order are the same.
- **This can change assignment** for payloads that already carry
  `holdouts` and `holdoutIds`: 0.1.0 ignored them, while 0.2.0 gives a
  held-out unit variant 0 and control variables and queues the holdout
  exposures. Payloads without holdouts are unaffected.
- A panicking event logger during `variable_value()` is resumed only after
  the remaining candidate experiments have queued their exposures.
- Every exposure, in the publish payload and the event logger's `exposure`
  event, now carries `ruleOverride` (`false` unless a rule decided the
  variant), so the backend can exclude rule-assigned exposures from analysis.
- The event logger also receives `ready` when a context becomes ready, and
  `error` with `{"message": ...}` when a context fails to load, `refresh()`
  has no fetcher or its fetch fails, `refresh_with` is called on a context
  that is not ready, finalizing or finalized, the publisher returns an error
  from `publish()`, `finalize()` or `close()`, or one of those calls on a
  failed context discards its pending events (it still returns `Ok`). A `publish()` or `refresh()` rejected by the readiness check
  returns `Err` without an event.
  `become_failed()` emits `error` with no data. 0.1.0 emitted neither event.
- The `X-Agent` header is now `absmartly-rust-sdk/<version>`. A configured
  `agent` is still sent as `User-Agent`.
- **BREAKING (source):** `ExperimentData` gained `assignment_rules: String` and
  `Exposure` gained `rule_override: bool`. JSON without them still
  deserializes (they default to `""` and `false`), but exhaustive struct
  literals must set them, for example by adding `assignment_rules: String::new()`
  and `rule_override: false`. `ContextData` gained `holdouts` and
  `ExperimentData` gained `holdout_ids` and `holdout_type` the same way
  (`holdouts: vec![]`, `holdout_ids: vec![]`, `holdout_type: None`);
  exhaustive destructuring patterns need `..`.
  `Assignment` is unchanged.
- **BREAKING:** the crate is imported as `absmartly` instead of
  `absmartly_sdk` (the package is still `absmartly-sdk`).
- **BREAKING:** `SDK` and `SDKOptions` were removed, with `SDK::new` and
  `SDK::create_context_with`. Build a `Client` with `Client::builder()`,
  `Client::new(endpoint, api_key, application, environment)` or
  `Client::from_config(Config)`, and create contexts with
  `Client::create_context_with` or the async `Client::create_context`.
  Client errors are `absmartly::Error`, which includes `HttpStatus(u16)`.
- **BREAKING:** `Context` methods that used to return a bare value now return
  `Result<_, String>`, so they can report a context that is not ready or is
  finalized: `peek`, `treatment`, `variable_value`, `peek_variable_value`,
  `variable_keys`, `custom_field_value`, `custom_field_value_type`,
  `custom_field_keys`, `experiments`, `data`, `publish`, `finalize` and
  `set_override`.
- **BREAKING:** unit values are `serde_json::Value`: `set_unit` takes
  `impl Into<Value>`, `get_unit` returns `Option<&Value>` and `get_units`
  returns `&HashMap<String, Value>`.
- **BREAKING:** `Context::refresh()` takes no argument and fetches. Use
  `refresh_with(data)` to apply data you already have. Unlike 0.1.0, a
  refresh keeps the assignments that are still valid.
- **BREAKING:** `ContextOptions` gained `event_logger`; build it with
  `..Default::default()`.
- **BREAKING:** the `in` operator takes the list first, `[haystack, needle]`,
  as in javascript-sdk. Audiences written needle-first must be updated.
- **BREAKING (behaviour):** audience and rule operators now follow
  javascript-sdk, which **can change assignment** for strict-audience
  experiments:
  - A comparison (`eq`, `gt`, `gte`, `lt`, `lte`, `match`, `in`) with a null
    operand is null (falsy). In 0.1.0, `eq(null, null)` was true, so an
    audience that tests for a missing attribute with `eq … null` no longer
    matches, and `not(eq … null)` now does.
  - An empty `or` is true (it was false).
  - `var` accepts the `{"path": ...}` form (it was always null).
  - `in` converts the needle: a string haystack matches a number
    (`in("a1b", 1)`), and array items match by comparison (`in([1], "1")`)
    instead of deep equality.
  - A binary operator given a non-array or fewer than two operands returns
    null instead of false, and one given more than two compares the first
    two instead of returning false.
- **BREAKING (behaviour):** `track` returns
  `Err("Goal '<name>' properties must be of type object.")` and records
  nothing when the properties are not an object or null, as in
  javascript-sdk. 0.1.0 recorded the goal without properties and returned
  `Ok`, so a call such as `track("purchase", 5)` now loses the goal, or
  panics if the result is unwrapped.
- Context errors spell the brand `ABsmartly` (for example
  `"ABsmartly Context is finalized."`). 0.1.0 used `ABSmartly`, so update
  code that matches on the error text.

## [0.1.0]

- Initial release.
