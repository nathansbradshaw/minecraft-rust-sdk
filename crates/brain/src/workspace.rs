//! Container-local file feedback. Paths never change the tool's working base.
use crate::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
const BUDGET: usize = 5000;
const SDK_REFERENCE: &str = "sdk://reference";
pub(crate) fn reference_index() -> Value {
    let source = include_str!("sdk.md");
    let lines: Vec<_> = source.lines().collect();
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(n, line)| line.starts_with("- ").then_some(n))
        .collect();
    let sections:Vec<_>=starts.iter().enumerate().map(|(n,start)|json!({"start_line":start+1,"end_line":starts.get(n+1).copied().unwrap_or(lines.len()),"title":lines[*start].chars().take(96).collect::<String>()})).collect();
    json!({"view":"sdk_reference_index","path":SDK_REFERENCE,"source_sha256":crate::digest(source.as_bytes()),"source_bytes":source.len(),"sections":sections,"note":"Full SDK documentation remains available through read input JSON {path: sdk://reference, start_line: N, end_line: M}. This is a read-only built-in resource. Read the relevant section, then act; it is not necessary to load the entire manual. Existing connected Profile is in MC_SDK_PROFILE; generated programs must not connect/disconnect the harness character. Use unique action IDs and inspect uncertainty without replay."})
}
fn target(root: &Path, input: &str) -> Result<(PathBuf, PathBuf)> {
    let root = fs::canonicalize(root)?;
    ensure(
        root.to_str().is_some(),
        "Workspace path needs a UTF-8 representation; use terminal Rust byte paths otherwise",
    )?;
    let path = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    Ok((root, path))
}
pub fn read(root: &Path, input: &str) -> Result<Value> {
    if input == SDK_REFERENCE {
        return read_range(
            root,
            &json!({"path":SDK_REFERENCE,"start_line":1,"end_line":200}).to_string(),
        );
    }
    if input.trim_start().starts_with('{') {
        return read_range(root, input);
    }
    let (root, path) = target(root, input)?;
    let attempt = || -> Result<Value> {
        let file = fs::File::open(&path)?;
        let meta = file.metadata()?;
        ensure(
            meta.is_file() && meta.len() <= 65536,
            "Read requires a regular UTF-8 file up to65536 bytes; use bounded terminal reads for larger files",
        )?;
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        ensure(bytes.len() <= 65536, "File grew beyond the read limit")?;
        let mut content = String::from_utf8(bytes)?;
        let total = content.len();
        let hash = crate::digest(content.as_bytes());
        loop {
            let answer = json!({"view":"file","workspace":root,"requested_path":input,"path":path,"source_sha256":hash,"source_bytes":total,"content_complete":content.len()==total,"content":content,"note":"Relative paths use workspace. For missing source use read input JSON {path,start_line,end_line} (1-based inclusive), not another identical full-file read. Use edit with an exact old/new fragment for a local correction."});
            if serde_json::to_vec(&answer)?.len() <= BUDGET {
                return Ok(answer);
            }
            ensure(
                !content.is_empty(),
                "File path metadata exceeds the view budget; use terminal tools",
            )?;
            let mut end = content.len() / 2;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
        }
    };
    attempt().map_err(|error| {
        format!(
            "Read {} failed (relative paths use workspace {}): {error}",
            path.display(),
            root.display()
        )
        .into()
    })
}

