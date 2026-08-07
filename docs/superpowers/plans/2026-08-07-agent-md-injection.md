# AGENT.md 强制注入实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 DAG 调度的 Agent 启动时强制注入 AGENT.md 项目上下文，确保所有 Agent 了解项目架构和协作规范。

**Architecture:** 修改 `AgentLauncher::create_agent_instruction()` 函数，在生成 `.ergatai-task.md` 指令文件时读取 `.ergatai/AGENT.md` 并注入到指令开头。使用异步 IO 和安全的 UTF-8 截断。

**Tech Stack:** Rust, tokio, anyhow

## Global Constraints

- 使用 `tokio::fs` 而非 `std::fs`（避免阻塞 async runtime）
- UTF-8 截断必须使用 `is_char_boundary()` 确保在字符边界截断
- AGENT.md 大小限制：10KB（10,000 字节）
- 如果 AGENT.md 不存在，使用默认提示 "No project context provided."
- 所有新代码必须有单元测试

---

## File Structure

**修改文件：**
- `src-rust/src/cross_agent/agent_launcher.rs` - 添加 AGENT.md 读取和注入逻辑
- `src-rust/src/orchestration/dag_topology.rs` - 添加 Skipped 状态
- `src-rust/src/cross_agent/dag_scheduler.rs` - 添加失败传播逻辑

**测试文件：**
- `src-rust/src/cross_agent/agent_launcher.rs` (内联测试模块)
- `src-rust/src/orchestration/dag_topology.rs` (内联测试模块)

---

### Task 1: 实现 UTF-8 安全截断函数

**Files:**
- Modify: `src-rust/src/cross_agent/agent_launcher.rs`

**Interfaces:**
- Produces: `fn safe_truncate_utf8(s: &str, max_len: usize) -> String`

- [ ] **Step 1: Write the failing test**

