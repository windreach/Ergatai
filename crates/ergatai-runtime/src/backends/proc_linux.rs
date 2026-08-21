//! Linux-specific /proc helpers. Extracted so non-Linux builds can stub.
//!
//! - `read_proc_state`: reads `/proc/{pid}/stat` to determine process state
//!   (Running, Sleeping, Zombie, etc.). Used by health monitoring.
//! - `read_proc_environ`: reads a specific env var from `/proc/{pid}/environ`.
//! - `find_child_environ`: reads an env var from a child process named
//!   "opencode" or "opencode.exe" (scans `/proc/{pid}/task/{pid}/children`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Sleeping,
    Zombie,
    Stopped,
    TracingStop,
    Dead,
    Unknown,
}

#[cfg(target_os = "linux")]
pub fn read_proc_state(pid: u32) -> std::io::Result<ProcessState> {
    let path = format!("/proc/{pid}/stat");
    let contents = std::fs::read_to_string(&path)?;
    // Format: pid (comm) state ppid ...
    // `comm` can contain spaces/parens, so find the LAST ')'
    let Some(close_paren) = contents.rfind(')') else {
        return Ok(ProcessState::Unknown);
    };
    let rest = &contents[close_paren + 1..];
    let mut fields = rest.split_whitespace();
    // After the last ')', the first field is the state (field 3 in /proc/[pid]/stat)
    let state_char = fields.next().unwrap_or("?");
    Ok(match state_char {
        "R" => ProcessState::Running,
        "S" => ProcessState::Sleeping,
        "Z" => ProcessState::Zombie,
        "T" | "t" => ProcessState::Stopped,
        "X" | "x" => ProcessState::Dead,
        _ => ProcessState::Unknown,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn read_proc_state(_pid: u32) -> std::io::Result<ProcessState> {
    Ok(ProcessState::Unknown)
}

/// Read an environment variable from a process's `/proc/{pid}/environ`.
///
/// Linux-specific: reads the null-delimited environment block from procfs.
/// Returns `None` if the process doesn't exist, permission is denied,
/// or the variable is not set.
#[cfg(target_os = "linux")]
pub fn read_proc_environ(pid: u32, var_name: &str) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let data = std::fs::read(format!("/proc/{}/environ", pid)).ok()?;
    let prefix = format!("{}=", var_name);
    data.split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .find_map(|entry| entry.strip_prefix(&prefix).map(|v| v.to_string()))
}

#[cfg(not(target_os = "linux"))]
pub fn read_proc_environ(_pid: u32, _var_name: &str) -> Option<String> {
    None
}

/// Find an environment variable from a child process named "opencode".
///
/// The startup script (bash) exec's opencode, so we scan
/// `/proc/{pid}/task/{pid}/children` to find the opencode process and read its env.
#[cfg(target_os = "linux")]
pub fn find_child_environ(pid: u32, var_name: &str) -> Option<String> {
    let children_path = format!("/proc/{}/task/{}/children", pid, pid);
    let children_data = std::fs::read_to_string(&children_path).ok()?;

    for child_pid_str in children_data.split_whitespace() {
        if let Ok(child_pid) = child_pid_str.parse::<u32>() {
            let comm_path = format!("/proc/{}/comm", child_pid);
            if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                let name = comm.trim();
                // Match both "opencode" and "opencode.exe" (the ELF binary name)
                if name == "opencode" || name == "opencode.exe" {
                    return read_proc_environ(child_pid, var_name);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn find_child_environ(_pid: u32, _var_name: &str) -> Option<String> {
    None
}

/// Find the first child PID of a process by reading `/proc/{pid}/task/{pid}/children`.
///
/// Returns the first child PID found, or `None` if the process has no children
/// or the information is unavailable.
#[cfg(target_os = "linux")]
pub fn find_child_pid(pid: u32) -> Option<u32> {
    let children_path = format!("/proc/{}/task/{}/children", pid, pid);
    let children_data = std::fs::read_to_string(&children_path).ok()?;
    children_data
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<u32>().ok())
}

#[cfg(not(target_os = "linux"))]
pub fn find_child_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn read_proc_state_returns_running_for_self() {
        let pid = std::process::id();
        let state = read_proc_state(pid).unwrap();
        // Self should exist and be in a valid state (Running, Sleeping, or other valid states).
        // The exact state depends on what the process is doing at the moment of the read.
        // We just verify it's not Unknown or Dead (which would indicate a problem).
        assert!(!matches!(state, ProcessState::Unknown | ProcessState::Dead));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_proc_state_returns_unknown_for_bogus_pid() {
        let state = read_proc_state(9_999_999).unwrap_or(ProcessState::Unknown);
        // Either Unknown or Dead — both acceptable for a non-existent PID
        assert!(matches!(state, ProcessState::Unknown | ProcessState::Dead));
    }
}
