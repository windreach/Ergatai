# Ergatai 工作区 Rust 安全审查报告

**审查范围**: 62 个 Rust 源文件，覆盖 10 个 crate + 1 个 example
**审查日期**: 2026-08-14
**编译状态**: `cargo check` 通过（0 errors, 18 warnings）；`cargo test --no-run` 有 2 个测试编译错误

---

## 执行摘要

代码整体质量较高，无裸指针、无 `transmute`、无 `Box::leak`、无 `mem::forget`。所有权模型遵循 Rust 惯用法，未发现悬垂引用或自引用结构体。主要问题集中在：**环境变量安全**、**进程注入面**、**全局单例模式**、以及若干 **设计级 TOCTOU 竞态**。

---

## 🔴 高危问题

### 🔴-1: `unsafe { std::env::set_var() }` — 多线程不安全

**文件**: `crates/ergatai-api/src/main.rs:95`
**类别**: 内存安全 / Undefined Behavior

```rust
// 当前代码
if args.verbose {
    // Safety: set_var is called before any threads are spawned
    unsafe { std::env::set_var("RUST_LOG", "debug") };
}
```

**问题**: Rust 2024 edition 将 `std::env::set_var` 标记为 `unsafe`，因为并发 `set_var` + `env::var` 是 UB。虽然注释声称"在任何线程启动前调用"，但：
- `#[tokio::main]` 在 `main()` 执行前已经启动了 tokio runtime（包含线程池）
- 因此**实际上已经有多线程存在**，此调用确实是 UB

**修复建议**:
```rust
// 方案 1: 在 tokio::main 之前手动设置（推荐）
fn main() -> Result<()> {
    let args = Args::parse();
    if args.verbose {
        std::env::set_var("RUST_LOG", "debug"); // 在 #[tokio::main] 展开之前
    }
    // ... 然后调用 async_main
}

// 方案 2: 不依赖环境变量，直接配置 tracing
let filter = if args.verbose { "debug" } else { "info" };
tracing_subscriber::fmt().with_env_filter(filter).init();
```

---

### 🔴-2: MCP 服务器命令注入面 — 未验证的 config 值直接传递给 Command::new

**文件**: `crates/ergatai-acp/src/mcp.rs:321-336`
**类别**: 安全 / 命令注入

```rust
let mut command = Command::new(&cmd);  // cmd 来自用户 JSON 配置文件
if let Some(args) = &config.args {
    command.args(args);
}
if let Some(env) = &config.env {
    for (k, v) in env {
        command.env(k, v);  // 环境变量也未经验证
    }
}
let child = command
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;
```

**问题**: MCP 服务器配置从三个来源加载：
1. `~/.config/ergatai/mcp.json`（用户配置）
2. `.mcp.json` / `mcp.json`（项目目录 — **可从网络克隆的仓库注入**）
3. `resource/` 目录（内置）

来源 2 特别危险：恶意 `.mcp.json` 可以执行任意命令。虽然 `find_mcp_configs()` 本身不做验证，且 `start_mcp_server` 目前标注为未连接（dead_code），但如果未来被启用，这将是严重的 RCE 漏洞。

**修复建议**:
```rust
// 1. 验证命令路径是否在白名单内
fn validate_command(cmd: &str) -> ErgataiResult<PathBuf> {
    let path = PathBuf::from(cmd);
    // 只允许绝对路径
    if !path.is_absolute() {
        return Err(ErgataiError::permission_denied("MCP command must be absolute path"));
    }
    // 拒绝可疑路径
    if path.to_string_lossy().contains("..") {
        return Err(ErgataiError::permission_denied("Path traversal not allowed"));
    }
    // 可选：限制在已知目录内
    Ok(path)
}

// 2. 项目级 .mcp.json 需要用户确认才能启动
```

---

### 🔴-3: 信号处理器中使用 `process::exit()` — 跳过 Drop 清理

