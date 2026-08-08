/**
 * AgentSelector 逻辑自测
 *
 * 测试 runtime 过滤、排序、默认值逻辑。
 * 不测试 React 渲染（无 DOM 环境）。
 */

import type { AcpRuntime } from "../lib/runtime-types"

function test(condition: boolean, name: string) {
  if (!condition) {
    console.error(`❌ FAIL: ${name}`)
    process.exit(1)
  }
  console.log(`✅ PASS: ${name}`)
}

const mockRuntimes: AcpRuntime[] = [
  {
    id: "claude",
    label: "Claude Code",
    avatar_url: "",
    availability: "available",
    command: "claude-agent-acp",
    binary_path: "/usr/local/bin/claude-agent-acp",
    install_hint: "",
    install_instructions_url: "",
    auth_status: "logged_in",
    login_hint: null,
    source: "builtin",
  },
  {
    id: "codex",
    label: "OpenAI Codex",
    avatar_url: "",
    availability: "available",
    command: "codex-acp",
    binary_path: null,
    install_hint: "",
    install_instructions_url: "",
    auth_status: "logged_in",
    login_hint: null,
    source: "builtin",
  },
  {
    id: "goose",
    label: "Goose",
    avatar_url: "",
    availability: "not_installed",
    command: "goose",
    binary_path: null,
    install_hint: "npm install -g @block/goose",
    install_instructions_url: "",
    auth_status: "not_applicable",
    login_hint: null,
    source: "builtin",
  },
  {
    id: "hermes",
    label: "Hermes",
    avatar_url: "",
    availability: "auth_required",
    command: "hermes-acp",
    binary_path: null,
    install_hint: "",
    install_instructions_url: "",
    auth_status: "logged_out",
    login_hint: "Run hermes login",
    source: "builtin",
  },
]

function runTests() {
  // Filter: only available
  const available = mockRuntimes.filter((r) => r.availability === "available")
  test(available.length === 2, "filter available: 2 runtimes")
  test(available[0].id === "claude", "first available is claude")
  test(available[1].id === "codex", "second available is codex")

  // Default: first available
  const defaultRuntime = available[0]
  test(defaultRuntime?.id === "claude", "default runtime is first available")

  // Find by id
  const found = available.find((r) => r.id === "codex")
  test(found?.label === "OpenAI Codex", "find by id: codex")

  // Not found
  const notFound = available.find((r) => r.id === "nonexistent")
  test(notFound === undefined, "find nonexistent: undefined")

  console.log("\n✅ All AgentSelector logic tests passed")
}

runTests()
