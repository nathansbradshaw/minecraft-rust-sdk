use super::*;
use std::{
    io::BufReader,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Instant,
};

pub struct Output {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}
pub trait Runner {
    fn ca(&mut self, _: Option<&Path>) {}
    fn run(
        &mut self,
        program: &Path,
        args: &[String],
        seconds: u64,
        secrets: &Secrets,
        on_line: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Output>;
}
#[derive(Default)]
pub struct SystemRunner {
    ca: Option<PathBuf>,
}
impl SystemRunner {
    pub fn new() -> Self {
        Self::default()
    }
}
struct ChildGuard(std::process::Child);
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Runner for SystemRunner {
    fn ca(&mut self, path: Option<&Path>) {
        self.ca = path.map(Path::to_path_buf);
    }
    fn run(
        &mut self,
        program: &Path,
        args: &[String],
        seconds: u64,
        secrets: &Secrets,
        on_line: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Output> {
        let mut command = Command::new(program);
        command.args(args).env_remove("MC_SDK_CA_CERT");
        if let Some(ca) = &self.ca {
            command.env("MC_SDK_CA_CERT", ca);
        }
        let mut child = ChildGuard(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let (send, recv) = mpsc::sync_channel(16);
        for (is_err, stream) in [
            (
                false,
                Box::new(child.stdout.take().ok_or("stdout absent")?) as Box<dyn Read + Send>,
            ),
            (
                true,
                Box::new(child.stderr.take().ok_or("stderr absent")?) as Box<dyn Read + Send>,
            ),
        ] {
            let send = send.clone();
            thread::spawn(move || {
                let mut stream = BufReader::new(stream);
                let mut buffer = [0; 8192];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if send.send((is_err, Ok(buffer[..n].to_vec()))).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = send.send((is_err, Err(())));
                            break;
                        }
                    }
                }
            });
        }
        drop(send);
        let started = Instant::now();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut pending = Vec::new();
        let mut timed_out = false;
        let mut failed = false;
        let mut status = None;
        loop {
            if started.elapsed() >= Duration::from_secs(seconds) {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            match recv.recv_timeout(Duration::from_millis(25)) {
                Ok((is_err, Ok(bytes))) => {
                    let target = if is_err { &mut stderr } else { &mut stdout };
                    if target.len() + bytes.len() > 16 * 1024 * 1024 {
                        failed = true;
                        let _ = child.kill();
                        break;
                    }
                    target.extend_from_slice(&bytes);
                    if !is_err {
                        pending.extend_from_slice(&bytes);
                        while let Some(n) = pending.iter().position(|b| *b == b'\n') {
                            let line: Vec<_> = pending.drain(..=n).collect();
                            if let Ok(value) = serde_json::from_slice(&line) {
                                if on_line(&value).is_err() {
                                    failed = true;
                                    let _ = child.kill();
                                    break;
                                }
                            }
                        }
                        if pending.len() > 1024 * 1024 {
                            failed = true;
                            let _ = child.kill();
                            break;
                        }
                    }
                }
                Ok((_, Err(()))) => {
                    failed = true;
                    let _ = child.kill();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(done) = child.try_wait()? {
                        status = Some(done);
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if failed {
                break;
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
        }
        // Pipes inherited by descendants remain subject to the same host deadline.
        let status = match status {
            Some(s) => s,
            None => child.wait()?,
        };
        if !pending.is_empty() && !failed && !timed_out {
            if let Ok(value) = serde_json::from_slice(&pending) {
                if on_line(&value).is_err() {
                    failed = true;
                }
            }
        }
        Ok(Output {
            exit_code: status.code(),
            signal: std::os::unix::process::ExitStatusExt::signal(&status),
            success: status.success() && !failed && !timed_out,
            stdout: secrets.scrub(&stdout),
            stderr: secrets.scrub(&stderr),
            timed_out,
        })
    }
}
pub fn execute(
    runner: &mut dyn Runner,
    f: &Fixture,
    tool: &Pinned,
    args: Vec<String>,
    seconds: u64,
    case: &Path,
    label: &str,
    on_line: &mut dyn FnMut(&Value) -> Result<()>,
) -> Result<Output> {
    tool.verify()?;
    let mut secrets = f.secrets()?;
    runner.ca(if tool.path == f.sdk.path {
        f.ca_file.as_ref().map(|ca| ca.file.path.as_path())
    } else {
        None
    });
    let started = utc_ms()?;
    let mut out = runner.run(&tool.path, &args, seconds, &secrets, on_line)?;
    // connect can atomically create a new session: include both generations before ANY persistence.
    match f.secrets() {
        Ok(new) => secrets.0.extend(new.0),
        Err(_) => {
            save(
                &case.join(format!("{label}.secret-refresh-failed.json")),
                &json!({"output_persisted":false,"scope":"Scoped profile invalid after child; raw output withheld"}),
            )?;
            return Err("scoped profile invalid after child; output withheld".into());
        }
    }
    out.stdout = secrets.scrub(&out.stdout);
    out.stderr = secrets.scrub(&out.stderr);
    let mut stdout = create(&case.join(format!("{label}.stdout")))?;
    stdout.write_all(&out.stdout)?;
    stdout.sync_all()?;
    let mut stderr = create(&case.join(format!("{label}.stderr")))?;
    stderr.write_all(&out.stderr)?;
    stderr.sync_all()?;
    save(
        &case.join(format!("{label}.status.json")),
        &json!({"started_utc_ms":started,"finished_utc_ms":utc_ms()?,"success":out.success,"exit_code":out.exit_code,"signal":out.signal,"timed_out":out.timed_out,"stdout_sha256":sha256(&out.stdout),"stderr_sha256":sha256(&out.stderr),"argv_retained":false}),
    )?;
    Ok(out)
}
pub fn call(
    runner: &mut dyn Runner,
    f: &Fixture,
    tool: &Pinned,
    args: Vec<String>,
    case: &Path,
    label: &str,
) -> Result<Value> {
    let out = execute(runner, f, tool, args, 30, case, label, &mut |_| Ok(()))?;
    ensure(out.success, "tool failed; private stdout/stderr retained")?;
    Ok(serde_json::from_slice(&out.stdout)?)
}
