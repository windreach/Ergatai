/**
 * Runtime atoms 自测
 *
 * 验证类型映射和 normalize 逻辑。
 * 无需测试 atom 运行时行为（Jotai 已覆盖），只测纯函数逻辑。
 */

import { normalizeRuntimeId, PROVIDER_TO_RUNTIME, RUNTIME_TO_PROVIDER } from "../lib/runtime-types"

function test(condition: boolean, name: string) {
  if (!condition) {
    console.error(`❌ FAIL: ${name}`)
    process.exit(1)
  }
  console.log(`✅ PASS: ${name}`)
}

function runTests() {
  // PROVIDER_TO_RUNTIME mapping
  test(PROVIDER_TO_RUNTIME["claude-code"] === "claude", "claude-code → claude")
  test(PROVIDER_TO_RUNTIME["codex"] === "codex", "codex → codex")
  test(PROVIDER_TO_RUNTIME["unknown"] === undefined, "unknown provider → undefined")

  // RUNTIME_TO_PROVIDER reverse mapping
  test(RUNTIME_TO_PROVIDER["claude"] === "claude-code", "claude → claude-code")
  test(RUNTIME_TO_PROVIDER["codex"] === "codex", "codex → codex")

  // normalizeRuntimeId: old provider IDs
  test(normalizeRuntimeId("claude-code") === "claude", "normalize claude-code")
  test(normalizeRuntimeId("codex") === "codex", "normalize codex")

  // normalizeRuntimeId: already runtime IDs (pass through)
  test(normalizeRuntimeId("claude") === "claude", "pass-through claude")
  test(normalizeRuntimeId("goose") === "goose", "pass-through goose")
  test(normalizeRuntimeId("custom-agent") === "custom-agent", "pass-through custom")

  console.log("\n✅ All runtime type tests passed")
}

runTests()
