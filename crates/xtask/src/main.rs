//! Source-only publication audit, independent of the private parent history.
use sha2::{Digest, Sha256};
mod licenses;
use std::{collections::HashSet, path::Path, process::Command};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn publication_budget(tree_bytes: usize, history_bytes: usize) -> Result<()> {
    if tree_bytes > 8 * 1024 * 1024 {
        return Err(
            format!("Current source tree exceeds reviewed budget: {tree_bytes} bytes").into(),
        );
    }
    // Source revisions accumulate while the current reusable project stays
    // small. Every historical blob still passes the same path/size/secret scan.
    // On 2026-11-13 retained reviewed source reached 25,297,539 bytes across
    // local refs, exceeding 24 MiB. Preserve recovery history with the explicitly
    // approved 32 MiB ceiling. Every blob still passes path/file/secret checks;
    // the 8 MiB current-tree and 2 MiB individual-file caps remain unchanged.
    if history_bytes > 32 * 1024 * 1024 {
        return Err(format!(
            "Source publication history exceeds reviewed budget: {history_bytes} bytes"
        )
        .into());
    }
    Ok(())
}
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!("Git source audit failed: {}", args[0]).into());
    }
    Ok(output.stdout)
}
fn allowed(path: &str) -> bool {
    if path.split('/').any(|s| {
        s == ".." || s.starts_with('.') && !matches!(s, ".github" | ".gitignore" | ".dockerignore")
    }) {
        return false;
    }
    if [
        "Cargo.toml",
        "Cargo.lock",
        "AGENTS.md",
        "LICENSE",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
        ".gitignore",
        ".dockerignore",
    ]
    .contains(&path)
    {
        return true;
    }
    if path == ".github/workflows/ci.yml" {
        return true;
    }
    if path.starts_with("crates/") {
        return path.ends_with(".rs")
            || path.ends_with("/Cargo.toml")
            || matches!(
                path,
                "crates/brain/README.md"
                    | "crates/brain/Cargo.container.toml"
                    | "crates/brain/src/system.md"
                    | "crates/brain/src/conversation.md"
                    | "crates/brain/src/sdk.md"
            )
            || path == "crates/java-1-21-1/packet-ids.json";
    }
    if path.starts_with("docs/") {
        return path.ends_with(".md");
    }
    if path.starts_with("examples/") {
        return path.ends_with(".rs")
            || path == "examples/lua/behavior.lua"
            || path == "examples/coding-bot/Cargo.toml"
            || path == "examples/coding-bot/Cargo.lock"
            || path == "examples/coding-bot/README.md";
    }
    if path.starts_with("licenses/") {
        return path.ends_with(".txt");
    }
    if path.starts_with("deploy/") {
        return path.ends_with(".json")
            || path.ends_with(".service")
            || path == "deploy/Dockerfile";
    }
    if path.starts_with("server-neoforge/src/") {
        return path.ends_with(".java") || path.ends_with(".json") || path.ends_with(".toml");
    }
    [
        "server-neoforge/settings.gradle",
        "server-neoforge/build.gradle",
        "server-neoforge/gradle.properties",
        "server-neoforge/gradlew",
        "server-neoforge/gradle/wrapper/gradle-wrapper.properties",
        "server-neoforge/gradle/wrapper/gradle-wrapper.jar",
    ]
    .contains(&path)
}
fn secret(bytes: &[u8]) -> bool {
    let header = [b"-----BEGIN ".as_slice(), b"PRIVATE KEY-----"].concat();
    if bytes.windows(header.len()).any(|w| w == header) {
        return true;
    }
    for prefix in [
        b"ghp_".as_slice(),
        b"gho_",
        b"github_pat_",
        b"sk-proj-",
        b"sk-ant-api",
    ] {
        for (i, w) in bytes.windows(prefix.len()).enumerate() {
            if w == prefix
                && bytes[i + prefix.len()..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || b"_-".contains(b))
                    .count()
                    >= 20
            {
                return true;
            }
        }
    }
    false
}
fn blob(root: &Path, hash: &str, path: &str, seen: &mut HashSet<String>) -> Result<usize> {
    if !allowed(path) {
        return Err(format!("Unreviewed publication path: {path}").into());
    }
    let data = git(root, &["cat-file", "blob", hash])?;
    if data.len() > 2 * 1024 * 1024 {
        return Err(format!("Source file exceeds budget: {path}").into());
    }
    if secret(&data) {
        return Err(format!("Potential secret excluded from publication: {path}").into());
    }
    if path.ends_with(".jar")
        && format!("{:x}", Sha256::digest(&data))
            != "7d3a4ac4de1c32b59bc6a4eb8ecb8e612ccd0cf1ae1e99f66902da64df296172"
    {
        return Err("Unreviewed Gradle wrapper artifact".into());
    }
    if seen.insert(hash.into()) {
        Ok(data.len())
    } else {
        Ok(0)
    }
}
fn audit(root: &Path) -> Result<()> {
    let mut seen = HashSet::new();
    let mut size = 0usize;
    let index = git(root, &["ls-files", "--stage", "-z"])?;
    for record in index.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let text = std::str::from_utf8(record)?;
        let (metadata, path) = text.split_once('\t').ok_or("Invalid index metadata")?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") || fields[2] != "0" {
            return Err("Unmerged, symlink or gitlink source refused".into());
        }
        size += blob(root, fields[1], path, &mut seen)?;
    }
    let tree_size = size;
    publication_budget(tree_size, tree_size)?;
    let history = git(root, &["rev-list", "--objects", "--all"])?;
    for record in String::from_utf8(history)?.lines() {
        let Some((hash, path)) = record.split_once(' ') else {
            continue;
        };
        if git(root, &["cat-file", "-t", hash])? == b"blob\n" {
            size += blob(root, hash, path, &mut seen)?;
        }
    }
    publication_budget(tree_size, size)?;
    for path in [
        "LICENSE",
        "THIRD_PARTY_NOTICES.md",
        "licenses/Gradle-Apache-2.0.txt",
        "licenses/minecraft-data-MIT.txt",
    ] {
        if !root.join(path).is_file() {
            return Err(format!("License provenance missing: {path}").into());
        }
    }
    println!(
        "Source audit passed: current tree {tree_size} bytes; {} unique reviewed blobs, {size} historical bytes; no runtime paths or detected credentials",
        seen.len()
    );
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--licenses") {
        return licenses::collect(Path::new(
            args.get(1).ok_or("License output directory required")?,
        ));
    }
    let root = if args.len() == 2 && !Path::new(&args[0]).is_dir() {
        std::env::current_dir()?
    } else {
        args.first()
            .map(std::path::PathBuf::from)
            .ok_or("Pass the standalone repository path")?
    };
    audit(&root)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_source_revisions_do_not_expand_the_current_tree_budget() {
        assert!(publication_budget(1_213_568, 8_418_253).is_ok());
        assert!(publication_budget(8 * 1024 * 1024 + 1, 9 * 1024 * 1024).is_err());
        assert!(publication_budget(1_753_260, 16_737_705).is_ok());
        assert!(publication_budget(1_753_260, 24 * 1024 * 1024).is_ok());
        assert!(publication_budget(1_753_260, 25_297_539).is_ok());
        assert!(publication_budget(1_753_260, 32 * 1024 * 1024).is_ok());
        assert!(publication_budget(1_213_568, 32 * 1024 * 1024 + 1).is_err());
    }
    #[test]
    fn blocks_runtime_and_private_paths() {
        for p in [
            ".state/pilot.key",
            "world/level.dat",
            "crates/core/../../secret.rs",
            "server-neoforge/run/server.properties",
            "server-neoforge/build/minecraft.jar",
            ".git/objects/private",
            "deploy/ssh.key",
        ] {
            assert!(!allowed(p), "{p}");
        }
    }
    #[test]
    fn catches_tokens_without_echoing_them() {
        let mut s = b"ghp_".to_vec();
        s.extend([b'x'; 40]);
        assert!(secret(&s));
        assert!(!secret(b"A public API field named key is not a secret"));
    }
}
