# 依赖关系图

## 跨包依赖（实际验证）

### renderer 层
```
renderer → main（通过 tRPC）
  - src/renderer/lib/trpc.ts → import type { AppRouter } from '../../main/lib/trpc'
  - 唯一依赖点，type-only import，符合 tRPC 规范
```

### main 层
```
main → native-binding（通过 NAPI）
  - src/main/lib/trpc/routers/* → import { ... } from '../../../native-binding'
  - 调用 Rust 核心层功能
```

### native-binding 层
```
native-binding → Rust 核心层（通过 FFI）
  - src/native-binding.js → 加载编译后的 Rust 二进制
  - src/native-binding.d.ts → TypeScript 类型声明
```

### shared 层
```
shared → 无依赖（独立）
  - 被 renderer 和 main 引用
  - 提供共享类型和工具函数
```

## Rust 内部依赖

### src-rust/src/ 模块依赖
```
lib.rs（入口）
  ├─→ napi/（NAPI 胶水层，600 行）
  │    ├─→ acp/
  │    ├─→ agent/
  │    ├─→ cross_agent/
  │    └─→ orchestration/
  │
  ├─→ acp/（Agent Client Protocol）
  │    ├─→ manager.rs
  │    ├─→ sdk_pool_manager.rs
  │    ├─→ sdk_session.rs
  │    ├─→ session_ops.rs
  │    └─→ persistence.rs
  │
  ├─→ agent/（Agent 配置和发现，1784 行）
  │    ├─→ config.rs
  │    ├─→ discovery.rs
  │    ├─→ custom_harness.rs
  │    ├─→ runtime_metadata.rs
  │    └─→ global_config.rs
  │    ⚠️ 与 cross_agent/agent_launcher.rs 职责重叠
  │
  ├─→ cross_agent/（跨 agent 通信，2500+ 行，需要拆分）
  │    ├─→ acp_bridge.rs
  │    ├─→ agent_launcher.rs（816 行，god file）
  │    ├─→ dag_scheduler.rs
  │    ├─→ plan_watcher.rs
  │    ├─→ task_coordinator.rs（811 行，god file）
  │    └─→ task_scheduler.rs
  │
  ├─→ orchestration/（编排逻辑，1400+ 行，需要拆分）
  │    ├─→ dag_parser.rs
  │    ├─→ dag_topology.rs
  │    ├─→ markdown_parser.rs
  │    └─→ tree_topology.rs
  │
  ├─→ error/（错误处理）
  │    ├─→ classify.rs
  │    ├─→ macros.rs
  │    ├─→ mod.rs
  │    └─→ types.rs
  │
  ├─→ skills.rs
  └─→ mcp.rs
```

## 禁止的依赖（架构规则）

### ❌ 绝对禁止
1. **renderer → native-binding**：必须通过 main 进程的 tRPC 路由
2. **renderer → Rust 核心层**：必须通过 main 进程的 tRPC 路由
3. **main → renderer**：不允许直接调用，通过 tRPC 事件/订阅
4. **Rust 核心层 → main**：不允许直接调用，通过 NAPI 返回值/回调
5. **shared → renderer/main**：共享层必须无依赖

### ⚠️ 需要审查
1. **agent/ → cross_agent/agent_launcher.rs**：职责重叠，需要评估合并方向
2. **cross_agent/ → agent/**：可能存在不必要的依赖

## 循环依赖风险

### 当前状态
- ✅ **无循环依赖**：依赖方向单向，从 renderer → main → native-binding → Rust

### 潜在风险
1. **tRPC 事件订阅**：如果 main 层错误地 import renderer 组件，会形成循环
   - 验证：grep 确认 main 层无 `import.*from.*renderer`（除 type-only）
   
2. **Rust 回调**：如果 Rust 层尝试调用 TypeScript 函数，会形成循环
   - 验证：NAPI 层只提供返回值，不持有 TS 回调引用

## 外部依赖

### TypeScript 侧
- **Electron 33.4.5**：桌面应用框架
- **React 19**：UI 框架
- **tRPC**：类型安全 RPC
- **Drizzle ORM**：数据库 ORM
- **better-sqlite3**：SQLite 驱动
- **Jotai**：原子状态管理
- **Zustand**：状态管理
- **Tailwind CSS**：样式

### Rust 侧
- **NAPI-RS**：Node.js native 模块
- **tokio**：异步运行时
- **serde**：序列化
- **agent-client-protocol**：ACP SDK（docs/acp-sdk/，独立仓库）

## 依赖统计

| 依赖方向 | 调用次数 | 状态 |
|---------|---------|------|
| renderer → main | ~1（type-only） | ✅ 健康 |
| main → native-binding | 多处 | ✅ 健康 |
| native-binding → Rust | FFI | ✅ 健康 |
| shared → 无 | 0 | ✅ 健康 |
| agent/ ↔ cross_agent/ | 需评估 | ⚠️ 待审查 |

## 待修复的依赖问题

### P0：CodeGraph 索引污染
- **问题**：`docs/acp-sdk/`（2209 节点）被错误索引为项目模块
- **影响**：导致"acp-sdk → renderer 109 calls"等虚假依赖
- **修复**：重新索引时排除 `docs/acp-sdk/`、`network-demo/`、`.deprecated-engines/`、`out/`

### P2：docs/acp-sdk/ 的 git 状态
- **问题**：独立 .git 目录但不在 .gitmodules
- **影响**：团队成员 clone 后困惑
- **修复**：要么 `git submodule add`，要么移到独立 repo
