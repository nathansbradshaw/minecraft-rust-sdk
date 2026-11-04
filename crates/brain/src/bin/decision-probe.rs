//! Declared zero-inference process/lifecycle fixture, not an intelligent agent.
use std::io::Read;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    std::io::stdin().take(65537).read_to_end(&mut input)?;
    if input.len() > 65536 {
        return Err("Fixture frame limit".into());
    }
    let request: serde_json::Value = serde_json::from_slice(&input)?;
    if request["version"] != 1 {
        return Err("Fixture protocol version".into());
    }
    let mode = match std::fs::read(".worker-probe.json") {
        Ok(b) if b.len() <= 4096 => serde_json::from_slice::<serde_json::Value>(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        _ => return Err("Fixture configuration invalid".into()),
    };
    let delay = mode["delay_ms"].as_u64().unwrap_or(0);
    if delay > 10000 {
        return Err("Fixture delay limit".into());
    }
    std::thread::sleep(std::time::Duration::from_millis(delay));
    if mode["fail"] == true {
        std::process::exit(7);
    }
    if mode["error"].is_object() {
        println!(
            "{}",
            serde_json::json!({"version":1,"request_id":request["request_id"],"answer":null,"error":mode["error"]})
        );
        return Ok(());
    }
    println!(
        "{}",
        serde_json::json!({"version":1,"request_id":if mode["wrong_identity"]==true{serde_json::json!("wrong")}else{request["request_id"].clone()},"answer":[{"tool":"observe","input":"","path":"","summary":"Declared worker lifecycle probe; no inference","notes":"","experiment":false},{"models_called":0,"probe":true}],"error":null})
    );
    Ok(())
}
