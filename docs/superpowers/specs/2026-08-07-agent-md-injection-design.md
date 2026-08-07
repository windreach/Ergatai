# AGENT.md 强制注入与 DAG 任务编排设计

**日期**: 2026-08-07  
**状态**: 设计完成，待审查

---

## 1. 概述

### 1.1 目标

确保所有执行 Agent 在开始工作前**强制接收**项目架构和协作规范，通过 DAG（有向无环图）编排多 Agent 协作任务。

### 1.2 核心需求

1. **强制注入上下文**：主 Agent 输出的 AGENT.md 必须被所有执行 Agent 接收
2. **DAG 任务编排**：支持节点依赖、层级、并行执行
3. **灵活任务定义**：
   - 详细模式：节点关联任务文档（`.plan/{node-name}.md`）
   - 快速模式：节点直接内联任务文字
4. **唯一标识**：系统为每个节点生成 UUID，内部使用 ID 调度（避免名称冲突）

### 1.3 约束

- 一个节点最多一个任务文档
- 不允许多个节点共享任务文档
- 任务文档可选（支持快速协作）
- 节点名称必须唯一，只允许字母、数字、连字符、下划线

---

## 2. 文件结构

```
.ergatai/
├── AGENT.md                    # 项目级上下文（所有 Agent 共享）
├── graphs/
│   ├── {task-id}.md           # 调用图（节点、依赖、层级）
│   └── ...
└── .plan/
    └── {task-id}/
        ├── {node-name}.md     # 节点任务文档（可选）
        ├── {node-name-2}.md   # 另一个节点的任务文档
        └── results/
            └── {node-id}.md   # Agent 执行结果
```

### 2.1 调用图格式（graph.md）

```markdown
# backend-api
- agent: @backend-agent
- task: 实现登录 API，包括 JWT 认证和数据库存储
- depends: []

## frontend-ui
- agent: @frontend-agent
- task: 实现登录界面，包括表单验证和 API 调用
- depends: [backend-api]

### integration-test
- agent: @qa-agent
- task: 执行端到端测试，验证登录流程
- depends: [backend-api, frontend-ui]
```

**格式规则**：
- `#` 数量表示层级（`#` = Level 1, `##` = Level 2, 等）
- 节点名称紧跟 `#` 后面（如 `# backend-api`）
- `agent`: 执行该节点的 Agent 名称
- `task`: 任务描述（内联文字）
- `depends`: 依赖的节点名称列表（引用其他节点的名称）
- **不写 ID**：ID 由系统自动生成

### 2.2 任务文档格式（可选）

```markdown
# Task: 实现登录 API

## Objective
实现完整的用户登录功能，包括 JWT 认证、密码加密、数据库存储。

## Requirements
- 使用 bcrypt 加密密码
- JWT token 有效期 24 小时
- 支持 email + password 登录
- 返回 token 和用户信息

## Files to Create/Modify
- `src/api/auth.rs` - 登录 API 端点
- `src/db/users.rs` - 用户数据库操作
- `src/models/user.rs` - 用户模型

## Constraints
- 不要修改现有的数据库迁移文件
- 必须使用 Drizzle ORM
- 错误处理使用 anyhow + thiserror

## Result Format
完成后写入 `.ergatai/.plan/{task-id}/results/{node-id}.md`
```

---

## 3. 架构设计

### 3.1 核心组件

