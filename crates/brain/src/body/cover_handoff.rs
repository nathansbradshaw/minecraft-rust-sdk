//! One finite continuation of a previously selected path, never hidden cover authority.
use super::{NativeBasis, geometry};
use minecraft_sdk::Observation;

struct Pending {
    basis: NativeBasis,
    entity: i64,
    original: String,
    endpoint: [f64; 3],
    remaining: Vec<[f64; 3]>,
    shooter: String,
    occluded: bool,
    ready: bool,
}
#[derive(Default)]
pub(super) struct Handoff {
    pending: Option<Pending>,
    spent: Option<String>,
    invalidated: Option<(String, &'static str)>,
}
impl Handoff {
    fn discard(&mut self, reason: &'static str) {
        if let Some(p) = self.pending.take() {
            self.invalidated = Some((p.original, reason));
        }
    }
    pub fn take_invalidation(&mut self) -> Option<(String, &'static str)> {
        self.invalidated.take()
    }
    /// Called only after this body received Running for its new durable ID.
    pub fn record(&mut self, o: &Observation, route: &geometry::Escape, id: &str) {
        self.discard("Superseded by a new owned movement");
        if !minecraft_sdk::valid_id(id) || self.spent.as_deref() == Some(id) || route.jump {
            return;
        }
        let Some(basis) = NativeBasis::read(o) else {
            return;
        };
        let Some(entity) = o.player["entity_id"].as_i64() else {
            return;
        };
        let endpoint = [route.target.x, route.target.y, route.target.z];
        if route.cover_path.len() > 32 {
            return;
        }
        let path: Vec<_> = route.cover_path.iter().map(|p| [p.x, p.y, p.z]).collect();
        let Some(at) = path.iter().position(|p| *p == endpoint) else {
            return;
        };
        let remaining = path[at + 1..].to_vec();
        if remaining.is_empty() || remaining.iter().flatten().any(|v| !v.is_finite()) {
            return;
        }
        let Some(start) = geometry::point(&o.player["position"]) else {
            return;
        };
        let threats: Vec<_> = o.world["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["visible"] == true && e["hostile"] == true)
            .filter(|e| {
                geometry::point(&e["position"]).is_some_and(|p| {
                    (0..3).map(|i| (start[i] - p[i]).powi(2)).sum::<f64>()
                        < geometry::threat_radius(e).powi(2)
                })
            })
            .collect();
        // Deliberately narrow: no multi-shooter continuation from partial facts.
        if threats.len() != 1 || threats[0]["ranged_attack"] != true {
            return;
        }
        let Some(shooter) = threats[0]["uuid"].as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        self.pending = Some(Pending {
            basis,
            entity,
            original: id.into(),
            endpoint,
            remaining,
            shooter: shooter.into(),
            occluded: false,
            ready: false,
        });
    }
    pub fn propose(
        &mut self,
        o: &Observation,
        owned: &str,
        urgent: bool,
        blocked: &[[i32; 3]],
        axis: f64,
    ) -> Option<geometry::Escape> {
        let p = self.pending.as_mut()?;
        p.ready = false;
        let Some(now) = NativeBasis::read(o) else {
            self.discard("Native actor or clock facts unavailable");
            return None;
        };
        if !p.basis.permits_cached(&now)
            || o.player["entity_id"].as_i64() != Some(p.entity)
            || owned != p.original
            || urgent
        {
            self.discard(
                "Actor/life/control scope changed or current emergency preempted continuation",
            );
            return None;
        }
        let control = &o.player["control"];
        if control["action_id"].as_str() != Some(&p.original)
            || geometry::point(&control["target"]) != Some(p.endpoint)
            || control["cancel_requested"] == true
        {
            self.discard("Original control replaced, cancelled or unavailable");
            return None;
        }
        let Some(shooter) = o.world["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e["uuid"].as_str() == Some(&p.shooter))
        else {
            self.discard("Original threat visibility facts unavailable");
            return None;
        };
        if o.world["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| {
                e["visible"] == true
                    && e["hostile"] == true
                    && e["uuid"].as_str() != Some(p.shooter.as_str())
                    && geometry::point(&e["position"])
                        .zip(geometry::point(&o.player["position"]))
                        .is_some_and(|(enemy, own)| {
                            (0..3).map(|i| (enemy[i] - own[i]).powi(2)).sum::<f64>()
                                < geometry::threat_radius(e).powi(2)
                        })
            })
        {
            self.discard("A different current threat requires fresh defensive behavior");
            return None;
        }
        if shooter["visible"] == false
            && shooter["client_memory"]["kind"] == "last_visible_ranged_threat"
            && shooter["client_memory"]["expires_after_ticks"] == 160
            && shooter["client_memory"]["age_ticks"]
                .as_u64()
                .is_some_and(|age| age <= 160)
        {
            p.occluded = true;
        } else if shooter["visible"] != true || p.occluded {
            self.discard(
                "Occlusion lease missing/expired or shooter reacquired; use fresh behavior",
            );
            return None;
        }
        // Never advance running, failed, cancelled, uncertain or reconciled work.
        if control["outcome"] == "running"
            || (control["outcome"] == "observed" && control["input_released"] == false)
        {
            p.basis = now;
            return None;
        }
        if control["outcome"] != "observed" || control["input_released"] != true {
            self.discard("Original leg lacks conclusive Observed and released evidence");
            return None;
        }
        if !p.occluded {
            self.discard("Completed leg remains visible; select a fresh cover plan");
            return None;
        }
        let position = geometry::point(&o.player["position"])?;
        if now.nanos <= p.basis.nanos
            || (0..3)
                .map(|i| (position[i] - p.endpoint[i]).powi(2))
                .sum::<f64>()
                > 0.15_f64.powi(2)
        {
            self.discard("No advancing terminal sample at the original endpoint");
            return None;
        }
        let route = geometry::continue_cover(o, &p.remaining, blocked, axis);
        if route.is_none() {
            self.discard(
                "Remaining old path is not currently supported or conflicts with current threats",
            );
            return None;
        }
        p.ready = true;
        route
    }
    /// Consume before creating the next action ID. A lost response never rearms it.
    pub fn consume(&mut self) -> Option<String> {
        if !self.pending.as_ref().is_some_and(|p| p.ready) {
            return None;
        }
        let p = self.pending.take()?;
        self.spent = Some(p.original.clone());
        Some(p.original)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minecraft_sdk::Position;
    use serde_json::{Value, json};
    fn scene() -> (Observation, geometry::Escape, Observation) {
        let mut blocks = Vec::new();
        for x in -3..=4 {
            for z in -3..=4 {
                for y in 0..=3 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},
                "collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let prior = Observation {
            revision: "1".into(),
            menu: Value::Null,
            player: json!({"uuid":"own","life_id":"life","entity_id":1,"dimension":"fixture:world",
                "position":[0.5,1.,0.5],"health":20.,"on_ground":true,"in_water":false,"in_lava":false}),
            world: json!({"sample_clock":{"epoch":"epoch","nanos":"10"},"game_time":100,"blocks":blocks,
                "entities":[{"uuid":"shooter","visible":true,"hostile":true,"ranged_attack":true,"position":[8.5,1.,-1.5]}]}),
        };
        // Synthetic selected path. This tests ownership/continuation, not native
        // occlusion, trajectory admission or a real Survival outcome.
        let route = geometry::Escape {
            target: Position {
                x: 1.5,
                y: 1.,
                z: 0.5,
            },
            jump: false,
            reason: "synthetic visible cover approach".into(),
            cover_path: vec![
                Position {
                    x: 1.5,
                    y: 1.,
                    z: 0.5,
                },
                Position {
                    x: 1.5,
                    y: 1.,
                    z: 1.5,
                },
            ],
        };
        let mut current = prior.clone();
        current.revision = "2".into();
        current.player["position"] = json!([1.5, 1., 0.5]);
        current.player["control"] = json!({"action_id":"original-leg","target":[1.5,1.,0.5],
            "outcome":"observed","input_released":true,"cancel_requested":false});
        current.world["sample_clock"]["nanos"] = json!("20");
        current.world["entities"][0]["visible"] = json!(false);
        current.world["entities"][0]["client_memory"] = json!({"kind":"last_visible_ranged_threat",
            "age_ticks":1,"expires_after_ticks":160,"scope":"Last seen pose, not current position"});
        (prior, route, current)
    }
    #[test]
    fn one_conclusive_owned_leg_can_finish_existing_path_without_hidden_cover_authority() {
        let (prior, route, current) = scene();
        let mut h = Handoff::default();
        h.record(&prior, &route, "original-leg");
        let continuation = h
            .propose(&current, "original-leg", false, &[], 3.5)
            .unwrap();
        assert_eq!(
            [
                continuation.target.x,
                continuation.target.y,
                continuation.target.z
            ],
            [1.5, 1., 1.5]
        );
        assert!(!continuation.jump && continuation.cover_path.is_empty());
        assert_eq!(h.consume().as_deref(), Some("original-leg"));
        assert!(h.consume().is_none());
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none()
        );
        h.record(&prior, &route, "original-leg");
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none(),
            "spent ID cannot rearm"
        );
        h.record(&current, &continuation, "continuation-leg");
        assert!(
            h.pending.is_none(),
            "continuation cannot seed another hidden goal"
        );
    }
    #[test]
    fn running_or_releasing_input_waits_without_creating_an_action_or_spending_the_lease() {
        let (prior, route, mut current) = scene();
        let mut h = Handoff::default();
        h.record(&prior, &route, "original-leg");
        current.player["control"]["outcome"] = json!("running");
        current.player["control"]["input_released"] = Value::Null;
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none()
        );
        assert!(h.consume().is_none());
        current.world["sample_clock"]["nanos"] = json!("30");
        current.player["control"]["outcome"] = json!("observed");
        current.player["control"]["input_released"] = json!(false);
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none()
        );
        current.world["sample_clock"]["nanos"] = json!("40");
        current.player["control"]["input_released"] = json!(true);
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_some()
        );
    }
    #[test]
    fn current_facts_and_original_terminal_outcome_fence_every_continuation() {
        for change in [
            "actor",
            "life",
            "entity",
            "epoch",
            "dimension",
            "clock",
            "id",
            "target",
            "pose",
            "cancelled",
            "failed",
            "uncertain",
            "reconciled",
            "cancel_request",
            "expired",
            "unknown",
            "new_threat",
            "terrain",
            "missing_cell",
            "danger",
        ] {
            let (prior, route, mut current) = scene();
            let mut h = Handoff::default();
            h.record(&prior, &route, "original-leg");
            match change {
                "actor"=>current.player["uuid"]=json!("other"),
                "life"=>current.player["life_id"]=json!("new-life"),
                "entity"=>current.player["entity_id"]=json!(2),
                "epoch"=>current.world["sample_clock"]["epoch"]=json!("new-epoch"),
                "dimension"=>current.player["dimension"]=json!("fixture:other"),
                "clock"=>current.world["sample_clock"]["nanos"]=json!("9"),
                "id"=>current.player["control"]["action_id"]=json!("foreign-control"),
                "target"=>current.player["control"]["target"]=json!([2.5,1.,0.5]),
                "pose"=>current.player["position"]=json!([0.5,1.,0.5]),
                "cancelled"|"failed"|"uncertain"|"reconciled"=>current.player["control"]["outcome"]=json!(change),
                "cancel_request"=>current.player["control"]["cancel_requested"]=json!(true),
                "expired"=>current.world["entities"][0]["client_memory"]["age_ticks"]=json!(161),
                "unknown"=>current.world["entities"]=json!([]),
                "new_threat"=>current.world["entities"].as_array_mut().unwrap().push(json!({"uuid":"new-threat","visible":true,"hostile":true,"position":[2.5,1.,0.5]})),
                "terrain"=>{ let b=current.world["blocks"].as_array_mut().unwrap().iter_mut().find(|b|b["position"]==json!([1,1,1])).unwrap(); b["block"]=json!("minecraft:stone"); b["collision"]=json!([[0.,0.,0.,1.,1.,1.]]); },
                "missing_cell"=>current.world["blocks"].as_array_mut().unwrap().retain(|b|b["position"]!=json!([1,2,1])),
                _=>{}
            }
            assert!(
                h.propose(&current, "original-leg", change == "danger", &[], 3.5)
                    .is_none(),
                "{change}"
            );
            assert!(h.consume().is_none(), "{change} must not advance old input");
        }
    }
    #[test]
    fn hidden_frames_and_reacquired_threats_cannot_create_or_resurrect_cover_goals() {
        let (prior, route, mut current) = scene();
        let mut h = Handoff::default();
        h.record(&current, &route, "original-leg");
        assert!(h.pending.is_none(), "no new goal from a hidden pose");
        h.record(&prior, &route, "original-leg");
        current.world["entities"][0]["visible"] = json!(true);
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none()
        );
        current.world["entities"][0]["visible"] = json!(false);
        assert!(
            h.propose(&current, "original-leg", false, &[], 3.5)
                .is_none(),
            "old completed plan cannot return later"
        );
        h.record(&prior, &route, "new-original");
        current.player["control"]["action_id"] = json!("new-original");
        current.player["control"]["outcome"] = json!("running");
        assert!(
            h.propose(&current, "new-original", false, &[], 3.5)
                .is_none()
        );
        current.world["entities"][0]["visible"] = json!(true);
        current.world["sample_clock"]["nanos"] = json!("30");
        assert!(
            h.propose(&current, "new-original", false, &[], 3.5)
                .is_none()
        );
        assert!(
            h.pending.is_none(),
            "reacquisition requires new current planning"
        );
    }
    #[test]
    #[ignore = "Explicit private operator replay input; no embedded capture or native calls"]
    fn private_selected_cover_pair_replay() {
        use std::io::Read;
        let path = std::env::var_os("MC_PRIVATE_COVER_HANDOFF_PAIR")
            .expect("explicit operator input path");
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .unwrap()
            .take((2 * super::super::decision::MAX_BYTES + 4097) as u64)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(
            bytes.len() <= 2 * super::super::decision::MAX_BYTES + 4096,
            "bounded two-frame input required"
        );
        let input: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(input["format"], "private-cover-handoff-pair-v1");
        let before: Observation =
            serde_json::from_value(input["prior"]["snapshot"]["observation"].clone()).unwrap();
        let current: Observation =
            serde_json::from_value(input["current"]["snapshot"]["observation"].clone()).unwrap();
        let selected = &input["prior"]["snapshot"]["selected"];
        let route = geometry::Escape {
            target: serde_json::from_value(selected["target"].clone()).unwrap(),
            jump: selected["jump"].as_bool().unwrap(),
            reason: selected["reason"].as_str().unwrap().into(),
            cover_path: serde_json::from_value(selected["cover_path"].clone()).unwrap(),
        };
        let id = input["prior"]["action_id"].as_str().unwrap();
        let old: super::super::decision::Snapshot =
            serde_json::from_value(input["current"]["snapshot"].clone()).unwrap();
        let old = old.replay();
        assert_eq!(old["matches_recorded_choice"], true);
        let mut h = Handoff::default();
        h.record(&before, &route, id);
        let result = h
            .propose(&current, id, false, &[], 3.5)
            .expect("the preselected final leg remains freshly supported");
        let target = json!({"x":result.target.x,"y":result.target.y,"z":result.target.z});
        assert_eq!(
            target,
            selected["cover_path"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone()
        );
        assert_ne!(target, old["replayed"]["target"]);
        assert!(result.cover_path.is_empty());
        assert_eq!(h.consume().as_deref(), Some(id));
        println!(
            "{}",
            json!({"old_generic":old,"continued_target":target,"one_consumed_continuation":true,"scope":"Production helper offline replay only; no native motion or Survival proof"})
        );
    }
}
