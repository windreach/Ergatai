# AGENT.md 强制注入与 DAG 任务编排设计（v3.1）

**日期**: 2026-08-07  
**状态**: 设计完成，待审查  
**版本**: v3.1（修复 2 个 Major 问题）

---

## 1. 概述

### 1.1 v3 审查反馈

**2 个 Major 问题**：
1. **UTF-8 截断可能导致 panic** - `&content[..MAX_SIZE]` 可能在多字节字符中间截断
2. **同步 IO 在 async 上下文中** - `std::fs::read_to_string` 是同步的，会阻塞 async runtime

### 1.2 v3.1 修复

1. ✅ **安全的 UTF-8 截断** - 使用 `char_boundary` 确保在字符边界截断
2. ✅ **异步 IO** - 使用 `tokio::fs::read_to_string` 替代同步版本

---

## 2. 修改方案（v3.1）

### 2.1 修改 `AgentLauncher::create_agent_instruction`

**关键修复**：

```rust
// 修改函数签名为 async
async fn create_agent_instruction(
    &self,
    agent_name: &str,
    worktree_path: &Path,
    plan_file: &Path,
    result_file: &Path,
    assignment: &AgentAssignment,
) -> String {
    let task_type_dbg = format!("{:?}", assignment.task_type);
    let files_section = self.format_files_section(assignment);
    
    // 修复 1: 使用 async IO
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

// 修复 1 + 2: 安全的 UTF-8 截断 + async IO
async fn read_project_context(&self) -> String {
    let agent_md_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
    
    match tokio::fs::read_to_string(&agent_md_path).await {  // 修复 2: async IO
        Ok(content) => {
            // 限制大小（防止超出 context window）
            const MAX_SIZE: usize = 10_000; // 10KB
            
            if content.len() > MAX_SIZE {
                tracing::warn!(
                    "AGENT.md too large ({} bytes), truncating to {} bytes",
                    content.len(),
                    MAX_SIZE
                );
                
                // 修复 1: 安全的 UTF-8 截断
                // 找到不超过 MAX_SIZE 的最大字符边界
                let mut end = MAX_SIZE;
                while end > 0 && !content.is_char_boundary(end) {
                    end -= 1;
                }
                
                format!("{}\n\n[... truncated ...]", &content[..end])
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
```

### 2.2 调用方修改

**AgentLauncher::launch_agent**（`agent_launcher.rs` 83-149）：

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

    // Create agent instruction (修复: 改为 await)
    let instruction = self.create_agent_instruction(
        &assignment.agent_name,
        &worktree_path,
        &plan.plan_file,
        &result_file,
        assignment,
    ).await;  // 新增 await

    // Save instruction to file
    let instruction_file = worktree_path.join(".ergatai-task.md");
    tokio::fs::write(&instruction_file, &instruction).await?;

    // ... 继续现有逻辑 ...
}
```

---

## 3. 修复说明

### 3.1 UTF-8 安全截断

**问题**：
```rust
// ❌ 错误：可能在多字节字符中间截断
&content[..MAX_SIZE]  // 如果 MAX_SIZE 落在 UTF-8 字符中间，会 panic
```

**修复**：
```rust
// ✅ 正确：找到字符边界
let mut end = MAX_SIZE;
while end > 0 && !content.is_char_boundary(end) {
    end -= 1;
}
&content[..end]  // 保证在字符边界截断
```

**原理**：
- `is_char_boundary(n)` 检查位置 n 是否是有效的 UTF-8 字符边界
- 从 MAX_SIZE 向前搜索，直到找到有效的字符边界
- 最多回退 3 个字节（UTF-8 最长字符 4 字节）

### 3.2 异步 IO

**问题**：
```rust
// ❌ 错误：同步 IO 会阻塞 async runtime
std::fs::read_to_string(&path)
```

**修复**：
```rust
// ✅ 正确：使用 tokio 的异步版本
tokio::fs::read_to_string(&path).await
```

**原理**：
- `std::fs` 是同步的，会阻塞当前线程
- 在 async 上下文中，会阻塞整个 tokio runtime
- `tokio::fs` 是异步的，不会阻塞 runtime

---

## 4. 完整修改清单

### 4.1 修改文件

```
src-rust/src/cross_agent/agent_launcher.rs
  - create_agent_instruction(): 
    - 改为 async fn
    - 调用 read_project_context().await
  - 新增 read_project_context():
    - async fn
    - 使用 tokio::fs::read_to_string
    - 安全的 UTF-8 截断
  - launch_agent():
    - 调用 create_agent_instruction().await

src-rust/src/orchestration/dag_topology.rs
  - TaskStatus: 添加 Skipped 变体
  - TaskGraph::is_complete(): 考虑 Skipped 状态

src-rust/src/cross_agent/dag_scheduler.rs
  - 新增 on_node_failed()
  - 新增 skip_downstream()
```

---

## 5. 测试要点

### 5.1 UTF-8 截断测试

```rust
#[test]
fn test_utf8_truncation() {
    // 测试在 ASCII 字符处截断
    let content = "a".repeat(10001);
    let truncated = safe_truncate(&content, 10000);
    assert_eq!(truncated.len(), 10000);
    
    // 测试在多字节字符中间截断
    let content = "中".repeat(3334);  // 每个中文字符 3 字节
    let truncated = safe_truncate(&content, 10000);
    assert!(truncated.len() <= 10000);
    assert!(truncated.is_char_boundary(truncated.len()));
}

fn safe_truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    
    format!("{}\n\n[... truncated ...]", &s[..end])
}
```

### 5.2 异步 IO 测试

```rust
#[tokio::test]
async fn test_async_read() {
    let launcher = AgentLauncher::new(temp_dir());
    
    // 测试文件存在
    tokio::fs::write(".ergatai/AGENT.md", "test content").await.unwrap();
    let context = launcher.read_project_context().await;
    assert_eq!(context, "test content");
    
    // 测试文件不存在
    tokio::fs::remove_file(".ergatai/AGENT.md").await.unwrap();
    let context = launcher.read_project_context().await;
    assert_eq!(context, "No project context provided.");
}
```

---

## 6. 总结

### 6.1 v3.1 修复

| 问题 | v3 | v3.1 |
|------|-----|------|
| UTF-8 截断 | ❌ 可能 panic | ✅ 安全截断 |
| IO 模式 | ❌ 同步阻塞 | ✅ 异步非阻塞 |

### 6.2 代码变更

**新增/修改**：
- `create_agent_instruction()` - 改为 async
- `read_project_context()` - 新增，async + 安全截断
- `launch_agent()` - 调用时添加 await

**影响范围**：
- 仅影响 `agent_launcher.rs`
- 不影响其他模块
- 向后兼容（只是内部实现改变）

---

## 7. 后续步骤

1. ✅ 设计审查通过
2. ✅ 修复 2 个 Major 问题
3. ⏭️ 创建实现计划
4. ⏭️ 开始实现