```
┌─────────────────────────────────────────────────────────┐
│ 1. Graph Parser (Rust)                                  │
│    - 解析 graph.md                                      │
│    - 提取节点、依赖、层级                               │
│    - 验证节点名称唯一性                                 │
│    - 验证依赖引用有效性                                 │
└────────────────────┬────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────────────────┐
│ 2. DAG Builder                                          │
│    - 构建有向无环图                                     │
│    - 为每个节点生成 UUID                                │
│    - 建立名称 → ID 映射                                 │
│    - 将依赖名称转换为依赖 ID                            │
│    - 检测循环依赖                                       │
└────────────────────┬────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────────────────┐
│ 3. Task Dispatcher                                      │
│    - 计算就绪节点（无依赖或依赖已完成）                 │
│    - 为就绪节点派发任务：                               │
│      a. 检查是否存在 .plan/{task-id}/{node-name}.md     │
│      b. 如果存在：读取任务文档                          │
│      c. 如果不存在：使用 graph.md 中的 task 字段        │
│    - 读取 .ergatai/AGENT.md（项目级上下文）             │
│    - 合并 Agent 配置的 persona（Agent 级上下文）        │
│    - 创建 ACP 会话                                      │
│    - 注入合并后的上下文作为第一个 Prompt                │
│    - 发送任务指令                                       │
└────────────────────┬────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────────────────┐
│ 4. Execution Agents                                     │
│    - 接收初始 Prompt（项目上下文 + 任务指令）           │
│    - 在隔离 worktree 中执行                             │
│    - 写入结果到 .plan/{task-id}/results/{node-id}.md    │
└────────────────────┬────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────────────────┐
│ 5. Completion Monitor                                   │
│    - 监控 Agent 完成状态                                │
│    - 更新 DAG 节点状态（pending → running → completed） │
│    - 触发下一批就绪节点                                 │
│    - 所有节点完成后合并结果                             │
└─────────────────────────────────────────────────────────┘
```

### 3.2 数据流

```
1. 主 Agent 创建调用图
   - 写入 .ergatai/graphs/{task-id}.md
   - 可选写入 .ergatai/.plan/{task-id}/{node-name}.md

2. 系统解析调用图
   graph.md → Graph Parser → RawGraph
   RawGraph → DAG Builder → DAG (with UUIDs)

3. 系统派发任务
   DAG → Task Dispatcher → Ready Nodes
   for each ready node:
     - Read task doc or use inline task
     - Read .ergatai/AGENT.md + agent.persona
     - Create ACP session
     - Inject context as first prompt
     - Send task instruction

4. Agent 执行
   Agent → Execute → Write result to results/{node-id}.md

5. 监控完成
   Completion Monitor → Update DAG → Trigger next batch
   All completed → Merge results
```

---

## 4. 关键实现细节

### 4.1 Graph Parser

**输入**: graph.md 内容（字符串）  
**输出**: RawGraph 结构（节点列表，未生成 ID）

**验证规则**:
1. 节点名称不能为空
2. 节点名称只能包含字母、数字、连字符、下划线
3. 节点名称必须唯一
4. depends 引用的节点必须存在

**示例实现**:
```rust
pub struct RawNode {
    pub name: String,
    pub level: usize,
    pub agent: String,
    pub task: String,
    pub depends: Vec<String>,  // 依赖节点名称
}

pub struct RawGraph {
    pub nodes: Vec<RawNode>,
}

pub fn parse_graph(content: &str) -> Result<RawGraph> {
    let mut nodes = Vec::new();
    let mut node_names = HashSet::new();
    
    for node in parse_nodes_from_markdown(content)? {
        validate_node_name(&node.name)?;
        
        if !node_names.insert(&node.name) {
            bail!("Duplicate node name: {}", node.name);
        }
        
        // Validate dependencies
        for dep in &node.depends {
            if !node_names.contains(dep) {
                // Check if dependency appears later in the file
                if !content.contains(&format!("# {}", dep)) {
                    bail!("Node '{}' depends on non-existent node '{}'", 
                          node.name, dep);
                }
            }
        }
        
        nodes.push(node);
    }
    
    Ok(RawGraph { nodes })
}

fn validate_node_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Node name cannot be empty");
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        bail!("Node name contains invalid characters: {}", name);
    }
    Ok(())
}
```

### 4.2 DAG Builder

**输入**: RawGraph  
**输出**: DAG 结构（节点带 UUID，依赖转换为 ID）

