/**
 * 手动测试多 Agent 协作
 *
 * 在浏览器控制台运行这个函数来测试 DAG 提交流程
 */

// 测试 DAG markdown
const TEST_DAG = `
## Task A (分析代码)
- **agent**: agent-a
- **task**: 分析 src-rust/src 目录的代码结构，找出主要模块

## Task B (写实现)
- **agent**: agent-b
- **task**: 基于分析结果实现一个新功能
- **depends_on**: [Task A]

## Task C (写测试)
- **agent**: agent-c
- **task**: 为新功能编写单元测试
- **depends_on**: [Task A]
`

async function testMultiAgent() {
  console.log("=== 测试多 Agent 协作 ===")
  console.log("\n1. 提交 DAG...")

  try {
    // 调用 tRPC API 提交 DAG
    const result = await window.__TRPC__.dag.submit.mutate({
      markdown: TEST_DAG
    })

    console.log("✓ DAG 提交成功!")
    console.log("提交的任务:", result.submittedTaskIds)

    // 轮询状态
    console.log("\n2. 获取 DAG 状态...")
    const state = await window.__TRPC__.dag.getState.query()
    console.log("DAG 状态:", state)

    // 获取 Agent 状态
    console.log("\n3. 获取 Agent 状态...")
    const agents = await window.__TRPC__.dag.getAgentsStatus.query()
    console.log("Agent 状态:", agents)

    console.log("\n=== 测试完成 ===")
    console.log("现在 AgentsPanel 应该显示 3 个子 Agent")

  } catch (error) {
    console.error("✗ 测试失败:", error)
  }
}

// 导出到全局
window.testMultiAgent = testMultiAgent

console.log("✅ 多 Agent 测试函数已加载")
console.log("在控制台运行: testMultiAgent()")
