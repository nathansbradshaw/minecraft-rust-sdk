pub use minecraft_sdk_core::*;
pub mod menu;
pub mod navigation;
#[cfg(test)]
mod perform_tests;
use reqwest::{Client as Http, Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub endpoint: String,
    pub instance_id: String,
    pub credential: Credential,
    pub session: Option<Session>,
}
pub struct Client {
    expected_instance: Option<String>,
    local_arbiter: Option<std::path::PathBuf>,
    endpoint: Url,
    http: Http,
    credential: Option<Credential>,
    session: Option<Session>,
}
impl Client {
    pub fn new(endpoint: &str) -> Result<Self> {
        let endpoint = Url::parse(endpoint)
            .map_err(|_| ApiError::new(ErrorCode::InvalidRequest, "Invalid SDK URL"))?;
        let local = endpoint.host_str().is_some_and(|s| {
            s == "localhost"
                || s.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if (endpoint.scheme() != "https" && !(local && endpoint.scheme() == "http"))
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.path().ends_with('/')
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Use an HTTPS gateway base URL ending in / (HTTP allowed only on loopback)",
            ));
        }
        let builder = Http::builder()
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        let builder = if local { builder.no_proxy() } else { builder };
        let http = builder
            .build()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Cannot create HTTP client"))?;
        Ok(Self {
            expected_instance: None,
            local_arbiter: std::env::var_os("MC_SDK_ARBITER").map(std::path::PathBuf::from),
            endpoint,
            http,
            credential: None,
            session: None,
        })
    }
    pub fn with_profile(profile: &Profile) -> Result<Self> {
        let mut client = Self::new(&profile.endpoint)?;
        client.expected_instance = Some(profile.instance_id.clone());
        client.credential = Some(profile.credential.clone());
        client.session = profile.session.clone();
        Ok(client)
    }
    pub fn set_session(&mut self, session: Option<Session>) {
        self.session = session;
    }
    /// For the container's own arbiter/body. This changes local scheduling only,
    /// never remote authentication or ordinary server permissions.
    pub fn without_local_arbiter(mut self) -> Self {
        self.local_arbiter = None;
        self
    }
    fn bot_path(&self, suffix: &str) -> Result<String> {
        let bot = &self
            .credential
            .as_ref()
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized, "Load a bot credential"))?
            .bot_id;
        if !valid_id(bot) {
            return Err(ApiError::new(ErrorCode::InvalidRequest, "Invalid bot ID"));
        }
        Ok(format!("v1/bots/{bot}/{suffix}"))
    }
    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let mut request = self.http.request(
            method,
            self.endpoint
                .join(path)
                .map_err(|_| ApiError::new(ErrorCode::InvalidRequest, "Invalid API path"))?,
        );
        // Discovery (including instance verification) and registration are public.
        // Do not disclose an existing key/session to a replacement instance.
        if path.starts_with("v1/bots/") {
            if let Some(c) = &self.credential {
                request = request.bearer_auth(&c.key);
            }
            if let Some(s) = &self.session {
                request = request.header("x-minecraft-session", &s.session_id);
            }
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request.send().await.map_err(|_| {
            ApiError::new(
                ErrorCode::UncertainOutcome,
                "Gateway request failed; query action status before retrying",
            )
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            ApiError::new(ErrorCode::UncertainOutcome, "Incomplete gateway response")
        })? {
            let limit = if path.ends_with("/observations/query") {
                8 * 1_048_576
            } else {
                1_048_576
            };
            if bytes.len() + chunk.len() > limit {
                return Err(ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Gateway response too large",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("Gateway returned HTTP {}", status.as_u16()),
                )
            }));
        }
        if bytes.is_empty() {
            bytes = b"null".to_vec();
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidRequest,
                "Gateway response does not match API contract",
            )
        })
    }
    pub async fn discover(&self) -> Result<ServerInfo> {
        let info: ServerInfo = self.request(Method::GET, "v1/server", None).await?;
        if info.api_version != API_VERSION {
            return Err(ApiError::new(
                ErrorCode::UnsupportedVersion,
                "Unsupported gateway API version",
            ));
        }
        Ok(info)
    }
    pub async fn verify_instance(&self, expected: &str) -> Result<ServerInfo> {
        let info = self.discover().await?;
        if info.instance_id != expected {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Gateway instance changed; operator review required",
            ));
        }
        Ok(info)
    }
    pub async fn register(&self, name: &str) -> Result<Credential> {
        self.request(
            Method::POST,
            "v1/bots",
            Some(serde_json::json!({"name":name})),
        )
        .await
    }
    pub async fn rotate_key(&self) -> Result<Credential> {
        self.request(Method::POST, &self.bot_path("key/rotate")?, None)
            .await
    }
    pub async fn connect(&self, request_id: &str) -> Result<Session> {
        self.request(
            Method::POST,
            &self.bot_path("session")?,
            Some(serde_json::json!({"request_id":request_id})),
        )
        .await
    }
    pub async fn disconnect(&self) -> Result<()> {
        self.request(Method::DELETE, &self.bot_path("session")?, None)
            .await
    }
    pub async fn observe(&self) -> Result<Observation> {
        self.request(Method::GET, &self.bot_path("observations")?, None)
            .await
    }
    pub async fn read_observation(&self, query: &ObservationQuery) -> Result<serde_json::Value> {
        query.validate()?;
        self.request(
            Method::POST,
            &self.bot_path("observations/query")?,
            Some(serde_json::to_value(query).map_err(|_| {
                ApiError::new(ErrorCode::InvalidRequest, "Invalid observation query")
            })?),
        )
        .await
    }
    pub async fn act(&self, request: &ActionRequest) -> Result<Receipt> {
        request.action.validate()?;
        if let Some(path) = &self.local_arbiter {
            return local_action(path, request).await;
        }
        self.request(
            Method::POST,
            &self.bot_path("actions")?,
            Some(
                serde_json::to_value(request)
                    .map_err(|_| ApiError::new(ErrorCode::InvalidRequest, "Invalid action"))?,
            ),
        )
        .await
    }
    pub async fn action_status(&self, id: &str) -> Result<Receipt> {
        if !valid_id(id) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid action ID",
            ));
        }
        self.request(Method::GET, &self.bot_path(&format!("actions/{id}"))?, None)
            .await
    }
    /// Dispatch exactly once, then inspect a running action. A caller's wait
    /// deadline returns its latest receipt; it never cancels or replays gameplay.
    pub async fn perform(&self, request: &ActionRequest, wait: Duration) -> Result<Receipt> {
        let until = tokio::time::Instant::now()
            .checked_add(wait)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Wait duration exceeds monotonic clock bounds",
                )
            })?;
        let receipt = self.act(request).await?;
        self.wait_receipt(&request.action_id, receipt, until).await
    }
    /// Observe an already dispatched ID without resubmitting it.
    pub async fn wait_for_action(&self, id: &str, wait: Duration) -> Result<Receipt> {
        let until = tokio::time::Instant::now()
            .checked_add(wait)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Wait duration exceeds monotonic clock bounds",
                )
            })?;
        let receipt = self.action_status(id).await?;
        self.wait_receipt(id, receipt, until).await
    }
    async fn wait_receipt(
        &self,
        id: &str,
        mut receipt: Receipt,
        until: tokio::time::Instant,
    ) -> Result<Receipt> {
        if receipt.action_id != id {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Action receipt identity changed",
            ));
        }
        while receipt.outcome == Outcome::Running && tokio::time::Instant::now() < until {
            tokio::time::sleep(
                Duration::from_millis(100)
                    .min(until.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
            receipt = self.action_status(id).await?;
            if receipt.action_id != id {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "Action status identity changed",
                ));
            }
        }
        Ok(receipt)
    }
    pub async fn pending_actions(&self) -> Result<Vec<PendingAction>> {
        self.request(Method::GET, &self.bot_path("actions/pending")?, None)
            .await
    }
    pub async fn reconcile(&self, id: &str) -> Result<Receipt> {
        if !valid_id(id) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid action ID",
            ));
        }
        self.request(
            Method::POST,
            &self.bot_path(&format!("actions/{id}/reconcile"))?,
            None,
        )
        .await
    }
}
#[cfg(unix)]
async fn local_action(path: &std::path::Path, request: &ActionRequest) -> Result<Receipt> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let unavailable = || {
        ApiError::new(
            ErrorCode::NotReady,
            "Container action arbiter unavailable; do not bypass it",
        )
    };
    let m = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !path.is_absolute() || !m.file_type().is_socket() || m.permissions().mode() & 0o077 != 0 {
        return Err(unavailable());
    }
    let exchange = async {
        let mut socket = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|_| unavailable())?;
        if socket.peer_cred().map_err(|_| unavailable())?.uid() != m.uid() {
            return Err(unavailable());
        }
        let mut payload = serde_json::to_vec(&LocalActionRequest {
            version: LOCAL_CONTROL_VERSION,
            request: request.clone(),
        })
        .map_err(|_| unavailable())?;
        if payload.len() > 16384 {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Local action exceeds budget",
            ));
        }
        payload.push(b'\n');
        socket
            .write_all(&payload)
            .await
            .map_err(|_| unavailable())?;
        let mut input = tokio::io::BufReader::new(socket);
        let mut bytes = Vec::new();
        loop {
            let buf = input.fill_buf().await.map_err(|_| unavailable())?;
            if buf.is_empty() {
                return Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Local action reply lost; inspect the original ID without replay",
                ));
            }
            let count = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |n| n + 1);
            if bytes.len() + count > 1_048_576 {
                return Err(unavailable());
            }
            let done = buf[count - 1] == b'\n';
            bytes.extend_from_slice(&buf[..count]);
            input.consume(count);
            if done {
                break;
            }
        }
        match serde_json::from_slice(&bytes).map_err(|_| unavailable())? {
            LocalActionResponse::Receipt(r) if r.action_id == request.action_id => Ok(r),
            LocalActionResponse::Error(e) => Err(e),
            _ => Err(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Local action correlation mismatch",
            )),
        }
    };
    tokio::time::timeout(Duration::from_secs(300), exchange)
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::UncertainOutcome,
                "Local action timed out; inspect its original ID",
            )
        })?
}
#[cfg(not(unix))]
async fn local_action(_: &std::path::Path, _: &ActionRequest) -> Result<Receipt> {
    Err(ApiError::new(
        ErrorCode::UnsupportedCapability,
        "Local Unix action arbitration is unavailable on this platform",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_actions_send_only_typed_intent_and_reject_crossed_receipts() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!(
            "sdk-arbiter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let path = dir.join("a.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio::io::BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            let req: LocalActionRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(req.version, LOCAL_CONTROL_VERSION);
            assert_eq!(req.request.action_id, "own-action");
            assert!(!line.contains("credential"));
            let response = LocalActionResponse::Receipt(Receipt {
                action_id: "foreign-action".into(),
                outcome: Outcome::Observed,
                adapter_version: "fixture".into(),
                evidence: serde_json::Value::Null,
                error: None,
            });
            socket
                .get_mut()
                .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
        });
        let error = local_action(
            &path,
            &ActionRequest {
                action_id: "own-action".into(),
                action: Action::Chat {
                    message: "hello".into(),
                },
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UncertainOutcome);
        peer.await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        assert!(serde_json::from_str::<LocalActionRequest>(r#"{"version":1,"actor":"other","request":{"action_id":"x","action":{"kind":"respawn"}}}"#).is_err());
    }
    #[tokio::test]
    async fn instance_verification_never_discloses_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 8192);
                header.push(stream.read_u8().await.unwrap());
            }
            let body = serde_json::to_vec(&ServerInfo {
                api_version: API_VERSION,
                instance_id: "replacement".into(),
                minecraft_version: "fixture".into(),
                platform: "fixture".into(),
                adapter_version: "fixture".into(),
                ready: true,
                capabilities: vec![],
                limits: serde_json::json!({}),
            })
            .unwrap();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            String::from_utf8(header).unwrap().to_ascii_lowercase()
        });
        let client = Client::with_profile(&Profile {
            endpoint,
            instance_id: "original".into(),
            credential: Credential {
                bot_id: "fixture".into(),
                player_uuid: "fixture".into(),
                key: "private-test-key".into(),
            },
            session: Some(Session {
                session_id: "private-test-session".into(),
                adapter_version: "fixture".into(),
            }),
        })
        .unwrap();
        let error = client.verify_instance("original").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        let header = peer.await.unwrap();
        assert!(header.starts_with("get /v1/server "));
        assert!(!header.contains("authorization:"));
        assert!(!header.contains("x-minecraft-session:"));
        assert!(!header.contains("private-test-"));
    }
    #[tokio::test]
    async fn refuses_credential_redirect_and_insecure_remote_origins() {
        for url in [
            "http://server.example/",
            "https://user:secret@example.com/",
            "https://example.com/?key=foo",
            "https://example.com/private",
        ] {
            assert!(Client::new(url).is_err());
        }
        assert!(Client::new("http://127.0.0.1:8080/").is_ok());
        assert!(Client::new("https://example.com/").is_ok());
    }
}
