# RustSafe 审查报告 — Ergatai 工作区

**审查范围**: `/home/yubing/code/ergatai/crates/` 全部 Rust 源码（62 文件，~28,000 行）
**审查日期**: 2026-08-14
**审查方法**: 5 个并行专业审查 agent + 交叉验证

---

## 概要

| 指标 | 数值 |
|------|------|
| 审查文件 | 62 个 Rust 源文件 |
| 代码行数 | ~28,000 行 |
| 🔴 高危 | **5 个**（去重后） |
| 🟡 中等 | **~15 个** |
| 🟢 低危 | **~20 个** |

---

## 编译基线

### cargo check
- 状态：❌ 失败
- 错误数量：25 个
- 警告数量：23 个

### cargo test --no-run
- 状态：❌ 失败（编译错误阻止）

### cargo clippy
- 状态：❌ 失败（编译错误阻止）

---

## 预先存在的编译错误

| 文件 | 行号 | 错误类型 | 摘要 | 根因 |
|------|------|---------|------|------|
| `ergatai-lock/src/audit.rs` | 659 | E0432 | `use tempfile::TempDir` 未解析 | 缺少 dev-dependency |
| `ergatai-lock/src/config.rs` | 305 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/lock_manager.rs` | 2954 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/lock_mode.rs` | 261 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/manager.rs` | 242 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/renewal.rs` | 307 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/snapshot.rs` | 413 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/watchdog.rs` | 566 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/watcher.rs` | 211 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/multi_agent_tests.rs` | 17 | E0432 | 同上 | 同上 |
| `ergatai-lock/src/lock_manager.rs` | 3107+ | E0433 | `tempfile::TempDir::new()` 未解析 | 同上（6 处） |
| `ergatai-collab/src/agent_launcher.rs` | 573 | E0433 | `use ergatai_collab::task_coordinator::TaskType` | 应为 `use crate::task_coordinator::TaskType` |
| `ergatai-collab/src/agent_launcher.rs` | 630+ | E0433 | `tempfile::tempdir()` 未解析 | 缺少 dev-dependency（6 处） |
| `ergatai-collab/src/dag_scheduler.rs` | 761+ | E0433 | 同上 | 同上（2 处） |

**根因分析**:
1. `ergatai-lock/Cargo.toml` 和 `ergatai-collab/Cargo.toml` 缺少 `[dev-dependencies] tempfile = "3.10"`
2. `ergatai-collab/src/agent_launcher.rs:573` 在 crate 内部测试中错误使用 `use ergatai_collab::...`（应为 `use crate::...`）

---

## 🔴 高危问题（5 个）

### H1. `unsafe { std::env::set_var() }` 在 tokio 运行时内 — 数据竞争

- **位置**: `crates/ergatai-api/src/main.rs:95`
- **类别**: 并发安全 / unsafe
- **来源**: 本次发现（3 个 agent 同时独立发现）
- **发现 agent**: 高级特性 + 所有权 + 并发
- **描述**:
  ```rust
  #[tokio::main]
  async fn main() -> Result<()> {
      let args = Args::parse();
      if args.verbose {
          // Safety: set_var is called before any threads are spawned  ← 注释不准确！
          unsafe { std::env::set_var("RUST_LOG", "debug") };
      }
  ```
  `#[tokio::main]` 宏在进入 `main` 函数体之前就已经启动了 tokio 多线程 runtime。此时 `std::env::set_var` 与 `libc::setenv` 一样不是线程安全的。Rust 1.83+ 将 `set_var` 标记为 `unsafe` 正是因为这一 UB。注释声称"在线程创建之前"是不准确的。
- **影响**: 潜在数据竞争（未定义行为）。虽然实际风险较低（tokio worker 线程不读取 RUST_LOG），但这是错误的模式。
- **修复建议**:
  ```rust
  // 方案 A：在 tokio::main 之前设置
  fn main() -> Result<()> {
      let args = Args::parse();
      if args.verbose {
          std::env::set_var("RUST_LOG", "debug"); // 安全：runtime 未启动
      }
      // 然后用 tokio::runtime::Runtime::new().block_on(async_main(args))
  }

  // 方案 B：使用 tracing EnvFilter 直接设 level
  if args.verbose {
      tracing_subscriber::fmt().with_env_filter("debug").init();
  }
  ```

---

### H2. `sdk_pool_manager.rs` 热路径中过量 `.clone()` + `serde_json::json!()` 堆分配

