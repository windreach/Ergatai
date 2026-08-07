# AGENT.md 强制注入与 DAG 任务编排设计（v2）

**日期**: 2026-08-07  
**状态**: 设计完成，待审查  
**版本**: v2（基于现有模块扩展）

---

## 1. 概述

### 1.1 目标

在现有 DAG 编排系统基础上，实现 AGENT.md 强制注入机制，确保所有执行 Agent 在开始工作前接收项目架构和协作规范。

### 1.2 核心发现

**现有基础设施已完整**：
- ✅ `src-rust/src/orchestration/dag_parser.rs` - 解析 Markdown DAG，自动生成 UUID
- ✅ `src-rust/src/orchestration/dag_topology.rs` - TaskGraph/TaskNode，含循环检测、就绪任务计算
- ✅ `src-rust/src/cross_agent/dag_scheduler.rs` - 集成 TaskGraph 与 TaskScheduler
- ✅ `src-rust/src/cross_agent/task_scheduler.rs` - 全局任务调度器
- ✅ `src-rust/src/cross_agent/task_coordinator.rs` - 任务协调、worktree 管理

**唯一缺失的功能**：AGENT.md 强制注入到 ACP 会话。

### 1.3 设计原则

1. **扩展现有模块** - 不新建文件，复用现有基础设施
2. **最小改动** - 只修改必要的接口
3. **向后兼容** - 不影响现有功能

---

## 2. 现有架构

### 2.1 DAG 解析与调度流程

```
主 Agent 写 DAG Markdown
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
Agent 执行 → 写入结果
  ↓
DagScheduler::on_node_completed() → 触发下一批
```

### 2.2 关键数据结构

**TaskNode**（`dag_topology.rs`）：
```rust
pub struct TaskNode {
    pub id: String,              // UUID（系统生成）
    pub agent: String,           // Agent 名称
    pub task: String,            // 任务描述
    pub status: TaskStatus,      // Pending | Running | Completed | Failed
    pub depends_on: Vec<String>, // 依赖节点 ID
    pub metadata: HashMap<String, String>, // 元数据（含 task_path）
    // ...
}
```

**TaskGraph**（`dag_topology.rs`）：
```rust
pub struct TaskGraph {
    pub nodes: Vec<TaskNode>,
    // ...
}

impl TaskGraph {
    pub fn ready_tasks(&self) -> Vec<&TaskNode>;
    pub fn update_status(&mut self, id: &str, status: TaskStatus) -> Result<()>;
    pub fn validate(&self) -> Result<()>;
    // ...
}
```

---

## 3. 新增功能：AGENT.md 强制注入

### 3.1 修改点

**唯一需要修改的地方**：`src-rust/src/acp/sdk_session.rs`

**目标**：在 ACP 会话创建后，立即发送 AGENT.md 内容作为初始 Prompt。

### 3.2 实现方案

#### 方案 A：扩展 `spawn_session_task` 参数

```rust
// src-rust/src/acp/sdk_session.rs
pub fn spawn_session_task(
    config: AgentConfig,
    cwd: String,
    session_id_tx: oneshot::Sender<ErgataiResult<String>>,
    initial_prompt: Option<String>,  // 新增参数
) {
    // ...
    tokio::spawn(async move {
        let result = run_sdk_session(
            config, 
            cwd, 
            cmd_rx, 
            evt_tx.clone(), 
            pending_perms.clone(), 
            cmd_tx.clone(),
            initial_prompt,  // 传递到 run_sdk_session
        ).await;
        // ...
    });
}

async fn run_sdk_session(
    config: AgentConfig,
    cwd: String,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    evt_tx: mpsc::UnboundedSender<SessionEvent>,
    pending_perms: PendingPermissions,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    initial_prompt: Option<String>,  // 新增参数
) -> ErgataiResult<String> {
    // ... 现有代码 ...
    
    // 在 NewSessionRequest 之后，添加初始 Prompt 注入
    let session_id = session_response.session_id.to_string();
    
    // 新增：如果提供了 initial_prompt，立即发送
    if let Some(prompt) = initial_prompt {
        tracing::info!(session_id = %session_id, "Sending initial prompt (AGENT.md context)");
        
        connection.send_request(PromptRequest::new(
            SessionId::new(session_id.clone()),
            vec![ContentBlock::Text(TextContent::new(prompt))],
        )).block_task().await
        .map_err(|e| anyhow!("Initial prompt failed: {}", e))?;
    }
    
    // ... 继续现有逻辑 ...
}
```

