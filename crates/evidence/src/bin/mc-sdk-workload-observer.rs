#[cfg(any(target_os = "linux", test))]
mod workload_observer;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mc-sdk-workload-observer requires Linux /proc; no workload was started");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    if let Err(code) = linux::run() {
        // Only static codes: do not disclose paths, arguments, or OS messages.
        eprintln!("{code}");
        std::process::exit(2);
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::workload_observer::{
        Identity, RootState, Stat, next_sample_at, normalized_sha256, parse_stat, root_state,
        valid_bounds,
    };
    use serde_json::{Value, json};
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        ffi::CString,
        fs::{self, File, Metadata},
        io::{self, Read},
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        path::{Path, PathBuf},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const MAX_PROCESSES: usize = 256;
    const MAX_THREADS: usize = 256;
    const MAX_CHILDREN: usize = 512;
    const MAX_IDENTITIES: usize = 4096;
    const MAX_ISSUES: usize = 512;
    const MAX_FILE_BYTES: u64 = 32 * 1024;
    const FINALIZATION_RESERVE: Duration = Duration::from_millis(100);
    type Result<T> = std::result::Result<T, &'static str>;

    struct Config {
        root: Identity,
        seconds: u64,
        interval_ms: u64,
        expected_exe: PathBuf,
        declared_sha: String,
    }
    impl Config {
        fn args() -> Result<Self> {
            let mut args = std::env::args_os().skip(1);
            let mut values = BTreeMap::new();
            while let Some(key) = args.next() {
                let key = key.into_string().map_err(|_| "invalid_argument")?;
                if ![
                    "--root-pid",
                    "--root-start-jiffies",
                    "--seconds",
                    "--interval-ms",
                    "--expected-rustc",
                    "--source-sha256",
                ]
                .contains(&key.as_str())
                {
                    return Err("unknown_argument");
                }
                let value = args.next().ok_or("missing_argument_value")?;
                if values.insert(key, value).is_some() {
                    return Err("duplicate_argument");
                }
            }
            let text = |key: &str| -> Result<&str> {
                values
                    .get(key)
                    .and_then(|s| s.to_str())
                    .ok_or("missing_or_invalid_argument")
            };
            let pid = text("--root-pid")?
                .parse()
                .map_err(|_| "invalid_root_pid")?;
            let start_jiffies = text("--root-start-jiffies")?
                .parse()
                .map_err(|_| "invalid_root_start")?;
            let seconds = text("--seconds")?.parse().map_err(|_| "invalid_duration")?;
            let interval_ms = text("--interval-ms")?
                .parse()
                .map_err(|_| "invalid_interval")?;
            if pid == 0 || start_jiffies == 0 || !valid_bounds(seconds, interval_ms) {
                return Err("invalid_bounds");
            }
            let expected_exe = PathBuf::from(
                values
                    .get("--expected-rustc")
                    .ok_or("missing_expected_executable")?,
            );
            if !expected_exe.is_absolute() {
                return Err("expected_executable_must_be_absolute");
            }
            Ok(Self {
                root: Identity { pid, start_jiffies },
                seconds,
                interval_ms,
                expected_exe,
                declared_sha: normalized_sha256(text("--source-sha256")?)
                    .ok_or("invalid_source_sha256")?,
            })
        }
    }

    #[derive(Clone, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_ns: i64,
        ctime: i64,
        ctime_ns: i64,
    }
    impl From<&Metadata> for FileIdentity {
        fn from(m: &Metadata) -> Self {
            Self {
                device: m.dev(),
                inode: m.ino(),
                size: m.len(),
                mode: m.mode(),
                mtime: m.mtime(),
                mtime_ns: m.mtime_nsec(),
                ctime: m.ctime(),
                ctime_ns: m.ctime_nsec(),
            }
        }
    }
    struct Expected {
        canonical: PathBuf,
        identity: FileIdentity,
    }
    impl Expected {
        fn open(path: &Path) -> Result<Self> {
            let canonical =
                fs::canonicalize(path).map_err(|_| "expected_executable_inaccessible")?;
            let m =
                fs::metadata(&canonical).map_err(|_| "expected_executable_metadata_unavailable")?;
            if !m.is_file() || m.mode() & 0o111 == 0 {
                return Err("expected_executable_not_executable_file");
            }
            let c = CString::new(canonical.as_os_str().as_bytes())
                .map_err(|_| "invalid_expected_executable")?;
            let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // The path is NUL terminated and stat points to writable storage.
            if unsafe { libc::statvfs(c.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return Err("expected_executable_mount_unavailable");
            }
            // The successful call initialized the structure.
            if unsafe { stat.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
                return Err("expected_executable_mount_not_read_only");
            }
            Ok(Self {
                canonical,
                identity: FileIdentity::from(&m),
            })
        }
        fn unchanged(&self) -> bool {
            fs::metadata(&self.canonical).is_ok_and(|m| FileIdentity::from(&m) == self.identity)
        }
        fn matches(&self, pid: u32) -> Result<bool> {
            let exe = fs::canonicalize(format!("/proc/{pid}/exe")).map_err(|e| io_code(&e))?;
            let m = fs::metadata(format!("/proc/{pid}/exe")).map_err(|e| io_code(&e))?;
            Ok(exe == self.canonical && FileIdentity::from(&m) == self.identity)
        }
    }
    fn io_code(e: &io::Error) -> &'static str {
        match e.kind() {
            io::ErrorKind::NotFound => "absent",
            io::ErrorKind::PermissionDenied => "inaccessible",
            _ => "read_error",
        }
    }
    fn bounded_read(path: impl AsRef<Path>) -> Result<String> {
        let file = File::open(path).map_err(|e| io_code(&e))?;
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_code(&e))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("proc_file_limit");
        }
        String::from_utf8(bytes).map_err(|_| "invalid_proc_text")
    }
    fn stat(pid: u32) -> Result<Stat> {
        let s =
            parse_stat(&bounded_read(format!("/proc/{pid}/stat"))?).ok_or("invalid_proc_stat")?;
        if s.identity.pid != pid {
            return Err("pid_mismatch");
        }
        Ok(s)
    }
    fn utc_ms() -> Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .ok_or("utc_unavailable")
    }
    struct Output {
        fd: libc::c_int,
        original_flags: libc::c_int,
    }
    impl Output {
        fn new(fd: libc::c_int) -> Result<Self> {
            if unsafe { libc::isatty(fd) } == 1 {
                return Err("output_must_be_private_pipe_or_file");
            }
            // Only the observer's output descriptor is changed, never a workload fd.
            let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if original_flags < 0
                || unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0
            {
                return Err("output_nonblocking_unavailable");
            }
            Ok(Self { fd, original_flags })
        }
        fn emit(&mut self, value: Value, deadline: Instant) -> Result<()> {
            let mut bytes =
                serde_json::to_vec(&value).map_err(|_| "output_serialization_failed")?;
            bytes.push(b'\n');
            let mut offset = 0;
            while offset < bytes.len() {
                if Instant::now() >= deadline {
                    return Err("output_deadline");
                }
                // bytes remains live and the unwritten range is valid.
                let written = unsafe {
                    libc::write(
                        self.fd,
                        bytes[offset..].as_ptr().cast(),
                        bytes.len() - offset,
                    )
                };
                if written > 0 {
                    offset += written as usize;
                    continue;
                }
                if written == 0 {
                    return Err("output_closed");
                }
                match io::Error::last_os_error().kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => {}
                    _ => return Err("output_failed"),
                }
                let mut poll = libc::pollfd {
                    fd: self.fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or("output_deadline")?;
                let wait_ms = remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32;
                // poll points to one valid descriptor; remaining time bounds backpressure.
                let ready = unsafe { libc::poll(&mut poll, 1, wait_ms) };
                if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return Err("output_poll_failed");
                }
                if ready > 0 && poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
                {
                    return Err("output_closed");
                }
            }
            if Instant::now() >= deadline {
                return Err("output_deadline");
            }
            Ok(())
        }
    }
    impl Drop for Output {
        fn drop(&mut self) {
            // Best-effort restoration; an external hard timeout may skip Drop.
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.original_flags);
            }
        }
    }
    fn identity_json(id: Identity) -> Value {
        json!({"pid":id.pid,"start_jiffies":id.start_jiffies})
    }
    fn issue(issues: &mut Vec<Value>, id: Identity, stage: &'static str, code: &'static str) {
        if issues.len() > MAX_ISSUES {
            return;
        }
        if issues.len() == MAX_ISSUES {
            issues.push(json!({"stage":"issues","status":"issue_limit_further_details_omitted"}));
            return;
        }
        issues.push(json!({"identity":identity_json(id),"stage":stage,"status":code}));
    }

    struct Sample {
        processes: BTreeMap<Identity, (Stat, Option<bool>)>,
        issues: Vec<Value>,
        complete: bool,
        stop: Option<&'static str>,
    }
    fn sample(root: Identity, expected: &Expected, deadline: Instant) -> Sample {
        let mut out = Sample {
            processes: BTreeMap::new(),
            issues: Vec::new(),
            complete: true,
            stop: None,
        };
        let root_stat = match stat(root.pid) {
            Ok(s) => s,
            Err(code) => {
                issue(&mut out.issues, root, "root_stat", code);
                out.complete = false;
                out.stop = Some(if code == "absent" {
                    "root_exited"
                } else {
                    "root_inaccessible"
                });
                return out;
            }
        };
        if root_state(root, Some(root_stat.identity)) != RootState::Same {
            out.stop = Some("root_pid_reused");
            out.complete = false;
            return out;
        }
        if root_stat.exited {
            out.stop = Some("root_exited");
            return out;
        }
        let mut queue = VecDeque::from([root_stat]);
        let mut queued = BTreeSet::from([root]);
        while let Some(current) = queue.pop_front() {
            if Instant::now() >= deadline {
                out.stop = Some("deadline");
                out.complete = false;
                break;
            }
            let id = current.identity;
            // Re-read immediately before traversal; queue entries may have exited/reused.
            let before = match stat(id.pid) {
                Ok(s) if s.identity == id && s.ppid == current.ppid => s,
                Ok(_) => {
                    issue(&mut out.issues, id, "identity", "changed");
                    out.complete = false;
                    continue;
                }
                Err(code) => {
                    issue(&mut out.issues, id, "stat", code);
                    out.complete = false;
                    continue;
                }
            };
            let matched = match expected.matches(id.pid) {
                Ok(value) => Some(value),
                Err(code) => {
                    issue(&mut out.issues, id, "exe", code);
                    None
                }
            };
            let tasks = match fs::read_dir(format!("/proc/{}/task", id.pid)) {
                Ok(tasks) => tasks,
                Err(e) => {
                    issue(&mut out.issues, id, "task_list", io_code(&e));
                    out.complete = false;
                    if stat(id.pid).is_ok_and(|s| s.identity == id && s.ppid == before.ppid) {
                        out.processes.insert(id, (before, matched));
                    }
                    continue;
                }
            };
            let mut children = BTreeSet::new();
            for (i, task) in tasks.enumerate() {
                if Instant::now() >= deadline {
                    out.stop = Some("deadline");
                    out.complete = false;
                    break;
                }
                if i >= MAX_THREADS {
                    issue(&mut out.issues, id, "task_list", "thread_limit");
                    out.complete = false;
                    break;
                }
                let task = match task {
                    Ok(t) => t,
                    Err(e) => {
                        issue(&mut out.issues, id, "task_entry", io_code(&e));
                        out.complete = false;
                        continue;
                    }
                };
                let Some(tid) = task
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                match bounded_read(format!("/proc/{}/task/{tid}/children", id.pid)) {
                    Ok(text) => {
                        for token in text.split_whitespace() {
                            let Some(pid) = token.parse::<u32>().ok().filter(|n| *n > 0) else {
                                issue(&mut out.issues, id, "children", "invalid_child_pid");
                                out.complete = false;
                                continue;
                            };
                            if children.len() >= MAX_CHILDREN {
                                issue(&mut out.issues, id, "children", "child_limit");
                                out.complete = false;
                                break;
                            }
                            children.insert(pid);
                        }
                    }
                    Err(code) => {
                        issue(&mut out.issues, id, "children", code);
                        out.complete = false;
                    }
                }
            }
            // A racing parent exit/reuse must not authorize following a new tree.
            match stat(id.pid) {
                Ok(after) if after.identity == id && after.ppid == before.ppid => {
                    out.processes.insert(id, (after, matched));
                }
                _ => {
                    issue(&mut out.issues, id, "identity_after", "changed_or_absent");
                    out.complete = false;
                    continue;
                }
            }
            for child_pid in children {
                if Instant::now() >= deadline {
                    out.stop = Some("deadline");
                    out.complete = false;
                    break;
                }
                match stat(child_pid) {
                    Ok(child) if child.ppid == id.pid => {
                        if queued.contains(&child.identity) {
                            continue;
                        }
                        if queued.len() >= MAX_PROCESSES {
                            issue(&mut out.issues, id, "descendants", "process_limit");
                            out.complete = false;
                            break;
                        }
                        queued.insert(child.identity);
                        queue.push_back(child);
                    }
                    Ok(_) => {
                        issue(&mut out.issues, id, "child_stat", "parent_changed");
                        out.complete = false;
                    }
                    Err(code) => {
                        issue(&mut out.issues, id, "child_stat", code);
                        out.complete = false;
                    }
                }
            }
        }
        // Stop if the specified root has gone away even when descendants remain.
        match stat(root.pid) {
            Ok(s) if s.identity == root && !s.exited => {}
            Ok(s) if s.identity == root => {
                out.stop = Some("root_exited");
            }
            Ok(_) => {
                out.stop = Some("root_pid_reused");
                out.complete = false;
            }
            Err(code) => {
                out.stop = Some(if code == "absent" {
                    "root_exited"
                } else {
                    "root_inaccessible"
                });
                out.complete = false;
                issue(&mut out.issues, root, "root_final", code);
            }
        }
        out
    }

    struct Seen {
        first_lower: u64,
        first_upper: u64,
        last_lower: u64,
        last_upper: u64,
        first_cpu: (u64, u64),
        last_cpu: (u64, u64),
        samples: u64,
        matching_samples: u64,
    }

    pub fn run() -> Result<()> {
        let start = Instant::now();
        let config = Config::args()?;
        let limit = Duration::from_secs(config.seconds);
        let deadline = start + limit;
        let observation_deadline = deadline - FINALIZATION_RESERVE;
        let expected = Expected::open(&config.expected_exe)?;
        let root = stat(config.root.pid).map_err(|code| match code {
            "absent" => "root_preflight_absent",
            "inaccessible" => "root_preflight_inaccessible",
            _ => "root_preflight_read_failed",
        })?;
        if root.identity != config.root {
            return Err("root_preflight_identity_mismatch");
        }
        if root.exited {
            return Err("root_preflight_exited");
        }
        let period = Duration::from_millis(config.interval_ms);
        let mut next = Duration::ZERO;
        let mut output = Output::new(libc::STDOUT_FILENO)?;
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks_per_second <= 0 {
            return Err("clock_tick_rate_unavailable");
        }
        output.emit(
            json!({"event":"observer_start","schema":1,"root":identity_json(config.root),
            "at_utc_ms":utc_ms()?,"duration_limit_seconds":config.seconds,"interval_ms":config.interval_ms,
            "finalization_reserve_ms":FINALIZATION_RESERVE.as_millis(),"outer_observer_only_timeout_required":true,
            "completion_requires":"exit0 plus complete observer_finished and observer_output_complete records within external wall deadline",
            "source_sha256_caller_declared":config.declared_sha,"source_input_certified":false,
            "expected_executable_read_only_mount":true,"clock_ticks_per_second":ticks_per_second,
            "limits":{"processes":MAX_PROCESSES,"threads_per_process":MAX_THREADS,"children_per_process":MAX_CHILDREN,"total_identities":MAX_IDENTITIES,"issues_per_sample":MAX_ISSUES,"proc_file_bytes":MAX_FILE_BYTES},
            "scope":"Allocated root and current descendants only; sampled process/executable evidence, not compiler input or inference certification"}), deadline,
        )?;
        let mut seen: BTreeMap<Identity, Seen> = BTreeMap::new();
        let mut previous_observed = BTreeSet::new();
        let mut seq = 0_u64;
        let mut previous_upper = None;
        let mut clock_regressed = false;
        let mut partial_samples = 0_u64;
        let mut max_sample_ms = 0_u128;
        let mut max_sample_cycle_ms = 0_u128;
        let mut max_gap_ms = 0_u128;
        let mut last_sample_start = None;
        let reason = loop {
            if Instant::now() >= observation_deadline {
                break "deadline";
            }
            if !expected.unchanged() {
                break "expected_executable_changed_or_inaccessible";
            }
            let sample_start = Instant::now();
            if let Some(previous) = last_sample_start {
                max_gap_ms = max_gap_ms.max(sample_start.duration_since(previous).as_millis());
            }
            last_sample_start = Some(sample_start);
            let lower = utc_ms()?;
            let sample = sample(config.root, &expected, observation_deadline);
            let upper = utc_ms()?;
            if upper < lower || previous_upper.is_some_and(|p| lower < p) {
                clock_regressed = true;
            }
            previous_upper = Some(upper);
            if !sample.complete {
                partial_samples += 1;
            }
            max_sample_ms = max_sample_ms.max(sample_start.elapsed().as_millis());
            let mut observations = Vec::new();
            let mut identity_limit = false;
            for (id, (stat, matched)) in &sample.processes {
                if !seen.contains_key(id) && seen.len() >= MAX_IDENTITIES {
                    identity_limit = true;
                    break;
                }
                let s = seen.entry(*id).or_insert(Seen {
                    first_lower: lower,
                    first_upper: upper,
                    last_lower: lower,
                    last_upper: upper,
                    first_cpu: (stat.user_ticks, stat.system_ticks),
                    last_cpu: (stat.user_ticks, stat.system_ticks),
                    samples: 0,
                    matching_samples: 0,
                });
                s.last_lower = lower;
                s.last_upper = upper;
                s.last_cpu = (stat.user_ticks, stat.system_ticks);
                s.samples += 1;
                s.matching_samples += u64::from(*matched == Some(true));
                observations.push(json!({"pid":id.pid,"ppid":stat.ppid,"start_jiffies":id.start_jiffies,
                    "user_ticks":stat.user_ticks,"system_ticks":stat.system_ticks,"exe_matches_expected":matched}));
            }
            // Emit each disappearance transition once, rather than repeatedly
            // expanding output for every historical identity.
            let absent: Vec<_> = previous_observed.iter().filter(|id| !sample.processes.contains_key(*id)).map(|id|
                json!({"identity":identity_json(*id),"status":if sample.complete {"not_in_current_tree"} else {"not_observed_in_partial_sample"}})).collect();
            output.emit(
                json!({"event":"proc_sample","seq":seq,"elapsed_ms":start.elapsed().as_millis(),
                "utc_lower_ms":lower,"utc_upper_ms":upper,"tree_complete":sample.complete,
                "processes":observations,"not_observed":absent,"issues":sample.issues,"identity_limit_reached":identity_limit}), deadline,
            )?;
            max_sample_cycle_ms = max_sample_cycle_ms.max(sample_start.elapsed().as_millis());
            previous_observed = sample.processes.keys().copied().collect();
            seq += 1;
            if identity_limit {
                break "total_identity_limit";
            }
            if let Some(reason) = sample.stop {
                break reason;
            }
            let Some(due) =
                next_sample_at(start.elapsed(), next, period, limit - FINALIZATION_RESERVE)
            else {
                break "deadline";
            };
            next = due;
            std::thread::sleep(next.saturating_sub(start.elapsed()));
        };
        let summaries: Vec<_> = seen.into_iter().map(|(id, s)| json!({"identity":identity_json(id),
            "first_observation_utc_lower_ms":s.first_lower,"first_observation_utc_upper_ms":s.first_upper,
            "last_observation_utc_lower_ms":s.last_lower,"last_observation_utc_upper_ms":s.last_upper,
            "first_user_ticks":s.first_cpu.0,"first_system_ticks":s.first_cpu.1,
            "last_user_ticks":s.last_cpu.0,"last_system_ticks":s.last_cpu.1,"samples":s.samples,"exe_matching_samples":s.matching_samples})).collect();
        output.emit(
            json!({"event":"observer_finished","stop_reason":reason,"at_utc_ms":utc_ms()?,
            "elapsed_ms":start.elapsed().as_millis(),"sample_count":seq,"partial_sample_count":partial_samples,
            "max_sample_duration_ms":max_sample_ms,"max_sample_start_gap_ms":max_gap_ms,"utc_regressed":clock_regressed,
            "max_sample_cycle_including_output_ms":max_sample_cycle_ms,
            "expected_executable_identity_unchanged":expected.unchanged(),"processes":summaries,
            "scope":"Only sampled identities and CPU counters; missed short processes and unobserved intervals remain possible; no processes signalled or spawned"}), deadline,
        )?;
        // This separate record measures the final summary write, including a
        // possible terminal stall that has no subsequent sample-start gap.
        let after_summary = Instant::now();
        output.emit(json!({"event":"observer_output_complete","summary_output_finished_elapsed_ms":start.elapsed().as_millis(),
            "trailing_gap_after_last_sample_start_ms":last_sample_start.map(|t|after_summary.duration_since(t).as_millis()),
            "deadline_overshoot_after_summary_ms":after_summary.saturating_duration_since(deadline).as_millis(),
            "at_utc_ms":utc_ms()?,"scope":"Requires exit0 and external observer-only wall deadline; truncated or missing completion is incomplete"}), deadline)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        #[test]
        fn full_pipe_backpressure_stops_at_output_deadline() {
            let mut fds = [-1; 2];
            // Both pipe ends are retained; no reader drains the full pipe.
            assert_eq!(
                unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
                0
            );
            let (_read, write) =
                unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            let bytes = [0_u8; 4096];
            let mut filled = false;
            for _ in 0..1024 {
                let n =
                    unsafe { libc::write(write.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                if n < 0 {
                    assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                    filled = true;
                    break;
                }
            }
            assert!(
                filled,
                "test pipe must be full before observing backpressure"
            );
            let mut output = Output::new(write.as_raw_fd()).unwrap();
            let start = Instant::now();
            assert_eq!(
                output.emit(
                    json!({"event":"backpressure_regression"}),
                    start + Duration::from_millis(80)
                ),
                Err("output_deadline")
            );
            assert!(start.elapsed() >= Duration::from_millis(60));
            assert!(start.elapsed() < Duration::from_secs(1));
        }
    }
}
