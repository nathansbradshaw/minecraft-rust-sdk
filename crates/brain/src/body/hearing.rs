//! Client auditory facts and explicitly uncertain blast hypotheses. Never entities.
use super::geometry;
use crate::{Result, ensure};
use minecraft_sdk::{Observation, auditory};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Memory {
    scope: Option<(String, String, String, String, i64)>,
    tick: Option<i64>,
    cursor: auditory::Cursor,
    facts: Vec<auditory::Sound>,
    hazards: Vec<auditory::Sound>,
}
impl Memory {
    /// The caller persists this before any reaction. Repeated observations and
    /// process restarts cannot renew a fuse hypothesis's original emission time.
    pub fn update(&mut self, o: &mut Observation) -> Result<Value> {
        let frame = auditory::frame(o)?;
        let clock = o.world["sample_clock"]["epoch"]
            .as_str()
            .ok_or("Hearing clock missing")?;
        let life = o.player["entity_id"]
            .as_i64()
            .ok_or("Hearing life missing")?;
        let scope = (
            frame.listener_uuid.clone(),
            frame.dimension.clone(),
            clock.to_owned(),
            frame.epoch.clone(),
            life,
        );
        let tick = frame.sample_game_time;
        let changed = self.scope.as_ref().is_some_and(|old| old != &scope)
            || self.tick.is_some_and(|old| tick < old);
        if changed {
            self.facts.clear();
            self.hazards.clear();
        }
        self.scope = Some(scope);
        self.tick = Some(tick);
        let alive = o.player["health"].as_f64().is_some_and(|v| v > 0.);
        let fresh = self.cursor.fresh(o)?;
        if !alive {
            self.facts.clear();
            self.hazards.clear();
        } else {
            self.facts
                .retain(|sound| (0..=600).contains(&tick.saturating_sub(sound.game_time)));
            self.facts.extend(
                fresh
                    .iter()
                    .filter(|sound| !changed || sound.game_time >= tick)
                    .cloned(),
            );
            if self.facts.len() > 32 {
                self.facts.drain(..self.facts.len() - 32);
            }
            self.hazards
                .retain(|sound| (0..100).contains(&tick.saturating_sub(sound.game_time)));
            self.hazards.extend(
                fresh
                    .iter()
                    .filter(|sound| {
                        (!changed || sound.game_time >= tick)
                            && matches!(
                                sound.sound_id.as_str(),
                                "minecraft:entity.creeper.primed" | "minecraft:entity.tnt.primed"
                            )
                    })
                    .cloned(),
            );
            if self.hazards.len() > 4 {
                self.hazards.drain(..self.hazards.len() - 4);
            }
        }
        ensure(self.facts.len() <= 32, "Unbounded hearing memory")?;
        let hypotheses: Vec<_> = self.hazards.iter().rev().filter_map(|sound| {
            let (radius, ttl) = match sound.sound_id.as_str() {
                // Sound does not disclose charge state, current position or
                // exact fuse. Use charged-capable clearance and a finite lease.
                "minecraft:entity.creeper.primed" => (12., 40),
                "minecraft:entity.tnt.primed" => (8., 100),
                _ => return None,
            };
            let age = tick.checked_sub(sound.game_time)?;
            (age >= 0 && age < ttl).then(|| json!({
                "kind":"possible_blast_at_heard_origin","origin":sound.origin,"radius":radius,
                "sound_id":sound.sound_id,"sequence":sound.sequence,"emission_game_time":sound.game_time,
                "expires_game_time":sound.game_time.saturating_add(ttl),
                "scope":"Client precaution from sound; source identity, current position, charge and exact fuse unknown"
            }))
        }).take(4).collect();
        let recent: Vec<_> = self.facts.iter().rev().take(8).collect();
        o.world["client_hearing"] = json!({"format":"client-hearing-v1","epoch":frame.epoch,
            "sample_game_time":tick,"dropped":frame.dropped,"recent_emissions":recent,"hypotheses":hypotheses,
            "scope":"Remembered auditory emissions and client hypotheses; never visual entities or current hidden poses"});
        Ok(json!({"fresh":fresh,"epoch":frame.epoch,"game_time":tick,"dropped":frame.dropped}))
    }
}

