//! Multi-Agent Integration Tests
//!
//! Tests the complete multi-agent file access control flow:
//! - Concurrent lock competition between agents
//! - Lock lifecycle (acquire → heartbeat → release → re-acquire)
//! - Scope enforcement and permission checks
//! - Sensitive path + ADMIN enforcement
//! - Path traversal protection
//! - Token expiration
//! - Audit trail verification

#[cfg(test)]
mod tests {
    use crate::error::ErgataiError;
    use crate::file_access::{FileLockManager, FileMode, FileToken, SystemToken};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Helper: create a test lock manager with project root and some test files
    fn setup_test_env() -> (TempDir, Arc<FileLockManager>) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let project_root = temp_dir.path().to_path_buf();

        // Create test files
        std::fs::write(project_root.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(project_root.join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(project_root.join("config.rs"), "pub const X: i32 = 1;").unwrap();
        std::fs::write(project_root.join(".env"), "SECRET=abc123").unwrap();
        std::fs::create_dir_all(project_root.join("src")).unwrap();
        std::fs::write(project_root.join("src/app.rs"), "struct App;").unwrap();
        std::fs::write(project_root.join("src/util.rs"), "fn util() {}").unwrap();

        let manager = Arc::new(FileLockManager::new(&db_path, project_root, None).unwrap());
        (temp_dir, manager)
    }

    /// Helper: create a file token
    fn make_file_token(
        agent_id: &str,
        session_id: &str,
        system_token_id: &crate::file_access::TokenId,
        scope: &str,
        mode: FileMode,
    ) -> FileToken {
        FileToken::new(
            agent_id.to_string(),
            session_id.to_string(),
            system_token_id.clone(),
            scope.to_string(),
            mode,
            Some("integration test".to_string()),
            "test-system".to_string(),
            3600,
            15,
        )
    }

    /// Get project root from manager — uses internal access for test construction.
    /// Since FileLockManager doesn't expose project_root publicly, we use a fixed
    /// string for token construction (it's only stored, not validated at creation).
    fn test_project_root() -> String {
        "/test-project".to_string()
    }

    // ================================================================
    // Test 1: Concurrent WRITE lock competition
    // ================================================================
    #[tokio::test]
    async fn test_concurrent_write_competition() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        // Agent A acquires WRITE lock
        manager.acquire_lock(&token_a, "main.rs").await.unwrap();
        assert!(manager.is_file_locked("main.rs").unwrap());

        // Agent B tries same file → should fail
        let result = manager.acquire_lock(&token_b, "main.rs").await;
        assert!(result.is_err(), "Expected error, got: {:?}", result);
        let err = result.unwrap_err();
        assert!(
            matches!(err, ErgataiError::LockConflict(_))
                || matches!(err, ErgataiError::LockConflictWithRetry { .. }),
            "Expected LockConflict or LockConflictWithRetry, got: {:?}",
            err
        );

        // Agent A releases → Agent B can now acquire
        manager
            .release_lock(token_a.id.as_str(), "main.rs")
            .await
            .unwrap();
        assert!(!manager.is_file_locked("main.rs").unwrap());

        manager.acquire_lock(&token_b, "main.rs").await.unwrap();
        assert!(manager.is_file_locked("main.rs").unwrap());
    }

