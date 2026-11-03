//! Runtime recipe admission before any gameplay packet. Knowledge grants no items.
use minecraft_sdk_core::*;
use serde_json::Value;
pub(super) const CAPABILITY: &str = "minecraft:recipes";
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
