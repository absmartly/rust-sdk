//! SDK client for communicating with the ABsmartly API.

use crate::context::Context;
use crate::models::{ContextData, ContextOptions};
use reqwest::Client;
use serde::Serialize;

/// Configuration for connecting to the ABsmartly API.
#[derive(Debug, Clone)]
pub struct SDKConfig {
    /// The ABsmartly API endpoint URL.
    pub endpoint: String,
    /// API key for authentication.
    pub api_key: String,
    /// Application name.
    pub application: String,
    /// Environment name (e.g., "production", "development").
    pub environment: String,
    /// Optional agent identifier.
    pub agent: Option<String>,
    /// Request timeout in milliseconds (default: 3000).
    pub timeout_ms: Option<u64>,
    /// Number of retry attempts for failed requests (default: 5).
    pub retries: Option<u32>,
}

impl SDKConfig {
    /// Creates a new SDK configuration with the required fields.
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        application: impl Into<String>,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            application: application.into(),
            environment: environment.into(),
            agent: None,
            timeout_ms: Some(3000),
            retries: Some(5),
        }
    }

    /// Sets the agent identifier (builder pattern).
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Sets the request timeout in milliseconds (builder pattern).
    #[must_use]
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Sets the number of retry attempts (builder pattern).
    #[must_use]
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }
}

#[derive(Debug, Serialize)]
struct ContextRequest {
    units: Vec<UnitRequest>,
}

#[derive(Debug, Serialize)]
struct UnitRequest {
    #[serde(rename = "type")]
    unit_type: String,
    uid: String,
}

/// The main ABsmartly SDK client.
#[derive(Debug)]
pub struct SDK {
    config: SDKConfig,
    client: Client,
}

/// Errors that can occur when using the SDK.
#[derive(Debug, thiserror::Error)]
pub enum SDKError {
    /// An HTTP request to the ABsmartly API failed.
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),
    /// The SDK configuration is invalid.
    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}

impl SDK {
    /// Creates a new SDK client with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if any required configuration field is empty or if the
    /// HTTP client fails to build.
    pub fn new(config: SDKConfig) -> Result<Self, SDKError> {
        if config.endpoint.is_empty() {
            return Err(SDKError::ConfigError("endpoint is required".to_owned()));
        }
        if config.api_key.is_empty() {
            return Err(SDKError::ConfigError("api_key is required".to_owned()));
        }
        if config.application.is_empty() {
            return Err(SDKError::ConfigError("application is required".to_owned()));
        }
        if config.environment.is_empty() {
            return Err(SDKError::ConfigError("environment is required".to_owned()));
        }

        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(
                config.timeout_ms.unwrap_or(3000),
            ))
            .build()?;

        Ok(Self { config, client })
    }

    /// Creates a context by fetching experiment data from the ABsmartly API.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails after all retry attempts.
    pub async fn create_context<I, K, V>(
        &self,
        units: I,
        options: Option<ContextOptions>,
    ) -> Result<Context, SDKError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let units_vec: Vec<(String, String)> = units
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();

        let request = ContextRequest {
            units: units_vec
                .iter()
                .map(|(t, u)| UnitRequest {
                    unit_type: t.clone(),
                    uid: u.clone(),
                })
                .collect(),
        };

        let url = format!("{}/context", self.config.endpoint.trim_end_matches('/'));

        let max_retries = self.config.retries.unwrap_or(5);
        let mut retries = max_retries;
        let mut last_error = None;

        while retries > 0 {
            let response = self
                .client
                .put(&url)
                .header("X-API-Key", &self.config.api_key)
                .header("X-Application", &self.config.application)
                .header("X-Environment", &self.config.environment)
                .header("X-Application-Version", "0")
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let data: ContextData = resp.json().await?;
                        return Ok(Self::build_context(units_vec, data, options));
                    } else if resp.status().is_server_error() {
                        retries -= 1;
                        if let Err(e) = resp.error_for_status() {
                            last_error = Some(SDKError::HttpError(e));
                        }
                    } else if let Err(e) = resp.error_for_status() {
                        return Err(SDKError::HttpError(e));
                    }
                }
                Err(e) => {
                    retries -= 1;
                    last_error = Some(SDKError::HttpError(e));
                }
            }

            if retries > 0 {
                let backoff = 50 * 2_u64.pow(max_retries - retries);
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SDKError::ConfigError("Failed to create context after retries".to_owned())
        }))
    }

    /// Creates a context with pre-fetched experiment data.
    pub fn create_context_with<I, K, V>(
        &self,
        units: I,
        data: ContextData,
        options: Option<ContextOptions>,
    ) -> Context
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let units_vec: Vec<(String, String)> = units
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        Self::build_context(units_vec, data, options)
    }

    fn build_context(
        units: Vec<(String, String)>,
        data: ContextData,
        _options: Option<ContextOptions>,
    ) -> Context {
        let mut context = Context::new(data);
        for (unit_type, uid) in units {
            let _ignored = context.set_unit(&unit_type, &uid);
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_config() -> SDKConfig {
        SDKConfig::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
    }

    fn make_context_data() -> ContextData {
        ContextData {
            experiments: vec![],
        }
    }

    #[test]
    fn test_sdk_config_validation() {
        let config = SDKConfig::new("", "key", "app", "env");
        assert!(SDK::new(config).is_err());

        let config = SDKConfig::new("endpoint", "", "app", "env");
        assert!(SDK::new(config).is_err());

        let config = SDKConfig::new("endpoint", "key", "", "env");
        assert!(SDK::new(config).is_err());

        let config = SDKConfig::new("endpoint", "key", "app", "");
        assert!(SDK::new(config).is_err());

        let config = make_config();
        assert!(SDK::new(config).is_ok());
    }

    #[test]
    fn test_create_context_with_array_of_tuples() {
        let sdk = SDK::new(make_config()).unwrap();
        let data = make_context_data();

        let context = sdk.create_context_with(
            [("session_id", "user123"), ("device_id", "device456")],
            data,
            None,
        );

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
        assert_eq!(
            context.get_unit("device_id"),
            Some(&"device456".to_string())
        );
    }

    #[test]
    fn test_create_context_with_vec_of_tuples() {
        let sdk = SDK::new(make_config()).unwrap();
        let data = make_context_data();

        let units = vec![
            ("session_id".to_string(), "user123".to_string()),
            ("device_id".to_string(), "device456".to_string()),
        ];

        let context = sdk.create_context_with(units, data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
        assert_eq!(
            context.get_unit("device_id"),
            Some(&"device456".to_string())
        );
    }

    #[test]
    fn test_create_context_with_hashmap() {
        let sdk = SDK::new(make_config()).unwrap();
        let data = make_context_data();

        let mut units = HashMap::new();
        units.insert("session_id".to_string(), "user123".to_string());
        units.insert("device_id".to_string(), "device456".to_string());

        let context = sdk.create_context_with(units, data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
        assert_eq!(
            context.get_unit("device_id"),
            Some(&"device456".to_string())
        );
    }

    #[test]
    fn test_create_context_with_single_unit() {
        let sdk = SDK::new(make_config()).unwrap();
        let data = make_context_data();

        let context = sdk.create_context_with([("session_id", "user123")], data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
    }
}
