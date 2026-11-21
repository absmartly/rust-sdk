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

use absmartly::{Client, ContextOptions};
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
    // Drop exactly the header separator so every body byte is kept.
    cap.body = body.strip_prefix("\r\n\r\n").unwrap_or(body).to_string();

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
    // A single full-on experiment so a treatment produces a real exposure that
    // ends up in the publish body. unit_type matches the unit we set below.
    let get_body = r#"{"experiments":[{"id":1,"name":"exp_test","unitType":"session_id","iteration":1,"fullOnVariant":1,"trafficSplit":[0.0,1.0],"split":[0.5,0.5],"variants":[{"config":null},{"config":null}],"variables":{}}]}"#;

    start_mock_server_with_body(get_body.to_string()).await
}

async fn start_mock_server_with_body(get_body: String) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    let log: Log = Arc::new(Mutex::new(Vec::new()));

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

    let sdk = Client::builder()
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
    let _variant = context.treatment("exp_test").unwrap();
    context
        .track("payment", serde_json::json!({ "value": 42 }))
        .expect("track should succeed");
    assert!(
        context.pending() > 0,
        "there should be a pending event to publish"
    );

    // ---- 2. PUT /context (real HTTP publish, awaitable) ----
    let mut params = context.get_publish_params();
    params.published_at = 0;
    for exposure in params.exposures.iter_mut().flatten() {
        exposure.exposed_at = 0;
    }
    for goal in params.goals.iter_mut().flatten() {
        goal.achieved_at = 0;
    }
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
    let agent = put.header("X-Agent").expect("PUT must carry an X-Agent");
    assert_eq!(agent, absmartly::SDK_AGENT);

    // Pin the actual request encoder, not just direct PublishParams serialization.
    let uid = params.units[0].uid.as_ref().unwrap();
    let expected = concat!(
        "{\"publishedAt\":0,\"units\":[{\"type\":\"session_id\",\"uid\":",
        "\"UID\"}],\"hashed\":true,\"exposures\":[{\"id\":1,\"name\":\"exp_test\",",
        "\"exposedAt\":0,\"unit\":\"session_id\",\"variant\":1,\"assigned\":true,",
        "\"eligible\":true,\"overridden\":false,\"fullOn\":true,\"custom\":false,",
        "\"audienceMismatch\":false,\"ruleOverride\":false}],\"goals\":[{\"name\":\"payment\",",
        "\"properties\":{\"value\":42},\"achievedAt\":0}]}"
    )
    .replace("UID", uid);
    assert_eq!(put.body.as_bytes(), expected.as_bytes());

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_returns_error_on_final_3xx_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let _ = read_full_request(&mut stream).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n")
                    .await;
                let _ = stream.flush().await;
            });
        }
    });

    let sdk = Client::builder()
        .endpoint(&base)
        .api_key("k")
        .application("www")
        .environment("production")
        .build()
        .expect("SDK should build");
    let params = absmartly::PublishParams {
        published_at: 0,
        units: vec![],
        hashed: true,
        exposures: None,
        goals: None,
        attributes: None,
    };
    let err = sdk
        .publish(&params)
        .await
        .expect_err("a 304 publish response must be an error, not a panic");
    assert!(err.to_string().contains("304"), "unexpected error: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetched_contexts_and_refresh_preserve_holdouts() {
    let fixture = serde_json::json!({
        "experiments": [{
            "id": 1, "name": "covered", "unitType": "session_id", "iteration": 1,
            "fullOnVariant": 1, "trafficSplit": [0.0, 1.0], "split": [0.5, 0.5],
            "holdoutIds": [11],
            "variants": [{"config": "{\"color\":\"control\"}"},
                         {"config": "{\"color\":\"treatment\"}"}]
        }],
        "holdouts": [{
            "id": 11, "name": "holdout", "unitType": "session_id", "iteration": 1,
            "seedHi": 13, "seedLo": 111, "split": [1.0, 0.0],
            "variants": [{"config": null}, {"config": null}]
        }]
    });
    let (base, log) = start_mock_server_with_body(fixture.to_string()).await;
    let sdk = Client::new(&base, "k", "www", "production").unwrap();
    let units = [("session_id", "fetched-user")];
    let ready = sdk.create_context(units, None).await.unwrap();
    let or_failed = sdk.create_context_or_failed(units, None).await;
    // Start without holdouts so refresh must introduce coverage, rather than
    // merely retain an already-cached suppressed assignment.
    let mut without_holdouts = fixture.clone();
    without_holdouts["holdouts"] = serde_json::json!([]);
    let mut refreshed = sdk.create_context_with(
        units,
        serde_json::from_value(without_holdouts).unwrap(),
        None,
    );
    assert_eq!(refreshed.peek("covered").unwrap(), 1);
    refreshed.refresh().unwrap();

    for mut ctx in [ready, or_failed, refreshed] {
        assert!(ctx.is_ready());
        assert!(!ctx.is_failed());
        assert_eq!(ctx.peek("covered").unwrap(), 0);
        assert_eq!(
            ctx.peek_variable_value("color", "default").unwrap(),
            "control"
        );
        assert_eq!(ctx.pending(), 0);
        assert_eq!(ctx.treatment("covered").unwrap(), 0);
        assert_eq!(ctx.variable_value("color", "default").unwrap(), "control");
        let exposures = ctx.get_publish_params().exposures.unwrap();
        assert_eq!(exposures.len(), 1);
        assert_eq!(exposures[0].id, 11);
        assert_eq!(exposures[0].variant, 0);
    }
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "GET")
            .count(),
        3
    );
}