pub(super) fn blasts(o: &Observation) -> Vec<([f64; 3], f64)> {
    let Some(tick) = o.world["game_time"].as_i64() else {
        return Vec::new();
    };
    if o.world["client_hearing"]["sample_game_time"].as_i64() != Some(tick) {
        return Vec::new();
    }
    let Some(start) = geometry::point(&o.player["position"]) else {
        return Vec::new();
    };
    o.world["client_hearing"]["hypotheses"]
        .as_array()
        .into_iter()
        .flatten()
        .take(4)
        .filter_map(|h| {
            if h["kind"] != "possible_blast_at_heard_origin"
                || h["emission_game_time"].as_i64().is_none_or(|v| v > tick)
                || h["expires_game_time"].as_i64().is_none_or(|v| v <= tick)
            {
                return None;
            }
            let p = geometry::point(&h["origin"])?;
            let radius = h["radius"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0. && *v <= 12.)?;
            let distance = (0..3)
                .map(|i| (p[i] - start[i]).powi(2))
                .sum::<f64>()
                .sqrt();
            (distance <= radius + 2.).then_some((p, radius))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scene(tick: i64, sound: &str) -> Observation {
        Observation {
            revision: tick.to_string(),
            menu: Value::Null,
            player: json!({"uuid":"own","entity_id":1,"health":20.,"dimension":"minecraft:overworld","position":[0.5,1.,0.5]}),
            world: json!({"game_time":tick,"sample_clock":{"epoch":"server"},"entities":[],"auditory":{
                "format":"minecraft-sdk-auditory-v1","epoch":"one","listener_uuid":"own","dimension":"minecraft:overworld",
                "sample_game_time":tick,"latest_sequence":1,"dropped":0,"retention_ticks":100,"scope":"emission only",
                "events":[{"sequence":1,"sound_id":sound,"category":"minecraft:hostile","origin":[5.5,1.,0.5],
                    "game_time":100,"volume":1.,"pitch":1.,"delivery":"position_sound","server_range":16.,"distance_at_delivery":5.}]}}),
        }
    }
    #[test]
    fn unheard_entity_is_not_invented_and_reflex_expires_across_restart() {
        let mut memory = Memory::default();
        let mut o = scene(100, "minecraft:entity.creeper.primed");
        assert_eq!(
            memory.update(&mut o).unwrap()["fresh"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(blasts(&o).len(), 1);
        assert!(o.world["entities"].as_array().unwrap().is_empty());
        let mut memory: Memory =
            serde_json::from_slice(&serde_json::to_vec(&memory).unwrap()).unwrap();
        let mut o = scene(139, "minecraft:entity.creeper.primed");
        assert!(
            memory.update(&mut o).unwrap()["fresh"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(blasts(&o).len(), 1);
        let mut o = scene(140, "minecraft:entity.creeper.primed");
        memory.update(&mut o).unwrap();
        assert!(blasts(&o).is_empty());
        assert_eq!(
            o.world["client_hearing"]["recent_emissions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn footsteps_are_context_not_automatic_hostility_and_old_life_cues_do_not_rearm() {
        let mut memory = Memory::default();
        let mut o = scene(100, "minecraft:block.glass.step");
        memory.update(&mut o).unwrap();
        assert!(blasts(&o).is_empty());
        assert_eq!(
            o.world["client_hearing"]["recent_emissions"][0]["sound_id"],
            "minecraft:block.glass.step"
        );
        for field in ["entity_id", "dimension", "health"] {
            let mut memory = Memory::default();
            memory
                .update(&mut scene(100, "minecraft:entity.creeper.primed"))
                .unwrap();
            let mut next = scene(101, "minecraft:entity.creeper.primed");
            match field {
                "entity_id" => next.player[field] = json!(2),
                "dimension" => {
                    next.player[field] = json!("minecraft:the_end");
                    next.world["auditory"][field] = json!("minecraft:the_end");
                }
                _ => next.player[field] = json!(0.),
            }
            memory.update(&mut next).unwrap();
            assert!(blasts(&next).is_empty(), "{field}");
        }
    }
    #[test]
    fn heard_hiss_selects_known_supported_escape_and_decision_replays() {
        let mut o = scene(100, "minecraft:entity.creeper.primed");
        o.player["on_ground"] = json!(true);
        let mut cells = Vec::new();
        for x in -6..=6 {
            for z in -6..=6 {
                for y in 0..=3 {
                    cells.push(json!({"position":[x,y,z],"block":if y == 0 {"minecraft:stone"} else {"minecraft:air"},
                "collision":if y == 0 {json!([[0.,0.,0.,1.,1.,1.]])} else {json!([])}}));
                }
            }
        }
        o.world["blocks"] = json!(cells);
        let mut memory = Memory::default();
        memory.update(&mut o).unwrap();
        let route = geometry::evade(&o).unwrap();
        assert!(route.target.x < 0.5);
        assert!(route.reason.contains("heard"));
        assert!(geometry::threatened(&o));
        let snapshot =
            super::super::decision::Snapshot::capture(&o, &[], &[], &[], 3.5, &[], &route);
        assert_eq!(snapshot.replay()["matches_recorded_choice"], true);
        o.world["game_time"] = json!(140);
        o.world["auditory"]["sample_game_time"] = json!(140);
        memory.update(&mut o).unwrap();
        assert!(geometry::evade(&o).is_none());
        // A burst of unrelated activity cannot erase a still-live alarm.
        let mut o = scene(100, "minecraft:entity.creeper.primed");
        let mut memory = Memory::default();
        memory.update(&mut o).unwrap();
        o.world["game_time"] = json!(101);
        o.world["auditory"]["sample_game_time"] = json!(101);
        o.world["auditory"]["latest_sequence"] = json!(65);
        let mut events = Vec::new();
        for sequence in 2..=65 {
            let mut sound = o.world["auditory"]["events"][0].clone();
            sound["sequence"] = json!(sequence);
            sound["sound_id"] = json!("minecraft:block.glass.step");
            events.push(sound);
        }
        o.world["auditory"]["events"] = json!(events);
        memory.update(&mut o).unwrap();
        assert_eq!(blasts(&o).len(), 1);
        assert_eq!(memory.facts.len(), 32);
    }
    #[test]
    fn default_body_refuses_missing_hearing_before_starting_a_character_controller() {
        let root = tempfile::tempdir().unwrap();
        let profile = minecraft_sdk::Profile {
            endpoint: "https://example.invalid/".into(),
            instance_id: "instance".into(),
            session: None,
            credential: minecraft_sdk::Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture".into(),
            },
        };
        let server = minecraft_sdk::ServerInfo {
            api_version: 1,
            instance_id: "instance".into(),
            minecraft_version: "1.21.1".into(),
            platform: "neoforge".into(),
            adapter_version: "fixture".into(),
            ready: true,
            limits: json!({}),
            capabilities: [
                "minecraft:finite_locomotion",
                "minecraft:survival_facts",
                "minecraft:ranged_observation",
                "minecraft:pending_actions",
                "minecraft:session_material_preservation",
            ]
            .iter()
            .map(|v| v.to_string())
            .collect(),
        };
        let error = super::super::Handle::start_for_server(
            profile,
            super::super::Config::default(),
            root.path(),
            scene(100, "minecraft:entity.creeper.primed"),
            &server,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains(auditory::CAPABILITY));
        assert!(!root.path().join("actions.sock").exists());
    }
}
