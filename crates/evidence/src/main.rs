use minecraft_sdk_evidence::{Spec, capture, private_file, sha256};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 || (args[0] != "capture" && args[0] != "reaction-latency") {
        return Err("Usage: mc-sdk-evidence capture GATEWAY.sqlite SPEC.json NEW_REPORT.json | reaction-latency TRACE.jsonl NATIVE_REPORT.json NEW_SUMMARY.json".into());
    }
    let value = if args[0] == "capture" {
        private_file(Path::new(&args[2]), 65536)?;
        let spec: Spec = serde_json::from_slice(&fs::read(&args[2])?)?;
        capture(Path::new(&args[1]), &spec)?
    } else {
        private_file(Path::new(&args[1]), 16 * 1024 * 1024)?;
        private_file(Path::new(&args[2]), 8 * 1024 * 1024)?;
        let native_bytes = fs::read(&args[2])?;
        let mut summary = minecraft_sdk_evidence::latency::summarize(
            &fs::read(&args[1])?,
            &serde_json::from_slice(&native_bytes)?,
        )?;
        summary["native_report_file_sha256"] = serde_json::json!(sha256(&native_bytes));
        summary
    };
    let report = serde_json::to_vec_pretty(&value)?;
    if report.len() > 8 * 1024 * 1024 {
        return Err("Evidence report exceeds budget".into());
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args[3])?;
    output.write_all(&report)?;
    output.sync_all()?;
    println!(
        "{}",
        serde_json::json!({"saved":Path::new(&args[3]).display().to_string(),"sha256":sha256(&report),"certifies_goal_completion":false})
    );
    Ok(())
}