**文件**: `crates/ergatai-core/src/signal.rs:55, 59, 66, 81`
**类别**: 资源泄漏 / 正确性

```rust
std::process::exit(0);  // line 55
std::process::exit(1);  // lines 59, 66, 81
```

**问题**: `process::exit()` **不会运行析构函数**。这意味着：
- 临时文件不会被清理
- SQLite 事务不会被回滚（依赖 WAL 检查点）
- NATS 子进程可能变成孤儿进程
- `FileLockManager` 的 `Drop` 不会执行
- 文件锁可能永远残留（直到 watchdog 超时回收）

虽然 `graceful_shutdown()` 在 `exit(0)` 之前运行，但如果 `graceful_shutdown` 的某个步骤 panic 了，后续的 `exit(1)` 路径就不会执行 Drop。

**修复建议**:
```rust
// 使用 std::process::Termination trait 的正常返回
// 或在 graceful_shutdown 完成后使用 channel 通知 main 函数返回
// 而不是直接 process::exit

// 如果必须用 process::exit（如强制退出场景），至少手动清理关键资源：
fn force_exit(code: i32) -> ! {
    // 确保 NATS 子进程被杀
    let _ = std::process::Command::new("pkill")
        .args(["-f", "nats-server.*ergatai"])
        .output();
    std::process::exit(code);
}
```

---

## 🟡 中等风险问题

### 🟡-1: HTTP ACP 客户端权限自动批准 — 安全风险

**文件**: `crates/ergatai-acp/src/http_client.rs:95-106`
**类别**: 安全 / 权限控制

```rust
// Auto-approve for now (middleware doesn't have UI for approval)
// TODO: Implement proper approval flow
let option_id = request.options.first().map(|o| o.option_id.clone());
if let Some(id) = option_id {
    let _ = responder.respond(
        RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(
                SelectedPermissionOutcome::new(id),
            ),
        ),
    );
}
```

**问题**: 中间件模式下所有权限请求被自动批准，没有任何限制。如果恶意 agent 通过 MCP 连接，可以无限制地执行文件操作。`SessionKind::Dag` 模式也有类似的自动批准（`sdk_session.rs:396-411`），但 DAG 模式是有意为之的（unattended execution）。

**修复建议**: 至少为中间件模式添加配置选项，或实现基础的权限白名单。

---

### 🟡-2: `serde_json::to_value().unwrap()` — 潜在的 panic 点

**文件**: `crates/ergatai-api/src/mcp/server.rs:137, 168, 188`
**类别**: 健壮性 / 错误处理

```rust
result: Some(serde_json::to_value(response).unwrap()),  // 3 处
```

**问题**: 虽然 `InitializeResponse`、`ToolsListResponse`、`ToolCallResponse` 在正常情况下总是可序列化的，但 `unwrap()` 在以下场景会 panic：
- 类型定义变更引入不可序列化字段
- 极端内存条件下序列化失败

**修复建议**:
```rust
result: Some(serde_json::to_value(response).unwrap_or_else(|e| {
    tracing::error!("Failed to serialize response: {}", e);
    serde_json::json!({"error": "internal serialization error"})
})),
```

---

### 🟡-3: 全局 `OnceLock` 单例模式 — 可测试性差 + 隐式耦合

**文件**: 多个文件
**类别**: 架构 / 正确性

