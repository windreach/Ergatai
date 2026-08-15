# Agent 配置托管设计

## 概述

Ergatai 支持两种 Agent 来源：
- **系统默认**：自动发现已安装的 agent，使用原生配置启动
- **用户自建**：用户创建自定义 agent 配置，Ergatai 托管，完全独立

每个自建 agent 的配置文件 = **原 agent 配置格式** + **`ergatai` 系统字段组**。启动时系统自动裁剪 `ergatai` 组，只将原 agent 配置传给它。

## 目录结构

```
~/.config/ergatai/agents/
├── my-claude-opus/                    ← 自定义 agent 名（用户定义，唯一标识）
│   ├── settings.json                  ← 配置文件（原 agent 格式 + ergatai 组）
│   └── avatar.png                     ← 可选头像
├── my-claude-sonnet-eu/
│   └── settings.json
└── my-codex-fast/
    └── settings.json
```

## 配置格式（JSON）

### 结构

```json
{
  "ergatai": {
    "agentBase": "claude",
    "displayName": "我的 Claude Opus",
    "proxy": "http://127.0.0.1:7890",
    "avatar": "./avatar.png"
  },

  // ↓↓↓ 以下全部是原 agent 配置，格式不变 ↓↓↓
  "env": { ... },
  "model": "...",
  ...
}
```

### `ergatai` 系统字段

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `agentBase` | string | ✅ | 底层 agent 标识：`claude`、`codex`、`goose`、`hermes` 等 |
| `displayName` | string | ❌ | 显示名称，不设则用 agent 目录名 |
| `proxy` | string | ❌ | 网络代理地址，如 `http://127.0.0.1:7890` |
| `avatar` | string | ❌ | 头像路径（相对或绝对），不设则系统自动分配 |

### 裁剪规则

启动时：
1. 读取完整 `settings.json`
2. 提取 `ergatai` 组 → 系统使用（agentBase 决定启动命令、proxy 注入环境变量等）
3. **删除 `ergatai` key** → 剩余部分就是原 agent 的原生配置
4. 将裁剪后的配置传给 agent 子进程

## 示例

### Claude Code（接入 DeepSeek）

```json
{
  "ergatai": {
    "agentBase": "claude",
    "displayName": "DeepSeek Claude",
    "proxy": "http://127.0.0.1:7890"
  },
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "sk-xxx",
    "ANTHROPIC_BASE_URL": "https://api.deepseek.com/anthropic",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "deepseek-v4-pro",
    "ANTHROPIC_MODEL": "deepseek-v4-flash",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "deepseek-v4-flash",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "deepseek-v4-flash"
  },
  "model": "deepseek-v4-flash",
  "theme": "auto"
}
```

**裁剪后传给 Claude Code 的：**
```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "sk-xxx",
    "ANTHROPIC_BASE_URL": "https://api.deepseek.com/anthropic",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "deepseek-v4-pro",
    "ANTHROPIC_MODEL": "deepseek-v4-flash",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "deepseek-v4-flash",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "deepseek-v4-flash"
  },
  "model": "deepseek-v4-flash",
  "theme": "auto"
}
```

### Codex

```json
{
  "ergatai": {
    "agentBase": "codex",
    "displayName": "Codex Fast"
  },
  "model": "o4-mini",
  "provider": "openai"
}
```

### Goose

```json
{
  "ergatai": {
    "agentBase": "goose",
    "displayName": "Goose Doc Writer"
  },
  "env": {
    "GOOSE_PROVIDER": "openai",
    "GOOSE_MODEL": "gpt-4o"
  }
}
```

## agentBase 映射表

`agentBase` 决定默认启动命令。用户如果在配置中写了 `command` 字段则覆盖默认值。

| agentBase | 默认 command | 说明 |
|-----------|-------------|------|
| `claude` | `claude` | Claude Code CLI（在 tmux 中运行） |
| `codex` | `codex` | Codex CLI |
| `goose` | `goose` | Goose CLI |
| `hermes` | `hermes` | Hermes CLI |

## 启动流程

```
1. 读取 ~/.config/ergatai/agents/{agentName}/settings.json
    ↓
2. 提取并删除 ergatai 组
    ├─ agentBase = "claude"
    ├─ displayName = "DeepSeek Claude"
    ├─ proxy = "http://127.0.0.1:7890"
    └─ avatar = null（自动分配）
    ↓
3. 裁剪后的 JSON = 原 agent 配置
    ↓
4. agentBase → 查映射表 → command = "claude"
   （如果配置中有 command 字段则用用户的）
    ↓
5. 如果 proxy 不为空 → 注入 HTTP_PROXY/HTTPS_PROXY 到 env
    ↓
6. 通过 MCP client 配置注入 Ergatai MCP Server
    ↓
7. 在 tmux pane 中启动子进程：command + args + env + 裁剪后的配置
```

## 系统默认 Agent

系统自动发现的 agent 不需要用户创建配置文件。启动时使用 agent 的原生配置路径：

| agentBase | 原生配置路径 |
|-----------|-------------|
| `claude` | `~/.claude/settings.json` |
| `codex` | `~/.codex/config.toml` |
| `goose` | `~/.config/goose/config.yaml` |

系统默认 agent 不经过 `ergatai` 裁剪流程，直接使用原生配置。

## Rust 数据结构

```rust
/// Ergatai 系统字段（从 settings.json 的 "ergatai" 组提取）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErgataiAgentMeta {
    pub agent_base: String,
    pub display_name: Option<String>,
    pub proxy: Option<String>,
    pub avatar: Option<String>,
}

/// 完整配置文件（ergatai 组 + 原 agent 配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedAgentConfig {
    pub ergatai: ErgataiAgentMeta,
    /// 剩余字段保持为原始 JSON Value，透传给 agent
    #[serde(flatten)]
    pub agent_config: serde_json::Value,
}

/// 裁剪后的纯 agent 配置（传给 agent 子进程）
/// 就是 HostedAgentConfig.agent_config
```

## 文件路径约定

| 用途 | 路径 |
|------|------|
| 自建 agent 配置目录 | `~/.config/ergatai/agents/` |
| 单个 agent 配置 | `~/.config/ergatai/agents/{name}/settings.json` |
| 单个 agent 头像 | `~/.config/ergatai/agents/{name}/avatar.{ext}` |
| 系统全局 agent 配置 | `~/.config/ergatai/agents.json`（可选，未来扩展） |
