# 项目架构

## 基本信息
- **项目名称**：Ergatai (21st Agents)
- **技术栈**：TypeScript 5.4.5 + Rust, Electron 33.4.5, React 19, tRPC, Drizzle ORM, SQLite
- **架构模式**：分层架构 + 混合语言后端
- **创建时间**：2026-08-08
- **最后更新**：2026-08-08
- **审查者**：AI + Subagent
- **AI 评分**：B
- **Subagent 评分**：C（发现 AI 的重大误判）

## 架构概述

Ergatai 是一个 local-first 的 Electron 桌面应用，用于 AI 代码助手。采用分层架构，混合使用 TypeScript（业务逻辑）和 Rust（性能关键路径）。

## 架构层次

### 1. 渲染层（src/renderer/）
- **职责**：UI 组件、状态管理、用户交互
- **技术**：React 19, TypeScript, Jotai, Zustand, Tailwind CSS
- **包含**：
  - `components/` - 可复用 UI 组件
  - `features/` - 功能模块（agents, sidebar, settings, etc.）
  - `lib/` - 工具函数、状态管理、API 客户端
  - `contexts/` - React Context providers
- **依赖**：main（通过 tRPC）
- **被依赖**：无（顶层）

### 2. 主进程层（src/main/）
- **职责**：Electron 主进程、窗口管理、系统 API、tRPC 路由
- **技术**：TypeScript, Electron, tRPC, Drizzle ORM
- **包含**：
  - `index.ts` - 应用入口（1030 行，需要拆分）
  - `windows/` - 窗口管理
  - `lib/trpc/routers/` - tRPC 路由（`chats.ts` 2196 行，需要拆分）
  - `lib/db/` - 数据库层
  - `auth-manager.ts`, `auth-store.ts` - 认证管理
- **依赖**：native-binding（通过 NAPI）
- **被依赖**：renderer（通过 tRPC）

### 3. 预加载层（src/preload/）
- **职责**：IPC 桥接、上下文隔离
- **技术**：TypeScript
- **包含**：
  - `index.ts` - 暴露 desktopApi + tRPC bridge
- **依赖**：无（独立）
- **被依赖**：renderer

### 4. 原生绑定层（src/native-binding.*）
- **职责**：NAPI 接口、Rust 调用桥接
- **技术**：TypeScript 声明 + JavaScript 胶水代码
- **包含**：
  - `native-binding.d.ts` - TypeScript 类型声明
  - `native-binding.js` - JavaScript 加载器
- **依赖**：Rust 核心层
- **被依赖**：main

### 5. Rust 核心层（src-rust/src/）
- **职责**：核心业务逻辑、性能关键路径
- **技术**：Rust, NAPI-RS
- **包含**：
  - `acp/` - Agent Client Protocol 管理
  - `agent/` - Agent 配置、发现、运行时（1784 行，与 cross_agent 有职责重叠）
  - `cross_agent/` - 跨 agent 通信、DAG 调度、任务协调（2500+ 行，需要拆分）
  - `orchestration/` - 编排逻辑、markdown/DAG/tree 解析（1400+ 行，需要拆分）
  - `napi/` - NAPI 胶水层（600 行，纯桥接）
  - `error/` - 错误处理
  - `skills.rs`, `mcp.rs` - 技能和 MCP 集成
- **依赖**：无（底层）
- **被依赖**：native-binding（通过 NAPI）

### 6. 共享层（src/shared/）
- **职责**：跨层共享类型和工具
- **技术**：TypeScript
- **包含**：
  - `changes-types.ts` - 变更类型定义
  - `detect-language.ts` - 语言检测
  - `codex-tool-normalizer.ts` - Codex 工具规范化
  - `external-apps.ts` - 外部应用定义
- **依赖**：无（独立）
- **被依赖**：renderer, main

### 7. 文档/SDK（docs/acp-sdk/）
- **职责**：ACP SDK 参考实现和文档
- **状态**：⚠️ 独立 git 仓库，不在主项目 workspace 中
- **技术**：Rust workspace
- **包含**：示例、文档、测试
- **注意**：应从主索引中排除

## 依赖规则

### 允许的依赖方向
- renderer → main（通过 tRPC）✅
- main → native-binding（通过 NAPI）✅
- native-binding → Rust 核心层 ✅
- renderer → shared ✅
- main → shared ✅
- preload → 无 ✅

### 禁止的依赖方向
- renderer → native-binding ❌（必须通过 main）
- renderer → Rust 核心层 ❌（必须通过 main）
- main → renderer ❌（通过 tRPC 事件/订阅）
- Rust 核心层 → main ❌（通过 NAPI 返回值/回调）
- shared → renderer/main ❌（共享层应无依赖）

## 命名规范
- **文件命名**：
  - 组件：PascalCase（`ChatView.tsx`, `AgentsSidebar.tsx`）
  - 工具/钩子：camelCase（`use-file-upload.ts`, `formatters.ts`）
  - 存储：kebab-case（`sub-chat-store.ts`, `agent-chat-store.ts`）
  - Rust 模块：snake_case（`task_coordinator.rs`, `agent_launcher.rs`）
- **类/函数命名**：
  - TypeScript：camelCase（函数）、PascalCase（类/组件）
  - Rust：snake_case（函数/方法）、PascalCase（结构体/枚举）

## 数据流

```
用户交互
    ↓
renderer (React)
    ↓ tRPC 调用
main (tRPC routers)
    ↓ NAPI 调用
native-binding
    ↓ FFI
Rust 核心层
    ↓ 返回值
native-binding → main → renderer
```

## 已知问题

### ~~高优先级（P0）~~ ✅ 已完成
1. ~~**CodeGraph 索引污染**：`docs/acp-sdk/`、`network-demo/`、`.deprecated-engines/`、`out/` 被错误索引~~
   - ~~**修复**：重新索引时排除这些目录~~
   - **已修复**：通过手动临时移动目录 + 重新索引完成（2026-08-08）
   - **结果**：节点数减少 37%，边数减少 54%，虚假依赖消失

### 中优先级（P1）
2. **Deprecated 代码残留**：`.deprecated-engines/claude-lib/` 仍被识别为 entry_points，污染打包
   - **修复**：删除目录，grep 验证无活引用

3. **God file 需要拆分**：
   - `src/main/lib/trpc/routers/chats.ts`（2196 行）→ 按子领域拆分
   - `src-rust/src/cross_agent/task_coordinator.rs`（811 行）→ 按职责拆分
   - `src-rust/src/cross_agent/agent_launcher.rs`（816 行）→ 按职责拆分
   - `src/main/index.ts`（1030 行）→ 拆分窗口管理、IPC 注册、app 生命周期

### 低优先级（P2）
4. **docs/acp-sdk/ git 状态异常**：独立 .git 但不在 .gitmodules，是 git 怪胎
   - **修复**：要么 `git submodule add`，要么移到独立 repo

5. **agent 模块职责重叠**：`src-rust/src/agent/` 与 `cross_agent/agent_launcher.rs` 都在做"启动 agent"
   - **修复**：grep 评估重叠范围，决定合并方向

6. **测试覆盖不足**：Rust 有测试，TS 侧几乎无测试
   - **修复**：补充测试策略评估

## 架构决策记录

### 2026-08-08：初始架构分析
- **决策**：采纳 Subagent 审查结果，撤销 AI 的"反向依赖"和"src 包混乱"判断
- **原因**：AI 的依赖分析基于被 CodeGraph 误索引的数据，实际代码中不存在这些问题
- **影响**：重新排序优先级，P0 改为修正索引，P1 改为删除 deprecated 和拆分 god file
