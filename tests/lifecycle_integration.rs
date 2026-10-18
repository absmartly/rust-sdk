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
        .retries(1)
        .build()
        .unwrap()
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
    assert!(!context.is_ready());
    let ready_error = context.ready_error().expect("ready_error is set");
    assert!(ready_error.contains("404"), "got: {}", ready_error);
    assert_eq!(context.get_unit("session_id"), Some(&"user-1".to_string()));

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
    context.treatment("exp_test");
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
    context.treatment("exp_test");

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
    context.treatment("exp_test");

    assert!(context.finalize().is_err());
    assert!(!context.is_finalized());
    assert_eq!(context.pending(), 1);

    context.finalize().unwrap();
    assert!(context.is_finalized());
    assert_eq!(context.pending(), 0);
    assert_eq!(server.puts().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn current_thread_runtime_publishes_in_background() {
    let server = start_server(Plan::default()).await;
    let mut context = sdk(&server.base)
        .create_context([("session_id", "user-1")], None)
        .await
        .unwrap();
    context.treatment("exp_test");

    // A current-thread runtime cannot block, so the PUT is spawned.
    context.publish().unwrap();
    assert_eq!(context.pending(), 0);

    for _ in 0..100 {
        if !server.puts().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(server.puts().len(), 1);
}
