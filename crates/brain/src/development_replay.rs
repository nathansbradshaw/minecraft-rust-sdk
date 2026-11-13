//! Recorded client evidence exercised through production SDK observation views.
use crate::{progress, views};
use minecraft_sdk::Observation;
use minecraft_sdk_xtask::workflow::{analyze, fixture};
use serde_json::{Value, json};

#[test]
fn captured_menu_replays_through_real_projection_and_preserves_slot_bindings() {
    let events = fixture::events().unwrap();
    let fixture: Value = serde_json::from_str(fixture::CAPTURE).unwrap();
    let mut slots = Vec::new();
    for page in fixture["pages"].as_array().unwrap() {
        slots.extend(page["menu"]["slots"].as_array().unwrap().clone());
    }
    slots.sort_by_key(|slot| slot["slot"].as_u64().unwrap());
    assert_eq!(slots.len(), 46);
    let mut observation = Observation {
        revision: String::new(),
        player: json!({"uuid":"offline-fixture","dimension":"minecraft:overworld",
            "position":[0,64,0],"inventory":[
                {"slot":0,"item":"minecraft:birch_planks","count":2},
                {"slot":2,"item":"minecraft:stick","count":4}]}),
        world: json!({}),
        menu: fixture["pages"][0]["menu"].clone(),
    };
    observation.menu["slots"] = json!(slots);
    let mut projected = Vec::new();
    let mut inspections = 0;
    for pair in events.chunks_exact(2) {
        let choice = &pair[0];
        let mut result = pair[1].clone();
        if choice["data"]["choice"]["tool"] == "observe" {
            observation.revision = result["data"]["result"]["revision"]
                .as_str()
                .unwrap()
                .into();
            let input = choice["data"]["choice"]["input"].as_str().unwrap();
            let actual = views::focused(&observation, input).unwrap();
            assert_eq!(actual["menu"], result["data"]["result"]["menu"]);
            assert_eq!(actual["page"], result["data"]["result"]["page"]);
            if let Some(directory) = std::env::var_os("BOT_DEV_CAPTURE_DIR") {
                use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
                let file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(
                        std::path::Path::new(&directory)
                            .join(format!("menu-{}.json", choice["sequence"])),
                    )
                    .unwrap();
                serde_json::to_writer_pretty(file, &actual).unwrap();
            }
            result["data"]["result"] = actual;
        }
        // The existing production counter must retain inspections across fresh
        // revisions. This test does not claim it stops the complete brain loop.
        inspections = progress::next_inspection_count(inspections, true, false);
        projected.push(choice.clone());
        projected.push(result);
    }
    assert_eq!(inspections, 20);
    let report = analyze(&projected).unwrap();
    assert_eq!(report["repeated_menu_results"], 17);
    assert_eq!(report["first_repeat"]["sequence"], 367);
    assert_eq!(report["summed_model_ms"], 285186);
    assert_eq!(observation.menu["carried"]["count"], 3);
    assert_eq!(
        observation.menu["slots"][0]["item"],
        "minecraft:birch_button"
    );
    assert_eq!(observation.player["inventory"][0]["count"], 2);
}

#[test]
fn a_real_menu_change_is_visible_and_does_not_trigger_duplicate_diagnosis() {
    let mut events = fixture::events().unwrap();
    // A changed native state ID, cursor or slot can make another read useful.
    for (index, pair) in events.chunks_exact_mut(2).enumerate() {
        if pair[1]["data"]["result"]["view"] == "menu" {
            pair[1]["data"]["result"]["menu"]["state_id"] = json!(100 + index);
        }
    }
    assert_eq!(analyze(&events).unwrap()["repeated_menu_results"], 0);
}