fn read_range(root: &Path, input: &str) -> Result<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Range {
        path: String,
        start_line: usize,
        end_line: usize,
    }
    let range: Range = serde_json::from_str(input)?;
    ensure(
        range.start_line > 0
            && range.end_line >= range.start_line
            && range.end_line - range.start_line < 200,
        "Read 1–200 lines using 1-based inclusive start_line/end_line",
    )?;
    let (path, source) = if range.path == SDK_REFERENCE {
        (json!(SDK_REFERENCE), include_str!("sdk.md").to_owned())
    } else {
        let (_, path) = target(root, &range.path)?;
        let file = fs::File::open(&path)?;
        ensure(
            file.metadata()?.is_file() && file.metadata()?.len() <= 65536,
            "Bounded regular source file required",
        )?;
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        ensure(bytes.len() <= 65536, "File grew beyond read limit")?;
        (json!(path), String::from_utf8(bytes)?)
    };
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    ensure(
        range.start_line <= lines.len(),
        "Start line is beyond the current file; inspect source hash and line count",
    )?;
    let end = range.end_line.min(lines.len());
    let mut content = String::new();
    let mut returned_end = range.start_line - 1;
    loop {
        let answer = json!({"view":"file_range","path":path,"source_sha256":crate::digest(source.as_bytes()),"source_bytes":source.len(),"total_lines":lines.len(),"start_line":range.start_line,"end_line":returned_end,"requested_end_line":range.end_line,"range_complete":returned_end==end,"next_line":if returned_end<end {Some(returned_end+1)} else {None},"content":content,"note":"Exact source lines for this hash; ranges from a changed hash must be read again. Use edit with an unambiguous old/new fragment, then run the changed program."});
        if returned_end == end {
            return Ok(answer);
        }
        let line = lines[returned_end];
        let mut next = answer.clone();
        next["content"] = json!(format!("{content}{line}"));
        if next.to_string().len() + 64 > BUDGET {
            ensure(
                returned_end >= range.start_line,
                "One source line exceeds the view budget; use a bounded terminal character slice",
            )?;
            return Ok(answer);
        }
        content.push_str(line);
        returned_end += 1;
    }
}
pub fn list(root: &Path, input: &str) -> Result<Value> {
    let (root, path) = target(root, if input.is_empty() { "." } else { input })?;
    let path = fs::canonicalize(&path).map_err(|e| {
        format!(
            "List {} failed (workspace {}): {e}",
            path.display(),
            root.display()
        )
    })?;
    ensure(
        path.to_str().is_some(),
        "Directory path needs a UTF-8 representation; use terminal Rust byte paths otherwise",
    )?;
    let mut scanned = fs::read_dir(&path)?
        .take(129)
        .collect::<std::io::Result<Vec<_>>>()?;
    scanned.sort_by_key(|entry| entry.file_name());
    let mut complete = scanned.len() <= 128;
    scanned.truncate(128);
    let mut answer = json!({"view":"files","workspace":root,"directory":path,"entries":[],"returned":0,"complete":false,"entry_limit":128,"byte_limit":BUDGET,"note":"Nonrecursive listing. Read/list using an entry's path; paths remain relative to workspace otherwise. A truncated listing is not proof of absence; use bounded terminal searches for more entries."});
    ensure(
        serde_json::to_vec(&answer)?.len() <= BUDGET,
        "Directory path metadata exceeds the view budget",
    )?;
    for entry in scanned {
        let kind = entry.file_type()?;
        let entry_path = entry.path();
        let usable = entry_path.to_str();
        let relative = entry_path.strip_prefix(&root).ok().and_then(Path::to_str);
        let row = json!({"name":entry.file_name().to_string_lossy(),"path":usable,"relative_path":relative,"directory":kind.is_dir(),"symlink":kind.is_symlink(),"path_encoding":if usable.is_some(){"utf-8"}else{"unsupported; use terminal Rust byte paths"}});
        answer["entries"].as_array_mut().unwrap().push(row);
        if serde_json::to_vec(&answer)?.len() > BUDGET - 32 {
            answer["entries"].as_array_mut().unwrap().pop();
            complete = false;
            break;
        }
    }
    answer["returned"] = json!(answer["entries"].as_array().unwrap().len());
    answer["complete"] = json!(complete);
    ensure(
        serde_json::to_vec(&answer)?.len() <= BUDGET,
        "Directory listing metadata exceeds the view budget",
    )?;
    Ok(answer)
}

