use super::*;

fn observation(health: f64, life: &str, epoch: &str, nanos: u64) -> Observation {
    Observation {
        revision: nanos.to_string(),
        player: json!({"uuid":"fixture-actor","life_id":life,"health":health}),
        world: json!({"sample_clock":{"epoch":epoch,"nanos":nanos.to_string()}}),
        menu: json!({}),
    }
}
fn sample(health: f64, nanos: u64) -> Observation {
    observation(health, "original-life", "jvm:original-life", nanos)
}
fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[test]
fn observed_death_then_healthy_respawn_cannot_pass_final_health() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(0., 200), at(150));
    tracker.observe(
        &observation(20., "respawned-life", "jvm:respawned-life", 300),
        at(300),
    );
    let summary = tracker.finish(at(300), true);
    assert_eq!(summary.final_sample.health, 20.);
    assert_eq!(summary.sampled_original_life_min_health, 0.);
    assert_eq!(summary.observed_actor_nonpositive_health_samples, 1);
    assert!(summary.failure_reasons.contains("native_life_changed"));
    assert!(
        summary
            .failure_reasons
            .contains("observed_actor_nonpositive_health")
    );
    assert!(!summary.no_observed_lifecycle_failure());
}

#[test]
fn missed_dead_interval_still_fails_native_life_and_clock_continuity() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(
        &observation(20., "respawned-life", "jvm:respawned-life", 200),
        at(150),
    );
    let summary = tracker.finish(at(150), true);
    assert_eq!(
        summary.observed_actor_nonpositive_health_samples, 0,
        "do not invent an observed death count"
    );
    assert!(summary.failure_reasons.contains("native_life_changed"));
    assert!(
        summary
            .failure_reasons
            .contains("native_clock_epoch_changed")
    );
    assert!(!summary.no_observed_lifecycle_failure());
}

#[test]
fn low_health_minimum_survives_recovery_without_claiming_a_safety_abort() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(4., 200), at(150));
    tracker.observe(&sample(20., 300), at(300));
    let summary = tracker.finish(at(300), true);
    assert_eq!(summary.sampled_original_life_min_health, 4.);
    assert_eq!(summary.final_sample.health, 20.);
    assert!(
        summary.no_observed_lifecycle_failure(),
        "continuity is separate from any chosen health acceptance floor"
    );
    assert!(summary.scope.contains("No guaranteed abort"));
}

#[test]
fn foreign_actor_and_new_clock_epochs_fail_even_with_healthy_samples() {
    for kind in ["actor", "epoch"] {
        let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
        let mut changed = sample(20., 200);
        if kind == "actor" {
            changed.player["uuid"] = json!("foreign-actor");
        } else {
            changed.world["sample_clock"]["epoch"] = json!("new-jvm");
        }
        tracker.observe(&changed, at(150));
        let summary = tracker.finish(at(150), true);
        assert!(!summary.no_observed_lifecycle_failure(), "{kind}");
        assert!(!summary.failure_reasons.is_empty());
    }
}

#[test]
fn clock_regression_is_not_a_fresh_sample_but_unsigned_wrap_is_valid() {
    let mut tracker = Tracker::new(&sample(20., 200)).unwrap();
    tracker.observe(&sample(20., 100), at(150));
    assert!(
        tracker
            .finish(at(150), true)
            .incomplete_reasons
            .contains("ordered_sample_clock_out_of_order")
    );
    let mut tracker = Tracker::new(&sample(20., u64::MAX - 20)).unwrap();
    tracker.observe(&sample(20., 10), at(150));
    assert!(
        tracker
            .finish(at(150), true)
            .no_observed_lifecycle_failure()
    );
}

#[test]
fn repeated_cached_frames_do_not_cover_a_native_sampling_gap() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(20., 100), at(400));
    tracker.observe(&sample(20., 100), at(800));
    tracker.observe(&sample(20., 200), at(1200));
    let summary = tracker.finish(at(1200), true);
    assert_eq!(summary.repeated_native_samples, 2);
    assert_eq!(summary.max_distinct_sample_gap_ms, 1200);
    assert_eq!(summary.gap_acceptance_limit_ms, None);
    assert!(!summary.gap_acceptance_evaluated);
    assert!(
        summary.no_observed_lifecycle_failure(),
        "gap budgets are a separate declared acceptance decision"
    );
}