- **位置**: `crates/ergatai-acp/src/sdk_pool_manager.rs:428-453`
- **类别**: 性能
- **来源**: 本次发现
- **发现 agent**: 性能
- **描述**:
  在 `pool_event_loop` 的热路径（每个任务都经过）中，同一 `task_id` 被重复 clone 多达 4 次，且每个任务的生命周期都调用 `serde_json::json!()` 产生临时 JSON 树：
  ```rust
  let task = PendingTask {
      task_id: msg.payload.task_id.clone(),       // clone 1
      prompt: msg.payload.prompt.clone(),         // clone 2
  };
  let task_id = task.task_id.clone();             // clone 3
  let task_id_clone = task_id.clone();            // clone 4
  // + serde_json::json!() 2-3 次 per task
  ```
- **影响**: 每个任务分发产生 5+ 次堆分配。在高吞吐量场景下可能成为瓶颈。
- **修复建议**: 使用 move 语义，减少 clone 次数；用结构化类型替代 `json!()` 宏。

---

### H3. `dag_scheduler.rs` 嵌套锁顺序风险

- **位置**: `crates/ergatai-collab/src/dag_scheduler.rs:359-360`
- **类别**: 并发安全
- **来源**: 本次发现
- **发现 agent**: 并发
- **描述**:
  ```rust
  async fn build_upstream_context_block(&self, node: &TaskNode) -> String {
      let graph = self.graph.lock().await;   // 锁 A
      let ctx = self.context.lock().await;   // 锁 B（持有 A 的情况下获取 B）
  ```
  当前全局锁顺序一致（graph → context），但没有任何文档说明。未来代码如果以相反顺序获取，会产生死锁。
- **影响**: 潜在死锁风险（当前未触发，但维护风险高）
- **修复建议**:
  ```rust
  // 方案 1: 添加文档注释
  /// # Lock Ordering Invariant
  /// Always acquire `graph` before `context`. Never reverse this order.

  // 方案 2: 重构为快照模式
  let (graph_snapshot, ctx_snapshot) = {
      let graph = self.graph.lock().await;
      let ctx = self.context.lock().await;
      (graph.clone_some_data(), ctx.clone_some_data())
  }; // 两把锁都在 await 之前释放
  ```

---

### H4. MCP 服务器中 3 处 `serde_json::to_value(response).unwrap()`

- **位置**: `crates/ergatai-api/src/mcp/server.rs:137, 168, 188`
- **类别**: 错误处理
- **来源**: 本次发现
- **发现 agent**: 错误处理
- **描述**: 在生产 HTTP 服务器的 MCP JSON-RPC 响应序列化中使用 `unwrap()`。如果序列化失败（虽然概率很低），整个服务器 panic。
- **影响**: 服务器 panic 导致所有连接的 agent 断开
- **修复建议**:
  ```rust
  match serde_json::to_value(response) {
      Ok(value) => JsonRpcResponse { result: Some(value), .. },
      Err(e) => JsonRpcResponse {
          error: Some(JsonRpcError { code: -32603, message: format!("Internal error: {}", e), .. }),
          ..
      },
  }
  ```

---

### H5. DAG 调度器忽略状态更新错误 — 可能导致任务重复执行

- **位置**: `crates/ergatai-collab/src/dag_scheduler.rs:103, 550, 555, 566`
- **类别**: 错误处理
- **来源**: 本次发现
- **发现 agent**: 错误处理
- **描述**:
  ```rust
  let _ = graph.update_status(&node.id, TaskStatus::Pending);  // 行 103
  let _ = graph.update_status(node_id, TaskStatus::Running);   // 行 550
  let _ = graph.update_status(node_id, TaskStatus::Failed);    // 行 555, 566
  ```
  `update_status()` 返回 `ErgataiResult<()>`，当节点 ID 不存在时会失败。忽略这些错误会导致 DAG 状态图与实际执行不一致。
- **影响**: 任务可能重复执行、失败状态未记录、下游任务可能在不正确的状态下继续
- **修复建议**: 至少记录 warning 日志；关键状态转换应传播错误

---

## 🟡 中等问题（~15 个）

### M1. 库代码使用 `anyhow::Result` 而非具体错误类型

- **位置**: `ergatai-acp/src/http_client.rs`, `ergatai-api/src/mcp/tools.rs`, `ergatai-api/src/mcp/message_relay.rs`
- **发现 agent**: 错误处理
- **描述**: `http_client.rs` 是 `ergatai-acp` crate 的公共 API，使用 `anyhow::Result` 让调用者无法进行精确错误匹配
- **修复建议**: 改为 `ErgataiResult<T>`

### M2. 缺少 `From` trait 实现 — 142 处手动 `map_err`