#### 方案 B：创建 SessionParams 包装器（推荐）

```rust
// src-rust/src/acp/sdk_session.rs
pub struct SessionParams {
    pub config: AgentConfig,
    pub cwd: String,
    pub initial_prompt: Option<String>,
}

pub fn spawn_session_task(
    params: SessionParams,
    session_id_tx: oneshot::Sender<ErgataiResult<String>>,
) {
    // ...
}
```

**优点**：
- 参数更清晰
- 易于扩展（未来可以添加更多会话参数）
- 不破坏现有调用（可以通过 trait 或重载兼容）

### 3.3 调用方修改

**DagScheduler::generate_and_submit**（`dag_scheduler.rs`）：

```rust
async fn generate_and_submit(&self, node: &TaskNode) -> Result<String> {
    let graph = self.graph.lock().await;
    
    // 1. 读取 AGENT.md（项目级上下文）
    let project_context = self.read_project_context().await?;
    
    // 2. 读取 Agent 配置的 persona（Agent 级上下文）
    let agent_config = get_agent_config(&node.agent)?;
    let persona = agent_config.persona.unwrap_or_default();
    
    // 3. 读取任务内容（任务文档或内联任务）
    let task_content = self.get_task_content(node).await?;
    
    // 4. 构建初始 Prompt
    let initial_prompt = format!(
        "# Project Context\n\n{}\n\n---\n\n# Agent Persona\n\n{}\n\n---\n\n# Your Task\n\n{}",
        project_context,
        persona,
        task_content
    );
    
    // 5. 生成计划文件（现有逻辑）
    let plan_file = self.generate_node_plan(&graph, node).await?;
    drop(graph);
    
    // 6. 提交到调度器（传递 initial_prompt）
    let task_id = self.scheduler.submit_task_with_context(
        plan_file,
        Some(initial_prompt),
    ).await?;
    
    Ok(task_id)
}

async fn read_project_context(&self) -> Result<String> {
    let agent_md_path = self.project_root.join(".ergatai/AGENT.md");
    
    if agent_md_path.exists() {
        let content = tokio::fs::read_to_string(&agent_md_path).await?;
        
        // 限制大小（防止超出 context window）
        const MAX_SIZE: usize = 10_000; // 10KB
        if content.len() > MAX_SIZE {
            tracing::warn!(
                "AGENT.md too large ({} bytes), truncating to {} bytes",
                content.len(),
                MAX_SIZE
            );
            Ok(content[..MAX_SIZE].to_string() + "\n\n[... truncated ...]")
        } else {
            Ok(content)
        }
    } else {
        Ok("No project context provided.".to_string())
    }
}

async fn get_task_content(&self, node: &TaskNode) -> Result<String> {
    // 检查是否有任务文档路径
    if let Some(task_path) = node.metadata.get("task_path") {
        let full_path = self.project_root.join(task_path);
        if full_path.exists() {
            return tokio::fs::read_to_string(&full_path).await
                .with_context(|| format!("Failed to read task doc: {:?}", full_path));
        }
    }
    
    // 回退到内联任务描述
    Ok(node.task.clone())
}
```

### 3.4 TaskScheduler 扩展

**添加新方法**（`task_scheduler.rs`）：

```rust
impl TaskScheduler {
    pub async fn submit_task_with_context(
        &self,
        plan_file: PathBuf,
        initial_prompt: Option<String>,
    ) -> Result<String> {
        let task_id = Uuid::new_v4().to_string();
        
        let pending_task = PendingTask {
            task_id: task_id.clone(),
            plan_file,
            initial_prompt,  // 新增字段
            submitted_at: chrono::Utc::now().timestamp() as u64,
            priority: 0,
        };
        
        // ... 现有队列逻辑 ...
        
        Ok(task_id)
    }
}

// PendingTask 结构添加字段
pub struct PendingTask {
    pub task_id: String,
    pub plan_file: PathBuf,
    pub initial_prompt: Option<String>,  // 新增
    pub submitted_at: u64,
    pub priority: u32,
}
```