| 全局单例 | 文件 | 类型 |
|----------|------|------|
| `STATE` (SessionManager) | `ergatai-acp/src/manager.rs:313` | `OnceLock<GlobalState>` |
| `APPROVAL_WAITERS` | `ergatai-acp/src/sdk_session.rs:58` | `OnceLock<Arc<Mutex<...>>>` |
| `MCP_REGISTRY` | `ergatai-acp/src/mcp.rs:244` | `OnceLock<McpProcessRegistry>` |
| `POOL_MANAGER` | `ergatai-acp/src/sdk_pool_manager.rs:114` | `OnceLock<GlobalPoolManager>` |
| `APP_STATE` | `ergatai-api/src/main.rs:55` | `OnceLock<AppState>` |
| `HTTP_CONNECTION_MANAGER` | `ergatai-api/src/mcp/message_relay.rs:23` | `OnceLock<HttpConnectionManager>` |
| `FILE_ACCESS_MANAGER` | `ergatai-lock/src/manager.rs:32` | `OnceLock<RwLock<...>>` |
| `NATS_STATE` | `ergatai-nats/src/manager.rs:19` | `OnceLock<RwLock<...>>` |
| `GLOBAL_DAG` | `ergatai-collab/src/dag_scheduler.rs:688` | `OnceLock<StdMutex<Option<...>>>` |
| `GLOBAL_SCHEDULER` | `ergatai-collab/src/task_scheduler.rs:570` | `OnceLock<Arc<TaskScheduler>>` |
| `AGENT_STATE` | `examples/simple-agent/src/main.rs:40` | `OnceLock<Arc<AgentState>>` |

**问题**:
1. **测试隔离**: 全局单例无法在测试间重置，导致测试互相干扰（CLAUDE.md 已提到此问题）
2. **隐式依赖**: 模块间通过全局状态耦合，难以理解数据流
3. **`GLOBAL_DAG`**: 使用 `Mutex<Option<DagScheduler>>` 使全局可变且可选，任何代码都可以 `take()` 或 `insert()`

**修复建议**: 考虑引入 `AppContext` 结构体作为依赖注入的根，通过 `Arc<AppContext>` 传递给需要全局状态的组件。

---

### 🟡-4: Mutex 中毒恢复可能导致不一致状态

**文件**: `crates/ergatai-acp/src/manager.rs:336-348`
**类别**: 正确性 / 并发安全

```rust
pub fn poll_events() -> Vec<NapiSessionEvent> {
    let mut rx = match state().event_rx.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("event_rx mutex poisoned, recovering");
            poisoned.into_inner()  // 恢复中毒的 mutex
        }
    };
```

**问题**: Mutex 中毒表示之前的持有者 panic 了。恢复后继续使用可能：
- 读到不完整的数据
- 队列状态不一致
- 后续的 panic 继续传播

**修复建议**: 对于事件轮询，考虑使用 `tokio::sync::mpsc` 代替 `std::sync::Mutex<Receiver>`，或者在恢复时重建状态。

---

### 🟡-5: `examples/simple-agent` — panic 风险 + 无认证监听

**文件**: `examples/simple-agent/src/main.rs:43, 88, 104`
**类别**: 健壮性 / 安全

```rust
fn get_state() -> Arc<AgentState> {
    AGENT_STATE.get().unwrap().clone()  // line 43: 如果 set 失败则 panic
}

let _ = AGENT_STATE.set(state.clone());  // line 88: 静默忽略 set 失败

let addr = format!("0.0.0.0:{}", port);  // line 104: 绑定所有接口
```

**问题**:
1. `unwrap()` 在 `get()` 失败时 panic（如果 `set()` 被第二次调用则失败）
2. 绑定 `0.0.0.0` 使 agent 在所有网络接口上可访问（包括公网）
3. 无认证机制 — 任何能访问网络的人都可以发送 prompt

**修复建议**:
```rust
// 绑定 localhost only
let addr = format!("127.0.0.1:{}", port);
// 或使用 expect 提供有意义的错误信息
let state = AGENT_STATE.get().expect("AGENT_STATE must be initialized before handlers");
```

---

### 🟡-6: `SessionManager::register` / `unregister` — 读锁在写锁之后立即获取

**文件**: `crates/ergatai-acp/src/manager.rs:123-137`
**类别**: 性能 / 竞态条件

