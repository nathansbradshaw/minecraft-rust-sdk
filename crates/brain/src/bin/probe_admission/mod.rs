//! Optional operator diagnostic admission, never a gameplay or safety policy.
use minecraft_sdk_brain::{Brain, Result, ensure, private_read};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub entity_uuid: String,
    entity_type: String,
    entity_position: [f64; 3],
    entity_health: f64,
    player_position: [f64; 3],
    position_tolerance: f64,
    player_health: f64,
    player_food: u64,
    main_hand: String,
    #[serde(default = "ranged_by_default")]
    ranged_attack: bool,
    day_time_range: Option<[u64; 2]>,
    timeout_secs: u64,
}
fn ranged_by_default() -> bool {
    true
}
fn point(value: &Value) -> Option<[f64; 3]> {
    let p = value.as_array()?;
    if p.len() != 3 {
        return None;
    }
    let p = [p[0].as_f64()?, p[1].as_f64()?, p[2].as_f64()?];
    p.iter().all(|v| v.is_finite()).then_some(p)
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}
impl Spec {
    pub fn read(path: &Path) -> Result<Self> {
        let spec: Self = serde_json::from_slice(&private_read(path, 16384)?)?;
        spec.validate()?;
        Ok(spec)
    }
    fn validate(&self) -> Result<()> {
        ensure(
            self.entity_uuid.len() == 36
                && self.entity_uuid.bytes().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == b'-'
                    } else {
                        c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
                    }
                }),
            "Canonical stimulus UUID required",
        )?;
        ensure(
            minecraft_sdk::registry_id(&self.entity_type)
                && minecraft_sdk::registry_id(&self.main_hand),
            "Runtime entity/item IDs required",
        )?;
        ensure(
            self.entity_position
                .iter()
                .chain(&self.player_position)
                .all(|v| v.is_finite())
                && self.position_tolerance.is_finite()
                && (0.001..=0.5).contains(&self.position_tolerance)
                && self.player_health.is_finite()
                && self.player_health > 0.
                && self.entity_health.is_finite()
                && self.entity_health > 0.
                && self.player_food <= 20
                && (1..=600).contains(&self.timeout_secs),
            "Invalid stimulus admission bounds",
        )?;
        if let Some([start, end]) = self.day_time_range {
            ensure(
                start <= end && end < 24000,
                "Admission day range must fit one Java day",
            )?;
        }
        Ok(())
    }
    /// Absence is a bounded wait. A present but displaced/dead/wrong stimulus
    /// is an invalid attempt, not an invitation to repair it during evaluation.
    fn check(&self, o: &minecraft_sdk::Observation) -> Result<bool> {
        let p = point(&o.player["position"]).ok_or("Player pose missing")?;
        ensure(
            distance(p, self.player_position) <= self.position_tolerance,
            "Player moved before stimulus admission",
        )?;
        ensure(
            o.player["health"]
                .as_f64()
                .is_some_and(|h| (h - self.player_health).abs() < 0.001)
                && o.player["food"].as_u64() == Some(self.player_food),
            "Player health/food changed before stimulus admission",
        )?;
        let inventory = o.player["inventory"]
            .as_array()
            .ok_or("Player inventory missing")?;
        ensure(
            inventory.iter().all(|row| row["count"].as_u64() == Some(0)),
            "Admission requires empty player inventory",
        )?;
        let entities = o.world["entities"]
            .as_array()
            .ok_or("Entity observations missing")?;
        ensure(
            !entities.iter().any(|e| {
                e["uuid"] != self.entity_uuid && e["hostile"] == true && e["visible"] == true
            }),
            "Unexpected hostile before stimulus admission",
        )?;
        let Some(enemy) = entities.iter().find(|e| e["uuid"] == self.entity_uuid) else {
            return Ok(false);
        };
        if let Some([start, end]) = self.day_time_range {
            let day = o.world["day_time"]
                .as_u64()
                .ok_or("Native day time missing")?
                % 24000;
            ensure(
                (start..=end).contains(&day),
                "Stimulus outside declared natural day window",
            )?;
        }
        let p = point(&enemy["position"]).ok_or("Stimulus pose missing")?;
        ensure(
            distance(p, self.entity_position) <= self.position_tolerance,
            "Stimulus moved outside declared starting pose",
        )?;
        ensure(
            enemy["type"] == self.entity_type
                && enemy["visible"] == true
                && enemy["hostile"] == true
                && enemy["health"]
                    .as_f64()
                    .is_some_and(|h| (h - self.entity_health).abs() < 0.001)
                && enemy["main_hand"]["item"] == self.main_hand
                && enemy["ranged_attack"] == self.ranged_attack,
            "Stimulus type,visibility,hostility,health,weapon or attack kind does not match",
        )?;
        Ok(true)
    }
}

