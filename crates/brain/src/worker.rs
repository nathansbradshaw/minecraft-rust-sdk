//! Decision processes never own the resident body, session or controller lock.
use crate::{Config, Journal, Result, digest, ensure, model, private_read, private_write};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const VERSION: u32 = 1;
const LIMIT: usize = 65536;
const BINARY_LIMIT: usize = 32 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    request_id: String,
    context: Value,
    endpoint: String,
    key_file: PathBuf,
    use_sol: bool,
    development: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u32,
    request_id: String,
    answer: Option<(model::Choice, Value)>,
    error: Option<Failure>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Failure {
    message: String,
    pub retry_at: Option<i64>,
    pub daily_reset: Option<i64>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "decision_worker: {}", self.message)
    }
}
impl std::error::Error for Failure {}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    executable: PathBuf,
    sha256: String,
}
fn manifest(cfg: &Config) -> PathBuf {
    cfg.state.join("decision-worker.json")
}
pub(crate) fn revision(cfg: &Config) -> Result<String> {
    if cfg.decision_worker.is_none() {
        return Ok("disabled".into());
    }
    if fs::symlink_metadata(manifest(cfg)).is_ok() {
        Ok(match private_read(&manifest(cfg), 16384) {
            Ok(bytes) => digest(&bytes),
            Err(error) => format!(
                "invalid:{error}:{:?}",
                fs::symlink_metadata(manifest(cfg))
                    .ok()
                    .map(|m| (m.len(), m.modified().ok()))
            ),
        })
    } else {
        Ok(format!("initial:{:?}", cfg.decision_worker))
    }
}
fn binary(path: &Path) -> Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    ensure(
        path.is_absolute()
            && m.is_file()
            && !m.file_type().is_symlink()
            && (m.uid() == unsafe { libc::geteuid() } || m.uid() == 0)
            && m.mode() & 0o022 == 0
            && m.mode() & 0o111 != 0
            && m.len() <= BINARY_LIMIT as u64,
        "Owned bounded executable without symlinks/shared write permissions required",
    )?;
    let bytes = fs::read(path)?;
    ensure(bytes.len() <= BINARY_LIMIT, "Worker binary exceeds budget")?;
    Ok(bytes)
}
fn selection(cfg: &Config) -> Result<Selection> {
    let s = if manifest(cfg).try_exists()? {
        serde_json::from_slice::<Selection>(&private_read(&manifest(cfg), 16384)?)?
    } else {
        let executable = cfg
            .decision_worker
            .clone()
            .ok_or("No decision worker configured")?;
        Selection {
            version: VERSION,
            sha256: digest(&binary(&executable)?),
            executable,
        }
    };
    ensure(
        s.version == VERSION && digest(&binary(&s.executable)?) == s.sha256,
        "Worker version/hash mismatch",
    )?;
    Ok(s)
}
/// Activate at a settled decision boundary, without opening a native connection.
pub fn replace(cfg: &Config, executable: &Path) -> Result<Value> {
    ensure(
        cfg.decision_worker.is_some(),
        "Resident decision-worker mode required",
    )?;
    let bytes = binary(executable)?;
    let hash = digest(&bytes);
    if manifest(cfg).try_exists()? && selection(cfg).is_ok_and(|s| s.sha256 == hash) {
        return Ok(
            json!({"selected":false,"reason":"already selected","sha256":hash,"native_connections_opened":0}),
        );
    }
    let directory = cfg.state.join("decision-workers");
    fs::create_dir_all(&directory)?;
    ensure(
        !fs::symlink_metadata(&directory)?.file_type().is_symlink(),
        "Worker cache must not be a symlink",
    )?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let path = directory.join(format!("{hash}.bin"));
    if path.try_exists()? {
        ensure(
            digest(&binary(&path)?) == hash,
            "Existing worker snapshot corrupt",
        )?;
    } else {
        private_write(&path, &bytes)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    private_write(
        &manifest(cfg),
        &serde_json::to_vec(&Selection {
            version: VERSION,
            executable: path,
            sha256: hash.clone(),
        })?,
    )?;
    Ok(
        json!({"selected":true,"sha256":hash,"activation":"next settled decision; old pending output discarded","native_connections_opened":0,"controller_lock_acquired":false}),
    )
}
pub async fn serve() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure(bytes.len() <= LIMIT, "Worker request exceeds budget")?;
    let r: Request = serde_json::from_slice(&bytes)?;
    ensure(
        r.version == VERSION && serde_json::to_vec(&r.context)?.len() <= 32768,
        "Unsupported worker protocol/context budget",
    )?;
    let result = match model::Model::new(&r.endpoint, &r.key_file) {
        Ok(m) => m.choose_for(&r.context, r.use_sol, r.development).await,
        Err(e) => Err(e),
    };
    let response = match result {
        Ok(answer) => Response {
            version: VERSION,
            request_id: r.request_id,
            answer: Some(answer),
            error: None,
        },
        Err(e) => Response {
            version: VERSION,
            request_id: r.request_id,
            answer: None,
            error: Some(Failure {
                message: e.to_string(),
                retry_at: model::retry_at(e.as_ref()),
                daily_reset: model::daily_reset(e.as_ref()),
            }),
        },
    };
    let encoded = serde_json::to_string(&response)?;
    ensure(encoded.len() <= LIMIT, "Worker response exceeds budget")?;
    println!("{encoded}");
    Ok(())
}
pub(crate) fn initialize(cfg: &Config) -> Result<Option<Value>> {
    match &cfg.decision_worker {
        Some(path) if !manifest(cfg).try_exists()? => replace(cfg, path).map(Some),
        _ => Ok(None),
    }
}
pub async fn choose(
    cfg: &Config,
    journal: &Journal,
    context: &Value,
    use_sol: bool,
    development: bool,
) -> Result<(model::Choice, Value)> {
    ensure(
        serde_json::to_vec(context)?.len() <= 32768,
        "Worker context exceeds budget",
    )?;
    let selected = selection(cfg)?;
    let id = digest(
        json!([context, use_sol, development, selected.sha256])
            .to_string()
            .as_bytes(),
    );
    let request_bytes = serde_json::to_vec(&Request {
        version: VERSION,
        request_id: id.clone(),
        context: context.clone(),
        endpoint: cfg.model_endpoint.clone(),
        key_file: cfg.model_key.clone(),
        use_sol,
        development,
    })?;
    ensure(
        request_bytes.len() <= LIMIT,
        "Worker request exceeds budget",
    )?;
    let mut child = tokio::process::Command::new(&selected.executable)
        .arg("decision-worker")
        .current_dir(&cfg.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().ok_or("Worker PID unavailable")?;
    let _group = crate::tools::ProcessGroup(pid);
    journal.event("decision_worker_started",json!({"pid":pid,"sha256":selected.sha256,"request_id":id,"protocol":VERSION,"body_owner":"resident engine","native_connections_opened":0}))?;
    let mut stdin = child.stdin.take().ok_or("Worker stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Worker stdout unavailable")?;
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(std::time::Duration::from_secs(260), async {
        stdin.write_all(&request_bytes).await?;
        drop(stdin);
        stdout
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure(bytes.len() <= LIMIT, "Worker response exceeds budget")?;
        ensure(
            child.wait().await?.success(),
            "Decision worker exited unsuccessfully; resident body remains active",
        )?;
        let r: Response = serde_json::from_slice(&bytes)?;
        ensure(
            r.version == VERSION && r.request_id == id && r.answer.is_some() != r.error.is_some(),
            "Worker response version/identity/result mismatch",
        )?;
        if let Some(e) = r.error {
            let now = crate::now();
            ensure(
                e.message.len() <= 1024
                    && e.retry_at
                        .is_none_or(|at| at > now && at <= now.saturating_add(86400))
                    && e.daily_reset.is_none_or(|at| Some(at) == e.retry_at),
                "Invalid bounded worker failure metadata",
            )?;
            return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
        }
        let (choice, mut usage) = r.answer.ok_or("Worker answer missing")?;
        model::validate_choice(&choice)?;
        ensure(usage.is_object(), "Worker usage must be an object")?;
        usage["decision_worker"] = json!({"sha256":selected.sha256,"pid":pid,"protocol":VERSION});
        Ok((choice, usage))
    })
    .await
    .map_err(|_| "Decision worker timeout; resident body remains active")?;
    journal.event(
        "decision_worker_finished",
        json!({"pid":pid,"request_id":id,"success":result.is_ok(),"body_owner":"resident engine"}),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_selection_revision_is_observable_without_stopping_the_resident() {
        let (_dir, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
        brain.cfg.decision_worker = Some(brain.cfg.state.join("missing"));
        private_write(&manifest(&brain.cfg), b"invalid manifest").unwrap();
        assert!(revision(&brain.cfg).is_ok());
        assert!(selection(&brain.cfg).is_err());
        let error = Failure {
            message: "quota".into(),
            retry_at: Some(crate::now() + 60),
            daily_reset: Some(crate::now() + 60),
        };
        assert_eq!(model::daily_reset(&error), error.daily_reset);
        brain.cfg.decision_worker = None;
        assert_eq!(revision(&brain.cfg).unwrap(), "disabled");
    }
}
