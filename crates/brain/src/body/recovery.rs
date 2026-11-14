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

fn life_scope(o: &Observation) -> Option<String> {
    for value in [
        &o.player["uuid"],
        &o.player["life_id"],
        &o.player["dimension"],
        &o.world["sample_clock"]["epoch"],
    ] {
        value.as_str().filter(|s| !s.is_empty())?;
    }
    o.player["entity_id"].as_i64()?;
    Some(crate::digest(
        json!({"actor":o.player["uuid"],"life":o.player["life_id"],
        "entity":o.player["entity_id"],"dimension":o.player["dimension"],
        "clock":o.world["sample_clock"]["epoch"]})
        .to_string()
        .as_bytes(),
    ))
}
/// A definite native refusal invalidates geometric guesses for this escape
/// episode. A short successful fallback or changed pose is not evidence that
/// the approximate planner can now handle the terrain. Discovery still binds
/// every returned option to its own fresh pose/physics/scene, independently.
/// Uncertainty never authorizes a retry or retires the original durable ID.
#[derive(Default)]
pub(super) struct Required {
    rejected: bool,
    scope: Option<String>,
}
impl Required {
    pub fn for_threat(&mut self, o: &Observation, threatened: bool, water_emergency: bool) -> bool {
        self.active(o) && threatened && !water_emergency
    }
    pub fn active(&mut self, o: &Observation) -> bool {
        if let Some(scope) = life_scope(o) {
            if self.scope.as_ref().is_some_and(|old| old != &scope) {
                self.episode_ended();
            } else if self.rejected {
                self.scope = Some(scope);
            }
        }
        // Missing identity facts cannot refund a refusal or authorize a
        // heuristic mutation. The authenticated discovery path must settle it.
        self.rejected
    }
    pub fn episode_ended(&mut self) {
        self.rejected = false;
        self.scope = None;
    }
    pub fn movement_rejected(&mut self, o: &Observation, receipt: &minecraft_sdk::Receipt) {
        if receipt.outcome == minecraft_sdk::Outcome::Rejected
            && minecraft_sdk::valid_id(&receipt.action_id)
            && receipt
                .error
                .as_ref()
                .is_some_and(|e| e.code == minecraft_sdk::ErrorCode::UnsupportedCapability)
        {
            self.rejected = true;
            self.scope = life_scope(o);
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

struct QueryReply {
    result: std::result::Result<Value, String>,
    elapsed_ms: u128,
}

/// Diagnostics only: never deserialize ignored fields into the accepted Page.
fn response_readiness(value: &Value) -> Value {
    let origin = value.get("origin");
    let busy = value.get("busy");
    let retry = value.get("retry_after_ms");
    let retry_ms = retry.and_then(Value::as_u64);
    json!({
        "origin_present":origin.map(|v|!v.is_null()),
        "origin_field":match origin {None=>"absent",Some(Value::Null)=>"null",Some(Value::Object(_))=>"object",_=>"invalid"},
        "busy":busy.and_then(Value::as_bool),
        "busy_field":match busy {None=>"absent",Some(Value::Bool(_))=>"boolean",_=>"invalid"},
        "retry_after_ms":retry_ms,
        "retry_after_ms_clamped":retry_ms.map(|ms|ms.clamp(25,5000)),
        "retry_field":match retry {None=>"absent",Some(Value::Null)=>"null",_ if retry_ms.is_some()=>"u64",_=>"invalid"},
        "raw_options_count":value["options"].as_array().map(Vec::len)
    })
}

pub(super) struct Discovery {
    task: Option<JoinHandle<QueryReply>>,
    basis: String,
    offset: usize,
    scene: Option<String>,
    done: bool,
    next_query: Instant,
    routes: Vec<Admitted>,
    received: Option<Instant>,
    prefix_busy_retries: u8,
    preference: Option<[f64; 3]>,
    preference_refreshed: bool,
    ready_pages: u8,
    page_origin: Option<[f64; 3]>,
}
const MAX_PREFIX_READY_PAGES: u8 = 4;

fn query_preference(o: &Observation, mut target: [f64; 3]) -> [f64; 3] {
    // Experiment: search level escape candidates first when the existing
    // defensive hint climbs. This orders a read only; it neither establishes
    // support at this height nor changes the native-admitted action target.
    if o.player["on_ground"] == true
        && o.player["in_water"] == false
        && o.player["eyes_in_water"] == false
        && o.player["in_lava"] == false
        && let Some(p) = geometry::point(&o.player["position"])
        && target[1] > p[1]
    {
        target[1] = p[1];
    }
    target
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
            preference: None,
            preference_refreshed: false,
            ready_pages: 0,
            page_origin: None,
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
        self.preference = None;
        self.preference_refreshed = false;
        self.ready_pages = 0;
        self.page_origin = None;
        // Keep the request spacing even across movement/scene changes.
    }
    fn update_basis(&mut self, o: &Observation, first_prefix_only: bool, axis: f64) {
        let physical = crate::digest(
            json!({"physical":basis(o),"first_prefix_only":first_prefix_only,"axis":axis})
                .to_string()
                .as_bytes(),
        );
        if physical != self.basis {
            self.reset(physical);
            // Reuse the existing defensive choice only as a read-order hint.
            // It is not native admission or a newly known enemy position.
            // Pin even an absent hint through this basis's busy retry/cursor;
            // moving enemies must not continuously invalidate an in-flight read.
            self.preference = first_prefix_only
                .then(|| geometry::evade_with_limits(o, &[], &[], &[], axis))
                .flatten()
                .map(|route| query_preference(o, [route.target.x, route.target.y, route.target.z]));
        } else if first_prefix_only
            && self.done
            && self.task.is_none()
            && !self.preference_refreshed
            && self.ready_pages < MAX_PREFIX_READY_PAGES
            && self
                .preference
                .is_some_and(|p| geometry::toward_close_threat(o, p))
        {
            let next = geometry::evade_with_limits(o, &[], &[], &[], axis)
                .map(|route| query_preference(o, [route.target.x, route.target.y, route.target.z]));
            if next.is_some() && next != self.preference {
                // One changed-threat investigation, never a clock/ambient
                // reset or a new mutation allowance. In-flight/busy reads keep
                // their exact target; request spacing survives this reset.
                let basis = self.basis.clone();
                let ready_pages = self.ready_pages;
                let busy_retries = self.prefix_busy_retries;
                self.reset(basis);
                // A changed hint on this same physical basis does not refund
                // the bounded read or readiness investigation allowances.
                self.ready_pages = ready_pages;
                self.prefix_busy_retries = busy_retries;
                self.preference = next;
                self.preference_refreshed = true;
            }
        }
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
        self.update_basis(o, first_prefix_only, axis);
        if self.task.as_ref().is_some_and(JoinHandle::is_finished) {
            let task = self.task.take().unwrap();
            // A changed basis aborts the task before this point. These are the
            // exact pinned request facts, before accept advances the cursor.
            let (request_offset, request_target) = (self.offset, self.preference);
            let (reply, elapsed_ms) = match task.await {
                Ok(reply) => (reply.result, Some(reply.elapsed_ms)),
                Err(error) => (Err(error.to_string()), None),
            };
            let readiness = reply.as_ref().ok().map(response_readiness);
            let pending_response = readiness
                .as_ref()
                .is_some_and(|r| r["origin_field"] == "null" || r["origin_field"] == "absent");
            let result = match reply {
                Ok(value) => {
                    if first_prefix_only {
                        self.accept_prefix(value, o, axis)
                    } else {
                        self.accept(value, o, axis)
                    }
                }
                Err(error) => Err(error.into()),
            };
            let error = result.err().map(|e| e.to_string());
            if error.is_some() {
                self.done = true;
                self.routes.clear();
            }
            let native_complete = self.done;
            let mut empty_continuation = false;
            if first_prefix_only {
                // Busy is readiness, not an empty completed prefix. Honor its
                // existing bounded retry hint once. A ready EMPTY prefix with
                // an advancing cursor is not proof that no route exists.
                if self.received.is_some() {
                    self.done |=
                        !self.routes.is_empty() || self.ready_pages >= MAX_PREFIX_READY_PAGES;
                    if !self.done {
                        empty_continuation = true;
                    }
                } else if self.done || self.prefix_busy_retries >= 1 {
                    self.done = true;
                } else {
                    self.prefix_busy_retries += 1;
                }
            }
            let reason = if error.is_some() {
                "query_failed"
            } else if self.done {
                if native_complete {
                    "native_complete"
                } else if !self.routes.is_empty() {
                    "usable_routes"
                } else if self.ready_pages >= MAX_PREFIX_READY_PAGES {
                    "ready_page_budget_exhausted"
                } else {
                    "readiness_retry_budget_exhausted"
                }
            } else if empty_continuation {
                "empty_ready_prefix_continue"
            } else if self.received.is_none() {
                if pending_response {
                    "readiness_retry_scheduled"
                } else {
                    "scene_restart_scheduled"
                }
            } else {
                "paging_continue"
            };
            let evidence = json!({"request_offset":request_offset,"request_limit":8,"request_target":request_target,
                "request_basis_sha256":self.basis,"transport_elapsed_ms":elapsed_ms,"response":readiness,
                "usable_routes_count":self.routes.len(),"first_prefix_only":first_prefix_only,
                "ready_pages":self.ready_pages,"ready_page_limit":first_prefix_only.then_some(MAX_PREFIX_READY_PAGES),
                "readiness_retry_reservations":self.prefix_busy_retries,"readiness_retry_limit":first_prefix_only.then_some(1),
                "retry_hint_applied":error.is_none()&&self.received.is_none()&&pending_response,
                "search_done":self.done,"policy_reason":reason,
                "scope":"Read-only query evidence; busy aggregates correction/control gates, not their individual cause or a safety claim"});
            if let Some(error) = error {
                j.event("body_native_escape_query_failed",json!({"error":error,
                    "offset":self.offset,"retry":"Only after a meaningful scene/pose change; no gameplay mutation submitted",
                    "query_evidence":evidence}))?;
            } else {
                j.event("body_native_escape_options",json!({"scene":self.scene,
                    "next_offset":self.offset,"complete":native_complete,"options":self.routes.len(),
                    "origin":o.player["position"],"revision":o.revision,
                    "ready_pages":self.ready_pages,"ready_page_limit":MAX_PREFIX_READY_PAGES,
                    "query_evidence":evidence,
                    "scope":"Adapter predictions; native action admission and observed effect still required"}))?;
            }
            if empty_continuation {
                j.event("body_native_escape_empty_prefix_continues",json!({
                    "next_offset":self.offset,"ready_pages":self.ready_pages,
                    "ready_page_limit":MAX_PREFIX_READY_PAGES,"scene":self.scene,
                    "origin":self.page_origin,"target":self.preference,
                    "scope":"Bounded read-only continuation; no heuristic mutation or safety claim"}))?;
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
            let mut input = json!({"offset":offset,"limit":8});
            if let Some(target) = self.preference {
                input["target"] = json!(target);
            }
            self.next_query = Instant::now() + Duration::from_millis(500);
            j.event("body_native_escape_query",json!({"offset":offset,"limit":8,
                "target":self.preference,"target_scope":"Pinned existing defensive choice; read ordering only, not native admission or current hidden enemy position",
                "completed_prefix_refresh_used":self.preference_refreshed,
                "revision":o.revision,"origin":o.player["position"],
                "reason":if first_prefix_only{"Native held-input movement rejected; inspect one current prefix"}else{"No two-block defensive displacement over80advancing native ticks"},
                "execution":"Read-only background query; body observations and urgent defense continue"}))?;
            self.task = Some(tokio::spawn(async move {
                let started = Instant::now();
                let result = match tokio::time::timeout(
                    Duration::from_millis(750),
                    client.read_observation(&ObservationQuery {
                        capability: "minecraft:movement_options".into(),
                        input,
                    }),
                )
                .await
                {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => {
                        Err("Native escape query exceeded750ms; no mutation submitted".into())
                    }
                };
                QueryReply {
                    result,
                    elapsed_ms: started.elapsed().as_millis(),
                }
            }));
        }
        Ok(self.routes.clone())
    }
    fn accept_prefix(&mut self, value: Value, o: &Observation, axis: f64) -> Result<()> {
        let ready = value["origin"].is_object();
        let origin = geometry::point(&value["origin"]["position"]);
        if ready {
            ensure(
                self.ready_pages < MAX_PREFIX_READY_PAGES,
                "Native prefix read budget exhausted",
            )?;
            ensure(
                value["options"].as_array().is_some_and(|v| v.len() <= 8),
                "Native prefix exceeded requested page limit",
            )?;
            ensure(
                value["complete"] != true || value["next_offset"].is_null(),
                "Complete native prefix has a continuation cursor",
            )?;
            if value["complete"] == false {
                ensure(
                    value["next_offset"]
                        .as_u64()
                        .is_some_and(|next| next <= (self.offset + 8) as u64),
                    "Native prefix cursor exceeded requested evaluation budget",
                )?;
            }
            if let Some(scene) = &self.scene {
                ensure(
                    value["scene"].as_str() == Some(scene.as_str()) && origin == self.page_origin,
                    "Native prefix scene or exact origin changed across continuation",
                )?;
            }
        }
        self.accept(value, o, axis)?;
        if ready && self.received.is_some() {
            self.ready_pages += 1;
            self.page_origin = origin;
        }
        Ok(())
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
    fn preference_floor() -> Observation {
        let mut o = observation();
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in -2..=4 {
                    blocks.push(json!({"position":[x,y,z],
                        "block":if y==0{"minecraft:stone"}else{"minecraft:air"},
                        "collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        o.world["blocks"] = json!(blocks);
        o
    }
    #[test]
    fn native_required_preference_is_existing_behavior_and_pinned_to_physical_basis() {
        let mut o = preference_floor();
        let route = geometry::evade_with_limits(&o, &[], &[], &[], 3.5).unwrap();
        let expected = [route.target.x, route.target.y, route.target.z];
        assert!(
            expected[0] < 0.5,
            "existing close-threat behavior points away"
        );
        let mut d = Discovery::default();
        d.update_basis(&o, true, 3.5);
        assert_eq!(d.preference, Some(expected));
        o.world["entities"][0]["position"] = json!([-3.5, 1., 0.5]);
        d.update_basis(&o, true, 3.5);
        assert_eq!(
            d.preference,
            Some(expected),
            "same physical basis pins query target, including readiness retry"
        );
        o.player["position"][2] = json!(0.6);
        d.update_basis(&o, true, 3.5);
        assert!(
            d.preference.unwrap()[0] > 0.5,
            "new pose obtains a new preference and new options"
        );
        d.update_basis(&o, false, 3.5);
        assert_eq!(
            d.preference, None,
            "ordinary stalled paging keeps its default ordering"
        );
        o.world["entities"] = json!([]);
        d.update_basis(&o, true, 3.5);
        assert_eq!(d.preference, None, "no invented threat or goal");
    }
    #[test]
    fn level_query_preference_requires_known_grounded_dry_facts_and_an_upward_hint() {
        let mut o = observation();
        for field in ["in_water", "eyes_in_water", "in_lava"] {
            o.player[field] = json!(false);
        }
        let up = [-2.5, 2., 0.5];
        assert_eq!(query_preference(&o, up), [-2.5, 1., 0.5]);
        for target in [[-2.5, 1., 0.5], [-2.5, 0., 0.5]] {
            assert_eq!(query_preference(&o, target), target);
        }
        for (field, value) in [
            ("on_ground", json!(false)),
            ("on_ground", Value::Null),
            ("in_water", json!(true)),
            ("in_water", Value::Null),
            ("eyes_in_water", json!(true)),
            ("eyes_in_water", Value::Null),
            ("in_lava", json!(true)),
            ("in_lava", Value::Null),
            ("position", Value::Null),
        ] {
            let mut unknown_or_immersed = o.clone();
            unknown_or_immersed.player[field] = value;
            assert_eq!(query_preference(&unknown_or_immersed, up), up, "{field}");
        }
    }
    #[test]
    fn level_query_hint_keeps_existing_basis_pinning_and_native_admission() {
        // Synthetic raised floor, not a reconstructed private-world route.
        let mut o = preference_floor();
        for field in ["in_water", "eyes_in_water", "in_lava"] {
            o.player[field] = json!(false);
        }
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0].as_i64().unwrap() < 0 && b["position"][1] == 1 {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let route = geometry::evade_with_limits(&o, &[], &[], &[], 3.5).unwrap();
        let original = [route.target.x, route.target.y, route.target.z];
        assert!(original[1] > 1., "fixture must exercise the upward hint");
        let mut d = Discovery::default();
        d.update_basis(&o, true, 3.5);
        let expected = [original[0], 1., original[2]];
        assert_eq!(d.preference, Some(expected));
        assert!(d.routes.is_empty(), "query hint is not admitted movement");
        d.ready_pages = 2;
        d.prefix_busy_retries = 1;
        o.world["game_time"] = json!(123);
        o.world["entities"][0]["position"] = json!([-3.5, 1., 0.5]);
        d.update_basis(&o, true, 3.5);
        assert_eq!(d.preference, Some(expected));
        assert_eq!((d.ready_pages, d.prefix_busy_retries), (2, 1));
        let old_basis = d.basis.clone();
        o.player["position"][2] = json!(0.6);
        d.update_basis(&o, true, 3.5);
        assert_ne!(d.basis, old_basis, "changed physical pose reseeds");
        assert_eq!((d.ready_pages, d.prefix_busy_retries), (0, 0));
        d.update_basis(&o, false, 3.5);
        assert_eq!(d.preference, None, "ordinary discovery remains untargeted");
    }
    #[tokio::test]
    async fn completed_unsafe_prefix_gets_one_changed_threat_read_without_player_motion() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            Client::with_profile(&minecraft_sdk::Profile {
                endpoint: format!("http://{}/", listener.local_addr().unwrap()),
                instance_id: "fixture".into(),
                credential: minecraft_sdk::Credential {
                    bot_id: "own".into(),
                    player_uuid: "own".into(),
                    key: "fixture-key".into(),
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
                assert!(
                    headers.starts_with("POST /v1/bots/own/observations/query "),
                    "read only"
                );
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
                assert_eq!(query.input["offset"], 0);
                assert_eq!(query.input["limit"], 8);
                let target = geometry::point(&query.input["target"]).unwrap();
                assert!(if index == 0 {
                    target[0] < 0.5
                } else {
                    target[0] > 0.5
                });
                let mut response = page();
                response["scene"] = json!(format!("target-{index}"));
                response["options"][0]["target"] = json!(target);
                let body = response.to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(80), listener.accept())
                    .await
                    .is_err(),
                "one refresh, no mutation/paging/storm"
            );
        });
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(&root.path().join("body.sqlite")).unwrap();
        let mut o = preference_floor();
        let mut d = Discovery::default();
        for index in 0..2 {
            if index == 1 {
                let old = d.preference.unwrap();
                o.world["entities"][0]["position"] = json!([-3.5, 1., 0.5]);
                assert!(geometry::toward_close_threat(&o, old));
                let spacing = d.next_query;
                d.update_basis(&o, true, 3.5);
                assert_eq!(
                    d.next_query, spacing,
                    "changed threat cannot waive read spacing"
                );
                assert!(d.routes.is_empty() && d.scene.is_none());
                assert!(d.preference_refreshed && !d.done);
                tokio::time::sleep_until(d.next_query.into()).await;
            }
            d.tick(client.clone(), &o, true, true, 3.5, &journal)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while d.waiting() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    d.tick(client.clone(), &o, true, true, 3.5, &journal)
                        .await
                        .unwrap();
                }
            })
            .await
            .unwrap();
            assert!(d.done && d.routes.len() == 1);
        }
        let final_preference = d.preference;
        o.world["entities"][0]["position"] = json!([4.5, 1., 0.5]);
        d.tick(client, &o, true, true, 3.5, &journal).await.unwrap();
        assert!(d.done && !d.waiting());
        assert_eq!(
            d.preference, final_preference,
            "no general liveness claim after the one same-pose refresh"
        );
        assert_eq!(o.player["position"], json!([0.5, 1., 0.5]));
        server.await.unwrap();
    }
    #[test]
    fn only_definite_typed_rejection_requires_native_until_life_or_episode_changes() {
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
            ("/player/entity_id", json!(2)),
            ("/player/dimension", json!("minecraft:the_nether")),
            ("/world/sample_clock/epoch", json!("other-clock")),
        ] {
            let mut required = Required::default();
            required.movement_rejected(&o, &refusal);
            assert!(required.active(&o));
            let mut value_o = serde_json::to_value(&o).unwrap();
            *value_o.pointer_mut(pointer).unwrap() = value;
            let changed = serde_json::from_value(value_o).unwrap();
            assert!(!required.active(&changed), "{pointer}");
        }
        for (pointer, value) in [
            ("/player/yaw", json!(90.)),
            ("/player/physics", json!({"changed":true})),
            ("/player/sprinting", json!(true)),
            ("/player/position", json!([1.5, 1., 0.5])),
            ("/player/on_ground", json!(false)),
            ("/player/velocity", json!([0., -0.3, 0.])),
            ("/world/blocks", json!([{"changed":true}])),
            ("/world/entities", json!([])),
            (
                "/world/entities",
                json!([{"uuid":"different-shooter","visible":true,"hostile":true,"position":[2.5,1.,0.5]}]),
            ),
        ] {
            let mut required = Required::default();
            required.movement_rejected(&o, &refusal);
            assert!(required.active(&o));
            let mut value_o = serde_json::to_value(&o).unwrap();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            value_o.pointer_mut(parent).unwrap()[key] = value;
            let changed = serde_json::from_value(value_o).unwrap();
            assert!(required.for_threat(&changed, true, false), "{pointer}");
        }
        assert!(!unrelated.for_threat(&o, false, false));
        assert!(
            unrelated.for_threat(&o, true, false),
            "a transient quiet frame does not end the existing escape episode"
        );
        let mut unknown = o.clone();
        unknown.player.as_object_mut().unwrap().remove("life_id");
        assert!(
            unrelated.active(&unknown),
            "missing scope facts cannot refund the constraint"
        );
        let mut unbound = Required::default();
        unbound.movement_rejected(&unknown, &refusal);
        assert!(unbound.active(&unknown));
        assert!(
            unbound.active(&o),
            "first typed player scope binds an unbound refusal without refunding it"
        );
        unrelated.episode_ended();
        assert!(
            !unrelated.active(&o),
            "the controller's actual safe handoff ends this episode"
        );
    }
    #[test]
    fn f11_small_fallbacks_do_not_refund_the_native_requirement_for_any_refused_family() {
        // F11 exposed retries after short fallbacks, elevation changes and
        // airborne observations. All poses here are synthetic; this checks
        // episode admission, not captured terrain or native trajectory effects.
        let mut o = observation();
        let refusal = minecraft_sdk::Receipt {
            action_id: "f11-original-refused-intention".into(),
            outcome: minecraft_sdk::Outcome::Rejected,
            adapter_version: "fixture".into(),
            evidence: Value::Null,
            error: Some(minecraft_sdk::ApiError::new(
                minecraft_sdk::ErrorCode::UnsupportedCapability,
                "No supported held-input trajectory",
            )),
        };
        let mut required = Required::default();
        for (index, position) in [
            [0.5, 1., 0.5],
            [-0.5, 1., -0.5],
            [-1.5, 0.5, 0.5],
            [-2.5, 0., 0.5],
            [0.5, 0., 1.5],
            [1.5, 0., 0.5],
        ]
        .into_iter()
        .enumerate()
        {
            o.player["position"] = json!(position);
            o.player["on_ground"] = json!(index != 2);
            o.world["game_time"] = json!(index * 20);
            if index == 0 {
                required.movement_rejected(&o, &refusal);
            }
            assert!(
                required.for_threat(&o, true, false),
                "fallback/frame{index} cannot restore geometric authority"
            );
        }
        assert_eq!(refusal.action_id, "f11-original-refused-intention");
    }
    fn observation() -> Observation {
        serde_json::from_value(json!({"revision":"fixture","player":{
            "uuid":"own","life_id":"life1","entity_id":1,"dimension":"minecraft:overworld",
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
    fn empty_page(offset: usize) -> Value {
        let mut p = page();
        p["options"] = json!([]);
        p["complete"] = json!(false);
        p["next_offset"] = json!(offset + 2);
        p
    }
    #[test]
    fn readiness_metadata_never_promotes_ignored_busy_fields_or_logs_raw_values() {
        let o = observation();
        let mut p = page();
        p["busy"] = json!({"private_extra":"must-not-be-recorded"});
        p["private_extra"] = json!("must-not-be-recorded");
        let metadata = response_readiness(&p);
        assert_eq!(metadata["busy"], Value::Null);
        assert_eq!(metadata["busy_field"], "invalid");
        assert_eq!(metadata["raw_options_count"], 1);
        assert!(!metadata.to_string().contains("must-not-be-recorded"));
        let mut d = Discovery::default();
        d.accept_prefix(p.clone(), &o, 3.5).unwrap();
        assert_eq!(
            d.routes.len(),
            1,
            "previously ignored busy field stays ignored"
        );
        p["retry_after_ms"] = json!(-1);
        let metadata = response_readiness(&p);
        assert_eq!(metadata["retry_field"], "invalid");
        assert_eq!(metadata["retry_after_ms_clamped"], Value::Null);
        assert!(
            Discovery::default().accept_prefix(p, &o, 3.5).is_err(),
            "negative hint still fails existing Page validation"
        );
        assert_eq!(
            response_readiness(&json!({"retry_after_ms":1}))["retry_after_ms_clamped"],
            25
        );
        assert_eq!(
            response_readiness(&json!({"retry_after_ms":9000}))["retry_after_ms_clamped"],
            5000
        );
    }
    #[tokio::test]
    async fn readiness_result_evidence_distinguishes_filtered_ready_busy_and_throttle_after_policy()
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        let mut server = tokio::spawn(async move {
            for (index, offset) in [0, 2, 2].into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                    assert!(bytes.len() < 8192);
                }
                let header = String::from_utf8(bytes).unwrap();
                assert!(
                    header.starts_with("POST /v1/bots/own/observations/query "),
                    "no gameplay mutation"
                );
                let size = header
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size < 8192);
                let mut bytes = vec![0; size];
                socket.read_exact(&mut bytes).await.unwrap();
                let query: ObservationQuery = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(query.input, json!({"offset":offset,"limit":8}));
                let mut value = if index == 0 {
                    let mut p = empty_page(0);
                    p["options"] = page()["options"].clone();
                    p["options"][0]["touched_blocks"] = json!(["minecraft:lava"]);
                    p
                } else {
                    json!({"format":"minecraft-movement-options-v1","origin":null,"scene":"",
                        "options":[],"complete":false,"next_offset":null,
                        "retry_after_ms":if index==1{250}else{100},"busy":index==1})
                };
                value["private_extra"] = json!("must-not-be-recorded");
                let body = value.to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "unchanged three-query limit; no fourth query or mutation"
            );
        });
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("body.sqlite")).unwrap();
        let mut d = Discovery::default();
        let o = observation();
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                assert!(
                    d.tick(client.clone(), &o, true, true, 3.5, &j)
                        .await
                        .unwrap()
                        .is_empty()
                );
                if d.done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            d.tick(client, &o, true, true, 3.5, &j).await.unwrap();
        })
        .await;
        let joined = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
        if joined.is_err() {
            server.abort();
        }
        joined
            .expect("bounded diagnostics transport fixture")
            .unwrap();
        result.unwrap();
        let mut q = j
            .db
            .prepare("SELECT data FROM events WHERE kind='body_native_escape_options' ORDER BY id")
            .unwrap();
        let results: Vec<Value> = q
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        assert_eq!(
            results.len(),
            3,
            "one enriched result per completed response"
        );
        for (index, r) in results.iter().enumerate() {
            let e = &r["query_evidence"];
            assert_eq!(e["request_offset"], [0, 2, 2][index]);
            assert_eq!(e["request_limit"], 8);
            assert_eq!(e["request_target"], Value::Null);
            assert_eq!(e["request_basis_sha256"].as_str().unwrap().len(), 64);
            assert!(e["transport_elapsed_ms"].as_u64().is_some());
            assert_eq!(e["ready_pages"], 1);
            assert_eq!(e["usable_routes_count"], 0);
            assert_eq!(e["search_done"], index == 2);
            assert_eq!(e["readiness_retry_reservations"], usize::from(index > 0));
            assert!(!r.to_string().contains("must-not-be-recorded"));
        }
        let first = &results[0]["query_evidence"];
        assert_eq!(first["response"]["raw_options_count"], 1);
        assert_eq!(first["response"]["origin_present"], true);
        assert_eq!(first["response"]["busy_field"], "absent");
        assert_eq!(first["policy_reason"], "empty_ready_prefix_continue");
        for (index, busy, hint, reason) in [
            (1, true, 250, "readiness_retry_scheduled"),
            (2, false, 100, "readiness_retry_budget_exhausted"),
        ] {
            let e = &results[index]["query_evidence"];
            assert_eq!(e["response"]["busy"], busy);
            assert_eq!(e["response"]["retry_after_ms"], hint);
            assert_eq!(e["response"]["origin_present"], false);
            assert_eq!(e["retry_hint_applied"], true);
            assert_eq!(e["policy_reason"], reason);
        }
        assert_eq!(
            results[2]["complete"], false,
            "legacy native completion stays separate from final search_done"
        );
    }
    #[test]
    fn empty_prefix_requires_exact_cursor_scene_origin_and_finite_read_budget() {
        let o = observation();
        for change in [
            "no_progress",
            "skip",
            "scene",
            "origin",
            "complete_cursor",
            "stale_pose",
            "too_many",
        ] {
            let mut d = Discovery::default();
            d.accept_prefix(empty_page(0), &o, 3.5).unwrap();
            let mut next = empty_page(2);
            let mut current = o.clone();
            match change {
                "no_progress" => next["next_offset"] = json!(2),
                "skip" => next["next_offset"] = json!(11),
                "scene" => next["scene"] = json!("different-scene"),
                "origin" => next["origin"]["position"] = json!([0.501, 1., 0.5]),
                "complete_cursor" => next["complete"] = json!(true),
                "stale_pose" => current.player["position"] = json!([1.5, 1., 0.5]),
                "too_many" => next["options"] = json!(vec![page()["options"][0].clone(); 9]),
                _ => unreachable!(),
            }
            assert!(d.accept_prefix(next, &current, 3.5).is_err(), "{change}");
            assert!(
                d.routes.is_empty(),
                "{change}: no stale or malformed route authority"
            );
        }
        let mut d = Discovery::default();
        for offset in [0, 2, 4, 6] {
            d.accept_prefix(empty_page(offset), &o, 3.5).unwrap();
        }
        assert_eq!(d.ready_pages, MAX_PREFIX_READY_PAGES);
        assert!(d.accept_prefix(empty_page(8), &o, 3.5).is_err());
        assert_eq!(d.offset, 8);
    }
    #[test]
    fn filtered_unusable_routes_continue_but_hint_refresh_cannot_refund_page_or_busy_budget() {
        let mut o = preference_floor();
        let mut d = Discovery::default();
        d.update_basis(&o, true, 3.5);
        let old_hint = d.preference.unwrap();
        let mut unusable = empty_page(0);
        unusable["options"] = page()["options"].clone();
        unusable["options"][0]["touched_blocks"] = json!(["minecraft:lava"]);
        d.accept_prefix(unusable, &o, 3.5).unwrap();
        assert!(d.routes.is_empty());
        assert_eq!(d.ready_pages, 1);
        d.done = true;
        d.prefix_busy_retries = 1;
        o.world["entities"][0]["position"] = json!([old_hint[0], 1., old_hint[2]]);
        d.update_basis(&o, true, 3.5);
        assert!(
            d.preference_refreshed,
            "existing close-threat hint change should rearm once"
        );
        assert_eq!(d.ready_pages, 1);
        assert_eq!(d.prefix_busy_retries, 1);
        d.ready_pages = MAX_PREFIX_READY_PAGES;
        d.done = true;
        d.preference_refreshed = false;
        let before = d.preference;
        if let Some(hint) = before {
            o.world["entities"][0]["position"] = json!([hint[0], 1., hint[2]]);
        }
        d.update_basis(&o, true, 3.5);
        assert!(d.done && !d.preference_refreshed);
        assert_eq!(
            d.preference, before,
            "cap cannot be refunded by another threat hint"
        );
    }
    #[tokio::test]
    async fn native_empty_prefix_search_stops_after_four_pages_without_polling_or_mutation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        let mut server = tokio::spawn(async move {
            for offset in [0, 2, 4, 6] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                    assert!(bytes.len() < 8192);
                }
                let header = String::from_utf8(bytes).unwrap();
                assert!(
                    header.starts_with("POST /v1/bots/own/observations/query "),
                    "read only"
                );
                let size = header
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size < 8192);
                let mut bytes = vec![0; size];
                socket.read_exact(&mut bytes).await.unwrap();
                let query: ObservationQuery = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(query.input, json!({"offset":offset,"limit":8}));
                let body = empty_page(offset).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "no fifth page or mutation"
            );
        });
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("body.sqlite")).unwrap();
        let mut d = Discovery::default();
        let o = observation();
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                assert!(
                    d.tick(client.clone(), &o, true, true, 3.5, &journal)
                        .await
                        .unwrap()
                        .is_empty()
                );
                if d.done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            for _ in 0..3 {
                assert!(
                    d.tick(client.clone(), &o, true, true, 3.5, &journal)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        })
        .await;
        let joined = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
        if joined.is_err() {
            server.abort();
        }
        joined.expect("bounded prefix fixture").unwrap();
        result.unwrap();
        assert_eq!(d.ready_pages, 4);
        assert!(d.task.is_none());
    }
    #[tokio::test]
    async fn f11_episode_keeps_constraint_but_requeries_native_options_for_the_new_pose() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
                assert!(headers.len() < 8192);
            }
            let headers = std::str::from_utf8(&headers).unwrap();
            assert!(
                headers.starts_with("POST /v1/bots/own/observations/query "),
                "no mutation/replay"
            );
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
            assert_eq!(query.input, json!({"offset":0,"limit":8}));
            let mut fresh = page();
            fresh["scene"] = json!("new-current-pose");
            fresh["origin"]["position"] = json!([1.5, 1., 0.5]);
            fresh["options"][0]["target"] = json!([2.5, 1., 0.5]);
            fresh["options"][0]["jump"] = json!(false);
            let body = fresh.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(80), listener.accept())
                    .await
                    .is_err(),
                "one fresh prefix, no full paging or mutation retry"
            );
        });
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(&root.path().join("body.sqlite")).unwrap();
        let mut o = observation();
        let mut required = Required::default();
        required.movement_rejected(
            &o,
            &minecraft_sdk::Receipt {
                action_id: "original-refusal".into(),
                outcome: minecraft_sdk::Outcome::Rejected,
                adapter_version: "fixture".into(),
                evidence: Value::Null,
                error: Some(minecraft_sdk::ApiError::new(
                    minecraft_sdk::ErrorCode::UnsupportedCapability,
                    "native refusal",
                )),
            },
        );
        let mut discovery = Discovery::default();
        discovery.basis = basis(&o);
        discovery.accept(page(), &o, 3.5).unwrap();
        assert_eq!(discovery.routes[0].target, [-0.5, 2., 0.5]);
        o.player["position"] = json!([1.5, 1., 0.5]);
        let options = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                assert!(
                    required.for_threat(&o, true, false),
                    "a fallback cannot restore guesses"
                );
                let options = discovery
                    .tick(client.clone(), &o, true, true, 3.5, &journal)
                    .await
                    .unwrap();
                assert!(
                    options.iter().all(|r| r.target != [-0.5, 2., 0.5]),
                    "old pose options are never reused"
                );
                if !options.is_empty() {
                    break options;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].target, [2.5, 1., 0.5]);
        assert!(
            required.active(&o),
            "obtaining a fresh prefix does not refund the episode"
        );
        server.await.unwrap();
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