```rust
pub async fn register(&self, handle: SessionHandle) {
    self.sessions.write().await.insert(handle.session_id.clone(), handle);
    // 写锁已释放
    let count = self.sessions.read().await.len();  // 再获取读锁
    let _ = self.session_count_watch.0.send(count);
}
```

**问题**: 写锁释放后、读锁获取前，另一个并发操作可能修改了 sessions map。导致 `count` 不准确。

**修复建议**:
```rust
pub async fn register(&self, handle: SessionHandle) {
    let count = {
        let mut sessions = self.sessions.write().await;
        sessions.insert(handle.session_id.clone(), handle);
        sessions.len()  // 在同一个锁内获取计数
    };
    let _ = self.session_count_watch.0.send(count);
}
```

---

### 🟡-7: 权限请求超时后 pending_perms 泄漏（300秒窗口）

**文件**: `crates/ergatai-acp/src/sdk_session.rs:484-509`
**类别**: 内存泄漏 / DoS

```rust
let outcome = match tokio::time::timeout(Duration::from_secs(300), response_rx).await {
    Ok(Ok(Some(option_id))) => {
        if let Ok(mut map) = pending_perms.lock() {
            map.remove(&request_id);  // 成功时移除
        }
        // ...
    }
    _ => {
        if let Ok(mut map) = pending_perms.lock() {
            map.remove(&request_id);  // 超时时移除
        }
        // ...
    }
};
```

**问题**: 虽然超时后有条目清理，但 300 秒的超时窗口很长。如果前端不响应但连接保持，恶意 agent 可以持续发送权限请求，每次在 `pending_perms` 中留下条目直到超时。5 分钟内可以积累大量条目。

相比之下，`acquire_file_locks_for_permission`（同文件 line 213-222）正确使用了 RAII `WaiterCleanup` 模式。建议统一。

**修复建议**: 为 `pending_perms` 也使用 `WaiterCleanup` RAII guard，并考虑降低超时到 30-60 秒。

---

## 🟢 低危问题

### 🟢-1: `close_chat_sessions` 的 target 计算竞态

**文件**: `crates/ergatai-acp/src/manager.rs:256-262`
**类别**: 正确性

```rust
let target = {
    let sessions = self.sessions.read().await;
    sessions.values().filter(|h| h.kind == SessionKind::Dag).count()
};
// target 计算后，DAG 会话数量可能已变化
```

在 `target` 计算和循环检查之间，DAG 会话可能被创建或销毁。

---

### 🟢-2: `NatsServer::Drop` — `child.kill()` 在 Drop 中执行同步 I/O

**文件**: `crates/ergatai-nats/src/server.rs:198-211`
**类别**: 性能（阻塞）

```rust
impl Drop for NatsServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(e) = child.kill() { /* ... */ }
            if let Err(e) = child.wait() { /* ... */ }  // 同步等待
        }
    }
}
```

`child.wait()` 是同步阻塞调用。如果 tokio runtime 正在关闭时触发此 Drop，可能在 runtime 的 blocking 线程池之外的线程上执行，导致短暂阻塞。但考虑到这是清理代码且 `nats-server` 响应 kill 信号很快，实际影响可忽略。

---

### 🟢-3: `validate_cwd` 的符号链接竞态

**文件**: `crates/ergatai-api/src/main.rs:258-278`
**类别**: 安全（TOCTOU）

```rust
fn validate_cwd(cwd: &str) -> Result<PathBuf, String> {
    // 拒绝 ..
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(...);
        }
    }
    let canonical = std::fs::canonicalize(path)?;
    // ...
}
```

`canonicalize` 解析符号链接，但在 `canonicalize` 返回后、实际使用路径前，攻击者可能替换符号链接目标。这是经典的 TOCTOU，但在 CLI 工具中风险很低。

---

### 🟢-4: 认证使用非常量时间比较

**文件**: `crates/ergatai-api/src/main.rs:236-238`
**类别**: 安全（时序攻击）