**功能**:
1. 为每个节点生成 UUID
2. 建立 name → UUID 映射
3. 将 depends（名称列表）转换为 depends_on（ID 列表）
4. 检测循环依赖

**示例实现**:
```rust
pub struct DagNode {
    pub id: String,              // UUID
    pub name: String,
    pub level: usize,
    pub agent: String,
    pub task: String,
    pub depends_on: Vec<String>, // 依赖节点 ID
    pub status: NodeStatus,      // Pending | Running | Completed | Failed
}

pub struct DAG {
    pub nodes: Vec<DagNode>,
    pub node_map: HashMap<String, usize>, // name → index
}

pub fn build_dag(raw: RawGraph) -> Result<DAG> {
    // Generate UUIDs and build name → id mapping
    let mut name_to_id = HashMap::new();
    let mut nodes = Vec::new();
    
    for raw_node in raw.nodes {
        let id = Uuid::new_v4().to_string();
        name_to_id.insert(raw_node.name.clone(), id.clone());
        
        nodes.push(DagNode {
            id,
            name: raw_node.name,
            level: raw_node.level,
            agent: raw_node.agent,
            task: raw_node.task,
            depends_on: Vec::new(),  // Will be filled in next pass
            status: NodeStatus::Pending,
        });
    }
    
    // Convert dependency names to IDs
    for (i, raw_node) in raw.nodes.iter().enumerate() {
        for dep_name in &raw_node.depends {
            let dep_id = name_to_id.get(dep_name)
                .ok_or_else(|| anyhow!("Dependency not found: {}", dep_name))?;
            nodes[i].depends_on.push(dep_id.clone());
        }
    }
    
    // Build node map
    let node_map = nodes.iter()
        .enumerate()
        .map(|(i, n)| (n.name.clone(), i))
        .collect();
    
    let dag = DAG { nodes, node_map };
    
    // Detect cycles
    dag.detect_cycles()?;
    
    Ok(dag)
}

impl DAG {
    fn detect_cycles(&self) -> Result<()> {
        // Topological sort to detect cycles
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        
        for node in &self.nodes {
            if !visited.contains(&node.id) {
                if self.detect_cycle_util(&node.id, &mut visited, &mut rec_stack) {
                    bail!("Cycle detected in DAG");
                }
            }
        }
        Ok(())
    }
    
    fn detect_cycle_util(
        &self,
        node_id: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> bool {
        visited.insert(node_id.to_string());
        rec_stack.insert(node_id.to_string());
        
        let node = &self.nodes[self.node_map.iter()
            .find(|(_, &i)| self.nodes[i].id == node_id)
            .unwrap().1];
        
        for dep_id in &node.depends_on {
            if !visited.contains(dep_id) {
                if self.detect_cycle_util(dep_id, visited, rec_stack) {
                    return true;
                }
            } else if rec_stack.contains(dep_id) {
                return true;
            }
        }
        
        rec_stack.remove(node_id);
        false
    }
    
    pub fn ready_nodes(&self) -> Vec<&DagNode> {
        self.nodes.iter().filter(|node| {
            node.status == NodeStatus::Pending &&
            node.depends_on.iter().all(|dep_id| {
                self.nodes.iter()
                    .find(|n| n.id == *dep_id)
                    .map(|n| n.status == NodeStatus::Completed)
                    .unwrap_or(false)
            })
        }).collect()
    }
}
```

### 4.3 ACP 会话初始 Prompt 注入

**修改点**: `src-rust/src/acp/sdk_session.rs`

**添加 AgentConfig 字段**:
```rust
// src-rust/src/agent/config.rs
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub persona: Option<String>,
    pub initial_prompt: Option<String>,  // 新增
    // ... other fields
}
```