**修改 dispatch 逻辑**：

```rust
async fn dispatch_task(&self, task: &PendingTask) -> Result<()> {
    // 解析计划文件获取 agent 名称
    let plan = self.parse_plan(&task.plan_file).await?;
    let agent_name = extract_agent_from_plan(&plan)?;
    
    // 加载 agent 配置
    let mut config = get_agent_config(&agent_name)?;
    
    // 如果有 initial_prompt，通过环境变量或参数传递
    // 注意：不修改 AgentConfig（持久化配置），而是传递给 spawn_session_task
    
    // 创建 ACP 会话（传递 initial_prompt）
    let params = SessionParams {
        config,
        cwd: self.project_root.to_string_lossy().to_string(),
        initial_prompt: task.initial_prompt.clone(),
    };
    
    let (session_id_tx, session_id_rx) = oneshot::channel();
    spawn_session_task(params, session_id_tx);
    
    let session_id = session_id_rx.await??;
    
    // ... 监控会话完成 ...
    
    Ok(())
}
```

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
- `##` 或 `###` 表示节点（层级可选）
- `**agent**`: Agent 名称
- `**task**`: 任务文档路径（可选）
- `**depends_on**`: 依赖节点名称列表
- **ID 自动生成**：系统为每个节点生成 UUID

### 4.2 简化格式（支持内联任务）

```markdown
## backend-api
- **agent**: backend-agent
- **task_inline**: 实现登录 API，包括 JWT 认证
- **depends_on**: []

## frontend-ui
- **agent**: frontend-agent
- **task_inline**: 实现登录界面
- **depends_on**: [backend-api]
```

**实现**：在 `TaskNodeBuilder` 中添加 `task_inline` 字段支持

```rust
// dag_parser.rs
match key.as_str() {
    "task" => {
        builder.task_path = Some(value);
    }
    "task_inline" => {  // 新增
        builder.task_inline = Some(value);
    }
    // ...
}
```

---

## 5. 文件结构

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

## 6. 完整流程

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
     - 读取 AGENT.md（项目上下文）
     - 读取 agent.persona（Agent 上下文）
     - 读取任务文档或使用内联任务
     - 构建 initial_prompt
     - 生成计划文件
     - 提交到 TaskScheduler（传递 initial_prompt）

5. TaskScheduler 派发
   TaskScheduler::dispatch_task()
   ↓
   - 解析计划文件
   - 创建 SessionParams (config, cwd, initial_prompt)
   - spawn_session_task(params, ...)

6. ACP 会话创建
   run_sdk_session()
   ↓
   - InitializeRequest
   - NewSessionRequest
   - **新增**: PromptRequest(initial_prompt)  ← AGENT.md 注入点
   - 进入命令循环

7. Agent 执行
   - 接收初始 Prompt（项目上下文 + 任务指令）
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

## 7. 错误处理

### 7.1 AGENT.md 读取失败

- 如果 `.ergatai/AGENT.md` 不存在 → 使用默认提示 "No project context provided."
- 如果读取失败 → 记录错误，继续使用默认提示
- 如果文件过大（>10KB）→ 截断并警告

### 7.2 任务文档读取失败

- 如果 `task_path` 指定的文件不存在 → 回退到 `task` 字段（内联描述）
- 如果读取失败 → 回退到 `task` 字段

### 7.3 初始 Prompt 发送失败

- 如果 ACP 会话创建成功但初始 Prompt 失败 → 标记节点为 Failed，记录错误
- 不重试（避免部分执行的混乱状态）

### 7.4 Agent 执行失败

- 使用现有的重试机制（`TaskGraph::retry_failed()`）
- 如果超过最大重试次数 → 标记为 Failed
- **需要添加**：将依赖该节点的下游标记为 Skipped

