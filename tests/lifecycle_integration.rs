//! Hermetic HTTP tests for the context lifecycle through the SDK's real
//! `create_context*` and `DefaultContextPublisher` paths:
//!   - `ready` is emitted after a successful fetch, `error` after a failed one
//!   - `Context::publish()` reports a failed PUT and keeps events pending
//!   - a later publish retries the same events

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use absmartly_sdk::{ABsmartly, ContextOptions};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CONTEXT_BODY: &str = r#"{"experiments":[{"id":1,"name":"exp_test","unitType":"session_id","iteration":1,"fullOnVariant":1,"trafficSplit":[0.0,1.0],"split":[0.5,0.5],"variants":[{"config":null},{"config":null}],"variables":{}}]}"#;

#[derive(Debug, Clone)]
struct Request {
    method: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Status codes served, in order, for GET and PUT requests. When a queue is
/// empty the server answers 200.
#[derive(Default)]
struct Plan {
    get: VecDeque<u16>,
    put: VecDeque<u16>,
}

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    fn puts(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "PUT")
            .cloned()
            .collect()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Request {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .split("\r\n")
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() - (end + 4) >= length {
                break;
            }
        }
    }

    let text = String::from_utf8_lossy(&buf).to_string();
    let end = text.find("\r\n\r\n").unwrap_or(text.len());
    let (head, body) = text.split_at(end);
    let mut lines = head.split("\r\n");
    let method = lines
        .next()
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Request {
        method,
        headers,
        body: body.trim_start_matches("\r\n\r\n").to_string(),
    }
}

async fn start_server(plan: Plan) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let plan = Arc::new(Mutex::new(plan));

    let log = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let log = log.clone();
            let plan = plan.clone();
            tokio::spawn(async move {
                let request = read_request(&mut stream).await;
                let status = {
                    let mut plan = plan.lock().unwrap();
                    let queue = if request.method == "PUT" {
                        &mut plan.put
                    } else {
                        &mut plan.get
                    };
                    queue.pop_front().unwrap_or(200)
                };
                let body = if request.method == "GET" && status == 200 {
                    CONTEXT_BODY
                } else {
                    "{}"
                };
                log.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    Server { base, requests }
}

fn sdk(base: &str) -> ABsmartly {
    ABsmartly::builder()
        .endpoint(base)
        .api_key("test-api-key")
        .application("www")
        .environment("production")
        .agent("absmartly-rust-sdk")
        .retries(0)
        .build()
        .unwrap()
}

fn retrying_sdk(base: &str, retries: u32, timeout_ms: u64) -> ABsmartly {
    ABsmartly::builder()
        .endpoint(base)
        .api_key("test-api-key")
        .application("www")
        .environment("production")
        .agent("absmartly-rust-sdk")
        .retries(retries)
        .timeout(timeout_ms)
        .build()
        .unwrap()
}

impl Server {
    fn gets(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "GET")
            .cloned()
            .collect()
    }
}

fn assert_js_headers(request: &Request) {
    assert_eq!(request.header("X-API-Key"), Some("test-api-key"));
    assert_eq!(request.header("X-Application"), Some("www"));
    assert_eq!(request.header("X-Environment"), Some("production"));
    assert_eq!(request.header("X-Application-Version"), Some("0"));
    assert_eq!(request.header("X-Agent"), Some("absmartly-rust-sdk"));
    assert_eq!(request.header("User-Agent"), Some("absmartly-rust-sdk"));
    assert_eq!(request.header("Content-Type"), Some("application/json"));
}

type Events = Arc<Mutex<Vec<(String, Option<Value>)>>>;

fn logging_options() -> (ContextOptions, Events) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let options = ContextOptions {
        event_logger: Some(Arc::new(move |_ctx, name, data| {
            sink.lock().unwrap().push((name.to_string(), data));
        })),
        ..Default::default()
    };
    (options, events)
}

