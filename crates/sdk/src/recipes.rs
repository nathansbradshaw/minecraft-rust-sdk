//! Optional read-only runtime recipe discovery. No recipe or material policy.
use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub recipe_type: Option<String>,
    pub offset: usize,
    pub limit: u8,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            recipe: None,
            output: None,
            recipe_type: None,
            offset: 0,
            limit: 8,
        }
    }
}
impl Query {
    pub fn request(&self) -> Result<ObservationQuery> {
        if !(1..=32).contains(&self.limit)
            || [&self.recipe, &self.output, &self.recipe_type]
                .into_iter()
                .flatten()
                .any(|id| !registry_id(id))
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Exact runtime recipe/item/type IDs and limit1..32 required",
            ));
        }
        Ok(ObservationQuery {
            capability: "minecraft:recipes".into(),
            input: serde_json::to_value(self)
                .map_err(|_| ApiError::new(ErrorCode::Internal, "Recipe query encoding failed"))?,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_query_has_no_fixed_material_or_actor_binding() {
        let mut q = Query {
            output: Some("custom:material".into()),
            ..Query::default()
        };
        let request = q.request().unwrap();
        assert_eq!(request.capability, "minecraft:recipes");
        assert_eq!(
            request.input,
            serde_json::json!({"output":"custom:material","offset":0,"limit":8})
        );
        q.recipe = Some("not namespaced".into());
        assert!(q.request().is_err());
        q.recipe = None;
        q.limit = 33;
        assert!(q.request().is_err());
        assert!(serde_json::from_str::<Query>(r#"{"player_uuid":"forged"}"#).is_err());
    }
}
