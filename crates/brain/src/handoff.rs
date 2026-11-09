//! An exhausted diagnosis fences its failed intention, not all useful work.
use crate::{Journal, Result, digest, progress};
use minecraft_sdk::{Action, Observation};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) struct ExhaustedIntention(String);
impl std::fmt::Display for ExhaustedIntention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unchanged exhausted SDK intention was not dispatched; prior receipt {}. Choose different useful work or inspect genuinely changed preconditions. Uncertain actions still require reconciliation.",
            self.0
        )
    }
}
impl std::error::Error for ExhaustedIntention {}

pub(crate) fn basis(observed: &Observation) -> Value {
    json!({"environment":progress::environment_signature(observed),
        "actor":observed.player["uuid"].as_str(),"life":observed.player["entity_id"].as_i64(),
        "clock_epoch":observed.world["sample_clock"]["epoch"].as_str()})
}

impl Journal {
    fn intention_fence(&self, action: &Action, observed: &Observation) -> Result<String> {
        // Exact typed parameters matter here: hit coordinates can select slab
        // halves or mod controls. Cost-scope jitter normalization cannot safely
        // decide which distinct gameplay requests are interchangeable.
        Ok(format!("exhausted-intention:{}",digest(json!({
            "action":action,"basis":basis(observed),
            "goal_revision":self.get("goal_revision")?,"contract":self.get("context_contract")?
        }).to_string().as_bytes())))
    }

    pub(crate) fn check_exhausted_intention(
        &self,
        action: &Action,
        observed: &Observation,
    ) -> Result<()> {
        if matches!(action, Action::Chat { .. } | Action::Look { .. }) {
            return Ok(()); // Normal chat limits and uncertainty handling still apply.
        }
        if let Some(id) = self.get(&self.intention_fence(action, observed)?)? {
            return Err(Box::new(ExhaustedIntention(id)));
        }
        Ok(())
    }

