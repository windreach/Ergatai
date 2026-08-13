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
        let mut new_results = Vec::new();

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
        let mut plans = Vec::new();

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
