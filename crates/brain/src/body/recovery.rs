//! Read-only, bounded route discovery when the fast defensive policy stalls.
use super::geometry::{self, Admitted};
use crate::{Journal, Result, ensure};
use minecraft_sdk::{Client, Observation, ObservationQuery};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

fn basis(o: &Observation) -> String {
    crate::digest(
        json!({"actor":o.player["uuid"],"life":o.player["life_id"],
        "entity":o.player["entity_id"],"dimension":o.player["dimension"],
        "clock":o.world["sample_clock"]["epoch"],
        "position":geometry::point(&o.player["position"]).map(geometry::target_key),
        "speed":o.player["movement_speed"],"physics":o.player["physics"],
        "yaw":o.player["yaw"],"sprinting":o.player["sprinting"],
        "on_ground":o.player["on_ground"],"velocity":o.player["velocity"],
        "sneaking":o.player["sneaking"],"blocks":o.world["blocks"]})
        .to_string()
        .as_bytes(),
    )
}

/// A definite native refusal invalidates geometric guesses for this scene.
/// Uncertainty never authorizes a retry or retires the original durable ID.
#[derive(Default)]
pub(super) struct Required {
    rejected_basis: Option<String>,
}
impl Required {
    pub fn for_threat(&mut self, o: &Observation, threatened: bool, water_emergency: bool) -> bool {
        self.active(o) && threatened && !water_emergency && o.player["on_ground"] == true
    }
    pub fn active(&mut self, o: &Observation) -> bool {
        if self
            .rejected_basis
            .as_ref()
            .is_some_and(|old| old != &basis(o))
        {
            self.rejected_basis = None;
        }
        self.rejected_basis.is_some()
    }
    pub fn movement_rejected(&mut self, o: &Observation, receipt: &minecraft_sdk::Receipt) {
        if receipt.outcome == minecraft_sdk::Outcome::Rejected
            && minecraft_sdk::valid_id(&receipt.action_id)
            && receipt
                .error
                .as_ref()
                .is_some_and(|e| e.code == minecraft_sdk::ErrorCode::UnsupportedCapability)
        {
            self.rejected_basis = Some(basis(o));
        }
    }
}

