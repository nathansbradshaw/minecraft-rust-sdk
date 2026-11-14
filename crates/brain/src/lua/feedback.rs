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

fn acquisition(value: &Value) -> Value {
    // Current SDK acquisition DTOs contain no Receipt/native scene. Preserve
    // their exact intentions, counts, outcomes and life bindings. Only their
    // untyped sample_clock can carry unrelated oversized diagnostic metadata.
    let mut result = value.clone();
    if value["sample_clock"].to_string().len() > 2048 {
        result["sample_clock"] = json!({"epoch":value["sample_clock"]["epoch"],
            "nanos":value["sample_clock"]["nanos"],"feedback_view":{
                "partial":true,"original":marker(&value["sample_clock"])}});
    }
    result
}

fn navigation(value: &Value) -> Value {
    let mut result = json!({"error":error(&value["error"]),"feedback_view":{
        "partial":true,"original":marker(value),
        "scope":"Historical SDK helper result, not refreshed or reclassified from a waypoint receipt. Navigation status does not establish a mined block or retained item."}});
    if let Some(report) = value.get("report").filter(|v| v.is_object()) {
        let mut view = json!({"note":text(&report["note"]),
            "note_complete":report["note"].as_str().is_some_and(|s|s.len()<=1024),
            "feedback_view":{"partial":true,"original":marker(report)}});
        for name in [
            "status",
            "pending_action",
            "position",
            "remaining_distance",
            "actions",
            "elapsed_ms",
            "preview_waits",
        ] {
            view[name] = report[name].clone();
        }
        for name in [
            "ready_pages",
            "pending_busy",
            "pending_other",
            "scene_restarts",
            "eager_intentions",
        ] {
            view["preview"][name] = report["preview"][name].clone();
        }
        // Keep original acquisition/settlement IDs and evidence together, or
        // fail the global bound. Never manufacture resource credit or omit an
        // unresolved cancellation identity while retaining its positive count.
        for name in ["inventory_completion", "inventory_cancellation"] {
            if let Some(field) = report.get(name) {
                view[name] = acquisition(field);
            }
        }
        result["report"] = view;
    }
    result
}

