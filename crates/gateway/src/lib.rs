pub mod adapter;
pub mod operator;
pub mod store;
use adapter::{Adapter, AdapterConfig};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use minecraft_sdk_core::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use store::{Actor, Store};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub journal: PathBuf,
    pub adapter: AdapterConfig,
    #[serde(default)]
    pub reserved_names: Vec<String>,
    pub registrations_per_minute: u32,
}
pub struct Gateway {
    store: Mutex<Store>,
    adapter: tokio::sync::Mutex<Adapter>,
    info: ServerInfo,
    registration_times: Mutex<VecDeque<Instant>>,
    registration_limit: u32,
    lanes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    health: Arc<std::sync::atomic::AtomicBool>,
}
type Shared = Arc<Gateway>;
struct HttpError(ApiError);
impl From<ApiError> for HttpError {
    fn from(e: ApiError) -> Self {
        Self(e)
    }
}
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ErrorCode::Unauthorized | ErrorCode::Revoked => StatusCode::UNAUTHORIZED,
            ErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Conflict | ErrorCode::SessionRequired | ErrorCode::UncertainOutcome => {
                StatusCode::CONFLICT
            }
            ErrorCode::UnsupportedCapability | ErrorCode::UnsupportedVersion => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ErrorCode::NotReady | ErrorCode::ServerFull => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
        };
        (status, Json(self.0)).into_response()
    }
}
impl Gateway {
    pub async fn start(config: Config) -> Result<Shared> {
        if !config.listen.ip().is_loopback() {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Bind to loopback behind a TLS reverse proxy",
            ));
        }
        let store = Store::open(&config.journal)?;
        for name in &config.reserved_names {
            store.reserve(name)?;
        }
        let mut adapter = Adapter::start(&config.adapter)
            .map_err(|_| ApiError::new(ErrorCode::NotReady, "Cannot start configured adapter"))?;
        let response = adapter
            .request(&AdapterRequest {
                protocol: ADAPTER_VERSION,
                instance_id: store.instance_id.clone(),
                bot_id: String::new(),
                player_uuid: String::new(),
                player_name: String::new(),
                operation: AdapterOperation::Discover,
            })
            .await?;
        let AdapterResponse::Info(mut info) = response else {
            return Err(ApiError::new(
                ErrorCode::UnsupportedVersion,
                "Adapter did not advertise its contract",
            ));
        };
        if info.api_version != API_VERSION
            || info.adapter_version.is_empty()
            || !info.capabilities.iter().all(|s| registry_id(s))
        {
            return Err(ApiError::new(
                ErrorCode::UnsupportedVersion,
                "Incompatible adapter",
            ));
        }
        info.instance_id = store.instance_id.clone();
        let health = adapter.health.clone();
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            adapter: tokio::sync::Mutex::new(adapter),
            info,
            registration_times: Mutex::new(VecDeque::new()),
            registration_limit: config.registrations_per_minute,
            lanes: Mutex::new(HashMap::new()),
            health,
        }))
    }
    fn db(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Journal unavailable"))
    }
    fn actor(&self, bot: &str, headers: &HeaderMap) -> Result<Actor> {
        let key = headers
            .get("authorization")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized, "Bot key required"))?;
        self.db()?.authenticate(bot, key)
    }
    fn lane(&self, bot: &str) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let mut lanes = self
            .lanes
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Controller lock unavailable"))?;
        Ok(lanes
            .entry(bot.into())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }
    fn controller(&self, actor: &Actor, headers: &HeaderMap) -> Result<String> {
        let session = headers
            .get("x-minecraft-session")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("");
        self.db()?.session(&actor.bot_id, session)
    }
    async fn call(
        self: &Arc<Self>,
        actor: &Actor,
        operation: AdapterOperation,
    ) -> Result<AdapterResponse> {
        let request = AdapterRequest {
            protocol: ADAPTER_VERSION,
            instance_id: self.info.instance_id.clone(),
            bot_id: actor.bot_id.clone(),
            player_uuid: actor.uuid.clone(),
            player_name: actor.name.clone(),
            operation,
        };
        let gateway = self.clone();
        // Drain exactly one reply even if its HTTP client disconnects. Otherwise an
        // orphaned observation could be consumed by another character's request.
        tokio::spawn(async move { gateway.adapter.lock().await.request(&request).await })
            .await
            .map_err(|_| {
                ApiError::new(ErrorCode::UncertainOutcome, "Adapter exchange interrupted")
            })?
    }
    fn ready(&self) -> Result<()> {
        if self.info.ready && self.health.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err(ApiError::new(
                ErrorCode::NotReady,
                "Server integration is not ready",
            ))
        }
    }
}
pub fn router(gateway: Shared) -> Router {
    Router::new()
        .route("/v1/server", get(discover))
        .route("/v1/bots", post(register))
        .route("/v1/bots/{bot}/key/rotate", post(rotate))
        .route("/v1/bots/{bot}/session", post(connect).delete(disconnect))
        .route("/v1/bots/{bot}/observations", get(observe))
        .route("/v1/bots/{bot}/actions", post(action))
        .route("/v1/bots/{bot}/actions/{id}", get(status))
        .route("/v1/bots/{bot}/actions/{id}/reconcile", post(reconcile))
        .layer(DefaultBodyLimit::max(16384))
        .with_state(gateway)
}
async fn discover(State(g): State<Shared>) -> Json<ServerInfo> {
    let mut info = g.info.clone();
    info.ready &= g.health.load(std::sync::atomic::Ordering::Acquire);
    Json(info)
}
async fn register(
    State(g): State<Shared>,
    Json(request): Json<Registration>,
) -> std::result::Result<Json<Credential>, HttpError> {
    g.ready()?;
    let mut times = g
        .registration_times
        .lock()
        .map_err(|_| ApiError::new(ErrorCode::Internal, "Registration unavailable"))?;
    let now = Instant::now();
    while times
        .front()
        .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
    {
        times.pop_front();
    }
    if times.len() >= g.registration_limit as usize {
        return Err(ApiError::new(
            ErrorCode::RateLimited,
            "Registration rate exceeded; retry later",
        )
        .into());
    }
    times.push_back(now);
    Ok(Json(g.db()?.register(&request)?))
}
async fn rotate(
    State(g): State<Shared>,
    Path(bot): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<Json<Credential>, HttpError> {
    g.actor(&bot, &headers)?;
    let _guard = g.lane(&bot)?.lock_owned().await;
    let actor = g.actor(&bot, &headers)?;
    Ok(Json(g.db()?.rotate(&actor)?))
}
async fn connect(
    State(g): State<Shared>,
    Path(bot): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SessionRequest>,
) -> std::result::Result<Json<Session>, HttpError> {
    g.actor(&bot, &headers)?;
    let guard = g.lane(&bot)?.lock_owned().await;
    let actor = g.actor(&bot, &headers)?;
    g.ready()?;
    let (session, new) = g.db()?.connect(&actor, &request, &g.info.adapter_version)?;
    if new {
        // Complete independently of a client dropping the HTTP connection.
        let task = tokio::spawn(async move {
            let _guard = guard;
            match g.call(&actor, AdapterOperation::Connect).await {
                Ok(AdapterResponse::Connected) => {
                    g.db()?.connected(&bot)?;
                    Ok(session)
                }
                Ok(AdapterResponse::Error(e)) => {
                    g.db()?.disconnect(&bot)?;
                    Err(e)
                }
                _ => Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Connection outcome unknown; operator reconciliation required",
                )),
            }
        });
        Ok(Json(task.await.map_err(|_| {
            ApiError::new(ErrorCode::Internal, "Connection task failed")
        })??))
    } else {
        Ok(Json(session))
    }
}
async fn disconnect(
    State(g): State<Shared>,
    Path(bot): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<StatusCode, HttpError> {
    g.actor(&bot, &headers)?;
    let guard = g.lane(&bot)?.lock_owned().await;
    let actor = g.actor(&bot, &headers)?;
    g.controller(&actor, &headers)?;
    let task = tokio::spawn(async move {
        let _guard = guard;
        match g.call(&actor, AdapterOperation::Disconnect).await? {
            AdapterResponse::Disconnected => {
                g.db()?.disconnect(&bot)?;
                Ok(())
            }
            AdapterResponse::Error(e) => Err(e),
            _ => Err(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Disconnect unconfirmed",
            )),
        }
    });
    task.await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "Disconnect task failed"))??;
    Ok(StatusCode::NO_CONTENT)
}
async fn observe(
    State(g): State<Shared>,
    Path(bot): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<Json<Observation>, HttpError> {
    let actor = g.actor(&bot, &headers)?;
    g.controller(&actor, &headers)?;
    match g.call(&actor, AdapterOperation::Observe).await? {
        AdapterResponse::Observation(o) if o.player["uuid"] == actor.uuid => Ok(Json(o)),
        AdapterResponse::Error(e) => Err(e.into()),
        _ => Err(ApiError::new(ErrorCode::Internal, "Unexpected adapter reply").into()),
    }
}
async fn action(
    State(g): State<Shared>,
    Path(bot): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ActionRequest>,
) -> std::result::Result<Json<Receipt>, HttpError> {
    g.actor(&bot, &headers)?;
    let guard = g.lane(&bot)?.lock_owned().await;
    let actor = g.actor(&bot, &headers)?;
    let adapter = g.controller(&actor, &headers)?;
    request.action.validate()?;
    if !g
        .info
        .capabilities
        .iter()
        .any(|s| s == request.action.capability())
    {
        return Err(ApiError::new(
            ErrorCode::UnsupportedCapability,
            "Action capability is not installed",
        )
        .into());
    }
    let (receipt, new) = g.db()?.prepare(&bot, &request, &adapter)?;
    if !new {
        return Ok(Json(receipt));
    }
    let task = tokio::spawn(async move {
        let _guard = guard;
        let response = g
            .call(
                &actor,
                AdapterOperation::Action {
                    request: request.clone(),
                },
            )
            .await;
        let receipt = match response {
            Ok(AdapterResponse::Receipt(r))
                if r.action_id == request.action_id
                    && r.adapter_version == adapter
                    && r.outcome != Outcome::Prepared =>
            {
                r
            }
            Ok(AdapterResponse::Error(e)) => Receipt {
                action_id: request.action_id.clone(),
                outcome: if e.code == ErrorCode::UncertainOutcome {
                    Outcome::Uncertain
                } else {
                    Outcome::Rejected
                },
                adapter_version: adapter,
                evidence: serde_json::Value::Null,
                error: Some(e),
            },
            _ => Receipt {
                action_id: request.action_id.clone(),
                outcome: Outcome::Uncertain,
                adapter_version: adapter,
                evidence: serde_json::Value::Null,
                error: Some(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Action may have occurred; reconcile without replay",
                )),
            },
        };
        g.db()?.save_receipt(&bot, &request.action_id, &receipt)?;
        Ok::<_, ApiError>(receipt)
    });
    Ok(Json(task.await.map_err(|_| {
        ApiError::new(ErrorCode::Internal, "Action task failed")
    })??))
}
async fn status(
    State(g): State<Shared>,
    Path((bot, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Receipt>, HttpError> {
    g.actor(&bot, &headers)?;
    Ok(Json(g.db()?.receipt(&bot, &id)?.ok_or_else(|| {
        ApiError::new(ErrorCode::NotFound, "Unknown action ID")
    })?))
}
async fn reconcile(
    State(g): State<Shared>,
    Path((bot, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Receipt>, HttpError> {
    g.actor(&bot, &headers)?;
    let guard = g.lane(&bot)?.lock_owned().await;
    let actor = g.actor(&bot, &headers)?;
    let adapter = g.controller(&actor, &headers)?;
    let old = g
        .db()?
        .receipt(&bot, &id)?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "Unknown action ID"))?;
    if matches!(
        old.outcome,
        Outcome::Observed | Outcome::Rejected | Outcome::Reconciled | Outcome::Imported
    ) {
        return Ok(Json(old));
    }
    let task = tokio::spawn(async move {
        let _guard = guard;
        match g
            .call(
                &actor,
                AdapterOperation::Reconcile {
                    action_id: id.clone(),
                },
            )
            .await?
        {
            AdapterResponse::Receipt(r)
                if r.action_id == id
                    && r.adapter_version == adapter
                    && r.outcome != Outcome::Prepared =>
            {
                g.db()?.save_receipt(&bot, &id, &r)?;
                Ok(r)
            }
            AdapterResponse::Error(e) => Err(e),
            _ => Err(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Reconciliation unavailable",
            )),
        }
    });
    Ok(Json(task.await.map_err(|_| {
        ApiError::new(ErrorCode::Internal, "Reconciliation task failed")
    })??))
}
