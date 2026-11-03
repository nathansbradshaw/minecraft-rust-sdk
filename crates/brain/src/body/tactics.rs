//! Client policy: fresh native evidence drives defense and local replanning.
use super::geometry::point;
use minecraft_sdk::Observation;
use std::collections::BTreeMap;

/// Client inference from advancing native positions. An embedded arrow can
/// retain a nonzero reported velocity; that alone does not establish flight.
#[derive(Default)]
pub(super) struct Projectiles {
    lane: Option<(String, String, String, i64)>,
    tick: Option<i64>,
    anchors: BTreeMap<String, ([f64; 3], [f64; 3], i64)>,
}
impl Projectiles {
    pub fn stationary(&mut self, o: &Observation) -> Vec<String> {
        let scope = || {
            Some((
                o.player["uuid"]
                    .as_str()
                    .filter(|s| !s.is_empty())?
                    .to_owned(),
                o.player["dimension"]
                    .as_str()
                    .filter(|s| !s.is_empty())?
                    .to_owned(),
                o.world["sample_clock"]["epoch"]
                    .as_str()
                    .filter(|s| !s.is_empty())?
                    .to_owned(),
                o.player["entity_id"].as_i64().filter(|id| *id >= 0)?,
            ))
        };
        let (Some(lane), Some(tick)) = (scope(), o.world["game_time"].as_i64()) else {
            *self = Self::default();
            return Vec::new();
        };
        if self.lane.as_ref() != Some(&lane)
            || self
                .tick
                .is_some_and(|last| tick < last || tick.saturating_sub(last) > 20)
        {
            self.anchors.clear();
        }
        self.lane = Some(lane);
        self.tick = Some(tick);
        let mut next = BTreeMap::new();
        let mut stationary = Vec::new();
        for e in o.world["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["visible"] == true && e["projectile"] == true)
            .take(256)
        {
            let (Some(id), Some(p), Some(v)) = (
                super::geometry::projectile_key(e),
                point(&e["position"]),
                point(&e["velocity"]),
            ) else {
                continue;
            };
            let unchanged = self.anchors.get(&id).filter(|(q, w, _)| {
                (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>() <= 1e-8
                    && (0..3).map(|i| (v[i] - w[i]).powi(2)).sum::<f64>() <= 0.01
            });
            let anchor = unchanged.copied().unwrap_or((p, v, tick));
            if tick.saturating_sub(anchor.2) >= 2 {
                stationary.push(id.clone());
            }
            next.insert(id, anchor);
        }
        self.anchors = next;
        stationary
    }
}

/// An escape that makes no meaningful displacement for 14 advancing game ticks
/// needs another route. Wall time alone cannot distinguish a wall from lag.
#[derive(Default)]
pub(super) struct Progress {
    lane: Option<(String, String, String, i64)>,
    anchor: Option<([f64; 3], i64)>,
}
impl Progress {
    pub fn stuck(&mut self, o: &Observation, owned: &str) -> bool {
        let valid = || {
            let c = &o.player["control"];
            if owned.is_empty()
                || c["outcome"] != "running"
                || c["action_id"].as_str()? != owned
                || c["cancel_requested"] == true
            {
                return None;
            }
            Some((
                (
                    owned.to_owned(),
                    o.player["dimension"].as_str()?.to_owned(),
                    o.world["sample_clock"]["epoch"].as_str()?.to_owned(),
                    o.player["entity_id"].as_i64()?,
                ),
                point(&o.player["position"])?,
                o.world["game_time"].as_i64()?,
            ))
        };
        let Some((lane, p, tick)) = valid() else {
            *self = Self::default();
            return false;
        };
        let reset = self.lane.as_ref() != Some(&lane)
            || self.anchor.is_none_or(|(q, t)| {
                tick < t || (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>() >= 0.04
            });
        if reset {
            self.lane = Some(lane);
            self.anchor = Some((p, tick));
            return false;
        }
        self.anchor
            .is_some_and(|(_, t)| tick.saturating_sub(t) >= 14)
    }
}

/// An attack is optional knockback while escaping, never a reason to pursue a
/// hostile. Server reach, sight, cooldown and claim checks remain authoritative.
pub(super) fn defensive_target(o: &Observation) -> Option<i32> {
    if o.player["health"].as_f64()? <= 0.
        || o.player["attack_strength"].as_f64()? < 0.9
        || o.player["using_item"] == true
        || o.player["interaction"]["outcome"] == "running"
    {
        return None;
    }
    let p = point(&o.player["position"])?;
    o.world["entities"]
        .as_array()?
        .iter()
        .filter(|e| e["hostile"] == true && e["visible"] == true)
        .filter_map(|e| {
            let q = point(&e["position"])?;
            let distance = (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>();
            (distance <= 9.).then_some((distance, i32::try_from(e["entity_id"].as_i64()?).ok()?))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Observation {
        Observation {
            revision: "1".into(),
            player: json!({"entity_id":1,"dimension":"minecraft:overworld","position":[0.,64.,0.],"health":20.,"attack_strength":1.,"control":{"action_id":"escape","outcome":"running"}}),
            world: json!({"game_time":100,"sample_clock":{"epoch":"server-a"},"entities":[{"entity_id":9,"hostile":true,"visible":true,"position":[2.,64.,0.]}]}),
            menu: json!({}),
        }
    }
    fn arrow() -> Observation {
        let mut o = fixture();
        o.player["uuid"] = json!("own");
        o.player["position"] = json!([0.5, 1., 0.5]);
        o.world["entities"] = json!([{"uuid":"arrow-a","entity_id":10,"visible":true,"projectile":true,"position":[2.5,1.9,0.5],"velocity":[-0.25,0.,0.]}]);
        o
    }
    #[test]
    fn retained_velocity_is_not_flight_when_native_positions_are_stationary() {
        let mut o = arrow();
        let mut motion = Projectiles::default();
        assert_eq!(
            super::super::geometry::incoming_projectiles(&o),
            vec!["uuid:arrow-a"]
        );
        assert!(motion.stationary(&o).is_empty());
        for _ in 0..20 {
            assert!(
                motion.stationary(&o).is_empty(),
                "Repeated cached tick is not new evidence"
            );
        }
        o.world["game_time"] = json!(101);
        assert!(motion.stationary(&o).is_empty());
        o.world["game_time"] = json!(102);
        let stationary = motion.stationary(&o);
        assert_eq!(stationary, vec!["uuid:arrow-a"]);
        assert!(super::super::geometry::incoming_with_stationary(&o, &stationary).is_empty());
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for y in 0..=4 {
                for z in -4..=4 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},"collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        o.world["blocks"] = json!(blocks);
        assert!(
            super::super::geometry::evade(&o).is_some(),
            "Stateless single-frame prediction stays conservative"
        );
        assert!(
            super::super::geometry::evade_with_stationary(&o, &[], &stationary).is_none(),
            "An observed stationary arrow alone does not require another escape"
        );
        assert_eq!(
            o.world["entities"][0]["velocity"],
            json!([-0.25, 0., 0.]),
            "Client classification must not overwrite native facts"
        );
        o.world["game_time"] = json!(103);
        o.world["entities"][0]["position"] = json!([2.25, 1.9, 0.5]);
        assert!(
            motion.stationary(&o).is_empty(),
            "Observed movement restores immediate flight response"
        );
        o.world["entities"][0]["uuid"] = json!("new-arrow");
        assert!(
            motion.stationary(&o).is_empty(),
            "New identity never inherits stationary history"
        );
        assert_eq!(
            super::super::geometry::incoming_with_stationary(&o, &[]),
            vec!["uuid:new-arrow"]
        );
    }
    #[test]
    fn projectile_history_resets_on_life_dimension_epoch_clock_gaps_and_absence() {
        for changed in [
            "life",
            "dimension",
            "epoch",
            "rollback",
            "gap",
            "absent",
            "velocity",
        ] {
            let mut o = arrow();
            let mut motion = Projectiles::default();
            motion.stationary(&o);
            o.world["game_time"] = json!(102);
            assert!(!motion.stationary(&o).is_empty());
            match changed {
                "life" => o.player["entity_id"] = json!(2),
                "dimension" => o.player["dimension"] = json!("other:dimension"),
                "epoch" => o.world["sample_clock"]["epoch"] = json!("new"),
                "rollback" => o.world["game_time"] = json!(1),
                "gap" => o.world["game_time"] = json!(130),
                "absent" => {
                    o.world["entities"] = json!([]);
                    motion.stationary(&o);
                    o.world["entities"] = arrow().world["entities"].clone();
                }
                "velocity" => o.world["entities"][0]["velocity"] = json!([-1., 0., 0.]),
                _ => unreachable!(),
            }
            assert!(motion.stationary(&o).is_empty(), "{changed}");
        }
        let mut o = arrow();
        o.world["entities"]=json!((0..512).map(|n|json!({"uuid":format!("arrow-{n}"),"visible":true,"projectile":true,"position":[2.5,1.9,0.5],"velocity":[-0.25,0.,0.]})).collect::<Vec<_>>());
        let mut motion = Projectiles::default();
        motion.stationary(&o);
        assert_eq!(motion.anchors.len(), 256, "Bounded observed history");
    }
    #[test]
    fn defense_remains_available_during_movement_but_not_during_item_use() {
        let mut o = fixture();
        assert_eq!(defensive_target(&o), Some(9));
        o.player["using_item"] = json!(true);
        assert_eq!(defensive_target(&o), None);
        o.player["using_item"] = json!(false);
        o.player["interaction"] = json!({"outcome":"running"});
        assert_eq!(defensive_target(&o), None);
        o.player["interaction"] = json!({});
        o.player["attack_strength"] = json!(0.5);
        assert_eq!(defensive_target(&o), None);
        o.player["attack_strength"] = json!(1.);
        o.world["entities"][0]["visible"] = json!(false);
        assert_eq!(defensive_target(&o), None);
        o.world["entities"][0]["visible"] = json!(true);
        o.world["entities"][0]["position"] = json!([4., 64., 0.]);
        assert_eq!(defensive_target(&o), None);
    }
    #[test]
    fn lag_and_new_life_are_not_stuck_routes_but_advancing_ticks_are() {
        let mut o = fixture();
        let mut p = Progress::default();
        assert!(!p.stuck(&o, "escape"));
        for _ in 0..100 {
            assert!(!p.stuck(&o, "escape"));
        }
        o.world["game_time"] = json!(114);
        assert!(p.stuck(&o, "escape"));
        o.player["entity_id"] = json!(2);
        assert!(!p.stuck(&o, "escape"));
        o.world["game_time"] = json!(128);
        o.player["position"] = json!([0.3, 64., 0.]);
        assert!(!p.stuck(&o, "escape"));
        o.world["game_time"] = json!(142);
        assert!(p.stuck(&o, "escape"));
        o.world["sample_clock"]["epoch"] = json!("server-b");
        assert!(!p.stuck(&o, "escape"));
        o.world["game_time"] = json!(1);
        assert!(!p.stuck(&o, "escape"));
        o.player["control"]["action_id"] = json!("program-owned");
        o.world["game_time"] = json!(1000);
        assert!(!p.stuck(&o, "escape"));
    }
}
