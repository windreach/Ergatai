# Installation Guide

## Quick Install (Recommended)

```bash
curl -sSL https://raw.githubusercontent.com/windreach/Ergatai/main/install.sh | bash
```

This one-liner:
1. Downloads `ergatai` (CLI) and `ergatai-server` (server)
2. Installs to `/usr/local/bin`
3. Creates symlink `ega` → `ergatai`
4. Grants `CAP_SYS_ADMIN` to `ergatai-server` for kernel-level file locking

## Manual Install

### 1. Download binaries

```bash
# CLI
curl -L -o ergatai https://github.com/windreach/Ergatai/releases/latest/download/ergatai-x86_64
chmod +x ergatai
sudo mv ergatai /usr/local/bin/

# Server
curl -L -o ergatai-server https://github.com/windreach/Ergatai/releases/latest/download/ergatai-server-x86_64
chmod +x ergatai-server
sudo mv ergatai-server /usr/local/bin/
```

### 2. Create symlink (optional)

```bash
sudo ln -s /usr/local/bin/ergatai /usr/local/bin/ega
```

### 3. Grant capabilities (CRITICAL)

```bash
sudo setcap 'cap_sys_admin+ep' /usr/local/bin/ergatai-server
```

Verify:
```bash
getcap /usr/local/bin/ergatai-server
# Should show: /usr/local/bin/ergatai-server cap_sys_admin=ep
```

## Build from Source

```bash
git clone https://github.com/windreach/Ergatai.git
cd Ergatai

# Build CLI
cargo build --release -p ergatai-cli
sudo cp target/release/ergatai /usr/local/bin/
sudo ln -s /usr/local/bin/ergatai /usr/local/bin/ega

# Build server
cargo build --release -p ergatai-api
sudo cp target/release/ergatai-server /usr/local/bin/
sudo setcap 'cap_sys_admin+ep' /usr/local/bin/ergatai-server
```

## File Locking Permissions

### Why CAP_SYS_ADMIN?

Ergatai uses Linux **fanotify** with `FAN_OPEN_PERM` events for kernel-level file locking. This intercepts `open()` syscalls at the VFS layer, preventing unauthorized access before it reaches the application.

The Linux kernel requires `CAP_SYS_ADMIN` for fanotify permission events.

### Without CAP_SYS_ADMIN

| Mode | Enforcement | Risk |
|------|-------------|------|
| **Mandatory** (with caps) | Kernel blocks unauthorized `open()` | None |
| **Advisory** (without) | Agents cooperate voluntarily | High — direct shell can bypass |

### Platform Support

| Platform | Fanotify | Default Mode |
|----------|----------|--------------|
| Linux (with `CAP_SYS_ADMIN`) | ✅ | Mandatory |
| Linux (no caps) | ⚠️ | Advisory |
| macOS | ❌ | Advisory |
| Windows | ❌ | Advisory |

## systemd Service

```ini
[Unit]
Description=Ergatai Multi-Agent Middleware
After=network.target

[Service]
Type=simple
User=ergatai
ExecStart=/usr/local/bin/ergatai-server --port 3000
AmbientCapabilities=CAP_SYS_ADMIN
CapabilityBoundingSet=CAP_SYS_ADMIN
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable ergatai
sudo systemctl start ergatai
```

## Verify Installation

```bash
# Check CLI
ergatai --version
ega --version

# Check server
ergatai-server --version

# Check capabilities
getcap /usr/local/bin/ergatai-server
```

## Next Steps

- [CLI Guide](../guide/CLI.md) — learn how to use the CLI
- [MCP Configuration](../guide/MCP.md) — configure your agents
