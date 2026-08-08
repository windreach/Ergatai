/**
 * ACP Runtime 类型定义
 *
 * 从后端 src-rust/src/agent/runtime_metadata.rs 的 AcpRuntimeCatalogEntry 映射。
 * 前端类型与后端保持字段名一致，便于 trpc.agents.listRuntimes 返回值直接赋值。
 */

/** Agent runtime 可用性状态 */
export type RuntimeAvailability = "available" | "not_installed" | "auth_required"

/** Agent runtime 来源 */
export type RuntimeSource = "builtin" | "custom"

/** Agent auth 状态 */
export type RuntimeAuthStatus = "logged_in" | "logged_out" | "not_applicable" | "unknown"

/** ACP Runtime 条目（一个可安装的 agent binary） */
export interface AcpRuntime {
  /** Runtime 唯一 ID（如 "claude", "codex", "goose"） */
  id: string
  /** 显示名称（如 "Claude Code", "OpenAI Codex"） */
  label: string
  /** 图标 URL（后端保证非空） */
  avatar_url: string
  /** 可用性状态 */
  availability: RuntimeAvailability
  /** 启动命令（如 "claude-agent-acp"） */
  command: string | null
  /** 二进制路径 */
  binary_path: string | null
  /** 安装提示文本 */
  install_hint: string
  /** 安装说明 URL */
  install_instructions_url: string
  /** Auth 状态 */
  auth_status: RuntimeAuthStatus
  /** 登录提示 */
  login_hint: string | null
  /** 来源：builtin（项目内置）或 custom（用户自定义） */
  source: RuntimeSource
}

/** ACP Runtime 支持的 Model */
export interface AgentModel {
  /** Model ID（如 "opus", "sonnet", "gpt-5.3-codex"） */
  id: string
  /** 显示名称（如 "Opus 4.6"） */
  label: string
  /** 所属 runtime ID */
  runtimeId: string
}

/**
 * Provider ID 到 Runtime ID 的映射（向后兼容）
 *
 * 旧前端用 "claude-code" | "codex" 作为 provider ID，
 * 新代码统一用 runtime ID（"claude", "codex"）。
 * 此映射保证旧数据能正确迁移。
 */
export const PROVIDER_TO_RUNTIME: Record<string, string> = {
  "claude-code": "claude",
  "codex": "codex",
}

/**
 * Runtime ID 到 Provider ID 的反向映射
 */
export const RUNTIME_TO_PROVIDER: Record<string, string> = {
  "claude": "claude-code",
  "codex": "codex",
}

/**
 * 将旧 provider ID 转换为 runtime ID
 * 如果已经是 runtime ID 则原样返回
 */
export function normalizeRuntimeId(id: string): string {
  return PROVIDER_TO_RUNTIME[id] ?? id
}