fn names(events: &Events) -> Vec<String> {
    events
        .lock()
        .unwrap()
        .iter()
        .map(|(n, _)| n.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_context_emits_ready_with_fetched_data() {
    let server = start_server(Plan::default()).await;
    let (options, events) = logging_options();

    let context = sdk(&server.base)
        .create_context([("session_id", "user-1")], Some(options))
        .await
        .unwrap();

    assert!(context.is_ready());
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "ready");
    let data = events[0]
        .1
        .as_ref()
        .expect("ready carries the fetched data");
    assert_eq!(data["experiments"][0]["name"], "exp_test");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_context_or_failed_returns_failed_context_with_error_event() {
    let server = start_server(Plan {
        get: VecDeque::from([404]),
        ..Default::default()
    })
    .await;
    let (options, events) = logging_options();

    let context = sdk(&server.base)
        .create_context_or_failed([("session_id", "user-1")], Some(options))
        .await;

    assert!(context.is_failed());
    assert!(context.is_ready());
    let ready_error = context.ready_error().expect("ready_error is set");
    assert!(ready_error.contains("404"), "got: {}", ready_error);
    assert_eq!(
        context.get_unit("session_id"),
        Some(&serde_json::json!("user-1"))
    );

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "error");
    let message = events[0].1.as_ref().unwrap()["message"].as_str().unwrap();
    assert_eq!(message, ready_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_context_still_returns_err_on_failed_fetch() {
    let server = start_server(Plan {
        get: VecDeque::from([404]),
        ..Default::default()
    })
    .await;

    let result = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await;

    assert!(result.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_publish_failure_keeps_events_and_retry_delivers_them() {
    let server = start_server(Plan {
        put: VecDeque::from([500]),
        ..Default::default()
    })
    .await;
    let (options, events) = logging_options();
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], Some(options))
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();
    context
        .track("payment", serde_json::json!({"value": 1}))
        .unwrap();
    assert_eq!(context.pending(), 2);
    events.lock().unwrap().clear();

    let error = context.publish().expect_err("a 500 must fail the publish");
    assert!(error.contains("500"), "got: {}", error);
    assert_eq!(context.pending(), 2);
    assert_eq!(names(&events), vec!["error"]);

    context.publish().expect("the retry succeeds");
    assert_eq!(context.pending(), 0);
    assert_eq!(names(&events), vec!["error", "publish"]);

    let puts = server.puts();
    assert_eq!(puts.len(), 2);
    let failed: Value = serde_json::from_str(&puts[0].body).unwrap();
    let retried: Value = serde_json::from_str(&puts[1].body).unwrap();
    // publishedAt is stamped per attempt; the events themselves must match.
    assert_eq!(failed["exposures"], retried["exposures"]);
    assert_eq!(failed["goals"], retried["goals"]);
    assert_eq!(retried["exposures"].as_array().map(Vec::len), Some(1));
    assert_eq!(retried["goals"].as_array().map(Vec::len), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_publish_sends_canonical_headers() {
    let server = start_server(Plan::default()).await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();

    context.publish().unwrap();

    let puts = server.puts();
    assert_eq!(puts.len(), 1, "publish completes before returning");
    let put = &puts[0];
    assert_eq!(put.header("X-API-Key"), Some("test-api-key"));
    assert_eq!(put.header("X-Application"), Some("www"));
    assert_eq!(put.header("X-Environment"), Some("production"));
    assert_eq!(put.header("X-Application-Version"), Some("0"));
    assert_eq!(put.header("X-Agent"), Some("absmartly-rust-sdk"));
    assert_eq!(put.header("Content-Type"), Some("application/json"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_publish_failure_leaves_context_open() {
    let server = start_server(Plan {
        put: VecDeque::from([503]),
        ..Default::default()
    })
    .await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();

    assert!(context.finalize().is_err());
    assert!(!context.is_finalized());
    assert_eq!(context.pending(), 1);

    context.finalize().unwrap();
    assert!(context.is_finalized());
    assert_eq!(context.pending(), 0);
    assert_eq!(server.puts().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn current_thread_runtime_publish_reports_result_and_never_panics() {
    let server = start_server(Plan {
        put: [500].into(),
        ..Plan::default()
    })
    .await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();

    // The test server shares this single-threaded runtime, so run the
    // synchronous calls on a blocking thread to let it answer.
    let (context, failed, retried, refreshed) = tokio::task::spawn_blocking(move || {
        let failed = context.publish();
        let retried = context.publish();
        let refreshed = context.refresh();
        (context, failed, retried, refreshed)
    })
    .await
    .unwrap();

    // A failed delivery is reported and the events stay pending for a retry.
    assert!(failed.is_err());
    assert!(retried.is_ok());
    assert_eq!(context.pending(), 0);
    assert!(refreshed.is_ok());
    assert_eq!(server.puts().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_unit_never_yields_a_ready_context() {
    let server = start_server(Plan::default()).await;
    let sdk = sdk(&server.base);

    let failed = sdk
        .create_context_or_failed([("session_id", "")], None)
        .await;
    assert!(failed.is_failed());
    assert!(failed.ready_error().is_some());
    // The unit is rejected before any context fetch.
    assert!(server.gets().is_empty());

    assert!(sdk
        .create_context([("session_id", "")], None)
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_rejects_non_success_response_and_keeps_data() {
    // First GET (create_context) succeeds, the refresh GET returns 500.
    let server = start_server(Plan {
        get: [200, 500].into(),
        ..Plan::default()
    })
    .await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    let before = context.experiments().unwrap().len();
    assert!(before > 0);
    let result = tokio::task::spawn_blocking(move || {
        let r = context.refresh();
        (context, r)
    })
    .await
    .unwrap();
    assert!(result.1.is_err());
    assert_eq!(result.0.experiments().unwrap().len(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_fetch_and_refresh_send_js_headers() {
    let server = start_server(Plan::default()).await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    let context = tokio::task::spawn_blocking(move || {
        context.refresh().unwrap();
        context
    })
    .await
    .unwrap();
    assert!(context.is_ready());

    let gets = server.gets();
    assert_eq!(gets.len(), 2, "create + refresh");
    for get in &gets {
        assert_js_headers(get);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_retries_server_errors() {
    let server = start_server(Plan {
        put: [500, 503].into(),
        ..Plan::default()
    })
    .await;
    let mut context = retrying_sdk(&server.base, 2, 3000)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();

    let context = tokio::task::spawn_blocking(move || {
        context.publish().expect("the third attempt succeeds");
        context
    })
    .await
    .unwrap();

    assert_eq!(context.pending(), 0);
    let puts = server.puts();
    assert_eq!(puts.len(), 3);
    for put in &puts {
        assert_js_headers(put);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_retries_server_errors() {
    let server = start_server(Plan {
        get: [200, 500].into(),
        ..Plan::default()
    })
    .await;
    let mut context = retrying_sdk(&server.base, 1, 3000)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();

    let result = tokio::task::spawn_blocking(move || context.refresh())
        .await
        .unwrap();

    assert!(result.is_ok(), "got: {:?}", result);
    assert_eq!(server.gets().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_errors_are_not_retried() {
    let server = start_server(Plan {
        put: [400].into(),
        ..Plan::default()
    })
    .await;
    let mut context = retrying_sdk(&server.base, 5, 3000)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test").unwrap();

    let (context, result) = tokio::task::spawn_blocking(move || {
        let r = context.publish();
        (context, r)
    })
    .await
    .unwrap();

    assert!(result.is_err());
    assert_eq!(context.pending(), 1);
    assert_eq!(server.puts().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_stop_at_the_timeout_budget() {
    // Every GET fails. With 10 retries the backoff alone would wait 51s; the
    // 300ms timeout bounds the whole exchange instead.
    let server = start_server(Plan {
        get: [500; 20].into(),
        ..Plan::default()
    })
    .await;
    let started = std::time::Instant::now();
    let result = retrying_sdk(&server.base, 10, 300)
        .create_context([("session_id", "user-1")], None)
        .await;
    let elapsed = started.elapsed();

    assert!(result.is_err());
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "took {:?}",
        elapsed
    );
    let attempts = server.gets().len();
    assert!((2..=6).contains(&attempts), "attempts: {}", attempts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_timeout_disables_deadline_for_publish_and_refresh() {
    let server = start_server(Plan::default()).await;
    let sdk = retrying_sdk(&server.base, 0, 0);
    let data = serde_json::from_str(CONTEXT_BODY).unwrap();
    let mut context = sdk.create_context_with([("session_id", "user-1")], data, None);
    context.track("goal", serde_json::json!({})).unwrap();
    assert!(context.pending() > 0);
    context
        .publish()
        .expect("zero timeout disables the deadline");
    assert_eq!(context.pending(), 0);
    assert_eq!(server.puts().len(), 1);
    context.refresh().expect("refresh has no deadline either");
    assert!(!context.is_failed());
    sdk.create_context([("session_id", "user-2")], None)
        .await
        .expect("async client also supports a disabled deadline");
}
