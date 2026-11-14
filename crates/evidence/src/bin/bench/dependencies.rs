//! Opt-in read-only dependency corpora, not a directory-name exclusion rule.
use super::*;

fn mounts(runner: &mut dyn Runner, f: &Fixture, case: &Path, label: &str) -> Result<()> {
    let value = process::call(runner,f,&f.docker,vec!["inspect".into(),"--format".into(),
        "{\"id\":{{json .Id}},\"started_at\":{{json .State.StartedAt}},\"running\":{{json .State.Running}},\"mounts\":{{json .Mounts}}}".into(),f.target()?.into()],case,label)?;
    ensure(
        value["id"] == f.target()?
            && value["started_at"].as_str() == f.container_started_at.as_deref()
            && value["running"] == true,
        "dependency container generation changed or stopped",
    )?;
    config::validate_dependency_mounts(f, &value)
}
fn witness(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    label: &str,
    index: usize,
) -> Result<Value> {
    let corpus = &f.read_only_dependency_corpora[index];
    let helper = f
        .runtime_check
        .as_ref()
        .ok_or("pinned runtime helper required for dependency witness")?;
    let value = process::call(
        runner,
        f,
        &f.docker,
        vec![
            "exec".into(),
            f.target()?.into(),
            helper.clone(),
            "dependency-witness".into(),
            corpus.container_path.clone(),
        ],
        case,
        label,
    )?;
    ensure(
        value["format"] == "minecraft-runtime-dependency-witness-v1"
            && value["complete"] == true
            && value["path"] == corpus.container_path
            && value["sha256"] == corpus.sha256
            && value["root_device"].is_u64()
            && value["root_inode"].as_u64().is_some_and(|n| n > 0)
            && value["entries"]
                .as_u64()
                .is_some_and(|n| (1..=65536).contains(&n))
            && value["bytes"]
                .as_u64()
                .is_some_and(|n| n <= 2 * 1024 * 1024 * 1024),
        "dependency witness incomplete, oversized or differs from reviewed pin",
    )?;
    Ok(value)
}
pub fn preflight(runner: &mut dyn Runner, f: &Fixture, case: &Path, label: &str) -> Result<()> {
    for index in 0..f.read_only_dependency_corpora.len() {
        witness(
            runner,
            f,
            case,
            &format!("{label}-dependency-{index}"),
            index,
        )?;
    }
    Ok(())
}
pub struct Selection {
    pub exclusions: Vec<String>,
    before: Vec<Option<Value>>,
    pub verified_before: bool,
}
pub fn select(runner: &mut dyn Runner, f: &Fixture, case: &Path) -> Selection {
    if f.read_only_dependency_corpora.is_empty() {
        return Selection {
            exclusions: vec![],
            before: vec![],
            verified_before: true,
        };
    }
    let mounts_ok = mounts(runner, f, case, "archive-dependency-mounts-before").is_ok();
    let mut selection = Selection {
        exclusions: vec![],
        before: vec![],
        verified_before: mounts_ok,
    };
    for (index, corpus) in f.read_only_dependency_corpora.iter().enumerate() {
        let found = if mounts_ok {
            witness(
                runner,
                f,
                case,
                &format!("archive-dependency-{index}-before"),
                index,
            )
            .ok()
        } else {
            None
        };
        if found.is_some() && Path::new(&corpus.container_path).starts_with(&f.container_workspace)
        {
            selection.exclusions.push(corpus.container_path.clone());
        }
        selection.verified_before &= found.is_some();
        selection.before.push(found);
    }
    selection
}
pub fn finish(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    selected: &Selection,
) -> Result<bool> {
    if f.read_only_dependency_corpora.is_empty() {
        return Ok(true);
    }
    let mounts_ok = mounts(runner, f, case, "archive-dependency-mounts-after").is_ok();
    let mut records = Vec::new();
    let mut unchanged = selected.verified_before && mounts_ok;
    for (index, corpus) in f.read_only_dependency_corpora.iter().enumerate() {
        let after = if mounts_ok {
            witness(
                runner,
                f,
                case,
                &format!("archive-dependency-{index}-after"),
                index,
            )
            .ok()
        } else {
            None
        };
        let same = selected
            .before
            .get(index)
            .and_then(Option::as_ref)
            .zip(after.as_ref())
            .is_some_and(|(a, b)| {
                a["root_device"] == b["root_device"]
                    && a["root_inode"] == b["root_inode"]
                    && a["sha256"] == b["sha256"]
            });
        unchanged &= same;
        records.push(json!({"container_path":corpus.container_path,"host_dir":corpus.host_dir,"reviewed_sha256":corpus.sha256,
            "excluded_from_tar":selected.exclusions.contains(&corpus.container_path),"unchanged":same,
            "before":selected.before.get(index).and_then(Option::as_ref),"after":after}));
    }
    save(
        &case.join("archive-dependency-corpora.json"),
        &json!({"verified":unchanged,"corpora":records,
        "corpus_bytes_copied_into_evidence":false,"operator_must_retain_host_corpora_and_provenance":true,"scope":"Reviewed dependency-only read-only binds, pinned actual tree snapshots and before/after root identity; host must retain corpus and regeneration provenance. No continuous host immutability or arbitrary content certification."}),
    )?;
    Ok(unchanged)
}

