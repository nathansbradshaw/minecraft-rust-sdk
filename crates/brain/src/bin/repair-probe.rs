//! Offline source/diagnostic applicability probe. Does not invoke a model.
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("private context JSON path required")?;
    if args.next().is_some() {
        return Err("one context path required".into());
    }
    let context: serde_json::Value = serde_json::from_slice(&minecraft_sdk_brain::private_read(
        std::path::Path::new(&path),
        65536,
    )?)?;
    println!(
        "{}",
        minecraft_sdk_brain::model::compiler_repair_preview(&context)
    );
    Ok(())
}
