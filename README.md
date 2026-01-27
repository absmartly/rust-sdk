# ABsmartly SDK for Rust

[![Crates.io](https://img.shields.io/crates/v/absmartly-sdk.svg)](https://crates.io/crates/absmartly-sdk)
[![Documentation](https://docs.rs/absmartly-sdk/badge.svg)](https://docs.rs/absmartly-sdk)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A Rust SDK for [ABsmartly](https://www.absmartly.com) - A/B testing and feature flagging platform.

## Compatibility

The ABsmartly Rust SDK is compatible with Rust 2021 edition and later. It provides a synchronous interface for variant assignment and goal tracking.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
absmartly-sdk = "0.1"
```

## Getting Started

Please follow the [installation](#installation) instructions before trying the following code.

### Initialization

This example assumes an API Key, an Application, and an Environment have been created in the ABsmartly web console.

```rust
use absmartly_sdk::{SDK, SDKOptions};

let sdk = SDK::new(SDKOptions::default());
```

### Creating a New Context with Pre-fetched Data

When doing full-stack experimentation with ABsmartly, we recommend creating a context only once on the server-side. Creating a context involves a round-trip to the ABsmartly event collector. We can avoid repeating the round-trip on the client-side by sending the server-side data embedded in the first document.

```rust
use absmartly_sdk::{SDK, SDKOptions, ContextData};

let sdk = SDK::new(SDKOptions::default());

// Define units for the context - accepts arrays of tuples (no .to_string() needed!)
let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

// Load context data from ABsmartly API (you'll need to fetch this from your backend)
let context_data: ContextData = serde_json::from_str(&api_response).unwrap();

// Create context with pre-fetched data
let mut context = sdk.create_context_with(units, context_data, None);
```

### Setting Extra Units for a Context

You can add additional units to a context by calling the `set_unit()` method. This method may be used, for example, when a user logs in to your application, and you want to use the new unit type in the context.

**Note:** You cannot override an already set unit type as that would be a change of identity. In this case, you must create a new context instead.

```rust
context.set_unit("db_user_id", "1000013").unwrap();
```

### Setting Context Attributes

Attributes are used for audience targeting. The `set_attribute()` method can be called before the context is ready. It accepts native Rust types directly:

```rust
// Accepts native Rust types - no json!() macro needed!
context.set_attribute("user_agent", "Mozilla/5.0").unwrap();
context.set_attribute("customer_age", "new_customer").unwrap();
context.set_attribute("age", 25).unwrap();
context.set_attribute("premium", true).unwrap();
```

### Selecting a Treatment

```rust
let variant = context.treatment("exp_test_experiment");

if variant == 0 {
    // User is in control group (variant 0)
    println!("Control group");
} else {
    // User is in treatment group
    println!("Treatment group: variant {}", variant);
}
```

### Tracking a Goal Achievement

Goals are created in the ABsmartly web console.

```rust
use serde_json::json;

// Track a simple goal (use () for no properties)
context.track("payment", ()).unwrap();

// Track a goal with properties using json!()
context.track("purchase", json!({
    "item_count": 1,
    "total_amount": 1999.99
})).unwrap();
```

### Publishing Pending Data

Sometimes it is necessary to ensure all events have been published to the ABsmartly collector before proceeding. You can explicitly call the `publish()` method.

```rust
let publish_params = context.publish();
// Send publish_params to ABsmartly collector API
```

### Finalizing

The `finalize()` method will ensure all events have been published to the ABsmartly collector, like `publish()`, and will also "seal" the context, preventing any further events from being tracked.

```rust
context.finalize();
// Context is now sealed - no more treatments or goals can be tracked
```

## Basic Usage

### Peek at Treatment Variants

Although generally not recommended, it is sometimes necessary to peek at a treatment without triggering an exposure. The ABsmartly SDK provides a `peek()` method for that.

```rust
let variant = context.peek("exp_test_experiment");

if variant == 0 {
    // User is in control group (variant 0)
} else {
    // User is in treatment group
}
```

### Overriding Treatment Variants

During development, for example, it is useful to force a treatment for an experiment. This can be achieved with the `set_override()` method.

```rust
context.set_override("exp_test_experiment", 1); // Force variant 1

// You can also set multiple overrides
context.set_override("exp_another_experiment", 0);
```

### Custom Assignments

Custom assignments allow you to set a specific variant for an experiment programmatically.

```rust
context.set_custom_assignment("exp_test_experiment", 1).unwrap();
```

## Variable Values

Get configuration values from experiments. Variables allow you to configure different values for each variant.

```rust
// Get a variable value with a default fallback
// Accepts native Rust types - no json!() macro needed!
let button_color = context.variable_value("button.color", "blue");
println!("Button color: {}", button_color);

// Get other types of variables
let show_banner = context.variable_value("banner.show", false);
let max_items = context.variable_value("cart.max_items", 10);
```

## Advanced Usage

### Getting Experiment Data

You can retrieve information about experiments in the current context.

```rust
// Get all experiment names
let experiments = context.get_experiments();
for name in experiments {
    println!("Experiment: {}", name);
}

// Get custom field value for an experiment
if let Some(value) = context.get_custom_field_value("exp_test", "analyst") {
    println!("Analyst: {}", value);
}
```

### Checking Variable Keys

You can get all variable keys for an experiment.

```rust
let keys = context.variable_keys();
for key in keys {
    println!("Variable key: {}", key);
}
```

## Context Data Structure

The SDK expects context data in the following format (typically fetched from the ABsmartly API):

```rust
use absmartly_sdk::ContextData;

let context_data = ContextData {
    experiments: vec![
        // Experiment configurations from ABsmartly API
    ],
};
```

## Error Handling

The SDK uses Rust's `Result` type for error handling:

```rust
use absmartly_sdk::SDKError;

match context.set_unit("user_id", "12345") {
    Ok(_) => println!("Unit set successfully"),
    Err(SDKError::UnitAlreadySet(unit_type)) => {
        println!("Cannot override unit type: {}", unit_type);
    }
    Err(e) => println!("Error: {:?}", e),
}
```

## Thread Safety

The Context is designed for single-threaded use. If you need to use it across threads, wrap it in appropriate synchronization primitives like `Arc<Mutex<Context>>`.

## Documentation

- [API Documentation](https://docs.rs/absmartly-sdk)
- [ABsmartly Documentation](https://docs.absmartly.com)

## About ABsmartly

**ABsmartly** is the leading provider of state-of-the-art, on-premises, full-stack experimentation platforms for engineering and product teams that want to confidently deploy features as fast as they can develop them.

ABsmartly's real-time analytics helps engineering and product teams ensure that new features will improve the customer experience without breaking or degrading performance and/or business metrics.

### Have a look at our growing list of SDKs:

- [Java SDK](https://www.github.com/absmartly/java-sdk)
- [JavaScript SDK](https://www.github.com/absmartly/javascript-sdk)
- [PHP SDK](https://www.github.com/absmartly/php-sdk)
- [Swift SDK](https://www.github.com/absmartly/swift-sdk)
- [Vue2 SDK](https://www.github.com/absmartly/vue2-sdk)
- [Vue3 SDK](https://www.github.com/absmartly/vue3-sdk)
- [React SDK](https://www.github.com/absmartly/react-sdk)
- [Ruby SDK](https://www.github.com/absmartly/ruby-sdk)
- [Golang SDK](https://www.github.com/absmartly/go-sdk)
- [Flutter/Dart SDK](https://www.github.com/absmartly/flutter-sdk)
- [Python3 SDK](https://www.github.com/absmartly/python3-sdk)
- [.NET SDK](https://www.github.com/absmartly/dotnet-sdk)
- [Rust SDK](https://www.github.com/absmartly/rust-sdk)

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
