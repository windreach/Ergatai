# Agent UI 改造 — 任务依赖表

## 任务完成情况

> 更新规则：任务完成后将 `[ ]` 改为 `[x]`，并标注完成日期。

| # | 任务 | 状态 | 依赖 | 完成日期 |
|---|------|------|------|----------|
| 1 | Runtime 类型 & Atom | [x] 已完成 | — | 2026-08-08 |
| 2 | Agent Selector 组件 | [x] 已完成 | 1 | 2026-08-08 |
| 3 | 聊天输入区集成 | [ ] 待执行 | 1, 2 | — |
| 4 | New Chat Form | [ ] 待执行 | 1, 2 | — |
| 5 | Mode 按钮重命名 | [ ] 待执行 | — | — |
| 6 | My Agents 页面 | [ ] 待执行 | 1, 7 | — |
| 7 | 后端 installRuntime | [ ] 待执行 | — | — |
| 8 | Transport 统一 | [ ] 待执行 | 1 | — |
| 9 | Slash /model 命令 | [ ] 待执行 | — | — |
| 10 | 旧代码清理 | [ ] 待执行 | 1, 2, 3, 4, 8 | — |

**进度：** 2/10 完成

---

## Subagent 审查结果（2026-08-08）

**评分：B** — 整体扎实，3 个 P0 问题已修复。

### P0 问题 & 修复

| # | 问题 | Plan | 修复状态 |
|---|------|------|----------|
| 1 | `avatar_url: string \| null` 类型不匹配后端 | Plan 1 | ✅ 已改为 `string` |
| 2 | `sh -c` 执行任意命令有注入风险 | Plan 7 | ✅ 已加命令白名单 |
| 3 | Mode 改名无迁移策略 | Plan 5 | ✅ 已加 `"agent" → "auto"` 迁移 |

### 新增任务

| Plan | 新增内容 |
|------|----------|
| Plan 8 | 加 `runtimeId` 列到 `sub_chats` 表（DB migration） |
| Plan 10 | 加 `lastSelectedAgentIdAtom` → `lastSelectedRuntimeIdAtom` 数据迁移 |

---

## 依赖关系图

```
Plan 1 (Runtime 类型 & Atom)
  ├──→ Plan 2 (Agent Selector 组件)
  │      ├──→ Plan 3 (聊天输入区集成)
  │      └──→ Plan 4 (New Chat Form)
  │
  └──→ Plan 8 (Transport 统一)
         └──→ Plan 10 (旧代码清理)

Plan 5 (Mode 按钮重命名)          ← 无依赖，可独立执行

Plan 7 (后端 installRuntime)      ← 无依赖，可独立执行
  └──→ Plan 6 (My Agents 页面)

Plan 9 (Slash /model 命令)        ← 无依赖，可独立执行
```

## 详细依赖表

| Plan | 名称 | 依赖 | 可并行? | 阻塞谁 |
|------|------|------|---------|--------|
| **1** | Runtime 类型 & Atom | 无 | ✅ 最先执行 | 2, 8 |
| **2** | Agent Selector 组件 | 1 | ❌ 等 1 | 3, 4 |
| **3** | 聊天输入区集成 | 1, 2 | ❌ 等 1+2 | 10 |
| **4** | New Chat Form | 1, 2 | ❌ 等 1+2 | 10 |
| **5** | Mode 按钮重命名 | 无 | ✅ 随时可执行 | — |
| **6** | My Agents 页面 | 1, 7 | ❌ 等 1+7 | — |
| **7** | 后端 installRuntime | 无 | ✅ 随时可执行 | 6 |
| **8** | Transport 统一 | 1 |  等 1 | 10 |
| **9** | Slash /model 命令 | 无 | ✅ 随时可执行 | — |
| **10** | 旧代码清理 | 1, 2, 3, 4, 8 | ❌ 等所有 | — |

## 并行执行方案

### 第一波（无依赖，可同时启动）

```
Plan 1 ─┐
Plan 5 ─  可同时执行
Plan 7 ─
Plan 9 ─┘
```

**说明：** Plan 5/7/9 完全不依赖其他计划，可与 Plan 1 同时启动。但 Plan 1 是主线起点，建议优先完成 Plan 1 后再并行 5/7/9。

### 第二波（依赖 Plan 1）

```
Plan 2 ─┐
Plan 8 ─┘  等 Plan 1 完成后同时执行
```

### 第三波（依赖 Plan 1+2）

```
Plan 3 ─┐
Plan 4 ─┘  等 Plan 1+2 完成后同时执行
```

### 第四波（依赖 Plan 1+7）

```
Plan 6      等 Plan 1+7 完成后执行
```

### 第五波（收尾）

```
Plan 10     等 Plan 1+2+3+4+8 完成后执行
```

## 最快执行路径（关键路径）

```
Plan 1 → Plan 2 → Plan 3 → Plan 10
                    ↘ Plan 4 ↗
              ↘ Plan 8 ↗
```

**关键路径长度：** Plan 1 → 2 → 3 → 10（4 个 Plan 串行）

如果所有可并行的都并行：
- 第 1 轮：Plan 1（主线）+ Plan 5/7/9（并行）
- 第 2 轮：Plan 2 + Plan 8（并行）
- 第 3 轮：Plan 3 + Plan 4 + Plan 6（并行，等 Plan 7 也完成后 Plan 6 才解除阻塞）
- 第 4 轮：Plan 10

**最少 4 轮完成。**

## 单独可执行任务

以下 Plan 不依赖任何其他 Plan，可在任何时候单独执行：

| Plan | 名称 | 说明 |
|------|------|------|
| **5** | Mode 按钮重命名 | 纯 UI 文案改动，改 `atoms/index.ts` + `chat-input-area.tsx` 的 mode 字符串 |
| **7** | 后端 installRuntime | 纯后端改动（Rust + tRPC），不影响前端运行 |
| **9** | Slash /model 命令 | 补 translateEvent + 合并三层命令，不依赖 runtime atoms |

**推荐优先做 Plan 7**：后端改动可以提前编译验证，不阻塞前端开发。

## 风险提示

| 风险 | 影响 | 缓解 |
|------|------|------|
| Plan 1 的 atoms 设计与现有 `atoms/index.ts` 冲突 | 后续 Plan 2-4 编译失败 | Plan 1 完成后先跑 `tsc --noEmit` 验证 |
| Plan 8 删 `ACPChatTransport` 影响现有聊天 | 聊天功能断裂 | 先确认 `active-chat.tsx` 所有 codex 引用都替换后再删 |
| Plan 10 清理旧类型遗漏 | 运行时类型错误 | 用 `grep` 全局搜索 + `tsc --noEmit` 双重验证 |
| Plan 7 安装命令跨平台 | macOS/Linux 脚本不同 | `install_command` 先只支持 npm，后续扩展 |
