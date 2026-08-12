/**
 * Ergatai 主 Agent 系统指令加载器
 *
 * 从 src-rust/prompts/ 目录加载提示词
 */

import { readFileSync } from 'fs'
import { join } from 'path'

/**
 * 加载主 Agent 系统指令
 */
function loadMainAgentInstruction(): string {
  console.log('[Ergatai] Loading main agent instruction...')

  try {
    // 尝试多个可能的路径
    const candidates = [
      join(__dirname, '../../src-rust/prompts/main_agent.md'),
      join(__dirname, '../../../src-rust/prompts/main_agent.md'),
      join(process.cwd(), 'src-rust/prompts/main_agent.md'),
    ]

    console.log('[Ergatai] Trying paths:', candidates)

    for (const path of candidates) {
      try {
        const content = readFileSync(path, 'utf-8')
        console.log('[Ergatai] ✅ Loaded from:', path)
        console.log('[Ergatai] Content length:', content.length)
        // 替换 {{agent_list}} 占位符（暂时用空字符串，后续可以从 Rust 获取）
        return content.replace(/\{\{agent_list\}\}/g, '- **claude-code** — Claude Code Agent\n- **codex** — Codex Agent')
      } catch (e) {
        console.log('[Ergatai] ❌ Failed to load from:', path, e.message)
        continue
      }
    }

    // 如果都失败，返回内置的默认指令
    console.warn('[Ergatai] ⚠️ All paths failed, using fallback')
    return getDefaultInstruction()
  } catch (error) {
    console.error('[Ergatai] ❌ Error loading main agent instruction:', error)
    return getDefaultInstruction()
  }
}

/**
 * 默认指令（fallback）
 */
function getDefaultInstruction(): string {
  return `
# 主 Agent 指令

你是 Ergatai 桌面应用的主 Agent。

## 何时使用多 Agent

当用户的请求可以拆分成独立的并行子任务时，使用 DAG 格式。

**关键词**：并行、同时、多个、重构、分析+实现+测试

## DAG 格式

使用 \`\`\`dag 代码块，参考 src-rust/prompts/main_agent.md 中的完整格式。

### 重要说明

- 每个 DAG task 会创建独立的 ACP session
- 不是 Claude Code 的 sub-agent，是 Ergatai 的并行任务系统
- 不要使用 Claude Code 内置的 agent team 功能
`
}

// 加载指令
const MAIN_AGENT_INSTRUCTION = loadMainAgentInstruction()

/**
 * 将系统指令与用户 prompt 合并
 */
export function prependSystemInstruction(userPrompt: string): string {
  return `${MAIN_AGENT_INSTRUCTION}\n\n---\n\n用户请求:\n${userPrompt}`
}

/**
 * 检查 prompt 是否需要多 Agent 指令
 *
 * 根据关键词判断是否注入指令
 */
export function needsMultiAgentInstruction(prompt: string): boolean {
  const keywords = [
    '并行', 'parallel', '多个', 'multiple',
    '同时', 'simultaneously', '分工',
    '重构', 'refactor', '模块', 'module',
    '分析', 'analyze', '实现', 'implement',
    '测试', 'test', '审查', 'review',
    '优化', 'optimize', '改进', 'improve'
  ]

  const lowerPrompt = prompt.toLowerCase()
  return keywords.some(keyword => lowerPrompt.includes(keyword))
}
