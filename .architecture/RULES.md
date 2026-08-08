# 架构规则

## 核心原则

### 1. 分层架构原则
- **上层可以依赖下层，下层不能依赖上层**
- renderer → main → native-binding → Rust 核心层
- shared 层独立，被上层共享

### 2. 混合语言原则
- **TypeScript**：业务逻辑、UI、系统 API 调用
- **Rust**：性能关键路径、核心算法、并发密集任务
- **NAPI 是纯胶水**：不包含业务逻辑，只做类型转换和调用转发

### 3. 本地优先原则
- 所有数据存储在本地 SQLite
- 不依赖远程服务（除 LLM API 调用）
- 支持离线工作

### 4. 类型安全原则
- 端到端使用 TypeScript 类型系统
- tRPC 提供 RPC 调用的类型安全
- NAPI 层提供 Rust ↔ TypeScript 类型桥接

## 具体规则

### 文件组织

#### ✅ 允许
- 组件文件使用 PascalCase：`ChatView.tsx`, `AgentsSidebar.tsx`
- 工具文件使用 camelCase：`use-file-upload.ts`, `formatters.ts`
- 存储文件使用 kebab-case：`sub-chat-store.ts`, `agent-chat-store.ts`
- Rust 模块使用 snake_case：`task_coordinator.rs`, `agent_launcher.rs`

#### ❌ 禁止
- 混用命名规范（如 `TaskCoordinator.rs`）
- 在组件目录放工具函数
- 在工具目录放 React 组件

### 依赖方向

#### ✅ 允许的依赖
```
renderer → main（通过 tRPC）✅
renderer → shared ✅
main → native-binding（通过 NAPI）✅
main → shared ✅
native-binding → Rust 核心层 ✅
shared → 无 ✅
```

#### ❌ 禁止的依赖
```
renderer → native-binding ❌（必须通过 main）
renderer → Rust 核心层 ❌（必须通过 main）
main → renderer ❌（通过 tRPC 事件/订阅）
Rust 核心层 → main ❌（通过 NAPI 返回值/回调）
shared → renderer/main ❌（共享层应无依赖）
```

### tRPC 使用规则

#### ✅ 正确用法
```typescript
// renderer 端
const { data } = trpc.chats.list.useQuery()

// main 端
export const chatsRouter = router({
  list: publicProcedure.query(async () => {
    // 业务逻辑
  })
})
```

#### ❌ 禁止用法
```typescript
// ❌ main 层直接调用 renderer
import { ChatView } from '../../renderer/components/ChatView'

// ❌ renderer 直接调用 native-binding
import { ... } from '../../native-binding'

// ❌ 在 router 中写业务逻辑（应该提取到 service 层）
export const chatsRouter = router({
  create: publicProcedure.mutation(async ({ input }) => {
    // 100 行业务逻辑代码 ❌
  })
})
```

### Rust 模块规则

#### ✅ 正确组织
```
src-rust/src/
├── napi/          # NAPI 胶水层（纯桥接，无业务逻辑）
├── acp/           # Agent Client Protocol
├── agent/         # Agent 配置和发现
├── cross_agent/   # 跨 agent 通信
├── orchestration/ # 编排逻辑
└── error/         # 错误处理
```

#### ❌ 禁止
```rust
// ❌ 在 napi/ 中写业务逻辑
#[napi]
pub fn create_agent(config: String) -> Result<String> {
    // 100 行业务逻辑 ❌
}

// ❌ 在 acp/ 中调用 agent/ 的内部函数
use crate::agent::internal::some_function;
```

### 代码大小规则

#### ⚠️ 警告阈值
- **TypeScript 文件**：> 1000 行需要审查
- **Rust 文件**：> 500 行需要审查
- **tRPC router**：> 500 行需要拆分

#### ❌ 禁止
- **TypeScript 文件**：> 2000 行（必须拆分）
- **Rust 文件**：> 1000 行（必须拆分）
- **tRPC router**：> 1000 行（必须拆分）

### 测试规则

#### ✅ 必须测试
- Rust 核心逻辑
- 复杂的 TypeScript 业务逻辑
- 错误处理路径

#### ⚠️ 建议测试
- UI 组件（至少 smoke test）
- tRPC router
- 工具函数

#### ❌ 不测试
- 纯 UI 样式
- 配置常量
- NAPI 胶水层

## 代码审查检查清单

### 新代码提交前
- [ ] 文件位置是否正确（组件在 components/，工具在 lib/）？
- [ ] 命名规范是否符合（PascalCase/camelCase/kebab-case）？
- [ ] 依赖方向是否符合规则（上层 → 下层）？
- [ ] TypeScript 文件是否 < 1000 行？
- [ ] Rust 文件是否 < 500 行？
- [ ] tRPC router 是否 < 500 行？
- [ ] 是否有相应的测试？

### 架构变更时
- [ ] 是否更新了 ARCHITECTURE.md？
- [ ] 是否更新了 DEPENDENCIES.md？
- [ ] 是否记录了 CHANGELOG.md？
- [ ] 是否经过 Subagent 审查？

## 违规处理

### 轻微违规（警告）
- 命名规范不符合
- 文件略超阈值（1000-1200 行）
- 缺少测试

**处理**：代码审查时指出，建议修复

### 严重违规（错误）
- 违反依赖方向（如 renderer → native-binding）
- 文件严重超标（> 2000 行）
- 在 NAPI 层写业务逻辑

**处理**：必须修复后才能合并

## 架构决策流程

### 重大变更
1. 提出变更请求
2. AI 分析影响
3. Subagent 审查
4. 用户确认
5. 执行变更
6. 更新架构文档

### 小变更
1. 开发者评估
2. 代码审查
3. 合并

## 工具配置

### CodeGraph 索引
必须排除以下目录：
- `docs/acp-sdk/`（独立仓库）
- `network-demo/`（孤立演示）
- `.deprecated-engines/`（废弃代码）
- `out/`（构建产物）
- `node_modules/`（依赖）
- `target/`（Rust 构建产物）
