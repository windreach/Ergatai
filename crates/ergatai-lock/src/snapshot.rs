//! Git snapshot management for file access control
//!
//! Provides Copy-on-Write snapshots before file modifications to prevent TOCTOU attacks.
//! Uses git2 crate for safe git operations (no shell command injection).
//!
//! ## Architecture
//!
//! ```text
//! Before WRITE:
//!   Agent requests WRITE lock on src/auth.rs
//!     ↓
//!   FileLockManager.create_snapshot()
//!     ↓
//!   Copy file to temp location
//!     ↓
//!   git hash-object -w <temp_file>
//!     ↓
//!   Store (file_path, git_hash) in snapshots table
//!     ↓
//!   Clean up temp file
//!     ↓
//!   Grant WRITE lock
//!
//! READ_HISTORY:
//!   Agent requests READ_HISTORY on src/auth.rs
//!     ↓
//!   Query snapshots table for latest git_hash
//!     ↓
//!   git cat-file blob <git_hash>
//!     ↓
//!   Return historical content
//! ```

use chrono::Utc;
use ergatai_error::ErgataiError;
use git2::{Oid, Repository};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{debug, info};

/// Git snapshot manager for file access control
///
/// Creates and manages git-based snapshots for Copy-on-Write semantics.
/// Thread-safe via internal Mutex on the git repository.
pub struct SnapshotManager {
    /// Git repository path
    repo_path: PathBuf,
    /// Cached canonical repo path (avoids repeated I/O on every snapshot)
    canonical_repo_path: PathBuf,
    /// Git repository instance (wrapped in Mutex for thread safety)
    repo: Mutex<Repository>,
}

