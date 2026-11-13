//! Existing engine work only; this module creates no compiler load or new model allocation.
use minecraft_sdk_brain::{Brain, Result};
use serde::Serialize;
use serde_json::json;
use std::{
    future::Future,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Blocked,
    Compile,
    Exposed,
    Navigate,
    Shore,
    Task,
    TaskExposed,
}
impl Mode {
    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "blocked" => Self::Blocked,
            "compile" => Self::Compile,
            "exposed" => Self::Exposed,
            "navigate" => Self::Navigate,
            "shore" => Self::Shore,
            "task" => Self::Task,
            "task-exposed" => Self::TaskExposed,
            _ => return None,
        })
    }
    pub(super) fn accepts_arity(self, n: usize) -> bool {
        if matches!(self, Self::Navigate | Self::Shore) {
            n == 4
        } else {
            matches!(n, 3 | 4)
        }
    }
    pub(super) fn is_task(self) -> bool {
        matches!(self, Self::Task | Self::TaskExposed)
    }
    pub(super) fn body_enabled(self) -> bool {
        !matches!(self, Self::Exposed | Self::TaskExposed)
    }
    pub(super) fn models_called(self) -> Option<u64> {
        if self.is_task() { None } else { Some(0) }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Outcome {
    pub(super) workload_completed: bool,
    pub(super) timed_out: bool,
    error: Option<String>,
    utc_before_ms: u128,
    utc_after_ms: u128,
    elapsed_ms: u128,
    budget_ms: u128,
    goal_success_verified: bool,
    models_called: Option<u64>,
    scope: &'static str,
}
async fn settle(future: impl Future<Output = Result<()>>, budget: Duration) -> Result<Outcome> {
    let utc_before_ms = super::probe_continuity::utc_ms()?;
    let started = Instant::now();
    let (workload_completed, timed_out, error) = match tokio::time::timeout(budget, future).await {
        Ok(Ok(())) => (true, false, None),
        Ok(Err(error)) => (false, false, Some(error.to_string())),
        Err(_) => (
            false,
            true,
            Some("Operator task window expired; caller future was cancelled".into()),
        ),
    };
    Ok(Outcome {
        workload_completed,
        timed_out,
        error,
        utc_before_ms,
        utc_after_ms: super::probe_continuity::utc_ms()?,
        elapsed_ms: started.elapsed().as_millis(),
        budget_ms: budget.as_millis(),
        goal_success_verified: false,
        models_called: None,
        scope: "Completion means the existing Brain::run future returned successfully, not native goal success. Model/provider usage and compiler overlap require original journals and external accounting. Timeout does not prove remote provider or native work stopped; never replay pending actions.",
    })
}
pub(super) async fn run(brain: &mut Brain, budget: Duration) -> Result<Outcome> {
    brain.journal.event("body_probe_task_started", json!({"utc_ms":super::probe_continuity::utc_ms()?,
        "budget_ms":budget.as_millis(),"goal_sha256":minecraft_sdk_brain::digest(brain.goal.as_bytes()),
        "models_called":null,"model_accounting":"Original brain journal and provider ledger; no quota changed",
        "work":"Existing Brain::run with the operator-selected goal and workspace; no synthetic workload"}))?;
    let outcome = settle(brain.run(), budget).await?;
    brain
        .journal
        .event("body_probe_task_finished", serde_json::to_value(&outcome)?)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_task_modes_have_truthful_model_labels_and_exposed_body_selection() {
        for name in ["task", "task-exposed"] {
            let mode = Mode::parse(name).unwrap();
            assert!(mode.is_task());
            assert_eq!(mode.models_called(), None);
            assert_eq!(mode.body_enabled(), name == "task");
            assert!(mode.accepts_arity(3) && mode.accepts_arity(4));
            assert!(!mode.accepts_arity(5));
        }
        assert_eq!(Mode::parse("blocked").unwrap().models_called(), Some(0));
        assert!(!Mode::parse("navigate").unwrap().accepts_arity(3));
        assert!(Mode::parse("unknown").is_none());
    }
    #[tokio::test]
    async fn completed_and_failed_engine_futures_are_not_native_goal_acceptance() {
        let completed = settle(async { Ok(()) }, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(completed.workload_completed && !completed.timed_out);
        assert!(!completed.goal_success_verified);
        assert_eq!(completed.models_called, None);
        let failed = settle(
            async { Err("fixture engine error".into()) },
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(!failed.workload_completed && !failed.timed_out);
        assert!(failed.error.unwrap().contains("fixture engine error"));
    }
    #[tokio::test]
    async fn bounded_task_timeout_is_retained_without_claiming_completion() {
        let outcome = settle(std::future::pending(), Duration::from_millis(1))
            .await
            .unwrap();
        assert!(outcome.timed_out && !outcome.workload_completed);
        assert!(outcome.utc_after_ms >= outcome.utc_before_ms);
        assert!(outcome.error.is_some());
    }
}
