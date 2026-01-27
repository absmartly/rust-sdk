use crate::context::Context;
use crate::models::{ContextData, ContextOptions};

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

    pub fn create_context_with<I, K, V>(
        &self,
        units: I,
        data: ContextData,
        _options: Option<ContextOptions>,
    ) -> Context
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut context = Context::new(data);
        for (unit_type, uid) in units {
            let _ = context.set_unit(&unit_type.into(), &uid.into());
        }
        context
    }
}

impl Default for SDK {
    fn default() -> Self {
        Self::new(SDKOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_context_data() -> ContextData {
        ContextData { experiments: vec![] }
    }

    #[test]
    fn test_create_context_with_array_of_tuples() {
        let sdk = SDK::default();
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
        let sdk = SDK::default();
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
        let sdk = SDK::default();
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
        let sdk = SDK::default();
        let data = make_context_data();

        let context = sdk.create_context_with([("session_id", "user123")], data, None);

        assert_eq!(context.get_unit("session_id"), Some(&"user123".to_string()));
    }
}
