/**
 * 测试多 Agent 协作流程
 *
 * 这个脚本手动提交一个 DAG 来测试整个流程是否工作
 */

// 模拟 DAG markdown
const testDagMarkdown = `
## Task A (分析代码)
- **agent**: agent-a
- **task**: 分析 src-rust/src 目录的代码结构

## Task B (写实现)
- **agent**: agent-b
- **task**: 基于分析结果实现新功能
- **depends_on**: [Task A]

## Task C (写测试)
- **agent**: agent-c
- **task**: 为新功能编写单元测试
- **depends_on**: [Task A]
`

console.log("=== 测试多 Agent 协作流程 ===\n")
console.log("DAG Markdown:")
console.log(testDagMarkdown)

// 测试步骤
console.log("\n=== 测试步骤 ===")
console.log("1. DagDetector 检测 ```dag 代码块")
console.log("2. 调用 trpc.dag.submit({ markdown })")
console.log("3. Rust DagScheduler 解析并创建 Agent")
console.log("4. 前端轮询 dag.getState() 获取状态")
console.log("5. AgentsPanel 显示 Agent 列表")

// 测试 DagDetector
console.log("\n=== 测试 DagDetector ===")
const { detectDagMarkdown } = require('./src/main/lib/dag-detector.ts')

// 模拟 Agent 输出
const agentOutput = `
好的，我会创建一个 DAG 来并行处理这个任务：

\`\`\`dag
${testDagMarkdown}
\`\`\`

这样就会有 3 个子 Agent 并行工作。
`

const detected = detectDagMarkdown(agentOutput)
console.log("检测到 DAG:", detected ? "✓" : "✗")
if (detected) {
  console.log("DAG 内容:", detected.slice(0, 100) + "...")
}