fn navigation_slice(value: &Value) -> Value {
    let mut result = json!({"feedback_view":{"partial":true,"original":marker(value),
        "scope":"Original helper slice classification and exact caller goal. A later inspected waypoint receipt does not change completed/can_resume or certify task completion."}});
    for name in [
        "format",
        "completed",
        "can_resume",
        "paused_for_control",
        "completion_kind",
        "goal",
        "until_inventory",
        "resume_tool",
    ] {
        result[name] = value[name].clone();
    }
    for name in ["resume_condition", "scope"] {
        result[name] = text(&value[name]);
        result[format!("{name}_complete")] =
            json!(value[name].as_str().is_some_and(|s| s.len() <= 1024));
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
        "exit_code",
        "timed_out",
        "elapsed_ms",
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
    if value["navigation"].is_object() {
        result["navigation"] = navigation(&value["navigation"]);
    }
    if value["navigation_slice"].is_object() {
        result["navigation_slice"] = navigation_slice(&value["navigation_slice"]);
    }
    if let Some(failure) = value.get("navigation_result_error") {
        result["navigation_result_error"] = text(failure);
        result["navigation_result_error_complete"] =
            json!(failure.as_str().is_some_and(|s| s.len() <= 1024));
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

    fn navigation_result() -> Value {
        // Actual public SDK serialization, not a parallel invented report schema.
        let report = minecraft_sdk::navigation::Report {
            elapsed_ms: 30000,
            status: "budget_exhausted".into(),
            position: [1., 2., 3.],
            remaining_distance: 2.25,
            actions: 3,
            preview_waits: 2,
            preview: minecraft_sdk::navigation::PreviewStats {
                ready_pages: 4,
                pending_busy: 1,
                pending_other: 2,
                scene_restarts: 3,
                eager_intentions: 3,
            },
            pending_action: Some("original-waypoint".into()),
            journal: "/state/fixture-navigation.json".into(),
            note: "Operation deadline reached; inspect the original pending ID before another intention".into(),
            inventory_completion: None,
            inventory_cancellation: None,
        };
        let goal = minecraft_sdk::navigation::Goal::BlockReach {
            dimension: "fixture:dimension".into(),
            block: [4, 8, 12],
            distance: 2.,
            eye_height: 1.5,
        };
        json!({"success":false,"exit_code":0,"timed_out":false,
            "navigation":{"report":report},"navigation_slice":{
                "format":"navigation-slice-v1","goal":goal,"until_inventory":null,
                "completed":false,"can_resume":true,"paused_for_control":false,
                "completion_kind":null,"resume_condition":"Reobserve and continue the bounded slice",
                "resume_tool":"navigate","scope":"Local bounded helper outcome; not parent-task completion"}})
    }

    #[test]
    fn large_serialized_navigation_report_reaches_real_lua_with_resume_and_progress() {
        let original = navigation_result();
        let mut result = original.clone();
        // Pending inspection appends a real-shaped large native receipt, just
        // as production settle() does before the next pure Lua decision.
        let receipt = json!({"action_id":"original-waypoint","outcome":"observed",
            "adapter_version":"fixture","error":null,"evidence":{
                "native_effect_verified":true,"input_released":true,
                "after":{"world":{"scene":"x".repeat(65536)}}}});
        settle(&mut result, receipt).unwrap();
        assert!(result.to_string().len() <= LIMIT);
        for key in [
            "status",
            "note",
            "position",
            "remaining_distance",
            "actions",
            "elapsed_ms",
            "preview_waits",
            "preview",
            "pending_action",
        ] {
            assert_eq!(
                result["navigation"]["report"][key], original["navigation"]["report"][key],
                "actual SDK Report field {key}"
            );
        }
        assert_eq!(result["navigation"]["report"]["note_complete"], true);
        assert_eq!(
            result["navigation"]["report"]["feedback_view"]["original"]["sha256"],
            digest(original["navigation"]["report"].to_string().as_bytes())
        );
        assert_eq!(
            result["navigation_slice"]["feedback_view"]["original"]["sha256"],
            digest(original["navigation_slice"].to_string().as_bytes())
        );
        assert_eq!(
            result["navigation_slice"]["goal"],
            original["navigation_slice"]["goal"]
        );
        // The production Lua parser/typed command validator can resume the
        // original goal using the delivered fields, without dispatching here.
        let source = "return {version=1,step=function(ctx) local r=ctx.last_result.navigation.report; local s=ctx.last_result.navigation_slice; assert(r.note and r.remaining_distance==2.25 and r.actions==3 and r.preview.ready_pages==4); assert(ctx.last_result.receipt.outcome=='observed' and not s.completed and s.can_resume); return {state={note=r.note},command={tool='navigate',input=s.goal}} end}";
        let next = super::super::step(source, &json!({"state":{},"last_result":result})).unwrap();
        let choice = next.command.unwrap().choice().unwrap();
        assert_eq!(choice.tool, "navigate");
        let resumed: Value = serde_json::from_str(&choice.input).unwrap();
        assert_eq!(resumed, original["navigation_slice"]["goal"]);
    }

    #[test]
    fn inspected_waypoint_never_promotes_historical_navigation_or_resource_completion() {
        for (status, completed, can_resume) in [
            ("budget_exhausted", false, true),
            ("preempted", false, true),
            ("target_not_reached", false, false),
            ("needs_reconciliation", false, false),
            ("reached", true, false),
        ] {
            let mut original = navigation_result();
            original["navigation"]["report"]["status"] = json!(status);
            original["navigation_slice"]["completed"] = json!(completed);
            original["navigation_slice"]["can_resume"] = json!(can_resume);
            original["navigation_slice"]["paused_for_control"] = json!(status == "preempted");
            let mut result = original.clone();
            let mut receipt = large("observed");
            receipt["action_id"] = json!("original-waypoint");
            settle(&mut result, receipt).unwrap();
            assert_eq!(result["receipt"]["outcome"], "observed");
            assert_eq!(result["navigation"]["report"]["status"], status);
            for key in [
                "completed",
                "can_resume",
                "paused_for_control",
                "goal",
                "until_inventory",
                "completion_kind",
            ] {
                assert_eq!(
                    result["navigation_slice"][key],
                    original["navigation_slice"][key]
                );
            }
            assert!(
                result["navigation"]["report"]
                    .get("inventory_completion")
                    .is_none()
            );
            assert!(result.get("inventory_changed").is_none());
            assert_eq!(
                result["navigation"]["report"]["pending_action"], "original-waypoint",
                "historical pending ID is not rewritten by projection"
            );
        }
    }

    #[test]
    fn large_navigation_errors_unknowns_and_exact_uncertain_ids_remain_actionable() {
        let error_only = json!({"success":false,"navigation":{"error":{"code":"local_preempted","message":"Body owns the action lane"}},"stdout":"x".repeat(40000)});
        let result = bounded(error_only).unwrap();
        assert_eq!(result["navigation"]["error"]["code"], "local_preempted");
        assert_eq!(
            result["navigation"]["error"]["message"],
            "Body owns the action lane"
        );
        assert!(result["navigation"].get("report").is_none());
        assert!(result.get("navigation_slice").is_none());
        let mut original = navigation_result();
        original["navigation"]["report"]["note"] = json!("é\\\"\n".repeat(12000));
        original["navigation_slice"]["completed"] = Value::Null;
        original["navigation_slice"]["can_resume"] = Value::Null;
        let clock = json!({"epoch":"fixture-clock","nanos":"17","diagnostics":"x".repeat(40000)});
        let condition = minecraft_sdk::navigation::RetainedInventory {
            item: "fixture:result".into(),
            minimum: 1,
        };
        let cancellation = minecraft_sdk::navigation::InventoryCancellation {
            request: minecraft_sdk::ActionRequest {
                action_id: "original-waypoint-acquisition-cancel".into(),
                action: minecraft_sdk::Action::CancelControl {
                    action_id: "original-waypoint".into(),
                },
            },
            control_action_id: "original-waypoint".into(),
            outcome: Some(minecraft_sdk::Outcome::Uncertain),
            condition: condition.clone(),
            observed_count: 1,
            observation_revision: "fixture-revision".into(),
            actor: "fixture-actor".into(),
            life: "fixture-life".into(),
            dimension: "fixture:dimension".into(),
            sample_clock: clock.clone(),
            local_pre_dispatch_rejected: false,
        };
        original["navigation"]["report"]["inventory_cancellation"] =
            serde_json::to_value(cancellation).unwrap();
        let result = bounded(original.clone()).unwrap();
        assert_eq!(pending(&original), Some("original-waypoint".into()));
        assert!(result["navigation_slice"]["completed"].is_null());
        assert!(result["navigation_slice"]["can_resume"].is_null());
        assert_eq!(result["navigation"]["report"]["note_complete"], false);
        assert!(
            result["navigation"]["report"]["note"]
                .as_str()
                .unwrap()
                .len()
                <= 1024
        );
        let projected = &result["navigation"]["report"]["inventory_cancellation"];
        for (key, field) in original["navigation"]["report"]["inventory_cancellation"]
            .as_object()
            .unwrap()
        {
            if key != "sample_clock" {
                assert_eq!(&projected[key], field);
            }
        }
        assert_eq!(projected["outcome"], "uncertain");
        assert_eq!(projected["sample_clock"]["epoch"], clock["epoch"]);
        assert_eq!(projected["sample_clock"]["nanos"], clock["nanos"]);
        assert_eq!(
            projected["sample_clock"]["feedback_view"]["original"]["sha256"],
            digest(clock.to_string().as_bytes())
        );
        assert!(result.to_string().len() <= LIMIT);
        let mut completion_result = navigation_result();
        completion_result["success"] = json!(true);
        completion_result["navigation"]["report"]["status"] = json!("inventory_satisfied");
        completion_result["navigation"]["report"]["pending_action"] = Value::Null;
        completion_result["navigation"]["report"]["inventory_completion"] =
            serde_json::to_value(minecraft_sdk::navigation::InventoryCompletion {
                condition,
                observed_count: 1,
                condition_satisfied: true,
                owned_input_settled: true,
                settled_action_id: Some("original-waypoint".into()),
                input_settlement: "observed release".into(),
                observation_revision: "fixture-revision".into(),
                actor: "fixture-actor".into(),
                life: "fixture-life".into(),
                sample_clock: clock,
            })
            .unwrap();
        completion_result["navigation_slice"]["completed"] = json!(true);
        completion_result["navigation_slice"]["can_resume"] = json!(false);
        completion_result["navigation_slice"]["until_inventory"] =
            json!({"item":"fixture:result","minimum":1});
        completion_result["navigation_slice"]["completion_kind"] = json!("retained_inventory");
        let completed = bounded(completion_result.clone()).unwrap();
        for key in [
            "settled_action_id",
            "observed_count",
            "condition",
            "condition_satisfied",
            "owned_input_settled",
            "actor",
            "life",
        ] {
            assert_eq!(
                completed["navigation"]["report"]["inventory_completion"][key],
                completion_result["navigation"]["report"]["inventory_completion"][key]
            );
        }
        assert_eq!(completed["navigation_slice"]["completed"], true);
        assert!(
            completed.get("inventory_changed").is_none(),
            "no new resource-credit field invented"
        );
        assert!(completed.to_string().len() <= LIMIT);
        let mut oversized_goal = original.clone();
        oversized_goal["navigation_slice"]["goal"] = json!({"critical_goal":"x".repeat(40000)});
        assert!(
            bounded(oversized_goal)
                .unwrap_err()
                .to_string()
                .contains("never replay")
        );
        original["navigation"]["report"]["inventory_cancellation"]["request"]["action_id"] =
            json!("x".repeat(40000));
        assert!(
            bounded(original)
                .unwrap_err()
                .to_string()
                .contains("never replay")
        );
    }
}