**修改会话创建逻辑**:
```rust
// src-rust/src/acp/sdk_session.rs (在 NewSessionRequest 之后)
let session_id = session_response.session_id.to_string();

// 新增：如果提供了 initial_prompt，立即发送
if let Some(initial_prompt) = &config.initial_prompt {
    tracing::info!(session_id = %session_id, "Sending initial prompt");
    
    connection.send_request(PromptRequest::new(
        SessionId::new(session_id.clone()),
        vec![ContentBlock::Text(TextContent::new(initial_prompt.clone()))],
    )).block_task().await
    .map_err(|e| anyhow!("Initial prompt failed: {}", e))?;
}
```

### 4.4 Task Dispatcher

**核心逻辑**:
```rust
pub struct TaskDispatcher {
    project_root: PathBuf,
    task_id: String,
    dag: Arc<Mutex<DAG>>,
}

impl TaskDispatcher {
    pub async fn dispatch_ready_nodes(&self) -> Result<()> {
        let dag = self.dag.lock().await;
        let ready = dag.ready_nodes();
        
        for node in ready {
            self.dispatch_node(node).await?;
        }
        
        Ok(())
    }
    
    async fn dispatch_node(&self, node: &DagNode) -> Result<()> {
        // 1. Determine task content
        let task_content = self.get_task_content(node).await?;
        
        // 2. Read project context
        let project_context = self.read_project_context().await?;
        
        // 3. Read agent config
        let agent_config = get_agent_config(&node.agent)?;
        let persona = agent_config.persona.unwrap_or_default();
        
        // 4. Build initial prompt
        let initial_prompt = format!(
            "# Project Context\n\n{}\n\n---\n\n# Your Task\n\n{}",
            project_context,
            task_content
        );
        
        // 5. Create ACP session with initial prompt
        let mut config = agent_config.clone();
        config.initial_prompt = Some(initial_prompt);
        
        let (session_id_tx, session_id_rx) = oneshot::channel();
        spawn_session_task(config, self.worktree_path(node).await?, session_id_tx);
        
        let session_id = session_id_rx.await??;
        
        // 6. Update node status
        let mut dag = self.dag.lock().await;
        dag.update_status(&node.id, NodeStatus::Running)?;
        
        Ok(())
    }
    
    async fn get_task_content(&self, node: &DagNode) -> Result<String> {
        // Check if task doc exists
        let task_doc_path = self.project_root
            .join(".ergatai/.plan")
            .join(&self.task_id)
            .join(format!("{}.md", node.name));
        
        if task_doc_path.exists() {
            // Read task doc
            tokio::fs::read_to_string(&task_doc_path).await
                .with_context(|| format!("Failed to read task doc: {:?}", task_doc_path))
        } else {
            // Use inline task
            Ok(node.task.clone())
        }
    }
    
    async fn read_project_context(&self) -> Result<String> {
        let agent_md_path = self.project_root.join(".ergatai/AGENT.md");
        
        if agent_md_path.exists() {
            tokio::fs::read_to_string(&agent_md_path).await
                .with_context(|| "Failed to read AGENT.md")
        } else {
            Ok("No project context provided.".to_string())
        }
    }
}
```

### 4.5 Completion Monitor

**功能**:
- 轮询检查 Agent 完成状态
- 更新 DAG 节点状态
- 触发下一批就绪节点

```rust
pub struct CompletionMonitor {
    dag: Arc<Mutex<DAG>>,
    dispatcher: Arc<TaskDispatcher>,
}

impl CompletionMonitor {
    pub async fn monitor(&self) -> Result<()> {
        loop {
            // Check for completed agents
            let completed = self.check_completed_agents().await?;
            
            // Update DAG
            {
                let mut dag = self.dag.lock().await;
                for node_id in completed {
                    dag.update_status(&node_id, NodeStatus::Completed)?;
                }
            }
            
            // Dispatch next batch
            self.dispatcher.dispatch_ready_nodes().await?;
            
            // Check if all nodes completed
            if self.all_completed().await? {
                break;
            }
            
            // Wait before next check
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        
        Ok(())
    }
}
```

