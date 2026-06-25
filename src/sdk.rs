use crate::context::Context;
use crate::context_publisher::DefaultContextPublisher;
use crate::models::{ContextData, ContextOptions, PublishParams};
use log::warn;
use reqwest::Client;

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

#[derive(Debug, Clone, Default)]
pub struct ABsmartlyBuilder {
    endpoint: Option<String>,
    api_key: Option<String>,
    application: Option<String>,
    environment: Option<String>,
    agent: Option<String>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
}

impl ABsmartlyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn application(mut self, application: impl Into<String>) -> Self {
        self.application = Some(application.into());
        self
    }

    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }

    pub fn build(self) -> Result<ABsmartly, SDKError> {
        let endpoint = self
            .endpoint
            .ok_or_else(|| SDKError::ConfigError("endpoint is required".to_string()))?;
        let api_key = self
            .api_key
            .ok_or_else(|| SDKError::ConfigError("api_key is required".to_string()))?;
        let application = self
            .application
            .ok_or_else(|| SDKError::ConfigError("application is required".to_string()))?;
        let environment = self
            .environment
            .ok_or_else(|| SDKError::ConfigError("environment is required".to_string()))?;

        if endpoint.is_empty() {
            return Err(SDKError::ConfigError(
                "endpoint cannot be empty".to_string(),
            ));
        }
        if api_key.is_empty() {
            return Err(SDKError::ConfigError("api_key cannot be empty".to_string()));
        }
        if application.is_empty() {
            return Err(SDKError::ConfigError(
                "application cannot be empty".to_string(),
            ));
        }
        if environment.is_empty() {
            return Err(SDKError::ConfigError(
                "environment cannot be empty".to_string(),
            ));
        }

        let config = SDKConfig {
            endpoint,
            api_key,
            application,
            environment,
            agent: self.agent,
            timeout_ms: Some(self.timeout_ms.unwrap_or(3000)),
            retries: Some(self.retries.unwrap_or(5)),
        };

        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(
                config.timeout_ms.unwrap_or(3000),
            ))
            .build()?;

        Ok(ABsmartly { config, client })
    }
}

