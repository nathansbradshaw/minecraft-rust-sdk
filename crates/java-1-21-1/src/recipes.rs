//! Runtime recipe admission before any gameplay packet. Knowledge grants no items.
use minecraft_sdk_core::*;
use serde_json::Value;
pub(super) const CAPABILITY: &str = "minecraft:recipes";

/// Version-adapter knowledge, not a core/client menu-layout assumption. These
/// two Java menus have a recipe result at slot zero. Other menus need an adapter.
pub(super) struct Preparation {
    recipe: String,
    menu_id: i32,
    menu_type: String,
    item: String,
    count: u64,
}
impl Preparation {
    pub fn bind(recipe: &str, menu_id: i32, lookup: &Value, before: &Observation) -> Result<Self> {
        validate(recipe, lookup)?;
        if before.menu["menu_id"] != menu_id {
            return Err(ApiError::new(ErrorCode::Conflict, "Recipe menu is stale"));
        }
        let row = &lookup["matches"][0];
        let menu_type = before.menu["type"].as_str().unwrap_or_default();
        let item = row["result"]["item"].as_str().unwrap_or_default();
        let count = row["result"]["count"].as_u64().unwrap_or(0);
        if !matches!(menu_type, "minecraft:inventory" | "minecraft:crafting")
            || row["type"] != "minecraft:crafting"
            || !matches!(
                row["serializer"].as_str(),
                Some("minecraft:crafting_shaped" | "minecraft:crafting_shapeless")
            )
            || row["special"] != false
            || row["result_known"] != true
            || !registry_id(item)
            || item == "minecraft:air"
            || count == 0
            || count > u64::from(u32::MAX)
        {
            return Err(ApiError::new(
                ErrorCode::UnsupportedCapability,
                "Recipe preparation verification requires a supported crafting menu and a known static result; other menus or dynamic recipes need an adapter",
            ));
        }
        if row["serializer"] == "minecraft:crafting_shaped" {
            shaped_menu(row, menu_type)?;
        }
        Ok(Self {
            recipe: recipe.into(),
            menu_id,
            menu_type: menu_type.into(),
            item: item.into(),
            count,
        })
    }
    pub fn proof(&self, before: &Observation, after: &Observation) -> Option<Value> {
        if before.menu["menu_id"] != self.menu_id
            || after.menu["menu_id"] != self.menu_id
            || before.menu["type"] != self.menu_type
            || after.menu["type"] != self.menu_type
        {
            return None;
        }
        let before_grid = grid(&before.menu)?;
        let after_grid = grid(&after.menu)?;
        // Inventory pickup, state-ID churn, an unchanged old output, or clearing
        // another recipe are not evidence that this requested recipe is ready.
        if before_grid == after_grid {
            return None;
        }
        let output = after_grid.iter().find(|row| row["slot"] == 0)?;
        if output["item"] != self.item
            || output["count"] != self.count
            || after.menu["slots"]
                .as_array()?
                .iter()
                .find(|row| row["slot"] == 0)?["may_pickup"]
                != true
        {
            return None;
        }
        Some(
            serde_json::json!({"kind":"recipe_preparation","recipe":self.recipe,
            "menu_id":self.menu_id,"menu_type":self.menu_type,"output_slot":0,
            "result":{"item":self.item,"count":self.count},
            "verified_fields":["item","count"],"components_verified":false,
            "inventory_acquisition_verified":false,
            "scope":"Changed native crafting grid with the requested runtime item/count result ready. Output has not been taken; custom component semantics require an adapter."}),
        )
    }
}
/// Standard Java menu geometry belongs in this adapter. Runtime recipe-book
/// knowledge alone does not imply that the currently open grid can hold it.
fn shaped_menu(row: &Value, menu: &str) -> Result<()> {
    let dimension = |key: &str| {
        row["shape"][key]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotReady,
                    "Runtime shaped recipe dimensions are missing or malformed; no crafting packet sent",
                )
            })
    };
    let width = dimension("width")?;
    let height = dimension("height")?;
    let capacity = if menu == "minecraft:inventory" { 2 } else { 3 };
    if width > capacity || height > capacity {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!(
                "Recipe requires a {width}x{height} grid but current {menu} provides {capacity}x{capacity}; open a compatible crafting menu before retrying. No crafting packet sent"
            ),
        ));
    }
    Ok(())
}
fn grid(menu: &Value) -> Option<Vec<Value>> {
    let slots = menu["slots"].as_array()?;
    if slots.is_empty() || slots.len() > 512 {
        return None;
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut grid = Vec::new();
    for row in slots {
        let slot = row["slot"].as_u64()?;
        if !seen.insert(slot) {
            return None;
        }
        if !row["player_inventory"].as_bool()? {
            let item = row["item"].as_str()?;
            let count = row["count"].as_u64()?;
            if !registry_id(item) || (item == "minecraft:air") != (count == 0) {
                return None;
            }
            grid.push(serde_json::json!({"slot":slot,"item":item,"count":count,"components":row["components"]}));
        }
    }
    grid.sort_by_key(|row| row["slot"].as_u64());
    Some(grid)
}
pub(super) fn validate(recipe: &str, result: &Value) -> Result<()> {
    if result["format"] != "minecraft-runtime-recipes-v1"
        || result["complete"] != true
        || !result["matches"].is_array()
        || !result["unknown_recipe"].is_boolean()
    {
        return Err(ApiError::new(
            ErrorCode::NotReady,
            "Runtime recipe lookup incomplete",
        ));
    }
    if result["unknown_recipe"] == true {
        return Err(ApiError::new(
            ErrorCode::InvalidRequest,
            "Unknown installed recipe ID; discover exact IDs with minecraft:recipes",
        ));
    }
    let row = result["matches"]
        .as_array()
        .and_then(|rows| {
            (rows.len() == 1)
                .then(|| &rows[0])
                .filter(|row| row["id"] == recipe)
        })
        .ok_or_else(|| ApiError::new(ErrorCode::NotReady, "Runtime recipe identity mismatch"))?;
    match row["known"].as_bool() {
        Some(true) => Ok(()),
        Some(false) => Err(ApiError::new(
            ErrorCode::Conflict,
            "Recipe is not in this character's recipe book; the SDK does not unlock it",
        )),
        None => Err(ApiError::new(
            ErrorCode::NotReady,
            "Character recipe-book state unavailable",
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn lookup() -> Value {
        json!({"format":"minecraft-runtime-recipes-v1","complete":true,"unknown_recipe":false,
            "matches":[{"id":"fixture:recipe","known":true,"type":"minecraft:crafting",
                "serializer":"minecraft:crafting_shaped","shape":{"width":1,"height":1},"special":false,"result_known":true,"result":{"item":"fixture:result","count":4}}]})
    }
    fn observation() -> Observation {
        Observation {
            revision: "1".into(),
            player: json!({}),
            world: json!({}),
            menu: json!({"menu_id":0,"state_id":1,"type":"minecraft:inventory","slots":[
                {"slot":0,"item":"fixture:old_output","count":4,"player_inventory":false,"may_pickup":true},
                {"slot":1,"item":"fixture:raw","count":2,"player_inventory":false,"may_pickup":true},
                {"slot":36,"item":"minecraft:air","count":0,"player_inventory":true,"may_pickup":true}]}),
        }
    }
    #[test]
    fn cleared_old_recipe_and_unrelated_inventory_are_not_preparation() {
        // Minimized G2v9 native failure: the old output was uncollected. A
        // request requiring that output merely returned its raw inputs.
        let before = observation();
        let bound = Preparation::bind("fixture:recipe", 0, &lookup(), &before).unwrap();
        let mut after = before.clone();
        for slot in [0, 1] {
            after.menu["slots"][slot]["item"] = json!("minecraft:air");
            after.menu["slots"][slot]["count"] = json!(0);
        }
        after.menu["slots"][2]["item"] = json!("fixture:raw");
        after.menu["slots"][2]["count"] = json!(2);
        assert!(bound.proof(&before, &after).is_none());
        after = before.clone();
        after.menu["slots"][2]["item"] = json!("fixture:loot");
        after.menu["slots"][2]["count"] = json!(1);
        assert!(bound.proof(&before, &after).is_none());
        after.menu["slots"][0]["item"] = json!("fixture:result");
        assert!(bound.proof(&before, &after).is_some());
        // An existing correct output plus unrelated pickup is still not a new
        // preparation. Neither menu state IDs nor player slots prove it.
        let mut prepared = before.clone();
        prepared.menu["slots"][0]["item"] = json!("fixture:result");
        after.menu["state_id"] = json!(2);
        assert!(bound.proof(&prepared, &after).is_none());
    }
    #[test]
    fn preparation_matches_bound_menu_result_and_remains_distinct_from_acquisition() {
        let before = observation();
        let bound = Preparation::bind("fixture:recipe", 0, &lookup(), &before).unwrap();
        let mut after = before.clone();
        after.menu["slots"][0]["item"] = json!("fixture:result");
        let proof = bound.proof(&before, &after).unwrap();
        assert_eq!(proof["kind"], "recipe_preparation");
        assert_eq!(proof["inventory_acquisition_verified"], false);
        assert_eq!(proof["components_verified"], false);
        for (path, value) in [
            ("/menu/menu_id", json!(1)),
            ("/menu/type", json!("fixture:menu")),
            ("/menu/slots/0/item", json!("fixture:wrong")),
            ("/menu/slots/0/count", json!(3)),
            ("/menu/slots/0/may_pickup", json!(false)),
            ("/menu/slots/0/player_inventory", json!(true)),
            ("/menu/slots/1/slot", json!(0)),
        ] {
            let mut bad = after.clone();
            *bad.menu
                .pointer_mut(path.strip_prefix("/menu").unwrap())
                .unwrap() = value;
            assert!(bound.proof(&before, &bad).is_none(), "accepted {path}");
        }
    }
    #[test]
    fn unsupported_menu_dynamic_result_and_stale_binding_fail_before_dispatch() {
        let before = observation();
        assert_eq!(
            Preparation::bind("fixture:recipe", 1, &lookup(), &before)
                .err()
                .unwrap()
                .code,
            ErrorCode::Conflict
        );
        for (path, value) in [
            ("/matches/0/special", json!(true)),
            ("/matches/0/serializer", json!("fixture:custom_dynamic")),
            ("/matches/0/result_known", json!(false)),
            ("/matches/0/type", json!("fixture:machine")),
            ("/matches/0/result/count", json!(0)),
            ("/matches/0/result/item", json!("minecraft:air")),
        ] {
            let mut bad = lookup();
            *bad.pointer_mut(path).unwrap() = value;
            assert_eq!(
                Preparation::bind("fixture:recipe", 0, &bad, &before)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::UnsupportedCapability
            );
        }
        let mut other = before.clone();
        other.menu["type"] = json!("fixture:unknown");
        assert_eq!(
            Preparation::bind("fixture:recipe", 0, &lookup(), &other)
                .err()
                .unwrap()
                .code,
            ErrorCode::UnsupportedCapability
        );
        other.menu["type"] = json!("minecraft:crafting");
        assert!(Preparation::bind("fixture:recipe", 0, &lookup(), &other).is_ok());
    }
    #[test]
    fn installed_shaped_recipe_must_fit_both_current_grid_axes() {
        let mut before = observation();
        for width in 1..=4 {
            for height in 1..=4 {
                let mut found = lookup();
                found["matches"][0]["shape"] = json!({"width":width,"height":height});
                for (menu, capacity) in [("minecraft:inventory", 2), ("minecraft:crafting", 3)] {
                    before.menu["type"] = json!(menu);
                    let result = Preparation::bind("fixture:recipe", 0, &found, &before);
                    if width <= capacity && height <= capacity {
                        assert!(result.is_ok(), "{width}x{height} in {menu}");
                    } else {
                        let error = result.err().unwrap();
                        assert_eq!(error.code, ErrorCode::Conflict);
                        assert!(error.message.contains("No crafting packet sent"));
                        assert!(error.message.contains(&format!("{width}x{height}")));
                    }
                }
            }
        }
    }
    #[test]
    fn unknown_shaped_geometry_never_defaults_to_a_fitting_recipe() {
        for shape in [
            Value::Null,
            json!({"width":1}),
            json!({"width":0,"height":1}),
            json!({"width":1,"height":-1}),
            json!({"width":"2","height":2}),
            json!({"width":1.5,"height":2}),
        ] {
            let mut found = lookup();
            found["matches"][0]["shape"] = shape;
            assert_eq!(
                Preparation::bind("fixture:recipe", 0, &found, &observation())
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::NotReady
            );
        }
        // Shapeless recipes do not have shaped bounding-box metadata. Their
        // existing admission is unchanged; this check makes no ingredient-fit
        // or material-sufficiency claim for either serializer.
        let mut found = lookup();
        found["matches"][0]["serializer"] = json!("minecraft:crafting_shapeless");
        found["matches"][0].as_object_mut().unwrap().remove("shape");
        assert!(Preparation::bind("fixture:recipe", 0, &found, &observation()).is_ok());
    }
    #[test]
    fn recipe_admission_requires_exact_installed_and_known_identity() {
        let mut result = json!({"format":"minecraft-runtime-recipes-v1","complete":true,"unknown_recipe":false,"matches":[{"id":"fixture:recipe","known":true}]});
        assert!(validate("fixture:recipe", &result).is_ok());
        assert_eq!(
            validate("other:recipe", &result).unwrap_err().code,
            ErrorCode::NotReady
        );
        result["matches"][0]["known"] = json!(false);
        assert_eq!(
            validate("fixture:recipe", &result).unwrap_err().code,
            ErrorCode::Conflict
        );
        result["unknown_recipe"] = json!(true);
        result["matches"] = json!([]);
        assert_eq!(
            validate("fixture:recipe", &result).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        result["complete"] = json!(false);
        assert_eq!(
            validate("fixture:recipe", &result).unwrap_err().code,
            ErrorCode::NotReady
        );
    }
}
