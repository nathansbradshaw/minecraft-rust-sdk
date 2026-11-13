//! Operator-only recovery fixture: reserve at most one durable Sol HTTP502, then forward
//! genuine requests to a fixed loopback broker. Never fabricates model choices.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use minecraft_sdk_brain::{Result, digest, private_read, private_write};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    net::SocketAddr,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const LIMIT: usize = 131072;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: SocketAddr,
    upstream: String,
    key_file: PathBuf,
    marker: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    model: String,
    injected_at_ms: u128,
}
struct Fixture {
    _lock: File,
    upstream: String,
    auth_hash: String,
    marker: PathBuf,
    spent: Mutex<bool>,
    client: reqwest::Client,
}
fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system UTC")
        .as_millis()
}
fn valid_origin(cfg: &Config) -> bool {
    let Ok(url) = reqwest::Url::parse(&cfg.upstream) else {
        return false;
    };
    cfg.listen.ip().is_loopback()
        && cfg.listen.port() != 0
        && url.scheme() == "http"
        && url
            .host_str()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
        && url
            .port_or_known_default()
            .is_some_and(|p| p != cfg.listen.port())
        && url.path() == "/model/v1/chat"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}
impl Fixture {
    fn open(cfg: Config) -> Result<Self> {
        if !valid_origin(&cfg) {
            return Err("Fixture requires distinct numeric loopback listener/broker origins and fixed model path".into());
        }
        let bytes = private_read(&cfg.key_file, 256)?;
        let key = std::str::from_utf8(&bytes)?.trim();
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid allocated key".into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(cfg.marker.with_extension("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Fixture marker is already owned by another process".into());
        }
        let spent = if cfg.marker.exists() {
            let value: Marker = serde_json::from_slice(&private_read(&cfg.marker, 1024)?)?;
            if value.version != 1 || value.model != "gpt-6.1-sol" {
                return Err("Unknown fault marker".into());
            }
            true
        } else {
            false
        };
        Ok(Self {
            _lock: lock,
            upstream: cfg.upstream,
            auth_hash: digest(format!("Bearer {key}").as_bytes()),
            marker: cfg.marker,
            spent: Mutex::new(spent),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(240))
                .build()?,
        })
    }
    fn inject(&self, model: &str) -> Result<bool> {
        let mut spent = self
            .spent
            .lock()
            .map_err(|_| "Fault marker lock unavailable")?;
        if *spent || model != "gpt-6.1-sol" {
            return Ok(false);
        }
        // Persist before replying. A crash/restart cannot refund this injection.
        let marker = Marker {
            version: 1,
            model: model.into(),
            injected_at_ms: now_ms(),
        };
        private_write(&self.marker, &serde_json::to_vec(&marker)?)?;
        *spent = true;
        Ok(true)
    }
}
fn answer(status: StatusCode, value: Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().expect("constant"));
    response
}
fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |n, (a, b)| n | (a ^ b)) == 0
}
async fn handle(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({"error":"fixture_auth_required"}),
        );
    };
    if !equal(&digest(auth.as_bytes()), &state.auth_hash) {
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({"error":"fixture_auth_required"}),
        );
    }
    let model = value["model"].as_str().unwrap_or("");
    if !matches!(model, "gpt-6-luna" | "gpt-6.1-sol") {
        return answer(
            StatusCode::BAD_REQUEST,
            json!({"error":"fixture_model_invalid"}),
        );
    }
    match state.inject(model) {
        Ok(true) => {
            println!(
                "{}",
                json!({"event":"evaluation_fault_injected","at_ms":now_ms(),"model":model,"body_sha256":digest(value.to_string().as_bytes()),"provider_admission":false,"scope":"controlled HTTP failure; not a real provider outage"})
            );
            return answer(
                StatusCode::BAD_GATEWAY,
                json!({"error":"model_unavailable_no_retry"}),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return answer(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error":"fixture_marker_failure"}),
            );
        }
    }
    let mut auth_value =
        reqwest::header::HeaderValue::from_str(auth).expect("validated HTTP header");
    auth_value.set_sensitive(true);
    let started = now_ms();
    let result = async {
        let mut response = state
            .client
            .post(&state.upstream)
            .header("authorization", auth_value)
            .json(&value)
            .send()
            .await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > LIMIT {
                return Err("upstream response budget".into());
            }
            bytes.extend(chunk);
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((status, bytes))
    }
    .await;
    match result {
        Ok((status, bytes)) => {
            println!(
                "{}",
                json!({"event":"evaluation_model_forwarded","started_at_ms":started,"finished_at_ms":now_ms(),"model":model,"status":status.as_u16(),"response_sha256":digest(&bytes),"provider_admission":"broker ledger is authoritative"})
            );
            let mut response = (status, bytes).into_response();
            response.headers_mut().insert(
                "content-type",
                "application/json".parse().expect("constant"),
            );
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().expect("constant"));
            response
        }
        Err(_) => {
            println!(
                "{}",
                json!({"event":"evaluation_forward_failed","started_at_ms":started,"finished_at_ms":now_ms(),"model":model,"provider_admission":"unknown; broker ledger is authoritative","error":"fixture_upstream_failure"})
            );
            answer(
                StatusCode::BAD_GATEWAY,
                json!({"error":"fixture_upstream_failure"}),
            )
        }
    }
}
fn router(state: Arc<Fixture>) -> Router {
    Router::new()
        .route("/model/v1/chat", post(handle))
        .layer(DefaultBodyLimit::max(LIMIT))
        .with_state(state)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        return Err(
            "Usage: model_fault_proxy PRIVATE_CONFIG.json (operator evaluation only)".into(),
        );
    }
    let cfg: Config = serde_json::from_slice(&private_read(std::path::Path::new(&args[0]), 4096)?)?;
    let address = cfg.listen;
    let state = Arc::new(Fixture::open(cfg)?);
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "{}",
        json!({"event":"evaluation_fault_proxy_ready","listen":address,"fault_already_consumed":*state.spent.lock().map_err(|_| "marker lock")?,"at_ms":now_ms(),"scope":"operator-only synthetic error; real upstream responses"})
    );
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(dir: &std::path::Path, upstream: String) -> Config {
        Config {
            listen: "127.0.0.1:19138".parse().unwrap(),
            upstream,
            key_file: dir.join("key"),
            marker: dir.join("marker"),
        }
    }
    #[test]
    fn closed_origins() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), "http://127.0.0.1:19137/model/v1/chat".into());
        assert!(valid_origin(&cfg));
        for upstream in [
            "https://example.org/model/v1/chat",
            "http://192.168.1.2/model/v1/chat",
            "http://127.0.0.1:19138/model/v1/chat",
            "http://user@127.0.0.1:19137/model/v1/chat",
            "http://127.0.0.1:19137/model/v1/chat?x=1",
        ] {
            cfg.upstream = upstream.into();
            assert!(!valid_origin(&cfg));
        }
    }
    #[test]
    fn restart_retains_consumed_fault_and_excludes_other_owner() {
        let dir = tempfile::tempdir().unwrap();
        private_write(&dir.path().join("key"), "a".repeat(64).as_bytes()).unwrap();
        let upstream = "http://127.0.0.1:19137/model/v1/chat".to_owned();
        let first = Fixture::open(config(dir.path(), upstream.clone())).unwrap();
        assert!(first.inject("gpt-6.1-sol").unwrap());
        assert!(Fixture::open(config(dir.path(), upstream.clone())).is_err());
        drop(first);
        let restarted = Fixture::open(config(dir.path(), upstream)).unwrap();
        assert!(!restarted.inject("gpt-6.1-sol").unwrap());
    }
    #[tokio::test]
    async fn authenticated_durable_fault_then_real_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let key = "a".repeat(64);
        private_write(&dir.path().join("key"), key.as_bytes()).unwrap();
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!(
            "http://{}/model/v1/chat",
            upstream_listener.local_addr().unwrap()
        );
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new().route(
                    "/model/v1/chat",
                    post(move || {
                        let counter = counter.clone();
                        async move {
                            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Json(json!({"upstream_test_response":true}))
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let fixture = Arc::new(Fixture::open(config(dir.path(), upstream.clone())).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model/v1/chat", listener.local_addr().unwrap());
        let fixture_task = tokio::spawn(async move {
            axum::serve(listener, router(fixture)).await.unwrap();
        });
        let client = reqwest::Client::new();
        let call = |token: String, model: &str| {
            client
                .post(&url)
                .bearer_auth(token)
                .json(&json!({"model":model}))
        };
        assert_eq!(
            call("b".repeat(64), "gpt-6.1-sol")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            call(key.clone(), "gpt-6-luna")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let injected = call(key.clone(), "gpt-6.1-sol").send().await.unwrap();
        assert_eq!(injected.status(), 502);
        assert_eq!(
            injected.json::<Value>().await.unwrap()["error"],
            "model_unavailable_no_retry"
        );
        assert_eq!(call(key, "gpt-6.1-sol").send().await.unwrap().status(), 200);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(Fixture::open(config(dir.path(), upstream)).is_err());
        fixture_task.abort();
        upstream_task.abort();
    }
}
