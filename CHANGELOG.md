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
  context. A three-arm holdout's arm 1 holds units out of non-full-on
  experiments only; the number of arms comes from the holdout's `split`.
  `set_override()` and a matching assignment rule win over a holdout.
  Payloads without holdouts assign and publish exactly as before.

### Changed

- Audiences and rule conditions always see the client's `application` and
  `environment`. Only non-empty values are used, and a user attribute with
  the same name overrides them. They are not added to the publish payload,
  and no `app_version` is injected. **This can change assignment** for
  strict-audience experiments that filter on `application` or `environment`:
  before 0.2.0 they fell back to variant 0 unless a user attribute of the
  same name matched the audience.
- The `X-Agent` header is now `absmartly-rust-sdk/<version>`. A configured
  `agent` is still sent as `User-Agent`.
- **BREAKING (source):** `ExperimentData` gained `assignment_rules: String` and
  `Exposure` gained `rule_override: bool`. JSON without them still
  deserializes (they default to `""` and `false`), but exhaustive struct
  literals must set them, for example by adding `assignment_rules: String::new()`
  and `rule_override: false`. `ContextData` gained `holdouts` and
  `ExperimentData` gained `holdout_ids` and `holdout_type` the same way
  (`holdouts: vec![]`, `holdout_ids: vec![]`, `holdout_type: None`).
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
