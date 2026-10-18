use reqwest::Client;
use tokio::runtime::{Handle, RuntimeFlavor};

use crate::models::PublishParams;
use crate::sdk::{send_publish, SDKConfig};

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
/// On a multi-thread tokio runtime the request is awaited, so a failed delivery
/// is returned as `Err` and the events stay pending. A current-thread runtime
/// cannot be blocked without deadlocking, so there the request runs in the
/// background, `publish` returns `Ok(())` immediately, and a failure is only
/// logged. Outside a tokio runtime nothing can be sent, so `publish` returns
/// `Err` and the events stay pending.
pub struct DefaultContextPublisher {
    config: SDKConfig,
    client: Client,
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
        Self { config, client }
    }
}

impl ContextPublisher for DefaultContextPublisher {
    fn publish(&self, params: &PublishParams) -> Result<(), String> {
        let handle = Handle::try_current()
            .map_err(|_| "Publish failed: no tokio runtime available".to_string())?;

        if handle.runtime_flavor() == RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| {
                handle.block_on(send_publish(&self.client, &self.config, params))
            })
            .map_err(|e| format!("Publish failed: {}", e));
        }

        let client = self.client.clone();
        let config = self.config.clone();
        let params = params.clone();
        handle.spawn(async move {
            if let Err(e) = send_publish(&client, &config, &params).await {
                log::warn!("Background publish failed: {}", e);
            }
        });
        Ok(())
    }
}
