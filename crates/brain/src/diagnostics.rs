//! Bounded delivery of previously unseen read-only tool failures.
use crate::{Journal, Result, digest};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS diagnostic_feedback(epoch TEXT PRIMARY KEY,failure_event INTEGER NOT NULL)")?;
    Ok(())
}

impl Journal {
    fn diagnostic_witness(&self, sequence: i64, revision: &str) -> Result<Value> {
        let data: String = self.db.query_row(
            "SELECT data FROM events WHERE id=?1 AND kind='tool_result'",
            [sequence],
            |r| r.get(0),
        )?;
        let data: Value = serde_json::from_str(&data)?;
        let choice: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM events WHERE kind='choice' AND id<?1 ORDER BY id DESC LIMIT 1",
                [sequence],
                |r| r.get(0),
            )
            .optional()?;
        let choice: Value = choice
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or(Value::Null);
        let input = choice["choice"]["input"].as_str().unwrap_or_default();
        let error = data["error"].as_str().unwrap_or_default();
        Ok(json!({"failure_event":sequence,"goal_revision":revision,
            "experiment_signature":self.get("experiment_signature")?,
            "request":{"tool":data["tool"],"input":input.chars().take(512).collect::<String>()},
            "request_input_complete":input.chars().count()<=512,
            "error":error.chars().take(512).collect::<String>(),"error_complete":error.chars().count()<=512,
            "scope":"Historical client diagnostic for the current experiment; not a current world fact or task success"}))
    }
    pub(crate) fn diagnostic_context(&self, revision: &str) -> Result<Value> {
        let mut witness: Option<Value> = self
            .get("active_diagnostic_feedback")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        if witness.is_none() {
            // Upgrade an existing notice only when its complete epoch matches
            // the unchanged recorded experiment/task and its original contract.
            let notice: Option<(String,i64)> = self.db.query_row("SELECT epoch,failure_event FROM diagnostic_feedback ORDER BY rowid DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((epoch, sequence)) = notice {
                let experiment: Value = self
                    .get("last_experiment")?
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let contract: Option<String> = self.db.query_row("SELECT data FROM events WHERE kind='context_contract_changed' AND id<?1 ORDER BY id DESC LIMIT 1", [sequence], |r|r.get(0)).optional()?;
                if let (Some(source), Some(contract)) =
                    (experiment["source_sha256"].as_str(), contract)
                {
                    let contract: Value = serde_json::from_str(&contract)?;
                    let old_contract = contract["contract_sha256"].as_str();
                    let expected = digest(json!({"source":source,"goal_revision":revision,"experiment":self.get("experiment_signature")?,"contract":old_contract}).to_string().as_bytes());
                    if epoch == expected
                        && !revision
                            .parse::<i64>()
                            .is_ok_and(|selected| sequence <= selected)
                    {
                        let restored = self.diagnostic_witness(sequence, revision)?;
                        self.set("active_diagnostic_feedback", &restored.to_string())?;
                        witness = Some(restored);
                    }
                }
            }
        }
        let experiment = json!(self.get("experiment_signature")?);
        Ok(witness
            .filter(|value| {
                value["goal_revision"] == revision && value["experiment_signature"] == experiment
            })
            .unwrap_or(Value::Null))
    }
    pub(crate) fn admit_diagnostic_feedback(&self, source: &str, revision: &str) -> Result<bool> {
        if self.get("needs_investigation")?.as_deref() != Some("true")
            || self.get("investigation_remaining")?.as_deref() != Some("0")
        {
            return Ok(false);
        }
        let Some((sequence, data)) = self
            .db
            .query_row(
                "SELECT id,data FROM events WHERE kind='tool_result' ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        let data: Value = serde_json::from_str(&data)?;
        if !matches!(
            data["tool"].as_str(),
            Some("query" | "observe" | "status" | "read" | "list" | "search")
        ) || !data["error"].is_string()
            || data
                .get("goal_revision")
                .is_some_and(|r| r.as_str() != Some(revision))
        {
            return Ok(false);
        }
        if revision
            .parse::<i64>()
            .is_ok_and(|selected| sequence <= selected)
        {
            return Ok(false);
        }
        let choice: Option<i64> =
            self.db
                .query_row("SELECT max(id) FROM events WHERE kind='choice'", [], |r| {
                    r.get(0)
                })?;
        if choice.is_some_and(|seen| seen > sequence) {
            return Ok(false);
        }
        // Position changes, error wording, different read tools and restart
        // cannot replenish this allowance. A new source/experiment/task can.
        let epoch = digest(json!({"source":source,"goal_revision":revision,
            "experiment":self.get("experiment_signature")?,"contract":self.get("context_contract")?}).to_string().as_bytes());
        let witness = self.diagnostic_witness(sequence, revision)?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='goal_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref().unwrap_or_default() != revision {
            return Ok(false);
        }
        if tx.execute(
            "INSERT OR IGNORE INTO diagnostic_feedback(epoch,failure_event) VALUES(?1,?2)",
            params![epoch, sequence],
        )? == 0
        {
            return Ok(false);
        }
        let notice = digest(
            json!({"epoch":epoch,"failure_event":sequence})
                .to_string()
                .as_bytes(),
        );
        tx.execute("INSERT INTO meta(key,value) VALUES('diagnostic_feedback_revision',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[&notice])?;
        tx.execute("INSERT INTO meta(key,value) VALUES('active_diagnostic_feedback',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[witness.to_string()])?;
        tx.commit()?;
        self.event("diagnostic_feedback_admitted",json!({"failure_event":sequence,"tool":data["tool"],"revision":notice,
            "opportunities_per_experiment":1,"provider_allowances_changed":false,"gameplay_replayed":false,
            "scope":"New client diagnostic feedback, not a game fact or task success"}))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_failure_survives_inspections_and_legacy_contract_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let j = Journal::open(&path).unwrap();
        j.context_contract("old-contract").unwrap();
        let rev = j
            .select_goal("wood", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.set("experiment_signature", "failed-program").unwrap();
        j.set(
            "last_experiment",
            &json!({"source_sha256":"source"}).to_string(),
        )
        .unwrap();
        stalled(&j);
        j.event(
            "choice",
            json!({"choice":{"tool":"query","input":"movement options"}}),
        )
        .unwrap();
        j.event(
            "tool_result",
            json!({"tool":"query","error":"Response too large","goal_revision":rev}),
        )
        .unwrap();
        assert!(j.admit_diagnostic_feedback("source", &rev).unwrap());
        j.event(
            "tool_result",
            json!({"tool":"read","result":{"content":"an unrelated inspection"}}),
        )
        .unwrap();
        let context = j.diagnostic_context(&rev).unwrap();
        assert_eq!(context["error"], "Response too large");
        assert_eq!(context["request"]["input"], "movement options");
        // Simulate the prior ledger, which recorded the notice/event but did
        // not yet retain its context. Upgrade cannot add another allowance.
        j.db.execute(
            "DELETE FROM meta WHERE key='active_diagnostic_feedback'",
            [],
        )
        .unwrap();
        j.context_contract("new-contract").unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(j.diagnostic_context(&rev).unwrap(), context);
        let notices: i64 =
            j.db.query_row("SELECT COUNT(*) FROM diagnostic_feedback", [], |r| r.get(0))
                .unwrap();
        assert_eq!(notices, 1);
        j.set("experiment_signature", "changed-experiment").unwrap();
        assert!(j.diagnostic_context(&rev).unwrap().is_null());
        assert!(j.diagnostic_context("different-goal").unwrap().is_null());
    }
    fn stalled(j: &Journal) {
        j.set("needs_investigation", "true").unwrap();
        j.set("investigation_remaining", "0").unwrap();
    }
    #[test]
    fn mutation_stale_goal_and_already_seen_errors_cannot_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("journal.sqlite")).unwrap();
        let rev = j
            .select_goal("wood", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        stalled(&j);
        j.event(
            "tool_result",
            json!({"tool":"sdk","error":"uncertain mutation","goal_revision":rev}),
        )
        .unwrap();
        assert!(!j.admit_diagnostic_feedback("source", &rev).unwrap());
        j.event(
            "tool_result",
            json!({"tool":"query","error":"failed","goal_revision":"old-goal"}),
        )
        .unwrap();
        assert!(!j.admit_diagnostic_feedback("source", &rev).unwrap());
        j.event(
            "tool_result",
            json!({"tool":"query","error":"failed","goal_revision":rev}),
        )
        .unwrap();
        j.event("choice", json!({"tool":"read"})).unwrap();
        assert!(!j.admit_diagnostic_feedback("source", &rev).unwrap());
        j.event(
            "tool_result",
            json!({"tool":"query","error":"failed","goal_revision":rev}),
        )
        .unwrap();
        j.set("investigation_remaining", "1").unwrap();
        assert!(!j.admit_diagnostic_feedback("source", &rev).unwrap());
        stalled(&j);
        assert!(!j.admit_diagnostic_feedback("source", "stale").unwrap());
        assert!(j.admit_diagnostic_feedback("source", &rev).unwrap());
        assert!(!j.admit_diagnostic_feedback("source", &rev).unwrap());
        j.set("experiment_signature", "a changed bounded experiment")
            .unwrap();
        assert!(j.admit_diagnostic_feedback("source", &rev).unwrap());
        assert_eq!(
            j.get("goal_revision").unwrap().as_deref(),
            Some(rev.as_str())
        );
    }
    #[test]
    fn legacy_configured_goal_failure_is_delivered_once_without_resetting_allowances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let j = Journal::open(&path).unwrap();
        stalled(&j);
        j.set("sol_daily_reset", "2000000000").unwrap();
        j.event(
            "tool_result",
            json!({"tool":"query","error":"Capability response exceeds the model view budget"}),
        )
        .unwrap();
        assert!(j.admit_diagnostic_feedback("source", "").unwrap());
        assert_eq!(
            j.get("sol_daily_reset").unwrap().as_deref(),
            Some("2000000000")
        );
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert!(!j.admit_diagnostic_feedback("source", "").unwrap());
        let new = j
            .select_goal("different task", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        stalled(&j);
        assert!(
            !j.admit_diagnostic_feedback("source", &new).unwrap(),
            "Legacy feedback predates the new task"
        );
    }
}
