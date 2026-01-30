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

**Using the builder pattern (recommended):**

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::builder()
    .endpoint("https://your-company.absmartly.io/v1")
    .api_key("YOUR-API-KEY")
    .application("website")
    .environment("development")
    .build()?;
```

**Using positional parameters:**

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;
```

**Advanced configuration:**

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::builder()
    .endpoint("https://your-company.absmartly.io/v1")
    .api_key("YOUR-API-KEY")
    .application("website")
    .environment("development")
    .timeout(5000)
    .retries(3)
    .agent("my-agent")
    .build()?;
```

**SDK Options**

| Config                  | Type                              | Required? |   Default   | Description                                                                                                                                                                   |
| :---------------------- | :-------------------------------- | :-------: | :---------: | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| endpoint                | `String`                          |  &#9989;  | `""`        | The URL to your API endpoint. Most commonly `"your-company.absmartly.io"`                                                                                                     |
| api_key                 | `String`                          |  &#9989;  | `""`        | Your API key which can be found on the Web Console.                                                                                                                           |
| environment             | `String`                          |  &#9989;  | `""`        | The environment of the platform where the SDK is installed. Environments are created on the Web Console and should match the available environments in your infrastructure.   |
| application             | `String`                          |  &#9989;  | `""`        | The name of the application where the SDK is installed. Applications are created on the Web Console and should match the applications where your experiments will be running. |
| retries                 | `u32`                             |  &#10060; | `5`         | Number of retry attempts for failed HTTP requests                                                                                                                             |
| timeout_ms              | `u64`                             |  &#10060; | `3000`      | Connection timeout in milliseconds                                                                                                                                            |
| agent                   | `Option<String>`                  |  &#10060; | `None`      | Custom user agent string                                                                                                                                                      |

### Creating a New Context

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;

// Define units for the context - accepts arrays of tuples (no .to_string() needed!)
let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

// Create context
let mut context = sdk.create_context(units, None).await?;

// Wait for context to be ready
context.wait_until_ready()?;
```

### Creating a New Context with Pre-fetched Data

When doing full-stack experimentation with ABsmartly, we recommend creating a context only once on the server-side. Creating a context involves a round-trip to the ABsmartly event collector. We can avoid repeating the round-trip on the client-side by sending the server-side data embedded in the first document.

```rust
use absmartly_sdk::{ABsmartly, ContextData};

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;

// Define units for the context
let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

// Load context data from ABsmartly API (you'll need to fetch this from your backend)
let context_data: ContextData = serde_json::from_str(&api_response)?;

// Create context with pre-fetched data - no network round-trip needed
let mut context = sdk.create_context_with(units, context_data, None);
assert!(context.is_ready()); // Context is immediately ready
```

### Refreshing the Context with Fresh Experiment Data

For long-running contexts, the context can be refreshed manually to pull updated experiment data:

```rust
// Refresh the context
context.refresh()?;
```

### Setting Extra Units for a Context

You can add additional units to a context by calling the `set_unit()` method. This method may be used, for example, when a user logs in to your application, and you want to use the new unit type in the context.

**Note:** You cannot override an already set unit type as that would be a change of identity. In this case, you must create a new context instead.

```rust
context.set_unit("db_user_id", "1000013")?;

// Or set multiple units at once
context.set_units([
    ("db_user_id", "1000013"),
    ("user_type", "premium"),
])?;
```

### Setting Context Attributes

Attributes are used for audience targeting. The `set_attribute()` method can be called before the context is ready. It accepts native Rust types directly:

```rust
// Accepts native Rust types - no json!() macro needed!
context.set_attribute("user_agent", "Mozilla/5.0")?;
context.set_attribute("customer_age", "new_customer")?;
context.set_attribute("age", 25)?;
context.set_attribute("premium", true)?;

// Or set multiple attributes at once
context.set_attributes([
    ("user_agent", "Mozilla/5.0"),
    ("customer_age", "new_customer"),
])?;
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
context.track("payment", ())?;

// Track a goal with properties using json!()
context.track("purchase", json!({
    "item_count": 1,
    "total_amount": 1999.99
}))?;
```

### Publishing Pending Data

Sometimes it is necessary to ensure all events have been published to the ABsmartly collector before proceeding. You can explicitly call the `publish()` method.

```rust
context.publish()?;
```

### Finalizing

