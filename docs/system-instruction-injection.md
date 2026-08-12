# Ergatai 系统指令注入方案

## ✅ 已实现

在第一次 ACP prompt 时自动注入系统指令，告诉主 Agent 如何使用 DAG 多 Agent 协作。

---

## 📝 系统指令内容

**文件**: `src/main/lib/ergatai-system-instruction.ts`

### 核心指令

```markdown
## Ergatai 多 Agent 协作

当任务可以并行执行时，使用 DAG（有向无环图）格式创建多个独立任务。

### DAG 格式

使用 ```dag 代码块：

```dag
## Task A: 分析代码结构
- **agent**: analyzer
- **scope**: src/core/

## Task B: 实现新功能
- **agent**: developer
- **scope**: src/feature/
- **depends_on**: [Task A]
```

### 任务属性

- **agent**: 执行者标识（如 analyzer, developer, tester）
- **scope**: 文件范围（glob 模式，如 src/**/*.rs）
- **depends_on**: 依赖的其他任务（可选）

### 何时使用

✅ **使用 DAG**：
- 多个独立模块可以并行处理
- 任务之间有明确的依赖关系
- 需要文件访问控制和隔离

❌ **不要使用**：
- 单一任务或顺序任务
- Claude Code 内置的 sub-agent 功能（本系统不使用）

### 重要说明

- 每个 DAG task 会创建**独立的 ACP session**
- 不是 Claude Code 的 sub-agent，是 Ergatai 的并行任务系统
- 任务可以并行执行，有文件锁保护
```

---

## 🔧 实现细节

### 1. 指令注入逻辑

**文件**: `src/main/lib/trpc/routers/acp.ts`

```typescript
// 导入系统指令
import { prependSystemInstruction } from "../../ergatai-system-instruction"

// 发送 prompt 时
const finalPrompt = sessionId
  ? prompt // 恢复的 session，不重复注入
  : prependSystemInstruction(prompt) // 新 session，注入指令

await acpSendPrompt(acpSessionId, finalPrompt)
```

### 2. 注入条件

- ✅ **新 Session**: 注入系统指令
- ❌ **恢复 Session**: 不注入（避免重复）

### 3. 指令格式

```typescript
export function prependSystemInstruction(userPrompt: string): string {
  return `${ERGATAI_SYSTEM_INSTRUCTION}\n\n---\n\n用户请求:\n${userPrompt}`
}
```

---

## 🎯 关键设计决策

### 1. 避免与 Claude Code Sub-agent 混淆

**问题**: Claude Code 有自己的 sub-agent 系统

**解决**: 在指令中明确说明
```markdown
### 重要说明

- 不是 Claude Code 的 sub-agent
- 是 Ergatai 的并行任务系统
- 不要使用 Claude Code 内置的 agent team 功能
```

### 2. 清晰的格式示例

提供完整的 DAG markdown 示例，包括：
- 任务定义（`## Task A:`）
- 必需属性（`agent`, `scope`）
- 可选属性（`depends_on`, `task`）

### 3. 明确的使用场景

告诉主 Agent 什么时候应该使用 DAG：
- ✅ 并行任务
- ✅ 有依赖关系
- ✅ 需要文件隔离

---

## 🧪 测试流程

### 1. 启动开发服务器

```bash
bun run dev
```

### 2. 发送多 Agent 请求

在聊天中输入：

```
用 3 个 Agent 并行重构这个模块：
- 一个分析现有代码
- 一个实现新功能
- 一个编写测试
```

### 3. 预期行为

主 Agent 应该输出：

```markdown
好的，我会创建一个 DAG 来并行处理这个任务：

```dag
## Task A: 分析现有代码
- **agent**: analyzer
- **scope**: src/
- **task**: 分析代码结构，找出主要模块和依赖关系

## Task B: 实现新功能
- **agent**: developer
- **scope**: src/feature/
- **depends_on**: [Task A]
- **task**: 基于分析结果实现新功能

## Task C: 编写测试
- **agent**: tester
- **scope**: tests/
- **depends_on**: [Task A]
- **task**: 为新功能编写单元测试
```
```

### 4. 验证

- ✅ DagDetector 检测到 ````dag` 代码块
- ✅ 自动提交到 Rust DagScheduler
- ✅ AgentsPanel 显示 3 个子 Agent
- ✅ 可以切换查看每个 Agent 的对话

---

## 📊 Token 成本

**系统指令长度**: ~800 tokens

**权衡**:
- ✅ 一次性成本（只在新 session 时注入）
- ✅ 确保主 Agent 知道如何使用 DAG
- ✅ 避免混淆和错误行为
- ❌ 增加每次新对话的 token 消耗

**优化建议**:
- 可以根据关键词判断是否需要注入（`needsMultiAgentInstruction`）
- 可以压缩指令长度
- 可以在用户明确请求时才注入

---

## 🔍 调试技巧

### 1. 查看注入的指令

```bash
# 启用详细日志
DEBUG=acp:* bun run dev
```

日志会显示：
```
[ACP] Sending prompt (length=1234): ## Ergatai 多 Agent 协作...
```

### 2. 验证指令效果

在 DevTools Console 中：
```javascript
// 查看发送给 Agent 的完整 prompt
// 应该在最前面看到系统指令
```

### 3. 测试恢复 Session

```javascript
// 恢复 session 时不应该重复注入
// 可以通过 sessionId 参数判断
```

---

## 🚀 下一步优化

### TODO 1: 智能注入

只在需要时注入指令：

```typescript
if (needsMultiAgentInstruction(prompt)) {
  finalPrompt = prependSystemInstruction(prompt)
} else {
  finalPrompt = prompt
}
```

### TODO 2: 用户可控

允许用户禁用系统指令：

```typescript
const enableInstructions = !settings.disableErgataiInstructions
```

### TODO 3: 多语言支持

根据用户语言设置注入对应的指令版本。

---

## 📚 相关文件

| 文件 | 作用 |
|------|------|
| `src/main/lib/ergatai-system-instruction.ts` | 系统指令定义 |
| `src/main/lib/trpc/routers/acp.ts` | 指令注入逻辑 |
| `src/main/lib/dag-detector.ts` | DAG 检测和自动提交 |
| `src-rust/src/cross_agent/dag_scheduler.rs` | DAG 调度器 |

---

## ✨ 总结

现在主 Agent 在第一次对话时会自动收到系统指令，知道如何使用 DAG 格式创建多 Agent 协作。

**关键特性**:
- ✅ 只在新 session 时注入
- ✅ 明确区分 Claude Code sub-agent 和 Ergatai DAG
- ✅ 清晰的格式示例和使用场景
- ✅ 避免混淆，确保正确行为

用户现在可以直接请求多 Agent 任务，主 Agent 会自动使用正确的 DAG 格式。
