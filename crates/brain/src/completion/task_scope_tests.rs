//! Production journal/task-lineage regressions; no model, SDK or service calls.
use super::*;
use crate::model::{Conversation, GoalAction};

fn fixture() -> (tempfile::TempDir, Journal, Config, Observation) {
    let (root, brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
    let mut cfg = brain.cfg.clone();
    cfg.goal = "Retain useful equipment".into();
    cfg.completion_inventory
        .insert("fixture:equipment".into(), 1);
    let observation = brain.observed.clone();
    drop(brain);
    let journal = Journal::open(&root.path().join("brain.sqlite")).unwrap();
    (root, journal, cfg, observation)
}

fn activate(journal: &Journal, cfg: &Config, observation: &Observation) {
    journal.activate_configured_goal(&cfg.goal, false).unwrap();
    bind(journal, cfg, &cfg.goal, observation).unwrap();
}

fn value(journal: &Journal, key: &str) -> String {
    journal.get(key).unwrap().unwrap()
}

fn conversation(journal: &Journal, action: GoalAction, goal: &str) -> bool {
    let reply = Conversation {
        message: String::new(),
        goal_action: action,
        goal: goal.into(),
        reason: "Synthetic explicit task relationship".into(),
    };
    journal.conversation_goal(
        &reply,
        &value(journal, "goal_revision"),
        &json!([{"kind":"player","sender_uuid":"fixture-player","message":"Synthetic request"}]),
    ).unwrap()
}

#[test]
fn refined_configured_task_keeps_requirements_across_a_real_journal_restart() {
    let (root, journal, cfg, mut observation) = fixture();
    activate(&journal, &cfg, &observation);
    let task = value(&journal, "task_revision");
    let original_revision = value(&journal, "goal_revision");
    let refined = "Continue the same equipment task with the latest observations";
    assert!(conversation(&journal, GoalAction::Refine, refined));
    let refined_revision = value(&journal, "goal_revision");
    assert_ne!(refined_revision, original_revision);
    assert_eq!(value(&journal, "task_revision"), task);
    assert_eq!(value(&journal, "completion_task_revision"), task);
    assert!(verify(&journal, &cfg, refined, &observation).is_err());
    drop(journal);

    let restarted = Journal::open(&root.path().join("brain.sqlite")).unwrap();
    restarted
        .activate_configured_goal(&cfg.goal, false)
        .unwrap();
    bind(&restarted, &cfg, refined, &observation).unwrap();
    assert_eq!(value(&restarted, "goal"), refined);
    assert_eq!(value(&restarted, "goal_revision"), refined_revision);
    assert_eq!(value(&restarted, "task_revision"), task);
    let requirements = view(&restarted, &cfg, refined).unwrap();
    assert_eq!(requirements["applies_to_current_goal"], true);
    assert_eq!(requirements["inventory_minimum"]["fixture:equipment"], 1);
    assert!(verify(&restarted, &cfg, refined, &observation).is_err());
    observation.player["inventory"] = json!([{"item":"fixture:equipment","count":1}]);
    let verified = verify(&restarted, &cfg, refined, &observation).unwrap();
    assert_eq!(verified["checked_inventory"][0]["observed"], 1);
    assert!(restarted.complete_goal(refined, &refined_revision).unwrap());
}

#[test]
fn explicit_same_text_replace_and_later_old_text_do_not_inherit_completion_binding() {
    let (root, journal, cfg, observation) = fixture();
    activate(&journal, &cfg, &observation);
    let original_task = value(&journal, "task_revision");
    assert!(conversation(&journal, GoalAction::Replace, &cfg.goal));
    let replacement = value(&journal, "task_revision");
    assert_ne!(
        replacement, original_task,
        "Explicit replace means a new task even with identical text"
    );
    assert_eq!(
        view(&journal, &cfg, &cfg.goal).unwrap()["applies_to_current_goal"],
        false
    );
    assert!(conversation(
        &journal,
        GoalAction::Replace,
        "Investigate a separate objective"
    ));
    assert!(conversation(&journal, GoalAction::Replace, &cfg.goal));
    let returning_task = value(&journal, "task_revision");
    assert_ne!(returning_task, replacement);
    assert_ne!(returning_task, original_task);
    drop(journal);

    let restarted = Journal::open(&root.path().join("brain.sqlite")).unwrap();
    restarted
        .activate_configured_goal(&cfg.goal, false)
        .unwrap();
    bind(&restarted, &cfg, &cfg.goal, &observation).unwrap();
    assert_eq!(value(&restarted, "task_revision"), returning_task);
    assert_eq!(value(&restarted, "completion_task_revision"), original_task);
    assert_eq!(
        view(&restarted, &cfg, &cfg.goal).unwrap()["applies_to_current_goal"],
        false
    );
    assert_eq!(
        verify(&restarted, &cfg, &cfg.goal, &observation).unwrap()["checked_inventory"],
        json!([])
    );
}

#[test]
fn concurrent_conversation_supersedes_the_view_without_weakening_completion_verification() {
    let (root, journal, cfg, observation) = fixture();
    activate(&journal, &cfg, &observation);
    let old_revision = value(&journal, "goal_revision");
    let writer = Journal::open(&root.path().join("brain.sqlite")).unwrap();
    let refined = "The same equipment objective with clarified wording";
    assert!(conversation(&writer, GoalAction::Refine, refined));
    let stale = view(&journal, &cfg, &cfg.goal).unwrap();
    assert_eq!(
        stale["superseded"], true,
        "A normal concurrent chat update is not a fatal projection error"
    );
    assert_eq!(stale["applies_to_current_goal"], false);
    assert!(verify(&journal, &cfg, &cfg.goal, &observation).is_err());
    assert!(!journal.complete_goal(&cfg.goal, &old_revision).unwrap());
    assert_eq!(
        view(&journal, &cfg, refined).unwrap()["applies_to_current_goal"],
        true
    );
    assert!(verify(&journal, &cfg, refined, &observation).is_err());
}

#[test]
fn legacy_lineage_is_initialized_once_without_guessing_a_paraphrases_origin() {
    for exact_configured_text in [true, false] {
        let (root, journal, cfg, observation) = fixture();
        let legacy_goal = if exact_configured_text {
            cfg.goal.as_str()
        } else {
            "A historical unclassified objective"
        };
        journal.set("goal", legacy_goal).unwrap();
        journal.set("goal_revision", "42").unwrap();
        journal
            .set("completion_inventory_contract", &contract(&cfg))
            .unwrap();
        assert!(journal.get("task_revision").unwrap().is_none());
        bind(&journal, &cfg, legacy_goal, &observation).unwrap();
        assert_eq!(value(&journal, "task_revision"), "42");
        assert_eq!(
            view(&journal, &cfg, legacy_goal).unwrap()["applies_to_current_goal"],
            exact_configured_text
        );
        assert!(conversation(&journal, GoalAction::Replace, &cfg.goal));
        let new_task = value(&journal, "task_revision");
        drop(journal);

        let restarted = Journal::open(&root.path().join("brain.sqlite")).unwrap();
        bind(&restarted, &cfg, &cfg.goal, &observation).unwrap();
        assert_eq!(value(&restarted, "task_revision"), new_task);
        assert_eq!(
            view(&restarted, &cfg, &cfg.goal).unwrap()["applies_to_current_goal"],
            false
        );
    }
}

#[test]
fn changed_operator_contract_binds_the_current_task_and_reopens_only_its_unmet_completion() {
    let (root, journal, mut cfg, mut observation) = fixture();
    activate(&journal, &cfg, &observation);
    let original_task = value(&journal, "task_revision");
    let new_goal = "Retain a separately configured result";
    assert!(conversation(&journal, GoalAction::Replace, new_goal));
    let new_task = value(&journal, "task_revision");
    let completed_revision = value(&journal, "goal_revision");
    assert_ne!(new_task, original_task);
    assert!(
        journal
            .complete_goal(new_goal, &completed_revision)
            .unwrap()
    );
    drop(journal);

    // This is an explicit operator configuration change across restart, not text
    // supplied by a model restoring an old configured completion requirement.
    cfg.goal = new_goal.into();
    cfg.completion_inventory.clear();
    cfg.completion_inventory
        .insert("fixture:new_result".into(), 2);
    let restarted = Journal::open(&root.path().join("brain.sqlite")).unwrap();
    restarted
        .activate_configured_goal(&cfg.goal, false)
        .unwrap();
    bind(&restarted, &cfg, new_goal, &observation).unwrap();
    assert_eq!(value(&restarted, "task_revision"), new_task);
    assert_eq!(value(&restarted, "completion_task_revision"), new_task);
    assert_ne!(value(&restarted, "goal_revision"), completed_revision);
    assert!(!restarted.goal_completed(new_goal).unwrap());
    assert!(verify(&restarted, &cfg, new_goal, &observation).is_err());
    observation.player["inventory"] = json!([{"item":"fixture:new_result","count":2}]);
    verify(&restarted, &cfg, new_goal, &observation).unwrap();
    let revision = value(&restarted, "goal_revision");
    assert!(restarted.complete_goal(new_goal, &revision).unwrap());
    let reopened: i64 = restarted
        .db
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind='completion_contract_reopened'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reopened, 1);
    observation.player["inventory"] = json!([]);
    bind(&restarted, &cfg, new_goal, &observation).unwrap();
    assert!(
        restarted.goal_completed(new_goal).unwrap(),
        "Unchanged contracts preserve historical success after later inventory use"
    );
}

#[test]
fn refining_completed_work_is_a_noop_and_never_copies_success_to_new_work() {
    let (_root, journal, cfg, mut observation) = fixture();
    activate(&journal, &cfg, &observation);
    observation.player["inventory"] = json!([{"item":"fixture:equipment","count":1}]);
    verify(&journal, &cfg, &cfg.goal, &observation).unwrap();
    let task = value(&journal, "task_revision");
    let revision = value(&journal, "goal_revision");
    assert!(journal.complete_goal(&cfg.goal, &revision).unwrap());
    assert!(!conversation(
        &journal,
        GoalAction::Refine,
        "A paraphrase of completed work"
    ));
    assert_eq!(value(&journal, "task_revision"), task);
    assert_eq!(value(&journal, "goal_revision"), revision);
    assert!(journal.goal_completed(&cfg.goal).unwrap());
    assert!(conversation(&journal, GoalAction::Replace, &cfg.goal));
    assert_ne!(value(&journal, "task_revision"), task);
    assert!(!journal.goal_completed(&cfg.goal).unwrap());
}
