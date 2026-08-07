# AGENT.md 强制注入与 DAG 任务编排设计（v3）

**日期**: 2026-08-07  
**状态**: 设计完成，待审查  
**版本**: v3（修正注入点）

---

## 1. 概述

### 1.1 核心发现（v2 审查反馈）

**v2 设计的 Critical 错误**：
- ❌ 修改了 `sdk_session.rs`（ACP SDK 会话路径）
- ❌ 但 DAG 调度**不使用这个路径**
- ✅ DAG 调度的实际流程：`DagScheduler → TaskScheduler → AgentLauncher → spawn_agent_process()`

**正确的注入点**：
- ✅ `AgentLauncher::create_agent_instruction()`（`agent_launcher.rs` 165-242 行）
- ✅ 这个函数生成 `.ergatai-task.md` 文件，Agent 启动时读取

### 1.2 设计目标

在 `AgentLauncher::create_agent_instruction()` 中注入 AGENT.md 内容，确保所有通过 DAG 调度的 Agent 接收项目上下文。

---

## 2. 现有架构（正确理解）

### 2.1 DAG 调度完整流程

```
主 Agent 写 DAG Markdown (.ergatai/graphs/{task-id}.md)
  ↓
parse_dag_markdown() → TaskGraph (带 UUID)
  ↓
DagScheduler::submit_graph()
  ↓
TaskGraph::ready_tasks() → 就绪节点
  ↓
TaskScheduler::submit_task() → 提交到调度队列
  ↓
TaskScheduler::dispatch_loop() → 派发给 Agent
  ↓
AgentLauncher::launch_agent()
  ↓
AgentLauncher::create_agent_instruction() → 生成 .ergatai-task.md  ← 注入点
  ↓
AgentLauncher::spawn_agent_process() → 启动 Agent 进程
  ↓
Agent 读取 .ergatai-task.md → 执行任务 → 写入结果
  ↓
DagScheduler::on_node_completed() → 触发下一批
```

### 2.2 关键代码路径

**AgentLauncher::create_agent_instruction**（`agent_launcher.rs` 165-242）：
```rust
fn create_agent_instruction(
    &self,
    agent_name: &str,
    worktree_path: &Path,
    plan_file: &Path,
    result_file: &Path,
    assignment: &AgentAssignment,
) -> String {
    format!(
        r#"# Task Assignment for @{agent_name}

## Your Work Directory
...

## Task Plan
Read the full plan: `{plan_file}`

## Your Objective
{objective}

...

## Important Notes
...
"#,
        // ... 参数替换
    )
}
```

**这个函数生成的内容会被写入 `.ergatai-task.md`，Agent 启动时作为参数接收。**

---

## 3. 修改方案（v3）

### 3.1 修改 `AgentLauncher::create_agent_instruction`

**目标**：在生成的指令开头注入 AGENT.md 内容。

```rust
fn create_agent_instruction(
    &self,
    agent_name: &str,
    worktree_path: &Path,
    plan_file: &Path,
    result_file: &Path,
    assignment: &AgentAssignment,
) -> String {
    let task_type_dbg = format!("{:?}", assignment.task_type);
    let files_section = self.format_files_section(assignment);
    
    // 新增：读取项目上下文（AGENT.md）
    let project_context = self.read_project_context();
    
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
        project_context = project_context,  // 新增
        agent_name = agent_name,
        worktree_path = worktree_path.display(),
        plan_file = plan_file.display(),
        objective = assignment.objective,
        task_type = task_type_dbg,
        files_section = files_section,
        result_file = result_file.display(),
    )
}

// 新增辅助方法
fn read_project_context(&self) -> String {
    let agent_md_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
    
    match std::fs::read_to_string(&agent_md_path) {
        Ok(content) => {
            // 限制大小（防止超出 context window）
            const MAX_SIZE: usize = 10_000; // 10KB
            if content.len() > MAX_SIZE {
                tracing::warn!(
                    "AGENT.md too large ({} bytes), truncating to {} bytes",
                    content.len(),
                    MAX_SIZE
                );
                format!("{}\n\n[... truncated ...]", &content[..MAX_SIZE])
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

### 3.2 同时复制 AGENT.md 到 worktree（可选）

**现有代码已经实现**（`agent_launcher.rs` 101-106）：
```rust
// Copy AGENT.md to worktree (if exists)
let agent_guide_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
if tokio::fs::try_exists(&agent_guide_path).await.unwrap_or(false) {
    let worktree_agent_guide = worktree_path.join("AGENT.md");
    tokio::fs::copy(&agent_guide_path, &worktree_agent_guide).await?;
}
```

**好处**：Agent 可以在需要时参考完整的 AGENT.md 文件。

---

## 4. DAG Markdown 格式

### 4.1 现有格式（已支持）

```markdown
## backend-api
- **agent**: backend-agent
- **task**: .ergatai/.plan/task-001/backend-api.md
- **depends_on**: []

