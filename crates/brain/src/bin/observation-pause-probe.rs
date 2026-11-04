//! Operator-only transport diagnostic. No Docker control enters bot containers.
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn docker(args: &[&str]) -> Result<()> {
    if !Command::new("docker")
        .args(args)
        .stdout(Stdio::null())
        .status()?
        .success()
    {
        return Err("Docker diagnostic operation failed".into());
    }
    Ok(())
}
struct Release(String);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = docker(&["unpause", &self.0]);
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("Usage: observation-pause-probe PROXY_CONTAINER EXPECTED_IMAGE_SHA256 MILLISECONDS (1..4000)".into());
    }
    let delay: u64 = args[2].parse()?;
    if args[0] == "--release-after" {
        if delay > 5000 {
            return Err("Watchdog bound exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(delay));
        return docker(&["unpause", &args[1]]);
    }
    let name = &args[0];
    if !name.starts_with("sdk-")
        || !name.ends_with("-egress")
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !(1..=4000).contains(&delay)
    {
        return Err("Named SDK egress proxy and bounded delay required".into());
    }
    let inspected = Command::new("docker")
        .args([
            "inspect",
            name,
            "--format",
            "{{.Image}} {{.State.Running}} {{.State.Paused}}",
        ])
        .output()?;
    if !inspected.status.success()
        || String::from_utf8(inspected.stdout)?.trim() != format!("{} true false", args[1])
    {
        return Err("Proxy image/running/unpaused state mismatch".into());
    }
    // A separate Unix session releases the proxy even if SSH or this process
    // dies. It has no terminal/pipe dependency and is bounded to five seconds.
    let mut watchdog = Command::new(std::env::current_exe()?);
    watchdog
        .args(["--release-after", name, &(delay + 500).to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        watchdog.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let child = watchdog.spawn()?;
    let _release = Release(name.clone());
    docker(&["pause", name])?;
    let started = Instant::now();
    println!(
        "{}",
        serde_json::json!({"event":"transport_pause_started","container":name,"requested_ms":delay,"watchdog_pid":child.id(),"models_called":0,"scope":"operator-declared fault; not an intelligence benchmark"})
    );
    std::thread::sleep(Duration::from_millis(delay));
    docker(&["unpause", name])?;
    // Avoid a harmless second unpause from Drop on the successful path.
    std::mem::forget(_release);
    println!(
        "{}",
        serde_json::json!({"event":"transport_resumed","elapsed_ms":started.elapsed().as_millis(),"container":name})
    );
    Ok(())
}
