#!/usr/bin/env bash
# install.sh — Install ergatai-api binary with fanotify capabilities
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/windreach/Ergatai/main/install.sh | bash
#   curl -sSL ... | bash -s -- v0.1.0 /usr/local/bin
#
# What this does:
#   1. Downloads the ergatai-api binary from GitHub Releases
#   2. Installs it to the target directory (default: /usr/local/bin)
#   3. Grants CAP_SYS_ADMIN via setcap — REQUIRED for kernel-level file locking
#
# Why CAP_SYS_ADMIN?
#   Ergatai uses Linux fanotify with FAN_OPEN_PERM events to intercept file
#   open() syscalls at the VFS layer. This is the only way to enforce mandatory
#   file locks across agents. Permission events require CAP_SYS_ADMIN.
#   Without this capability, Ergatai falls back to advisory-only mode (locks
#   can be bypassed by direct shell access).
#
# Arguments:
#   $1 — version tag (default: latest)
#   $2 — install directory (default: /usr/local/bin)

set -euo pipefail

VERSION="${1:-latest}"
INSTALL_DIR="${2:-/usr/local/bin}"
REPO="windreach/Ergatai"
BINARY_NAME="ergatai-api"

# ── Preflight checks ────────────────────────────────────────────────────────

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "⚠️  This installer is for Linux only."
    echo "   Ergatai on macOS/Windows runs in advisory-only mode (no kernel locking)."
    echo "   Download the binary manually from https://github.com/${REPO}/releases"
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
echo "   Install:   $INSTALL_DIR/$BINARY_NAME"
echo ""

# ── Download ────────────────────────────────────────────────────────────────

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${BINARY_NAME}-${ARCH}"
TMP_FILE=$(mktemp)
trap 'rm -f "$TMP_FILE"' EXIT

echo "⬇️  Downloading $BINARY_NAME from $DOWNLOAD_URL"
if ! curl -fL -o "$TMP_FILE" "$DOWNLOAD_URL"; then
    echo "❌ Download failed. Check:"
    echo "   - Version exists: https://github.com/${REPO}/releases"
    echo "   - Network connection"
    exit 1
fi

chmod +x "$TMP_FILE"

# ── Install ─────────────────────────────────────────────────────────────────

echo "📁 Installing to $INSTALL_DIR/$BINARY_NAME"
if [[ -w "$INSTALL_DIR" ]]; then
    mv "$TMP_FILE" "$INSTALL_DIR/$BINARY_NAME"
else
    sudo mv "$TMP_FILE" "$INSTALL_DIR/$BINARY_NAME"
fi

# ── Grant CAP_SYS_ADMIN (REQUIRED for kernel-level file locking) ────────────

echo "🔐 Granting CAP_SYS_ADMIN for fanotify (kernel-level file locking)..."
echo ""
echo "   ⚠️  This step requires sudo and is CRITICAL:"
echo "       WITHOUT this capability, file locking runs in ADVISORY MODE"
echo "       (locks can be bypassed by direct shell access)."
echo ""

TARGET="$INSTALL_DIR/$BINARY_NAME"
if [[ -w "$INSTALL_DIR" ]]; then
    setcap 'cap_sys_admin+ep' "$TARGET"
else
    sudo setcap 'cap_sys_admin+ep' "$TARGET"
fi

# ── Verify ──────────────────────────────────────────────────────────────────

echo "🔍 Verifying installation..."
INSTALLED_CAPS=$(getcap "$TARGET")
echo "   Binary:     $TARGET"
echo "   Capabilities: ${INSTALLED_CAPS:-<none>}"
echo ""

if [[ "$INSTALLED_CAPS" == *"cap_sys_admin"* ]]; then
    echo "✅ Installation successful!"
    echo ""
    echo "   File locking: MANDATORY (kernel-enforced via fanotify)"
    echo ""
    echo "   Run with:"
    echo "     $TARGET --port 3000"
    echo ""
    echo "   📖 Documentation: https://github.com/${REPO}#readme"
else
    echo "⚠️  WARNING: CAP_SYS_ADMIN not set!"
    echo ""
    echo "   File locking will run in ADVISORY MODE only."
    echo "   To enable mandatory locking, run manually:"
    echo ""
    echo "     sudo setcap 'cap_sys_admin+ep' $TARGET"
    echo ""
    echo "   See: https://github.com/${REPO}#file-locking-permissions"
fi
