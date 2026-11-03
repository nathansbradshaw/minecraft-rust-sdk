//! Persistent model-role scheduling. Provider admission limits remain external.
use crate::{Journal, Result};
use rusqlite::{OptionalExtension, params};

pub(crate) const DEVELOPMENT_TURNS: u32 = 4;

impl Journal {
    pub(crate) fn request_development(&self) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for (key, value) in [
            ("develop_next", "true".to_owned()),
            ("development_turns_remaining", DEVELOPMENT_TURNS.to_string()),
        ] {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Consume before inference, so a crash or restart cannot renew a session.
    /// Legacy sticky requests receive one finite session, never a fresh one
    /// whenever their old boolean gets set again by an investigation.
    pub(crate) fn take_development_turn(&self) -> Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let get = |key: &str| -> Result<Option<String>> {
            Ok(tx
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()?)
        };
        let requested = get("develop_next")?.as_deref() == Some("true");
        let remaining = match get("development_turns_remaining")? {
            Some(value) => value.parse::<u32>().unwrap_or(0).min(DEVELOPMENT_TURNS),
            None if requested => DEVELOPMENT_TURNS,
            None => 0,
        };
        let develop = requested && remaining > 0;
        let next = if develop { remaining - 1 } else { 0 };
        for (key, value) in [
            ("develop_next", (next > 0).to_string()),
            ("development_turns_remaining", next.to_string()),
        ] {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        tx.commit()?;
        Ok(develop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_development_expires_across_restarts_and_boolean_reassertion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roles.sqlite");
        let j = Journal::open(&path).unwrap();
        j.set("develop_next", "true").unwrap();
        assert!(j.take_development_turn().unwrap());
        drop(j);
        let j = Journal::open(&path).unwrap();
        for _ in 1..DEVELOPMENT_TURNS {
            assert!(j.take_development_turn().unwrap());
        }
        for _ in 0..8 {
            // Existing investigation state cannot silently replenish Sol.
            j.set("develop_next", "true").unwrap();
            assert!(!j.take_development_turn().unwrap());
        }
        j.request_development().unwrap();
        assert!(j.take_development_turn().unwrap());
    }

    #[test]
    fn routine_goal_handoff_and_corrupt_counts_do_not_escalate_or_change_allowances() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("roles.sqlite")).unwrap();
        j.set("model_admissions", "225").unwrap();
        assert!(!j.take_development_turn().unwrap());
        j.request_development().unwrap();
        j.set("develop_next", "false").unwrap();
        assert!(!j.take_development_turn().unwrap());
        j.set("develop_next", "true").unwrap();
        j.set("development_turns_remaining", "bad").unwrap();
        assert!(!j.take_development_turn().unwrap());
        assert_eq!(j.get("model_admissions").unwrap().as_deref(), Some("225"));
    }

    #[test]
    fn replacing_the_goal_returns_to_routine_without_replenishing_an_old_session() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("roles.sqlite")).unwrap();
        j.select_goal("craft", "fixture", serde_json::json!({}), None)
            .unwrap();
        j.request_development().unwrap();
        assert!(j.take_development_turn().unwrap());
        j.select_goal("explore", "fixture", serde_json::json!({}), None)
            .unwrap();
        assert!(!j.take_development_turn().unwrap());
        j.request_development().unwrap();
        assert!(j.take_development_turn().unwrap());
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("3")
        );
    }
}
