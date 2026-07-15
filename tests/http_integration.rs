//! Hermetic end-to-end HTTP integration test.
//!
//! Starts a REAL local HTTP/1.1 server on an ephemeral port, points the SDK's
//! public client at it, and drives the public async API so the SDK's REAL
//! reqwest client makes:
//!   1. a GET /context  (via `create_context().await`)
//!   2. a PUT /context  (via `sdk.publish(&params).await` after treatment + track)
//!
//! No live backend, no mocking crate -- a tiny tokio TCP server is used so the
//! whole thing runs in this repo's own PR CI.

use std::sync::{Arc, Mutex};

use absmartly_sdk::{ABsmartly, ContextOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A single request captured by the mock server.
#[derive(Debug, Default, Clone)]
struct Captured {
    method: String,
    path: String, // full request target, e.g. "/context?application=..."
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Shared record of all requests the server saw.
type Log = Arc<Mutex<Vec<Captured>>>;

/// Parse a raw HTTP/1.1 request (already fully read) into a `Captured`.
fn parse_request(raw: &str) -> Captured {
    let mut cap = Captured::default();
    let header_end = raw.find("\r\n\r\n").unwrap_or(raw.len());
    let (head, body) = raw.split_at(header_end);
    cap.body = body.trim_start_matches("\r\n\r\n").to_string();

    let mut lines = head.split("\r\n");
    if let Some(request_line) = lines.next() {
        let mut parts = request_line.split_whitespace();
        cap.method = parts.next().unwrap_or_default().to_string();
        cap.path = parts.next().unwrap_or_default().to_string();
    }
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            cap.headers
                .push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    cap
}

/// Read a full HTTP request from the socket, honoring Content-Length so the
/// PUT body is captured in full.
async fn read_full_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    loop {
        let n = stream.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);

        let text = String::from_utf8_lossy(&buf);
        if let Some(header_end) = text.find("\r\n\r\n") {
            // Determine declared body length (if any) and keep reading until we
            // have all of it.
            let head = &text[..header_end];
            let content_length = head
                .split("\r\n")
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            let body_so_far = buf.len() - (header_end + 4);
            if body_so_far >= content_length {
                break;
            }
        }
    }

    String::from_utf8_lossy(&buf).to_string()
}

