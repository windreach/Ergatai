// Plan Watcher - Monitors task plans and result files
// Detects when agents complete tasks and triggers merging

use std::collections::HashSet;
use std::path::PathBuf;

/// Simple polling-based watcher
pub struct PollingWatcher {
    plan_dir: PathBuf,
    results_dir: PathBuf,
    seen_results: HashSet<String>,
}

impl PollingWatcher {
    pub fn new(plan_dir: PathBuf) -> Self {
        let results_dir = plan_dir.join("results");
        Self {
            plan_dir,
            results_dir,
            seen_results: HashSet::new(),
        }
    }

    /// Poll for new result files
    pub async fn poll_new_results(&mut self) -> Vec<(String, String, PathBuf)> {
        // Pre-allocate with a reasonable default
        let mut new_results = Vec::with_capacity(8);

        let mut entries = match tokio::fs::read_dir(&self.results_dir).await {
            Ok(rd) => rd,
            Err(_) => return new_results,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                    if !self.seen_results.contains(file_name) {
                        self.seen_results.insert(file_name.to_string());
                        if let Some((task_id, agent_name)) = file_name.split_once('-') {
                            new_results.push((task_id.to_string(), agent_name.to_string(), path));
                        }
                    }
                }
            }
        }

        new_results
    }

    /// Get list of plan files
    pub async fn list_plans(&self) -> Vec<PathBuf> {
        // Pre-allocate with a reasonable default
        let mut plans = Vec::with_capacity(8);

        let mut entries = match tokio::fs::read_dir(&self.plan_dir).await {
            Ok(rd) => rd,
            Err(_) => return plans,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                plans.push(path);
            }
        }

        plans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_watcher(plan_dir: PathBuf) -> PollingWatcher {
        PollingWatcher::new(plan_dir)
    }

    #[tokio::test]
    async fn test_polling_watcher_construction() {
        let dir = tempdir().unwrap();
        let plan_dir = dir.path().to_path_buf();
        let watcher = make_watcher(plan_dir.clone());
        assert_eq!(watcher.plan_dir, plan_dir);
        assert_eq!(watcher.results_dir, plan_dir.join("results"));
        assert!(watcher.seen_results.is_empty());
    }

    #[tokio::test]
    async fn test_poll_new_results_empty_dir() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        let mut watcher = make_watcher(dir.path().to_path_buf());
        let results = watcher.poll_new_results().await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_poll_new_results_detects_md_files() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        // Create result files with "task_id-agent_name.md" pattern
        tokio::fs::write(results_dir.join("task1-agentA.md"), "result A")
            .await
            .unwrap();
        tokio::fs::write(results_dir.join("task1-agentB.md"), "result B")
            .await
            .unwrap();

        let mut watcher = make_watcher(dir.path().to_path_buf());
        let results = watcher.poll_new_results().await;
        assert_eq!(results.len(), 2);

        // Verify task_id/agent_name extraction
        let names: Vec<_> = results
            .iter()
            .map(|(tid, agent, _)| (tid.as_str(), agent.as_str()))
            .collect();
        assert!(names.contains(&("task1", "agentA")));
        assert!(names.contains(&("task1", "agentB")));
    }

    #[tokio::test]
    async fn test_poll_new_results_deduplicates_seen() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        tokio::fs::write(results_dir.join("task1-agentA.md"), "result A")
            .await
            .unwrap();

        let mut watcher = make_watcher(dir.path().to_path_buf());
        let first = watcher.poll_new_results().await;
        assert_eq!(first.len(), 1);
        // Second poll should return no new results (dedup)
        let second = watcher.poll_new_results().await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn test_poll_new_results_detects_new_after_first_poll() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        tokio::fs::write(results_dir.join("task1-agentA.md"), "A")
            .await
            .unwrap();

        let mut watcher = make_watcher(dir.path().to_path_buf());
        let first = watcher.poll_new_results().await;
        assert_eq!(first.len(), 1);

        // Add a new result file
        tokio::fs::write(results_dir.join("task2-agentB.md"), "B")
            .await
            .unwrap();
        let second = watcher.poll_new_results().await;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, "task2");
        assert_eq!(second[0].1, "agentB");
    }

    #[tokio::test]
    async fn test_poll_new_results_ignores_non_md_files() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        tokio::fs::write(results_dir.join("task1-agentA.txt"), "text")
            .await
            .unwrap();
        tokio::fs::write(results_dir.join("task1-agentB.json"), "{}")
            .await
            .unwrap();

        let mut watcher = make_watcher(dir.path().to_path_buf());
        let results = watcher.poll_new_results().await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_poll_new_results_ignores_files_without_dash() {
        let dir = tempdir().unwrap();
        let results_dir = dir.path().join("results");
        tokio::fs::create_dir_all(&results_dir).await.unwrap();
        // File with no dash separator should be skipped
        tokio::fs::write(results_dir.join("nodash.md"), "x")
            .await
            .unwrap();

        let mut watcher = make_watcher(dir.path().to_path_buf());
        let results = watcher.poll_new_results().await;
        // No (task_id, agent) tuple should be produced
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_list_plans_empty() {
        let dir = tempdir().unwrap();
        let watcher = make_watcher(dir.path().to_path_buf());
        let plans = watcher.list_plans().await;
        assert!(plans.is_empty());
    }

    #[tokio::test]
    async fn test_list_plans_returns_md_files() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("plan1.md"), "plan 1")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("plan2.md"), "plan 2")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("notes.txt"), "not a plan")
            .await
            .unwrap();

        let watcher = make_watcher(dir.path().to_path_buf());
        let plans = watcher.list_plans().await;
        assert_eq!(plans.len(), 2);
        for plan in &plans {
            assert_eq!(plan.extension().and_then(|e| e.to_str()), Some("md"));
        }
    }

    #[tokio::test]
    async fn test_poll_new_results_missing_results_dir_returns_empty() {
        // results_dir doesn't exist — should gracefully return empty
        let dir = tempdir().unwrap();
        let mut watcher = make_watcher(dir.path().to_path_buf());
        let results = watcher.poll_new_results().await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_list_plans_missing_plan_dir_returns_empty() {
        let nonexistent = PathBuf::from("/tmp/definitely-not-a-real-dir-xyz");
        let watcher = make_watcher(nonexistent);
        let plans = watcher.list_plans().await;
        assert!(plans.is_empty());
    }
}
