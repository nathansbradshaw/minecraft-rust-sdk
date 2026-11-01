//! Operator-only Unix control socket. Never routed through the public HTTP router or MCP.
use crate::*;
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    ImportLegacy {
        label: String,
        player_uuid: String,
        player_name: String,
        journal: PathBuf,
    },
    Revoke {
        bot_id: String,
    },
    RecoverKey {
        bot_id: String,
    },
    ReserveName {
        name: String,
    },
}
pub struct OperatorListener {
    listener: UnixListener,
    path: PathBuf,
}
impl Drop for OperatorListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn bind(path: PathBuf) -> std::io::Result<OperatorListener> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    if path.exists() {
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.file_type().is_socket()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(std::io::Error::other(
                "Unsafe existing operator socket path",
            ));
        }
        match std::os::unix::net::UnixStream::connect(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(&path)?
            }
            _ => return Err(std::io::Error::other("Operator socket is already active")),
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(OperatorListener { listener, path })
}
pub async fn serve(gateway: Shared, server: OperatorListener) -> std::io::Result<()> {
    let permits = Arc::new(tokio::sync::Semaphore::new(8));
    loop {
        let (mut socket, _) = server.listener.accept().await?;
        if socket.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let g = gateway.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                let mut bytes = Vec::new();
                loop {
                    let byte = socket.read_u8().await?;
                    bytes.push(byte);
                    if bytes.len() > 16384 {
                        return Err(std::io::Error::other("Operator request too large"));
                    }
                    if byte == b'\n' {
                        break;
                    }
                }
                Ok::<_, std::io::Error>(bytes)
            })
            .await;
            let Ok(Ok(bytes)) = result else {
                return;
            };
            let result = match serde_json::from_slice::<Request>(&bytes) {
                Ok(request) => execute(&g, request).await,
                Err(_) => Err(ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Invalid operator request",
                )),
            };
            let value = match result {
                Ok(v) => serde_json::json!({"ok":v}),
                Err(e) => serde_json::json!({"error":e}),
            };
            if let Ok(mut bytes) = serde_json::to_vec(&value) {
                bytes.push(b'\n');
                let _ = socket.write_all(&bytes).await;
            }
        });
    }
}
async fn execute(g: &Shared, request: Request) -> Result<serde_json::Value> {
    match request {
        Request::ImportLegacy {
            label,
            player_uuid,
            player_name,
            journal,
        } => serde_json::to_value(g.db()?.import_legacy(
            &label,
            &player_uuid,
            &player_name,
            &journal,
        )?)
        .map_err(|_| ApiError::new(ErrorCode::Internal, "Cannot encode imported credential")),
        Request::ReserveName { name } => {
            g.db()?.reserve(&name)?;
            Ok(serde_json::json!({"reserved":true}))
        }
        Request::RecoverKey { bot_id } => {
            let _guard = g.lane(&bot_id)?.lock_owned().await;
            serde_json::to_value(g.db()?.recover(&bot_id)?)
                .map_err(|_| ApiError::new(ErrorCode::Internal, "Cannot encode credential"))
        }
        Request::Revoke { bot_id } => {
            let _guard = g.lane(&bot_id)?.lock_owned().await;
            let actor = g.db()?.operator_actor(&bot_id)?;
            g.db()?.revoke(&bot_id)?;
            match g.call(&actor, AdapterOperation::Disconnect).await? {
                AdapterResponse::Disconnected => {
                    g.db()?.disconnect(&bot_id)?;
                    Ok(serde_json::json!({"revoked":true,"disconnected":true}))
                }
                _ => Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Key revoked but native disconnect unconfirmed",
                )),
            }
        }
    }
}
