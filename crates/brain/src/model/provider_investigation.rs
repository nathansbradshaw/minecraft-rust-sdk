//! One durable provider-error investigation per material controller scope.
use super::HttpFailure;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderFailure {
    pub status: u16,
    pub class: String,
}
impl ProviderFailure {
    pub fn from_error(error: &(dyn std::error::Error + Send + Sync + 'static)) -> Option<Self> {
        let failure = error.downcast_ref::<HttpFailure>()?;
        ((500..=599).contains(&failure.status)
            && matches!(
                failure.class.as_str(),
                "provider_failed"
                    | "provider_unavailable"
                    | "provider_timeout"
                    | "model_unavailable_no_retry"
            ))
        .then(|| Self {
            status: failure.status,
            class: failure.class.clone(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Episode {
    /// Material goal/source/worker binding supplied by the controller, never
    /// observation revisions, ticks or newly formatted context hashes.
    scope: String,
    fingerprint: String,
    original: ProviderFailure,
    original_sol: bool,
    alternate_pending: bool,
    exhausted: bool,
    #[serde(default)]
    accepted: bool,
}
impl Episode {
    pub fn same_scope(&self, scope: &str) -> bool {
        self.scope == scope
    }
    /// The caller MUST durably save this consumed lease before any request.
    /// A new object is allowed only after a documented material scope change.
    pub fn reserve(scope: String, failure: ProviderFailure, original_sol: bool) -> Self {
        let fingerprint = crate::digest(
            json!({"scope":scope,"status":failure.status,
            "class":failure.class,"original_sol":original_sol})
            .to_string()
            .as_bytes(),
        );
        Self {
            scope,
            fingerprint,
            original: failure,
            original_sol,
            alternate_pending: true,
            exhausted: false,
            accepted: false,
        }
    }
    pub fn alternate(&self, scope: &str) -> Option<bool> {
        (self.scope == scope && self.alternate_pending && !self.exhausted)
            .then_some(!self.original_sol)
    }
    /// Call before the alternate request and durably save again. Restarts or
    /// another differently classified failure cannot grant a second dispatch.
    pub fn consume_dispatch(&mut self, scope: &str) -> Option<bool> {
        let role = self.alternate(scope)?;
        self.alternate_pending = false;
        self.exhausted = true;
        Some(role)
    }
    pub fn context(&self) -> Value {
        json!({"mode":"provider_error_investigation","fingerprint":self.fingerprint,
            "original_failure":self.original,"original_role":if self.original_sol{"sol"}else{"luna"},
            "task":"Investigate the retained provider error and choose useful work from current facts. Do not repeat the original inference request or change allowances.",
            "allowance_changes":false,"remaining_alternate_dispatches":u8::from(self.alternate_pending && !self.exhausted)})
    }
}

pub(crate) enum Route {
    Normal,
    Healthy(bool),
    Blocked,
}
fn load(journal: &crate::Journal) -> crate::Result<Option<Episode>> {
    journal
        .get("provider_investigation_episode")?
        .map(|saved| serde_json::from_str(&saved))
        .transpose()
        .map_err(Into::into)
}
pub(crate) fn route(journal: &crate::Journal, scope: &str) -> crate::Result<Route> {
    Ok(
        match load(journal)?.filter(|episode| episode.same_scope(scope)) {
            None => Route::Normal,
            Some(episode) if episode.accepted => Route::Healthy(!episode.original_sol),
            Some(_) => Route::Blocked,
        },
    )
}
pub(crate) fn accepted(journal: &crate::Journal, scope: &str) -> crate::Result<()> {
    let mut episode = load(journal)?.ok_or("Provider investigation episode missing")?;
    crate::ensure(
        episode.same_scope(scope) && episode.exhausted,
        "Provider investigation scope changed",
    )?;
    episode.accepted = true;
    journal.set(
        "provider_investigation_episode",
        &serde_json::to_string(&episode)?,
    )
}
pub(crate) fn block_existing(journal: &crate::Journal, scope: &str) -> crate::Result<bool> {
    let Some(mut episode) = load(journal)?.filter(|e| e.same_scope(scope)) else {
        return Ok(false);
    };
    episode.accepted = false;
    episode.alternate_pending = false;
    episode.exhausted = true;
    journal.set(
        "provider_investigation_episode",
        &serde_json::to_string(&episode)?,
    )?;
    Ok(true)
}

/// Persist exhaustion before transport/lane admission. No success, different
/// provider failure, ambient observation or process restart refunds this lease.
pub(crate) fn consume(
    journal: &crate::Journal,
    scope: String,
    failure: ProviderFailure,
    original_sol: bool,
) -> crate::Result<Option<(bool, Value)>> {
    if let Some(previous) = load(journal)?
        && previous.same_scope(&scope)
    {
        return Ok(None);
    }
    let mut episode = Episode::reserve(scope.clone(), failure, original_sol);
    let role = episode
        .consume_dispatch(&scope)
        .ok_or("Provider investigation lease unavailable")?;
    journal.set(
        "provider_investigation_episode",
        &serde_json::to_string(&episode)?,
    )?;
    let context = episode.context();
    journal.event(
        "provider_investigation_reserved",
        json!({"fingerprint":context["fingerprint"],
        "original_failure":context["original_failure"],"alternate_sol":role,
        "lease_consumed_before_request":true,"allowances_changed":false}),
    )?;
    Ok(Some((role, context)))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn reply(socket: &mut tokio::net::TcpStream, status: &str, value: Value) {
        use tokio::io::AsyncWriteExt;
        let body = value.to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn direct_controller_502_uses_changed_luna_context_then_pins_and_blocks_on_any_alternate_error()
     {
        use tokio::io::AsyncReadExt;
        let sdk = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_root, mut brain) =
            crate::admission_tests::fixture(&format!("http://{}/", sdk.local_addr().unwrap()));
        brain.cfg.workspace = _root.path().join("editable");
        std::fs::create_dir(&brain.cfg.workspace).unwrap();
        let refresh = std::sync::Arc::new(tokio::sync::Notify::new());
        let refreshed = refresh.clone();
        let sdk_task = tokio::spawn(async move {
            let (mut socket, _) = sdk.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            assert!(header.starts_with(b"GET /v1/bots/own/observations "));
            reply(
                &mut socket,
                "200 OK",
                json!({"revision":"during-alternate","player":{
                "uuid":"actor","entity_id":1,"life_id":"same-life","health":20.,"inventory":[],
                "dimension":"minecraft:overworld","position":[0.5,1.,0.5]},
                "world":{"game_time":10,"blocks":[],"entities":[],"chat":[]},"menu":{}}),
            )
            .await;
            refreshed.notify_one();
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model = crate::model::Model {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: format!("http://{}/chat", listener.local_addr().unwrap()),
        };
        let journal_path = _root.path().join("brain.sqlite");
        let upstream = tokio::spawn(async move {
            let mut contexts = Vec::new();
            for index in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = crate::model::tests::read_request(&mut socket).await;
                assert_eq!(
                    request["model"],
                    if index == 0 {
                        "gpt-6.1-sol"
                    } else {
                        "gpt-6-luna"
                    }
                );
                let context: Value =
                    serde_json::from_str(request["messages"][1]["content"].as_str().unwrap())
                        .unwrap();
                if index == 1 {
                    assert_ne!(context, contexts[0]);
                    assert_eq!(
                        context["provider_error_investigation"]["original_failure"]["status"],
                        502
                    );
                    assert_eq!(
                        context["provider_error_investigation"]["original_failure"]["class"],
                        "model_unavailable_no_retry"
                    );
                    assert_eq!(context["decision_session"]["role"], "luna");
                    let saved = {
                        let j = crate::Journal::open(&journal_path).unwrap();
                        j.get("provider_investigation_episode").unwrap().unwrap()
                    };
                    let episode: Episode = serde_json::from_str(&saved).unwrap();
                    assert!(
                        episode.exhausted && !episode.accepted && !episode.alternate_pending,
                        "lease must be durably consumed before request arrives"
                    );
                    refresh.notified().await;
                }
                contexts.push(context);
                if index == 0 {
                    reply(
                        &mut socket,
                        "502 Bad Gateway",
                        json!({"error":"model_unavailable_no_retry"}),
                    )
                    .await;
                } else if index == 1 {
                    let choice = json!({"tool":"report","input":"investigate retained provider failure",
                        "path":"","summary":"bounded investigation","notes":"","experiment":false});
                    reply(
                        &mut socket,
                        "200 OK",
                        json!({"done":true,"message":{"content":choice.to_string()}}),
                    )
                    .await;
                } else {
                    reply(
                        &mut socket,
                        "403 Forbidden",
                        json!({"error":"permission_denied"}),
                    )
                    .await;
                }
            }
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(75), listener.accept())
                    .await
                    .is_err(),
                "no fourth request or model ping-pong after alternate failure"
            );
        });
        let context = json!({"goal":"fixture","decision_session":{"role":"sol"}});
        let failure = model.choose_for(&context, true, true).await.unwrap_err();
        assert!(
            brain
                .investigate_provider_failure(&model, &context, true, failure.as_ref())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            brain.observed.revision, "during-alternate",
            "SDK refresh stays live while alternate inference waits"
        );
        let scope = brain.provider_scope().unwrap();
        assert_eq!(
            brain
                .allocated_reasoning_role(true, crate::now(), &scope)
                .unwrap(),
            Some(false)
        );
        brain
            .journal
            .set("luna_daily_reset", &(crate::now() + 1000).to_string())
            .unwrap();
        assert_eq!(
            brain
                .allocated_reasoning_role(true, crate::now(), &scope)
                .unwrap(),
            None,
            "an unavailable healthy alternate cannot fall back to the known failed role"
        );
        brain.journal.set("luna_daily_reset", "0").unwrap();
        brain.observed.revision = "ambient-next-tick".into();
        brain.observed.world["game_time"] = json!(999);
        assert_eq!(
            brain.provider_scope().unwrap(),
            scope,
            "ambient facts cannot reset the episode"
        );
        let role = brain
            .allocated_reasoning_role(true, crate::now(), &scope)
            .unwrap()
            .unwrap();
        let next_failure = model.choose_for(&context, role, true).await.unwrap_err();
        assert!(
            brain
                .investigate_provider_failure(&model, &context, role, next_failure.as_ref())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            brain
                .allocated_reasoning_role(true, crate::now(), &scope)
                .unwrap(),
            None
        );
        let reloaded = crate::Journal::open(&_root.path().join("brain.sqlite")).unwrap();
        assert!(matches!(route(&reloaded, &scope).unwrap(), Route::Blocked));
        assert_eq!(
            brain
                .allocated_reasoning_role(true, crate::now(), "material-new-goal")
                .unwrap(),
            Some(true)
        );
        sdk_task.await.unwrap();
        upstream.await.unwrap();
    }
    #[tokio::test]
    async fn aliased_state_root_cannot_refund_a_lease_from_journal_churn_or_start_recovery() {
        let (_root, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
        let scope = brain.provider_scope().unwrap();
        brain
            .journal
            .event("model_error", json!({"error":"original provider failure"}))
            .unwrap();
        brain.observed.revision = "ambient-changed".into();
        assert_eq!(brain.provider_scope().unwrap(), scope);
        let model = crate::model::Model {
            http: reqwest::Client::new(),
            endpoint: "http://127.0.0.1:1/never".into(),
        };
        let failure = error(502, "model_unavailable_no_retry");
        assert!(
            brain
                .investigate_provider_failure(&model, &json!({}), true, &failure)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(brain.provider_scope().unwrap(), scope);
        assert!(
            brain
                .journal
                .get("provider_investigation_episode")
                .unwrap()
                .is_none()
        );
    }
    fn error(status: u16, class: &str) -> HttpFailure {
        HttpFailure {
            status,
            class: class.into(),
            retry_at: None,
            wire: None,
            transport_attempts: 1,
        }
    }
    #[test]
    fn only_typed_transient_provider_failures_are_eligible() {
        assert!(ProviderFailure::from_error(&error(502, "provider_failed")).is_some());
        assert!(ProviderFailure::from_error(&error(502, "model_unavailable_no_retry")).is_some());
        for (status, class) in [
            (401, "provider_failed"),
            (429, "allocation_daily_limit"),
            (502, "authentication_error"),
            (503, "permission_denied"),
            (500, "invalid_schema"),
            (502, "quota_exceeded"),
            (503, "response_unavailable"),
        ] {
            assert!(ProviderFailure::from_error(&error(status, class)).is_none());
        }
        let opaque = std::io::Error::other("model_http_502: provider_failed");
        assert!(ProviderFailure::from_error(&opaque).is_none());
    }
    #[test]
    fn journal_consumption_pins_only_accepted_role_and_never_refunds_on_restart_or_new_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("brain.sqlite");
        let j = crate::Journal::open(&path).unwrap();
        let failure =
            ProviderFailure::from_error(&error(502, "model_unavailable_no_retry")).unwrap();
        assert!(matches!(route(&j, "material").unwrap(), Route::Normal));
        consume(&j, "material".into(), failure.clone(), true)
            .unwrap()
            .unwrap();
        drop(j);
        let j = crate::Journal::open(&path).unwrap();
        assert!(matches!(route(&j, "material").unwrap(), Route::Blocked));
        assert!(
            consume(&j, "material".into(), failure.clone(), false)
                .unwrap()
                .is_none()
        );
        accepted(&j, "material").unwrap();
        assert!(matches!(
            route(&j, "material").unwrap(),
            Route::Healthy(false)
        ));
        assert!(block_existing(&j, "material").unwrap());
        assert!(matches!(route(&j, "material").unwrap(), Route::Blocked));
        assert!(
            consume(
                &j,
                "material".into(),
                ProviderFailure {
                    status: 503,
                    class: "provider_timeout".into()
                },
                false
            )
            .unwrap()
            .is_none()
        );
        assert!(matches!(route(&j, "new-source").unwrap(), Route::Normal));
        assert!(
            consume(&j, "new-source".into(), failure, true)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn one_durable_episode_cannot_ping_pong_or_reset_from_different_failure() {
        let failure = ProviderFailure::from_error(&error(502, "provider_failed")).unwrap();
        let mut episode = Episode::reserve("goal-source-worker".into(), failure, true);
        assert_eq!(episode.alternate("other-goal"), None);
        assert_eq!(episode.consume_dispatch("goal-source-worker"), Some(false));
        let saved = serde_json::to_string(&episode).unwrap();
        let mut restored: Episode = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.consume_dispatch("goal-source-worker"), None);
        assert_eq!(restored.context()["remaining_alternate_dispatches"], 0);
        assert_eq!(restored.context()["original_failure"]["status"], 502);
        assert_eq!(restored.context()["original_role"], "sol");
    }
}