#[test]
fn native_gaps_and_missing_final_coverage_are_explicit() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(20., 2_000_000_100), at(150));
    let summary = tracker.finish(at(150), false);
    assert_eq!(summary.max_native_sample_gap_nanos, "2000000000");
    assert!(
        summary
            .incomplete_reasons
            .contains("final_scene_not_observed")
    );
    assert!(summary.no_observed_lifecycle_failure());
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(20., 200), at(150));
    let summary = tracker.finish(at(1300), true);
    assert_eq!(summary.uncovered_tail_ms, 1150);
    assert!(summary.no_observed_lifecycle_failure());
}

#[test]
fn read_errors_and_invalid_native_facts_do_not_become_healthy_evidence() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.read_error("fixture transport unavailable", at(100));
    let mut malformed = sample(20., 200);
    malformed.player["life_id"] = Value::Null;
    tracker.observe(&malformed, at(150));
    tracker.observe(&sample(20., 300), at(300));
    let summary = tracker.finish(at(300), true);
    assert_eq!(summary.read_errors, 1);
    assert!(summary.incomplete_reasons.contains("invalid_native_sample"));
    assert!(
        summary.no_observed_lifecycle_failure(),
        "incomplete samples cannot certify no deaths; they do not invent one either"
    );
    assert!(Tracker::new(&malformed).is_err());
}

#[test]
fn no_advanced_samples_cannot_claim_coverage_or_external_death_statistics() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(20., 100), at(500));
    let summary = tracker.finish(at(500), true);
    assert!(summary.no_observed_lifecycle_failure());
    assert!(
        summary
            .incomplete_reasons
            .contains("no_advancing_native_samples")
    );
    assert!(summary.external_death_statistics.starts_with("not read"));
}

#[test]
fn initially_dead_character_stays_failed_after_health_recovers() {
    let mut tracker = Tracker::new(&sample(0., 100)).unwrap();
    tracker.observe(&sample(20., 200), at(150));
    let summary = tracker.finish(at(150), true);
    assert_eq!(summary.final_sample.health, 20.);
    assert_eq!(summary.sampled_original_life_min_health, 0.);
    assert!(!summary.no_observed_lifecycle_failure());
}

#[test]
fn missing_identity_clock_or_health_cannot_initialize_coverage() {
    for missing in ["uuid", "life", "epoch", "nanos", "health"] {
        let mut o = sample(20., 100);
        match missing {
            "uuid" => o.player["uuid"] = json!(""),
            "life" => o.player["life_id"] = Value::Null,
            "epoch" => o.world["sample_clock"]["epoch"] = Value::Null,
            "nanos" => o.world["sample_clock"]["nanos"] = json!(100),
            _ => o.player["health"] = Value::Null,
        }
        assert!(Tracker::new(&o).is_err(), "{missing}");
    }
}

#[test]
fn final_gateway_response_older_than_last_shared_sample_is_not_clock_regression() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(&sample(20., 300), at(150));
    // Protected finalization joins the worker, then takes a final Shared sample.
    tracker.observe(&sample(20., 400), at(300));
    // The already-returned gateway response is an unordered secondary witness.
    tracker.gateway_final_scene(&sample(20., 200), at(300));
    let summary = tracker.finish(at(300), true);
    assert_eq!(summary.final_sample.native_nanos, "400");
    assert_eq!(summary.gateway_final_scene.unwrap().native_nanos, "200");
    assert!(
        !summary
            .incomplete_reasons
            .contains("ordered_sample_clock_out_of_order")
    );
    assert!(summary.failure_reasons.is_empty());
}

#[test]
fn nonpositive_health_counter_covers_observed_actor_across_changed_lives() {
    let mut tracker = Tracker::new(&sample(20., 100)).unwrap();
    tracker.observe(
        &observation(0., "other-life", "jvm:other-life", 200),
        at(150),
    );
    let summary = tracker.finish(at(150), true);
    assert_eq!(summary.observed_actor_nonpositive_health_samples, 1);
    assert_eq!(
        summary.sampled_original_life_min_health, 20.,
        "a different life cannot overwrite original-life minimum"
    );
    assert!(!summary.no_observed_lifecycle_failure());
}
