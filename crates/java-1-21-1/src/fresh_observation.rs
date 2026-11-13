//! Explicit foreground read. Ordinary Observe remains the cached body path.
use super::*;

pub(super) const CAPABILITY: &str = "minecraft:fresh_observation";
pub(super) const FORMAT: &str = "minecraft-fresh-observation-v1";
const NATIVE_WAIT: Duration = Duration::from_millis(400);

pub(super) fn schema() -> Value {
    json!({
        "input_schema":{"type":"object","properties":{},"additionalProperties":false},
        "description":"One bounded direct native snapshot for foreground revision-sensitive work. Contains full native terrain, entities, inventory and menu; heavier than cached Observe. No polling, retries, mutations or guarantee that state cannot subsequently change.",
        "format":FORMAT,
        "native_timeout_ms":NATIVE_WAIT.as_millis()
    })
}
pub(super) fn read(native: &Native, uuid: &str, input: &Value) -> Result<Observation> {
    if !input.as_object().is_some_and(|o| o.is_empty()) {
        return Err(ApiError::new(
            ErrorCode::InvalidRequest,
            "Fresh observation requires an empty input object",
        ));
    }
    let o = native.observe_bounded(uuid, NATIVE_WAIT).map_err(|_| {
        ApiError::new(
            ErrorCode::NotReady,
            "Bounded fresh native observation unavailable; no cached fallback",
        )
    })?;
    if o.player["uuid"] != uuid
        || !o.player["life_id"].as_str().is_some_and(|s| !s.is_empty())
        || !o.world["sample_clock"]["epoch"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        || !o.world["sample_clock"]["nanos"]
            .as_str()
            .is_some_and(|s| s.parse::<u64>().is_ok())
    {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "Fresh native character, life or sample-clock binding unavailable",
        ));
    }
    Ok(o)
}

#[cfg(test)]
mod tests;