```rust
let is_valid = auth_header
    .and_then(|h| h.strip_prefix("Bearer "))
    .map(|token| token == expected_token.as_str())  // 非常量时间比较
    .unwrap_or(false);
```

`==` 在第一个不匹配字节处返回，理论上允许时序攻击推断 token 值。对于本地服务，实际风险极低。

**修复建议**: 使用 `subtle::ConstantTimeEq` 进行恒定时间比较。

---

### 🟢-5: `is_server_running` 的 TOCTOU 竞态

**文件**: `crates/ergatai-acp/src/mcp.rs:259-302`
**类别**: 正确性

```rust
fn is_server_running(name: &str) -> bool {
    let mut procs = mcp_registry().processes.lock().unwrap();
    let child_arc = procs.get(name).cloned()?;
    drop(procs);  // 释放锁

    let mut child = child_arc.try_lock()?;  // 然后加锁 child
    // 在两次锁定之间，另一个线程可能已经 stop 了这个 server
}
```

`start_mcp_server` 中的 check-then-act 也有类似竞态：两个并发 `start` 可能都看到 "not running" 然后都启动。代码通过 `procs.insert()` 的覆盖语义处理了这一点（后启动的覆盖先启动的，先启动的进程泄漏）。

---

## 预先存在的测试编译错误（不在审查范围）

`cargo test --no-run` 产生 2 个编译错误（非用户提到的 25 个）：

| 错误 | 位置 | 原因 |
|------|------|------|
| `E0433: cannot find ergatai_collab` | `ergatai-collab/src/agent_launcher.rs:573` | 测试代码引用了不存在的模块路径 |
| `E0432: unresolved import tempfile` | `ergatai-lock/src/audit.rs:659` | `tempfile` crate 未在 `[dev-dependencies]` 中声明 |

---

## 亮点：良好的安全实践

| 模式 | 位置 | 说明 |
|------|------|------|
| **RAII TransactionGuard** | `ergatai-lock/src/lock_manager.rs:35-65` | 事务自动回滚，防止 panic 导致的锁泄漏 |
| **CompletionGuard** | `ergatai-acp/src/sdk_pool_manager.rs:338-347` | Drop-based 完成信号，防止 pool agent 永久 busy |
| **WaiterCleanup** | `ergatai-acp/src/sdk_session.rs:213-221` | RAII 清理审批等待者，防止超时/错误时泄漏 |
| **kill_on_drop(true)** | `ergatai-acp/src/mcp.rs:335` | 确保 MCP 子进程不会泄漏 |
| **NatsServer Drop** | `ergatai-nats/src/server.rs:198-211` | kill + wait 正确清理子进程 |
| **路径遍历防护** | `ergatai-api/src/main.rs:262-265` | 拒绝 `..` 组件 |
| **配置权限保护** | 安全修复（2026-08-13） | 配置文件 0o600 权限 |
| **spawn_blocking** | `ergatai-acp/src/sdk_session.rs:844` | 正确地将阻塞 I/O 移出 async 上下文 |
| **原子操作** | 多处 `AtomicU64/AtomicUsize` | 无锁计数器，避免不必要的 Mutex |

---

## 总结

| 严重性 | 数量 | 关键问题 |
|--------|------|----------|
| 🔴 高危 | 3 | unsafe set_var + 命令注入面 + process::exit 跳过清理 |
| 🟡 中等 | 7 | 权限自动批准 + unwrap panic + 全局单例 + Mutex 中毒 + example 安全 + 锁竞态 + 超时泄漏 |
| 🟢 低危 | 5 | TOCTOU + 时序攻击 + 阻塞 Drop + 计数竞态 |

**优先修复建议**:
1. 🔴-1 最紧急 — 实际 UB，一行代码即可修复
2. 🔴-2 需要在 MCP 功能启用前解决
3. 🔴-3 需要重新设计 shutdown 流程
