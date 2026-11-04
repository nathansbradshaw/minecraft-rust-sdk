//! Offline context-budget replay. Never opens an SDK/model connection.
use minecraft_sdk_brain::{Journal, Result, private_read, private_write, replay_context};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("Usage: context-probe CLOSED_JOURNAL.sqlite PRIVATE_LAST_CONTEXT.json [NEW_FITTED_CONTEXT.json]".into());
    }
    let journal = Journal::open_read_only(Path::new(&args[0]))?;
    let mut context = serde_json::from_slice(&private_read(Path::new(&args[1]), 1024 * 1024)?)?;
    let report = replay_context(&journal, &mut context)?;
    if let Some(path) = args.get(2) {
        if Path::new(path).exists() {
            return Err("Use a new private output path".into());
        }
        private_write(Path::new(path), &serde_json::to_vec_pretty(&context)?)?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
