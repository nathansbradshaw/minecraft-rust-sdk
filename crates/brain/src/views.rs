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
    let (rows, field, id_field): (&Value, &str, &str) = match query.view.as_str() {
        "menu" => (&observation.menu["slots"], "slots", "item"),
        "inventory" => (&observation.player["inventory"], "inventory", "item"),
        "entities" => (&observation.world["entities"], "entities", "type"),
        "blocks" => (&observation.world["blocks"], "blocks", "block"),
        _ => {
            return Err("Unknown observation view; use menu, inventory, entities or blocks".into());
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
}
