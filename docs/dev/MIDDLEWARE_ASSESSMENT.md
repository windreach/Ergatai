# 中间件架构改造效果评估

**评估日期**: 2026-08-14  
**评估范围**: 中间件架构完整性、ACP 协议使用情况

---

## 一、改造效果总体评估

### ✅ 已完成的改造（约 60%）

#### 1. 基础架构层
- ✅ **HTTP ACP 客户端** (`http_client.rs`, 369 行)
  - 连接管理
  - 会话创建
  - 基础消息发送
  
- ✅ **MCP 服务器** (`ergatai-api/src/mcp/`, 1,507 行)
  - Agent 注册表
  - 工具定义和分发
  - 消息中继

- ✅ **通信协议**
  - 代理通过 MCP 连接 Ergatai
  - 代理暴露 ACP HTTP 端点
  - 双向通信机制建立

#### 2. 代理管理
- ✅ **Agent 注册表** - 跟踪已连接的代理
- ✅ **ACP 端点注册** - 代理可以注册自己的 HTTP 端点
- ✅ **消息路由** - 通过 HTTP 向代理推送消息

#### 3. 示例和测试
- ✅ **示例代理** - 完整的中间件模式示例
- ✅ **集成测试** - 端到端流程验证

---

### ❌ 未完成的改造（约 40%）

#### 1. **DAG 执行系统完全禁用** ⚠️ 严重
**位置**: `crates/ergatai-collab/src/agent_launcher.rs:423-439`

```rust
async fn spawn_acp_session(...) -> ErgataiResult<()> {
    // TODO(middleware): Implement HTTP ACP connection
    tracing::warn!("Agent spawning disabled in middleware mode...");
    Err(ErgataiError::AgentSpawnFailed(...))
}
```

**影响**: 
- DAG 工作流无法执行
- 多代理协作功能失效
- 这是中间件的核心功能，目前不可用

**需要实现**:
```rust
// 伪代码
async fn spawn_acp_session(&self, agent_id: &str, ...) {
    // 1. 从 AgentRegistry 获取 ACP 端点
    let endpoint = registry.get_acp_endpoint(agent_id).await?;
    
    // 2. 通过 HttpConnectionManager 连接
    let session_id = http_manager.connect(agent_id, &endpoint, cwd, kind).await?;
    
    // 3. 发送指令
    http_manager.send_prompt(agent_id, instruction).await?;
    
    // 4. 监听完成事件
    // ...
}
```

#### 2. **MCP 服务器功能部分禁用**
**位置**: `crates/ergatai-core/src/signal.rs:143,166`

```rust
// TODO(middleware): Re-enable after HTTP client migration
// tracing::info!("Step 1/5: shutting down agent pools...");
// crate::acp::sdk_pool_manager::acp_pool_shutdown_all().await;
```

**影响**: 优雅关闭流程不完整

---

## 二、ACP 协议使用情况分析

### 📊 使用率统计

| 功能类别 | ACP 功能 | 使用状态 | 代码位置 |
|---------|---------|---------|---------|
| **连接管理** | Initialize | ✅ 已使用 | http_client.rs:121 |
| | NewSession | ✅ 已使用 | http_client.rs:129 |
| | Close | ✅ 已使用 | http_client.rs:168 |
| **消息交互** | Prompt | ✅ 已使用 | http_client.rs:148 |
| | SessionNotification | ⚠️ 接收但未处理 | http_client.rs:84-89 |
| | PermissionRequest | ⚠️ 自动批准 | http_client.rs:94-112 |
| **会话控制** | SetMode | ❌ 未实现 | http_client.rs:160 |
| | Steer | ❌ 未实现 | http_client.rs:165 |
| **高级功能** | ToolUse | ❌ 未使用 | - |
| | ToolResult | ❌ 未使用 | - |
| | Sampling | ❌ 未使用 | - |
| | Resources | ❌ 未使用 | - |
| | Logging | ❌ 未使用 | - |

### 📈 使用率：**约 30%**（基础功能）

---

### 🔍 详细分析

#### ✅ 已使用的功能（3/10）

1. **Initialize** - 协议握手
   ```rust
   InitializeRequest::new(ProtocolVersion::V1)
   ```

2. **NewSession** - 创建会话
   ```rust
   NewSessionRequest::new(PathBuf::from(&cwd))
   ```

3. **Prompt** - 发送消息
   ```rust
   PromptRequest::new(session_id, vec![ContentBlock::Text(...)])
   ```

#### ⚠️ 部分使用的功能（2/10）

4. **SessionNotification** - 接收通知
   ```rust
   // 只是打印日志，未转发到事件总线
   info!("Received notification from agent: {:?}", notification.update);
   // TODO: Forward to event bus
   ```
   
   **问题**: 代理发送的状态更新、进度通知等未被处理

5. **PermissionRequest** - 权限请求
   ```rust
   // 自动批准第一个选项，无 UI 交互
   let option_id = request.options.first().map(|o| o.option_id.clone());
   if let Some(id) = option_id {
       let _ = responder.respond(...Selected(id));
   }
   // TODO: Implement proper approval flow
   ```
   
   **问题**: 
   - 无用户确认流程
   - 安全风险：自动批准所有权限请求

#### ❌ 未使用的功能（5/10）

6. **SetMode** - 设置代理模式
   ```rust
   // TODO: Implement mode setting
   let _ = reply_tx.send(Ok(()));  // 直接返回成功，未实现
   ```

7. **Steer** - 引导/纠偏代理
   ```rust
   // TODO: Implement steering
   let _ = reply_tx.send(Ok(()));  // 直接返回成功，未实现
   ```