## frontend-ui
- **agent**: frontend-agent
- **task**: .ergatai/.plan/task-001/frontend-ui.md
- **depends_on**: [backend-api]
```

**说明**：
- `##` 或 `###` 表示节点
- `**agent**`: Agent 名称
- `**task**`: 任务文档路径（可选）
- `**depends_on**`: 依赖节点名称列表
- **ID 自动生成**：系统为每个节点生成 UUID

### 4.2 文件结构

```
.ergatai/
├── AGENT.md                    # 项目级上下文（所有 Agent 共享）
├── graphs/
│   └── {task-id}.md           # 调用图（DAG Markdown）
└── .plan/
    └── {task-id}/
        ├── backend-api.md     # 节点任务文档（可选）
        ├── frontend-ui.md     # 节点任务文档（可选）
        └── results/
            └── {node-id}.md   # Agent 执行结果
```

---

## 5. 完整流程（修正版）

```
1. 主 Agent 创建 DAG
   - 写入 .ergatai/graphs/{task-id}.md
   - 可选写入 .ergatai/.plan/{task-id}/*.md（任务文档）

2. 系统解析 DAG
   parse_dag_markdown(graph_content) → TaskGraph (with UUIDs)

3. 创建 DagScheduler
   DagScheduler::new(project_root, graph)

4. 提交 DAG
   DagScheduler::submit_graph()
   ↓
   for each ready node:
     - 生成计划文件
     - 提交到 TaskScheduler

5. TaskScheduler 派发
   TaskScheduler::dispatch_task()
   ↓
   - 解析计划文件
   - 调用 AgentLauncher::launch_agent()

6. AgentLauncher 启动 Agent
   AgentLauncher::launch_agent()
   ↓
   a. 创建 worktree
   b. 复制 AGENT.md 到 worktree（现有代码）
   c. 调用 create_agent_instruction()
      ↓
      - 读取 .ergatai/AGENT.md（新增）
      - 生成包含项目上下文的指令
   d. 写入 .ergatai-task.md
   e. 调用 spawn_agent_process()
      ↓
      - 启动 Agent 进程
      - 传入 .ergatai-task.md 路径作为参数

7. Agent 执行
   - 读取 .ergatai-task.md（包含 AGENT.md 内容）
   - 在 worktree 中执行
   - 写入结果到 results/{node-id}.md

8. 监控完成
   DagScheduler::on_node_completed()
   ↓
   - 更新 DAG 状态
   - 检查新就绪节点
   - 重复步骤 4-7

9. 所有节点完成
   - 合并结果
   - 返回最终状态
```

---

## 6. 失败传播

### 6.1 问题

当某节点失败时，依赖它的下游节点会永远停留在 `Pending` 状态。

### 6.2 解决方案

**添加 Skipped 状态**（`dag_topology.rs`）：

```rust
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,  // 新增：因依赖失败而跳过
}
```

**在 DagScheduler 中实现传播**：

