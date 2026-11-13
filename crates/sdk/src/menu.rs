//! Typed menu builders and optional journaled recipe-output composition.
use crate::*;
mod crafting;
pub use crafting::{CraftPhase, CraftReport, CraftStatus, craft_once};

pub const FRESH_OBSERVATION_CAPABILITY: &str = "minecraft:fresh_observation";

/// Explicit foreground snapshot, independent of the adapter's cached body read.
/// Makes one query with no polling, retry or cached fallback. The advertised
/// adapter bounds its single native read; full terrain/entity work is included.
/// A fresh revision can still become stale before a subsequent mutation.
pub async fn observe_fresh(client: &Client) -> Result<Observation> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Snapshot {
        format: String,
        observation: Observation,
    }
    let value = client
        .read_observation(&ObservationQuery {
            capability: FRESH_OBSERVATION_CAPABILITY.into(),
            input: serde_json::json!({}),
        })
        .await?;
    let snapshot: Snapshot = serde_json::from_value(value).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "Fresh observation response does not match its versioned contract",
        )
    })?;
    if snapshot.format != "minecraft-fresh-observation-v1" {
        return Err(ApiError::new(
            ErrorCode::UnsupportedCapability,
            "Unsupported fresh observation format",
        ));
    }
    Ok(snapshot.observation)
}

fn unavailable(message: &str) -> ApiError {
    ApiError::new(ErrorCode::NotReady, message)
}
fn identifier(observation: &Observation, key: &str) -> Result<i32> {
    observation.menu[key]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .filter(|n| *n >= 0)
        .ok_or_else(|| unavailable("Observed menu identifier/revision missing or outside contract"))
}
/// The caller selects a runtime slot and mode and retains its own action ID.
/// Pass a fresh observation. Stale state remains subject to server rejection.
pub fn click_slot(observation: &Observation, slot: i16, button: u8, mode: u8) -> Result<Action> {
    if !observation.menu["slots"].as_array().is_some_and(|slots| {
        slots
            .iter()
            .any(|row| row["slot"].as_i64() == Some(i64::from(slot)))
    }) {
        return Err(unavailable(
            "Requested slot is absent from the observed menu",
        ));
    }
    let action = Action::ClickSlot {
        menu_id: identifier(observation, "menu_id")?,
        state_id: identifier(observation, "state_id")?,
        slot,
        button,
        mode,
    };
    action.validate()?;
    Ok(action)
}
/// Request recipe placement in the current menu. This does not take its output
/// or establish inventory gain. Recipe identity and batch policy are caller data.
pub fn prepare_recipe(
    observation: &Observation,
    recipe: impl Into<String>,
    make_all: bool,
) -> Result<Action> {
    let action = Action::Craft {
        menu_id: identifier(observation, "menu_id")?,
        recipe: recipe.into(),
        make_all,
    };
    action.validate()?;
    Ok(action)
}
/// Count an exact runtime item ID in the observed player inventory. Menu outputs
/// and carried stacks are deliberately not counted as retained inventory.
pub fn item_count(observation: &Observation, item: &str) -> Result<u64> {
    if !registry_id(item) {
        return Err(ApiError::new(
            ErrorCode::InvalidRequest,
            "Runtime item registry ID required",
        ));
    }
    let rows = observation.player["inventory"]
        .as_array()
        .ok_or_else(|| unavailable("Observed player inventory unavailable"))?;
    rows.iter()
        .filter(|row| row["item"] == item)
        .try_fold(0u64, |sum, row| {
            sum.checked_add(
                row["count"]
                    .as_u64()
                    .ok_or_else(|| unavailable("Observed stack count invalid"))?,
            )
            .ok_or_else(|| unavailable("Observed inventory count exceeds contract"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn menu_builders_bind_observed_revisions_and_do_not_count_output_as_inventory() {
        let mut observation = Observation {
            revision: "sample".into(),
            player: json!({"inventory":[{"slot":1,"item":"fixture:material","count":3}]}),
            world: json!({}),
            menu: json!({"menu_id":17,"state_id":42,"slots":[{"slot":9,"item":"fixture:result","count":4}],"carried":{"item":"fixture:result","count":2}}),
        };
        let action = click_slot(&observation, 9, 0, click_mode::QUICK_MOVE).unwrap();
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            json!({"kind":"click_slot","menu_id":17,"state_id":42,"slot":9,"button":0,"mode":1})
        );
        assert!(click_slot(&observation, 0, 0, 1).is_err());
        assert_eq!(item_count(&observation, "fixture:result").unwrap(), 0);
        assert_eq!(item_count(&observation, "fixture:material").unwrap(), 3);
        assert!(prepare_recipe(&observation, "../../host", false).is_err());
        observation.menu["state_id"] = json!(-1);
        assert!(click_slot(&observation, 9, 0, 1).is_err());
    }
}
