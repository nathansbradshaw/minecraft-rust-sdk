//! Run INSIDE the dedicated disposable coding container, never on the host.
use std::{fs,io::Write,net::{SocketAddr,TcpStream},process::{Command,Stdio},time::Duration};
fn main()->Result<(),Box<dyn std::error::Error>>{
    if !std::path::Path::new("/.dockerenv").exists(){return Err("This probe must run inside the disposable coding container".into());}
    let forbidden=std::env::var("FORBIDDEN_PATH_PROBES").unwrap_or_else(|_|"/var/run/docker.sock,/srv/minecraft,/workspace-peer".into());
    for path in forbidden.split(','){println!("forbidden_path_absent {path} {}",!std::path::Path::new(path).exists());}
    println!("root_read_only {}",fs::write("/root-write-probe",b"probe").is_err());
    println!("workspace_writable {}",fs::write("/workspace/boundary-probe.txt",b"bot-owned source").is_ok());
    let status=fs::read_to_string("/proc/self/status")?;
    println!("capabilities_dropped {}",status.lines().any(|line|line.starts_with("CapEff:")&&line.ends_with("0000000000000000")));
    println!("no_new_privileges {}",status.lines().any(|line|line.starts_with("NoNewPrivs:")&&line.ends_with('1')));
    for file in["memory.max","memory.swap.max","cpu.max","pids.max"]{println!("cgroup_{file} {}",fs::read_to_string(format!("/sys/fs/cgroup/{file}"))?.trim());}
    let probes=std::env::var("DIRECT_NETWORK_PROBES").unwrap_or_else(|_|"169.254.169.254:80,10.0.0.1:80,[fd00::1]:80".into());
    for socket in probes.split(','){let address:SocketAddr=socket.parse()?;println!("direct_network_denied {socket} {}",TcpStream::connect_timeout(&address,Duration::from_secs(1)).is_err());}
    let mut processes=Vec::new();let mut denied=false;
    for _ in 0..160{match Command::new("sleep").arg("30").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn(){Ok(child)=>processes.push(child),Err(_)=>{denied=true;break;}}}
    println!("process_budget_enforced {denied} children={}",processes.len());
    for mut child in processes{let _=child.kill();let _=child.wait();}
    let mut file=fs::File::create("/workspace/storage-budget-probe.bin")?;let chunk=[0u8;1024*1024];let mut denied=false;
    for _ in 0..400{if file.write_all(&chunk).is_err(){denied=true;break;}}
    drop(file);fs::remove_file("/workspace/storage-budget-probe.bin")?;println!("workspace_storage_budget_enforced {denied}");
    Ok(())
}