/// A bounded persistent source-location hint, never a claim of game progress.
pub fn manifest_hint(root: &Path) -> Value {
    let source = match read(root, "Cargo.toml") {
        Ok(value) => value,
        Err(error) => {
            return json!({"available":false,"error":error.to_string().chars().take(240).collect::<String>()});
        }
    };
    let original = source["content"].as_str().unwrap_or_default();
    let mut content = original.to_owned();
    loop {
        let value = json!({"available":true,"path":source["path"],"source_bytes":source["source_bytes"],"content_complete":source["content_complete"]==true&&content.len()==original.len(),"content":content});
        if value.to_string().len() <= 1024 {
            return value;
        }
        if content.is_empty() {
            return json!({"available":false,"error":"Manifest path exceeds context hint budget; use file tools"});
        }
        let mut end = content.len() / 2;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_manual_is_retrievable_without_a_checkout_or_host_access() {
        let index = reference_index();
        assert!(index.to_string().len() < 4000);
        for section in index["sections"].as_array().unwrap() {
            let result=read(Path::new("/does-not-exist"),&json!({"path":SDK_REFERENCE,"start_line":section["start_line"],"end_line":section["end_line"]}).to_string()).unwrap();
            assert!(
                result["content"]
                    .as_str()
                    .unwrap()
                    .starts_with(section["title"].as_str().unwrap())
            );
            assert_eq!(result["source_sha256"], index["source_sha256"]);
        }
    }
    #[test]
    fn ranged_source_reads_reach_omitted_lines_and_bind_pages_to_source_hash() {
        let dir = tempfile::tempdir().unwrap();
        let source = (1..=180)
            .map(|n| format!("line{n}: {}\n", "λ".repeat(30)))
            .collect::<String>();
        fs::write(dir.path().join("source.rs"), &source).unwrap();
        let first = read(dir.path(), "source.rs").unwrap();
        assert_eq!(first["content_complete"], false);
        let mut line = 100;
        let mut complete = String::new();
        loop {
            let page = read(
                dir.path(),
                &json!({"path":"source.rs","start_line":line,"end_line":180}).to_string(),
            )
            .unwrap();
            assert_eq!(page["source_sha256"], crate::digest(source.as_bytes()));
            assert!(page.to_string().len() <= BUDGET);
            complete.push_str(page["content"].as_str().unwrap());
            if page["range_complete"] == true {
                break;
            }
            let next = page["next_line"].as_u64().unwrap();
            assert!(next > line);
            line = next;
        }
        assert_eq!(
            complete,
            source.split_inclusive('\n').skip(99).collect::<String>()
        );
        assert!(
            read(
                dir.path(),
                r#"{"path":"source.rs","start_line":0,"end_line":1}"#
            )
            .is_err()
        );
        assert!(
            read(
                dir.path(),
                r#"{"path":"source.rs","start_line":1,"end_line":201}"#
            )
            .is_err()
        );
        assert!(
            read(
                dir.path(),
                r#"{"path":"source.rs","start_line":181,"end_line":182}"#
            )
            .is_err()
        );
    }
    #[test]
    fn large_listings_and_unicode_file_previews_have_honest_bounds() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..160 {
            fs::write(
                root.path().join(format!("{n:03}-{}.rs", "name".repeat(20))),
                "x",
            )
            .unwrap();
        }
        let listing = list(root.path(), "").unwrap();
        assert_eq!(listing["complete"], false);
        assert_eq!(
            listing["returned"].as_u64().unwrap() as usize,
            listing["entries"].as_array().unwrap().len()
        );
        assert!(serde_json::to_vec(&listing).unwrap().len() <= BUDGET);
        assert!(listing["returned"].as_u64().unwrap() > 0);
        let original = "é".repeat(20000);
        fs::write(root.path().join("unicode.rs"), &original).unwrap();
        let preview = read(root.path(), "unicode.rs").unwrap();
        assert_eq!(preview["content_complete"], false);
        assert_eq!(preview["source_bytes"], original.len());
        assert!(original.starts_with(preview["content"].as_str().unwrap()));
        assert!(serde_json::to_vec(&preview).unwrap().len() <= BUDGET);
        fs::write(root.path().join("oversized.rs"), vec![b'x'; 65537]).unwrap();
        assert!(read(root.path(), "oversized.rs").is_err());
    }
    #[test]
    fn listed_data_reaches_context_without_changing_the_workspace_base() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname=\"fixture\"\n",
        )
        .unwrap();
        fs::write(root.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let listing = list(root.path(), "src").unwrap();
        let path = listing["entries"][0]["path"].as_str().unwrap();
        assert!(read(root.path(), path).is_ok());
        assert!(
            read(root.path(), "Cargo.toml").unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("fixture")
        );
        let j = crate::Journal::open(&root.path().join("journal.sqlite")).unwrap();
        j.event("tool_result", json!({"tool":"list","result":listing}))
            .unwrap();
        let mut context = json!({"last_tool_result":j.last_result().unwrap(),"recent":vec!["old context ".repeat(1000);20]});
        crate::fit_context(&mut context).unwrap();
        assert_eq!(context["last_tool_result"]["result"], listing);
        let hint = manifest_hint(root.path());
        assert_eq!(hint["available"], true);
        assert_eq!(hint["content_complete"], true);
        assert!(hint["content"].as_str().unwrap().contains("fixture"));
        fs::write(
            root.path().join("Cargo.toml"),
            "# large manifest\n".repeat(2000),
        )
        .unwrap();
        let hint = manifest_hint(root.path());
        assert_eq!(hint["content_complete"], false);
        assert!(hint.to_string().len() <= 1024);
    }
}
