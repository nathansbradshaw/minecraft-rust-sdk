//! Java air cells cannot establish that a block-mining operation began.
use super::*;

fn target<'a>(observation: &'a Observation, position: &BlockPos) -> Option<&'a Value> {
    observation.world["blocks"]
        .as_array()?
        .iter()
        .find(|b| b["position"] == json!([position.x, position.y, position.z]))
}
pub(super) fn has_target(observation: &Observation, position: &BlockPos) -> bool {
    target(observation, position).is_some_and(|b| {
        b["block"].as_str().is_some_and(|id| {
            registry_id(id)
                && !matches!(
                    id,
                    "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
                )
        })
    })
}
pub(super) fn validate(action: &Action, before: &Observation) -> Result<()> {
    if let Action::Dig {
        block,
        stage: DigStage::Start,
        ..
    } = action
    {
        if target(before, block)
            .and_then(|b| b["block"].as_str())
            .is_none_or(|id| !registry_id(id))
        {
            return Err(ApiError::new(
                ErrorCode::NotReady,
                "Mining target is absent from the current native observation",
            ));
        }
        if !has_target(before, block) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Mining target is currently air; no mining input submitted. Observe the target and retained inventory before choosing another intention",
            ));
        }
    }
    // Finish/cancel can refer to a previously broken target; retain their normal
    // no-replay status and input-release paths.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scene(id: &str, active: bool) -> Observation {
        Observation {
            revision: "native".into(),
            player: json!({"mining":{"active":active,"block":[1,64,2]}}),
            world: json!({"blocks":[{"position":[1,64,2],"block":id,"state":id}]}),
            menu: Value::Null,
        }
    }
    fn dig(stage: DigStage) -> Action {
        Action::Dig {
            block: BlockPos { x: 1, y: 64, z: 2 },
            face: 1,
            stage,
        }
    }
    #[test]
    fn a_native_active_flag_does_not_make_air_into_a_mining_target() {
        for id in ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"] {
            let before = scene(id, false);
            let after = scene(id, true);
            assert_eq!(
                validate(&dig(DigStage::Start), &before).unwrap_err().code,
                ErrorCode::InvalidRequest
            );
            assert!(!verified_effect(&dig(DigStage::Start), &before, &after));
            assert!(validate(&dig(DigStage::Cancel), &after).is_ok());
            assert!(validate(&dig(DigStage::Finish), &after).is_ok());
        }
        let mut unknown = scene("minecraft:stone", false);
        unknown.world["blocks"] = json!([]);
        assert_eq!(
            validate(&dig(DigStage::Start), &unknown).unwrap_err().code,
            ErrorCode::NotReady
        );
    }
    #[test]
    fn staged_and_instant_breaks_keep_their_distinct_native_effects() {
        let before = scene("minecraft:oak_log", false);
        assert!(validate(&dig(DigStage::Start), &before).is_ok());
        assert!(verified_effect(
            &dig(DigStage::Start),
            &before,
            &scene("minecraft:oak_log", true)
        ));
        assert!(!verified_effect(
            &dig(DigStage::Start),
            &scene("minecraft:oak_log", true),
            &scene("minecraft:oak_log", true)
        ));
        assert!(!verified_effect(
            &dig(DigStage::Finish),
            &before,
            &scene("minecraft:oak_log", true)
        ));
        let broken = scene("minecraft:air", false);
        assert!(verified_effect(&dig(DigStage::Finish), &before, &broken));
        assert!(verified_effect(
            &dig(DigStage::Start),
            &scene("minecraft:short_grass", false),
            &broken
        ));
    }
}
