//! Source-only fixture contract regressions; no tools, credentials, or native calls.
use super::*;
use config::Mode;

fn update_config(f: &mut Fixture, edit: impl FnOnce(&mut Value)) {
    let path = f.config_path().unwrap().to_owned();
    let index = f
        .artifacts
        .iter()
        .position(|a| a.container_path == path)
        .unwrap();
    let file = f.artifacts[index].file.path.clone();
    let mut cfg = json(&file, 65536).unwrap();
    edit(&mut cfg);
    fs::write(&file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    f.artifacts[index].file.sha256 = hash_file(&file).unwrap();
}
pub(super) fn acquisition() -> super::tests::Setup {
    let mut setup = super::tests::setup();
    let config = setup.f.config_path().unwrap().to_owned();
    let admission = setup.f.handler_args[3].clone();
    setup.f.mode = Mode::Acquisition;
    setup.f.npc_uuid = None;
    setup.f.expected_npc_position = None;
    setup.f.duration_secs = 1200;
    setup.f.handler_args = vec!["run".into(), config];
    setup.f.artifacts.retain(|a| a.container_path != admission);
    update_config(&mut setup.f, |cfg| cfg["body"]["enabled"] = json!(true));
    setup
}
#[test]
fn legacy_fixture_defaults_to_skeleton_with_two_private_inputs() {
    let setup = super::tests::setup();
    assert_eq!(Mode::default(), Mode::Skeleton);
    assert_eq!(setup.f.mode, Mode::Skeleton);
    setup.f.validate().unwrap();
    assert!(!setup.f.is_acquisition());
    assert_eq!(setup.f.config_path().unwrap(), setup.f.handler_args[0]);
    assert_eq!(
        setup.f.private_input_paths().unwrap(),
        vec![
            setup.f.handler_args[0].as_str(),
            setup.f.handler_args[3].as_str()
        ]
    );
    let (npc, position) = setup.f.skeleton_npc().unwrap();
    assert_eq!(Some(npc), setup.f.npc_uuid.as_deref());
    assert_eq!(Some(position), setup.f.expected_npc_position);
}
#[test]
fn acquisition_needs_no_npc_or_admission_artifact_and_uses_brain_config() {
    let setup = acquisition();
    setup.f.validate().unwrap();
    assert!(setup.f.is_acquisition());
    assert!(setup.f.npc_uuid.is_none() && setup.f.expected_npc_position.is_none());
    assert!(setup.f.skeleton_npc().is_err());
    assert_eq!(setup.f.config_path().unwrap(), setup.f.handler_args[1]);
    assert_eq!(
        setup.f.private_input_paths().unwrap(),
        vec![setup.f.handler_args[1].as_str()]
    );
    assert!(
        setup
            .f
            .artifacts
            .iter()
            .all(|a| a.container_path != "/workspace/spec.json")
    );
    assert_eq!(setup.f.state_path().unwrap(), "/tmp/brain-state");
}
#[test]
fn acquisition_forbids_even_valid_declared_npc_values() {
    let mut setup = acquisition();
    setup.f.npc_uuid = Some("a3af7dfd-4558-4ef8-bccb-4c5379960383".into());
    assert!(
        setup
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("forbids declared NPC")
    );
    setup.f.npc_uuid = None;
    setup.f.expected_npc_position = Some([262.5, 64.0, -709.5]);
    assert!(
        setup
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("forbids declared NPC")
    );
}
#[test]
fn skeleton_still_requires_both_npc_fields() {
    let mut setup = super::tests::setup();
    let original = setup.f.npc_uuid.take();
    assert!(setup.f.validate().is_err());
    setup.f.npc_uuid = original;
    setup.f.expected_npc_position = None;
    assert!(setup.f.validate().is_err());
}
#[test]
fn modes_enforce_exact_duration_and_distinct_startup_body_policy() {
    let mut setup = acquisition();
    setup.f.duration_secs = 120;
    assert!(setup.f.validate().is_err());
    setup.f.duration_secs = 1200;
    update_config(&mut setup.f, |cfg| cfg["body"]["enabled"] = json!(false));
    assert!(
        setup
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("mode-specific startup body")
    );
    let mut skeleton = super::tests::setup();
    skeleton.f.duration_secs = 1200;
    assert!(skeleton.f.validate().is_err());
    skeleton.f.duration_secs = 120;
    update_config(&mut skeleton.f, |cfg| cfg["body"]["enabled"] = json!(true));
    assert!(
        skeleton
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("mode-specific startup body")
    );
}
#[test]
fn acquisition_rejects_probe_argv_and_missing_allocated_proxy() {
    let mut setup = acquisition();
    let original = setup.f.handler_args.clone();
    setup.f.handler_args = vec![
        "/workspace/config.json".into(),
        "task".into(),
        "120".into(),
        "/workspace/spec.json".into(),
    ];
    assert!(setup.f.validate().is_err());
    setup.f.handler_args = original;
    setup.f.allocated_model_kind = None;
    assert!(
        setup
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("allocated proxy")
    );
    setup.f.allocated_model_kind = Some("operator-allocated-proxy".into());
    fs::remove_file(setup.f.model_key().unwrap()).unwrap();
    setup.f.model_key_name = None;
    update_config(&mut setup.f, |cfg| cfg["model_key"] = json!(""));
    assert!(
        setup
            .f
            .validate()
            .unwrap_err()
            .to_string()
            .contains("allocated proxy")
    );
}
#[test]
fn acquisition_config_must_remain_pinned_and_scoped() {
    let mut setup = acquisition();
    let path = setup.f.config_path().unwrap().to_owned();
    setup.f.artifacts.retain(|a| a.container_path != path);
    assert!(setup.f.validate().is_err());
    let mut setup = acquisition();
    update_config(&mut setup.f, |cfg| {
        cfg["profile"] = json!("/workspace/copied-profile.json")
    });
    assert!(setup.f.validate().is_err());
}
#[test]
fn mode_and_input_accessors_reject_unknown_or_truncated_shapes_without_panicking() {
    assert!(serde_json::from_str::<Mode>("\"other\"").is_err());
    let mut setup = acquisition();
    setup.f.handler_args = vec!["run".into()];
    assert!(setup.f.config_path().is_err());
    assert!(setup.f.private_input_paths().is_err());
    assert!(setup.f.state_path().is_err());
    assert!(setup.f.validate().is_err());
    setup.f.mode = Mode::Skeleton;
    setup.f.handler_args.clear();
    assert!(setup.f.config_path().is_err());
    assert!(setup.f.private_input_paths().is_err());
    assert!(setup.f.state_path().is_err());
}
fn combined() -> super::tests::Setup {
    let mut s = super::tests::setup();
    s.f.mode = Mode::Combined;
    s.f.duration_secs = 1200;
    s.f.handler_args[1] = "task-kit".into();
    s.f.handler_args[2] = "1200".into();
    let journal =
        s.f.workspace
            .parent()
            .unwrap()
            .join("synthetic-operator.sqlite");
    drop(create(&journal).unwrap());
    s.f.operator_gateway_journal = Some(journal);
    update_config(&mut s.f, |cfg| {
        cfg["goal"] =
            json!("Create and retain the declared kit while surviving the admitted threat");
        cfg["completion_inventory"] =
            json!({"minecraft:crafting_table":1,"minecraft:wooden_pickaxe":1});
    });
    s
}
#[test]
fn combined_mode_binds_kit_admission_deadlines_and_separate_evidence_limits() {
    let s = combined();
    s.f.validate().unwrap();
    assert!(s.f.is_combined() && s.f.immediate_launch() && !s.f.is_acquisition());
    assert_eq!(s.f.private_input_paths().unwrap().len(), 2);
    assert_eq!(s.f.child_deadlines(), (1325, 1335));
    assert_eq!(
        (s.f.trace_bytes_limit(), s.f.action_limit()),
        (128 * 1024 * 1024, 32768)
    );
    let legacy = super::tests::setup();
    assert_eq!(legacy.f.child_deadlines(), (245, 255));
    assert_eq!(
        (legacy.f.trace_bytes_limit(), legacy.f.action_limit()),
        (16 * 1024 * 1024, 4096)
    );
    assert_eq!(acquisition().f.child_deadlines(), (1200, 1210));
}
#[test]
fn combined_mode_rejects_wrong_kit_goal_argv_duration_and_missing_authority() {
    for scenario in 0..6 {
        let mut s = combined();
        match scenario {
            0 => s.f.operator_gateway_journal = None,
            1 => s.f.handler_args[1] = "task".into(),
            2 => s.f.duration_secs = 120,
            3 => s.f.npc_uuid = None,
            4 => update_config(&mut s.f, |cfg| cfg["completion_inventory"] = json!({})),
            _ => update_config(&mut s.f, |cfg| cfg["goal"] = json!("")),
        }
        assert!(
            s.f.validate().is_err(),
            "accepted invalid combined scenario{scenario}"
        );
    }
}