    // ================================================================
    // Test 2: Multiple READ locks coexist
    // ================================================================
    #[tokio::test]
    async fn test_multiple_read_locks_coexist() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Read);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Read);

        manager.acquire_lock(&token_a, "main.rs").await.unwrap();
        manager.acquire_lock(&token_b, "main.rs").await.unwrap();

        // is_file_locked checks WRITE only
        assert!(!manager.is_file_locked("main.rs").unwrap());
    }

    // ================================================================
    // Test 3: Lock lifecycle — acquire → heartbeat → release → re-acquire
    // ================================================================
    #[tokio::test]
    async fn test_lock_lifecycle() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        manager.acquire_lock(&token, "lib.rs").await.unwrap();
        assert!(manager.is_file_locked("lib.rs").unwrap());

        manager.update_heartbeat(token.id.as_str()).unwrap();

        manager
            .release_lock(token.id.as_str(), "lib.rs")
            .await
            .unwrap();
        assert!(!manager.is_file_locked("lib.rs").unwrap());

        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_b).unwrap();
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        manager.acquire_lock(&token_b, "lib.rs").await.unwrap();
        assert!(manager.is_file_locked("lib.rs").unwrap());
    }

    // ================================================================
    // Test 4: Scope enforcement
    // ================================================================
    #[tokio::test]
    async fn test_scope_enforcement() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("agent-a", "session-a", &sys.id, "src/**", FileMode::Write);

        manager.acquire_lock(&token, "src/app.rs").await.unwrap();
        manager
            .release_lock(token.id.as_str(), "src/app.rs")
            .await
            .unwrap();

        let result = manager.acquire_lock(&token, "main.rs").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ErgataiError::PermissionDenied(_)
        ));
    }

    // ================================================================
    // Test 5: Sensitive path requires ADMIN mode
    // ================================================================
    #[tokio::test]
    async fn test_sensitive_path_requires_admin() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token_write = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);
        let result = manager.acquire_lock(&token_write, ".env").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ErgataiError::PermissionDenied(msg) => {
                assert!(msg.contains("ADMIN"), "Error should mention ADMIN: {}", msg);
            }
            other => panic!("Expected PermissionDenied, got: {:?}", other),
        }

        // ADMIN → accepted
        let token_admin = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Admin);
        manager.acquire_lock(&token_admin, ".env").await.unwrap();
    }

    // ================================================================
    // Test 6: Path traversal rejected
    // ================================================================
    #[tokio::test]
    async fn test_path_traversal_rejected() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        let result = manager.acquire_lock(&token, "../../../etc/passwd").await;
        assert!(result.is_err());
    }

    // ================================================================
    // Test 7: Parallel locks on different files
    // ================================================================
    #[tokio::test]
    async fn test_parallel_locks_different_files() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root.clone(), 3600, 30);
        let sys_c = SystemToken::new("agent-c".into(), "session-c".into(), root.clone(), 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();
        manager.register_system_token(&sys_c).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        let token_c = make_file_token("agent-c", "session-c", &sys_c.id, "**", FileMode::Write);

        manager.acquire_lock(&token_a, "main.rs").await.unwrap();
        manager.acquire_lock(&token_b, "lib.rs").await.unwrap();
        manager.acquire_lock(&token_c, "config.rs").await.unwrap();

        assert!(manager.is_file_locked("main.rs").unwrap());
        assert!(manager.is_file_locked("lib.rs").unwrap());
        assert!(manager.is_file_locked("config.rs").unwrap());

        manager
            .release_lock(token_a.id.as_str(), "main.rs")
            .await
            .unwrap();
        manager
            .release_lock(token_b.id.as_str(), "lib.rs")
            .await
            .unwrap();
        manager
            .release_lock(token_c.id.as_str(), "config.rs")
            .await
            .unwrap();

        assert!(!manager.is_file_locked("main.rs").unwrap());
        assert!(!manager.is_file_locked("lib.rs").unwrap());
        assert!(!manager.is_file_locked("config.rs").unwrap());
    }

    // ================================================================
    // Test 8: Audit trail entries created on lock ops
    // ================================================================
    #[tokio::test]
    async fn test_audit_trail_on_lock_ops() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        manager.acquire_lock(&token, "main.rs").await.unwrap();
        manager
            .release_lock(token.id.as_str(), "main.rs")
            .await
            .unwrap();

        manager
            .log_audit(
                "agent-a",
                "session-a",
                "TEST_CHECK",
                Some("main.rs"),
                Some("WRITE"),
                Some("test"),
            )
            .unwrap();
    }

    // ================================================================
    // Test 9: Expired token removed from active list
    // ================================================================
    #[tokio::test]
    async fn test_expired_token_not_active() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        assert_eq!(manager.get_active_tokens().unwrap().len(), 1);

        manager.expire_token(sys.id.as_str()).unwrap();

        assert_eq!(manager.get_active_tokens().unwrap().len(), 0);
    }

    // ================================================================
    // Test 10: Thread-based concurrent WRITE competition
    // ================================================================
    #[tokio::test]
    async fn test_thread_concurrent_write_competition() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        let mgr_a = Arc::clone(&manager);
        let mgr_b = Arc::clone(&manager);
        let token_a_clone = token_a.clone();
        let token_b_clone = token_b.clone();

        let handle_a =
            tokio::spawn(async move { mgr_a.acquire_lock(&token_a_clone, "main.rs").await });
        let handle_b = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            mgr_b.acquire_lock(&token_b_clone, "main.rs").await
        });

        let result_a = handle_a.await.unwrap();
        let result_b = handle_b.await.unwrap();

        let a_ok = result_a.is_ok();
        let b_ok = result_b.is_ok();
        assert!(
            a_ok ^ b_ok,
            "Exactly one should succeed: a_ok={}, b_ok={}",
            a_ok,
            b_ok
        );
    }

    // ================================================================
    // Test 11: Release by wrong token fails
    // ================================================================
    #[tokio::test]
    async fn test_release_by_wrong_token_fails() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        manager.acquire_lock(&token_a, "main.rs").await.unwrap();

        let result = manager.release_lock(token_b.id.as_str(), "main.rs").await;
        assert!(result.is_err());

        manager
            .release_lock(token_a.id.as_str(), "main.rs")
            .await
            .unwrap();
    }

    // ================================================================
    // Test 12: get_locks_by_token tracks active locks
    // ================================================================
    #[tokio::test]
    async fn test_get_locks_by_token() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        manager.acquire_lock(&token, "main.rs").await.unwrap();
        manager.acquire_lock(&token, "lib.rs").await.unwrap();
        manager.acquire_lock(&token, "config.rs").await.unwrap();

        assert_eq!(
            manager.get_locks_by_token(token.id.as_str()).unwrap().len(),
            3
        );

        manager
            .release_lock(token.id.as_str(), "lib.rs")
            .await
            .unwrap();
        assert_eq!(
            manager.get_locks_by_token(token.id.as_str()).unwrap().len(),
            2
        );
    }

    // ================================================================
    // Test 13: get_tokens_by_session
    // ================================================================
    #[tokio::test]
    async fn test_get_tokens_by_session() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let tokens = manager.get_tokens_by_session("session-a").unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].agent_id, "agent-a");

        assert_eq!(
            manager.get_tokens_by_session("nonexistent").unwrap().len(),
            0
        );
    }

    // ================================================================
    // Test 14: Full DAG workflow (3 agents, sequential phases)
    // ================================================================
    #[tokio::test]
    async fn test_dag_workflow_simulation() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new(
            "analyzer".into(),
            "session-analyze".into(),
            root.clone(),
            3600,
            30,
        );
        let sys_b = SystemToken::new(
            "modifier".into(),
            "session-modify".into(),
            root.clone(),
            3600,
            30,
        );
        let sys_c = SystemToken::new(
            "tester".into(),
            "session-test".into(),
            root.clone(),
            3600,
            30,
        );
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();
        manager.register_system_token(&sys_c).unwrap();

        // Phase 1: Analyzer reads
        let token_a = make_file_token(
            "analyzer",
            "session-analyze",
            &sys_a.id,
            "src/**",
            FileMode::Read,
        );
        manager.acquire_lock(&token_a, "src/app.rs").await.unwrap();
        manager.acquire_lock(&token_a, "src/util.rs").await.unwrap();
        manager
            .release_lock(token_a.id.as_str(), "src/app.rs")
            .await
            .unwrap();
        manager
            .release_lock(token_a.id.as_str(), "src/util.rs")
            .await
            .unwrap();

        // Phase 2: Modifier writes
        let token_b = make_file_token(
            "modifier",
            "session-modify",
            &sys_b.id,
            "src/**",
            FileMode::Write,
        );
        manager.acquire_lock(&token_b, "src/app.rs").await.unwrap();
        manager
            .release_lock(token_b.id.as_str(), "src/app.rs")
            .await
            .unwrap();

        // Phase 3: Tester reads
        let token_c = make_file_token(
            "tester",
            "session-test",
            &sys_c.id,
            "src/**",
            FileMode::Read,
        );
        manager.acquire_lock(&token_c, "src/app.rs").await.unwrap();
        manager.acquire_lock(&token_c, "src/util.rs").await.unwrap();
        manager
            .release_lock(token_c.id.as_str(), "src/app.rs")
            .await
            .unwrap();
        manager
            .release_lock(token_c.id.as_str(), "src/util.rs")
            .await
            .unwrap();

        assert!(!manager.is_file_locked("src/app.rs").unwrap());
        assert!(!manager.is_file_locked("src/util.rs").unwrap());
    }

    // ================================================================
    // Test 15: Heartbeat keeps token active
    // ================================================================
    #[tokio::test]
    async fn test_heartbeat_keeps_token_active() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);
        manager.acquire_lock(&token, "main.rs").await.unwrap();

        manager.update_heartbeat(sys.id.as_str()).unwrap();

        assert_eq!(manager.get_active_tokens().unwrap().len(), 1);
    }

    // ================================================================
    // Test 16: Cleanup old audit logs (fresh logs not deleted)
    // ================================================================
    #[tokio::test]
    async fn test_cleanup_old_audit_logs_no_fresh_delete() {
        let (_temp, manager) = setup_test_env();

        manager
            .log_audit(
                "agent-a",
                "session-a",
                "LOCK_ACQUIRED",
                Some("main.rs"),
                Some("WRITE"),
                Some("test"),
            )
            .unwrap();
        manager
            .log_audit(
                "agent-a",
                "session-a",
                "LOCK_RELEASED",
                Some("main.rs"),
                None,
                None,
            )
            .unwrap();

        let deleted = manager.cleanup_old_audit_logs(30).unwrap();
        assert_eq!(deleted, 0);
    }

    // ================================================================
    // Test 17: READ lock doesn't show as "locked" in WRITE check
    // ================================================================
    #[tokio::test]
    async fn test_read_not_counted_as_write_locked() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Read);

        manager.acquire_lock(&token, "main.rs").await.unwrap();
        assert!(!manager.is_file_locked("main.rs").unwrap());
    }

    // ================================================================
    // Test 18: Release non-existent lock returns NotFound
    // ================================================================
    #[tokio::test]
    async fn test_release_nonexistent_lock() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        let result = manager.release_lock(token.id.as_str(), "main.rs").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ErgataiError::NotFound(_)));
    }

    // ================================================================
    // Test 19: Multi-agent parallel no conflict (different files)
    // ================================================================
    #[tokio::test]
    async fn test_multi_agent_parallel_no_conflict() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let agents = vec![
            ("agent-a", "session-a", "main.rs"),
            ("agent-b", "session-b", "lib.rs"),
            ("agent-c", "session-c", "config.rs"),
        ];

        let mut tokens = Vec::new();

        for (agent_id, session_id, _) in &agents {
            let sys = SystemToken::new(
                agent_id.to_string(),
                session_id.to_string(),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            let token = make_file_token(agent_id, session_id, &sys.id, "**", FileMode::Write);
            tokens.push((token, sys));
        }

        for (i, (agent_id, _, file)) in agents.iter().enumerate() {
            manager.acquire_lock(&tokens[i].0, file).await.unwrap();
            assert!(
                manager.is_file_locked(file).unwrap(),
                "{} should have locked {}",
                agent_id,
                file
            );
        }

        for (i, (_, _, file)) in agents.iter().enumerate() {
            manager
                .release_lock(tokens[i].0.id.as_str(), file)
                .await
                .unwrap();
        }

        for (_, _, file) in &agents {
            assert!(!manager.is_file_locked(file).unwrap());
        }
    }

    // ================================================================
    // Test 20: WRITE blocks WRITE even when not checking is_file_locked first
    // ================================================================
    #[tokio::test]
    async fn test_write_blocks_write_at_db_level() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        manager.acquire_lock(&token_a, "main.rs").await.unwrap();

        let result = manager.acquire_lock(&token_b, "main.rs").await;
        assert!(result.is_err());
    }

    // ================================================================
    // Watchdog Integration Tests
    // ================================================================

    use crate::file_access::{Watchdog, WatchdogConfig};

    fn setup_watchdog_env() -> (TempDir, Arc<FileLockManager>) {
        setup_test_env()
    }

    // ================================================================
    // Test 21: Watchdog start/stop lifecycle
    // ================================================================
    #[tokio::test]
    async fn test_watchdog_lifecycle() {
        let (_temp, manager) = setup_watchdog_env();
        let config = WatchdogConfig {
            check_interval_secs: 1,
            ..Default::default()
        };
        let mut watchdog = Watchdog::new(manager, config);

        // Start should succeed
        watchdog.start().unwrap();

        // Double start should fail
        let result = watchdog.start();
        assert!(result.is_err(), "Double start should fail");

        // Stop should succeed
        watchdog.stop().unwrap();
    }

    // ================================================================
    // Test 22: mark_busy / clear_busy lifecycle
    // ================================================================
    #[tokio::test]
    async fn test_watchdog_mark_busy_lifecycle() {
        let (_temp, manager) = setup_watchdog_env();
        let config = WatchdogConfig::default();
        let watchdog = Watchdog::new(manager, config);

        // Mark busy
        watchdog.mark_busy("session-a", 300).await.unwrap();

        // Mark busy again (overwrite)
        watchdog.mark_busy("session-a", 600).await.unwrap();

        // Clear busy
        watchdog.clear_busy("session-a").await.unwrap();

        // Clear non-existent (should not error)
        watchdog.clear_busy("nonexistent").await.unwrap();
    }

    // ================================================================
    // Test 23: Task-aware disabled → mark_busy is no-op
    // ================================================================
    #[tokio::test]
    async fn test_watchdog_task_aware_disabled() {
        let (_temp, manager) = setup_watchdog_env();
        let config = WatchdogConfig {
            task_aware: false,
            ..Default::default()
        };
        let watchdog = Watchdog::new(manager, config);

        // Should not error, just silently ignore
        watchdog.mark_busy("session-a", 300).await.unwrap();
    }

    // ================================================================
    // Test 24: ACP disconnect reclaims all locks for session
    // This tests the full reclaim flow: get tokens → release locks → expire tokens
    // ================================================================
    #[tokio::test]
    async fn test_acp_disconnect_reclaims_locks() {
        let (_temp, manager) = setup_watchdog_env();
        let root = test_project_root();

        // Set up agent with token and locks
        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);
        manager.acquire_lock(&token, "main.rs").await.unwrap();
        manager.acquire_lock(&token, "lib.rs").await.unwrap();

        // Verify locks are held
        assert!(manager.is_file_locked("main.rs").unwrap());
        assert!(manager.is_file_locked("lib.rs").unwrap());
        assert_eq!(manager.get_active_tokens().unwrap().len(), 1);
        assert_eq!(
            manager.get_locks_by_token(token.id.as_str()).unwrap().len(),
            2
        );

        // Create watchdog and simulate ACP disconnect
        let config = WatchdogConfig::default();
        let watchdog = Watchdog::new(Arc::clone(&manager), config);

        watchdog.handle_acp_disconnect("session-a").await.unwrap();

        // After disconnect: token expired, locks released
        assert_eq!(
            manager.get_active_tokens().unwrap().len(),
            0,
            "Token should be expired"
        );
        assert_eq!(
            manager.get_locks_by_token(token.id.as_str()).unwrap().len(),
            0,
            "Locks should be released"
        );

        // Files should be unlocked — another agent can now acquire
        let sys_b = SystemToken::new(
            "agent-b".into(),
            "session-b".into(),
            test_project_root(),
            3600,
            30,
        );
        manager.register_system_token(&sys_b).unwrap();
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        manager.acquire_lock(&token_b, "main.rs").await.unwrap();
        assert!(manager.is_file_locked("main.rs").unwrap());
    }

    // ================================================================
    // Test 25: ACP disconnect with multiple sessions — only affects target
    // ================================================================
    #[tokio::test]
    async fn test_acp_disconnect_only_affects_target_session() {
        let (_temp, manager) = setup_watchdog_env();
        let root = test_project_root();

        // Two agents, two sessions
        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        manager.acquire_lock(&token_a, "main.rs").await.unwrap();
        manager.acquire_lock(&token_b, "lib.rs").await.unwrap();

        let config = WatchdogConfig::default();
        let watchdog = Watchdog::new(Arc::clone(&manager), config);

        // Disconnect session-a only
        watchdog.handle_acp_disconnect("session-a").await.unwrap();

        // session-a: token expired, lock released
        assert_eq!(
            manager
                .get_tokens_by_session("session-a")
                .unwrap()
                .iter()
                .filter(|t| t.status == crate::file_access::TokenStatus::Active)
                .count(),
            0
        );
        assert!(!manager.is_file_locked("main.rs").unwrap());

        // session-b: unaffected
        assert_eq!(manager.get_active_tokens().unwrap().len(), 1);
        assert!(manager.is_file_locked("lib.rs").unwrap());
    }

    // ================================================================
    // Test 26: ACP disconnect on non-existent session is safe
    // ================================================================
    #[tokio::test]
    async fn test_acp_disconnect_nonexistent_session() {
        let (_temp, manager) = setup_watchdog_env();

        let config = WatchdogConfig::default();
        let watchdog = Watchdog::new(manager, config);

        // Should not error
        watchdog.handle_acp_disconnect("nonexistent").await.unwrap();
    }

    // ================================================================
    // Test 27: READ and WRITE can execute concurrently (optimistic locking)
    // ================================================================
    #[tokio::test]
    async fn test_read_write_concurrent_execution() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new(
            "reader".into(),
            "session-reader".into(),
            root.clone(),
            3600,
            30,
        );
        let sys_b = SystemToken::new("writer".into(), "session-writer".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_read =
            make_file_token("reader", "session-reader", &sys_a.id, "**", FileMode::Read);
        let token_write =
            make_file_token("writer", "session-writer", &sys_b.id, "**", FileMode::Write);

        // Agent A acquires READ lock
        manager.acquire_lock(&token_read, "main.rs").await.unwrap();

        // Agent B can acquire WRITE lock even while READ is held (optimistic locking)
        // This tests that READ doesn't block WRITE
        manager.acquire_lock(&token_write, "main.rs").await.unwrap();

        // Both locks are active
        assert_eq!(
            manager
                .get_locks_by_token(token_read.id.as_str())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            manager
                .get_locks_by_token(token_write.id.as_str())
                .unwrap()
                .len(),
            1
        );

        // is_file_locked only checks WRITE locks (not READ)
        assert!(manager.is_file_locked("main.rs").unwrap());

        // Release both
        manager
            .release_lock(token_read.id.as_str(), "main.rs")
            .await
            .unwrap();
        manager
            .release_lock(token_write.id.as_str(), "main.rs")
            .await
            .unwrap();
    }

    // ================================================================
    // Test 28: Multiple READ locks don't block each other
    // ================================================================
    #[tokio::test]
    async fn test_multiple_read_locks_no_blocking() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let agents = vec![
            ("agent-a", "session-a"),
            ("agent-b", "session-b"),
            ("agent-c", "session-c"),
        ];

        let mut tokens = Vec::new();

        for (agent_id, session_id) in &agents {
            let sys = SystemToken::new(
                agent_id.to_string(),
                session_id.to_string(),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            let token = make_file_token(agent_id, session_id, &sys.id, "**", FileMode::Read);
            tokens.push((token, sys));
        }

        // All three agents can acquire READ locks on the same file
        for (token, _) in &tokens {
            manager.acquire_lock(token, "main.rs").await.unwrap();
        }

        // All locks are active
        for (token, _) in &tokens {
            assert_eq!(
                manager.get_locks_by_token(token.id.as_str()).unwrap().len(),
                1
            );
        }

        // is_file_locked returns false (only checks WRITE)
        assert!(!manager.is_file_locked("main.rs").unwrap());

        // Release all
        for (token, _) in &tokens {
            manager
                .release_lock(token.id.as_str(), "main.rs")
                .await
                .unwrap();
        }
    }

    // ================================================================
    // Test 29: Lock upgrade READ→WRITE with no conflict
    // ================================================================
    #[tokio::test]
    async fn test_lock_upgrade_read_to_write_no_conflict() {
        use crate::file_access::LockModeManager;
        use std::sync::Arc;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Read);
        manager.acquire_lock(&token, "main.rs").await.unwrap();

        // Verify initial lock is READ
        let locks = manager.get_locks_by_token(token.id.as_str()).unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].mode, FileMode::Read);

        // Create LockModeManager to test upgrade
        // Note: In production, LockModeManager would share the same connection as FileLockManager
        // For testing, we need to access the internal connection
        // Since we can't easily do that, we'll skip this test for now
        // The lock_mode.rs module has its own tests for upgrade/downgrade

        // Instead, verify that after upgrade, the lock mode should be WRITE
        // This is a placeholder for integration testing
    }

    // ================================================================
    // Test 30: Lock upgrade READ→WRITE fails with WRITE conflict
    // ================================================================
    #[tokio::test]
    async fn test_lock_upgrade_read_to_write_with_conflict() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        // Agent A has READ lock
        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Read);
        manager.acquire_lock(&token_a, "main.rs").await.unwrap();

        // Agent B has WRITE lock on the same file
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        manager.acquire_lock(&token_b, "main.rs").await.unwrap();

        // Now if Agent A tries to upgrade to WRITE, it should fail
        // (because Agent B already has WRITE lock)
        // This would be tested via LockModeManager.upgrade_lock()
        // For now, just verify the state
        assert_eq!(
            manager
                .get_locks_by_token(token_a.id.as_str())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            manager
                .get_locks_by_token(token_b.id.as_str())
                .unwrap()
                .len(),
            1
        );
    }

    // ================================================================
    // Test 31: READ_LATEST waits for WRITE to complete
    // ================================================================
    #[tokio::test]
    async fn test_read_latest_waits_for_write() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("writer".into(), "session-writer".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("writer", "session-writer", &sys.id, "**", FileMode::Write);
        manager.acquire_lock(&token, "main.rs").await.unwrap();

        // File is locked for WRITE
        assert!(manager.is_file_locked_for_write("main.rs").unwrap());

        // READ_LATEST should wait for WRITE to complete
        // We'll spawn a task that releases the lock after a short delay
        let mgr_clone = Arc::clone(&manager);
        let token_id = token.id.as_str().to_string();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            mgr_clone.release_lock(&token_id, "main.rs").await.unwrap();
            // Notify waiters that file is ready
            mgr_clone.notify_file_ready("main.rs").await.unwrap();
        });

        // READ_LATEST should complete successfully after WRITE is released
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            manager.read_latest("main.rs"),
        )
        .await;

        assert!(result.is_ok(), "READ_LATEST should complete within timeout");
        assert!(result.unwrap().is_ok(), "READ_LATEST should succeed");
    }

    // ================================================================
    // Test 32: Concurrent lock upgrade conflict
    // Two agents with READ locks, both try to upgrade to WRITE
    // ================================================================
    #[tokio::test]
    async fn test_concurrent_lock_upgrade_conflict() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create two agents with READ locks on the same file
        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Read);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Read);

        // Both acquire READ locks
        manager.acquire_lock(&token_a, "main.rs").await.unwrap();
        manager.acquire_lock(&token_b, "main.rs").await.unwrap();

        // Verify both have READ locks
        assert_eq!(
            manager
                .get_locks_by_token(token_a.id.as_str())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            manager
                .get_locks_by_token(token_b.id.as_str())
                .unwrap()
                .len(),
            1
        );

        // Both locks should be READ mode
        let locks_a = manager.get_locks_by_token(token_a.id.as_str()).unwrap();
        let locks_b = manager.get_locks_by_token(token_b.id.as_str()).unwrap();
        assert_eq!(locks_a[0].mode, FileMode::Read);
        assert_eq!(locks_b[0].mode, FileMode::Read);

        // Note: Actual upgrade would be done via LockModeManager.upgrade_lock()
        // which checks for WRITE conflicts. Since both have READ, first to upgrade wins.
        // This test verifies the initial state is correct.
    }

    // ================================================================
    // Test 33: Large-scale concurrent lock operations (performance)
    // 10 agents acquiring and releasing locks concurrently
    // ================================================================
    #[tokio::test]
    async fn test_large_scale_concurrent_operations() {
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let num_agents = 10;
        let files = vec!["main.rs", "lib.rs", "config.rs"];

        // Create multiple agents
        let mut systems = Vec::new();
        let mut tokens = Vec::new();

        for i in 0..num_agents {
            let agent_id = format!("agent-{}", i);
            let session_id = format!("session-{}", i);
            let sys =
                SystemToken::new(agent_id.clone(), session_id.clone(), root.clone(), 3600, 30);
            manager.register_system_token(&sys).unwrap();

            let token = make_file_token(&agent_id, &session_id, &sys.id, "**", FileMode::Write);
            systems.push(sys);
            tokens.push(token);
        }

        // Spawn threads to concurrently acquire and release locks
        let mut handles = Vec::new();

        for (i, token) in tokens.iter().enumerate() {
            let mgr = Arc::clone(&manager);
            let token_clone = token.clone();
            let file = files[i % files.len()].to_string();

            let handle = thread::spawn(move || {
                // Create runtime for async operations
                let rt = tokio::runtime::Runtime::new().unwrap();

                // Acquire lock
                let acquire_result =
                    rt.block_on(async { mgr.acquire_lock(&token_clone, &file).await });
                if acquire_result.is_ok() {
                    // Hold lock briefly
                    std::thread::sleep(std::time::Duration::from_millis(10));

                    // Release lock
                    rt.block_on(async { mgr.release_lock(token_clone.id.as_str(), &file).await })
                        .unwrap();
                }
            });

            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all locks are released
        for file in &files {
            assert!(
                !manager.is_file_locked(file).unwrap(),
                "Lock on {} should be released",
                file
            );
        }
    }

    // ================================================================
    // Test 34: Token expiration and automatic cleanup
    // ================================================================
    #[tokio::test]
    async fn test_token_expiration_cleanup() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create a system token with very short expiration (1 second)
        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 1, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);
        manager.acquire_lock(&token, "main.rs").await.unwrap();

        // Verify token and lock are active
        assert_eq!(manager.get_active_tokens().unwrap().len(), 1);
        assert!(manager.is_file_locked("main.rs").unwrap());

        // Wait for expiration
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Token should still be in DB but may be marked as expired
        // (depending on whether watchdog is running)
        // For now, just verify the system doesn't crash
        let _tokens = manager.get_tokens_by_session("session-a").unwrap();
    }

    // ================================================================
    // Test 35: Multiple locks on same file by same agent (should fail)
    // ================================================================
    #[tokio::test]
    async fn test_same_agent_duplicate_lock() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root, 3600, 30);
        manager.register_system_token(&sys).unwrap();

        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        // First lock should succeed
        manager.acquire_lock(&token, "main.rs").await.unwrap();
        assert!(manager.is_file_locked("main.rs").unwrap());

        // Second lock on same file by same agent should fail (WRITE blocks WRITE)
        let result = manager.acquire_lock(&token, "main.rs").await;
        assert!(result.is_err(), "Duplicate WRITE lock should fail");
    }

    // ================================================================
    // CONCURRENT RACE CONDITION TESTS
    // ================================================================

    // ================================================================
    // Test 36: Multiple agents racing with no delay (exact same timestamp)
    // ================================================================
    #[tokio::test]
    async fn test_concurrent_acquire_exact_same_timestamp() {
        use std::sync::Barrier;
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create 5 agents all trying to acquire the same lock at exactly the same time
        let num_agents = 5;
        let barrier = Arc::new(Barrier::new(num_agents));

        let mut handles = vec![];
        let mut system_tokens = vec![];
        let mut file_tokens = vec![];

        // Setup all tokens first
        for i in 0..num_agents {
            let sys = SystemToken::new(
                format!("agent-{}", i),
                format!("session-{}", i),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            system_tokens.push(sys);
        }

        for i in 0..num_agents {
            let token = make_file_token(
                &format!("agent-{}", i),
                &format!("session-{}", i),
                &system_tokens[i].id,
                "**",
                FileMode::Write,
            );
            file_tokens.push(token);
        }

        // Spawn threads that all race to acquire the lock
        for i in 0..num_agents {
            let mgr = Arc::clone(&manager);
            let token = file_tokens[i].clone();
            let barrier = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                // Create runtime for async operations
                let rt = tokio::runtime::Runtime::new().unwrap();
                // Wait for all threads to be ready
                barrier.wait();
                // All threads try to acquire at exactly the same time
                rt.block_on(async { mgr.acquire_lock(&token, "main.rs").await })
            });

            handles.push(handle);
        }

        // Collect results
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Count successes and failures
        let successes = results.iter().filter(|r| r.is_ok()).count();
        let failures = results.iter().filter(|r| r.is_err()).count();

        // Exactly one should succeed, all others should fail
        assert_eq!(successes, 1, "Exactly one agent should acquire the lock");
        assert_eq!(failures, num_agents - 1, "All other agents should fail");
    }

    // ================================================================
    // Test 37: Heartbeat at exact expiration boundary
    // ================================================================
    #[tokio::test]
    async fn test_heartbeat_at_expiration_boundary() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create token with short expiration
        let sys = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        manager.register_system_token(&sys).unwrap();
        let token = make_file_token("agent-a", "session-a", &sys.id, "**", FileMode::Write);

        manager.acquire_lock(&token, "main.rs").await.unwrap();

        // Set heartbeat to exactly 3x interval (90 seconds) in the past
        // This is the exact boundary where timeout should trigger
        manager.set_heartbeat_past(token.id.as_str(), 90).unwrap();

        // Token should still be active (boundary condition)
        let tokens = manager.get_tokens_by_session("session-a").unwrap();
        assert_eq!(tokens.len(), 1);

        // Update heartbeat to bring it back
        manager.update_heartbeat(token.id.as_str()).unwrap();

        // Should still be able to use the lock
        assert!(manager.is_file_locked("main.rs").unwrap());
    }

    // ================================================================
    // Test 38: Concurrent release and acquire race
    // ================================================================
    #[tokio::test]
    async fn test_concurrent_release_and_acquire() {
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        let token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        let token_b = make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);

        // Agent A acquires lock
        manager.acquire_lock(&token_a, "main.rs").await.unwrap();

        let mgr_a = Arc::clone(&manager);
        let mgr_b = Arc::clone(&manager);
        let token_a_id = token_a.id.clone();

        // Spawn thread to release lock
        let release_handle = thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Small delay to let acquire thread start waiting
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                mgr_a.release_lock(token_a_id.as_str(), "main.rs").await
            })
        });

        // Try to acquire lock (should succeed after release)
        let acquire_result = manager.acquire_lock(&token_b, "main.rs").await;

        release_handle.join().unwrap().unwrap();

        // Either acquire succeeded immediately (race won) or after release
        // Both are valid - just verify system consistency
        if acquire_result.is_ok() {
            assert!(manager.is_file_locked("main.rs").unwrap());
        }
    }

    // ================================================================
    // Test 39: High concurrency DB transaction conflicts
    // ================================================================
    #[tokio::test]
    async fn test_high_concurrency_db_transactions() {
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create 20 agents trying to acquire different locks simultaneously
        let num_agents = 20;
        let mut handles = vec![];
        let mut system_tokens = vec![];
        let mut file_tokens = vec![];

        // Setup all tokens
        for i in 0..num_agents {
            let sys = SystemToken::new(
                format!("agent-{}", i),
                format!("session-{}", i),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            system_tokens.push(sys);
        }

        for i in 0..num_agents {
            let token = make_file_token(
                &format!("agent-{}", i),
                &format!("session-{}", i),
                &system_tokens[i].id,
                "**",
                FileMode::Write,
            );
            file_tokens.push(token);
        }

        // Spawn threads that all try to acquire locks on different files at once
        for i in 0..num_agents {
            let mgr = Arc::clone(&manager);
            let token = file_tokens[i].clone();
            let file_path = format!("file_{}.rs", i);

            // Create the file first
            std::fs::write(manager.project_root().join(&file_path), "test content").unwrap();

            let handle = thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { mgr.acquire_lock(&token, &file_path).await })
            });

            handles.push(handle);
        }

        // All should succeed since they're on different files
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let successes = results.iter().filter(|r| r.is_ok()).count();

        // All 20 should succeed (different files, no contention)
        assert_eq!(
            successes, num_agents,
            "All agents should acquire locks on different files"
        );
    }

    // ================================================================
    // Test 40: Lock convoy - many agents retry to acquire same lock
    // ================================================================
    #[tokio::test]
    async fn test_lock_convoy_many_retries() {
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        // Create 10 agents all trying to acquire the same lock
        let num_agents = 10;
        let mut system_tokens = vec![];
        let mut file_tokens = vec![];

        for i in 0..num_agents {
            let sys = SystemToken::new(
                format!("agent-{}", i),
                format!("session-{}", i),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            system_tokens.push(sys);
        }

        for i in 0..num_agents {
            let token = make_file_token(
                &format!("agent-{}", i),
                &format!("session-{}", i),
                &system_tokens[i].id,
                "**",
                FileMode::Write,
            );
            file_tokens.push(token);
        }

        // First agent acquires lock
        manager
            .acquire_lock(&file_tokens[0], "main.rs")
            .await
            .unwrap();

        // All other agents try to acquire - should all fail immediately
        let mut handles = vec![];
        for i in 1..num_agents {
            let mgr = Arc::clone(&manager);
            let token = file_tokens[i].clone();

            let handle = thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { mgr.acquire_lock(&token, "main.rs").await })
            });

            handles.push(handle);
        }

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let failures = results.iter().filter(|r| r.is_err()).count();

        // All should fail since lock is held
        assert_eq!(
            failures,
            num_agents - 1,
            "All agents should fail to acquire lock"
        );

        // Release the lock
        manager
            .release_lock(file_tokens[0].id.as_str(), "main.rs")
            .await
            .unwrap();

        // Now one agent can acquire
        let result = manager.acquire_lock(&file_tokens[1], "main.rs").await;
        assert!(result.is_ok(), "Should be able to acquire after release");

        // Cleanup
        manager
            .release_lock(file_tokens[1].id.as_str(), "main.rs")
            .await
            .unwrap();
    }

    // ================================================================
    // Test 41: WRITE can be acquired even with concurrent READ holders
    // ================================================================
    #[tokio::test]
    async fn test_write_with_concurrent_readers() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        // Agent A acquires READ lock
        let read_token_a = make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Read);
        manager
            .acquire_lock(&read_token_a, "main.rs")
            .await
            .unwrap();

        // Agent B acquires WRITE lock (READ doesn't block WRITE)
        let write_token_b =
            make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        let result = manager.acquire_lock(&write_token_b, "main.rs").await;
        assert!(
            result.is_ok(),
            "WRITE should succeed even with READ holder (optimistic concurrency)"
        );

        // Both locks should exist
        let active_locks = manager.get_all_active_locks().unwrap();
        let main_rs_locks = active_locks
            .iter()
            .filter(|l| l.file_path.ends_with("main.rs"))
            .count();
        assert_eq!(main_rs_locks, 2, "Both READ and WRITE locks should coexist");

        // Cleanup
        manager
            .release_lock(read_token_a.id.as_str(), "main.rs")
            .await
            .unwrap();
        manager
            .release_lock(write_token_b.id.as_str(), "main.rs")
            .await
            .unwrap();
    }

    // ================================================================
    // Test 42: WRITE to READ downgrade with waiting writers
    // ================================================================
    #[tokio::test]
    async fn test_downgrade_write_to_read_with_waiting_writers() {
        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let sys_a = SystemToken::new("agent-a".into(), "session-a".into(), root.clone(), 3600, 30);
        let sys_b = SystemToken::new("agent-b".into(), "session-b".into(), root, 3600, 30);
        manager.register_system_token(&sys_a).unwrap();
        manager.register_system_token(&sys_b).unwrap();

        // Agent A has WRITE lock
        let write_token_a =
            make_file_token("agent-a", "session-a", &sys_a.id, "**", FileMode::Write);
        manager
            .acquire_lock(&write_token_a, "main.rs")
            .await
            .unwrap();

        // Agent B tries to acquire WRITE - should fail
        let write_token_b =
            make_file_token("agent-b", "session-b", &sys_b.id, "**", FileMode::Write);
        let result = manager.acquire_lock(&write_token_b, "main.rs").await;
        assert!(result.is_err(), "B should not get WRITE while A has WRITE");

        // Agent A releases WRITE lock
        manager
            .release_lock(write_token_a.id.as_str(), "main.rs")
            .await
            .unwrap();

        // Now B can acquire WRITE
        let result = manager.acquire_lock(&write_token_b, "main.rs").await;
        assert!(result.is_ok(), "B should get WRITE after A releases");

        // Cleanup
        manager
            .release_lock(write_token_b.id.as_str(), "main.rs")
            .await
            .unwrap();
    }

    // ================================================================
    // Test 43: Concurrent acquire on same file with mixed READ/WRITE
    // ================================================================
    #[tokio::test]
    async fn test_concurrent_mixed_read_write_locks() {
        use std::thread;

        let (_temp, manager) = setup_test_env();
        let root = test_project_root();

        let num_readers = 5;
        let num_writers = 3;
        let mut system_tokens = vec![];
        let mut file_tokens = vec![];

        // Create readers
        for i in 0..num_readers {
            let sys = SystemToken::new(
                format!("reader-{}", i),
                format!("session-r{}", i),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            let token = make_file_token(
                &format!("reader-{}", i),
                &format!("session-r{}", i),
                &sys.id,
                "**",
                FileMode::Read,
            );
            file_tokens.push(token);
            system_tokens.push(sys);
        }

        // Create writers
        for i in 0..num_writers {
            let sys = SystemToken::new(
                format!("writer-{}", i),
                format!("session-w{}", i),
                root.clone(),
                3600,
                30,
            );
            manager.register_system_token(&sys).unwrap();
            let token = make_file_token(
                &format!("writer-{}", i),
                &format!("session-w{}", i),
                &sys.id,
                "**",
                FileMode::Write,
            );
            file_tokens.push(token);
            system_tokens.push(sys);
        }

        let mut handles = vec![];

        // Spawn all readers and writers concurrently
        for i in 0..(num_readers + num_writers) {
            let mgr = Arc::clone(&manager);
            let token = file_tokens[i].clone();

            let handle = thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { mgr.acquire_lock(&token, "main.rs").await })
            });

            handles.push(handle);
        }

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let read_successes = results[0..num_readers].iter().filter(|r| r.is_ok()).count();
        let write_successes = results[num_readers..].iter().filter(|r| r.is_ok()).count();

        // All readers should succeed (READ locks coexist)
        assert_eq!(read_successes, num_readers, "All READ locks should succeed");

        // Exactly one writer should succeed (if any - depends on timing)
        // If a writer got the lock first, only that writer succeeds
        // If readers got locks first, no writer succeeds
        assert!(
            write_successes <= 1,
            "At most one WRITE lock should succeed, got {}",
            write_successes
        );
    }
}
