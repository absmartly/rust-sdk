# ABsmartly Rust SDK

[![Crates.io](https://img.shields.io/crates/v/absmartly-sdk.svg)](https://crates.io/crates/absmartly-sdk)
[![Documentation](https://docs.rs/absmartly-sdk/badge.svg)](https://docs.rs/absmartly-sdk)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A Rust SDK for [ABsmartly](https://www.absmartly.com) - A/B testing and feature flagging platform.

## Compatibility

The ABsmartly Rust SDK is compatible with Rust 2021 edition and later. It uses async/await with [tokio](https://tokio.rs/) for network operations and provides a synchronous interface for variant assignment and goal tracking.

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

#### Recommended: Builder Pattern

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::builder()
    .endpoint("https://your-company.absmartly.io/v1")
    .api_key("YOUR-API-KEY")
    .application("website")
    .environment("development")
    .build()?;
```

#### With Optional Parameters

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::builder()
    .endpoint("https://your-company.absmartly.io/v1")
    .api_key("YOUR-API-KEY")
    .application("website")
    .environment("development")
    .timeout(5000)      // connection timeout in milliseconds
    .retries(3)         // max retry attempts
    .agent("my-agent")  // custom user agent string
    .build()?;
```

#### Positional Parameters

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;
```

**SDK Options**

| Config                  | Type                              | Required? |   Default   | Description                                                                                                                                                                   |
| :---------------------- | :-------------------------------- | :-------: | :---------: | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| endpoint                | `String`                          |  &#9989;  | `""`        | The URL to your API endpoint. Most commonly `"https://your-company.absmartly.io/v1"`                                                                                          |
| api_key                 | `String`                          |  &#9989;  | `""`        | Your API key which can be found on the Web Console.                                                                                                                           |
| environment             | `String`                          |  &#9989;  | `""`        | The environment of the platform where the SDK is installed. Environments are created on the Web Console and should match the available environments in your infrastructure.   |
| application             | `String`                          |  &#9989;  | `""`        | The name of the application where the SDK is installed. Applications are created on the Web Console and should match the applications where your experiments will be running. |
| retries                 | `u32`                             |  &#10060; | `5`         | Number of retry attempts for failed HTTP requests                                                                                                                             |
| timeout_ms              | `u64`                             |  &#10060; | `3000`      | Connection timeout in milliseconds                                                                                                                                            |
| agent                   | `Option<String>`                  |  &#10060; | `None`      | Custom user agent string                                                                                                                                                      |

## Creating a New Context

### With Async Network Fetch

```rust
use absmartly_sdk::ABsmartly;

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;

let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

let mut context = sdk.create_context(units, None).await?;
```

### With Pre-fetched Data

When doing full-stack experimentation with ABsmartly, we recommend creating a context only once on the server-side. Creating a context involves a round-trip to the ABsmartly event collector. We can avoid repeating the round-trip on the client-side by sending the server-side data embedded in the first document.

```rust
use absmartly_sdk::{ABsmartly, ContextData};

let sdk = ABsmartly::new(
    "https://your-company.absmartly.io/v1",
    "YOUR-API-KEY",
    "website",
    "development",
)?;

let units = [("session_id", "5ebf06d8cb5d8137290c4abb64155584fbdb64d8")];

let context_data: ContextData = serde_json::from_str(&api_response)?;

let mut context = sdk.create_context_with(units, context_data, None);
assert!(context.is_ready());
```

### Refreshing the Context with Fresh Experiment Data

For long-running contexts, the context can be refreshed manually with updated experiment data:

```rust
let fresh_data: ContextData = serde_json::from_str(&api_response)?;

context.refresh(fresh_data);
```

### Setting Extra Units

You can add additional units to a context by calling the `set_unit()` method. This method may be used, for example, when a user logs in to your application, and you want to use the new unit type in the context.

**Note:** You cannot override an already set unit type as that would be a change of identity. In this case, you must create a new context instead.

```rust
context.set_unit("db_user_id", "1000013")?;

context.set_units([
    ("db_user_id", "1000013"),
    ("user_type", "premium"),
])?;
```

## Basic Usage

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

### Treatment Variables

Get configuration values from experiments. Variables allow you to configure different values for each variant.

```rust
let button_color = context.variable_value("button.color", "blue");
println!("Button color: {}", button_color);

let show_banner = context.variable_value("banner.show", false);
let max_items = context.variable_value("cart.max_items", 10);
```

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

#### Peeking at Variables

```rust
let button_color = context.peek_variable_value("button.color", "red");
```

### Overriding Treatment Variants

During development, for example, it is useful to force a treatment for an experiment. This can be achieved with the `set_override()` method. The `set_override()` and `set_overrides()` methods can be called before the context is ready.

```rust
context.set_override("exp_test_experiment", 1)?; // Force variant 1

context.set_overrides([
    ("exp_test_experiment", 1),
    ("exp_another_experiment", 0),
])?;
```

## Advanced

### Context Attributes

Attributes are used to pass meta-data about the user and/or the request. They can be used later in the Web Console to create segments or audiences. The `set_attribute()` method can be called before the context is ready. It accepts native Rust types directly:

```rust
context.set_attribute("user_agent", "Mozilla/5.0")?;
context.set_attribute("customer_age", "new_customer")?;
context.set_attribute("age", 25)?;
context.set_attribute("premium", true)?;

context.set_attributes([
    ("user_agent", "Mozilla/5.0"),
    ("customer_age", "new_customer"),
])?;
```

### Custom Assignments

Custom assignments allow you to set a specific variant for an experiment programmatically.

```rust
context.set_custom_assignment("exp_test_experiment", 1)?;

context.set_custom_assignments([
    ("exp_test_experiment", 1),
    ("exp_another_experiment", 0),
])?;
```

### Tracking Goals

Goals are created in the ABsmartly web console.

```rust
use serde_json::json;

context.track("payment", ())?;

context.track("purchase", json!({
    "item_count": 1,
    "total_amount": 1999.99
}))?;
```

### Publishing Pending Data

Sometimes it is necessary to ensure all events have been published to the ABsmartly collector before proceeding. You can explicitly call the `publish()` method.

```rust
context.publish();
```

### Finalizing

The `finalize()` method will ensure all events have been published to the ABsmartly collector, like `publish()`, and will also "seal" the context, preventing any further events from being tracked.

```rust
context.finalize();
```

### Custom Event Logger

You can set a custom event logger to handle SDK events. This is useful for debugging, analytics, or integrating with other systems.

```rust
context.set_event_logger(Box::new(|ctx, event_type, data| {
    match event_type {
        "exposure" => println!("Exposure event"),
        "goal" => println!("Goal tracked"),
        "error" => eprintln!("Error: {:?}", data),
        "ready" | "refresh" | "publish" | "finalize" => {}
        _ => {}
    }
}));
```

**Event Types**

| Event      | When                                               | Data                                   |
| ---------- | -------------------------------------------------- | -------------------------------------- |
| `error`    | Context receives an error                          | Error data                             |
| `ready`    | Context turns ready                                | `ContextData` used to initialize       |
| `refresh`  | `refresh()` method succeeds                        | `ContextData` used to refresh          |
| `publish`  | `publish()` method succeeds                        | `PublishParams` sent to collector      |
| `exposure` | `treatment()` succeeds on first exposure           | `Exposure` enqueued for publishing     |
| `goal`     | `track()` method succeeds                          | `Goal` enqueued for publishing         |
| `finalize` | `finalize()` method succeeds the first time        | `None`                                 |

### Error Handling

The SDK uses Rust's `Result` type for error handling:

```rust
use absmartly_sdk::SDKError;

match context.set_unit("user_id", "12345") {
    Ok(_) => println!("Unit set successfully"),
    Err(msg) => println!("Error: {}", msg),
}
```

### Thread Safety

The Context is designed for single-threaded use. If you need to use it across threads, wrap it in appropriate synchronization primitives like `Arc<Mutex<Context>>`.

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

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handler(State(state): State<AppState>) -> Html<String> {
    let units = [("session_id", "example-session-id")];

    let mut context = state.absmartly
        .create_context(units, None)
        .await
        .expect("Failed to create context");

    let treatment = context.treatment("exp_test_experiment");

    context.finalize();

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

    let treatment = context.treatment("exp_test_experiment");

    context.finalize();

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

    let treatment = context.treatment("exp_test_experiment");

    context.finalize();

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
- [Rust SDK](https://www.github.com/absmartly/rust-sdk) (this package)