---

## 8. 并发控制

### 8.1 现有机制

`TaskScheduler` 已有并发控制：
- 检查 Agent 可用性（`AgentAvailability`）
- 队列管理（`pending_tasks`）
- 调度策略（`WaitForAgent`, `QueueTask`, `Parallel`）

### 8.2 新增限制

```rust
const MAX_CONCURRENT_AGENTS: usize = 5;

impl TaskScheduler {
    async fn dispatch_loop(&self) {
        loop {
            // 检查当前运行数量
            let running_count = self.running_count().await;
            if running_count >= MAX_CONCURRENT_AGENTS {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            
            // 派发下一个任务
            if let Some(task) = self.pending_tasks.lock().await.pop() {
                self.dispatch_task(&task).await?;
            } else {
                break;
            }
        }
    }
}
```

---

## 9. 失败传播

### 9.1 问题

当某节点失败时，依赖它的下游节点会永远停留在 `Pending` 状态。

### 9.2 解决方案

**添加 Skipped 状态**：

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

## 10. 测试计划

### 10.1 单元测试

1. **AGENT.md 读取**
   - 文件存在 → 正确读取
   - 文件不存在 → 使用默认提示
   - 文件过大 → 截断

2. **初始 Prompt 构建**
   - 项目上下文 + Agent persona + 任务内容
   - 各部分缺失时的回退逻辑

3. **失败传播**
   - 单节点失败 → 下游被跳过
   - 多层依赖 → 递归跳过

### 10.2 集成测试

1. **完整流程**
   - 创建 DAG Markdown
   - 解析 → TaskGraph
   - 提交 → 派发（带 AGENT.md）
   - 执行 → 完成
   - 验证 Agent 接收到了 AGENT.md

2. **边界情况**
   - 空 DAG
   - 单节点 DAG
   - 全并行 DAG
   - 全串行 DAG
   - 节点失败 → 下游跳过

---

## 11. 文件修改清单

### 11.1 修改文件

```
src-rust/src/acp/sdk_session.rs
  - spawn_session_task(): 添加 initial_prompt 参数
  - run_sdk_session(): 注入初始 Prompt

src-rust/src/cross_agent/dag_scheduler.rs
  - generate_and_submit(): 读取 AGENT.md，构建 initial_prompt
  - 新增 read_project_context()
  - 新增 get_task_content()
  - 新增 on_node_failed()
  - 新增 skip_downstream()

src-rust/src/cross_agent/task_scheduler.rs
  - PendingTask: 添加 initial_prompt 字段
  - 新增 submit_task_with_context()
  - dispatch_task(): 传递 initial_prompt 到 spawn_session_task

src-rust/src/orchestration/dag_topology.rs
  - TaskStatus: 添加 Skipped 变体
  - TaskGraph::is_complete(): 考虑 Skipped 状态

src-rust/src/orchestration/dag_parser.rs
  - TaskNodeBuilder: 添加 task_inline 字段
  - parse_property(): 支持 task_inline
```

### 11.2 不新增文件

所有功能通过扩展现有模块实现。

---

## 12. 总结

### 12.1 核心变更

1. **AGENT.md 强制注入** - 修改 ACP 会话创建流程
2. **失败传播** - 添加 Skipped 状态和传播逻辑
3. **内联任务** - 支持 task_inline 字段

### 12.2 优势

1. **复用现有基础设施** - 不重复造轮子
2. **最小改动** - 只修改必要的接口
3. **向后兼容** - 不影响现有功能
4. **清晰的责任分离** - DAG 解析、调度、执行各司其职

### 12.3 风险

1. **ACP 协议限制** - 初始 Prompt 消耗额外 token
2. **Agent 行为不确定** - 某些 Agent 可能忽略长上下文
3. **并发复杂性** - 需要仔细处理锁和状态

### 12.4 后续优化

1. **事件驱动完成监控** - 用 channel/notify 替代轮询
2. **AGENT.md 缓存** - 避免重复读取
3. **DAG 可视化** - 生成 Mermaid 图
