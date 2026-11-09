//! Offline planning inspection: saved SDK observation and exact runtime recipe query.
//! cargo run -p minecraft-sdk --example recipe_materials -- observation.json recipes.json
use minecraft_sdk::{Observation, recipes};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = std::env::args_os().skip(1);
    let observation: Observation = serde_json::from_slice(&std::fs::read(
        paths.next().ok_or("observation path required")?,
    )?)?;
    let query: serde_json::Value = serde_json::from_slice(&std::fs::read(
        paths.next().ok_or("runtime recipe query path required")?,
    )?)?;
    if paths.next().is_some()
        || query["format"] != "minecraft-runtime-recipes-v1"
        || query["complete"] != true
    {
        return Err("exact complete runtime recipe query required".into());
    }
    let rows = query["matches"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or("exactly one runtime recipe required")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&recipes::material_summary(&observation, &rows[0])?)?
    );
    Ok(())
}