- **位置**: 全工作区
- **发现 agent**: 错误处理
- **描述**: 缺少 `From<async_nats::Error>`、`From<SendError<T>>`、`From<RecvError>`、`From<Elapsed>` 等实现，导致 142 处 `map_err(|e| ErgataiError::internal(format!("...: {}", e)))` 手动转换，丢失原始错误类型
- **修复建议**: 添加缺失的 `From` 实现

### M3. `init_nats()` 持有 RwLock 写锁跨越网络 I/O

- **位置**: `crates/ergatai-nats/src/manager.rs:34-63`
- **发现 agent**: 并发
- **描述**: 获取全局 `NATS_STATE` 写锁后，执行子进程启动、TCP 连接、JetStream 创建等耗时异步操作。所有读操作被阻塞
- **修复建议**: 在锁外执行耗时操作，然后二次检查后写入

### M4. `audit.rs` 持有 Mutex 期间执行文件 I/O

- **位置**: `crates/ergatai-lock/src/audit.rs:547-599`
- **发现 agent**: 并发
- **描述**: `archive_old_audit_logs` 在持有 `self.conn` Mutex 期间执行 `serde_json::to_string_pretty` + `std::fs::write`
- **修复建议**: 在锁外执行文件写入

### M5. `validate_and_normalize_path` 在 async 函数中执行阻塞 I/O

- **位置**: `crates/ergatai-lock/src/lock_manager.rs:1102-1130`
- **发现 agent**: 并发
- **描述**: `acquire_lock` 是 `async fn`，调用 `Path::canonicalize()` 执行文件系统 I/O，阻塞 tokio worker 线程
- **修复建议**: 使用 `tokio::task::spawn_blocking`

### M6. `std::fs::create_dir_all` 在 async 函数中

- **位置**: `crates/ergatai-nats/src/server.rs:49`
- **发现 agent**: 并发
- **修复建议**: 改为 `tokio::fs::create_dir_all`

### M7. 函数参数取 `String` 导致调用方被迫 `.clone()`

- **位置**: `sdk_pool_manager.rs:125,205,222,242`、`http_client.rs:299`、`mcp.rs:308,182` 等
- **发现 agent**: 性能
- **修复建议**: 使用 `impl Into<String>` 或 `&str`

### M8. `Vec::new()` 后跟 `push` 循环无预分配 — 多处

- **位置**: `task_coordinator.rs:349`、`agent_launcher.rs:103,154,383,502`、`dag_scheduler.rs:92,362,431,584` 等
- **发现 agent**: 性能
- **修复建议**: 使用 `Vec::with_capacity(n)`

### M9. `.collect::<Vec<_>>().join()` 不必要的中间分配

- **位置**: `task_coordinator.rs:362-390`（3 处相同模式）
- **发现 agent**: 性能
- **修复建议**: 直接写入 `String`

### M10. 双重迭代计算 idle/busy 计数

- **位置**: `sdk_pool_manager.rs:584-585`
- **发现 agent**: 性能
- **修复建议**: 使用 `fold` 一次遍历

### M11. `expect()` 在 SIGTERM handler 安装

- **位置**: `crates/ergatai-core/src/signal.rs:93`
- **发现 agent**: 错误处理
- **描述**: 容器环境中 signal handler 安装可能失败
- **修复建议**: 改为 graceful fallback

### M12. 15+ 个 `OnceLock` 全局静态 — 测试隔离性差

- **位置**: 全工作区
- **发现 agent**: 所有权
- **描述**: CLAUDE.md 中已承认此问题（"Some tests may fail intermittently when run together"）
- **修复建议**: 长期重构为依赖注入

### M13. `TransactionGuard` Drop 静默忽略 ROLLBACK 错误

- **位置**: `crates/ergatai-lock/src/lock_manager.rs:58-65`
- **发现 agent**: 所有权
- **修复建议**: 至少记录 warning 日志（已有注释说明）

### M14. 错误信息泄露 — MCP 响应中返回内部错误详情

- **位置**: `ergatai-api/src/mcp/server.rs:149`
- **发现 agent**: 错误处理
- **修复建议**: 对外部客户端返回通用错误消息

### M15. `std::sync::Mutex` 在异步上下文中使用

- **位置**: `sdk_session.rs`、`manager.rs`、`session_ops.rs` 等多处
- **发现 agent**: 并发
- **描述**: 临界区极短，项目已有文档说明。低风险但需监控
- **修复建议**: 如竞争加剧，改为 `tokio::sync::Mutex` 或 `dashmap`

---

## 🟢 低危问题（~20 个）