    /// Some(true): one new bounded alternative window; Some(false): the same
    /// window still has inspection capacity. None: no reset/retry allowance.
    pub(crate) fn alternative_window(
        &self,
        subject: &str,
        revision: &str,
        goal: &str,
        observed: &Observation,
        inspected: u32,
        ceiling: u32,
    ) -> Result<Option<bool>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='goal_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref().unwrap_or_default() != revision {
            return Ok(None);
        }
        let key = format!("alternative-window:{subject}");
        let exists = tx
            .query_row("SELECT 1 FROM meta WHERE key=?1", [&key], |_| Ok(()))
            .optional()?
            .is_some();
        if exists && inspected >= ceiling {
            return Ok(None);
        }
        if !exists {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,'used')", [&key])?;
            let failure = self.gameplay_failure_context(revision)?;
            if matches!(
                failure["outcome"].as_str(),
                Some("failed" | "expired" | "rejected")
            ) && failure["basis"] == basis(observed)
                && let Ok(action) = serde_json::from_value::<Action>(failure["action"].clone())
                && !matches!(action, Action::Chat { .. } | Action::Look { .. })
            {
                let fence = self.intention_fence(&action, observed)?;
                if let Some(id) = failure["action_id"].as_str() {
                    tx.execute(
                        "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
                        params![fence, id],
                    )?;
                }
            }
            let context = json!({"goal_revision":revision,"exhausted_subject":subject,
                "inspection_ceiling":ceiling,"task":"The previous diagnosis is exhausted. Its unchanged failed SDK intention is fenced locally. Choose a different useful action, changed executable experiment, code repair, or goal. Do not spend this window repeating the same diagnosis. Provider/run budgets are unchanged; inspection-only loops remain bounded.",
                "authority":"Client scheduling feedback, not native effect proof or gameplay permission"});
            for (key, value) in [
                ("inspection_streak", "0".to_owned()),
                ("inspection_goal", goal.to_owned()),
                ("develop_next", "false".to_owned()),
                ("development_turns_remaining", "0".to_owned()),
                ("alternative_context", context.to_string()),
            ] {
                tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
            }
        }
        for key in ["needs_investigation", "investigation_remaining"] {
            let value = if key == "needs_investigation" {
                "false"
            } else {
                "0"
            };
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        tx.commit()?;
        if !exists {
            self.event("investigation_alternatives",json!({"subject":subject,"goal_revision":revision,
                "inspection_ceiling":ceiling,"diagnosis_refilled":false,"provider_allowances_changed":false}))?;
        }
        Ok(Some(!exists))
    }
    pub(crate) fn alternative_context(&self, revision: &str) -> Result<Value> {
        let value: Value = self
            .get("alternative_context")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(if value["goal_revision"] == revision {
            value
        } else {
            Value::Null
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhaustion_hands_off_once_fences_exact_mutation_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let j = Journal::open(&path).unwrap();
        let rev = j
            .select_goal("build", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.set("model_admissions", "99").unwrap();
        j.set("sol_daily_reset", "200").unwrap();
        let mut o = Observation {
            revision: "sample".into(),
            player: json!({"uuid":"own","entity_id":3,"position":[0.5,1.,0.5],"selected":0}),
            world: json!({"game_time":1,"sample_clock":{"epoch":"clock","nanos":"1"}}),
            menu: json!({}),
        };
        let a:Action=serde_json::from_value(json!({"kind":"place","block":{"x":0,"y":0,"z":0},"face":1,"hand":"main","hit":[0.5,1.,0.5]})).unwrap();
        let receipt = minecraft_sdk::Receipt {
            action_id: "failed-id".into(),
            outcome: minecraft_sdk::Outcome::Expired,
            adapter_version: "fixture".into(),
            evidence: json!({}),
            error: None,
        };
        j.note_gameplay_failure(&a, &receipt, &o).unwrap();
        j.investigation_budget("subject", 1, &rev).unwrap();
        j.investigation_spent("subject", false).unwrap();
        assert_eq!(
            j.alternative_window("subject", &rev, "build", &o, 8, 8)
                .unwrap(),
            Some(true)
        );
        assert!(
            j.check_exhausted_intention(&a, &o)
                .unwrap_err()
                .downcast_ref::<ExhaustedIntention>()
                .is_some()
        );
        assert_eq!(
            j.investigation_budget("subject", 1, &rev).unwrap(),
            Some((progress::Investigation::Exhausted, 0))
        );
        assert_eq!(
            j.alternative_window("subject", &rev, "build", &o, 2, 8)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            j.alternative_window("subject", &rev, "build", &o, 8, 8)
                .unwrap(),
            None
        );
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.alternative_window("subject", &rev, "build", &o, 8, 8)
                .unwrap(),
            None
        );
        o.world["game_time"] = json!(1000);
        o.world["sample_clock"]["nanos"] = json!("1000");
        assert!(j.check_exhausted_intention(&a, &o).is_err());
        let mut distinct = serde_json::to_value(&a).unwrap();
        distinct["hit"] = json!([0.5, 0.25, 0.5]);
        assert!(
            j.check_exhausted_intention(&serde_json::from_value(distinct).unwrap(), &o)
                .is_ok(),
            "meaningful exact hit changes remain available without renewing diagnosis"
        );
        o.player["selected"] = json!(7);
        assert!(j.check_exhausted_intention(&a, &o).is_ok());
        o.player["selected"] = json!(0);
        o.player["entity_id"] = json!(4);
        assert!(
            j.check_exhausted_intention(&a, &o).is_ok(),
            "a new native life cannot inherit a stale failure fence"
        );
        o.player["entity_id"] = json!(3);
        o.world["sample_clock"]["epoch"] = json!("new-native-clock");
        assert!(j.check_exhausted_intention(&a, &o).is_ok());
        assert_eq!(j.get("model_admissions").unwrap().as_deref(), Some("99"));
        assert_eq!(j.get("sol_daily_reset").unwrap().as_deref(), Some("200"));
        let next = j
            .select_goal("other", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        assert!(j.alternative_context(&next).unwrap().is_null());
        assert_eq!(
            j.alternative_window("stale", &rev, "build", &o, 0, 8)
                .unwrap(),
            None
        );
    }
    #[test]
    fn historical_failure_in_different_facts_never_fences_a_current_request() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        let rev = j
            .select_goal("build", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        let mut o = Observation {
            revision: "sample".into(),
            player: json!({"uuid":"own","entity_id":3,"position":[0.5,1.,0.5],"selected":0}),
            world: json!({}),
            menu: json!({}),
        };
        let a:Action=serde_json::from_value(json!({"kind":"place","block":{"x":0,"y":0,"z":0},"face":1,"hand":"main","hit":[0.5,1.,0.5]})).unwrap();
        let receipt = minecraft_sdk::Receipt {
            action_id: "old-failure".into(),
            outcome: minecraft_sdk::Outcome::Expired,
            adapter_version: "fixture".into(),
            evidence: json!({}),
            error: None,
        };
        j.note_gameplay_failure(&a, &receipt, &o).unwrap();
        o.player["selected"] = json!(7);
        assert_eq!(
            j.alternative_window("subject", &rev, "build", &o, 8, 8)
                .unwrap(),
            Some(true)
        );
        assert!(
            j.check_exhausted_intention(&a, &o).is_ok(),
            "do not relabel old facts as a failed current attempt"
        );
        assert!(
            j.check_exhausted_intention(
                &Action::Chat {
                    message: "hello".into()
                },
                &o
            )
            .is_ok()
        );
    }
}
