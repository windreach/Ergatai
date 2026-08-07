# Ergatai

> ⚠️ **This project is under active development.** Building a multi-agent collaboration platform with Rust backend and ACP protocol standardization.

A **multi-agent collaboration platform** that turns AI coding assistants into a coordinated team. Built with a **Rust backend**, **ACP (Agent Client Protocol)** standardization, and **multi-agent orchestration** — enabling multiple agents to collaborate, communicate, and coordinate tasks.

## What is Ergatai?

Ergatai is not just another coding assistant — it's a **platform for agent collaboration**. While traditional tools focus on single-agent interactions, Ergatai enables:

- **Multi-Agent Teams** — PM → Dev → QA pipelines that work together
- **ACP Standardization** — Plug in any ACP-compliant agent (Claude, Codex, Goose, 13+ supported)
- **Rust Performance** — Native backend with NAPI bindings for speed and safety
- **Agent Network** — NATS-based communication for task distribution and coordination
- **Local-First** — Your code, your agents, your machine (no cloud lock-in)

## Architecture

```
┌─────────────────────────────────────────┐
│  Frontend (React + Electron)            │
│  - Chat UI, diff preview, tool display  │
│  - Plan mode, file viewer, terminal     │
└──────────────┬──────────────────────────┘
               │ tRPC
┌──────────────▼──────────────────────────┐
│  Rust Backend (src-rust/)               │
│  ┌─────────────────────────────────┐    │
│  │  ACP Layer (session mgmt)       │    │
│  ├─────────────────────────────────┤    │
│  │  Agent Network (NATS)           │    │
│  │  - Register/unregister agents   │    │
│  │  - Send tasks/results           │    │
│  │  - Broadcast to channels        │    │
│  │  - AI-friendly messages         │    │
│  ├─────────────────────────────────┤    │
│  │  Agent Discovery & Management   │    │
│  │  - 13 builtin agents supported  │    │
│  │  - Custom harness support       │    │
│  │  - Auth probing & binary scan   │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

## Supported Agents

Ergatai supports **13 builtin agents** with automatic discovery, plus unlimited custom agents:

### Tier 1 Agents (Full Metadata)
| Agent | Protocol | Status |
|-------|----------|--------|
| **Goose** | ACP native | ✅ Supported |
| **Claude Code** | ACP via `claude-agent-sdk` | ✅ Supported |
| **Codex** | ACP via `@zed-industries/codex-acp` | ✅ Supported |
| **Hermes** | ACP | ✅ Supported |

### Tier 2 Agents (Preset)
| Agent | Protocol | Status |
|-------|----------|--------|
| **Devin** | ACP | ✅ Supported |
| **Cursor** | ACP | ✅ Supported |
| **Oh My Pi** | ACP | ✅ Supported |
| **Grok Build** | ACP | ✅ Supported |
| **OpenCode** | ACP | ✅ Supported |
| **Kimi Code** | ACP | ✅ Supported |
| **Amp** | ACP | ✅ Supported |
| **OpenClaw** | ACP | ✅ Supported |

### Custom Agents
Register any ACP-compliant agent via custom harness definitions in `~/.config/ergatai/custom_harnesses/`.

## Key Features

### Multi-Agent Collaboration
- **Agent Network** — Register, discover, and communicate with agents via NATS
- **Task Distribution** — Send tasks between agents, track progress, collect results
- **AI-Friendly Messages** — Structured messages with intent, expectations, and constraints
- **Channel Broadcasting** — Broadcast to agent groups for coordination

### ACP Agent Management
- **Automatic Discovery** — Detects installed agents, probes authentication status
- **Global Configuration** — Unified provider/model/env_vars configuration
- **Custom Harness** — Define custom agents via JSON files
- **Environment Injection** — Automatic env var injection based on runtime metadata

### Visual Collaboration UI
- **Chat Interface** — Familiar chat UX for each agent
- **Tool Execution** — Watch agents run bash, edit files, search web
- **Diff Preview** — See code changes before they land
- **Agent Status** — Monitor which agents are active, idle, or busy

### Additional Features
- **Git Worktree Isolation** — Each chat runs in its own isolated worktree
- **Background Agents** — Cloud sandboxes that run when your laptop sleeps
- **Live Browser Previews** — Preview dev branches in a real browser
- **Kanban Board** — Visualize agent sessions
- **Built-in Git Client** — Visual staging, diffs, PR creation
- **MCP & Plugins** — Server management, plugin marketplace
- **Voice Input** — Hold-to-talk dictation
- **Plan Mode** — Structured plans with markdown preview
- **Extended Thinking** — Enabled by default with visual UX

## Development

```bash
# Prerequisites: Bun, Python 3.11, Rust toolchain
bun install
bun run dev
```

### Build from Source

```bash
bun install
bun run build:rust        # Build Rust backend
bun run build:napi        # Generate NAPI bindings
bun run build             # Build Electron app
bun run package:linux     # or package:mac, package:win
```

## Installation

### macOS
```bash
# Download from releases (coming soon)
brew install ergatai
```

### Linux
```bash
# AppImage or DEB package (coming soon)
```

### Windows
```bash
# NSIS installer (coming soon)
```

## Community

- **Discord**: Join our [Discord server](https://discord.gg/8ektTZGnj4) for support and discussions
- **Issues**: Report bugs or request features via [GitHub Issues](https://github.com/windreach/ergatai/issues)
- **Contributions**: PRs welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines

## Acknowledgments

Ergatai is built with inspiration and code from two excellent projects:

### Frontend Architecture
The frontend is based on [1Code](https://1code.dev) by 21st.dev team (React + Electron + TypeScript). We thank the original contributors for their work on the UI components, Electron integration, and initial agent support.

**Note**: 1Code only has a TypeScript backend. The **Rust backend in Ergatai is original work**, built from scratch for native performance and ACP protocol support.

### Backend Design
The ACP agent management system (runtime metadata, discovery mechanism, global configuration) is inspired by [Buzz](https://github.com/nicepkg/buzz), a pioneering multi-agent desktop application.

Both projects are licensed under Apache 2.0. See [NOTICE](NOTICE) for detailed attribution information.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

This project includes code derived from 1Code and Buzz, both licensed under Apache 2.0. Original copyright notices are preserved as required.
