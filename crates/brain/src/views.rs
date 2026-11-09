//! Focused, paged views of observations already available to this character.
use crate::{Result, ensure};
use minecraft_sdk::Observation;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    view: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    match_id: Option<String>,
}
fn default_limit() -> usize {
    16
}
fn terrain(observation: &Observation, query: &Query) -> Result<Value> {
    ensure(
        query.match_id.is_none(),
        "Terrain columns do not accept match_id",
    )?;
    let position = &observation.player["position"];
    let mut feet = [0i32; 3];
    for axis in 0..3 {
        let value = position[axis]
            .as_f64()
            .ok_or("Player position unavailable")?;
        ensure(
            value.is_finite()
                && value.floor() >= f64::from(i32::MIN) + 2.
                && value.floor() <= f64::from(i32::MAX) - 2.,
            "Player position outside terrain-view bounds",
        )?;
        feet[axis] = value.floor() as i32;
    }
    let offsets = [
        (0, 0),
        (0, -1),
        (1, 0),
        (0, 1),
        (-1, 0),
        (-1, -1),
        (1, -1),
        (1, 1),
        (-1, 1),
    ];
    ensure(
        query.offset <= offsets.len(),
        "Terrain offset exceeds snapshot; restart at zero",
    )?;
    let blocks: Vec<_> = observation.world["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut entries = Vec::new();
    for (dx, dz) in offsets.iter().skip(query.offset).take(query.limit) {
        let mut cells = serde_json::Map::new();
        for (name, dy) in [("support", -1), ("feet", 0), ("head", 1), ("above_head", 2)] {
            let at = json!([feet[0] + dx, feet[1] + dy, feet[2] + dz]);
            let mut cell = blocks
                .iter()
                .find(|b| b["position"] == at)
                .map(|b| {
                    fields(
                        b,
                        &[
                            "block",
                            "state",
                            "replaceable",
                            "may_interact",
                            "destroy_progress",
                            "collision",
                        ],
                    )
                })
                .unwrap_or_else(|| json!({}));
            cell["known"] = json!(cell["block"].is_string());
            cell["position"] = at;
            cells.insert(name.into(), cell);
        }
        entries.push(json!({"offset_xz":[dx,dz],"cells":cells}));
        if serde_json::to_vec(&entries)?.len() > 5000 {
            entries.pop();
            ensure(
                !entries.is_empty(),
                "Terrain column exceeds focused-view budget; inspect through an SDK program",
            )?;
            break;
        }
    }
    let next = query.offset + entries.len();
    Ok(json!({"view":"terrain","revision":observation.revision,
        "player":{"uuid":observation.player["uuid"],"dimension":observation.player["dimension"],"position":position},
        "world":{"terrain_columns":entries},
        "page":{"offset":query.offset,"returned":next-query.offset,"total_in_snapshot":offsets.len(),
            "next_offset":(next<offsets.len()).then_some(next),"complete":next==offsets.len()},
        "binding":"Read-only local cell facts at this revision. Layers are relative to floor(player feet Y), not predicted landing heights. Missing cells/collision data are unknown, never air or safe. Page completeness describes nine sampled columns, not observation or route completeness. Pages are live; restart after movement. Permissions and collision facts do not prove a digging ray, safe clearance or a route; ordinary native admission remains authoritative."}))
}
fn fields(value: &Value, names: &[&str]) -> Value {
    let mut result = serde_json::Map::new();
    for name in names {
        if let Some(v) = value.get(*name) {
            result.insert((*name).to_owned(), v.clone());
        }
    }
    Value::Object(result)
}
pub fn entity(value: &Value) -> Value {
    let mut result = fields(
        value,
        &[
            "uuid",
            "entity_id",
            "type",
            "position",
            "velocity",
            "visible",
            "hostile",
            "projectile",
            "creeper",
            "health",
            "name",
            "ranged_attack",
            "main_hand",
            "off_hand",
        ],
    );
    if value["item"].is_object() {
        result["item"] = fields(&value["item"], &["item", "count"]);
    }
    result
}
pub fn menu_slot(value: &Value) -> Value {
    fields(
        value,
        &[
            "slot",
            "inventory_slot",
            "item",
            "count",
            "player_inventory",
            "may_pickup",
            "may_place_carried",
        ],
    )
}
pub fn focused(observation: &Observation, input: &str) -> Result<Value> {
    ensure(input.len() <= 1024, "Bounded observation query required")?;
    let query: Query = serde_json::from_str(input)?;
    ensure(
        (1..=32).contains(&query.limit),
        "Observation limit must be 1..32",
    )?;
    ensure(
        query
            .match_id
            .as_ref()
            .is_none_or(|s| minecraft_sdk::registry_id(s)),
        "match_id must be an exact runtime registry ID",
    )?;
    if query.view == "terrain" {
        return terrain(observation, &query);
    }
    let (rows, field, id_field): (&Value, &str, &str) = match query.view.as_str() {
        "menu" => (&observation.menu["slots"], "slots", "item"),
        "inventory" => (&observation.player["inventory"], "inventory", "item"),
        "entities" => (&observation.world["entities"], "entities", "type"),
        "blocks" => (&observation.world["blocks"], "blocks", "block"),
        _ => {
            return Err(
                "Unknown observation view; use menu, inventory, entities, blocks or terrain".into(),
            );
        }
    };
    let rows: Vec<_> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            query
                .match_id
                .as_ref()
                .is_none_or(|id| row[id_field].as_str() == Some(id))
        })
        .collect();
    ensure(
        query.offset <= rows.len(),
        "Observation offset exceeds current snapshot; restart at zero",
    )?;
    let mut entries = Vec::new();
    for row in rows.iter().skip(query.offset).take(query.limit) {
        let row = match query.view.as_str() {
            "entities" => entity(row),
            "menu" => menu_slot(row),
            "inventory" => fields(
                row,
                &[
                    "slot",
                    "item",
                    "count",
                    "food",
                    "on_cooldown",
                    "use_animation",
                    "use_duration_ticks",
                ],
            ),
            _ => fields(
                row,
                &[
                    "position",
                    "block",
                    "state",
                    "replaceable",
                    "may_interact",
                    "destroy_progress",
                    "collision",
                ],
            ),
        };
        entries.push(row);
        if serde_json::to_vec(&entries)?.len() > 5000 {
            entries.pop();
            ensure(
                !entries.is_empty(),
                "Observation record exceeds focused-view budget; inspect through an SDK program",
            )?;
            break;
        }
    }
    let next = query.offset + entries.len();
    let scope = match query.view.as_str() {
        "menu" => "menu",
        "inventory" => "player",
        _ => "world",
    };
    let mut result = json!({"view":query.view,"revision":observation.revision,
        "player":{"uuid":observation.player["uuid"],"dimension":observation.player["dimension"],"position":observation.player["position"]},
        "world":{},
        "menu":{"menu_id":observation.menu["menu_id"],"state_id":observation.menu["state_id"],"type":observation.menu["type"],"carried":fields(&observation.menu["carried"], &["item","count"])},
        "page":{"offset":query.offset,"returned":entries.len(),"total_in_snapshot":rows.len(),"next_offset":(next<rows.len()).then_some(next),"complete":next==rows.len(),"binding":"Each page is live, not an atomic multi-page snapshot. Match stable entity IDs or slot numbers; restart menu paging if menu/state ID changes. Complete describes this snapshot suffix only; omitted records are unknown."}});
    result[scope][field] = json!(entries);
    if query.view == "menu" {
        result["menu"]["summary"] = json!(crate::containers::menu_summary(&observation.menu));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> Observation {
        Observation {
            revision: "42".into(),
            player: json!({"uuid":"one","position":[1,2,3],"inventory":[]}),
            menu: json!({"menu_id":7,"state_id":11,"type":"mod:menu","carried":{"item":"minecraft:air","count":0},"slots":[{"slot":0,"item":"mod:result","count":4,"may_pickup":true,"components":"private giant data"},{"slot":27,"inventory_slot":8,"item":"minecraft:air","count":0,"may_place_carried":true}]}),
            world: json!({"entities":(0..50).map(|i|json!({"type":if i==49 {"minecraft:item"} else {"minecraft:arrow"},"entity_id":i,"position":[i,2,3],"item":{"item":"mod:plank","count":1,"components":"huge"}})).collect::<Vec<_>>()}),
        }
    }
    #[test]
    fn new_loot_after_many_old_arrows_can_be_requested_without_a_compiler_or_host_tool() {
        let result = focused(
            &observation(),
            r#"{"view":"entities","match_id":"minecraft:item"}"#,
        )
        .unwrap();
        assert_eq!(result["world"]["entities"][0]["entity_id"], 49);
        assert_eq!(
            result["world"]["entities"][0]["item"],
            json!({"item":"mod:plank","count":1})
        );
        assert_eq!(result["page"]["complete"], true);
        let overview = crate::projection(&observation());
        assert_eq!(overview["world"]["entities"][0]["entity_id"], 49);
    }
    #[test]
    fn menu_pages_preserve_output_and_empty_slot_bindings_without_reindexing() {
        let first = focused(&observation(), r#"{"view":"menu","limit":1}"#).unwrap();
        assert_eq!(first["menu"]["state_id"], 11);
        assert_eq!(first["menu"]["slots"][0]["slot"], 0);
        assert_eq!(first["menu"]["slots"][0]["count"], 4);
        assert!(first["menu"]["slots"][0].get("components").is_none());
        assert_eq!(first["page"]["next_offset"], 1);
        let next = focused(&observation(), r#"{"view":"menu","offset":1}"#).unwrap();
        assert_eq!(next["menu"]["slots"][0]["slot"], 27);
        assert_eq!(next["menu"]["slots"][0]["may_place_carried"], true);
        assert_eq!(next["page"]["complete"], true);
    }
    #[test]
    fn focused_views_are_closed_read_only_and_bounded() {
        for input in [
            r#"{"view":"console"}"#,
            r#"{"view":"menu","actor":"other"}"#,
            r#"{"view":"menu","limit":0}"#,
            r#"{"view":"menu","limit":33}"#,
            r#"{"view":"menu","offset":99}"#,
            r#"{"view":"entities","match_id":"../../host"}"#,
            r#"{"view":"terrain","match_id":"minecraft:dirt"}"#,
            r#"{"view":"terrain","offset":10}"#,
        ] {
            assert!(focused(&observation(), input).is_err(), "{input}");
        }
        let mut obs = observation();
        obs.world["entities"] = json!(
            (0..32)
                .map(|i| json!({"type":"mod:entity","entity_id":i,"name":"x".repeat(700)}))
                .collect::<Vec<_>>()
        );
        let result = focused(&obs, r#"{"view":"entities","limit":32}"#).unwrap();
        assert!(result.to_string().len() < 6500);
        assert_eq!(result["page"]["complete"], false);
        assert!(result["page"]["returned"].as_u64().unwrap() < 32);
        let mut context = json!({"last_tool_result":{"tool":"observe","result":result},"sdk_reference":"API docs".repeat(500),"server":{"description":"x".repeat(30000)},"recent":vec![json!({"text":"x".repeat(10000)});12],"observation":{"world":{"blocks":vec![json!({"block":"mod:block","position":[1,2,3]});80]}},"last_experiment":{"result":{"stderr":"previous failure"}}});
        let before = context["last_tool_result"].clone();
        crate::fit_context(&mut context).unwrap();
        assert_eq!(
            context["last_tool_result"], before,
            "Context fitting must not silently change page counts or omit returned records"
        );
    }
    #[test]
    fn terrain_distinguishes_missing_cells_from_air_and_keeps_native_shapes() {
        let mut obs = observation();
        obs.player["position"] = json!([-0.2, 73.0, -1.1]);
        obs.world["blocks"] = json!([
            {"position":[-1,72,-2],"block":"minecraft:stone","collision":[[0,0,0,1,1,1]]},
            {"position":[-1,73,-2],"block":"minecraft:air","collision":[]},
            {"position":[-1,74,-2],"block":"custom:slab","collision":[[0,0,0,1,0.5,1]],"may_interact":false,"destroy_progress":0.0}
        ]);
        let page = focused(&obs, r#"{"view":"terrain","limit":1}"#).unwrap();
        let cells = &page["world"]["terrain_columns"][0]["cells"];
        assert_eq!(cells["support"]["position"], json!([-1, 72, -2]));
        assert_eq!(cells["feet"]["known"], true);
        assert_eq!(cells["feet"]["block"], "minecraft:air");
        assert_eq!(cells["head"]["collision"], json!([[0, 0, 0, 1, 0.5, 1]]));
        assert_eq!(cells["head"]["may_interact"], false);
        assert_eq!(cells["above_head"]["known"], false);
        assert!(cells["above_head"].get("block").is_none());
        assert_eq!(page["page"]["next_offset"], 1);
        let next = focused(&obs, r#"{"view":"terrain","offset":1,"limit":1}"#).unwrap();
        assert_eq!(
            next["world"]["terrain_columns"][0]["cells"]["head"]["position"],
            json!([-1, 74, -3])
        );
        assert_eq!(
            next["world"]["terrain_columns"][0]["cells"]["head"]["known"],
            false
        );
        assert!(page.to_string().len() < 7000);
        for position in [json!(null), json!([1e30, 73, 0]), json!([0, "bad", 0])] {
            obs.player["position"] = position;
            assert!(focused(&obs, r#"{"view":"terrain"}"#).is_err());
        }
    }
    #[test]
    fn runtime_material_feedback_survives_history_compaction_without_becoming_a_craftability_claim()
    {
        let recipe = json!({"id":"fixture:recipe","ingredients":[
            {"matching_stacks":[{"item":"fixture:input","count":1}],"omitted_alternatives":0}],
            "omitted_ingredient_slots":0});
        let mut obs = observation();
        obs.player["inventory"] = json!([]);
        let summary = minecraft_sdk::recipes::material_summary(&obs, &recipe).unwrap();
        let answer = json!({"tool":"query","result":{"view":"capability_query",
            "capability":"minecraft:recipes","result":{"matches":[recipe]},"client_materials":summary}});
        let mut context = json!({"last_tool_result":answer,"sdk_reference":include_str!("sdk.md"),
            "server":{"description":"x".repeat(30000)},"recent":vec![json!({"text":"x".repeat(10000)});12],
            "observation":crate::projection(&obs)});
        crate::fit_context(&mut context).unwrap();
        assert_eq!(context["last_tool_result"], answer);
        assert_eq!(
            context["last_tool_result"]["result"]["client_materials"]["groups"][0]["alternatives"]
                [0]["inventory_count"],
            0
        );
        assert!(
            context["last_tool_result"]["result"]["client_materials"]
                .get("craftable")
                .is_none()
        );
        assert!(context.to_string().len() <= 32768);
    }
}
