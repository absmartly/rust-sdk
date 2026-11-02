use reqwest::Client;

use crate::models::PublishParams;
use crate::sdk::{blocking_client, run_blocking, send_publish, SDKConfig};

/// Delivers a context's pending events to the ABsmartly collector.
pub trait ContextPublisher: Send + Sync {
    /// Publish `params`.
    ///
    /// Return `Err` when the events were not delivered. The context then keeps
    /// its exposures and goals pending, so a later `publish()` retries them.
    fn publish(&self, params: &PublishParams) -> Result<(), String>;
}

/// The publisher installed by `ABsmartly` on every context it creates. It sends
/// `PUT {endpoint}/context` with the same request as [`crate::ABsmartly::publish`].
///
/// The request runs to completion on a dedicated thread with its own runtime,
/// so this works on any tokio runtime flavor (or none) and never panics. A
/// failed delivery is returned as `Err` and the events stay pending.
pub struct DefaultContextPublisher {
    config: SDKConfig,
}

impl DefaultContextPublisher {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        let config = SDKConfig::new(endpoint, api_key, "", "");
        let timeout = std::time::Duration::from_millis(config.timeout_ms.unwrap_or(3000));
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self::from_config(config, client)
    }

    pub(crate) fn from_config(config: SDKConfig, client: Client) -> Self {
        let _ = client;
        Self { config }
    }
}

impl ContextPublisher for DefaultContextPublisher {
    fn publish(&self, params: &PublishParams) -> Result<(), String> {
        let config = self.config.clone();
        let params = params.clone();
        let timeout_ms = config.timeout_ms.unwrap_or(3000);
        run_blocking(async move {
            let client = blocking_client(timeout_ms)?;
            send_publish(&client, &config, &params)
                .await
                .map_err(|e| format!("Publish failed: {}", e))
        })?
    }
}
