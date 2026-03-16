//! ABsmartly SDK for Rust - A/B testing and feature flagging.
//!
//! This crate provides a Rust client for the ABsmartly platform, enabling
//! A/B testing, feature flagging, and experimentation in Rust applications.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::str_to_string,
        clippy::needless_pass_by_value,
        clippy::unreadable_literal,
        unused_results,
    )
)]

pub mod assigner;
pub mod context;
pub mod jsonexpr;
pub mod matcher;
pub mod md5;
pub mod models;
pub mod murmur3;
pub mod sdk;
pub mod utils;

pub use context::Context;
pub use models::{
    Assignment, Attribute, ContextData, ContextOptions, ContextParams, ContextState,
    CustomFieldValue, ExperimentData, Exposure, Goal, PublishParams, Unit, Variant,
};
pub use sdk::{SDKConfig, SDKError, SDK};