impl SnapshotManager {
    /// Create a new snapshot manager for the given repository path
    pub fn new(repo_path: &Path) -> Result<Self, ErgataiError> {
        let repo = Repository::open(repo_path).map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to open git repository at {:?}: {}",
                repo_path, e
            ))
        })?;

        let canonical_repo_path = repo_path.canonicalize().map_err(|e| {
            ErgataiError::internal(format!("Failed to canonicalize repo path: {}", e))
        })?;

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            canonical_repo_path,
            repo: Mutex::new(repo),
        })
    }

    /// Create a snapshot of a file before modification (Copy-on-Write)
    ///
    /// This implements the H3 fix: read file → hash content → store in git object store
    /// Prevents TOCTOU by snapshotting the exact version before WRITE.
    ///
    /// # Arguments
    /// * `file_path` - Relative path to the file from project root
    /// * `agent_id` - Agent requesting the snapshot
    ///
    /// # Returns
    /// Git hash of the snapshot
    ///
    /// # Errors
    /// Returns error if file is too large (>100MB) or cannot be read
    pub fn create_snapshot(&self, file_path: &str, agent_id: &str) -> Result<String, ErgataiError> {
        let full_path = self.repo_path.join(file_path);

        // M4 fix: Reject symlinks explicitly to prevent symlink-based path traversal
        if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
            if metadata.file_type().is_symlink() {
                return Err(ErgataiError::InvalidArgument(format!(
                    "Symlink not allowed in snapshot path: {:?}",
                    file_path
                )));
            }
        }

        // M4 fix: Canonicalize first (resolves any remaining traversal), then check existence
        let canonical_file = match full_path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // File doesn't exist or is inaccessible
                debug!(
                    file_path = file_path,
                    "File does not exist, skipping snapshot"
                );
                return Ok(String::new());
            }
        };

        // Path traversal check using cached canonical repo path
        if !canonical_file.starts_with(&self.canonical_repo_path) {
            return Err(ErgataiError::InvalidArgument(format!(
                "Path traversal detected: {:?} resolves outside project root",
                file_path
            )));
        }

        // Check file size (limit: 100MB) to prevent OOM
        let metadata = fs::metadata(&full_path)
            .map_err(|e| ErgataiError::internal(format!("Failed to read file metadata: {}", e)))?;

        const MAX_SNAPSHOT_SIZE: u64 = 100 * 1024 * 1024; // 100MB
        if metadata.len() > MAX_SNAPSHOT_SIZE {
            return Err(ErgataiError::InvalidArgument(format!(
                "File too large for snapshot: {} bytes (max: {} bytes)",
                metadata.len(),
                MAX_SNAPSHOT_SIZE
            )));
        }

        // Read file content directly (no temp file needed)
        let content = fs::read(&full_path).map_err(|e| {
            ErgataiError::internal(format!("Failed to read file {}: {}", file_path, e))
        })?;

        // Create blob in git object store (lock repository for thread safety)
        let repo = self
            .repo
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to lock git repository: {}", e)))?;
        let oid = repo.blob(&content).map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to create git blob for {}: {}",
                file_path, e
            ))
        })?;

        let git_hash = oid.to_string();

        info!(
            file_path = file_path,
            git_hash = %git_hash,
            agent_id = agent_id,
            size_bytes = metadata.len(),
            "Created snapshot"
        );

        Ok(git_hash)
    }

    /// Read a file from a git snapshot (READ_HISTORY)
    ///
    /// Retrieves the historical content of a file using its git hash.
    ///
    /// # Arguments
    /// * `git_hash` - Git hash of the snapshot
    ///
    /// # Returns
    /// File content as bytes
    pub fn read_snapshot(&self, git_hash: &str) -> Result<Vec<u8>, ErgataiError> {
        if git_hash.is_empty() {
            return Err(ErgataiError::NotFound(
                "Cannot read snapshot: empty git hash".to_string(),
            ));
        }

        // Parse git hash
        let oid = Oid::from_str(git_hash).map_err(|e| {
            ErgataiError::InvalidArgument(format!("Invalid git hash '{}': {}", git_hash, e))
        })?;

        // Read blob from git object store (lock repository for thread safety)
        let repo = self
            .repo
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to lock git repository: {}", e)))?;
        let blob = repo.find_blob(oid).map_err(|e| {
            ErgataiError::NotFound(format!("Failed to find git blob {}: {}", git_hash, e))
        })?;

        Ok(blob.content().to_vec())
    }

    /// Get the latest snapshot for a file from the snapshots table
    ///
    /// This is a helper for READ_HISTORY to find the most recent snapshot.
    ///
    /// # Arguments
    /// * `conn` - SQLite connection to the lock database
    /// * `file_path` - Relative path to the file
    ///
    /// # Returns
    /// Git hash of the latest snapshot, or None if no snapshot exists
    pub fn get_latest_snapshot(
        conn: &rusqlite::Connection,
        file_path: &str,
    ) -> Result<Option<String>, ErgataiError> {
        let git_hash: Option<String> = match conn.query_row(
            "SELECT git_hash FROM snapshots
                 WHERE file_path = ?1
                 ORDER BY created_at DESC
                 LIMIT 1",
            rusqlite::params![file_path],
            |row| row.get(0),
        ) {
            Ok(hash) => Some(hash),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                return Err(ErgataiError::internal(format!(
                    "Failed to query latest snapshot: {}",
                    e
                )));
            }
        };

        Ok(git_hash)
    }

    /// Store a snapshot record in the database
    ///
    /// # Arguments
    /// * `conn` - SQLite connection to the lock database
    /// * `file_path` - Relative path to the file
    /// * `git_hash` - Git hash of the snapshot
    /// * `agent_id` - Agent who created the snapshot
    pub fn store_snapshot_record(
        conn: &rusqlite::Connection,
        file_path: &str,
        git_hash: &str,
        agent_id: &str,
    ) -> Result<(), ErgataiError> {
        let now = Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO snapshots (id, file_path, git_hash, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, file_path, git_hash, now, agent_id],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to store snapshot record: {}", e)))?;

        debug!(
            file_path = file_path,
            git_hash = git_hash,
            agent_id = agent_id,
            "Stored snapshot record"
        );

        Ok(())
    }

    /// Clean up old snapshots based on age
    ///
    /// Removes snapshots older than the specified number of days.
    ///
    /// # Arguments
    /// * `conn` - SQLite connection to the lock database
    /// * `days_to_keep` - Number of days to keep snapshots
    ///
    /// # Returns
    /// Number of snapshots deleted
    pub fn cleanup_old_snapshots(
        conn: &rusqlite::Connection,
        days_to_keep: u32,
    ) -> Result<usize, ErgataiError> {
        // L7 fix: prevent accidental deletion of all snapshots
        if days_to_keep == 0 {
            return Err(ErgataiError::InvalidArgument(
                "days_to_keep must be > 0 to prevent accidental deletion of all snapshots"
                    .to_string(),
            ));
        }

        let cutoff = Utc::now() - chrono::Duration::days(days_to_keep as i64);

        let deleted = conn
            .execute(
                "DELETE FROM snapshots WHERE created_at < ?1",
                rusqlite::params![cutoff.to_rfc3339()],
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to cleanup old snapshots: {}", e))
            })?;

        info!(
            deleted = deleted,
            days_to_keep = days_to_keep,
            "Cleaned up old snapshots"
        );

        Ok(deleted)
    }

    /// Cleanup snapshots to enforce disk size limit
    ///
    /// Removes oldest snapshots until total size is under the limit.
    /// This is a best-effort cleanup - git objects may not be immediately freed.
    ///
    /// # Arguments
    /// * `conn` - SQLite connection
    /// * `max_bytes` - Maximum total size in bytes (e.g., 500MB = 500_000_000)
    ///
    /// # Returns
    /// Number of snapshots deleted
    pub fn cleanup_snapshots_by_size(
        conn: &rusqlite::Connection,
        max_bytes: u64,
    ) -> Result<usize, ErgataiError> {
        // Get all snapshots ordered by creation date (oldest first)
        let mut stmt = conn
            .prepare("SELECT id, file_path, created_at FROM snapshots ORDER BY created_at ASC")?;

        let snapshots: Vec<(String, String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        // Estimate total size (approximate: 10KB per snapshot average)
        // In production, this could be enhanced to query git object sizes
        let estimated_size_per_snapshot = 10_000u64;
        let total_estimated = snapshots.len() as u64 * estimated_size_per_snapshot;

        if total_estimated <= max_bytes {
            debug!(
                snapshot_count = snapshots.len(),
                estimated_bytes = total_estimated,
                max_bytes = max_bytes,
                "Snapshot size within limit, no cleanup needed"
            );
            return Ok(0);
        }

        // Calculate how many to delete
        let target_count = (max_bytes / estimated_size_per_snapshot) as usize;
        let to_delete = snapshots.len().saturating_sub(target_count);

        if to_delete == 0 {
            return Ok(0);
        }

        // Delete oldest snapshots
        let ids_to_delete: Vec<String> = snapshots
            .iter()
            .take(to_delete)
            .map(|(id, _, _)| id.clone())
            .collect();

        let mut deleted = 0;
        for id in &ids_to_delete {
            let count =
                conn.execute("DELETE FROM snapshots WHERE id = ?1", rusqlite::params![id])?;
            deleted += count;
        }

        info!(
            deleted = deleted,
            estimated_bytes_before = total_estimated,
            max_bytes = max_bytes,
            "Cleaned up snapshots to enforce size limit"
        );

        Ok(deleted)
    }

    /// Run git garbage collection to free unreferenced objects
    ///
    /// This should be run periodically (e.g., daily) to reclaim disk space
    /// from deleted snapshots.
    pub fn run_git_gc(repo: &Repository) -> Result<(), ErgataiError> {
        use std::process::Command;

        let repo_path = repo
            .path()
            .parent()
            .ok_or_else(|| ErgataiError::internal("Failed to get repo path"))?;

        // Run git gc --auto
        let output = Command::new("git")
            .arg("gc")
            .arg("--auto")
            .current_dir(repo_path)
            .output()
            .map_err(|e| ErgataiError::internal(format!("Failed to run git gc: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ErgataiError::internal(format!("git gc failed: {}", stderr)));
        }

        info!("Git garbage collection completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_repo() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();

        // Set local git config so signature() works without global config
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "Test User").unwrap();
            config.set_str("user.email", "test@example.com").unwrap();
        }

        // Create initial commit
        let mut index = repo.index().unwrap();
        let oid = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(oid).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                .unwrap();
        } // tree is dropped here, releasing the borrow on repo

        (temp_dir, repo)
    }

    #[test]
    fn test_create_snapshot() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();
        let git_hash = manager.create_snapshot("test.txt", "test-agent").unwrap();

        // Git hash should be non-empty
        assert!(!git_hash.is_empty());
        assert_eq!(git_hash.len(), 40); // SHA-1 hash length
    }

    #[test]
    fn test_read_snapshot() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();
        let git_hash = manager.create_snapshot("test.txt", "test-agent").unwrap();

        // Read snapshot
        let content = manager.read_snapshot(&git_hash).unwrap();
        assert_eq!(content, b"test content");
    }

    #[test]
    fn test_snapshot_nonexistent_file() {
        let (temp_dir, _repo) = create_test_repo();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();
        let git_hash = manager
            .create_snapshot("nonexistent.txt", "test-agent")
            .unwrap();

        // Should return empty hash for non-existent files
        assert_eq!(git_hash, "");
    }

    #[test]
    fn test_copy_on_write() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "version 1").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();

        // Create snapshot of version 1
        let hash1 = manager.create_snapshot("test.txt", "test-agent").unwrap();

        // Modify file
        fs::write(&test_file, "version 2").unwrap();

        // Create snapshot of version 2
        let hash2 = manager.create_snapshot("test.txt", "test-agent").unwrap();

        // Hashes should be different
        assert_ne!(hash1, hash2);

        // Read version 1
        let content1 = manager.read_snapshot(&hash1).unwrap();
        assert_eq!(content1, b"version 1");

        // Read version 2
        let content2 = manager.read_snapshot(&hash2).unwrap();
        assert_eq!(content2, b"version 2");
    }

    #[test]
    fn test_snapshot_restore() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "original content").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();

        // Create snapshot
        let hash = manager.create_snapshot("test.txt", "test-agent").unwrap();

        // Modify file
        fs::write(&test_file, "modified content").unwrap();

        // Verify file is modified
        let content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "modified content");

        // Restore from snapshot
        let snapshot_content = manager.read_snapshot(&hash).unwrap();
        fs::write(&test_file, snapshot_content).unwrap();

        // Verify file is restored
        let restored_content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(restored_content, "original content");
    }

    #[test]
    fn test_snapshot_path_traversal_protection() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a file outside the project root
        let outside_file = temp_dir.path().parent().unwrap().join("outside.txt");
        fs::write(&outside_file, "outside content").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();

        // Try to access file outside project root using path traversal
        // This should fail because the canonical path doesn't start with repo_path
        let result = manager.create_snapshot("../outside.txt", "test-agent");
        assert!(result.is_err(), "Path traversal should be rejected");
        assert!(result.unwrap_err().to_string().contains("Path traversal"));

        // Clean up
        fs::remove_file(&outside_file).ok();
    }

    #[test]
    fn test_concurrent_snapshots() {
        use std::sync::Arc;
        use std::thread;

        let (temp_dir, _repo) = create_test_repo();

        // Create multiple test files
        for i in 0..5 {
            let test_file = temp_dir.path().join(format!("file{}.txt", i));
            fs::write(&test_file, format!("content {}", i)).unwrap();
        }

        let manager = Arc::new(SnapshotManager::new(temp_dir.path()).unwrap());

        // Spawn threads to create snapshots concurrently
        let mut handles = Vec::new();

        for i in 0..5 {
            let mgr = Arc::clone(&manager);
            let handle = thread::spawn(move || {
                let file_name = format!("file{}.txt", i);
                let agent_id = format!("agent-{}", i);
                mgr.create_snapshot(&file_name, &agent_id).unwrap()
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let mut hashes = Vec::new();
        for handle in handles {
            let hash = handle.join().unwrap();
            assert!(!hash.is_empty(), "Snapshot should succeed");
            hashes.push(hash);
        }

        // All hashes should be unique
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "Snapshots should be unique");
            }
        }
    }

    #[test]
    fn test_large_file_snapshot() {
        let (temp_dir, _repo) = create_test_repo();

        // Create a large test file (1MB)
        let test_file = temp_dir.path().join("large.txt");
        let large_content = "x".repeat(1024 * 1024); // 1MB
        fs::write(&test_file, &large_content).unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();

        // Create snapshot
        let hash = manager.create_snapshot("large.txt", "test-agent").unwrap();
        assert!(!hash.is_empty());

        // Read snapshot
        let content = manager.read_snapshot(&hash).unwrap();
        assert_eq!(content.len(), large_content.len());
        assert_eq!(content, large_content.as_bytes());
    }

    #[test]
    fn test_empty_file_snapshot() {
        let (temp_dir, _repo) = create_test_repo();

        // Create an empty test file
        let test_file = temp_dir.path().join("empty.txt");
        fs::write(&test_file, "").unwrap();

        let manager = SnapshotManager::new(temp_dir.path()).unwrap();

        // Create snapshot
        let hash = manager.create_snapshot("empty.txt", "test-agent").unwrap();
        assert!(!hash.is_empty());

        // Read snapshot
        let content = manager.read_snapshot(&hash).unwrap();
        assert_eq!(content.len(), 0);
    }
}
