use crate::context::Context;
use crate::models::{ContextData, ContextOptions};
use std::collections::HashMap;

pub struct SDK {
    _options: SDKOptions,
}

#[derive(Default)]
pub struct SDKOptions {
    pub agent: Option<String>,
}

impl SDK {
    pub fn new(options: SDKOptions) -> Self {
        Self { _options: options }
    }

    pub fn create_context_with(
        &self,
        units: HashMap<String, String>,
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

impl Default for SDK {
    fn default() -> Self {
        Self::new(SDKOptions::default())
    }
}