在 `src-rust/src/cross_agent/agent_launcher.rs` 文件末尾的测试模块中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_truncate_utf8_ascii() {
        let content = "a".repeat(10001);
        let truncated = safe_truncate_utf8(&content, 10000);
        assert_eq!(truncated.len(), 10000 + "\n\n[... truncated ...]".len());
        assert!(truncated.ends_with("[... truncated ...]"));
    }

    #[test]
    fn test_safe_truncate_utf8_multibyte() {
        // 每个中文字符 3 字节
        let content = "中".repeat(3334);  // 10002 字节
        let truncated = safe_truncate_utf8(&content, 10000);
        
        // 应该小于等于 10000 + 后缀长度
        assert!(truncated.len() <= 10000 + "\n\n[... truncated ...]".len());
        // 应该在字符边界截断
        let truncated_content = &truncated[..truncated.len() - "\n\n[... truncated ...]".len()];
        assert!(truncated_content.is_char_boundary(truncated_content.len()));
        // 不应该 panic
        assert!(truncated_content.chars().count() > 0);
    }

    #[test]
    fn test_safe_truncate_utf8_no_truncation_needed() {
        let content = "short content";
        let truncated = safe_truncate_utf8(content, 10000);
        assert_eq!(truncated, content);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-rust && cargo test safe_truncate_utf8 --lib`
Expected: FAIL with "cannot find function `safe_truncate_utf8`"

- [ ] **Step 3: Write minimal implementation**

在 `src-rust/src/cross_agent/agent_launcher.rs` 文件中添加辅助函数（在 `impl AgentLauncher` 之前）：

```rust
/// Safely truncate a UTF-8 string to at most max_len bytes,
/// ensuring we don't split multi-byte characters.
fn safe_truncate_utf8(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    
    // Find the largest character boundary <= max_len
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    
    format!("{}\n\n[... truncated ...]", &s[..end])
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-rust && cargo test safe_truncate_utf8 --lib`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src-rust/src/cross_agent/agent_launcher.rs
git commit -m "feat: add UTF-8 safe truncation helper"
```

---

### Task 2: 实现异步 read_project_context 方法

**Files:**
- Modify: `src-rust/src/cross_agent/agent_launcher.rs`

**Interfaces:**
- Consumes: `safe_truncate_utf8` from Task 1
- Produces: `async fn read_project_context(&self) -> String`

- [ ] **Step 1: Write the failing test**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_read_project_context_exists() {
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    // Create .ergatai directory and AGENT.md
    let ergatai_dir = project_root.join(".ergatai");
    tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
    tokio::fs::write(ergatai_dir.join("AGENT.md"), "test context").await.unwrap();
    
    let launcher = AgentLauncher::new(project_root);
    let context = launcher.read_project_context().await;
    
    assert_eq!(context, "test context");
}

#[tokio::test]
async fn test_read_project_context_not_exists() {
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    let launcher = AgentLauncher::new(project_root);
    let context = launcher.read_project_context().await;
    
    assert_eq!(context, "No project context provided.");
}

#[tokio::test]
async fn test_read_project_context_truncation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    // Create .ergatai directory and large AGENT.md
    let ergatai_dir = project_root.join(".ergatai");
    tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
    let large_content = "a".repeat(15000);
    tokio::fs::write(ergatai_dir.join("AGENT.md"), &large_content).await.unwrap();
    
    let launcher = AgentLauncher::new(project_root);
    let context = launcher.read_project_context().await;
    
    // Should be truncated
    assert!(context.len() < 15000);
    assert!(context.ends_with("[... truncated ...]"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-rust && cargo test read_project_context --lib`
Expected: FAIL with "no method named `read_project_context` found"

- [ ] **Step 3: Write minimal implementation**

在 `impl AgentLauncher` 中添加方法：

```rust
impl AgentLauncher {
    // ... existing methods ...
    
    /// Read project context from .ergatai/AGENT.md
    /// Returns default message if file doesn't exist
    /// Truncates to 10KB if file is too large
    async fn read_project_context(&self) -> String {
        let agent_md_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
        
        match tokio::fs::read_to_string(&agent_md_path).await {
            Ok(content) => {
                const MAX_SIZE: usize = 10_000; // 10KB
                
                if content.len() > MAX_SIZE {
                    tracing::warn!(
                        "AGENT.md too large ({} bytes), truncating to {} bytes",
                        content.len(),
                        MAX_SIZE
                    );
                    safe_truncate_utf8(&content, MAX_SIZE)
                } else {
                    content
                }
            }
            Err(e) => {
                tracing::debug!("Failed to read AGENT.md: {}", e);
                "No project context provided.".to_string()
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-rust && cargo test read_project_context --lib`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src-rust/src/cross_agent/agent_launcher.rs
git commit -m "feat: add async read_project_context method"
```

---

### Task 3: 修改 create_agent_instruction 为 async 并注入上下文

**Files:**
- Modify: `src-rust/src/cross_agent/agent_launcher.rs:165-242`

**Interfaces:**
- Consumes: `read_project_context` from Task 2
- Produces: `async fn create_agent_instruction(...) -> String`

- [ ] **Step 1: Write the failing test**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_create_agent_instruction_includes_context() {
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    // Create .ergatai directory and AGENT.md
    let ergatai_dir = project_root.join(".ergatai");
    tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
    tokio::fs::write(ergatai_dir.join("AGENT.md"), "# My Project Context\n\nThis is important.").await.unwrap();
    
    let launcher = AgentLauncher::new(project_root.clone());
    
    let assignment = AgentAssignment {
        agent_name: "test-agent".to_string(),
        objective: "Test objective".to_string(),
        files_to_create: vec![],
        files_to_modify: vec![],
        files_to_read: vec![],
        task_type: TaskType::CreateNew,
        worktree_name: "test-worktree".to_string(),
        depends_on: vec![],
    };
    
    let worktree_path = project_root.join("worktree");
    let plan_file = project_root.join("plan.md");
    let result_file = project_root.join("result.md");
    
    let instruction = launcher.create_agent_instruction(
        "test-agent",
        &worktree_path,
        &plan_file,
        &result_file,
        &assignment,
    ).await;
    
    // Should include project context
    assert!(instruction.contains("# My Project Context"));
    assert!(instruction.contains("This is important."));
    // Should include task assignment
    assert!(instruction.contains("@test-agent"));
    assert!(instruction.contains("Test objective"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-rust && cargo test create_agent_instruction_includes_context --lib`
Expected: FAIL (function not async, doesn't include context)

- [ ] **Step 3: Write minimal implementation**

修改 `create_agent_instruction` 函数（`agent_launcher.rs:165-242`）：

```rust
/// Create instruction for agent (in English for token efficiency)
async fn create_agent_instruction(  // Changed to async
    &self,
    agent_name: &str,
    worktree_path: &Path,
    plan_file: &Path,
    result_file: &Path,
    assignment: &AgentAssignment,
) -> String {
    let task_type_dbg = format!("{:?}", assignment.task_type);
    let files_section = self.format_files_section(assignment);
    
    // Read project context
    let project_context = self.read_project_context().await;
    
    format!(
        r#"# Project Context

{project_context}

---

# Task Assignment for @{agent_name}

## Your Work Directory
```
{worktree_path}
```

## Task Plan
Read the full plan: `{plan_file}`

Find your assignment section (marked with `@{agent_name}`)

## Your Objective
{objective}

## Task Type
{task_type}

## Files
{files_section}

## Instructions

1. Read the plan file to understand the full context
2. Work in your designated worktree directory
3. Complete your assigned task
4. Write your results to: `{result_file}`

## Result Format

Write your results in markdown:

```markdown
# Task Result

## Status
[Completed/Failed/Partial]

## Summary
[Brief summary of what you did]

## Details
[Detailed description of your work]

## Files Created/Modified
- [list of files]

## Notes
[Any additional notes or issues]
```

## Important Notes

- Do NOT modify files outside your worktree
- Focus only on your assigned objective
- If you encounter issues, document them in your result file
- Complete your task and write the result file when done
"#,
        project_context = project_context,
        agent_name = agent_name,
        worktree_path = worktree_path.display(),
        plan_file = plan_file.display(),
        objective = assignment.objective,
        task_type = task_type_dbg,
        files_section = files_section,
        result_file = result_file.display(),
    )
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-rust && cargo test create_agent_instruction_includes_context --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-rust/src/cross_agent/agent_launcher.rs
git commit -m "feat: inject AGENT.md context into agent instructions"
```

---

### Task 4: 更新 launch_agent 调用以适配 async

**Files:**
- Modify: `src-rust/src/cross_agent/agent_launcher.rs:83-149`

**Interfaces:**
- Consumes: async `create_agent_instruction` from Task 3

- [ ] **Step 1: Update the call site**

修改 `launch_agent` 方法（`agent_launcher.rs:83-149`），在调用 `create_agent_instruction` 时添加 `.await`：

```rust
pub async fn launch_agent(
    &self,
    plan: &TaskPlan,
    assignment: &AgentAssignment,
) -> Result<String> {
    let agent_id = Self::make_agent_id(&plan.task_id, &assignment.agent_name);

    // Create worktree
    let worktree_path = self
        .coordinator
        .create_worktree(&plan.task_id, &assignment.agent_name)
        .await?;

    // Get result file path
    let result_file = self
        .coordinator
        .get_result_path(&plan.task_id, &assignment.agent_name)?;

    // Copy AGENT.md to worktree (if exists)
    let agent_guide_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
    if tokio::fs::try_exists(&agent_guide_path).await.unwrap_or(false) {
        let worktree_agent_guide = worktree_path.join("AGENT.md");
        tokio::fs::copy(&agent_guide_path, &worktree_agent_guide).await?;
    }

    // Create agent instruction (add .await here)
    let instruction = self.create_agent_instruction(
        &assignment.agent_name,
        &worktree_path,
        &plan.plan_file,
        &result_file,
        assignment,
    ).await;  // <-- Added .await

    // Save instruction to file
    let instruction_file = worktree_path.join(".ergatai-task.md");
    tokio::fs::write(&instruction_file, &instruction).await?;

    // ... rest of the existing code remains unchanged ...
}
```

- [ ] **Step 2: Run cargo check to verify compilation**

Run: `cd src-rust && cargo check`
Expected: No errors related to async/await

- [ ] **Step 3: Run all tests to ensure nothing broke**

Run: `cd src-rust && cargo test --lib`
Expected: All existing tests still pass

- [ ] **Step 4: Commit**

```bash
git add src-rust/src/cross_agent/agent_launcher.rs
git commit -m "refactor: update launch_agent to await async instruction creation"
```

---

### Task 5: 添加 Skipped 状态到 TaskStatus 枚举

**Files:**
- Modify: `src-rust/src/orchestration/dag_topology.rs:73-84`

**Interfaces:**
- Produces: `TaskStatus::Skipped` variant

- [ ] **Step 1: Write the failing test**

在 `src-rust/src/orchestration/dag_topology.rs` 的测试模块中添加：

```rust
#[test]
fn test_skipped_status_exists() {
    let status = TaskStatus::Skipped;
    assert_eq!(status, TaskStatus::Skipped);
}

#[test]
fn test_is_complete_with_skipped() {
    let graph = TaskGraph::new(vec![
        TaskNode::new("n1", "agent", "Task 1"),
        TaskNode::new("n2", "agent", "Task 2"),
    ]);
    
    let mut graph = graph;
    graph.update_status("n1", TaskStatus::Completed).unwrap();
    graph.update_status("n2", TaskStatus::Skipped).unwrap();
    
    // Should be complete (Completed + Skipped = all done)
    assert!(graph.is_complete());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-rust && cargo test skipped --lib`
Expected: FAIL with "no variant named `Skipped`"

- [ ] **Step 3: Write minimal implementation**

修改 `TaskStatus` 枚举（`dag_topology.rs:73-84`）：

```rust
/// Task execution status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not started yet (waiting for dependencies)
    Pending,
    /// Currently being executed
    Running,
    /// Successfully completed
    Completed,
    /// Failed (may retry)
    Failed,
    /// Skipped (dependency failed, won't execute)
    Skipped,
}
```

- [ ] **Step 4: Update is_complete() to consider Skipped**

修改 `is_complete` 方法（`dag_topology.rs:192-194`）：

```rust
/// Check if all tasks are completed (or failed/skipped)
pub fn is_complete(&self) -> bool {
    self.nodes.iter().all(|n| {
        matches!(
            n.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Skipped
        )
    })
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd src-rust && cargo test skipped --lib`
Expected: PASS (2 tests)

- [ ] **Step 6: Commit**

```bash
git add src-rust/src/orchestration/dag_topology.rs
git commit -m "feat: add Skipped status for failed dependency propagation"
```

---

### Task 6: 实现失败传播逻辑

**Files:**
- Modify: `src-rust/src/cross_agent/dag_scheduler.rs`

**Interfaces:**
- Consumes: `TaskStatus::Skipped` from Task 5
- Produces: `on_node_failed()` and `skip_downstream()` methods

- [ ] **Step 1: Write the failing test**

在 `src-rust/src/cross_agent/dag_scheduler.rs` 文件末尾添加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{TaskGraph, TaskNode, TaskStatus};

    #[tokio::test]
    async fn test_on_node_failed_marks_downstream_skipped() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent", "Task 1"),
            TaskNode::new("n2", "agent", "Task 2").with_dependencies(vec!["n1".into()]),
            TaskNode::new("n3", "agent", "Task 3").with_dependencies(vec!["n2".into()]),
        ]);
        
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);
        
        // Mark n1 as failed
        scheduler.on_node_failed("n1").await.unwrap();
        
        // Check that n2 and n3 are skipped
        let graph = scheduler.graph.lock().await;
        assert_eq!(graph.find_node("n1").unwrap().status, TaskStatus::Failed);
        assert_eq!(graph.find_node("n2").unwrap().status, TaskStatus::Skipped);
        assert_eq!(graph.find_node("n3").unwrap().status, TaskStatus::Skipped);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-rust && cargo test on_node_failed --lib`
Expected: FAIL with "no method named `on_node_failed` found"

- [ ] **Step 3: Write minimal implementation**

在 `impl DagScheduler` 中添加方法：

```rust
impl DagScheduler {
    // ... existing methods ...
    
    /// Called when a node fails
    /// Marks the node as Failed and all downstream nodes as Skipped
    pub async fn on_node_failed(&self, node_id: &str) -> Result<()> {
        // Mark as Failed
        {
            let mut graph = self.graph.lock().await;
            graph.update_status(node_id, TaskStatus::Failed)?;
        }
        
        tracing::warn!("Node {} failed, skipping downstream nodes", node_id);
        
        // Skip all downstream nodes
        self.skip_downstream(node_id).await?;
        
        Ok(())
    }
    
    /// Recursively skip all nodes that depend on the failed node
    async fn skip_downstream(&self, failed_id: &str) -> Result<()> {
        let mut graph = self.graph.lock().await;
        
        // Find all nodes that depend on the failed node
        let downstream: Vec<String> = graph.nodes.iter()
            .filter(|n| n.depends_on.contains(&failed_id.to_string()))
            .map(|n| n.id.clone())
            .collect();
        
        // Mark each downstream node as Skipped
        for node_id in downstream {
            if let Some(node) = graph.find_node_mut(&node_id) {
                if node.status == TaskStatus::Pending {
                    node.status = TaskStatus::Skipped;
                    tracing::info!("Skipped node {} (depends on failed {})", node_id, failed_id);
                }
            }
            
            // Recursively skip nodes that depend on this one
            drop(graph);
            self.skip_downstream(&node_id).await?;
            graph = self.graph.lock().await;
        }
        
        Ok(())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-rust && cargo test on_node_failed --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-rust/src/cross_agent/dag_scheduler.rs
git commit -m "feat: implement failure propagation with Skipped status"
```

---

### Task 7: 集成测试 - 完整流程

**Files:**
- Test: `src-rust/tests/agent_md_injection_test.rs`

- [ ] **Step 1: Write integration test**

创建 `src-rust/tests/agent_md_injection_test.rs`：

```rust
use ergatai::cross_agent::AgentLauncher;
use ergatai::cross_agent::task_coordinator::{AgentAssignment, TaskType};
use std::path::PathBuf;

#[tokio::test]
async fn test_agent_md_injected_into_instruction() {
    // Setup
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    // Create .ergatai/AGENT.md
    let ergatai_dir = project_root.join(".ergatai");
    tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
    let agent_md_content = "# Project Architecture\n\nThis is a Rust + React project.";
    tokio::fs::write(ergatai_dir.join("AGENT.md"), agent_md_content).await.unwrap();
    
    let launcher = AgentLauncher::new(project_root.clone());
    
    // Create assignment
    let assignment = AgentAssignment {
        agent_name: "test-agent".to_string(),
        objective: "Implement feature X".to_string(),
        files_to_create: vec![],
        files_to_modify: vec![],
        files_to_read: vec![],
        task_type: TaskType::CreateNew,
        worktree_name: "test-worktree".to_string(),
        depends_on: vec![],
    };
    
    let worktree_path = project_root.join("worktree");
    let plan_file = project_root.join("plan.md");
    let result_file = project_root.join("result.md");
    
    // Execute
    let instruction = launcher.create_agent_instruction(
        "test-agent",
        &worktree_path,
        &plan_file,
        &result_file,
        &assignment,
    ).await;
    
    // Verify
    assert!(instruction.contains("# Project Architecture"));
    assert!(instruction.contains("This is a Rust + React project."));
    assert!(instruction.contains("@test-agent"));
    assert!(instruction.contains("Implement feature X"));
}

#[tokio::test]
async fn test_agent_md_not_found_uses_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    
    let launcher = AgentLauncher::new(project_root.clone());
    
    let assignment = AgentAssignment {
        agent_name: "test-agent".to_string(),
        objective: "Test".to_string(),
        files_to_create: vec![],
        files_to_modify: vec![],
        files_to_read: vec![],
        task_type: TaskType::CreateNew,
        worktree_name: "test".to_string(),
        depends_on: vec![],
    };
    
    let instruction = launcher.create_agent_instruction(
        "test-agent",
        &project_root.join("worktree"),
        &project_root.join("plan.md"),
        &project_root.join("result.md"),
        &assignment,
    ).await;
    
    assert!(instruction.contains("No project context provided."));
}
```

- [ ] **Step 2: Run integration test**

Run: `cd src-rust && cargo test agent_md_injection --test agent_md_injection_test`
Expected: PASS (2 tests)

- [ ] **Step 3: Commit**

```bash
git add src-rust/tests/agent_md_injection_test.rs
git commit -m "test: add integration tests for AGENT.md injection"
```

---

### Task 8: 运行完整测试套件

- [ ] **Step 1: Run all Rust tests**

Run: `cd src-rust && cargo test --lib`
Expected: All tests pass

- [ ] **Step 2: Run cargo check**

Run: `cd src-rust && cargo check`
Expected: No errors or warnings

- [ ] **Step 3: Run cargo clippy**

Run: `cd src-rust && cargo clippy`
Expected: No warnings (or only pre-existing warnings)

- [ ] **Step 4: Final commit (if any fixes needed)**

```bash
git add -A
git commit -m "chore: final cleanup and formatting"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** All requirements from v3.1 design are covered
  - ✅ UTF-8 safe truncation (Task 1)
  - ✅ Async IO (Task 2)
  - ✅ AGENT.md injection (Task 3)
  - ✅ Async call update (Task 4)
  - ✅ Skipped status (Task 5)
  - ✅ Failure propagation (Task 6)
  - ✅ Integration tests (Task 7)

- [x] **Placeholder scan:** No TBD, TODO, or incomplete sections

- [x] **Type consistency:** All method signatures match across tasks

---

**Plan complete and saved to `docs/superpowers/plans/2026-08-07-agent-md-injection.md`. Two execution options:**

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