---

## 5. 并发控制

### 5.1 并发限制

```rust
const MAX_CONCURRENT_AGENTS: usize = 5;

impl TaskDispatcher {
    pub async fn dispatch_ready_nodes(&self) -> Result<()> {
        let dag = self.dag.lock().await;
        let ready = dag.ready_nodes();
        
        // Limit concurrent agents
        let running_count = dag.nodes.iter()
            .filter(|n| n.status == NodeStatus::Running)
            .count();
        
        let available_slots = MAX_CONCURRENT_AGENTS.saturating_sub(running_count);
        let to_dispatch = ready.into_iter().take(available_slots);
        
        for node in to_dispatch {
            self.dispatch_node(node).await?;
        }
        
        Ok(())
    }
}
```

---

## 6. 错误处理

### 6.1 解析错误

- 节点名称无效 → 返回错误，停止解析
- 节点名称重复 → 返回错误，停止解析
- 依赖引用不存在 → 返回错误，停止解析
- 检测到循环依赖 → 返回错误，停止解析

### 6.2 执行错误

- Agent 启动失败 → 标记节点为 Failed，记录错误
- Agent 执行失败 → 标记节点为 Failed，记录错误日志
- 任务文档读取失败 → 回退到内联任务

### 6.3 恢复策略

- 单个节点失败不影响其他节点（除非依赖该节点）
- 可以重试失败的节点
- 所有节点完成后，汇总成功/失败状态

---

## 7. 测试计划

### 7.1 单元测试

1. **Graph Parser**
   - 解析简单图（无依赖）
   - 解析复杂图（多层依赖）
   - 验证节点名称唯一性
   - 验证依赖引用
   - 验证节点名称格式

2. **DAG Builder**
   - 生成 UUID
   - 名称 → ID 映射
   - 依赖转换
   - 循环检测

3. **Task Dispatcher**
   - 读取任务文档
   - 回退到内联任务
   - 读取项目上下文
   - 构建初始 Prompt

### 7.2 集成测试

1. **完整流程**
   - 创建 graph.md
   - 解析 → DAG
   - 派发任务
   - 监控完成
   - 验证结果

2. **边界情况**
   - 空图（无节点）
   - 单节点图
   - 全并行图（无依赖）
   - 全串行图（链式依赖）

---

## 8. 文件清单

### 8.1 新增文件

```
src-rust/src/orchestration/
├── graph_parser.rs        # Graph Parser
├── dag_builder.rs         # DAG Builder
├── task_dispatcher.rs     # Task Dispatcher
└── completion_monitor.rs  # Completion Monitor

src-rust/src/acp/
└── sdk_session.rs         # 修改：支持初始 Prompt 注入

src-rust/src/agent/
└── config.rs              # 修改：添加 initial_prompt 字段
```

### 8.2 修改文件

```
src-rust/src/lib.rs        # 导出新模块
src-rust/src/napi/tasks.rs # 添加 NAPI 接口
```

---

## 9. 后续优化

### 9.1 短期优化

- 添加 DAG 可视化（生成 Mermaid 图）
- 支持任务优先级
- 支持任务超时控制

### 9.2 长期优化

- 支持动态 DAG（运行时添加节点）
- 支持条件依赖（节点 A 完成后，根据结果决定是否执行节点 B）
- 支持 Agent 池（预创建 ACP 会话，减少启动开销）

---

## 10. 总结

本设计实现了一个完整的 AGENT.md 强制注入和 DAG 任务编排系统：

1. **强制注入**：通过 ACP 初始 Prompt 注入项目上下文
2. **DAG 编排**：支持节点依赖、层级、并行执行
3. **灵活任务**：支持详细任务文档和快速内联任务
4. **唯一标识**：系统生成 UUID，内部使用 ID 调度

系统设计遵循 Rust 最佳实践，模块化清晰，易于测试和维护。
