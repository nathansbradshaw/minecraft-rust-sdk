//! Response-only delivery metadata. Never rewrite stored native evidence.
use minecraft_sdk_core::Receipt;
use serde_json::json;

pub(crate) fn existing(mut receipt: Receipt) -> Receipt {
    if !receipt.evidence.is_object() {
        receipt.evidence = json!({"original_evidence":receipt.evidence});
    }
    receipt.evidence["gateway_delivery"] = json!({
        "action_id_already_recorded":true,
        "new_intention_dispatched":false,
        "scope":"Existing receipt returned for this exact action ID and intent. Outcome and native effects are historical. Reobserve current state; inspect/reconcile uncertain IDs without replay."});
    receipt
}

#[cfg(test)]
mod tests {
    use super::*;
    use minecraft_sdk_core::{ApiError, ErrorCode, Outcome};
    #[test]
    fn delivery_does_not_relabel_old_effects_or_resolve_interruption() {
        for outcome in [
            Outcome::Observed,
            Outcome::Expired,
            Outcome::Running,
            Outcome::Uncertain,
        ] {
            let stored = Receipt {
                action_id: "durable-own-intent".into(),
                outcome: outcome.clone(),
                adapter_version: "pinned-adapter".into(),
                evidence: json!({"native_effect_verified":outcome==Outcome::Observed,
                    "after":{"world":{"game_time":123}}}),
                error: Some(ApiError::new(ErrorCode::UncertainOutcome, "original error")),
            };
            let delivered = existing(stored.clone());
            assert_eq!(delivered.action_id, stored.action_id);
            assert_eq!(delivered.outcome, stored.outcome);
            assert_eq!(delivered.adapter_version, stored.adapter_version);
            assert_eq!(
                serde_json::to_value(&delivered.error).unwrap(),
                serde_json::to_value(&stored.error).unwrap()
            );
            let mut native = delivered.evidence.clone();
            native.as_object_mut().unwrap().remove("gateway_delivery");
            assert_eq!(native, stored.evidence);
            assert_eq!(
                delivered.evidence["gateway_delivery"]["new_intention_dispatched"],
                false
            );
            assert!(stored.evidence.get("gateway_delivery").is_none());
        }
    }
    #[test]
    fn empty_or_opaque_plugin_evidence_is_preserved_without_panicking() {
        for evidence in [serde_json::Value::Null, json!("opaque"), json!([1, 2, 3])] {
            let delivered = existing(Receipt {
                action_id: "own".into(),
                outcome: Outcome::Rejected,
                adapter_version: "fixture".into(),
                evidence: evidence.clone(),
                error: None,
            });
            assert_eq!(delivered.evidence["original_evidence"], evidence);
            assert_eq!(
                delivered.evidence["gateway_delivery"]["action_id_already_recorded"],
                true
            );
        }
    }
}
