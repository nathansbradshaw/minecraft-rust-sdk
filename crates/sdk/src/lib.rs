pub use minecraft_sdk_core::*;
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
            || endpoint.path() != "/"
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Use an HTTPS gateway origin (HTTP allowed only on loopback)",
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
            endpoint,
            http,
            credential: None,
            session: None,
        })
    }
    pub fn with_profile(profile: &Profile) -> Result<Self> {
        let mut client = Self::new(&profile.endpoint)?;
        client.credential = Some(profile.credential.clone());
        client.session = profile.session.clone();
        Ok(client)
    }
    pub fn set_session(&mut self, session: Option<Session>) {
        self.session = session;
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
        if let Some(c) = &self.credential {
            request = request.bearer_auth(&c.key);
        }
        if let Some(s) = &self.session {
            request = request.header("x-minecraft-session", &s.session_id);
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
            if bytes.len() + chunk.len() > 1_048_576 {
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
    pub async fn act(&self, request: &ActionRequest) -> Result<Receipt> {
        request.action.validate()?;
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
#[cfg(test)]
mod tests {
    use super::*;
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