The `finalize()` method will ensure all events have been published to the ABsmartly collector, like `publish()`, and will also "seal" the context, preventing any further events from being tracked.

```rust
context.finalize()?;
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

### Peeking at Variables

You can peek at variable values without triggering an exposure.

```rust
let button_color = context.peek_variable_value("button.color", "red");
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
context.set_custom_assignment("exp_test_experiment", 1)?;

// Or set multiple custom assignments at once
context.set_custom_assignments([
    ("exp_test_experiment", 1),
    ("exp_another_experiment", 0),
])?;
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

## Custom Event Logger

You can implement a custom event logger to handle SDK events. This is useful for debugging, analytics, or integrating with other systems.

```rust
use absmartly_sdk::{EventLogger, EventType, Context};

struct CustomEventLogger;

impl EventLogger for CustomEventLogger {
    fn handle_event(&self, context: &Context, event: EventType, data: &dyn std::any::Any) {
        match event {
            EventType::Exposure => {
                if let Some(exposure) = data.downcast_ref::<Exposure>() {
                    println!("Exposed to experiment: {}", exposure.name);
                }
            }
            EventType::Goal => {
                if let Some(goal) = data.downcast_ref::<GoalAchievement>() {
                    println!("Goal tracked: {}", goal.name);
                }
            }
            EventType::Error => {
                if let Some(error) = data.downcast_ref::<SDKError>() {
                    eprintln!("Error: {:?}", error);
                }
            }
            EventType::Ready | EventType::Refresh | EventType::Publish | EventType::Close => {
                // Handle other events as needed
            }
        }
    }
}

// Usage with custom event logger requires SDKConfig
let config = SDKConfig::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
);
// Note: event_logger support coming soon

let sdk = ABsmartly::from_config(config)?;
```

**Event Types**

| Event      | When                                               | Data                                   |
| ---------- | -------------------------------------------------- | -------------------------------------- |
| `Error`    | Context receives an error                          | `SDKError`                             |
| `Ready`    | Context turns ready                                | `ContextData` used to initialize       |
| `Refresh`  | `refresh()` method succeeds                        | `ContextData` used to refresh          |
| `Publish`  | `publish()` method succeeds                        | `PublishEvent` sent to collector       |
| `Exposure` | `treatment()` succeeds on first exposure           | `Exposure` enqueued for publishing     |
| `Goal`     | `track()` method succeeds                          | `GoalAchievement` enqueued for publishing |
| `Close`    | `finalize()` method succeeds the first time        | `()`                                   |

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

## Platform-Specific Examples

### Using with Axum

```rust
use axum::{
    routing::get,
    Router,
    extract::State,
    response::Html,
};
use absmartly_sdk::ABsmartly;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    absmartly: Arc<ABsmartly>,
}

#[tokio::main]
async fn main() {
    let sdk = ABsmartly::new(
        "https://your-company.absmartly.io/v1",
        "YOUR-API-KEY",
        "website",
        "production",
    ).expect("Failed to initialize ABsmartly SDK");

    let state = AppState {
        absmartly: Arc::new(sdk),
    };

    let app = Router::new()
        .route("/", get(handler))
        .with_state(state);

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler(State(state): State<AppState>) -> Html<String> {
    let units = [("session_id", "example-session-id")];

    let mut context = state.absmartly
        .create_context(units, None)
        .await
        .expect("Failed to create context");

    context.wait_until_ready().expect("Context failed to become ready");

    let treatment = context.treatment("exp_test_experiment");

    context.finalize().expect("Failed to finalize context");

    if treatment == 0 {
        Html("<h1>Control Group</h1>".to_string())
    } else {
        Html("<h1>Treatment Group</h1>".to_string())
    }
}
```

### Using with Actix Web

```rust
use actix_web::{get, web, App, HttpServer, HttpResponse};
use absmartly_sdk::ABsmartly;
use std::sync::Arc;

struct AppState {
    absmartly: Arc<ABsmartly>,
}

#[get("/")]
async fn index(data: web::Data<AppState>) -> HttpResponse {
    let units = [("session_id", "example-session-id")];

    let mut context = data.absmartly
        .create_context(units, None)
        .await
        .expect("Failed to create context");

    context.wait_until_ready().expect("Context failed to become ready");

    let treatment = context.treatment("exp_test_experiment");

    context.finalize().expect("Failed to finalize context");

    if treatment == 0 {
        HttpResponse::Ok().body("<h1>Control Group</h1>")
    } else {
        HttpResponse::Ok().body("<h1>Treatment Group</h1>")
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let sdk = ABsmartly::new(
        "https://your-company.absmartly.io/v1",
        "YOUR-API-KEY",
        "website",
        "production",
    ).expect("Failed to initialize ABsmartly SDK");

    let app_state = web::Data::new(AppState {
        absmartly: Arc::new(sdk),
    });

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .service(index)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
```

