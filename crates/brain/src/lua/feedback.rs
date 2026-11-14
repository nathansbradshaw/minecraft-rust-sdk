//! Bounded Lua views of already-journalled results, never action authority.
use crate::{Result, digest, ensure};
use serde_json::{Value, json};

pub(super) const LIMIT: usize = 32768;

/// Read from the full result before projection. Status/reconcile use a bare
/// receipt; SDK dispatch uses a wrapper. Neither form may lose an unsettled ID.
pub(super) fn pending(value: &Value) -> Option<String> {
    serde_json::from_value::<minecraft_sdk::Receipt>(value.get("receipt").unwrap_or(value).clone())
        .ok()
        .filter(|receipt| !receipt.outcome.terminal())
        .map(|receipt| receipt.action_id)
        .or_else(|| {
            value["navigation"]["report"]["pending_action"]
                .as_str()
                .map(str::to_owned)
        })
}

fn marker(value: &Value) -> Value {
    let bytes = value.to_string();
    json!({"omitted":true,"original_bytes":bytes.len(),"sha256":digest(bytes.as_bytes())})
}

fn text(value: &Value) -> Value {
    let Some(value) = value.as_str() else {
        return Value::Null;
    };
    let mut end = value.len().min(1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    json!(&value[..end])
}

fn error(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if value.is_string() {
        return json!({"message":text(value),"message_complete":value.as_str().is_some_and(|s|s.len()<=1024),"original":marker(value)});
    }
    json!({"code":value["code"],"message":text(&value["message"]),"message_complete":value["message"].as_str().is_some_and(|s|s.len()<=1024),"original":marker(value)})
}

fn receipt(value: &Value) -> Value {
    let evidence = &value["evidence"];
    let boolean = |key: &str| evidence[key].as_bool().map_or(Value::Null, Value::Bool);
    let mut result = json!({"action_id":value["action_id"],"outcome":value["outcome"],
        "adapter_version":value["adapter_version"],"error":error(&value["error"]),
        "evidence":{"native_effect_verified":boolean("native_effect_verified"),
            "historical_effect_verified":boolean("historical_effect_verified"),
            "input_released":boolean("input_released"),
            "gateway_delivery":{
                "action_id_already_recorded":evidence["gateway_delivery"]["action_id_already_recorded"].as_bool(),
                "new_intention_dispatched":evidence["gateway_delivery"]["new_intention_dispatched"].as_bool()},
            "reconciliation":if evidence["reconciliation"].to_string().len()<=2048 {evidence["reconciliation"].clone()} else {marker(&evidence["reconciliation"])}}
    });
    // Local reconciliation can return a bare proof rather than a Receipt. Keep
    // its explicit non-native scope and lack of historical effect credit.
    for name in [
        "scope",
        "gateway_status",
        "local_validation_error",
        "recorded_intent_sha256",
        "historical_effect_verified",
        "native_effect_verified",
        "replayed",
    ] {
        if let Some(field) = value.get(name) {
            result[name] = field.clone();
        }
    }
    result["feedback_view"] = json!({"partial":true,"original":marker(value),
        "scope":"Historical receipt view; full receipt remains in the Brain journal under this exact action ID. Outcome is unchanged; terminal does not imply success. Omitted native before/after is not evidence of an effect."});
    result
}

fn observed(value: &Value) -> Value {
    let mut result = json!({"revision":value["revision"],"player":{},"menu":{},"feedback_view":{
        "partial":true,"original":marker(value),"scope":"Historical focused observation; use ctx.observation for current full state, inventory and scene."}});
    for key in [
        "uuid", "life_id", "position", "health", "food", "air", "mining", "control",
    ] {
        let field = &value["player"][key];
        result["player"][key] = if field.to_string().len() <= 2048 {
            field.clone()
        } else {
            marker(field)
        };
    }
    for key in ["menu_id", "state_id", "type"] {
        let field = &value["menu"][key];
        result["menu"][key] = if field.to_string().len() <= 1024 {
            field.clone()
        } else {
            marker(field)
        };
    }
    result
}

/// Preserve protocol-critical layout and exact IDs. If even those cannot fit,
/// fail closed so the caller's durable in-flight fence prevents mutation replay.
pub(super) fn bounded(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() <= LIMIT {
        return Ok(value);
    }
    let mut result = json!({"truncated":true,"feedback_view":{
        "partial":true,"original":marker(&value),
        "scope":"Bounded result view, not a new receipt or gameplay success. Full SDK receipts/status/reconciliation remain in the Brain journal; ctx.observation is freshly observed."}});
    if value["receipt"].is_object() {
        result["receipt"] = receipt(&value["receipt"]);
    } else if value["action_id"].is_string() && value["outcome"].is_string() {
        // status/reconcile return bare receipts: never invent a wrapper.
        result = receipt(&value);
        result["truncated"] = json!(true);
    }
    for name in [
        "request",
        "pending_action",
        "success",
        "done",
        "status",
        "waiting_for_action",
    ] {
        if let Some(field) = value.get(name) {
            result[name] = field.clone();
        }
    }
    if let Some(field) = value.get("error") {
        result["error"] = error(field);
    }
    if value["after"].is_object() {
        result["after"] = observed(&value["after"]);
    }
    if let Some(report) = value["navigation"].get("report") {
        result["navigation"] = json!({"report":{"status":report["status"],
            "pending_action":report["pending_action"],"message":text(&report["message"]),
            "error":error(&report["error"]),"feedback_view":marker(report)}});
    }
    ensure(
        serde_json::to_vec(&result)?.len() <= LIMIT,
        "Lua safety-critical feedback exceeds32768 bytes; inspect original journalled IDs, never replay",
    )?;
    Ok(result)
}

/// Refresh the existing layout with the original pending ID's actual status.
/// Bound before another Lua decision; inspection must not reintroduce huge scenes.
pub(super) fn settle(result: &mut Value, receipt: Value) -> Result<()> {
    if result["action_id"].is_string() && result["outcome"].is_string() {
        *result = bounded(receipt)?;
    } else {
        result["receipt"] = receipt;
        *result = bounded(std::mem::take(result))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn large(outcome: &str) -> Value {
        json!({"action_id":"exact-original-id","outcome":outcome,"adapter_version":"fixture",
            "error":{"code":"uncertain_outcome","message":"é\\\"\n".repeat(6000)},
            "evidence":{"native_effect_verified":false,"input_released":false,
                "historical_effect_verified":false,
                "gateway_delivery":{"action_id_already_recorded":true,"new_intention_dispatched":false},
                "reconciliation":{"replayed":false},"before":{"scene":"x".repeat(40000)}}})
    }

    #[test]
    fn bare_status_preserves_every_outcome_unknowns_and_bounded_actionable_errors() {
        for outcome in [
            "prepared",
            "dispatched",
            "running",
            "observed",
            "cancelled",
            "expired",
            "failed",
            "rejected",
            "uncertain",
            "reconciled",
            "imported",
        ] {
            let original = large(outcome);
            let expected_pending =
                matches!(outcome, "prepared" | "dispatched" | "running" | "uncertain")
                    .then(|| "exact-original-id".to_owned());
            assert_eq!(pending(&original), expected_pending);
            assert_eq!(pending(&json!({"receipt":original})), expected_pending);
            let result = bounded(original.clone()).unwrap();
            assert!(result.to_string().len() <= LIMIT);
            assert!(result.get("receipt").is_none());
            assert_eq!(result["action_id"], original["action_id"]);
            assert_eq!(result["outcome"], outcome);
            assert_eq!(result["error"]["code"], "uncertain_outcome");
            assert_eq!(result["error"]["message_complete"], false);
            assert!(result["error"]["message"].as_str().unwrap().len() <= 1024);
            assert_eq!(result["evidence"]["native_effect_verified"], false);
            assert_eq!(result["evidence"]["historical_effect_verified"], false);
            assert_eq!(result["evidence"]["input_released"], false);
            assert_eq!(
                result["evidence"]["gateway_delivery"]["new_intention_dispatched"],
                false
            );
            assert_eq!(
                result["feedback_view"]["original"]["sha256"],
                digest(original.to_string().as_bytes())
            );
        }
        let result=bounded(json!({"action_id":"unknown","outcome":"uncertain","evidence":{"input_released":"true","scene":"x".repeat(40000)}})).unwrap();
        assert!(result["evidence"]["native_effect_verified"].is_null());
        assert!(result["evidence"]["input_released"].is_null());
    }

    #[test]
    fn pending_inspection_preserves_bare_layout_and_local_reconciliation_non_authority() {
        let mut result = large("running");
        let original = large("cancelled");
        settle(&mut result, original.clone()).unwrap();
        assert_eq!(result["outcome"], "cancelled");
        assert_eq!(result["action_id"], "exact-original-id");
        assert!(result.get("receipt").is_none());
        assert!(result.to_string().len() <= LIMIT);
        let proof = json!({"action_id":"local-rejection","outcome":"reconciled",
            "scope":"local client-rejected intention; not an authoritative game receipt",
            "historical_effect_verified":false,"native_effect_verified":false,"replayed":false,
            "gateway_status":"not_found","local_validation_error":"invalid_request",
            "omitted_scene":"x".repeat(40000)});
        let proof = bounded(proof).unwrap();
        assert_eq!(proof["outcome"], "reconciled");
        assert_eq!(proof["historical_effect_verified"], false);
        assert_eq!(proof["native_effect_verified"], false);
        assert_eq!(proof["replayed"], false);
        assert_eq!(proof["gateway_status"], "not_found");
        assert!(
            proof["scope"]
                .as_str()
                .unwrap()
                .contains("not an authoritative")
        );
    }

    #[test]
    fn bounded_navigation_keeps_pending_id_and_oversized_critical_identity_fails_closed() {
        let value = json!({"navigation":{"report":{"pending_action":"exact-pending-id",
            "status":"needs_reconciliation","message":"Inspect original ID","legs":"x".repeat(40000)}}});
        let result = bounded(value).unwrap();
        assert_eq!(
            result["navigation"]["report"]["pending_action"],
            "exact-pending-id"
        );
        assert_eq!(
            result["navigation"]["report"]["status"],
            "needs_reconciliation"
        );
        let mut value = large("uncertain");
        value["action_id"] = json!("x".repeat(40000));
        assert!(
            bounded(value)
                .unwrap_err()
                .to_string()
                .contains("never replay")
        );
        let unchanged = json!({"receipt":{"action_id":"small","outcome":"observed"}});
        assert_eq!(bounded(unchanged.clone()).unwrap(), unchanged);
    }
}