#[cfg(test)]
mod tests {
    //! Simulated Docker/helper/tar responses; real tree hashing is tested separately.
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        inspect: Value,
        witnesses: VecDeque<Value>,
        calls: Vec<Vec<String>>,
        overflow: bool,
    }
    impl Runner for Fake {
        fn run(
            &mut self,
            _: &Path,
            args: &[String],
            _: u64,
            _: &Secrets,
            _: &mut dyn FnMut(&Value) -> Result<()>,
        ) -> Result<process::Output> {
            self.calls.push(args.to_vec());
            let response = match args[0].as_str() {
                "inspect" => self.inspect.clone(),
                "exec" if args.get(3).is_some_and(|a| a == "dependency-witness") => self
                    .witnesses
                    .pop_front()
                    .ok_or("unexpected dependency witness")?,
                "capture" => {
                    use std::os::unix::fs::DirBuilderExt;
                    let path = Path::new(&args[1]);
                    fs::DirBuilder::new().mode(0o700).create(path)?;
                    let mut output = create(&path.join("stdout.log"))?;
                    output.write_all(&[0x1f, 0x8b, 0x08, 0x00])?;
                    output.sync_all()?;
                    let summary = json!({"format":"bot-dev-capture-v1","exit_code":if self.overflow{1}else{0},"timed_out":false,"output_limit":self.overflow,"interrupted":false,"spawn_error":null,"signal":null});
                    save(&path.join("summary.json"), &summary)?;
                    summary
                }
                _ => return Err("unexpected simulated archive call".into()),
            };
            Ok(super::super::tests::output(
                response.to_string(),
                !self.overflow || args[0] != "capture",
            ))
        }
    }
    fn setup() -> (super::super::tests::Setup, Value) {
        let mut s = super::super::tests::setup();
        let host = s.f.workspace.join("vendor");
        fs::create_dir(&host).unwrap();
        fs::write(host.join("lib.rs"), b"preserve dependency source").unwrap();
        s.f.read_only_dependency_corpora
            .push(config::DependencyCorpus {
                host_dir: host,
                container_path: "/workspace/vendor".into(),
                sha256: "d".repeat(64),
            });
        let mut inspect = super::super::tests::mounts(&s.f);
        inspect["mounts"].as_array_mut().unwrap().push(json!({"Type":"bind","Source":s.f.read_only_dependency_corpora[0].host_dir,"Destination":"/workspace/vendor","RW":false}));
        (s, inspect)
    }
    fn witness() -> Value {
        json!({"format":"minecraft-runtime-dependency-witness-v1","complete":true,"path":"/workspace/vendor","sha256":"d".repeat(64),"root_device":1,"root_inode":9,"entries":2,"bytes":26})
    }
    #[test]
    fn only_pinned_readonly_corpus_is_excluded_and_true_overflow_stays_incomplete() {
        for overflow in [false, true] {
            let (s, inspect) = setup();
            s.f.validate().unwrap();
            config::validate_mounts(&s.f, &inspect).unwrap();
            let mut fake = Fake {
                inspect,
                witnesses: VecDeque::from([witness(), witness()]),
                calls: vec![],
                overflow,
            };
            assert_eq!(
                remote::archive(&mut fake, &s.f, &s.case, 1, true).unwrap(),
                !overflow
            );
            let tar = fake.calls.iter().find(|a| a[0] == "capture").unwrap();
            let selected = tar
                .iter()
                .position(|a| a == "--exclude=workspace/vendor")
                .unwrap();
            assert!(tar[..selected].iter().any(|a| a == "--anchored"));
            assert!(tar[..selected].iter().any(|a| a == "--no-wildcards"));
            assert!(tar.iter().all(|a| a != "--exclude=vendor"));
            assert_eq!(
                fs::read(s.f.workspace.join("vendor/lib.rs")).unwrap(),
                b"preserve dependency source"
            );
            assert!(fake.calls.iter().all(|a| a[0] != "stop"));
        }
    }
    #[test]
    fn unlisted_vendor_is_included_and_changed_or_replaced_corpora_are_not_complete() {
        for scenario in 0..4 {
            let (mut s, mut inspect) = setup();
            let mut before = witness();
            let mut after = witness();
            if scenario == 0 {
                s.f.read_only_dependency_corpora.clear();
                inspect["mounts"].as_array_mut().unwrap().pop();
            }
            if scenario == 1 {
                before["sha256"] = json!("e".repeat(64));
            }
            if scenario == 2 {
                after["sha256"] = json!("e".repeat(64));
            }
            if scenario == 3 {
                after["root_inode"] = json!(10);
            }
            let mut fake = Fake {
                inspect,
                witnesses: VecDeque::from([before, after]),
                calls: vec![],
                overflow: false,
            };
            assert_eq!(
                remote::archive(&mut fake, &s.f, &s.case, 1, true).unwrap(),
                scenario == 0
            );
            let tar = fake.calls.iter().find(|a| a[0] == "capture").unwrap();
            assert_eq!(
                tar.iter().any(|a| a == "--exclude=workspace/vendor"),
                scenario >= 2
            );
            assert_eq!(
                fs::read(s.f.workspace.join("vendor/lib.rs")).unwrap(),
                b"preserve dependency source"
            );
        }
    }
    #[test]
    fn writable_nested_missing_and_aliased_mounts_are_rejected() {
        for scenario in 0..5 {
            let (s, mut inspect) = setup();
            let mounts = inspect["mounts"].as_array_mut().unwrap();
            match scenario {
                0 => mounts.last_mut().unwrap()["RW"] = json!(true),
                1 => {
                    mounts.push(json!({"Type":"tmpfs","Source":"","Destination":"/workspace/vendor/nested","RW":true}));
                }
                2 => {
                    mounts.pop();
                }
                3 => {
                    mounts.push(json!({"Type":"bind","Source":s.f.workspace,"Destination":"/alias","RW":true}));
                }
                _ => {
                    mounts.push(mounts.last().unwrap().clone());
                }
            }
            assert!(
                config::validate_dependency_mounts(&s.f, &inspect).is_err(),
                "accepted scenario{scenario}"
            );
        }
        let (s, mut inspect) = setup();
        // A writable workspace parent is safe only when its alias is masked by
        // this exact read-only child destination.
        inspect["mounts"][1] =
            json!({"Type":"bind","Source":s.f.workspace,"Destination":"/workspace","RW":true});
        config::validate_mounts(&s.f, &inspect).unwrap();
    }
    #[test]
    fn corpus_configuration_cannot_cover_program_state_or_operator_inputs() {
        for scenario in 0..6 {
            let (mut s, _) = setup();
            match scenario {
                0 => {
                    s.f.read_only_dependency_corpora[0].container_path = "/workspace/program".into()
                }
                1 => s.f.read_only_dependency_corpora[0].container_path = "/workspace".into(),
                2 => s.f.read_only_dependency_corpora[0].container_path = "/etc".into(),
                3 => s.f.read_only_dependency_corpora[0].host_dir = s.f.secret_dir.clone(),
                4 => s.f.read_only_dependency_corpora[0].sha256 = "not-a-reviewed-tree-pin".into(),
                _ => s.f.read_only_dependency_corpora[0].container_path = "/tmp/brain-state".into(),
            }
            assert!(
                s.f.validate().is_err(),
                "accepted overlapping corpus scenario{scenario}"
            );
        }
    }
}
