use crate::models::PublishParams;

pub trait ContextPublisher: Send + Sync {
    fn publish(&self, params: &PublishParams);
}

pub struct DefaultContextPublisher {
    pub(crate) endpoint: String,
    pub(crate) api_key: String,
}

impl DefaultContextPublisher {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
        }
    }
}

impl ContextPublisher for DefaultContextPublisher {
    fn publish(&self, params: &PublishParams) {
        let url = format!("{}/context", self.endpoint.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        let params = params.clone();

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let client = reqwest::Client::new();
                    let _ = client
                        .put(&url)
                        .header("X-API-Key", &api_key)
                        .header("Content-Type", "application/json")
                        .json(&params)
                        .send()
                        .await;
                });
            }
            Err(_) => {
                log::warn!("No tokio runtime available for publish");
            }
        }
    }
}
