//! Optional bounded diagnostic inputs; never authoritative gameplay evidence.
use super::geometry::{self, Admitted, Escape};
use minecraft_sdk::{Observation, Position};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_BYTES: usize = 128 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    target: Position,
    jump: bool,
    reason: String,
    #[serde(default)]
    cover_path: Vec<Position>,
}
impl From<&Escape> for Choice {
    fn from(route: &Escape) -> Self {
        Self {
            target: route.target.clone(),
            jump: route.jump,
            reason: route.reason.clone(),
            cover_path: route.cover_path.clone(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    observation: Observation,
    blocked: Vec<[i32; 3]>,
    stationary: Vec<String>,
    recent_retreat: Vec<[f64; 3]>,
    waypoint_axis: f64,
    admitted: Vec<Admitted>,
    selected: Option<Choice>,
}
impl Snapshot {
    pub(super) fn capture(
        o: &Observation,
        blocked: &[[i32; 3]],
        stationary: &[String],
        recent_retreat: &[[f64; 3]],
        waypoint_axis: f64,
        admitted: &[Admitted],
        route: &Escape,
    ) -> Self {
        // Preserve every field read by the geometric policy. Do not infer air
        // for missing cells, retain only solids, or omit current threat facts.
        let blocks: Vec<_> = o.world["blocks"].as_array().into_iter().flatten().map(|b|
            json!({"position":b["position"],"block":b["block"],"collision":b["collision"],"dangerous":b["dangerous"]})).collect();
        Self {
            observation: Observation {
                revision: o.revision.clone(),
                player: o.player.clone(),
                menu: Value::Null,
                world: json!({"blocks":blocks,"entities":o.world["entities"],"game_time":o.world["game_time"],"sample_clock":o.world["sample_clock"]}),
            },
            blocked: blocked.to_vec(),
            stationary: stationary.to_vec(),
            recent_retreat: recent_retreat.to_vec(),
            waypoint_axis,
            admitted: admitted.to_vec(),
            selected: Some(route.into()),
        }
    }
    pub fn replay(&self) -> Value {
        let actual = geometry::evade_with_native(
            &self.observation,
            &self.blocked,
            &self.stationary,
            &self.recent_retreat,
            self.waypoint_axis,
            &self.admitted,
        );
        let actual = actual.as_ref().map(Choice::from);
        let expected = serde_json::to_value(&self.selected).expect("finite captured policy choice");
        let actual = serde_json::to_value(actual).expect("finite replayed policy choice");
        json!({"matches_recorded_choice":actual==expected,"recorded":expected,"replayed":actual,
            "revision":self.observation.revision,"position":self.observation.player["position"],
            "scope":"Container-local policy replay, not native movement or Survival evidence"})
    }
}
