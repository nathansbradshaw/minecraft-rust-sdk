//! Collect pinned dependency license declarations/texts for source/binary redistribution.
use super::*;
const MLUA_SYS_LICENSE_ORIGIN: &str =
    "https://github.com/mlua-rs/mlua/blob/a186d461c7772aed4cb0a9fa1d1899121c6450e7/LICENSE";

/// This exact published crate omits its repository's MIT text. Do not generalize
/// the exception to another source, version, declaration or dependency.
fn reviewed_license(
    name: &str,
    version: &str,
    declaration: &str,
    source: &str,
) -> Option<&'static str> {
    match (name, version, declaration, source) {
        ("mlua-sys", "0.13.0", "MIT", "registry+https://github.com/rust-lang/crates.io-index") => {
            Some(include_str!("../../../licenses/mlua-sys-0.13.0-MIT.txt"))
        }
        _ => None,
    }
}
pub fn collect(output: &Path) -> Result<()> {
    let metadata = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            "x86_64-unknown-linux-gnu",
        ])
        .output()?;
    if !metadata.status.success() {
        return Err("Cannot resolve pinned dependency license metadata".into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    std::fs::create_dir_all(output)?;
    let mut report = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("Cargo packages missing")?
    {
        if package["source"].is_null() {
            continue;
        }
        let name = package["name"].as_str().ok_or("Package name missing")?;
        let version = package["version"]
            .as_str()
            .ok_or("Package version missing")?;
        let declaration = package["license"]
            .as_str()
            .ok_or_else(|| format!("Review missing license declaration for {name}"))?;
        if declaration.contains("GPL") {
            return Err(format!("Review copyleft dependency before redistribution: {name}").into());
        }
        let root = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("Package manifest missing")?,
        )
        .parent()
        .ok_or("Manifest parent missing")?;
        let destination = output.join(format!("{name}-{version}"));
        std::fs::create_dir_all(&destination)?;
        let mut files = Vec::new();
        let mut reviewed_origin = None;
        copy_texts(root, root, &destination, &mut files, 0)?;
        // lua-src's root LICENSE retains an older Lua copyright range. Include
        // the exact notice carried by the Lua 5.4.9 engine we embed as well.
        if (name, version, declaration, package["source"].as_str())
            == (
                "lua-src",
                "551.0.2",
                "MIT",
                Some("registry+https://github.com/rust-lang/crates.io-index"),
            )
        {
            let filename = "Lua-5.4.9-MIT.txt";
            std::fs::write(
                destination.join(filename),
                include_str!("../../../licenses/Lua-5.4.9-MIT.txt"),
            )?;
            files.push(filename.to_owned());
            reviewed_origin = Some(
                "https://github.com/mlua-rs/lua-src-rs/blob/3c65a4674fd25a89fb924538ce7fe1c89c64a71b/lua-5.4.9/lua.h",
            );
        }
        if let Some(explicit) = package["license_file"].as_str() {
            let source = root.join(explicit);
            if source.is_file() {
                let file = source.file_name().ok_or("Invalid license filename")?;
                std::fs::copy(&source, destination.join(file))?;
                let file = file.to_string_lossy().into_owned();
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        if files.is_empty()
            && let Some(text) = reviewed_license(
                name,
                version,
                declaration,
                package["source"].as_str().ok_or("Package source missing")?,
            )
        {
            let filename = "LICENSE.upstream-reviewed.txt";
            std::fs::write(destination.join(filename), text)?;
            files.push(filename.to_owned());
            reviewed_origin = Some(MLUA_SYS_LICENSE_ORIGIN);
        }
        if files.is_empty() {
            return Err(format!("Review missing packaged license text for {name}").into());
        }
        report.push(serde_json::json!({"name":name,"version":version,"license":declaration,"source":package["source"],"files":files,"reviewed_upstream_license":reviewed_origin}));
    }
    report.sort_by_key(|p| format!("{} {}", p["name"], p["version"]));
    std::fs::write(
        output.join("DEPENDENCIES.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "Recorded SPDX declarations and packaged license texts for {} pinned dependencies",
        report.len()
    );
    Ok(())
}

fn copy_texts(
    root: &Path,
    current: &Path,
    destination: &Path,
    files: &mut Vec<String>,
    depth: usize,
) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let selected = depth > 0
            || name.starts_with("license")
            || name.starts_with("notice")
            || name.starts_with("copying")
            || name.starts_with("copyright")
            || name == "unlicense";
        if !selected {
            continue;
        }
        let source = entry.path();
        let relative = source.strip_prefix(root)?;
        if kind.is_dir() {
            copy_texts(root, &source, destination, files, depth + 1)?;
        } else if kind.is_file() {
            if entry.metadata()?.len() > 1024 * 1024 {
                return Err("Dependency license file exceeds text budget".into());
            }
            let target = destination.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source, &target)?;
            files.push(relative.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_packaged_text_exception_is_pinned_and_preserves_the_guard() {
        let source = "registry+https://github.com/rust-lang/crates.io-index";
        let text = reviewed_license("mlua-sys", "0.13.0", "MIT", source).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(text.as_bytes())),
            "63b61a30d3e3abcfcb1bda4b99f4eea9fdb7d6ab05fbc513461fef6cf89baf29"
        );
        for (name, version, license, origin) in [
            ("mlua-sys", "0.13.1", "MIT", source),
            ("mlua-sys", "0.13.0", "Apache-2.0", source),
            ("other-crate", "0.13.0", "MIT", source),
            ("mlua-sys", "0.13.0", "MIT", "git+https://example.invalid/"),
        ] {
            assert!(reviewed_license(name, version, license, origin).is_none());
        }
    }
}