```rust
impl DagScheduler {
    pub async fn on_node_failed(&self, node_id: &str) -> Result<()> {
        // 标记为 Failed
        {
            let mut graph = self.graph.lock().await;
            graph.update_status(node_id, TaskStatus::Failed)?;
        }
        
        // 将所有下游标记为 Skipped
        self.skip_downstream(node_id).await?;
        
        Ok(())
    }
    
    async fn skip_downstream(&self, failed_id: &str) -> Result<()> {
        let mut graph = self.graph.lock().await;
        
        // 找到所有依赖失败节点的下游
        let downstream: Vec<String> = graph.nodes.iter()
            .filter(|n| n.depends_on.contains(&failed_id.to_string()))
            .map(|n| n.id.clone())
            .collect();
        
        // 递归标记为 Skipped
        for node_id in downstream {
            if let Some(node) = graph.find_node_mut(&node_id) {
                if node.status == TaskStatus::Pending {
                    node.status = TaskStatus::Skipped;
                    tracing::info!("Skipped node {} (depends on failed {})", node_id, failed_id);
                }
            }
            
            // 递归处理
            drop(graph);
            self.skip_downstream(&node_id).await?;
            graph = self.graph.lock().await;
        }
        
        Ok(())
    }
}
```

**修改完成检查**：

```rust
impl TaskGraph {
    pub fn is_complete(&self) -> bool {
        self.nodes.iter().all(|n| {
            matches!(n.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Skipped)
        })
    }
}
```

---

## 7. 文件修改清单

### 7.1 修改文件

```
src-rust/src/cross_agent/agent_launcher.rs
  - create_agent_instruction(): 读取 AGENT.md 并注入到指令开头
  - 新增 read_project_context() 辅助方法

src-rust/src/orchestration/dag_topology.rs
  - TaskStatus: 添加 Skipped 变体
  - TaskGraph::is_complete(): 考虑 Skipped 状态

src-rust/src/cross_agent/dag_scheduler.rs
  - 新增 on_node_failed()
  - 新增 skip_downstream()
```

### 7.2 不修改的文件

```
src-rust/src/acp/sdk_session.rs  ← 不需要修改（v2 的错误）
src-rust/src/cross_agent/task_scheduler.rs  ← 不需要修改
```

---

## 8. 测试计划

### 8.1 单元测试

1. **AGENT.md 读取**
   - 文件存在 → 正确读取并注入
   - 文件不存在 → 使用默认提示
   - 文件过大 → 截断

2. **指令生成**
   - 验证生成的 `.ergatai-task.md` 包含 AGENT.md 内容
   - 验证格式正确

3. **失败传播**
   - 单节点失败 → 下游被跳过
   - 多层依赖 → 递归跳过

### 8.2 集成测试

1. **完整流程**
   - 创建 DAG Markdown
   - 解析 → TaskGraph
   - 提交 → 派发（带 AGENT.md）
   - 执行 → 完成
   - 验证 Agent 接收到了 AGENT.md

2. **边界情况**
   - 空 DAG
   - 单节点 DAG
   - 节点失败 → 下游跳过

---

## 9. 总结

### 9.1 核心变更（v3）

1. **修正注入点** - 从 `sdk_session.rs` 改为 `agent_launcher.rs`
2. **简化实现** - 只需修改 `create_agent_instruction()` 函数
3. **保留失败传播** - 添加 Skipped 状态

### 9.2 优势

1. **正确的代码路径** - 修改 DAG 调度实际使用的流程
2. **最小改动** - 只需修改一个函数
3. **复用现有基础设施** - 不重复造轮子
4. **向后兼容** - 不影响现有功能

### 9.3 与 v2 的对比

| 方面 | v2 | v3 |
|------|-----|-----|
| 注入点 | ❌ `sdk_session.rs` | ✅ `agent_launcher.rs` |
| 代码路径 | ❌ DAG 调度不使用 | ✅ DAG 调度使用 |
| 复杂度 | ❌ 高（需修改多个文件） | ✅ 低（只修改一个函数） |
| 正确性 | ❌ 无法工作 | ✅ 可以工作 |

---

## 10. 后续优化

1. **AGENT.md 缓存** - 避免重复读取
2. **Agent 级 persona** - 从 AgentConfig 读取 persona 并注入
3. **DAG 可视化** - 生成 Mermaid 图
4. **事件驱动完成监控** - 用 channel/notify 替代轮询
