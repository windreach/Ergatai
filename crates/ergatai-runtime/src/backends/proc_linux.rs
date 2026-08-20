//! Linux-specific /proc helpers. Extracted so non-Linux builds can stub.
//!
//! Reads `/proc/{pid}/stat` to determine process state (Running, Sleeping, Zombie, etc.).
//! Used by health monitoring to detect unhealthy agent processes.

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
