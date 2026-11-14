//! Bounded agent hypotheses for current work, separate from terminal evidence.
use crate::{Brain, Journal, Result, completion, digest, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const KEY: &str = "active_goal_plan";
pub(crate) const CONTENT_BYTES: usize = 2048;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Content {
    next_step: String,
    dependencies: Vec<String>,
    resources: Vec<String>,
}
impl Content {
    fn validate(&self) -> Result<()> {
        ensure(
            !self.next_step.trim().is_empty() && self.next_step.len() <= 768,
            "Plan needs a bounded nonempty next_step",
        )?;
        for rows in [&self.dependencies, &self.resources] {
            ensure(
                rows.len() <= 8 && rows.iter().all(|s| !s.trim().is_empty() && s.len() <= 256),
                "Plan has at most8 bounded nonempty dependencies/resources",
            )?;
        }
        ensure(
            serde_json::to_vec(self)?.len() <= CONTENT_BYTES,
            "Complete plan content exceeds2048bytes",
        )
    }
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Scope {
    goal: String,
    revision: String,
    completion_contract: String,
}
impl Scope {
    fn current(journal: &Journal) -> Result<Self> {
        let get = |key| {
            journal.get(key)?.filter(|s| !s.is_empty()).ok_or_else(|| {
                Box::<dyn std::error::Error + Send + Sync>::from(
                    "Active goal/revision/completion contract unavailable",
                )
            })
        };
        Ok(Self {
            goal: get("goal")?,
            revision: get("goal_revision")?,
            completion_contract: get("completion_inventory_contract")?,
        })
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u8,
    scope: Scope,
    plan: Content,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    kind: String,
    text: Option<String>,
    goal_revision: Option<String>,
    plan: Option<Content>,
}
pub(crate) fn remember(brain: &mut Brain, bytes: &str) -> Result<Value> {
    ensure(bytes.len() <= 10000, "Bounded remember input required")?;
    let input: Input = serde_json::from_str(bytes)?;
    if input.kind != "plan" {
        ensure(
            input.plan.is_none() && input.goal_revision.is_none(),
            "Plan fields belong only to kind=plan",
        )?;
        brain
            .journal
            .remember(&input.kind, &input.text.ok_or("Memory text required")?)?;
        return Ok(
            json!({"stored":true,"kind":input.kind,"authority":"source-labelled memory; not native proof"}),
        );
    }
    ensure(
        input.text.is_none(),
        "Use the structured plan, not text, for kind=plan",
    )?;
    let plan = input.plan.ok_or("Structured plan required")?;
    plan.validate()?;
    let expected = input
        .goal_revision
        .ok_or("Expected goal_revision required for a plan")?;
    let tx = brain.journal.db.unchecked_transaction()?;
    let scope = Scope::current(&brain.journal)?;
    ensure(
        scope.goal == brain.goal
            && scope.revision == expected
            && scope.completion_contract == completion::contract(&brain.cfg),
        "Goal or completion contract changed; inspect current context before revising its plan",
    )?;
    let stored = Stored {
        version: 1,
        scope,
        plan,
    };
    let bytes = serde_json::to_string(&stored)?;
    ensure(
        bytes.len() <= 8192,
        "Goal-scoped stored plan exceeds8192bytes",
    )?;
    brain.journal.set(KEY, &bytes)?;
    brain.journal.event(
        "active_goal_plan_saved",
        json!({"scope":stored.scope,
        "plan":stored.plan,"authority":"model_hypothesis","native_evidence":false}),
    )?;
    tx.commit()?;
    Ok(json!({"stored":true,"kind":"plan","goal_revision":expected,
        "authority":"model_hypothesis","native_evidence":false,"completion_credit":false}))
}
fn invalid(journal: &Journal, expected: &str, reason: &str) -> Result<Value> {
    // Keep append-only plan history. A stale/corrupt optional plan is not a
    // current plan, and never stops observation, conversation or ordinary work.
    let changed = journal.db.execute(
        "UPDATE meta SET value='' WHERE key=?1 AND value=?2",
        rusqlite::params![KEY, expected],
    )?;
    if changed == 0 {
        return Ok(
            json!({"active":false,"reason":"Plan changed during projection; replacement preserved",
            "authority":"model_hypothesis"}),
        );
    }
    journal.event(
        "active_goal_plan_invalidated",
        json!({"reason":reason,
        "historical_events_preserved":true,"native_actions_replayed":false}),
    )?;
    Ok(json!({"active":false,"reason":reason,"authority":"model_hypothesis"}))
}
pub(crate) fn view(journal: &Journal, goal: &str) -> Result<Value> {
    let Some(bytes) = journal.get(KEY)?.filter(|s| !s.is_empty()) else {
        return Ok(
            json!({"active":false,"reason":"No explicit plan for this goal contract",
            "authority":"model_hypothesis"}),
        );
    };
    let current = Scope::current(journal)?;
    // A historical replay frame cannot invalidate another current goal's plan.
    if current.goal != goal {
        return Ok(json!({"active":false,"reason":"Context goal is superseded"}));
    }
    if journal.goal_completed(goal)? {
        return invalid(journal, &bytes, "Goal completed; plan retained in history");
    }
    if bytes.len() > 8192 {
        return invalid(journal, &bytes, "Stored plan exceeds its bounded contract");
    }
    let stored = match serde_json::from_str::<Stored>(&bytes) {
        Ok(stored) if stored.version == 1 && stored.plan.validate().is_ok() => stored,
        _ => return invalid(journal, &bytes, "Stored plan is malformed or unsupported"),
    };
    if stored.scope != current {
        return invalid(
            journal,
            &bytes,
            "Goal text, revision or completion contract changed",
        );
    }
    let plan = serde_json::to_value(stored.plan)?;
    let plan_bytes = serde_json::to_vec(&plan)?;
    Ok(json!({"format":"active-goal-plan-v1","active":true,
        "goal_sha256":digest(current.goal.as_bytes()),"goal_revision":current.revision,
        "completion_contract":current.completion_contract,"plan":plan,
        "content_sha256":digest(&plan_bytes),"content_bytes":plan_bytes.len(),
        "authority":"model_hypothesis","native_evidence":false,"completion_credit":false,
        "scope":"Intent and resource/dependency hypotheses, not observed facts or permission. Verify referenced resources before use. Final inventory requirements apply to the success claim, not every intermediate step."}))
}
pub(crate) fn supply(journal: &Journal, context: &mut Value) {
    context["active_plan"] = match context["goal"].as_str() {
        Some(goal) => view(journal, goal).unwrap_or_else(|_| {
            json!({"active":false,
            "reason":"Optional plan storage unavailable","authority":"model_hypothesis"})
        }),
        None => json!({"active":false,"reason":"Current goal unavailable"}),
    };
}

pub(crate) fn fit_optional(context: &mut Value) -> Result<()> {
    if serde_json::to_vec(context)?.len() > 32768
        && context
            .as_object()
            .is_some_and(|v| v.contains_key("active_plan"))
    {
        // Do not truncate a dependency into a misleading partial plan. The
        // journal is untouched, so supply restores the full plan next turn.
        context["active_plan"] = json!({"active":false,"omitted_for_context_budget":true,
            "persistent_plan_unchanged":true,"authority":"model_hypothesis"});
        if serde_json::to_vec(context)?.len() > 32768 {
            context.as_object_mut().unwrap().remove("active_plan");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Brain) {
        fixture_at("http://127.0.0.1:1/")
    }
    fn fixture_at(endpoint: &str) -> (tempfile::TempDir, Brain) {
        let (root, mut brain) = crate::admission_tests::fixture(endpoint);
        brain
            .cfg
            .completion_inventory
            .insert("fixture:result".into(), 1);
        brain
            .journal
            .select_goal(&brain.goal, "fixture", json!({}), None)
            .unwrap();
        completion::bind(&brain.journal, &brain.cfg, &brain.goal, &brain.observed).unwrap();
        (root, brain)
    }
    fn input(brain: &Brain) -> Value {
        json!({"kind":"plan","goal_revision":brain.journal.get("goal_revision").unwrap(),
            "plan":{"next_step":"Use the observed processing resource for the remaining result",
            "dependencies":["Keep inputs for the remaining result","Return reusable resources before final success"],
            "resources":["Previously interacted resource; recheck its current state"]}})
    }
    #[tokio::test]
    async fn active_plan_survives_real_remember_reads_notes_noise_fitting_and_restart() {
        let (_root, mut brain) = fixture();
        let mut choice = crate::model::Choice {
            tool: "remember".into(),
            input: input(&brain).to_string(),
            path: String::new(),
            summary: "Retain working plan".into(),
            notes: String::new(),
            experiment: false,
        };
        crate::tools::execute(&mut brain, &choice).await.unwrap();
        let expected = view(&brain.journal, &brain.goal).unwrap();
        for n in 0..20 {
            brain
                .journal
                .set("working_notes", &format!("Unrelated inspection{n}"))
                .unwrap();
            choice.tool = "remember".into();
            choice.input =
                json!({"kind":"fact","text":format!("Unrelated persistent fact{n}")}).to_string();
            crate::tools::execute(&mut brain, &choice).await.unwrap();
            choice.tool = "read".into();
            choice.input =
                json!({"path":"sdk://reference","start_line":1,"end_line":3}).to_string();
            crate::tools::execute(&mut brain, &choice).await.unwrap();
        }
        let mut context = json!({"goal":brain.goal,"memory":vec![json!({"text":"noise".repeat(4000)});20],
            "recent":vec![json!({"text":"noise".repeat(4000)});20],"working_notes":"last inspection"});
        supply(&brain.journal, &mut context);
        crate::fit_context(&mut context).unwrap();
        assert_eq!(context["active_plan"], expected);
        assert!(serde_json::to_vec(&context).unwrap().len() <= 32768);
        let reopened = Journal::open(&brain.cfg.state.join("brain.sqlite")).unwrap();
        assert_eq!(view(&reopened, &brain.goal).unwrap(), expected);
    }
    #[tokio::test]
    async fn active_plan_rejects_malformed_oversized_and_stale_updates_without_losing_current_plan()
    {
        let (_root, mut brain) = fixture();
        let request = input(&brain).to_string();
        remember(&mut brain, &request).unwrap();
        let expected = view(&brain.journal, &brain.goal).unwrap();
        let base = input(&brain);
        for change in [
            json!({"next_step":"x","dependencies":[],"resources":[],"native_evidence":true}),
            json!({"next_step":"x".repeat(2049),"dependencies":[],"resources":[]}),
            json!({"next_step":"","dependencies":[],"resources":[]}),
            json!({"next_step":"x","dependencies":"wrong","resources":[]}),
        ] {
            let mut bad = base.clone();
            bad["plan"] = change;
            assert!(remember(&mut brain, &bad.to_string()).is_err());
            assert_eq!(view(&brain.journal, &brain.goal).unwrap(), expected);
        }
        let mut bad = base;
        bad["goal_revision"] = json!("superseded");
        assert!(remember(&mut brain, &bad.to_string()).is_err());
        assert_eq!(view(&brain.journal, &brain.goal).unwrap(), expected);
    }
    #[test]
    fn active_plan_invalidates_actual_goal_revision_text_and_completion_contract_not_history() {
        for change in ["revision", "text", "completion"] {
            let (_root, mut brain) = fixture();
            let request = input(&brain).to_string();
            remember(&mut brain, &request).unwrap();
            match change {
                "revision" => {
                    brain
                        .journal
                        .select_goal(&brain.goal, "new_request", json!({}), None)
                        .unwrap();
                }
                "text" => brain.journal.set("goal", "different actual goal").unwrap(),
                _ => {
                    brain
                        .cfg
                        .completion_inventory
                        .insert("fixture:result".into(), 2);
                    completion::bind(&brain.journal, &brain.cfg, &brain.goal, &brain.observed)
                        .unwrap();
                }
            }
            let current = brain.journal.get("goal").unwrap().unwrap();
            assert_eq!(
                view(&brain.journal, &current).unwrap()["active"],
                false,
                "{change}"
            );
            assert_eq!(brain.journal.get(KEY).unwrap().as_deref(), Some(""));
            let count: i64 = brain
                .journal
                .db
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE kind='active_goal_plan_saved'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "historical plan was not erased");
        }
    }
    #[tokio::test]
    async fn active_plan_never_satisfies_terminal_inventory_or_grants_native_authority() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_root, mut brain) = fixture_at(&format!("http://{}/", listener.local_addr().unwrap()));
        let request = input(&brain).to_string();
        remember(&mut brain, &request).unwrap();
        let view = view(&brain.journal, &brain.goal).unwrap();
        assert_eq!(view["native_evidence"], false);
        assert_eq!(view["completion_credit"], false);
        assert!(completion::verify(&brain.cfg, &brain.goal, &brain.observed).is_err());
        let claim = crate::model::Choice {
            tool: "complete".into(),
            input: json!({"status":"succeeded","summary":"The plan says to finish"}).to_string(),
            path: String::new(),
            summary: "Claim completion".into(),
            notes: String::new(),
            experiment: false,
        };
        let response = serde_json::to_string(&brain.observed).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 16384);
                header.push(socket.read_u8().await.unwrap());
            }
            assert!(
                String::from_utf8(header)
                    .unwrap()
                    .starts_with("GET /v1/bots/own/observations ")
            );
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        });
        let error = crate::tools::execute(&mut brain, &claim).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("retained inventory requirements not met")
        );
        server.await.unwrap();
        assert!(!brain.journal.goal_completed(&brain.goal).unwrap());
        assert_eq!(
            completion::view(&brain.cfg, &brain.goal)["evaluation_phase"],
            "final_success_claim_only"
        );
    }
    #[test]
    fn corrupt_optional_plan_becomes_unavailable_without_breaking_goal_or_thinking() {
        for value in ["{\"unrecognized\":true}".to_owned(), "x".repeat(8193)] {
            let (_root, mut brain) = fixture();
            let request = input(&brain).to_string();
            remember(&mut brain, &request).unwrap();
            brain.journal.set(KEY, &value).unwrap();
            let mut context = json!({"goal":brain.goal});
            supply(&brain.journal, &mut context);
            assert_eq!(context["active_plan"]["active"], false);
            assert_eq!(
                brain.journal.get("goal").unwrap().as_deref(),
                Some(brain.goal.as_str())
            );
            assert!(!brain.journal.goal_completed(&brain.goal).unwrap());
            crate::fit_context(&mut context).unwrap();
        }
    }
    #[test]
    fn superseded_projection_cannot_clear_a_replacement_plan_from_another_connection() {
        let (_root, mut brain) = fixture();
        let original = input(&brain).to_string();
        remember(&mut brain, &original).unwrap();
        let reader = Journal::open(&brain.cfg.state.join("brain.sqlite")).unwrap();
        let stale = reader.get(KEY).unwrap().unwrap();
        // Deterministic interleaving of a stale reader and a new goal/plan writer.
        brain
            .journal
            .select_goal(&brain.goal, "new_same_text_request", json!({}), None)
            .unwrap();
        let mut replacement = input(&brain);
        replacement["plan"]["next_step"] = json!("Replacement dependency step");
        remember(&mut brain, &replacement.to_string()).unwrap();
        let expected = view(&brain.journal, &brain.goal).unwrap();
        invalid(&reader, &stale, "Old projection found a superseded scope").unwrap();
        assert_eq!(view(&reader, &brain.goal).unwrap(), expected);
        assert_eq!(expected["active"], true);
    }

    #[test]
    fn optional_max_plan_cannot_break_near_cap_native_frame_and_returns_next_turn() {
        let (_root, mut brain) = fixture();
        let mut request = input(&brain);
        request["plan"] = json!({"next_step":"n".repeat(768),
            "dependencies":vec!["d".repeat(256);3],
            "resources":["r".repeat(256),""]});
        let remaining = CONTENT_BYTES - serde_json::to_vec(&request["plan"]).unwrap().len();
        assert!(remaining > 0 && remaining <= 256);
        request["plan"]["resources"][1] = json!("s".repeat(remaining));
        assert_eq!(
            serde_json::to_vec(&request["plan"]).unwrap().len(),
            CONTENT_BYTES
        );
        remember(&mut brain, &request.to_string()).unwrap();
        let expected = view(&brain.journal, &brain.goal).unwrap();
        let saved = brain.journal.get(KEY).unwrap();
        for gap in [512, 1] {
            let mut frame = json!({"goal":brain.goal,"completion_requirements":completion::view(&brain.cfg,&brain.goal),
                "mandatory_fixture":""});
            crate::fit_context(&mut frame).unwrap();
            let pad = 32768 - gap - serde_json::to_vec(&frame).unwrap().len();
            let mandatory = "m".repeat(pad);
            frame["mandatory_fixture"] = json!(mandatory);
            crate::fit_context(&mut frame).unwrap();
            let before = frame.clone();
            supply(&brain.journal, &mut frame);
            crate::fit_context(&mut frame).unwrap();
            assert!(serde_json::to_vec(&frame).unwrap().len() <= 32768);
            assert_ne!(frame["active_plan"]["active"], true);
            assert!(frame["active_plan"]["plan"].is_null());
            if gap == 512 {
                assert_eq!(frame["active_plan"]["omitted_for_context_budget"], true);
            } else {
                assert!(frame.get("active_plan").is_none());
            }
            frame.as_object_mut().unwrap().remove("active_plan");
            assert_eq!(
                frame, before,
                "Mandatory facts must not be changed to fit optional intent"
            );
            assert_eq!(brain.journal.get(KEY).unwrap(), saved);
        }
        let mut smaller = json!({"goal":brain.goal});
        supply(&brain.journal, &mut smaller);
        crate::fit_context(&mut smaller).unwrap();
        assert_eq!(smaller["active_plan"], expected);
        let mut oversized = json!({"mandatory_fixture":"m".repeat(32769)});
        supply(&brain.journal, &mut oversized);
        assert!(crate::fit_context(&mut oversized).is_err());
    }
}