8. **ToolUse / ToolResult** - 工具调用
   - 完全未使用
   - 中间件无法监控代理的工具调用

9. **Sampling** - 采样请求
   - 未使用
   - 无法实现代理输出采样

10. **Resources / Logging** - 资源和日志
    - 未使用
    - 无法收集代理的资源使用情况和日志

---

## 三、作为中间件的合适性评估

### ✅ 合适的方面

1. **架构方向正确**
   - 代理独立运行，生命周期自管理
   - 通过标准协议（MCP/ACP）通信
   - 松耦合设计

2. **通信机制完善**
   - MCP: 代理 → Ergatai（工具调用）
   - ACP HTTP: Ergatai → 代理（推送消息）
   - 双向通信已建立

3. **代理发现机制**
   - 代理通过 MCP 注册
   - 代理暴露 ACP 端点
   - 动态发现和管理

### ❌ 不合适的方面

1. **核心功能缺失**
   - DAG 执行无法工作 → 多代理协作失效
   - 这是中间件最重要的功能之一

2. **ACP 协议使用不充分**
   - 只用了 30% 的基础功能
   - 无法监控代理状态
   - 无法控制代理行为
   - 无法收集代理数据

3. **缺少中间件应有的功能**
   - ❌ 代理状态监控
   - ❌ 性能指标收集
   - ❌ 日志聚合
   - ❌ 权限管理 UI
   - ❌ 代理健康检查
   - ❌ 负载均衡

---

## 四、改进建议

### 🔴 高优先级（必须修复）

#### 1. 完成 DAG 执行的 HTTP ACP 集成
**预估工作量**: 2-3 天  
**影响**: 核心功能恢复

```rust
// agent_launcher.rs
async fn spawn_acp_session(&self, agent_id: &str, ...) {
    let registry = get_agent_registry().await;
    let endpoint = registry.get_acp_endpoint(agent_id)
        .ok_or_else(|| anyhow!("Agent {} has no ACP endpoint", agent_id))?;
    
    let http_manager = http_connection_manager();
    let session_id = http_manager
        .connect(agent_id, &endpoint, worktree_path.to_string_lossy(), SessionKind::Dag)
        .await?;
    
    // 发送指令
    http_manager.send_prompt(agent_id, instruction).await?;
    
    // 监听完成事件（通过 SessionNotification）
    // ...
}
```

#### 2. 实现 SessionNotification 处理
**预估工作量**: 1 天  
**影响**: 代理状态监控

```rust
// http_client.rs
.on_receive_notification(
    async |notification: SessionNotification, _conn: ConnectionTo<Agent>| {
        // 转发到 NATS 事件总线
        let event_bus = get_event_bus();
        event_bus.publish_agent_notification(agent_id, notification).await;
        Ok(())
    },
    ...
)
```

#### 3. 实现权限审批流程
**预估工作量**: 2 天  
**影响**: 安全性

```rust
// 方案 1: CLI UI（ratatui）
// 方案 2: HTTP API（外部审批）
// 方案 3: 配置文件预设规则
```

### 🟡 中优先级（建议实现）

#### 4. 实现 SetMode 和 Steer
**预估工作量**: 1 天  
**影响**: 代理控制能力

#### 5. 添加工具调用监控
**预估工作量**: 1-2 天  
**影响**: 可观测性

```rust
// 拦截 ToolUse 请求，记录到数据库
.on_receive_request(
    async |request: ToolUseRequest, ...| {
        log_tool_call(agent_id, request.tool_name, request.arguments).await;
        // 转发给代理
        Ok(())
    },
    ...
)
```

#### 6. 添加代理健康检查
**预估工作量**: 1 天  
**影响**: 可靠性

```rust
// 定期检查 ACP 端点
async fn health_check_loop() {
    loop {
        let agents = registry.list_agents().await;
        for agent in agents {
            if let Some(endpoint) = agent.acp_endpoint {
                let healthy = check_health(&endpoint).await;
                if !healthy {
                    warn!("Agent {} is unhealthy", agent.id);
                }
            }
        }
        sleep(Duration::from_secs(30)).await;
    }
}
```

### 🟢 低优先级（可选）

#### 7. 实现 Resources 和 Logging
**预估工作量**: 2 天  
**影响**: 资源管理和日志聚合

#### 8. 添加性能指标
**预估工作量**: 1-2 天  
**影响**: 性能优化

---

## 五、总结

### 当前状态：**半成品中间件** ⚠️

- ✅ 架构方向正确（60% 完成）
- ❌ 核心功能缺失（DAG 执行）
- ⚠️ ACP 协议使用不充分（30%）
- ❌ 缺少中间件应有的监控和管理功能

### 是否适合作为中间件？

**短期（当前状态）**: ❌ **不适合生产使用**
- DAG 执行无法工作
- 核心功能缺失

**中期（完成高优先级后）**: ✅ **适合作为轻量级中间件**
- 恢复 DAG 执行
- 基本监控能力

**长期（完成所有改进后）**: ✅✅ **适合成为完整的协作中间件**
- 完整的 ACP 协议支持
- 完善的监控和管理
- 生产级可靠性

### 建议下一步

1. **立即**: 完成 DAG 执行的 HTTP ACP 集成（2-3 天）
2. **本周**: 实现 SessionNotification 处理（1 天）
3. **下周**: 实现权限审批流程（2 天）
4. **后续**: 逐步添加监控、健康检查等功能

完成高优先级改进后，这个中间件就可以投入使用了。
