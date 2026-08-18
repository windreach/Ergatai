// Integration test: real fanotify enforcement.
// Agent A holds lock, Agent B (different PID) tries to open → should be denied.
// Requires root + CAP_SYS_ADMIN.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use ergatai_lock::{
    CallbackPidResolver, Enforcer, EnforcerConfig, FileMode, FileLockManager, FileToken,
    SystemToken,
};

fn setup() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join(".ergatai")).unwrap();
    (tmp, root)
}

fn make_lock_manager(root: &PathBuf) -> Arc<FileLockManager> {
    let db_path = root.join(".ergatai").join("locks.db");
    let mgr = FileLockManager::new(&db_path, root.clone(), None).unwrap();
    Arc::new(mgr)
}

fn make_resolver(holder_pid: u32) -> Arc<CallbackPidResolver> {
    Arc::new(CallbackPidResolver::new(move || {
        vec![
            (holder_pid, "agent-a".to_string(), "session-a".to_string()),
            (99999, "agent-b".to_string(), "session-b".to_string()),
        ]
    }))
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_enforcer_allows_holder_subprocess() {
    // Test that a subprocess spawned by the holder agent can read locked files.
    // The subprocess is identified as the holder via ancestor walk in PidResolver.
    if !is_root() {
        eprintln!("SKIP: not root");
        return;
    }

    let (tmp, root) = setup();
    let lock_mgr = make_lock_manager(&root);

    // Create a test file (name must not match sensitive patterns like *secret*, *key*, etc.)
    let test_file = root.join("data.txt");
    fs::write(&test_file, "hello world").unwrap();

    // Agent A's PID is our own process
    let agent_a_pid = std::process::id();
    let resolver = make_resolver(agent_a_pid);

    // Start the enforcer
    let enforcer = Enforcer::start(
        root.clone(),
        "integration-test".to_string(),
        lock_mgr.clone(),
        resolver,
        None, // no NATS
        EnforcerConfig::default(),
    )
    .unwrap();

    assert!(enforcer.is_active(), "enforcer should be active as root");

    // Give the event loop a moment to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create SystemToken + FileToken for agent-a
    let sys_token = SystemToken::new(
        "agent-a".to_string(),
        "session-a".to_string(),
        root.to_string_lossy().to_string(),
        3600,
        30,
    );

    let file_token = FileToken::new(
        "agent-a".to_string(),
        "session-a".to_string(),
        sys_token.id.clone(),
        "**".to_string(), // scope: all files
        FileMode::Write,
        Some("integration test".to_string()),
        "system".to_string(),
        3600,
        30,
    );

    // Agent A acquires a WRITE lock on data.txt
    lock_mgr
        .acquire_lock(&file_token, "data.txt")
        .await
        .expect("agent-a should acquire WRITE lock");

    // Spawn a subprocess (child of agent-a) that tries to read the file.
    // The subprocess's PID is different, but PidResolver walks ancestors
    // and identifies it as agent-a → fanotify ALLOW.
    // NOTE: must use tokio::process::Command (async), NOT std::process::Command.
    // The synchronous version would block the current-thread tokio runtime,
    // preventing the fanotify event loop from processing the permission event — deadlock.
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("cat")
            .arg(&test_file)
            .output()
    )
    .await;

    let output = match output {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            enforcer.stop().await;
            panic!("failed to spawn cat: {}", e);
        }
        Err(_) => {
            enforcer.stop().await;
            panic!("cat timed out - deadlock detected");
        }
    };

    // Cat should succeed (identified as agent-a via ancestor walk).
    assert!(
        output.status.success(),
        "cat (holder subprocess) should be allowed to read, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "hello world");

    // Verify agent A (our own PID, the holder) CAN still read the file directly.
    let content = fs::read_to_string(&test_file).expect("holder should be able to read");
    assert_eq!(content, "hello world");

    // Cleanup
    enforcer.stop().await;
    drop(tmp);
}
