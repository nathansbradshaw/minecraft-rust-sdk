//! Local operator client. Responses (including recovered keys) go only to a private new file.
use std::{
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "Usage: minecraft-sdk-admin SOCKET REQUEST.json NEW_PRIVATE_RESPONSE.json".into(),
        );
    }
    let path = Path::new(&args[0]);
    let meta = std::fs::symlink_metadata(path)?;
    if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("Operator socket must be private and owned by you".into());
    }
    let request = std::fs::read(&args[1])?;
    if request.len() > 16384 {
        return Err("Operator request too large".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&request)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args[2])?;
    let mut socket = UnixStream::connect(path).await?;
    if socket.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err("Unexpected operator server identity".into());
    }
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    socket.write_all(&bytes).await?;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(40), async {
        let mut reply = Vec::new();
        loop {
            let b = socket.read_u8().await?;
            reply.push(b);
            if reply.len() > 16384 {
                return Err(std::io::Error::other("Operator reply too large"));
            }
            if b == b'\n' {
                break;
            }
        }
        Ok::<_, std::io::Error>(reply)
    })
    .await??;
    output.write_all(&reply)?;
    output.sync_all()?;
    println!(
        "Private operator response saved to {}",
        Path::new(&args[2]).display()
    );
    Ok(())
}
