//! Client policy: fresh native evidence drives defense and local replanning.
use super::geometry::point;
use minecraft_sdk::Observation;

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
