use minecraft_sdk_core::*;
use serde::Deserialize;
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterConfig {
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}
pub struct Adapter {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    failed: bool,
    pub health: Arc<AtomicBool>,
}
impl Adapter {
    pub fn start(config: &AdapterConfig) -> std::io::Result<Self> {
        if !config.executable.is_absolute() {
            return Err(std::io::Error::other("Adapter executable must be absolute"));
        }
        let mut child = Command::new(&config.executable)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        Ok(Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            failed: false,
            health: Arc::new(AtomicBool::new(true)),
        })
    }
    pub async fn request(&mut self, request: &AdapterRequest) -> Result<AdapterResponse> {
        if self.failed {
            return Err(ApiError::new(
                ErrorCode::NotReady,
                "Adapter is quarantined; operator restart required",
            ));
        }
        let result = tokio::time::timeout(Duration::from_secs(30), self.exchange(request)).await;
        match result {
            Ok(Ok(response)) => Ok(response),
            _ => {
                self.failed = true;
                self.health.store(false, Ordering::Release);
                let _ = self.child.kill().await;
                Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Adapter communication failed; do not replay mutations",
                ))
            }
        }
    }
    async fn exchange(&mut self, request: &AdapterRequest) -> std::io::Result<AdapterResponse> {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let mut line = Vec::new();
        // Bound before allocation, including a hostile/malfunctioning trusted plugin.
        loop {
            let buffer = self.output.fill_buf().await?;
            if buffer.is_empty() {
                return Err(std::io::Error::other("Adapter closed"));
            }
            let n = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(buffer.len());
            if line.len() + n > 1_048_576 {
                return Err(std::io::Error::other("Adapter response too large"));
            }
            let end = buffer[n - 1] == b'\n';
            line.extend_from_slice(&buffer[..n]);
            self.output.consume(n);
            if end {
                break;
            }
        }
        serde_json::from_slice(&line).map_err(std::io::Error::other)
    }
}