/// Start the mock server. Returns the base URL ("http://127.0.0.1:<port>")
/// and the shared request log.
async fn start_mock_server() -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    // A single full-on experiment so a treatment produces a real exposure that
    // ends up in the publish body. unit_type matches the unit we set below.
    let get_body = r#"{"experiments":[{"id":1,"name":"exp_test","unitType":"session_id","iteration":1,"fullOnVariant":1,"trafficSplit":[0.0,1.0],"split":[0.5,0.5],"variants":[{"config":null},{"config":null}],"variables":{}}]}"#;

    let log_clone = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let log = log_clone.clone();
            let get_body = get_body.to_string();
            tokio::spawn(async move {
                let raw = read_full_request(&mut stream).await;
                let cap = parse_request(&raw);
                let method = cap.method.clone();
                log.lock().unwrap().push(cap);

                let response = if method == "PUT" {
                    let body = "{}";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        get_body.len(),
                        get_body
                    )
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (base, log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_get_and_put_against_local_server() {
    let (base, log) = start_mock_server().await;

    let api_key = "test-api-key";
    let application = "www";
    let environment = "production";

    let sdk = ABsmartly::builder()
        .endpoint(&base)
        .api_key(api_key)
        .application(application)
        .environment(environment)
        .agent("absmartly-rust-sdk")
        .build()
        .expect("SDK should build");

    // ---- 1. GET /context (real HTTP fetch driving context to ready) ----
    let mut context = sdk
        .create_context(
            [("session_id", "bleh@absmartly.com")],
            Some(ContextOptions::default()),
        )
        .await
        .expect("create_context should fetch and succeed");

    assert!(context.is_ready(), "context should be ready after fetch");

    // ---- Queue an event: treatment (exposure) + track (goal) ----
    let _variant = context.treatment("exp_test");
    context
        .track("payment", serde_json::json!({ "value": 42 }))
        .expect("track should succeed");
    assert!(
        context.pending() > 0,
        "there should be a pending event to publish"
    );

    // ---- 2. PUT /context (real HTTP publish, awaitable) ----
    let params = context.get_publish_params();
    sdk.publish(&params).await.expect("publish should succeed");

    // ---------- Assertions on what the server actually received ----------
    let captured = log.lock().unwrap().clone();

    // GET assertions: path is /context and the query carries application + environment.
    let get = captured
        .iter()
        .find(|c| c.method == "GET")
        .expect("server should have received a GET");
    let (get_path, get_query) = get.path.split_once('?').unwrap_or((get.path.as_str(), ""));
    assert_eq!(get_path, "/context", "GET path should be /context");
    assert!(
        get_query.contains(&format!("application={}", application)),
        "GET query should contain application={}, got: {}",
        application,
        get_query
    );
    assert!(
        get_query.contains(&format!("environment={}", environment)),
        "GET query should contain environment={}, got: {}",
        environment,
        get_query
    );

    // PUT assertions: method/path, headers the SDK actually sends, body fields.
    let put = captured
        .iter()
        .find(|c| c.method == "PUT")
        .expect("server should have received a PUT");
    let (put_path, _) = put.path.split_once('?').unwrap_or((put.path.as_str(), ""));
    assert_eq!(put_path, "/context", "PUT path should be /context");

    // The publish request must carry the full canonical header set, matching the
    // collector contract and the other SDKs.
    assert_eq!(
        put.header("X-API-Key"),
        Some(api_key),
        "PUT must carry the X-API-Key header"
    );
    assert_eq!(
        put.header("X-Application"),
        Some(application),
        "PUT must carry the X-Application header"
    );
    assert_eq!(
        put.header("X-Environment"),
        Some(environment),
        "PUT must carry the X-Environment header"
    );
    assert_eq!(
        put.header("X-Application-Version"),
        Some("0"),
        "PUT must carry X-Application-Version: 0"
    );
    assert_eq!(
        put.header("Content-Type"),
        Some("application/json"),
        "PUT must declare Content-Type: application/json"
    );
    let agent = put
        .header("X-Agent")
        .expect("PUT must carry an X-Agent (the configured agent)");
    assert!(!agent.is_empty(), "X-Agent must be non-empty");

    // Body JSON fields required by the wire contract.
    let body: serde_json::Value =
        serde_json::from_str(&put.body).expect("PUT body should be valid JSON");
    assert!(
        body.get("hashed").and_then(|v| v.as_bool()).is_some(),
        "publish body must contain `hashed`"
    );
    assert!(
        body.get("units").and_then(|v| v.as_array()).is_some(),
        "publish body must contain a `units` array"
    );
    let units = body["units"].as_array().unwrap();
    assert!(!units.is_empty(), "`units` should not be empty");
    assert!(
        units[0].get("type").is_some() && units[0].get("uid").is_some(),
        "each unit should have `type` and `uid`"
    );
    assert!(
        body.get("publishedAt").and_then(|v| v.as_i64()).is_some(),
        "publish body must contain `publishedAt` (epoch millis)"
    );

    // We did a treatment + track, so exposures and goals should both be present.
    let exposures = body
        .get("exposures")
        .and_then(|v| v.as_array())
        .expect("publish body should contain an `exposures` array after a treatment");
    assert!(!exposures.is_empty(), "`exposures` should not be empty");
    let goals = body
        .get("goals")
        .and_then(|v| v.as_array())
        .expect("publish body should contain a `goals` array after a track");
    assert!(!goals.is_empty(), "`goals` should not be empty");
    assert_eq!(
        goals[0].get("name").and_then(|v| v.as_str()),
        Some("payment"),
        "tracked goal name should round-trip"
    );
}