| # | 位置 | 描述 | 发现 agent |
|---|------|------|-----------|
| L1 | 多处 | `to_string_lossy().to_string()` 双分配模式 | 性能 |
| L2 | 多处 | `HashMap::new()` 无预分配 | 性能 |
| L3 | 多处 | `format!()` 在正常路径的 tracing 日志中 | 性能 |
| L4 | `mcp.rs:104` | `HashMap<String, bool>` 可用 `HashSet` | 性能 |
| L5 | `sdk_pool_manager.rs:403` | `cancelled_tasks: HashSet<String>` 可用 `Arc<str>` | 性能 |
| L6 | 多处 | 事件类型字符串反复 `.to_string()` | 性能 |
| L7 | `sdk_pool_manager.rs:434` | `prompt_preview` 每次 dispatch 分配 String | 性能 |
| L8 | `mcp.rs:61-83` | `find_mcp_configs` 可用 `Vec::with_capacity(3)` | 性能 |
| L9 | `template.rs:70` | `var_name.clone()` 可能可避免 | 性能 |
| L10 | `sdk_session.rs:103` | `pending_perms` 可用 `Option` 延迟分配 | 性能 |
| L11 | `dag_scheduler.rs:362-390` | `build_upstream_context_block` 可用 `String::with_capacity` | 性能 |
| L12 | 多处 | 安全的 `expect()` 调用（3 处） | 错误处理 |
| L13 | `types.rs:8` | `BoxError` 类型别名（合理使用） | 错误处理 |
| L14 | 多处 | 可接受的 `let _ =` 忽略（关闭信号、监控指标等） | 错误处理 |
| L15 | `config.rs:169,191` | Mutex poison 正确处理（正面案例） | 错误处理 |
| L16 | `sdk_session.rs:42` | `AtomicU64` 使用正确 | 并发 |
| L17 | 多处 | OnceLock / LazyLock 使用正确 | 并发 |
| L18 | `lock_manager.rs:97-106` | M13 安全约定文档良好 | 并发 |
| L19 | `mcp.rs:241` | 双层锁模式正确 | 并发 |
| L20 | 多处 | Channel RAII 清理正确 | 并发 |

---

## ✅ 确认不存在的风险

以下高风险模式在整个 `crates/` 目录中**完全不存在**：

- ❌ `static mut` 声明
- ❌ 裸指针 (`*const T` / `*mut T`)
- ❌ `Rc<T>`（无循环引用风险）
- ❌ `RefCell<T>` 在多线程环境
- ❌ `unsafe impl Send` / `unsafe impl Sync`
- ❌ `transmute` / `transmute_copy`
- ❌ `MaybeUninit` / `mem::zeroed()` / `mem::uninitialized()`
- ❌ `macro_rules!` 宏
- ❌ `#[proc_macro]`
- ❌ `extern "C"` / FFI 边界
- ❌ 自定义 `Deref` / `DerefMut`
- ❌ `_unchecked` 系列方法
- ❌ blanket impl
- ❌ `Pin::new_unchecked`
- ❌ 裸 `Result<T>` 缺少错误类型

---

## 安全姿态评价

Ergatai 代码库的安全姿态**整体优秀**：

1. **零 unsafe 代码（修复 H1 后）** — ~28,000 行 Rust，仅 1 处不必要的 unsafe 块
2. **零裸指针、零 FFI、零宏** — 完全避免常见的高风险来源
3. **生产代码仅 3 个 unwrap()** — 其余 450+ 个全在测试代码中
4. **零 panic!() 在生产代码中**
5. **错误类型使用 thiserror 规范定义**
6. **锁使用有良好的文档说明**（lock_manager.rs M13 安全约定）
7. **Channel RAII 清理正确，无泄漏风险**
8. **Atomic 使用正确，Ordering 选择合理**

---

## 审查来源汇总

| 发现 | 高级特性 | 性能 | 所有权 | 并发 | 错误处理 |
|------|---------|------|--------|------|---------|
| H1 unsafe set_var | ✅ | | ✅ | ✅ | |
| H2 sdk_pool clone | | ✅ | | | |
| H3 嵌套锁顺序 | | | | ✅ | |
| H4 MCP unwrap | | | | | ✅ |
| H5 DAG let _ = | | | | | ✅ |
| M1 anyhow 滥用 | | | | | ✅ |
| M2 缺 From trait | | | | | ✅ |
| M3 NATS init 持锁 | | | | ✅ | |
| M4 audit 持锁 I/O | | | | ✅ | |
| M5 canonicalize 阻塞 | | | | ✅ | |
