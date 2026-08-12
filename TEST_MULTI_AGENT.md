# 多 Agent 测试指南

## 🧪 测试步骤

### 1. 重启应用

```bash
# 停止当前运行的应用（Ctrl+C）
# 然后重新启动
bun run dev
```

### 2. 查看启动日志

启动后应该看到：

```
[Ergatai] Loading main agent instruction...
[Ergatai] Trying paths: [path1, path2, path3]
[Ergatai] ✅ Loaded from: /path/to/src-rust/prompts/main_agent.md
[Ergatai] Content length: 3456
```

如果没有看到这些日志，说明系统指令没有加载。

### 3. 创建新的 Chat

- 点击 "New Chat"
- 选择一个项目文件夹

### 4. 发送测试请求

发送以下消息：

```
用 3 个 Agent 并行重构认证模块
```

### 5. 查看 ACP 日志

在终端日志中应该看到：

```
[ACP] Session ID: new
[ACP] Final prompt length: 4567 (original: 25)
[ACP] Sending prompt (first 200 chars): # 主 Agent 指令...
```

如果 `Final prompt length` 和 `original` 相同，说明系统指令没有注入。

### 6. 观察主 Agent 的响应

**期望的响应**：

主 Agent 应该输出类似：

````
好的，我会创建一个 DAG 来并行处理：

```dag
# Task: 重构认证模块

## Task A: 分析现有代码
- **agent**: claude-code
- **task**: 分析 src/auth/ 的代码结构
- **depends_on**: []

## Task B: 实现修复
- **agent**: codex
- **task**: 基于分析结果实现安全修复
- **depends_on**: [Task A]

## Task C: 编写测试
- **agent**: codex
- **task**: 为重构后的代码编写单元测试
- **depends_on**: [Task B]
```
````

**如果主 Agent 没有生成 DAG**：

可能的原因：
1. 系统指令没有正确加载（查看日志）
2. 系统指令加载了但主 Agent 忽略了
3. 主 Agent 生成了 DAG 但 DagDetector 没有检测到

### 7. 检查 DagDetector 日志

如果主 Agent 生成了 DAG，应该看到：

```
[DAG Detector] Detected DAG markdown, auto-submitting...
[DAG Detector] Submitted successfully: ["n1", "n2", "n3"]
```

### 8. 检查 AgentsPanel

如果 DAG 提交成功：
- AgentsPanel 应该自动展开
- 显示 3 个子 Agent
- 状态实时更新（Pending → Running → Completed）

---

## 🔍 故障排查

### 问题 1: 没有看到加载日志

**原因**: 文件路径不正确

**解决**: 检查 `src-rust/prompts/main_agent.md` 是否存在

```bash
ls -la src-rust/prompts/main_agent.md
```

### 问题 2: 系统指令加载了但主 Agent 没有生成 DAG

**原因**: 指令不够清晰或主 Agent 忽略了

**解决**:
1. 查看完整的 prompt（日志中的前 200 字符）
2. 确认指令包含了 DAG 格式示例
3. 尝试更明确的请求："请生成一个 DAG，包含 3 个任务..."

### 问题 3: 主 Agent 生成了 DAG 但没有提交

**原因**: DagDetector 没有检测到

**解决**:
1. 检查 DAG 格式是否正确（必须有 ```dag）
2. 查看 DagDetector 日志
3. 手动测试 DagDetector

### 问题 4: DAG 提交了但 AgentsPanel 没有显示

**原因**: 前端没有正确获取数据

**解决**:
1. 在 DevTools Console 中运行：
   ```javascript
   window.__TRPC__.dag.getState.query()
   ```
2. 检查是否返回了 TaskGraph 数据
3. 查看 useAgents hook 是否正常工作

---

## 📊 预期的完整日志流程

```
1. 应用启动
   [Ergatai] Loading main agent instruction...
   [Ergatai] ✅ Loaded from: src-rust/prompts/main_agent.md

2. 用户发送消息
   [ACP] Session ID: new
   [ACP] Final prompt length: 4567 (original: 25)
   [ACP] Sending prompt (first 200 chars): # 主 Agent 指令...

3. 主 Agent 生成 DAG
   [ACP] Emitted chunk: type=text-delta subChatId=abc123
   [ACP] Emitted chunk: type=text-delta subChatId=abc123
   ...

4. DagDetector 检测到 DAG
   [DAG Detector] Detected DAG markdown, auto-submitting...
   [DAG Detector] Submitted successfully: ["n1", "n2", "n3"]

5. Rust 创建子 Agent
   [DagScheduler] Created session for task n1
   [DagScheduler] Created session for task n2
   [DagScheduler] Created session for task n3

6. 前端显示
   [AgentsPanel] Rendering 3 agents
   [useAgents] Fetching DAG state...
   [useAgents] Got 3 agents
```

---

## 🎯 快速测试命令

在浏览器控制台运行：

```javascript
// 测试 DagDetector
const { detectDagMarkdown } = require('./src/main/lib/dag-detector.ts')
const testDag = `
\`\`\`dag
## Task A
- **agent**: test
- **task**: test task
\`\`\`
`
console.log(detectDagMarkdown(testDag))

// 测试 tRPC API
window.__TRPC__.dag.submit.mutate({ markdown: testDag })
  .then(r => console.log('Success:', r))
  .catch(e => console.error('Error:', e))
```

---

## ✅ 测试检查清单

- [ ] 应用启动时看到加载日志
- [ ] 新 Chat 时看到注入日志
- [ ] 主 Agent 收到系统指令
- [ ] 主 Agent 生成 DAG 格式
- [ ] DagDetector 检测到 DAG
- [ ] DAG 提交成功
- [ ] Rust 创建子 Agent
- [ ] AgentsPanel 显示子 Agent
- [ ] 可以切换查看子 Agent 对话