### Using with Rocket

```rust
use rocket::{State, get, routes};
use absmartly_sdk::ABsmartly;
use std::sync::Arc;

#[get("/")]
async fn index(sdk: &State<Arc<ABsmartly>>) -> String {
    let units = [("session_id", "example-session-id")];

    let mut context = sdk.create_context(units, None)
        .await
        .expect("Failed to create context");

    context.wait_until_ready().expect("Context failed to become ready");

    let treatment = context.treatment("exp_test_experiment");

    context.finalize().expect("Failed to finalize context");

    if treatment == 0 {
        String::from("<h1>Control Group</h1>")
    } else {
        String::from("<h1>Treatment Group</h1>")
    }
}

#[rocket::main]
async fn main() {
    let sdk = ABsmartly::new(
        "https://your-company.absmartly.io/v1",
        "YOUR-API-KEY",
        "website",
        "production",
    ).expect("Failed to initialize ABsmartly SDK");

    rocket::build()
        .manage(Arc::new(sdk))
        .mount("/", routes![index])
        .launch()
        .await
        .unwrap();
}
```

## Advanced Request Configuration

### Request Timeout with Tokio

```rust
use absmartly_sdk::ABsmartly;
use tokio::time::{timeout, Duration};

async fn create_context_with_timeout() -> Result<Context, Box<dyn std::error::Error>> {
    let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

    let mut context = sdk.create_context(units, None).await?;

    match timeout(Duration::from_millis(1500), context.wait_until_ready_async()).await {
        Ok(Ok(_)) => Ok(context),
        Ok(Err(e)) => Err(Box::new(e)),
        Err(_) => Err("Context creation timed out".into()),
    }
}
```

### Request Cancellation with Tokio

```rust
use absmartly_sdk::ABsmartly;
use tokio::select;
use tokio::sync::oneshot;

async fn create_context_with_cancellation() {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

    let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

    let mut context = sdk.create_context(units, None).await.unwrap();

    select! {
        result = context.wait_until_ready_async() => {
            match result {
                Ok(_) => println!("Context ready"),
                Err(e) => eprintln!("Context failed: {:?}", e),
            }
        }
        _ = cancel_rx => {
            println!("Context creation cancelled");
        }
    }

    // To cancel: drop(cancel_tx) or cancel_tx.send(()).ok();
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

## About A/B Smartly

**A/B Smartly** is the leading provider of state-of-the-art, on-premises, full-stack experimentation platforms for engineering and product teams that want to confidently deploy features as fast as they can develop them.

A/B Smartly's real-time analytics helps engineering and product teams ensure that new features will improve the customer experience without breaking or degrading performance and/or business metrics.

### Have a look at our growing list of clients and SDKs:

- [JavaScript SDK](https://www.github.com/absmartly/javascript-sdk)
- [Java SDK](https://www.github.com/absmartly/java-sdk)
- [PHP SDK](https://www.github.com/absmartly/php-sdk)
- [Swift SDK](https://www.github.com/absmartly/swift-sdk)
- [Vue2 SDK](https://www.github.com/absmartly/vue2-sdk)
- [Vue3 SDK](https://www.github.com/absmartly/vue3-sdk)
- [React SDK](https://www.github.com/absmartly/react-sdk)
- [Python3 SDK](https://www.github.com/absmartly/python3-sdk)
- [Go SDK](https://www.github.com/absmartly/go-sdk)
- [Ruby SDK](https://www.github.com/absmartly/ruby-sdk)
- [.NET SDK](https://www.github.com/absmartly/dotnet-sdk)
- [Dart SDK](https://www.github.com/absmartly/dart-sdk)
- [Flutter SDK](https://www.github.com/absmartly/flutter-sdk)
- [Rust SDK](https://www.github.com/absmartly/rust-sdk)

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
