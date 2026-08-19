#!/usr/bin/env bash
# install.sh — Install Ergatai (CLI + server) with fanotify capabilities
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/windreach/Ergatai/main/install.sh | bash
#   curl -sSL ... | bash -s -- v0.1.0 /usr/local/bin
#
# What this does:
#   1. Downloads TWO binaries from GitHub Releases:
#        - `ergatai`        (CLI) — user-facing tool for managing workspaces/agents,
#                                  wraps rmux commands via the API server
#        - `ergatai-server` (server) — HTTP/MCP server that manages rmux-daemon,
#                                  nats-server, and exposes the MCP API to agents
#   2. Installs them to the target directory (default: /usr/local/bin)
#   3. Creates symlink: `ega` -> `ergatai` (short alias)
#   4. Grants CAP_SYS_ADMIN to `ergatai-server` only — REQUIRED for kernel-level
#      file locking via fanotify. The CLI does not need any special capabilities.
#
# Architecture:
#   ┌────────────────────┐        ┌────────────────────┐
#   │   ergatai (CLI)    │─HTTP─►│ergatai-server (API)│
#   │  workspace/agent   │        │  MCP + HTTP server │
#   │  management        │        │                    │
#   └────────────────────┘        │  ┌─ fanotify ───┐  │  ← needs CAP_SYS_ADMIN
#                                  │  │ (file locks) │  │
#   rmux-daemon ◄── rmux SDK ──── │  └──────────────┘  │  ← auto-managed, bundled
#   nats-server ◄── embedded ──── │  (JetStream bus)   │  ← auto-managed, bundled
#                                  └────────────────────┘
#
# Arguments:
#   $1 — version tag (default: latest)
#   $2 — install directory (default: /usr/local/bin)

set -euo pipefail

VERSION="${1:-latest}"
INSTALL_DIR="${2:-/usr/local/bin}"
REPO="windreach/Ergatai"
CLI_NAME="ergatai"
SERVER_NAME="ergatai-server"

# ── Preflight checks ────────────────────────────────────────────────────────

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "⚠️  This installer is for Linux only."
    echo "   Ergatai on macOS/Windows runs in advisory-only mode (no kernel locking)."
    echo "   Download the binaries manually from https://github.com/${REPO}/releases"
    exit 1
fi

for cmd in curl getcap setcap; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "❌ Required command not found: $cmd"
        case "$cmd" in
            getcap|setcap)
                echo "   Install with: sudo apt-get install libcap2-bin  (Debian/Ubuntu)"
                echo "                 sudo yum install libcap  (RHEL/CentOS)"
                ;;
        esac
        exit 1
    fi
done

if [[ "$(uname -m)" == "x86_64" ]]; then
    ARCH="x86_64"
elif [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]]; then
    ARCH="aarch64"
else
    echo "❌ Unsupported architecture: $(uname -m)"
    exit 1
fi

# ── Resolve version ─────────────────────────────────────────────────────────

