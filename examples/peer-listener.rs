//! Disposable test service for proving cross-container network isolation.
use std::{io::Write,net::TcpListener};
fn main()->std::io::Result<()>{
    for stream in TcpListener::bind("0.0.0.0:8081")?.incoming(){let mut stream=stream?;stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\npeer-only")?;}Ok(())
}
