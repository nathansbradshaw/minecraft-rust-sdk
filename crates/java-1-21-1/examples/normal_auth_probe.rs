//! Operator-run negative admission check. It never authenticates or enters a world.
use minecraft_sdk_java_1_21_1::{Wire, encode_signed_varint as vi};
use minecraft_sdk_wire::encode_string;
use std::{
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = std::env::args()
        .nth(1)
        .ok_or("Usage: normal_auth_probe LOOPBACK_BACKEND")?
        .parse()?;
    if !address.ip().is_loopback() {
        return Err("Probe requires an operator's loopback backend".into());
    }
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut wire = Wire::new(stream);
    let mut handshake = vi(767);
    handshake.extend(encode_string(&address.ip().to_string())?);
    handshake.extend(address.port().to_be_bytes());
    handshake.extend(vi(2));
    wire.send(0, &handshake)?;
    let mut login = encode_string("SdkAuthProbe")?;
    login.extend([0u8; 16]);
    wire.send(0, &login)?;
    let response = wire.receive_until(Instant::now() + Duration::from_secs(5))?;
    if response.id != 1 {
        return Err("Normal unticketed login did not require Java authentication".into());
    }
    println!(
        "Unticketed normal Java login requires encrypted authentication; no world entry attempted."
    );
    Ok(())
}
