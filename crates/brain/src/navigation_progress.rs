//! Soft goal-progress diagnostics, separate from native execution and quotas.
use crate::{Journal, Observation, Result, digest};
use minecraft_sdk::navigation::Goal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const KEY: &str = "navigation_nonprogress";
// Settling within five centimetres does not justify a new reasoning session.
// This heuristic never changes native goal acceptance or effect verification.
const IMPROVEMENT: f64 = 0.05;

#[derive(Default, Serialize, Deserialize)]
struct State {
    basis: String,
    count: u32,
    requested: bool,
    best_gaps: BTreeMap<String, f64>,
    slices: Vec<Value>,
}
fn basis(source: &str, revision: &str, observation: &Observation) -> Option<String> {
    let p = &observation.player;
    if source.is_empty()
        || revision.is_empty()
        || p["uuid"].as_str().is_none_or(str::is_empty)
        || p["entity_id"].as_i64().is_none()
        || p["dimension"].as_str().is_none_or(str::is_empty)
        || observation.world["sample_clock"]["epoch"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return None;
    }
    Some(digest(
        json!([
            source,
            revision,
            p["uuid"],
            p["entity_id"],
            p["dimension"],
            observation.world["sample_clock"]["epoch"]
        ])
        .to_string()
        .as_bytes(),
    ))
}
fn point(v: &Value) -> Option<[f64; 3]> {
    let p = [v[0].as_f64()?, v[1].as_f64()?, v[2].as_f64()?];
    p.iter().all(|n| n.is_finite()).then_some(p)
}
fn gap(goal: &Goal, mut p: [f64; 3]) -> Option<f64> {
    let (target, r) = match goal {
        Goal::Position {
            target, distance, ..
        } => (*target, *distance),
        Goal::BlockReach {
            block,
            distance,
            eye_height,
            ..
        } => {
            if !eye_height.is_finite() || *eye_height <= 0. || *eye_height >= 10. {
                return None;
            }
            p[1] += eye_height;
            (
                std::array::from_fn(|i| p[i].clamp(f64::from(block[i]), f64::from(block[i]) + 1.)),
                *distance,
            )
        }
    };
    if !r.is_finite() || r < 0. || !target.iter().all(|n| n.is_finite()) {
        return None;
    }
    Some(
        ((0..3)
            .map(|i| (p[i] - target[i]).powi(2))
            .sum::<f64>()
            .sqrt()
            - r)
            .max(0.),
    )
}
fn clean_scheduling(result: &Value, observation: &Observation) -> Option<bool> {
    let before = &result["navigation_scheduling"]["before"];
    let after = &result["navigation_scheduling"]["after"];
    let valid = [before, after].into_iter().all(|b| {
        b["body_running"] == true
            && b["emergency"] == false
            && b["scheduling_revision"].as_u64().is_some()
    }) && ["actor", "entity", "dimension", "epoch"]
        .iter()
        .all(|k| before[*k] == after[*k])
        && after["actor"] == observation.player["uuid"]
        && after["entity"] == observation.player["entity_id"]
        && after["dimension"] == observation.player["dimension"]
        && after["epoch"] == observation.world["sample_clock"]["epoch"];
    if !valid {
        return None;
    }
    let previous = before["scheduling_revision"].as_u64()?;
    let current = after["scheduling_revision"].as_u64()?;
    // Urgency that started and finished within a complete slice is diagnostic
    // evidence, not a reason to discard that slice's lack of retained progress.
    (current >= previous).then_some(current > previous)
}
fn clean_result(result: &Value) -> bool {
    result["exit_code"] == 0
        && result["timed_out"] == false
        && result["navigation"]["error"].is_null()
        && result["navigation_slice"]["paused_for_control"] != true
        && result["error"].is_null()
        && result["output_files"]["error"].is_null()
        && result["local_action_feedback"]["error"].is_null()
        && result["local_action_feedback"]["omitted_records"] == 0
        && result["navigation"]["report"]
            .get("pending_action")
            .is_some_and(Value::is_null)
        && result["local_action_feedback"]["requests"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().all(|row| {
                    row["response"]["outcome"] == "observed"
                        && row["response"]["native_effect_verified"] == true
                        && row["response"]["error"].is_null()
                        && row["status_refresh_error"].is_null()
                })
            })
}
impl Journal {
    fn navigation_nonprogress_state(&self, basis: &str) -> Result<State> {
        Ok(self
            .get(KEY)?
            .map(|s| serde_json::from_str::<State>(&s))
            .transpose()?
            .filter(|s| s.basis == basis)
            .unwrap_or_else(|| State {
                basis: basis.into(),
                ..State::default()
            }))
    }
    /// Only complete, unpreempted helper slices participate. Ordinary movement
    /// receipts remain valid; this counter diagnoses choosing an unhelpful goal.
    pub(crate) fn record_navigation_nonprogress(
        &self,
        result: &Value,
        source: &str,
        revision: &str,
        observation: &Observation,
        work_active: bool,
    ) -> Result<()> {
        if self.get("goal_revision")?.as_deref() != Some(revision) {
            return Ok(());
        }
        let Some(basis) = basis(source, revision, observation) else {
            return Ok(());
        };
        let Some(body_cycled) = clean_scheduling(result, observation) else {
            return Ok(());
        };
        if !clean_result(result) {
            return Ok(());
        }
        let report = &result["navigation"]["report"];
        let status = report["status"].as_str().unwrap_or_default();
        if !matches!(status, "budget_exhausted" | "reached") {
            return Ok(());
        }
        let goal: Goal = match serde_json::from_value(result["navigation_slice"]["goal"].clone()) {
            Ok(g) => g,
            Err(_) => return Ok(()),
        };
        let dimension = match &goal {
            Goal::Position { dimension, .. } | Goal::BlockReach { dimension, .. } => dimension,
        };
        if observation.player["dimension"] != *dimension {
            return Ok(());
        }
        let Some(start) = point(&result["position_before"]).and_then(|p| gap(&goal, p)) else {
            return Ok(());
        };
        let Some(end) = report["remaining_distance"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.)
        else {
            return Ok(());
        };
        let Some(actual) = point(&report["position"]).and_then(|p| gap(&goal, p)) else {
            return Ok(());
        };
        if (actual - end).abs() > 0.000001 {
            return Ok(());
        }
        let reported_end = end;
        let retained_position = if body_cycled && status == "budget_exhausted" {
            let Some(position) = point(&result["position_after"]) else {
                return Ok(());
            };
            position
        } else {
            point(&report["position"]).expect("Validated report position")
        };
        let Some(end) = gap(&goal, retained_position) else {
            return Ok(());
        };
        let mut state = self.navigation_nonprogress_state(&basis)?;
        let goal_key = digest(serde_json::to_string(&goal)?.as_bytes());
        let best = *state.best_gaps.entry(goal_key.clone()).or_insert(start);
        // Require improvement within this slice AND beyond this goal's prior
        // best. Body repositioning or regaining lost distance is not new gain.
        let improved = end + IMPROVEMENT < start.min(best);
        if status == "reached" || improved {
            state.count = 0;
            state.requested = false;
            state.slices.clear();
            state.best_gaps.insert(goal_key.clone(), end);
        } else {
            state.count = state.count.saturating_add(1);
            state.slices.push(json!({"goal":goal,"start_gap":start,"end_gap":end,
                "previous_best_gap":best,"report_position":report["position"],
                "sdk_report_gap":reported_end,"retained_position":retained_position,
                "kind":if body_cycled{"body_navigation_backtrack"}else{"goal_gap_nonprogress"},
                "body_scheduling_revisions":[result["navigation_scheduling"]["before"]["scheduling_revision"],
                    result["navigation_scheduling"]["after"]["scheduling_revision"]],
                "scope":"A completed helper missed retained goal improvement; overlapping body activity is a cooperation diagnostic, not a failed or revoked native movement effect"}));
            if state.slices.len() > 2 {
                state.slices.remove(0);
            }
            if state.count >= 2 && !state.requested {
                state.requested = true;
                // Reuse any active work session, including a legacy sticky
                // request, instead of extending its remaining decisions.
                // The final active decision has already consumed its last
                // persisted turn before dispatch. It must not open a new four.
                let active = work_active
                    || (self.get("develop_next")?.as_deref() == Some("true")
                        && self
                            .get("development_turns_remaining")?
                            .is_none_or(|n| n.parse::<u32>().unwrap_or(0) > 0));
                let investigating = self.get("needs_investigation")?.as_deref() == Some("true")
                    || self
                        .get("investigation_remaining")?
                        .and_then(|n| n.parse::<u32>().ok())
                        .unwrap_or(0)
                        > 0;
                // Persist the one-shot latch before changing routing. Restart
                // cannot repeatedly mint sessions for these same conditions.
                self.set(KEY, &serde_json::to_string(&state)?)?;
                if !active && !investigating {
                    self.request_development()?;
                    self.set("development_request","Investigate navigation_nonprogress: recent bounded routes performed real movement without improving their requested goal. Check the chosen goal against current observed terrain and helper feedback, then use a discriminating read or changed bounded experiment. No gameplay success or impossibility has been established.")?;
                }
                self.event(
                    "navigation_nonprogress_diagnosis_requested",
                    json!({"basis":basis,
                    "count":state.count,"existing_session_reused":active||investigating,
                    "daily_allowances_changed":false,"legacy_investigation_changed":false,
                    "body_chat_and_tools":"remain available"}),
                )?;
            }
        }
        while state.best_gaps.len() > 16 {
            if let Some(key) = state
                .best_gaps
                .keys()
                .find(|key| key.as_str() != goal_key.as_str())
                .cloned()
            {
                state.best_gaps.remove(&key);
            } else {
                break;
            }
        }
        self.set(KEY, &serde_json::to_string(&state)?)?;
        self.event(
            "navigation_goal_gap_sample",
            json!({"basis":basis,"status":status,
            "start_gap":start,"end_gap":end,"sdk_report_gap":reported_end,"body_cycled":body_cycled,
            "retained_position":retained_position,"improved":improved,"nonprogress_slices":state.count,
            "diagnosis_requested":state.requested,"native_effects_reclassified":false}),
        )?;
        Ok(())
    }
    pub(crate) fn navigation_nonprogress_context(
        &self,
        source: &str,
        revision: &str,
        observation: &Observation,
    ) -> Result<Value> {
        let Some(basis) = basis(source, revision, observation) else {
            return Ok(Value::Null);
        };
        let state = self.navigation_nonprogress_state(&basis)?;
        if state.count == 0 {
            return Ok(Value::Null);
        }
        Ok(
            json!({"format":"navigation-goal-nonprogress-v1","count":state.count,
            "diagnosis_requested":state.requested,"recent_slices":state.slices,
            "scope":"Soft client diagnostic. Native execution remains valid. One existing bounded work session may investigate; repeated move vectors do not renew it. A closer/reached goal, new parent goal, changed source or native life can clear this context. New frontier relevance is not automatically classified. No quota reset, halt or gameplay failure claim."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observed() -> Observation {
        Observation {
            revision: "native".into(),
            player: json!({"uuid":"actor","entity_id":7,"dimension":"fixture:world"}),
            world: json!({"sample_clock":{"epoch":"life-one"}}),
            menu: Value::Null,
        }
    }
    fn slice(start: [f64; 3], end: [f64; 3]) -> Value {
        let goal = Goal::Position {
            dimension: "fixture:world".into(),
            target: [4.5, 63., -731.5],
            distance: 0.4,
        };
        let scheduling = json!({"actor":"actor","entity":7,"dimension":"fixture:world","epoch":"life-one",
            "body_running":true,"emergency":false,"scheduling_revision":8});
        json!({"success":false,"exit_code":0,"timed_out":false,"position_before":start,"position_after":end,
            "navigation":{"report":{"status":"budget_exhausted","pending_action":null,
                "position":end,"remaining_distance":gap(&goal,end).unwrap()}},
            "navigation_slice":{"format":"navigation-slice-v1","completed":false,"can_resume":true,
                "paused_for_control":false,"goal":goal},
            "navigation_scheduling":{"before":scheduling,"after":scheduling},
            "local_action_feedback":{"omitted_records":0,"requests":[{"request":{"action_id":"move-1",
                "action":{"kind":"start_move","target":{"x":end[0],"y":end[1],"z":end[2]}}},
                "response":{"outcome":"observed","native_effect_verified":true}}]}})
    }
    fn journal() -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        j.set("goal_revision", "parent").unwrap();
        (dir, j)
    }
    fn record(j: &Journal, v: &Value) {
        j.record_navigation_nonprogress(v, "source", "parent", &observed(), false)
            .unwrap();
    }
    fn body_backtrack() -> Value {
        let start = [284.5, 63., -731.5];
        let wet = [281.5, 62., -731.5];
        let goal = Goal::BlockReach {
            dimension: "fixture:world".into(),
            block: [275, 63, -731],
            distance: 4.,
            eye_height: 1.62,
        };
        let mut result = slice(start, wet);
        result["navigation_slice"]["goal"] = serde_json::to_value(&goal).unwrap();
        result["navigation"]["report"]["remaining_distance"] = json!(gap(&goal, wet).unwrap());
        result["position_after"] = json!(start);
        result["navigation_scheduling"]["after"]["scheduling_revision"] = json!(10);
        result
    }
    #[test]
    fn completed_block_reach_and_body_rescue_cycles_request_one_cooperation_diagnosis() {
        let (_dir, j) = journal();
        let v = body_backtrack();
        let native_receipts = v["local_action_feedback"].clone();
        record(&j, &v);
        let context = j
            .navigation_nonprogress_context("source", "parent", &observed())
            .unwrap();
        assert_eq!(context["count"], 1);
        let evidence = &context["recent_slices"][0];
        assert_eq!(evidence["kind"], "body_navigation_backtrack");
        assert!(
            evidence["sdk_report_gap"].as_f64().unwrap() < evidence["end_gap"].as_f64().unwrap(),
            "transient native arrival and retained post-rescue gap must stay separate"
        );
        assert_eq!(evidence["start_gap"], evidence["end_gap"]);
        assert_eq!(evidence["body_scheduling_revisions"], json!([8, 10]));
        record(&j, &v);
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("4")
        );
        for _ in 0..4 {
            assert!(j.take_development_turn().unwrap());
        }
        let mut repeated = v.clone();
        repeated["navigation_scheduling"]["after"]["scheduling_revision"] = json!(12);
        record(&j, &repeated);
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("0")
        );
        assert!(j.get("needs_investigation").unwrap().is_none());
        assert_eq!(
            v["local_action_feedback"], native_receipts,
            "native Observed receipts are never reclassified"
        );
    }
    #[test]
    fn completed_body_cycles_with_retained_closer_or_reached_goals_are_not_penalized() {
        let (_dir, j) = journal();
        let v = body_backtrack();
        record(&j, &v);
        let mut closer = v.clone();
        closer["position_after"] = closer["navigation"]["report"]["position"].clone();
        record(&j, &closer);
        assert!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()
                .is_null()
        );
        assert!(j.get("develop_next").unwrap().is_none());
        record(&j, &v);
        let mut reached = slice([1.5, 63., -731.5], [4.5, 63., -731.5]);
        reached["navigation_scheduling"]["after"]["scheduling_revision"] = json!(10);
        reached["navigation"]["report"]["status"] = json!("reached");
        record(&j, &reached);
        assert!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()
                .is_null()
        );
        assert!(j.get("develop_next").unwrap().is_none());
    }
    #[test]
    fn actual_preemption_urgency_pending_and_uncertainty_still_exclude_body_cycles() {
        let (_dir, j) = journal();
        let v = body_backtrack();
        for (path, value) in [
            ("/navigation/report/status", json!("preempted")),
            ("/navigation_slice/paused_for_control", json!(true)),
            (
                "/navigation/report/pending_action",
                json!("pending-original-id"),
            ),
            ("/navigation_scheduling/before/emergency", json!(true)),
            ("/navigation_scheduling/after/emergency", json!(true)),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("uncertain"),
            ),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("cancelled"),
            ),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("reconciled"),
            ),
        ] {
            let mut excluded = v.clone();
            *excluded.pointer_mut(path).unwrap() = value;
            record(&j, &excluded);
            assert!(
                j.get(KEY).unwrap().is_none(),
                "excluded body-cycle boundary {path}"
            );
        }
    }
    #[test]
    fn motion_and_new_vectors_remain_valid_but_do_not_renew_goal_progress_or_work_sessions() {
        let (dir, j) = journal();
        let first = slice([1.5, 63., -731.5], [0.5, 63., -731.5]);
        let second = slice([0.5, 63., -731.5], [1., 63., -731.5]);
        assert!(crate::progress::executed(
            "navigate", &first, "source", "source"
        ));
        assert_ne!(
            crate::progress::experiment_signature("source", &first),
            crate::progress::experiment_signature("source", &second)
        );
        j.set("sol_daily_reset", "2000000000").unwrap();
        j.set("model_admissions", "17").unwrap();
        record(&j, &first);
        assert_eq!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()["count"],
            1
        );
        assert!(j.get("develop_next").unwrap().is_none());
        record(&j, &second);
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("4")
        );
        assert!(
            j.get("needs_investigation").unwrap().is_none(),
            "soft diagnosis cannot enter the exhausted-work halt"
        );
        for _ in 0..4 {
            assert!(j.take_development_turn().unwrap());
        }
        assert!(!j.take_development_turn().unwrap());
        drop(j);
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        j.set("discovery_revision", "changed-origin-and-query-hash")
            .unwrap();
        record(&j, &first);
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("0")
        );
        assert!(
            !j.take_development_turn().unwrap(),
            "repeat vectors or a raw query revision cannot grant another session"
        );
        assert_eq!(j.get("model_admissions").unwrap().as_deref(), Some("17"));
        assert_eq!(
            j.get("sol_daily_reset").unwrap().as_deref(),
            Some("2000000000")
        );
        assert_eq!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()["count"],
            3
        );
    }
    #[test]
    fn active_development_or_real_investigation_is_not_extended_or_overwritten() {
        for investigation in [false, true] {
            let (_dir, j) = journal();
            if investigation {
                j.set("needs_investigation", "true").unwrap();
            } else {
                j.set("develop_next", "true").unwrap();
                j.set("development_turns_remaining", "2").unwrap();
            }
            j.set(
                "development_request",
                "Existing compiler or gameplay investigation",
            )
            .unwrap();
            let v = slice([1.5, 63., -731.5], [0.5, 63., -731.5]);
            record(&j, &v);
            record(&j, &v);
            assert_eq!(
                j.get("development_request").unwrap().as_deref(),
                Some("Existing compiler or gameplay investigation")
            );
            assert_eq!(
                j.get("development_turns_remaining").unwrap().as_deref(),
                if investigation { None } else { Some("2") }
            );
            assert_eq!(
                j.navigation_nonprogress_context("source", "parent", &observed())
                    .unwrap()["diagnosis_requested"],
                true
            );
        }
        let (_dir, j) = journal();
        j.set("develop_next", "false").unwrap();
        j.set("development_turns_remaining", "0").unwrap();
        let v = slice([1.5, 63., -731.5], [0.5, 63., -731.5]);
        for _ in 0..2 {
            j.record_navigation_nonprogress(&v, "source", "parent", &observed(), true)
                .unwrap();
        }
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("0"),
            "the current session's final consumed decision cannot renew four turns"
        );
    }
    #[test]
    fn pending_preempted_uncertain_and_urgent_slices_never_count_or_erase_history() {
        let (_dir, j) = journal();
        let v = slice([1.5, 63., -731.5], [0.5, 63., -731.5]);
        record(&j, &v);
        for (path, value) in [
            (
                "/navigation/report/pending_action",
                json!("preserved-original-id"),
            ),
            ("/navigation/report/status", json!("preempted")),
            ("/navigation_slice/paused_for_control", json!(true)),
            ("/timed_out", json!(true)),
            ("/local_action_feedback/omitted_records", json!(1)),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("uncertain"),
            ),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("reconciled"),
            ),
            (
                "/local_action_feedback/requests/0/response/outcome",
                json!("cancelled"),
            ),
            (
                "/local_action_feedback/requests/0/response/native_effect_verified",
                json!(false),
            ),
            ("/navigation_scheduling/after/scheduling_revision", json!(7)),
            ("/navigation_scheduling/before/emergency", json!(true)),
            ("/navigation_scheduling/after/emergency", json!(true)),
            ("/navigation_scheduling/after/body_running", json!(false)),
            ("/navigation_scheduling/after/epoch", json!("new-life")),
            ("/navigation/report/remaining_distance", json!(0.01)),
        ] {
            let mut excluded = v.clone();
            *excluded.pointer_mut(path).unwrap() = value;
            let before = j.get(KEY).unwrap();
            record(&j, &excluded);
            assert_eq!(j.get(KEY).unwrap(), before, "excluded boundary {path}");
        }
        let mut error = v.clone();
        error["local_action_feedback"]["error"] = json!("capture unavailable");
        record(&j, &error);
        let mut unknown = v;
        unknown
            .as_object_mut()
            .unwrap()
            .remove("navigation_scheduling");
        record(&j, &unknown);
        assert_eq!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()["count"],
            1
        );
        assert!(j.get("develop_next").unwrap().is_none());
    }
    #[test]
    fn real_closer_and_reached_goals_clear_the_soft_latch_without_inventory_requirements() {
        let (_dir, j) = journal();
        let v = slice([1.5, 63., -731.5], [0.5, 63., -731.5]);
        record(&j, &v);
        record(&j, &v);
        let closer = slice([1.5, 63., -731.5], [2.5, 63., -731.5]);
        record(&j, &closer);
        assert!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()
                .is_null()
        );
        record(&j, &v);
        assert_eq!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()["count"],
            1
        );
        let mut reached = slice([2.5, 63., -731.5], [4.5, 63., -731.5]);
        reached["navigation"]["report"]["status"] = json!("reached");
        record(&j, &reached);
        assert!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()
                .is_null()
        );
    }
    #[test]
    fn parent_source_and_native_life_are_separate_counter_scopes() {
        let (_dir, j) = journal();
        record(&j, &slice([1.5, 63., -731.5], [0.5, 63., -731.5]));
        for (source, revision) in [("changed-source", "parent"), ("source", "other-parent")] {
            assert!(
                j.navigation_nonprogress_context(source, revision, &observed())
                    .unwrap()
                    .is_null()
            );
        }
        let mut next = observed();
        next.world["sample_clock"]["epoch"] = json!("life-two");
        assert!(
            j.navigation_nonprogress_context("source", "parent", &next)
                .unwrap()
                .is_null()
        );
        assert_eq!(
            j.navigation_nonprogress_context("source", "parent", &observed())
                .unwrap()["count"],
            1
        );
    }
    #[test]
    fn block_reach_gap_uses_nearest_block_surface_and_caller_eye_height() {
        let goal = Goal::BlockReach {
            dimension: "fixture:world".into(),
            block: [0, 64, -732],
            distance: 4.,
            eye_height: 1.62,
        };
        assert_eq!(gap(&goal, [0.5, 64., -731.5]), Some(0.));
        assert!(gap(&goal, [10.5, 64., -731.5]).unwrap() > 5.);
    }
}
