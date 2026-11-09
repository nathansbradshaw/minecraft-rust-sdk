//! Client memory of visible ranged threats; never reads unseen current poses.
use minecraft_sdk::Observation;
use serde_json::{Value, json};
use std::{borrow::Cow, collections::BTreeMap};
#[derive(Default)]
pub(super) struct RangedMemory {
    scope: Option<(String, String, String, i64)>,
    tick: Option<i64>,
    seen: BTreeMap<String, (i64, Value)>,
}
impl RangedMemory {
    pub fn policy<'a>(&mut self, o: &'a Observation) -> Cow<'a, Observation> {
        let binding = (|| {
            Some((
                o.player["uuid"].as_str()?.to_owned(),
                o.player["dimension"].as_str()?.to_owned(),
                o.world["sample_clock"]["epoch"].as_str()?.to_owned(),
                o.player["entity_id"].as_i64()?,
            ))
        })();
        let Some(tick) = o.world["game_time"].as_i64() else {
            self.seen.clear();
            return Cow::Borrowed(o);
        };
        if binding.is_none()
            || self.scope != binding
            || self.tick.is_some_and(|old| tick < old)
            || o.player["health"].as_f64().is_none_or(|h| h <= 0.)
        {
            self.seen.clear();
        }
        self.scope = binding;
        self.tick = Some(tick);
        if self.scope.is_none() || o.player["health"].as_f64().is_none_or(|h| h <= 0.) {
            return Cow::Borrowed(o);
        }
        self.seen
            .retain(|_, (at, _)| tick.saturating_sub(*at) <= 160);
        let rows = o.world["entities"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for enemy in rows.iter().filter(|e| e["visible"] == true) {
            let Some(id) = enemy["uuid"].as_str() else {
                continue;
            };
            if enemy["hostile"] != true
                || enemy["ranged_attack"] != true
                || enemy["health"].as_f64().is_none_or(|h| h <= 0.)
            {
                self.seen.remove(id);
            } else if self.seen.contains_key(id) || self.seen.len() < 8 {
                self.seen.insert(id.to_owned(), (tick, enemy.clone()));
            }
        }
        let recalled:Vec<_>=self.seen.iter().filter(|(id,_)|
            !rows.iter().any(|e|e["uuid"].as_str()==Some(id.as_str())&&e["visible"]==true))
            .map(|(_,(at,enemy))| {
                let mut estimate=enemy.clone();
                estimate["visible"]=json!(false);
                estimate["client_memory"]=json!({"kind":"last_visible_ranged_threat","last_seen_game_time":at,
                    "age_ticks":tick-at,"expires_after_ticks":160,
                    "scope":"Last observed pose only; current location and continued presence unknown"});
                estimate
            }).collect();
        if recalled.is_empty() {
            return Cow::Borrowed(o);
        }
        let mut policy = o.clone();
        if let Some(entities) = policy.world["entities"].as_array_mut() {
            entities.extend(recalled);
        }
        Cow::Owned(policy)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scene(tick: i64) -> Observation {
        Observation {
            revision: tick.to_string(),
            player: json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","health":20.}),
            world: json!({"game_time":tick,"sample_clock":{"epoch":"server"},"entities":[{"uuid":"archer","visible":true,
                "hostile":true,"ranged_attack":true,"health":20.,"position":[10.,1.,0.]}]}),
            menu: Value::Null,
        }
    }
    #[test]
    fn hidden_native_coordinates_cannot_update_last_seen_memory_and_it_expires() {
        let mut memory = RangedMemory::default();
        memory.policy(&scene(10));
        let mut hidden = scene(11);
        hidden.world["entities"][0]["visible"] = json!(false);
        hidden.world["entities"][0]["position"] = json!([999., 1., 999.]);
        let policy = memory.policy(&hidden);
        let recalled = &policy.world["entities"][1];
        assert_eq!(recalled["position"], json!([10., 1., 0.]));
        assert_eq!(recalled["visible"], false);
        assert_eq!(recalled["client_memory"]["age_ticks"], 1);
        assert_eq!(hidden.world["entities"].as_array().unwrap().len(), 1);
        hidden.world["game_time"] = json!(171);
        assert_eq!(
            memory.policy(&hidden).world["entities"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn death_respawn_clock_or_dimension_changes_cannot_recall_another_life() {
        for (field, value) in [
            ("entity_id", json!(2)),
            ("dimension", json!("minecraft:the_nether")),
            ("health", json!(0.)),
        ] {
            let mut memory = RangedMemory::default();
            memory.policy(&scene(10));
            let mut hidden = scene(11);
            hidden.world["entities"] = json!([]);
            hidden.player[field] = value;
            assert!(
                memory.policy(&hidden).world["entities"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let mut memory = RangedMemory::default();
        memory.policy(&scene(10));
        let mut old = scene(9);
        old.world["entities"] = json!([]);
        assert!(
            memory.policy(&old).world["entities"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
