//! Auditory emission facts, separate from visual entities and inferred threats.
use crate::*;

pub const CAPABILITY: &str = "minecraft:auditory_observation";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sound {
    pub sequence: u64,
    pub sound_id: String,
    pub category: String,
    /// Emission origin supplied by the audible cue, not a current entity pose.
    pub origin: [f64; 3],
    pub game_time: i64,
    pub volume: f64,
    pub pitch: Option<f64>,
    pub delivery: String,
    pub server_range: f64,
    pub distance_at_delivery: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub format: String,
    pub epoch: String,
    pub listener_uuid: String,
    pub dimension: String,
    pub sample_game_time: i64,
    pub latest_sequence: u64,
    pub dropped: u64,
    pub retention_ticks: u16,
    pub events: Vec<Sound>,
    pub scope: String,
}
fn unavailable(message: &str) -> ApiError {
    ApiError::new(ErrorCode::NotReady, message)
}
/// Missing hearing is unavailable; it must never be interpreted as silence.
pub fn frame(observed: &Observation) -> Result<Frame> {
    let Some(value) = observed.world.get("auditory") else {
        return Err(ApiError::new(
            ErrorCode::UnsupportedCapability,
            "Auditory observation unavailable",
        ));
    };
    let frame: Frame = serde_json::from_value(value.clone())
        .map_err(|_| unavailable("Invalid auditory observation schema"))?;
    if frame.format != "minecraft-sdk-auditory-v1" {
        return Err(ApiError::new(
            ErrorCode::UnsupportedCapability,
            "Unsupported auditory observation version",
        ));
    }
    if frame.epoch.is_empty()
        || frame.epoch.len() > 128
        || frame.scope.len() > 1024
        || !registry_id(&frame.dimension)
        || !(1..=1200).contains(&frame.retention_ticks)
        || frame.events.len() > 128
        || observed.player["uuid"] != frame.listener_uuid
        || observed.player["dimension"] != frame.dimension
        || observed.world["game_time"].as_i64() != Some(frame.sample_game_time)
    {
        return Err(unavailable("Auditory scope, clock or bounds mismatch"));
    }
    let mut previous = 0;
    for sound in &frame.events {
        if sound.sequence <= previous
            || sound.sequence > frame.latest_sequence
            || !registry_id(&sound.sound_id)
            || !registry_id(&sound.category)
            || sound.delivery.is_empty()
            || sound.delivery.len() > 64
            || sound.origin.iter().any(|v| !v.is_finite())
            || !sound.volume.is_finite()
            || sound.volume <= 0.
            || sound.pitch.is_some_and(|v| !v.is_finite())
            || !sound.server_range.is_finite()
            || sound.server_range <= 0.
            || !sound.distance_at_delivery.is_finite()
            || sound.distance_at_delivery < 0.
            || sound.distance_at_delivery > sound.server_range
            || frame
                .sample_game_time
                .checked_sub(sound.game_time)
                .is_none_or(|age| age < 0 || age > i64::from(frame.retention_ticks))
        {
            return Err(unavailable("Invalid, stale or unordered auditory event"));
        }
        previous = sound.sequence;
    }
    Ok(frame)
}
/// Persist in the caller's own state. No action, inference or replay is performed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    epoch: String,
    listener_uuid: String,
    dimension: String,
    sequence: u64,
}
impl Cursor {
    pub fn fresh(&mut self, observation: &Observation) -> Result<Vec<Sound>> {
        let frame = frame(observation)?;
        let same = self.epoch == frame.epoch
            && self.listener_uuid == frame.listener_uuid
            && self.dimension == frame.dimension;
        if same && frame.latest_sequence < self.sequence {
            return Err(unavailable(
                "Auditory sequence rolled back without a new epoch",
            ));
        }
        let since = if same { self.sequence } else { 0 };
        let sounds = frame
            .events
            .into_iter()
            .filter(|v| v.sequence > since)
            .collect();
        self.epoch = frame.epoch;
        self.listener_uuid = frame.listener_uuid;
        self.dimension = frame.dimension;
        self.sequence = frame.latest_sequence;
        Ok(sounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn observation() -> Observation {
        Observation {
            revision: "1".into(),
            menu: serde_json::Value::Null,
            player: json!({"uuid":"own","dimension":"minecraft:overworld"}),
            world: json!({"game_time":100,"entities":[],"auditory":{
                "format":"minecraft-sdk-auditory-v1","epoch":"connection-one","listener_uuid":"own","dimension":"minecraft:overworld",
                "sample_game_time":100,"latest_sequence":1,"dropped":0,"retention_ticks":100,"scope":"Emission fact,not a visual entity",
                "events":[{"sequence":1,"sound_id":"minecraft:entity.creeper.primed","category":"minecraft:hostile","origin":[5.,1.,0.],
                    "game_time":99,"volume":1.,"pitch":1.,"delivery":"position_sound","server_range":16.,"distance_at_delivery":5.}]}}),
        }
    }
    #[test]
    fn heard_but_unseen_cues_are_once_per_listener_epoch_and_survive_cursor_restart() {
        let o = observation();
        let mut cursor = Cursor::default();
        assert_eq!(cursor.fresh(&o).unwrap().len(), 1);
        let bytes = serde_json::to_vec(&cursor).unwrap();
        let mut cursor: Cursor = serde_json::from_slice(&bytes).unwrap();
        assert!(cursor.fresh(&o).unwrap().is_empty());
        let mut reconnect = o.clone();
        reconnect.world["auditory"]["epoch"] = json!("connection-two");
        assert_eq!(cursor.fresh(&reconnect).unwrap().len(), 1);
        assert!(o.world["entities"].as_array().unwrap().is_empty());
    }
    #[test]
    fn foreign_scope_future_stale_silent_or_unordered_events_cannot_be_heard() {
        let base = observation();
        for (key, value) in [
            ("listener_uuid", json!("other")),
            ("dimension", json!("minecraft:the_nether")),
            ("sample_game_time", json!(101)),
        ] {
            let mut o = base.clone();
            o.world["auditory"][key] = value;
            assert!(frame(&o).is_err(), "{key}");
        }
        for (key, value) in [
            ("game_time", json!(101)),
            ("game_time", json!(-1)),
            ("volume", json!(0.)),
            ("sequence", json!(0)),
            ("distance_at_delivery", json!(17.)),
        ] {
            let mut o = base.clone();
            o.world["auditory"]["events"][0][key] = value;
            assert!(frame(&o).is_err(), "{key}");
        }
        let mut unknown = base.clone();
        unknown.world["auditory"]["format"] = json!("future-v2");
        assert_eq!(
            frame(&unknown).unwrap_err().code,
            ErrorCode::UnsupportedCapability
        );
        let mut absent = base;
        absent.world.as_object_mut().unwrap().remove("auditory");
        assert_eq!(
            frame(&absent).unwrap_err().code,
            ErrorCode::UnsupportedCapability
        );
    }
    #[test]
    fn packet_loss_is_explicit_and_empty_frames_do_not_replay_expired_events() {
        let mut o = observation();
        let mut cursor = Cursor::default();
        cursor.fresh(&o).unwrap();
        o.world["auditory"]["events"] = json!([]);
        o.world["auditory"]["latest_sequence"] = json!(5);
        o.world["auditory"]["dropped"] = json!(4);
        assert_eq!(frame(&o).unwrap().dropped, 4);
        assert!(cursor.fresh(&o).unwrap().is_empty());
        o.world["auditory"]["latest_sequence"] = json!(4);
        assert!(cursor.fresh(&o).is_err());
    }
}
