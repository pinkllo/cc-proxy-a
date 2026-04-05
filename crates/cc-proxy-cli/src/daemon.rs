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
pub fn read_pid() -> Option<u32> {
    let path = pid_file_path();
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Remove PID file
pub fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

/// Check if a process is alive
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let pid = pid as libc::pid_t;
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        // On Windows, use tasklist to check if PID exists
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// Start the proxy in daemon mode by re-executing self
pub fn start_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;

    // Log to file instead of /dev/null
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cc-proxy");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("proxy.log");
    let log_file = fs::File::create(&log_path)?;
    let err_file = log_file.try_clone()?;

    let child = Command::new(exe)
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(err_file)
        .spawn()?;

    let pid = child.id();
    write_pid(pid)?;

    println!("🚀 cc-proxy 已在后台启动 (PID: {pid})");
    println!("   PID 文件: {}", pid_file_path().display());
    println!("   日志文件: {}", log_path.display());
    println!();
    println!("   停止: cc-proxy stop");
    println!("   状态: cc-proxy status");

    Ok(())
}