#[derive(Default)]
pub(super) struct Progress {
    lane: Option<(String, String, String, i64)>,
    last_tick: i64,
    anchor: Option<([f64; 3], i64)>,
}
impl Progress {
    pub fn stalled(&mut self, o: &Observation, danger: bool) -> bool {
        let sample = || {
            Some((
                (
                    o.player["uuid"].as_str()?.to_owned(),
                    o.player["dimension"].as_str()?.to_owned(),
                    o.world["sample_clock"]["epoch"].as_str()?.to_owned(),
                    o.player["entity_id"].as_i64()?,
                ),
                geometry::point(&o.player["position"])?,
                o.world["game_time"].as_i64()?,
            ))
        };
        let Some((lane, p, tick)) = sample() else {
            *self = Self::default();
            return false;
        };
        if !danger || o.player["health"].as_f64().is_none_or(|h| h <= 0.) {
            *self = Self::default();
            return false;
        }
        if self.lane.as_ref() != Some(&lane)
            || tick < self.last_tick
            || tick.saturating_sub(self.last_tick) > 20
            || self
                .anchor
                .is_none_or(|(q, _)| (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>() >= 4.)
        {
            self.anchor = Some((p, tick));
        }
        self.lane = Some(lane);
        self.last_tick = tick;
        self.anchor
            .is_some_and(|(_, t)| tick.saturating_sub(t) >= 80)
    }
}

#[derive(Deserialize)]
struct Origin {
    position: [f64; 3],
    dimension: String,
}
#[derive(Deserialize)]
struct Route {
    target: [f64; 3],
    jump: bool,
    duration_ticks: u16,
    max_ticks: u16,
    touched_blocks: Vec<String>,
}
#[derive(Deserialize)]
struct Page {
    format: String,
    origin: Option<Origin>,
    scene: String,
    options: Vec<Route>,
    complete: bool,
    next_offset: Option<usize>,
    retry_after_ms: Option<u64>,
}

pub(super) struct Discovery {
    task: Option<JoinHandle<std::result::Result<Value, String>>>,
    basis: String,
    offset: usize,
    scene: Option<String>,
    done: bool,
    next_query: Instant,
    routes: Vec<Admitted>,
    received: Option<Instant>,
    prefix_busy_retries: u8,
}
impl Default for Discovery {
    fn default() -> Self {
        Self {
            task: None,
            basis: String::new(),
            offset: 0,
            scene: None,
            done: false,
            next_query: Instant::now(),
            routes: Vec::new(),
            received: None,
            prefix_busy_retries: 0,
        }
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.abort();
    }
}
impl Discovery {
    fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
    fn reset(&mut self, basis: String) {
        self.abort();
        self.basis = basis;
        self.offset = 0;
        self.scene = None;
        self.done = false;
        self.routes.clear();
        self.received = None;
        self.prefix_busy_retries = 0;
        // Keep the request spacing even across movement/scene changes.
    }
    pub fn waiting(&self) -> bool {
        self.task.is_some()
    }
    pub async fn tick(
        &mut self,
        client: Arc<Client>,
        o: &Observation,
        enabled: bool,
        first_prefix_only: bool,
        axis: f64,
        j: &Journal,
    ) -> Result<Vec<Admitted>> {
        if !enabled {
            self.reset(String::new());
            return Ok(Vec::new());
        }
        let basis = basis(o);
        if basis != self.basis {
            self.reset(basis);
        }
        if self.task.as_ref().is_some_and(JoinHandle::is_finished) {
            let task = self.task.take().unwrap();
            let result = match task.await {
                Ok(Ok(value)) => self.accept(value, o, axis),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(error.to_string().into()),
            };
            if let Err(error) = result {
                self.done = true;
                self.routes.clear();
                j.event("body_native_escape_query_failed",json!({"error":error.to_string(),
                    "offset":self.offset,"retry":"Only after a meaningful scene/pose change; no gameplay mutation submitted"}))?;
            } else {
                j.event("body_native_escape_options",json!({"scene":self.scene,
                    "next_offset":self.offset,"complete":self.done,"options":self.routes.len(),
                    "origin":o.player["position"],"revision":o.revision,
                    "scope":"Adapter predictions; native action admission and observed effect still required"}))?;
            }
            if first_prefix_only {
                // Busy is readiness, not an empty completed prefix. Honor its
                // existing bounded retry hint once for this exact basis; never
                // page a ready response or repeatedly poll unchanged failures.
                if self.received.is_some() || self.done || self.prefix_busy_retries >= 1 {
                    self.done = true;
                } else {
                    self.prefix_busy_retries += 1;
                }
            }
        }
        if self
            .received
            .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
        {
            self.routes.clear();
        }
        if self.task.is_none() && !self.done && Instant::now() >= self.next_query {
            let offset = self.offset;
            self.next_query = Instant::now() + Duration::from_millis(500);
            j.event("body_native_escape_query",json!({"offset":offset,"limit":8,
                "revision":o.revision,"origin":o.player["position"],
                "reason":if first_prefix_only{"Native held-input movement rejected; inspect one current prefix"}else{"No two-block defensive displacement over80advancing native ticks"},
                "execution":"Read-only background query; body observations and urgent defense continue"}))?;
            self.task = Some(tokio::spawn(async move {
                match tokio::time::timeout(
                    Duration::from_millis(750),
                    client.read_observation(&ObservationQuery {
                        capability: "minecraft:movement_options".into(),
                        input: json!({"offset":offset,"limit":8}),
                    }),
                )
                .await
                {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => {
                        Err("Native escape query exceeded750ms; no mutation submitted".into())
                    }
                }
            }));
        }
        Ok(self.routes.clone())
    }
    fn accept(&mut self, value: Value, o: &Observation, axis: f64) -> Result<()> {
        let page: Page = serde_json::from_value(value)?;
        ensure(
            page.format == "minecraft-movement-options-v1"
                && page.scene.len() <= 128
                && page.options.len() <= 64,
            "Invalid native escape page",
        )?;
        let Some(origin) = page.origin else {
            ensure(
                page.scene.is_empty()
                    && page.options.is_empty()
                    && !page.complete
                    && page.next_offset.is_none()
                    && page.retry_after_ms.is_some(),
                "Invalid pending native escape page",
            )?;
            // A busy adapter must not retire discovery until the next pose
            // change. Retry the same read page while urgent defense continues.
            self.routes.clear();
            self.received = None;
            self.next_query = Instant::now()
                + Duration::from_millis(page.retry_after_ms.unwrap().clamp(25, 5000));
            return Ok(());
        };
        ensure(!page.scene.is_empty(), "Native escape scene unavailable")?;
        let p = geometry::point(&o.player["position"]).ok_or("Player pose unavailable")?;
        ensure(
            origin.dimension == o.player["dimension"].as_str().unwrap_or_default()
                && origin.position.iter().all(|n| n.is_finite())
                && (0..3)
                    .map(|i| (p[i] - origin.position[i]).powi(2))
                    .sum::<f64>()
                    <= 0.15_f64.powi(2),
            "Native escape prediction no longer matches the actor pose",
        )?;
        if self.scene.as_ref().is_some_and(|s| s != &page.scene) {
            self.offset = 0;
            self.scene = None;
            self.routes.clear();
            self.received = None;
            return Ok(()); // Never consume an old offset in a new scene.
        }
        let next = if page.complete {
            self.offset
        } else {
            page.next_offset
                .filter(|n| *n > self.offset && *n <= 1024)
                .ok_or("Native escape pagination did not advance")?
        };
        let mut routes = Vec::new();
        for r in page.options {
            ensure(
                r.target.iter().all(|n| n.is_finite())
                    && r.max_ticks > 0
                    && r.duration_ticks > 0
                    && r.duration_ticks <= r.max_ticks
                    && r.touched_blocks.len() <= 729,
                "Invalid native escape prediction",
            )?;
            if r.duration_ticks > 60
                || (r.target[0] - p[0]).abs() > axis
                || (r.target[2] - p[2]).abs() > axis
                || r.target[1] - p[1] > 1.25
                || p[1] - r.target[1] > 3.
                || r.touched_blocks
                    .iter()
                    .any(|id| geometry::KNOWN_HAZARDS.contains(&id.as_str()))
            {
                continue;
            }
            routes.push(Admitted {
                target: r.target,
                jump: r.jump,
            });
        }
        self.routes = routes;
        self.received = Some(Instant::now());
        self.scene = Some(page.scene);
        self.offset = next;
        self.done = page.complete;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_definite_typed_rejection_requires_native_and_every_basis_change_retires_it() {
        let o = observation();
        let receipt = |outcome, code, id: &str| minecraft_sdk::Receipt {
            action_id: id.into(),
            outcome,
            adapter_version: "fixture".into(),
            evidence: Value::Null,
            error: Some(minecraft_sdk::ApiError::new(code, "fixture")),
        };
        for (outcome, code, id) in [
            (
                minecraft_sdk::Outcome::Uncertain,
                minecraft_sdk::ErrorCode::UnsupportedCapability,
                "unknown",
            ),
            (
                minecraft_sdk::Outcome::Rejected,
                minecraft_sdk::ErrorCode::Conflict,
                "conflict",
            ),
            (
                minecraft_sdk::Outcome::Rejected,
                minecraft_sdk::ErrorCode::UnsupportedCapability,
                "invalid/id",
            ),
        ] {
            let mut required = Required::default();
            required.movement_rejected(&o, &receipt(outcome, code, id));
            assert!(!required.active(&o));
        }
        let refusal = receipt(
            minecraft_sdk::Outcome::Rejected,
            minecraft_sdk::ErrorCode::UnsupportedCapability,
            "rejected-move",
        );
        let mut unrelated = Required::default();
        unrelated.movement_rejected(&o, &refusal);
        let mut burning = o.clone();
        burning.world["entities"] = json!([]);
        burning.player["fire_ticks"] = json!(20);
        assert!(
            !unrelated.for_threat(&burning, false, false),
            "old rejection must not suppress unrelated fire recovery without a threat"
        );
        assert!(
            !unrelated.for_threat(&o, true, true),
            "breathing recovery retains immediate authority"
        );
        assert!(unrelated.for_threat(&o, true, false));
        for (pointer, value) in [
            ("/player/uuid", json!("other")),
            ("/player/life_id", json!("new-life")),
            ("/player/dimension", json!("minecraft:the_nether")),
            ("/player/yaw", json!(90.)),
            ("/player/physics", json!({"changed":true})),
            ("/player/sprinting", json!(true)),
            ("/player/position", json!([1.5, 1., 0.5])),
            ("/world/blocks", json!([{"changed":true}])),
        ] {
            let mut required = Required::default();
            required.movement_rejected(&o, &refusal);
            assert!(required.active(&o));
            let mut value_o = serde_json::to_value(&o).unwrap();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            value_o.pointer_mut(parent).unwrap()[key] = value;
            let changed = serde_json::from_value(value_o).unwrap();
            assert!(!required.active(&changed), "{pointer}");
        }
    }
    fn observation() -> Observation {
        serde_json::from_value(json!({"revision":"fixture","player":{
            "uuid":"own","entity_id":1,"dimension":"minecraft:overworld",
            "position":[0.5,1.,0.5],"health":20.,"on_ground":true},
            "world":{"game_time":0,"sample_clock":{"epoch":"clock"},"blocks":[],
                "entities":[{"visible":true,"hostile":true,"position":[4.5,1.,0.5]}]},"menu":null}))
        .unwrap()
    }
    fn page() -> Value {
        json!({"format":"minecraft-movement-options-v1","scene":"scene-a",
            "origin":{"position":[0.5,1.,0.5],"dimension":"minecraft:overworld"},
            "options":[{"target":[-0.5,2.,0.5],"jump":true,"duration_ticks":16,
                "max_ticks":100,"touched_blocks":["minecraft:stone","minecraft:air"]}],
            "complete":true,"next_offset":null})
    }
    #[test]
    fn busy_escape_preview_preserves_discovery_and_retries_without_a_pose_change() {
        let o = observation();
        let pending = json!({"format":"minecraft-movement-options-v1","origin":null,
            "scene":"","options":[],"complete":false,"next_offset":null,
            "retry_after_ms":250,"busy":true});
        let mut d = Discovery::default();
        d.routes.push(Admitted {
            target: [-0.5, 2., 0.5],
            jump: true,
        });
        d.received = Some(Instant::now());
        let start = Instant::now();
        d.accept(pending.clone(), &o, 3.5).unwrap();
        assert!(!d.done);
        assert!(
            d.routes.is_empty(),
            "A pending page cannot validate stale controls"
        );
        assert!(d.received.is_none());
        assert!(d.next_query >= start + Duration::from_millis(250));
        d.accept(page(), &o, 3.5).unwrap();
        assert_eq!(d.routes.len(), 1);
        for field in [
            "complete",
            "scene",
            "next_offset",
            "retry_after_ms",
            "format",
        ] {
            let mut malformed = pending.clone();
            malformed[field] = match field {
                "complete" => json!(true),
                "scene" => json!("unexpected"),
                "next_offset" => json!(8),
                "format" => json!("future-v2"),
                _ => Value::Null,
            };
            assert!(Discovery::default().accept(malformed, &o, 3.5).is_err());
        }
    }
    #[tokio::test]
    async fn rejection_prefix_retries_busy_readiness_once_then_consumes_ready_or_stops() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for ready in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = Arc::new(
                Client::with_profile(&minecraft_sdk::Profile {
                    endpoint: format!("http://{}/", listener.local_addr().unwrap()),
                    instance_id: "fixture".into(),
                    credential: minecraft_sdk::Credential {
                        bot_id: "own".into(),
                        player_uuid: "own".into(),
                        key: "fixture".into(),
                    },
                    session: None,
                })
                .unwrap()
                .without_local_arbiter(),
            );
            let server = tokio::spawn(async move {
                for index in 0..2 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        headers.push(socket.read_u8().await.unwrap());
                        assert!(headers.len() < 8192);
                    }
                    let headers = std::str::from_utf8(&headers).unwrap();
                    assert!(headers.starts_with("POST /v1/bots/own/observations/query "));
                    let size = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(size < 16384);
                    let mut bytes = vec![0; size];
                    socket.read_exact(&mut bytes).await.unwrap();
                    let query: ObservationQuery = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(query.capability, "minecraft:movement_options");
                    assert_eq!(
                        query.input,
                        json!({"offset":0,"limit":8}),
                        "never page a ready prefix"
                    );
                    let value = if index == 1 && ready {
                        let mut p = page();
                        p["complete"] = json!(false);
                        p["next_offset"] = json!(8);
                        p
                    } else {
                        json!({"format":"minecraft-movement-options-v1","origin":null,"scene":"",
                            "options":[],"complete":false,"next_offset":null,"retry_after_ms":250,"busy":true})
                    };
                    let body = value.to_string();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(80), listener.accept())
                        .await
                        .is_err(),
                    "at most one same-basis readiness retry, no full paging or storm"
                );
            });
            let root = tempfile::tempdir().unwrap();
            let j = Journal::open(&root.path().join("body.sqlite")).unwrap();
            let mut d = Discovery::default();
            let o = observation();
            d.tick(client.clone(), &o, true, true, 3.5, &j)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while d.waiting() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    d.tick(client.clone(), &o, true, true, 3.5, &j)
                        .await
                        .unwrap();
                }
            })
            .await
            .unwrap();
            assert!(!d.done);
            assert_eq!(d.prefix_busy_retries, 1);
            assert!(d.routes.is_empty());
            assert!(
                d.next_query > Instant::now(),
                "honor the busy response's bounded retry hint"
            );
            tokio::time::sleep(Duration::from_millis(260)).await;
            d.tick(client.clone(), &o, true, true, 3.5, &j)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while d.waiting() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    d.tick(client.clone(), &o, true, true, 3.5, &j)
                        .await
                        .unwrap();
                }
            })
            .await
            .unwrap();
            assert!(d.done);
            assert_eq!(d.routes.len(), usize::from(ready));
            d.tick(client, &o, true, true, 3.5, &j).await.unwrap();
            assert!(!d.waiting());
            server.await.unwrap();
        }
    }
    #[test]
    fn short_oscillation_is_not_retreat_and_new_lives_or_clocks_reset_the_window() {
        let mut o = observation();
        let mut progress = Progress::default();
        for tick in 0..=90 {
            o.world["game_time"] = json!(tick);
            o.player["position"][2] = json!(if tick % 2 == 0 { 0.5 } else { 1.5 });
            assert_eq!(progress.stalled(&o, true), tick >= 80);
        }
        o.player["entity_id"] = json!(2);
        assert!(!progress.stalled(&o, true));
        o.world["game_time"] = json!(200);
        assert!(
            !progress.stalled(&o, true),
            "A gap is not advancing observed progress"
        );
        o.world["sample_clock"]["epoch"] = json!("replacement");
        assert!(!progress.stalled(&o, true));
        assert!(!progress.stalled(&o, false));
        o.world["game_time"] = json!(201);
        o.player["health"] = json!(0.);
        assert!(!progress.stalled(&o, true));
    }
    #[test]
    fn stale_wrong_dimension_hazardous_and_unbounded_predictions_do_not_become_controls() {
        let o = observation();
        let mut d = Discovery::default();
        d.accept(page(), &o, 3.5).unwrap();
        assert_eq!(d.routes.len(), 1);
        let mut p = page();
        p["origin"]["position"][0] = json!(1.);
        assert!(Discovery::default().accept(p, &o, 3.5).is_err());
        let mut p = page();
        p["origin"]["dimension"] = json!("minecraft:the_nether");
        assert!(Discovery::default().accept(p, &o, 3.5).is_err());
        let mut p = page();
        p["options"][0]["target"][0] = json!(100.);
        let mut d = Discovery::default();
        d.accept(p, &o, 3.5).unwrap();
        assert!(d.routes.is_empty());
        let mut p = page();
        p["options"][0]["touched_blocks"] = json!(["minecraft:lava"]);
        let mut d = Discovery::default();
        d.accept(p, &o, 3.5).unwrap();
        assert!(d.routes.is_empty());
        let mut p = page();
        p["options"][0]["duration_ticks"] = json!(80);
        let mut d = Discovery::default();
        d.accept(p, &o, 3.5).unwrap();
        assert!(d.routes.is_empty());
        let mut p = page();
        p["complete"] = json!(false);
        p["next_offset"] = json!(0);
        assert!(Discovery::default().accept(p, &o, 3.5).is_err());
        let mut d = Discovery::default();
        d.offset = 8;
        d.scene = Some("old".into());
        d.accept(page(), &o, 3.5).unwrap();
        assert!(d.routes.is_empty());
        assert_eq!(d.offset, 0);
    }
    #[test]
    fn native_prediction_can_supply_a_low_clearance_step_the_fast_arc_misses() {
        // Synthetic corridor, not a private-world capture. The native adapter
        // can account for ceiling contact; a sine arc cannot establish it.
        let mut o = observation();
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in -1..=5 {
                    let solid = y == 0
                        || y == 4
                        || ((z == -1 || z == 1) && (1..=3).contains(&y))
                        || (x == -1 && z == 0 && y == 1);
                    blocks.push(json!({"position":[x,y,z],"block":if solid{"minecraft:stone"}else{"minecraft:air"},
                "collision":if solid{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        o.world["blocks"] = json!(blocks);
        assert!(geometry::evade(&o).is_none());
        let mut d = Discovery::default();
        d.accept(page(), &o, 3.5).unwrap();
        let escape = geometry::evade_with_native(&o, &[], &[], &[], 3.5, &d.routes).unwrap();
        assert_eq!(
            [escape.target.x, escape.target.y, escape.target.z],
            [-0.5, 2., 0.5]
        );
        assert!(escape.jump);
        assert!(escape.reason.contains("native-predicted"));
    }
    #[tokio::test]
    async fn a_stalled_query_is_background_work_and_is_aborted_when_the_basis_is_retired() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            Client::with_profile(&minecraft_sdk::Profile {
                endpoint: format!("http://{}/", listener.local_addr().unwrap()),
                instance_id: "fixture".into(),
                credential: minecraft_sdk::Credential {
                    bot_id: "own".into(),
                    player_uuid: "own".into(),
                    key: "fixture".into(),
                },
                session: None,
            })
            .unwrap()
            .without_local_arbiter(),
        );
        let root = tempfile::tempdir().unwrap();
        let j = Journal::open(&root.path().join("body.sqlite")).unwrap();
        let mut d = Discovery::default();
        let o = observation();
        let start = Instant::now();
        d.tick(client.clone(), &o, true, false, 3.5, &j)
            .await
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(d.waiting());
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = [0; 4096];
        use tokio::io::AsyncReadExt;
        let n = socket.read(&mut request).await.unwrap();
        assert!(
            String::from_utf8_lossy(&request[..n])
                .starts_with("POST /v1/bots/own/observations/query")
        );
        d.tick(client, &o, false, false, 3.5, &j).await.unwrap();
        assert!(!d.waiting());
        assert!(d.routes.is_empty());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), socket.read(&mut request))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
