//! Client policy: fresh native evidence drives defense and local replanning.
use super::geometry::point;
use minecraft_sdk::{Action, Hand, Observation, click_mode};
use std::collections::BTreeMap;

// Replaceable client policy. Native nutrition alone doesn't establish that
// modded food is safe. Start with ordinary prepared foods whose use is known.
const MEALS: &[&str] = &[
    "minecraft:apple",
    "minecraft:baked_potato",
    "minecraft:bread",
    "minecraft:carrot",
    "minecraft:cooked_beef",
    "minecraft:cooked_chicken",
    "minecraft:cooked_cod",
    "minecraft:cooked_mutton",
    "minecraft:cooked_porkchop",
    "minecraft:cooked_rabbit",
    "minecraft:cooked_salmon",
    "minecraft:golden_carrot",
];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Meal {
    pub slot: u8,
    pub item: String,
    pub stock: u64,
    pub hunger: u64,
    selected: u8,
    ticks: u16,
    nutrition: u64,
    saturation: f64,
    preparation: Option<MealSwap>,
}
#[derive(Clone, Debug)]
struct MealSwap {
    menu_id: i32,
    state_id: i32,
    source: i16,
    hotbar: u8,
}
impl PartialEq for MealSwap {
    fn eq(&self, other: &Self) -> bool {
        // A menu revision alone is not a new meal/preparation opportunity.
        // The fresh revision is still sent to native click admission.
        self.menu_id == other.menu_id && self.source == other.source && self.hotbar == other.hotbar
    }
}
fn meal_swap(
    o: &Observation,
    source: u8,
    item: &str,
    stock: u64,
    selected: u8,
) -> Option<MealSwap> {
    if o.menu["type"] != "minecraft:inventory"
        || o.menu["carried"]["item"] != "minecraft:air"
        || o.menu["carried"]["count"] != 0
    {
        return None;
    }
    let state_id = i32::try_from(o.menu["state_id"].as_i64()?)
        .ok()
        .filter(|n| *n >= 0)?;
    let rows = o.menu["slots"].as_array()?;
    if rows.len() > 512 {
        return None;
    }
    let bound = |inventory_slot: u8, expected: &str, count: u64| {
        let mut matches = rows.iter().filter(|row| {
            row["player_inventory"] == true
                && row["inventory_slot"].as_u64() == Some(u64::from(inventory_slot))
        });
        let row = matches.next()?;
        if matches.next().is_some()
            || row["item"] != expected
            || row["count"] != count
            || row["may_pickup"] != true
        {
            return None;
        }
        let slot = i16::try_from(row["slot"].as_i64()?).ok().filter(|slot| {
            *slot >= 0 && usize::try_from(*slot).is_ok_and(|slot| slot < rows.len())
        })?;
        Some((row, slot))
    };
    let (_, source_slot) = bound(source, item, stock)?;
    let inventory = o.player["inventory"].as_array()?;
    let hotbar = inventory
        .iter()
        .filter_map(|row| {
            let slot = u8::try_from(row["slot"].as_u64()?).ok()?;
            if slot > 8
                || row["item"] != "minecraft:air"
                || row["count"] != 0
                || inventory
                    .iter()
                    .filter(|row| row["slot"].as_u64() == Some(u64::from(slot)))
                    .count()
                    != 1
            {
                return None;
            }
            let (native, _) = bound(slot, "minecraft:air", 0)?;
            (native["may_place_carried"] == true).then_some(slot)
        })
        .min_by_key(|slot| (*slot != selected, *slot))?;
    Some(MealSwap {
        menu_id: 0,
        state_id,
        source: source_slot,
        hotbar,
    })
}
impl Meal {
    pub fn action(&self) -> Action {
        if let Some(swap) = &self.preparation {
            Action::ClickSlot {
                menu_id: swap.menu_id,
                state_id: swap.state_id,
                slot: swap.source,
                button: swap.hotbar,
                mode: click_mode::SWAP,
            }
        } else if self.slot <= 8 && self.slot != self.selected {
            Action::SelectSlot { slot: self.slot }
        } else {
            Action::StartUseItem {
                hand: if self.slot == 40 {
                    Hand::Off
                } else {
                    Hand::Main
                },
                max_ticks: self.ticks + 8,
            }
        }
    }
}
pub(super) fn meal(o: &Observation) -> Option<Meal> {
    let player = &o.player;
    let hunger = player["food"].as_u64()?;
    let health = player["health"].as_f64()?;
    let max_health = player["max_health"].as_f64()?;
    if health <= 0.0
        || !(hunger <= 14 || (hunger < 18 && health + 0.1 < max_health))
        || player["using_item"] != false
        || player["on_ground"] != true
        || player["sprinting"] == true
        || player["control"]["outcome"] == "running"
        || player["interaction"]["outcome"] == "running"
        || o.menu["menu_id"] != 0
    {
        return None;
    }
    let selected = u8::try_from(player["selected"].as_u64()?)
        .ok()
        .filter(|slot| *slot <= 8)?;
    player["inventory"]
        .as_array()?
        .iter()
        .filter_map(|row| {
            let slot = u8::try_from(row["slot"].as_u64()?).ok()?;
            let item = row["item"].as_str()?;
            let stock = row["count"].as_u64()?;
            let ticks = u16::try_from(row["use_duration_ticks"].as_u64()?).ok()?;
            let food = &row["food"];
            let nutrition = food["nutrition"].as_u64()?;
            let saturation = food["saturation"].as_f64()?;
            if !(slot <= 35 || slot == 40)
                || stock == 0
                || !(1..=64).contains(&ticks)
                || nutrition == 0
                || row["use_animation"] != "EAT"
                || row["on_cooldown"] != false
                || food["can_eat"] != true
                || !MEALS.contains(&item)
            {
                return None;
            }
            let preparation = if (9..=35).contains(&slot) {
                Some(meal_swap(o, slot, item, stock, selected)?)
            } else {
                None
            };
            Some(Meal {
                slot,
                item: item.to_owned(),
                stock,
                hunger,
                selected,
                ticks,
                nutrition,
                saturation,
                preparation,
            })
        })
        .max_by(|a, b| {
            a.nutrition
                .min(20 - hunger)
                .cmp(&b.nutrition.min(20 - hunger))
                .then(a.saturation.total_cmp(&b.saturation))
                .then(
                    (a.slot == selected || a.slot == 40).cmp(&(b.slot == selected || b.slot == 40)),
                )
        })
}

