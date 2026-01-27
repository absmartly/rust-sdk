use crate::context::Context;
use crate::models::{ContextData, ContextOptions};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct SDKConfig {
    pub endpoint: String,
    pub api_key: String,
    pub application: String,
    pub environment: String,
    pub agent: Option<String>,
    pub timeout_ms: Option<u64>,
    pub retries: Option<u32>,
}

impl SDKConfig {
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

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

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

pub struct SDK {
    config: SDKConfig,
    client: Client,
}

#[derive(Debug, thiserror::Error)]
pub enum SDKError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}

impl SDK {
    pub fn new(config: SDKConfig) -> Result<Self, SDKError> {
        if config.endpoint.is_empty() {
            return Err(SDKError::ConfigError("endpoint is required".to_string()));
        }
        if config.api_key.is_empty() {
            return Err(SDKError::ConfigError("api_key is required".to_string()));
        }
        if config.application.is_empty() {
            return Err(SDKError::ConfigError("application is required".to_string()));
        }
        if config.environment.is_empty() {
            return Err(SDKError::ConfigError("environment is required".to_string()));
        }

        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(
                config.timeout_ms.unwrap_or(3000),
            ))
            .build()?;

        Ok(Self { config, client })
    }

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

        let mut retries = self.config.retries.unwrap_or(5);
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
                        return Ok(self.create_context_with_internal(units_vec, data, options));
                    } else if resp.status().is_server_error() {
                        retries -= 1;
                        last_error = Some(SDKError::HttpError(
                            resp.error_for_status().unwrap_err(),
                        ));
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * (2_u64.pow((self.config.retries.unwrap_or(5) - retries) as u32)),
                        ))
                        .await;
                        continue;
                    } else {
                        return Err(SDKError::HttpError(resp.error_for_status().unwrap_err()));
                    }
                }
                Err(e) => {
                    retries -= 1;
                    last_error = Some(SDKError::HttpError(e));
                    if retries > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * (2_u64.pow((self.config.retries.unwrap_or(5) - retries) as u32)),
                        ))
                        .await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SDKError::ConfigError("Failed to create context after retries".to_string())
        }))
    }

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
        self.create_context_with_internal(units_vec, data, options)
    }

    fn create_context_with_internal(
        &self,
        units: Vec<(String, String)>,
        data: ContextData,
        _options: Option<ContextOptions>,
    ) -> Context {
        let mut context = Context::new(data);
        for (unit_type, uid) in units {
            let _ = context.set_unit(&unit_type, &uid);
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
        ContextData { experiments: vec![] }
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
        assert_eq!(context.get_unit("device_id"), Some(&"device456".to_string()));
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
        assert_eq!(context.get_unit("device_id"), Some(&"device456".to_string()));
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
        assert_eq!(context.get_unit("device_id"), Some(&"device456".to_string()));
    }

    #[test]
    fn test_create_context_with_single_unit() {
        let sdk = SDK::new(make_config()).unwrap();
        let data = make_context_data();

        let context = sdk.create_context_with([("session_id", "user123")], data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
    }
}
