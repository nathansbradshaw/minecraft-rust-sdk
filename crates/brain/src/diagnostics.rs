//! Bounded delivery of previously unseen read-only tool failures.
use crate::{Journal, Result, digest};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS diagnostic_feedback(epoch TEXT PRIMARY KEY,failure_event INTEGER NOT NULL)")?;
    Ok(())
}

impl Journal {
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
