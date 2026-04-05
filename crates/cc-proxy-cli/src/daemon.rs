use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// PID file location: ~/.cc-proxy/proxy.pid
pub fn pid_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cc-proxy")
        .join("proxy.pid")
}

/// Write PID to file
pub fn write_pid(pid: u32) -> anyhow::Result<()> {
    let path = pid_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, pid.to_string())?;
    Ok(())
}

/// Read PID from file
#[allow(dead_code)]
pub fn read_pid() -> Option<u32> {
    let path = pid_file_path();
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Remove PID file
#[allow(dead_code)]
pub fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

/// Check if a process is alive
#[allow(dead_code)]
pub fn is_process_alive(pid: u32) -> bool {
    // Use kill -0 to check if process exists
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Start the proxy in daemon mode by re-executing self
pub fn start_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;

    // Collect all relevant env vars
    let child = Command::new(exe)
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    write_pid(pid)?;

    println!("🚀 cc-proxy 已在后台启动 (PID: {pid})");
    println!("   PID 文件: {}", pid_file_path().display());
    println!();
    println!("   停止: cc-proxy stop");
    println!("   状态: cc-proxy status");

    Ok(())
}