pub struct ABsmartly {
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

fn backoff_ms(max_retries: u32, retries_remaining: u32) -> u64 {
    let attempt = (max_retries - retries_remaining).min(10);
    50u64.saturating_mul(2u64.saturating_pow(attempt))
}

impl ABsmartly {
    pub fn builder() -> ABsmartlyBuilder {
        ABsmartlyBuilder::new()
    }

    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        application: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, SDKError> {
        Self::builder()
            .endpoint(endpoint)
            .api_key(api_key)
            .application(application)
            .environment(environment)
            .build()
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

        let url = format!(
            "{}/context?application={}&environment={}",
            self.config.endpoint.trim_end_matches('/'),
            urlencoding::encode(&self.config.application),
            urlencoding::encode(&self.config.environment)
        );

        let mut retries = self.config.retries.unwrap_or(5);
        let mut last_error = None;

        while retries > 0 {
            let mut request = self
                .client
                .get(&url)
                .header("X-API-Key", &self.config.api_key);
            if let Some(ref agent) = self.config.agent {
                request = request.header("User-Agent", agent);
            }
            let response = request.send().await;

            match response {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let data: ContextData = resp.json().await?;
                        return Ok(self.create_context_with_internal(units_vec, data, options));
                    } else if resp.status().is_server_error() {
                        retries -= 1;
                        last_error =
                            Some(SDKError::HttpError(resp.error_for_status().unwrap_err()));
                        let max_retries = self.config.retries.unwrap_or(5);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(
                            max_retries,
                            retries,
                        )))
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
                        let max_retries = self.config.retries.unwrap_or(5);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(
                            max_retries,
                            retries,
                        )))
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
        options: Option<ContextOptions>,
    ) -> Context {
        let mut context = match options {
            Some(opts) => Context::new_with_options(data, opts),
            None => Context::new(data),
        };
        for (unit_type, uid) in units {
            if let Err(e) = context.set_unit(&unit_type, &uid) {
                warn!("Failed to set unit '{}': {}", unit_type, e);
            }
        }

        context.set_publisher(Box::new(DefaultContextPublisher::new(
            &self.config.endpoint,
            &self.config.api_key,
        )));

        let url = format!(
            "{}/context?application={}&environment={}",
            self.config.endpoint.trim_end_matches('/'),
            urlencoding::encode(&self.config.application),
            urlencoding::encode(&self.config.environment)
        );
        let api_key = self.config.api_key.clone();
        let agent = self.config.agent.clone();
        let client = self.client.clone();
        context.set_data_fetcher(Box::new(move || {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut request = client.get(&url).header("X-API-Key", &api_key);
                    if let Some(ref a) = agent {
                        request = request.header("User-Agent", a);
                    }
                    let resp = request
                        .send()
                        .await
                        .map_err(|e| format!("Refresh fetch failed: {}", e))?;
                    resp.json::<ContextData>()
                        .await
                        .map_err(|e| format!("Refresh parse failed: {}", e))
                })
            })
        }));

        context
    }

    pub async fn publish(&self, params: &PublishParams) -> Result<(), SDKError> {
        let url = format!("{}/context", self.config.endpoint.trim_end_matches('/'));

        let mut request = self
            .client
            .put(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("X-Application", &self.config.application)
            .header("X-Environment", &self.config.environment)
            .header("X-Application-Version", "0")
            .header("Content-Type", "application/json")
            .json(params);
        if let Some(ref agent) = self.config.agent {
            request = request.header("User-Agent", agent).header("X-Agent", agent);
        }
        let response = request.send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(SDKError::HttpError(
                response.error_for_status().unwrap_err(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_context_data() -> ContextData {
        ContextData {
            experiments: vec![],
        }
    }

    #[test]
    fn test_sdk_config_validation() {
        assert!(ABsmartly::builder()
            .api_key("key")
            .application("app")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .application("app")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("key")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("key")
            .application("app")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("key")
            .application("app")
            .environment("env")
            .build()
            .is_ok());
    }

    #[test]
    fn test_builder_pattern() {
        let sdk = ABsmartly::builder()
            .endpoint("https://test.absmartly.io/v1")
            .api_key("test-api-key")
            .application("test-app")
            .environment("development")
            .timeout(5000)
            .retries(3)
            .agent("my-agent")
            .build();

        assert!(sdk.is_ok());
        let sdk = sdk.unwrap();
        assert_eq!(sdk.config.endpoint, "https://test.absmartly.io/v1");
        assert_eq!(sdk.config.api_key, "test-api-key");
        assert_eq!(sdk.config.application, "test-app");
        assert_eq!(sdk.config.environment, "development");
        assert_eq!(sdk.config.timeout_ms, Some(5000));
        assert_eq!(sdk.config.retries, Some(3));
        assert_eq!(sdk.config.agent, Some("my-agent".to_string()));
    }

    #[test]
    fn test_create_context_with_array_of_tuples() {
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
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
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
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
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
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
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
        let data = make_context_data();

        let context = sdk.create_context_with([("session_id", "user123")], data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
    }

    #[test]
    fn test_sdk_config_new() {
        let config = SDKConfig::new("endpoint", "key", "app", "env");
        assert_eq!(config.endpoint, "endpoint");
        assert_eq!(config.api_key, "key");
        assert_eq!(config.application, "app");
        assert_eq!(config.environment, "env");
        assert_eq!(config.timeout_ms, Some(3000));
        assert_eq!(config.retries, Some(5));
        assert!(config.agent.is_none());
    }

    #[test]
    fn test_sdk_config_with_agent() {
        let config = SDKConfig::new("endpoint", "key", "app", "env").with_agent("test-agent");
        assert_eq!(config.agent, Some("test-agent".to_string()));
    }

    #[test]
    fn test_sdk_config_with_timeout() {
        let config = SDKConfig::new("endpoint", "key", "app", "env").with_timeout(5000);
        assert_eq!(config.timeout_ms, Some(5000));
    }

    #[test]
    fn test_sdk_config_with_retries() {
        let config = SDKConfig::new("endpoint", "key", "app", "env").with_retries(10);
        assert_eq!(config.retries, Some(10));
    }

    #[test]
    fn test_builder_default_timeout() {
        let sdk = ABsmartly::builder()
            .endpoint("https://test.absmartly.io/v1")
            .api_key("key")
            .application("app")
            .environment("env")
            .build()
            .unwrap();
        assert_eq!(sdk.config.timeout_ms, Some(3000));
    }

    #[test]
    fn test_builder_default_retries() {
        let sdk = ABsmartly::builder()
            .endpoint("https://test.absmartly.io/v1")
            .api_key("key")
            .application("app")
            .environment("env")
            .build()
            .unwrap();
        assert_eq!(sdk.config.retries, Some(5));
    }

    #[test]
    fn test_builder_static_method() {
        let sdk = ABsmartly::builder()
            .endpoint("https://test.absmartly.io/v1")
            .api_key("key")
            .application("app")
            .environment("env")
            .build();
        assert!(sdk.is_ok());
    }

    #[test]
    fn test_new_convenience_method() {
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        );
        assert!(sdk.is_ok());
    }

    #[test]
    fn test_create_context_with_context_is_ready() {
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
        let data = make_context_data();

        let context = sdk.create_context_with([("session_id", "user123")], data, None);
        assert!(context.is_ready());
    }

    #[test]
    fn test_create_context_with_empty_units() {
        let sdk = ABsmartly::new(
            "https://test.absmartly.io/v1",
            "test-api-key",
            "test-app",
            "development",
        )
        .unwrap();
        let data = make_context_data();

        let empty_units: Vec<(String, String)> = vec![];
        let context = sdk.create_context_with(empty_units, data, None);
        assert!(context.is_ready());
        assert!(context.get_units().is_empty());
    }

    #[test]
    fn test_sdk_error_display() {
        let err = SDKError::ConfigError("test error".to_string());
        assert_eq!(format!("{}", err), "Invalid configuration: test error");
    }

    #[test]
    fn test_backoff_ms_calculation() {
        assert_eq!(backoff_ms(5, 5), 50);
        assert_eq!(backoff_ms(5, 4), 100);
        assert_eq!(backoff_ms(5, 3), 200);
        assert_eq!(backoff_ms(5, 2), 400);
        assert_eq!(backoff_ms(5, 1), 800);
        assert_eq!(backoff_ms(5, 0), 1600);
    }

    #[test]
    fn test_backoff_ms_caps_at_10() {
        let at_10 = backoff_ms(15, 5);
        let at_11 = backoff_ms(15, 4);
        assert_eq!(at_10, at_11);
    }

    #[test]
    fn test_builder_empty_fields_rejected() {
        assert!(ABsmartly::builder()
            .endpoint("")
            .api_key("key")
            .application("app")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("")
            .application("app")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("key")
            .application("")
            .environment("env")
            .build()
            .is_err());

        assert!(ABsmartly::builder()
            .endpoint("endpoint")
            .api_key("key")
            .application("app")
            .environment("")
            .build()
            .is_err());
    }
}