if [[ "$VERSION" == "latest" ]]; then
    echo "🔍 Resolving latest release..."
    VERSION=$(curl -sSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    if [[ -z "$VERSION" ]]; then
        echo "❌ Failed to resolve latest version. Try specifying explicitly:"
        echo "   curl -sSL ... | bash -s -- v0.1.0"
        exit 1
    fi
fi

echo "📦 Ergatai Installer"
echo "   Version:   $VERSION"
echo "   Arch:      $ARCH"
echo "   Install:   $INSTALL_DIR/"
echo ""
echo "   Binaries to install:"
echo "     • $CLI_NAME      (CLI — manages workspaces/agents, wraps rmux)"
echo "     • ega            (symlink → $CLI_NAME, short alias)"
echo "     • $SERVER_NAME  (server — MCP/HTTP API, needs CAP_SYS_ADMIN)"
echo ""

# ── Download ────────────────────────────────────────────────────────────────

TMP_CLI=$(mktemp)
TMP_SERVER=$(mktemp)
trap 'rm -f "$TMP_CLI" "$TMP_SERVER"' EXIT

for binary in "$CLI_NAME" "$SERVER_NAME"; do
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${binary}-${ARCH}"
    TMP_FILE=$(mktemp)
    echo "⬇️  Downloading $binary from $DOWNLOAD_URL"
    if ! curl -fL -o "$TMP_FILE" "$DOWNLOAD_URL"; then
        echo "❌ Download failed for $binary. Check:"
        echo "   - Version exists: https://github.com/${REPO}/releases"
        echo "   - Network connection"
        exit 1
    fi
    chmod +x "$TMP_FILE"
    if [[ "$binary" == "$CLI_NAME" ]]; then
        mv "$TMP_FILE" "$TMP_CLI"
    else
        mv "$TMP_FILE" "$TMP_SERVER"
    fi
done

# ── Install ─────────────────────────────────────────────────────────────────

echo "📁 Installing to $INSTALL_DIR"

install_binary() {
    local src="$1"
    local name="$2"
    if [[ -w "$INSTALL_DIR" ]]; then
        mv "$src" "$INSTALL_DIR/$name"
    else
        sudo mv "$src" "$INSTALL_DIR/$name"
    fi
}

install_binary "$TMP_CLI" "$CLI_NAME"
install_binary "$TMP_SERVER" "$SERVER_NAME"

# Create symlink: ega -> ergatai
CLI_TARGET="$INSTALL_DIR/$CLI_NAME"
EGA_TARGET="$INSTALL_DIR/ega"
if [[ -w "$INSTALL_DIR" ]]; then
    ln -sf "$CLI_TARGET" "$EGA_TARGET"
else
    sudo ln -sf "$CLI_TARGET" "$EGA_TARGET"
fi

echo "   ✓ $CLI_NAME     (CLI)"
echo "   ✓ ega           (symlink → $CLI_NAME)"
echo "   ✓ $SERVER_NAME  (server)"

# ── Grant CAP_SYS_ADMIN to SERVER only (REQUIRED for kernel-level file locking) ─

echo ""
echo "🔐 Granting CAP_SYS_ADMIN to $SERVER_NAME for fanotify..."
echo ""
echo "   ⚠️  This step requires sudo and is CRITICAL:"
echo "       WITHOUT this capability, file locking runs in ADVISORY MODE"
echo "       (locks can be bypassed by direct shell access)."
echo "   ℹ️  Note: only the SERVER needs this capability, not the CLI."
echo ""

SERVER_TARGET="$INSTALL_DIR/$SERVER_NAME"
if [[ -w "$INSTALL_DIR" ]]; then
    setcap 'cap_sys_admin+ep' "$SERVER_TARGET"
else
    sudo setcap 'cap_sys_admin+ep' "$SERVER_TARGET"
fi

# ── Verify ──────────────────────────────────────────────────────────────────

echo "🔍 Verifying installation..."
echo ""
echo "   CLI ($CLI_NAME):"
echo "     Path:         $INSTALL_DIR/$CLI_NAME"
if command -v "$CLI_NAME" &>/dev/null; then
    echo "     Version:      $("$CLI_NAME" --version 2>/dev/null || echo '<unknown>')"
else
    echo "     Version:      (not in PATH — re-open shell or source ~/.bashrc)"
fi

echo ""
echo "   Server ($SERVER_NAME):"
echo "     Path:         $SERVER_TARGET"
INSTALLED_CAPS=$(getcap "$SERVER_TARGET")
echo "     Capabilities: ${INSTALLED_CAPS:-<none>}"

echo ""
if [[ "$INSTALLED_CAPS" == *"cap_sys_admin"* ]]; then
    echo "✅ Installation successful!"
    echo ""
    echo "   File locking: MANDATORY (kernel-enforced via fanotify)"
    echo ""
    echo "   Start the server:"
    echo "     $SERVER_NAME --port 3000"
    echo ""
    echo "   Manage workspaces/agents (in another terminal):"
    echo "     $CLI_NAME workspace list"
    echo "     $CLI_NAME agent list"
    echo "     $CLI_NAME agent spawn --workspace <id> --command <cmd>"
    echo ""
    echo "   📖 Documentation: https://github.com/${REPO}#readme"
else
    echo "⚠️  WARNING: CAP_SYS_ADMIN not set on $SERVER_NAME!"
    echo ""
    echo "   File locking will run in ADVISORY MODE only."
    echo "   To enable mandatory locking, run manually:"
    echo ""
    echo "     sudo setcap 'cap_sys_admin+ep' $SERVER_TARGET"
    echo ""
    echo "   See: https://github.com/${REPO}#file-locking-permissions"
fi
