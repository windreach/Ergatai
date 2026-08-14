# Prompts Directory

Agent prompt templates for Ergatai's orchestration system.

## Architecture

### 主 Agent vs 子 Agent

| 角色 | 需要知道 | 不需要知道 |
|------|---------|-----------|
| **主 Agent** | ✅ 生成 DAG（编排）<br>✅ 与其他 Agent 通信<br>✅ 意图识别 | ❌ 如何执行具体任务 |
| **子 Agent** | ✅ 与其他 Agent 通信<br>✅ 执行任务并返回结果 | ❌ 如何生成 DAG |

## Files

### `main_agent.md` ⭐

**Purpose**: Injected into the **main agent** session (the one users directly interact with).

**When**: Automatically prepended to the first prompt in new sessions.

**Contains**:
- ✅ 意图识别：何时使用多 Agent（决策流程、关键词、示例）
- ✅ DAG 格式规范
- ✅ 可用 Agent 列表
- ✅ 通信方式（@mentions）
- ✅ 完整的示例
- ❌ 不包含任务执行细节

**Target**: 主 Agent（用户直接对话的 Agent）

---

### `sub_agent.md` ⭐

**Purpose**: Injected into **sub-agent** sessions (DAG task executors).

**When**: Automatically prepended when executing DAG tasks.

**Contains**:
- ✅ 角色定位（你是执行者，不是编排者）
- ✅ 通信方式（@mentions）
- ✅ 执行指南
- ✅ 返回结果格式
- ❌ 不包含 DAG 生成

**Target**: 子 Agent（DAG 任务执行者）

---

### `dag_generation.md` (Legacy)

**Purpose**: 详细的 DAG 生成指南（参考文档）

**Status**: 已被 `main_agent.md` 替代，但保留作为参考

---

### `dag_orchestration.md` (Legacy)

**Purpose**: DAG 执行指南

**Status**: 已被 `sub_agent.md` 替代，但保留作为参考

---

### `base.md`

**Purpose**: Base instructions for all agents.

**When**: Always loaded first.

**Contains**: Fundamental instructions common to all agents.

**Target**: All agents

## Usage

### Main Agent (TypeScript side)

File: `src/main/lib/ergatai-system-instruction.ts`

```typescript
import { prependSystemInstruction } from "../../ergatai-system-instruction"

// 加载 main_agent.md
const finalPrompt = sessionId
  ? prompt // 恢复的 session，不重复注入
  : prependSystemInstruction(prompt) // 新 session，注入指令

await acpSendPrompt(acpSessionId, finalPrompt)
```

### Sub-Agents (Rust side)

File: `src-rust/src/cross_agent/agent_launcher.rs`

```rust
// 加载 sub_agent.md
let sub_agent_prompt = include_str!("../../prompts/sub_agent.md");

// 替换模板变量
let sub_agent_prompt = sub_agent_prompt.replace("{{agent_list}}", &agent_list);

// 注入到子 Agent session
let full_instruction = format!("{}\n\n---\n\n{}", sub_agent_prompt, task_instruction);
```

## 意图识别示例

### ✅ 适合多 Agent 的请求

**用户**: "用 3 个 Agent 并行重构认证模块"
**判断**: 明确请求多 Agent → 生成 DAG

**用户**: "分析代码结构并实现新功能"
**判断**: 包含多个独立步骤 → 生成 DAG

**用户**: "优化这个模块的性能"
**判断**: 可能需要分析 + 实现 → 生成 DAG

### ❌ 不适合多 Agent 的请求

**用户**: "修复这个 bug"
**判断**: 单一任务 → 单 Agent 执行

**用户**: "解释这段代码"
**判断**: 快速问答 → 单 Agent 执行

## Design Principles

- **角色分离**: 主 Agent 负责编排，子 Agent 负责执行
- **意图清晰**: 主 Agent 需要能识别何时使用多 Agent
- **职责单一**: 每个提示词文件有明确的用途
- **模板友好**: 使用 `{{variable}}` 进行动态替换
- **自包含**: 每个提示词应该完整且独立可理解