/// Keep a short trail of positions the player actually reached under threat.
/// This is local route feedback, never evidence of unseen walkable terrain.
#[derive(Default)]
pub(super) struct RetreatTrail {
    scope: Option<(String, String, String, i64)>,
    last: Option<([f64; 3], i64)>,
    points: Vec<([f64; 3], i64)>,
}
impl RetreatTrail {
    pub fn update(&mut self, o: &Observation, danger: bool) -> Vec<[f64; 3]> {
        let sample = || {
            Some((
                (
                    o.player["uuid"].as_str()?.to_owned(),
                    o.player["dimension"].as_str()?.to_owned(),
                    o.world["sample_clock"]["epoch"].as_str()?.to_owned(),
                    o.player["entity_id"].as_i64()?,
                ),
                point(&o.player["position"])?,
                o.world["game_time"].as_i64()?,
            ))
        };
        let Some((scope, p, tick)) = sample() else {
            *self = Self::default();
            return Vec::new();
        };
        let distance =
            |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>();
        if self.scope.as_ref() != Some(&scope)
            || self.last.is_some_and(|(q, t)| {
                tick < t || tick.saturating_sub(t) > 20 || distance(q, p) > 64.
            })
            || o.player["health"].as_f64().is_some_and(|h| h <= 0.0)
        {
            self.points.clear();
        }
        self.scope = Some(scope);
        self.last = Some((p, tick));
        self.points.retain(|&(_, t)| tick.saturating_sub(t) <= 160);
        if danger
            && self
                .points
                .last()
                .is_none_or(|&(q, _)| distance(q, p) >= 2.25)
        {
            if self.points.len() == 12 {
                self.points.remove(0);
            }
            self.points.push((p, tick));
        }
        self.points.iter().map(|(position, _)| *position).collect()
    }
}

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
fn runtime_attack_reach(o: &Observation, entity: &serde_json::Value) -> Option<bool> {
    let eye = o.player["eye_position"]
        .as_array()
        .filter(|a| a.len() == 3)?;
    let eye = [eye[0].as_f64()?, eye[1].as_f64()?, eye[2].as_f64()?];
    let range = o.player["entity_interaction_range"].as_f64()?;
    if !eye.iter().all(|n| n.is_finite()) || !range.is_finite() || range < 0. {
        return None;
    }
    let values = entity["bounds"].as_array().filter(|a| a.len() == 6)?;
    let mut bounds = [0.; 6];
    for (bound, value) in bounds.iter_mut().zip(values) {
        *bound = value.as_f64().filter(|n| n.is_finite())?;
    }
    if (0..3).any(|i| bounds[i] > bounds[i + 3]) {
        return None;
    }
    let gap: [f64; 3] = std::array::from_fn(|i| eye[i] - eye[i].clamp(bounds[i], bounds[i + 3]));
    Some(gap[0].hypot(gap[1]).hypot(gap[2]) <= range)
}
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
        .filter(|e| {
            e["hostile"] == true
                && e["visible"] == true
                && super::geometry::primed_blast(e).is_none()
        })
        .filter_map(|e| {
            let q = point(&e["position"])?;
            let distance = (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>();
            // New facts affect eligibility only. Missing/malformed facts keep
            // the exact old feet-centre fallback and target ordering. A valid
            // runtime denial never falls back to the broader legacy rule.
            runtime_attack_reach(o, e)
                .unwrap_or(distance <= 9.)
                .then_some((distance, i32::try_from(e["entity_id"].as_i64()?).ok()?))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn stored_meal() -> Observation {
        let mut o = fixture();
        o.player["food"] = json!(12);
        o.player["max_health"] = json!(20.);
        o.player["using_item"] = json!(false);
        o.player["on_ground"] = json!(true);
        o.player["sprinting"] = json!(false);
        o.player["control"] = Value::Null;
        o.player["selected"] = json!(0);
        o.player["inventory"] = json!([
            {"slot":0,"item":"minecraft:wooden_pickaxe","count":1},
            {"slot":1,"item":"minecraft:air","count":0},
            {"slot":9,"item":"minecraft:bread","count":2,"use_duration_ticks":32,
                "use_animation":"EAT","on_cooldown":false,
                "food":{"can_eat":true,"nutrition":5,"saturation":6.0}}
        ]);
        // Runtime menu binding, deliberately independent of hotbar slot number.
        o.menu = json!({"menu_id":0,"state_id":7,"type":"minecraft:inventory",
        "carried":{"item":"minecraft:air","count":0},"slots":[
            {"slot":0,"player_inventory":false,"inventory_slot":0,"item":"minecraft:air","count":0},
            {"slot":1,"player_inventory":true,"inventory_slot":9,"item":"minecraft:bread","count":2,"may_pickup":true},
            {"slot":2,"player_inventory":true,"inventory_slot":1,"item":"minecraft:air","count":0,"may_pickup":true,"may_place_carried":true}
        ]});
        o
    }
    #[test]
    fn safe_stored_food_moves_only_to_a_confirmed_empty_hotbar_then_selects_and_uses() {
        let mut o = stored_meal();
        let prepared = meal(&o).expect("known bread can be prepared");
        assert!(matches!(
            prepared.action(),
            Action::ClickSlot {
                menu_id: 0,
                state_id: 7,
                slot: 1,
                button: 1,
                mode: click_mode::SWAP
            }
        ));
        // A fresh native snapshot of the swap drives the next step. The held
        // pickaxe in slot0 was never a swap destination or cursor material.
        let mut bread = o.player["inventory"][2].clone();
        bread["slot"] = json!(1);
        o.player["inventory"][1] = bread;
        o.player["inventory"][2] = json!({"slot":9,"item":"minecraft:air","count":0});
        o.menu["state_id"] = json!(8);
        o.menu["slots"][1]["item"] = json!("minecraft:air");
        o.menu["slots"][1]["count"] = json!(0);
        o.menu["slots"][2]["item"] = json!("minecraft:bread");
        o.menu["slots"][2]["count"] = json!(2);
        assert_ne!(meal(&o).unwrap(), prepared);
        assert!(matches!(
            meal(&o).unwrap().action(),
            Action::SelectSlot { slot: 1 }
        ));
        assert_eq!(o.player["inventory"][0]["item"], "minecraft:wooden_pickaxe");
        o.player["selected"] = json!(1);
        assert!(matches!(
            meal(&o).unwrap().action(),
            Action::StartUseItem {
                hand: Hand::Main,
                max_ticks: 40
            }
        ));
    }
    #[test]
    fn stored_meal_refuses_unknown_space_cursor_or_conflicting_native_bindings() {
        for kind in [
            "occupied",
            "missing_inventory",
            "missing_native",
            "cursor",
            "source_mismatch",
            "locked",
            "duplicate_source",
            "duplicate_inventory",
            "foreign_menu",
            "unknown_state",
        ] {
            let mut o = stored_meal();
            match kind {
                "occupied" => {
                    o.player["inventory"][1]["item"] = json!("minecraft:stone");
                    o.player["inventory"][1]["count"] = json!(64);
                }
                "missing_inventory" => {
                    o.player["inventory"].as_array_mut().unwrap().remove(1);
                }
                "missing_native" => {
                    o.menu["slots"].as_array_mut().unwrap().pop();
                }
                "cursor" => {
                    o.menu["carried"] = json!({"item":"minecraft:wooden_pickaxe","count":1});
                }
                "source_mismatch" => {
                    o.menu["slots"][1]["count"] = json!(1);
                }
                "locked" => {
                    o.menu["slots"][2]["may_place_carried"] = json!(false);
                }
                "duplicate_source" => {
                    let row = o.menu["slots"][1].clone();
                    o.menu["slots"].as_array_mut().unwrap().push(row);
                }
                "duplicate_inventory" => {
                    let row = o.player["inventory"][1].clone();
                    o.player["inventory"].as_array_mut().unwrap().push(row);
                }
                "foreign_menu" => {
                    o.menu["type"] = json!("fixture:machine");
                }
                _ => {
                    o.menu["state_id"] = Value::Null;
                }
            }
            assert!(meal(&o).is_none(), "{kind}");
        }
    }
    #[test]
    fn menu_revision_churn_does_not_retry_an_unchanged_stored_meal() {
        let mut o = stored_meal();
        let first = meal(&o).unwrap();
        o.menu["state_id"] = json!(8);
        let current = meal(&o).unwrap();
        assert_eq!(
            first, current,
            "native revision churn is not new food/space evidence"
        );
        assert!(matches!(
            current.action(),
            Action::ClickSlot { state_id: 8, .. }
        ));
        o.player["inventory"][2]["item"] = json!("minecraft:rotten_flesh");
        o.menu["slots"][1]["item"] = json!("minecraft:rotten_flesh");
        assert!(
            meal(&o).is_none(),
            "observed nutrition cannot promote risky food into the safe list"
        );
    }
    #[test]
    fn hungry_body_uses_observed_prepared_food_and_respects_hand_and_control_state() {
        let mut o = fixture();
        o.menu = json!({"menu_id":0});
        o.player["food"] = json!(12);
        o.player["health"] = json!(16.);
        o.player["max_health"] = json!(20.);
        o.player["using_item"] = json!(false);
        o.player["on_ground"] = json!(true);
        o.player["selected"] = json!(0);
        o.player["inventory"] = json!([
            {"slot":0,"item":"minecraft:spider_eye","count":3,"use_duration_ticks":32,"use_animation":"EAT","on_cooldown":false,"food":{"can_eat":true,"nutrition":8,"saturation":20.0}},
            {"slot":3,"item":"minecraft:bread","count":2,"use_duration_ticks":32,"use_animation":"EAT","on_cooldown":false,"food":{"can_eat":true,"nutrition":5,"saturation":6.0}}
        ]);
        assert!(meal(&o).is_none(), "do not eat during active movement");
        o.player["control"] = json!(null);
        assert!(matches!(
            meal(&o).unwrap().action(),
            Action::SelectSlot { slot: 3 }
        ));
        o.player["selected"] = json!(3);
        assert!(matches!(
            meal(&o).unwrap().action(),
            Action::StartUseItem {
                hand: Hand::Main,
                max_ticks: 40
            }
        ));
        o.player["inventory"][1]["slot"] = json!(40);
        assert!(matches!(
            meal(&o).unwrap().action(),
            Action::StartUseItem {
                hand: Hand::Off,
                max_ticks: 40
            }
        ));
        o.player["inventory"][1]["food"]["can_eat"] = json!(false);
        assert!(meal(&o).is_none());
    }
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
            super::super::geometry::evade_with_stationary(&o, &[], &stationary, &[]).is_none(),
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
    fn native_eye_and_wide_entity_bounds_allow_reach_without_pursuit_or_visibility_grants() {
        let mut o = fixture();
        // Synthetic wide entity: feet distance3.191, nearest eye/bounds gap2.289.
        // The box/range are supplied facts, never inferred from a species name.
        o.player["position"] = json!([0., 0., 0.]);
        o.player["eye_position"] = json!([0., 1.62, 0.]);
        o.player["entity_interaction_range"] = json!(3.);
        o.world["entities"][0]["position"] = json!([0.496, 1., 2.989]);
        o.world["entities"][0]["bounds"] = json!([-0.204, 1., 2.289, 1.196, 1.9, 3.689]);
        assert_eq!(defensive_target(&o), Some(9));
        let mut legacy = o.clone();
        legacy.player["eye_position"] = Value::Null;
        assert_eq!(defensive_target(&legacy), None);
        for guard in ["hidden", "friendly", "weak", "using", "interaction"] {
            let mut refused = o.clone();
            match guard {
                "hidden" => refused.world["entities"][0]["visible"] = json!(false),
                "friendly" => refused.world["entities"][0]["hostile"] = json!(false),
                "weak" => refused.player["attack_strength"] = json!(0.5),
                "using" => refused.player["using_item"] = json!(true),
                _ => refused.player["interaction"] = json!({"outcome":"running"}),
            }
            assert_eq!(defensive_target(&refused), None, "{guard}");
        }
    }
    #[test]
    fn actual_runtime_range_and_eye_pose_can_deny_a_legacy_reachable_target() {
        let mut o = fixture();
        o.player["eye_position"] = json!([0., 65.62, 0.]);
        o.player["entity_interaction_range"] = json!(1.);
        o.world["entities"][0]["bounds"] = json!([1.5, 64., -0.2, 2.5, 66., 0.2]);
        assert_eq!(defensive_target(&o), None, "valid denial must not fallback");
        o.player["entity_interaction_range"] = json!(2.9);
        o.world["entities"][0]["position"] = json!([3., 66., 0.]);
        o.world["entities"][0]["bounds"] = json!([2.8, 66., -0.2, 3.2, 66.4, 0.2]);
        assert_eq!(defensive_target(&o), Some(9));
        o.player["eye_position"] = json!([0., 65., 0.]);
        assert_eq!(
            defensive_target(&o),
            None,
            "use runtime crouched eye, no fixed offset"
        );
        o.player["entity_interaction_range"] = json!(0.);
        assert_eq!(defensive_target(&o), None, "zero is a valid runtime range");
    }
    #[test]
    fn unknown_or_malformed_runtime_geometry_preserves_exact_legacy_fallback() {
        let mut o = fixture();
        o.player["eye_position"] = json!([0., 65.62, 0.]);
        o.player["entity_interaction_range"] = json!(1.);
        o.world["entities"][0]["bounds"] = json!([1.5, 64., -0.2, 2.5, 66., 0.2]);
        assert_eq!(defensive_target(&o), None);
        for (field, value) in [
            ("eye_position", Value::Null),
            ("eye_position", json!([0., 65.])),
            ("eye_position", json!([0., 65., 0., 1.])),
            ("eye_position", json!([0., "NaN", 0.])),
            ("entity_interaction_range", Value::Null),
            ("entity_interaction_range", json!(-1.)),
            ("entity_interaction_range", json!("Infinity")),
            ("bounds", Value::Null),
            ("bounds", json!([1.5, 64., -0.2, 2.5, 66.])),
            ("bounds", json!([1.5, 64., -0.2, 2.5, 66., 0.2, 3.])),
            ("bounds", json!([2.5, 64., -0.2, 1.5, 66., 0.2])),
            ("bounds", json!([1.5, 64., "NaN", 2.5, 66., 0.2])),
        ] {
            let mut absent = o.clone();
            if field == "bounds" {
                absent.world["entities"][0][field] = value;
            } else {
                absent.player[field] = value;
            }
            assert_eq!(defensive_target(&absent), Some(9), "{field}");
            absent.world["entities"][0]["position"] = json!([4., 64., 0.]);
            assert_eq!(
                defensive_target(&absent),
                None,
                "legacy far target: {field}"
            );
        }
        let mut ranked = fixture();
        ranked.world["entities"].as_array_mut().unwrap().insert(
            0,
            json!({
            "entity_id":10,"hostile":true,"visible":true,"position":[2.5,64.,0.],
            "bounds":[0.5,64.,-0.2,3.,66.,0.2]}),
        );
        assert_eq!(
            defensive_target(&ranked),
            Some(9),
            "legacy feet-distance ordering"
        );
        ranked.player["eye_position"] = json!([0., 65.62, 0.]);
        ranked.player["entity_interaction_range"] = json!(3.);
        ranked.world["entities"][1]["bounds"] = json!([1.5, 64., -0.2, 2.5, 66., 0.2]);
        assert_eq!(
            defensive_target(&ranked),
            Some(9),
            "new facts change eligibility, not target ranking"
        );
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
