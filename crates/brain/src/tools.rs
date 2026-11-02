use crate::{Brain, Result, ensure, model::Choice};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

fn writable(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    ensure(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Write inside the assigned workspace",
    )?;
    let path = root.join(relative);
    let mut ancestor = path.parent().ok_or("File parent required")?;
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or("Parent required")?;
    }
    ensure(
        ancestor.canonicalize()?.starts_with(root.canonicalize()?),
        "Symlink leaves assigned workspace",
    )?;
    ensure(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "Do not overwrite a symlink",
    )?;
    Ok(path)
}
async fn bounded_output(mut stream: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        total += n;
        ensure(total <= 4 * 1024 * 1024, "Terminal output flood stopped")?;
        let keep = n.min(8001usize.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
    }
    Ok(bytes)
}
fn local_shell(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("/bin/sh");
    // Preserve the image's explicit toolchain PATH. A login shell replaced it
    // with /etc/profile's PATH and made the installed Cargo disappear.
    c.args(["-c", command]);
    c
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Also clean up background descendants on timeout, cancellation or
        // ordinary shell exit; a finite tool must not leave a second controller.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
pub async fn terminal(brain: &mut Brain, command: &str) -> Result<Value> {
    ensure(
        !command.is_empty() && command.len() <= 8192,
        "Bounded terminal command required",
    )?;
    let mut c = local_shell(command);
    c.current_dir(&brain.cfg.workspace)
        .env("MC_SDK_PROFILE", &brain.cfg.profile)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "debuginfo")
        .env(
            "CARGO_TARGET_DIR",
            brain
                .cfg
                .cargo_target
                .clone()
                .unwrap_or_else(|| brain.cfg.workspace.join("target")),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(body) = &brain.body {
        c.env("MC_SDK_ARBITER", &body.socket);
    }
    // Leave scheduling room for the independent reactive controller.
    unsafe {
        c.pre_exec(|| {
            if libc::setpriority(libc::PRIO_PROCESS, 0, 10) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c.process_group(0);
    let mut child = c.spawn()?;
    let pid = child.id().ok_or("Process ID unavailable")?;
    let group = ProcessGroup(pid);
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr unavailable")?;
    let out = tokio::spawn(bounded_output(stdout));
    let err = tokio::spawn(bounded_output(stderr));
    let started = std::time::Instant::now();
    let initial_inventory = brain.observed.player["inventory"].clone();
    let deadline = tokio::time::sleep(Duration::from_secs(brain.cfg.tool_timeout_secs));
    tokio::pin!(deadline);
    let mut timed_out = false;
    let status = loop {
        tokio::select! {s=child.wait()=>break s?,_=&mut deadline=>{timed_out=true;unsafe{libc::kill(-(pid as i32),libc::SIGKILL);};let _=child.kill().await;break child.wait().await?;},_=tokio::time::sleep(Duration::from_secs(1))=>{if let Err(e)=brain.refresh().await{brain.journal.event("observation_error",json!({"error":e.to_string()}))?;}}}
    };
    drop(group);
    // Bound inherited open pipes even if a subprocess detached from its parent.
    let stdout = tokio::time::timeout(Duration::from_secs(2), out).await???;
    let stderr = tokio::time::timeout(Duration::from_secs(2), err).await???;
    brain.refresh().await?;
    Ok(
        json!({"exit_code":status.code(),"success":status.success()&&!timed_out,"timed_out":timed_out,"elapsed_ms":started.elapsed().as_millis(),"inventory_changed":initial_inventory!=brain.observed.player["inventory"],"output_truncated":stdout.len()>8000||stderr.len()>8000,"stdout":String::from_utf8_lossy(&stdout[..stdout.len().min(8000)]),"stderr":String::from_utf8_lossy(&stderr[..stderr.len().min(8000)])}),
    )
}
pub async fn execute(brain: &mut Brain, choice: &Choice) -> Result<Value> {
    match choice.tool.as_str() {
        "develop" => {
            brain.journal.set("development_request", &choice.input)?;
            Ok(json!({"next_reasoning_model":"gpt-6.1-sol","request":choice.input}))
        }
        "observe" => {
            brain.refresh().await?;
            Ok(crate::projection(&brain.observed))
        }
        "sdk" => brain.act(serde_json::from_str(&choice.input)?).await,
        "status" => Ok(serde_json::to_value(
            brain.client.action_status(&choice.input).await?,
        )?),
        "reconcile" => brain.settle(&choice.input).await,
        "read" => {
            let path = Path::new(&choice.input);
            let path = if path.is_absolute() {
                path.to_owned()
            } else {
                brain.cfg.workspace.join(path)
            };
            let m = fs::metadata(&path)?;
            ensure(m.is_file() && m.len() <= 65536, "Read a bounded UTF-8 file")?;
            Ok(json!({"path":choice.input,"content":fs::read_to_string(path)?}))
        }
        "write" => {
            ensure(
                choice.input.len() <= 32768 && !choice.path.is_empty(),
                "Bounded source edit and path required",
            )?;
            let path = writable(&brain.cfg.workspace, &choice.path)?;
            fs::create_dir_all(path.parent().ok_or("Parent required")?)?;
            let before = fs::read(&path).ok().map(|b| crate::digest(&b));
            fs::write(&path, &choice.input)?;
            Ok(
                json!({"path":choice.path,"before":before,"sha256":crate::digest(choice.input.as_bytes()),"bytes":choice.input.len()}),
            )
        }
        "list" => {
            let path = if choice.input.is_empty() {
                brain.cfg.workspace.clone()
            } else {
                brain.cfg.workspace.join(&choice.input)
            };
            let mut names = Vec::new();
            for entry in fs::read_dir(path)?.take(128) {
                let entry = entry?;
                names.push(json!({"name":entry.file_name().to_string_lossy(),"directory":entry.file_type()?.is_dir()}));
            }
            Ok(json!(names))
        }
        "search" => Ok(
            json!({"memory":brain.journal.recall(&choice.input)?,"history":brain.journal.history(&choice.input)?}),
        ),
        "terminal" => terminal(brain, &choice.input).await,
        "remember" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Memory {
                kind: String,
                text: String,
            }
            let m: Memory = serde_json::from_str(&choice.input)?;
            brain.journal.remember(&m.kind, &m.text)?;
            Ok(
                json!({"stored":true,"kind":m.kind,"authority":"source-labelled memory; not native proof"}),
            )
        }
        "goal" => {
            ensure(
                !choice.input.trim().is_empty() && choice.input.len() <= 2048,
                "Bounded goal required",
            )?;
            brain.goal = choice.input.clone();
            brain.journal.set("goal", &brain.goal)?;
            Ok(json!({"goal":brain.goal}))
        }
        "complete" => {
            brain.refresh().await?;
            brain.journal.event("model_completion_claim",json!({"claim":choice.input,"observed":crate::projection(&brain.observed),"externally_verified":false}))?;
            Ok(
                json!({"recorded":"model claim","native_observation":crate::projection(&brain.observed),"benchmark_pass_granted":false}),
            )
        }
        _ => Err("Unknown tool".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accidental_write_escape_is_refused() {
        let t = tempfile::tempdir().unwrap();
        assert!(writable(t.path(), "../outside.rs").is_err());
        assert!(writable(t.path(), "/etc/test").is_err());
        assert!(writable(t.path(), "src/main.rs").is_ok());
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), t.path().join("escape")).unwrap();
        assert!(writable(t.path(), "escape/file.rs").is_err());
    }
    #[tokio::test]
    async fn tool_output_is_bounded() {
        let input = std::io::Cursor::new(vec![b'x'; 20000]);
        let bytes = bounded_output(input).await.unwrap();
        assert_eq!(bytes.len(), 8001);
        assert!(bounded_output(tokio::io::repeat(b'x')).await.is_err());
    }
    #[tokio::test]
    async fn terminal_preserves_the_images_explicit_toolchain_path() {
        let t = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/bin/echo", t.path().join("image-toolchain-probe")).unwrap();
        let result = local_shell("image-toolchain-probe ready")
            .env("PATH", t.path())
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "toolchain PATH probe failed: status={}, stderr={}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"ready\n");
    }
}
