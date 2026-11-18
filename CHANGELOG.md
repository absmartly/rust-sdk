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
- Exposures carry `ruleOverride` so the backend can exclude rule-assigned
  exposures from analysis.
- Holdouts, as in javascript-sdk: `ContextData.holdouts` and
  `ExperimentData.holdout_ids` are read, and a unit that a covering holdout
  holds out gets variant 0 and control variables, with no exposure for the
  experiment. Every covering holdout queues its own exposure once per
  context and unit type. A three-arm holdout's arm 1 holds units out of non-full-on
  experiments only; the number of arms comes from the holdout's `split`.
  `set_override()` and a matching assignment rule win over a holdout.
  Payloads without holdouts assign and publish exactly as before.
- Switchback experiments, as in java-sdk (`SWITCHBACK.md`): an experiment
  with `randomizationType: "switchback"` is served per cluster and period.
  The first unit, in `switchbackClusters` order, whose attribute path gives a
  value serves it, and that raw value (a non-empty string, or an integer as
  its decimal text; never a float such as `3.0`) is the cluster key. The
  variant is the platform's balanced-block draw for the period, pinned by its
  golden vectors. Ineligible clusters, the time before `switchbackStartAt` and
  unservable schedules give variant 0 with no exposure. Overrides, assignment
  rules, strict audiences, full-on, traffic and custom assignments keep their
  unit-experiment exposure flags; holdouts never apply to a switchback. Each
  period is exposed once, stamped with the time that chose it. Payloads
  without switchbacks assign and publish exactly as before.

### Changed

- **Switchback experiments are assigned differently.** Before 0.2.0 the SDK
  ignored the switchback fields and served such an experiment a sticky unit
  assignment on its `unitType`, which did not follow the schedule. It now
  serves the cluster's schedule (see *Added*), so units in a running
  switchback can change variant and are exposed once per period.
- Audiences and rule conditions always see the client's `application` and
  `environment`. Only non-empty values are used, and a user attribute with
  the same name overrides them. They are not added to the publish payload,
  and no `app_version` is injected. **This can change assignment** for
  strict-audience experiments that filter on `application` or `environment`:
  before 0.2.0 they fell back to variant 0 unless a user attribute of the
  same name matched the audience.
- An exposure is queued before the event logger receives it, as in
  javascript-sdk, so `pending()` read inside an `exposure` callback already
  counts it, and a panicking logger no longer loses the exposure.
- The `X-Agent` header is now `absmartly-rust-sdk/<version>`. A configured
  `agent` is still sent as `User-Agent`.
- **BREAKING (source):** `ExperimentData` gained `assignment_rules: String` and
  `Exposure` gained `rule_override: bool`. JSON without them still
  deserializes (they default to `""` and `false`), but exhaustive struct
  literals must set them, for example by adding `assignment_rules: String::new()`
  and `rule_override: false`. `ContextData` gained `holdouts` and
  `ExperimentData` gained `holdout_ids` and `holdout_type` the same way
  (`holdouts: vec![]`, `holdout_ids: vec![]`, `holdout_type: None`);
  exhaustive destructuring patterns need `..`. `ExperimentData` also gained
  the switchback fields (`randomization_type`, `switchback_clusters`,
  `switchback_cluster_values`, `switchback_start_at`, `switchback_period_ms`,
  `switchback_carryover_duration_ms`, `switchback_burn_in_duration_ms`,
  `switchback_burn_out_duration_ms`, `switchback_design_type`,
  `switchback_estimand`), all `Option`s that default to `None`, and now
  implements `Default`, so a literal can end with `..Default::default()`.
  `Assignment` is unchanged.
- **BREAKING:** the crate is imported as `absmartly`, and the client types
  were renamed. `ABsmartly` became `Client`, `ABsmartlyBuilder` became
  `ClientBuilder`, `SDKConfig` became `Config` and `SDKError` became `Error`,
  and `SDK` is replaced by `Client::from_config`. `ContextOptions` gained
  `event_logger`. `Context::refresh()` now fetches. Unit values are
  `serde_json::Value`. The `in` operator takes the list first. `Error` gained
  `HttpStatus(u16)`. See the README's Compatibility section.

## [0.1.0]

- Initial release.