pub async fn wait(brain: &mut Brain, spec: &Spec) -> Result<minecraft_sdk::Observation> {
    brain.journal.event("body_probe_ready",json!({"entity_uuid":spec.entity_uuid,
        "body_enabled":false,"models_called":0,"observation":brain.observed,
        "scope":"Same character/session connected; no timed exposure or protective body started. Spawn the declared ordinary stimulus now."}))?;
    let timeout = Duration::from_secs(spec.timeout_secs);
    tokio::time::timeout(timeout, async {
        loop {
            let observed=brain.client.observe().await?;
            match spec.check(&observed) {
                Ok(true)=>{
                    brain.journal.event("body_probe_stimulus_admitted",json!({"entity_uuid":spec.entity_uuid,
                        "observation":observed,"models_called":0,
                        "scope":"Actual SDK actor/stimulus poses and native clock before enabling body/timing. Normal enemy AI is not frozen or targeted by this harness."}))?;
                    return Ok(observed);
                }
                Err(error)=>{
                    brain.journal.event("body_probe_stimulus_rejected",json!({"error":error.to_string(),
                        "observation":observed,"timed_trial_started":false,"models_called":0}))?;
                    return Err(error);
                }
                Ok(false)=>tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }).await.map_err(|_|"Stimulus admission timed out; no timed trial started")?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> Spec {
        serde_json::from_value(json!({"entity_uuid":"00000000-0000-4000-8000-000000000001",
            "entity_type":"fixture:archer","entity_position":[10.,1.,0.],"entity_health":20.,"player_position":[0.,1.,0.],
            "position_tolerance":0.25,"player_health":20.,"player_food":20,"main_hand":"fixture:bow",
            "day_time_range":[13000,20500],"timeout_secs":10})).unwrap()
    }
    fn observation() -> minecraft_sdk::Observation {
        minecraft_sdk::Observation {
            revision: "1".into(),
            player: json!({"position":[0.,1.,0.],"health":20.,"food":20,"inventory":[]}),
            menu: json!({}),
            world: json!({"day_time":14000,"entities":[{"uuid":"00000000-0000-4000-8000-000000000001","type":"fixture:archer","position":[10.,1.,0.],"health":20.,"visible":true,"hostile":true,"ranged_attack":true,"main_hand":{"item":"fixture:bow"}}]}),
        }
    }
    #[test]
    fn absent_stimulus_waits_but_actual_fourteen_metre_drift_is_rejected() {
        let s = spec();
        s.validate().unwrap();
        let mut o = observation();
        assert!(s.check(&o).unwrap());
        o.world["entities"][0]["position"] = json!([14.7, 1., 0.]);
        assert!(s.check(&o).is_err());
        o.world["entities"] = json!([]);
        assert!(!s.check(&o).unwrap());
    }
    #[test]
    fn unhealthy_or_moved_players_and_wrong_stimuli_cannot_start_a_trial() {
        for (path, value) in [
            ("/player/health", json!(19.)),
            ("/player/position", json!([1., 1., 0.])),
            ("/world/day_time", json!(21000)),
            ("/world/entities/0/health", json!(0.)),
            ("/world/entities/0/health", json!(10.)),
            ("/world/entities/0/main_hand/item", json!("fixture:wrong")),
            ("/world/entities/0/visible", json!(false)),
        ] {
            let o = observation();
            let mut value_o = serde_json::to_value(o).unwrap();
            *value_o.pointer_mut(path).unwrap() = value;
            let o = serde_json::from_value(value_o).unwrap();
            assert!(spec().check(&o).is_err(), "accepted {path}");
        }
    }
    #[test]
    fn tolerance_and_wait_limits_are_bounded_and_unknown_fields_refused() {
        let mut s = spec();
        s.position_tolerance = 5.;
        assert!(s.validate().is_err());
        s = spec();
        s.timeout_secs = 0;
        assert!(s.validate().is_err());
        let mut extra = serde_json::to_value(spec()).unwrap();
        extra["command"] = json!("operator console");
        assert!(serde_json::from_value::<Spec>(extra).is_err());
    }
    #[test]
    fn legacy_specs_require_ranged_but_explicit_melee_preserves_all_other_bindings() {
        let legacy = spec();
        assert!(legacy.ranged_attack);
        let mut s = spec();
        s.ranged_attack = false;
        s.entity_type = "minecraft:zombie".into();
        s.main_hand = "minecraft:air".into();
        let mut o = observation();
        o.world["entities"][0]["type"] = json!("minecraft:zombie");
        o.world["entities"][0]["ranged_attack"] = json!(false);
        o.world["entities"][0]["main_hand"]["item"] = json!("minecraft:air");
        assert!(s.check(&o).unwrap());
        s.ranged_attack = true;
        assert!(
            s.check(&o).is_err(),
            "attack kind alone must reject a mismatch"
        );
        s.ranged_attack = false;
        assert!(legacy.check(&o).is_err());
        o.world["entities"][0]["hostile"] = json!(false);
        assert!(s.check(&o).is_err());
        o.world["entities"][0]["hostile"] = json!(true);
        o.world["entities"]
            .as_array_mut()
            .unwrap()
            .push(json!({"uuid":"other","hostile":true,"visible":true}));
        assert!(s.check(&o).is_err());
    }
}
