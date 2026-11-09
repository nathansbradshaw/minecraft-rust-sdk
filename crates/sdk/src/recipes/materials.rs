//! Client planning facts from runtime representatives, never a craftability test.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::NotReady, message)
}

/// Group identical runtime ingredient descriptions and pair each representative
/// with retained player inventory. Counts are not allocated between groups;
/// overlapping alternatives share supplies. Components, grid/cursor contents,
/// menu compatibility and custom matching semantics are deliberately unproved.
/// Missing inventory is unknown, not zero. No recipe or material IDs are built in.
pub fn material_summary(
    observation: &Observation,
    recipe: &serde_json::Value,
) -> Result<serde_json::Value> {
    use serde_json::json;
    let id = recipe["id"]
        .as_str()
        .filter(|id| registry_id(id))
        .ok_or_else(|| invalid("Runtime recipe identity missing"))?;
    let ingredients = recipe["ingredients"]
        .as_array()
        .filter(|rows| rows.len() <= 64)
        .ok_or_else(|| invalid("Runtime ingredient descriptions unavailable or oversized"))?;
    let mut inventory: Option<BTreeMap<String, u64>> = None;
    if let Some(rows) = observation.player["inventory"].as_array() {
        if rows.len() > 256 {
            return Err(invalid("Inventory summary exceeds bounds"));
        }
        let mut totals = BTreeMap::<String, u64>::new();
        let mut slots = BTreeSet::new();
        for row in rows {
            let slot = row["slot"]
                .as_u64()
                .ok_or_else(|| invalid("Inventory slot unavailable"))?;
            if !slots.insert(slot) {
                return Err(invalid("Duplicate inventory slot"));
            }
            let item = row["item"]
                .as_str()
                .filter(|id| registry_id(id))
                .ok_or_else(|| invalid("Inventory item unavailable"))?;
            let count = row["count"]
                .as_u64()
                .ok_or_else(|| invalid("Inventory count unavailable"))?;
            let total = totals.entry(item.into()).or_default();
            *total = total
                .checked_add(count)
                .ok_or_else(|| invalid("Inventory count overflow"))?;
        }
        inventory = Some(totals);
    }
    // The omitted count is part of the grouping key: a partial description must
    // never merge with an apparently complete one that has the same samples.
    type GroupKey = (Vec<(String, u64)>, Option<u64>);
    let mut groups = BTreeMap::<GroupKey, Vec<usize>>::new();
    for (index, ingredient) in ingredients.iter().enumerate() {
        let rows = ingredient["matching_stacks"]
            .as_array()
            .filter(|rows| rows.len() <= 16)
            .ok_or_else(|| invalid("Ingredient representatives unavailable or oversized"))?;
        let mut options = BTreeSet::new();
        for row in rows {
            let item = row["item"]
                .as_str()
                .filter(|id| registry_id(id))
                .ok_or_else(|| invalid("Ingredient representative item unavailable"))?;
            let count = row["count"]
                .as_u64()
                .filter(|count| *count > 0)
                .ok_or_else(|| invalid("Ingredient representative count unavailable"))?;
            options.insert((item.to_owned(), count));
        }
        groups
            .entry((
                options.into_iter().collect(),
                ingredient["omitted_alternatives"].as_u64(),
            ))
            .or_default()
            .push(index);
    }
    let groups: Vec<_> = groups.into_iter().map(|((options, omitted), indices)| {
        json!({"ingredient_indices":indices,"occurrences":indices.len(),
            "omitted_alternatives":omitted,"empty_representatives":options.is_empty(),
            "alternatives":options.into_iter().map(|(item,count)| {
                let available = inventory.as_ref().map(|rows| rows.get(&item).copied().unwrap_or(0));
                json!({"item":item,"representative_count":count,"inventory_count":available})
            }).collect::<Vec<_>>()})
    }).collect();
    let result = json!({"format":"recipe-materials-v1","recipe":id,
        "observation_revision":observation.revision,"player_uuid":observation.player["uuid"],
        "inventory_known":inventory.is_some(),"groups":groups,
        "omitted_ingredient_slots":recipe["omitted_ingredient_slots"],
        "scope":"Client planning only. Counts cover retained player inventory, excluding grid, output and cursor. Groups share supplies; alternatives are not allocated. Representatives do not prove component/custom matching, quantities, menu compatibility or craftability. Empty or omitted alternatives are unknown. Recipe lookup and inventory observation are separate samples; reobserve before acting."});
    if result.to_string().len() > 16384 {
        return Err(invalid(
            "Material summary exceeds 16 KiB; use a smaller recipe description",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn observation() -> Observation {
        Observation {
            revision: "sample".into(),
            player: json!({"uuid":"actor","inventory":[
            {"slot":0,"item":"fixture:raw","count":1},{"slot":8,"item":"fixture:handle","count":8}]}),
            world: json!({}),
            menu: json!({"carried":{"item":"fixture:board","count":99},"slots":[{"item":"fixture:board","count":99}]}),
        }
    }
    fn ingredient(item: &str) -> serde_json::Value {
        json!({"matching_stacks":[{"item":item,"count":1}],"omitted_alternatives":0})
    }
    fn recipe() -> serde_json::Value {
        json!({"id":"fixture:tool","ingredients":[ingredient("fixture:board"),ingredient("fixture:board"),ingredient("fixture:board"),ingredient("fixture:handle"),ingredient("fixture:handle")],"omitted_ingredient_slots":0})
    }
    #[test]
    fn repeated_ingredients_explain_missing_material_without_counting_output_or_cursor() {
        let summary = material_summary(&observation(), &recipe()).unwrap();
        let groups = summary["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["occurrences"], 3);
        assert_eq!(groups[0]["alternatives"][0]["inventory_count"], 0);
        assert_eq!(groups[1]["occurrences"], 2);
        assert_eq!(groups[1]["alternatives"][0]["inventory_count"], 8);
        assert!(summary.get("craftable").is_none());
    }
    #[test]
    fn incomplete_descriptions_and_shared_alternatives_do_not_claim_sufficient_supplies() {
        let mut recipe = recipe();
        recipe["ingredients"][0]["omitted_alternatives"] = json!(2);
        recipe["ingredients"][1]["matching_stacks"] = json!([]);
        recipe["ingredients"][2]["matching_stacks"] =
            json!([{"item":"fixture:board","count":1},{"item":"fixture:handle","count":1}]);
        let summary = material_summary(&observation(), &recipe).unwrap();
        assert_eq!(summary["groups"].as_array().unwrap().len(), 4);
        assert!(
            summary["groups"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["empty_representatives"] == true)
        );
        let mut obs = observation();
        obs.player.as_object_mut().unwrap().remove("inventory");
        let unknown = material_summary(&obs, &recipe).unwrap();
        assert_eq!(unknown["inventory_known"], false);
        assert!(unknown["groups"].as_array().unwrap().iter().all(|group| {
            group["alternatives"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["inventory_count"].is_null())
        }));
    }
    #[test]
    fn malformed_duplicate_or_oversized_inputs_are_explicit_errors() {
        let mut obs = observation();
        obs.player["inventory"][1]["slot"] = json!(0);
        assert!(material_summary(&obs, &recipe()).is_err());
        let mut recipe = recipe();
        recipe["ingredients"] = json!(vec![ingredient("fixture:x"); 65]);
        assert!(material_summary(&observation(), &recipe).is_err());
        recipe["ingredients"] =
            json!([{"matching_stacks":[{"item":"not a runtime id","count":1}]}]);
        assert!(material_summary(&observation(), &recipe).is_err());
    }
}
