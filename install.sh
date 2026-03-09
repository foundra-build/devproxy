#!/bin/sh
set -eu

DEVPROXY_VERSION="${DEVPROXY_VERSION:-latest}"
DEVPROXY_INSTALL_DIR="${DEVPROXY_INSTALL_DIR:-${HOME}/.local/bin}"
DEVPROXY_INSTALL_BASE_URL="${DEVPROXY_INSTALL_BASE_URL:-https://github.com/foundra-build/devproxy/releases}"

main() {
    detect_platform
    construct_url
    create_install_dir
    download_binary
    verify_installation
    echo "devproxy installed successfully to ${DEVPROXY_INSTALL_DIR}/devproxy"
    case ":${PATH}:" in
        *":${DEVPROXY_INSTALL_DIR}:"*) ;;
        *) echo "Note: ${DEVPROXY_INSTALL_DIR} is not in your PATH. Add it with:" >&2
           echo "  export PATH=\"${DEVPROXY_INSTALL_DIR}:\$PATH\"" >&2 ;;
    esac
}

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin) OS_TARGET="apple-darwin" ;;
        Linux)  OS_TARGET="unknown-linux-gnu" ;;
        *)      echo "Error: unsupported operating system: $OS" >&2; exit 1 ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH_TARGET="x86_64" ;;
        aarch64|arm64) ARCH_TARGET="aarch64" ;;
        *)             echo "Error: unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac

    TARGET="${ARCH_TARGET}-${OS_TARGET}"
}

construct_url() {
    BINARY_NAME="devproxy-${TARGET}"
    if [ "$DEVPROXY_VERSION" = "latest" ]; then
        DOWNLOAD_URL="${DEVPROXY_INSTALL_BASE_URL}/latest/download/${BINARY_NAME}"
    else
        DOWNLOAD_URL="${DEVPROXY_INSTALL_BASE_URL}/download/${DEVPROXY_VERSION}/${BINARY_NAME}"
    fi
}

create_install_dir() {
    if [ ! -d "$DEVPROXY_INSTALL_DIR" ]; then
        if ! mkdir -p "$DEVPROXY_INSTALL_DIR" 2>/dev/null; then
            echo "Error: failed to create install directory ${DEVPROXY_INSTALL_DIR}" >&2
            echo "Try running with sudo or set DEVPROXY_INSTALL_DIR to a writable location." >&2
            exit 1
        fi
    elif [ ! -w "$DEVPROXY_INSTALL_DIR" ]; then
        echo "Error: install directory ${DEVPROXY_INSTALL_DIR} is not writable" >&2
        echo "Try running with sudo or set DEVPROXY_INSTALL_DIR to a writable location." >&2
        exit 1
    fi
}

download_binary() {
    TMPFILE="$(mktemp)"
    trap 'rm -f "$TMPFILE"' EXIT

    if command -v curl >/dev/null 2>&1; then
        if ! curl -fsSL -o "$TMPFILE" "$DOWNLOAD_URL"; then
            echo "Error: failed to download devproxy from ${DOWNLOAD_URL}" >&2
            exit 1
        fi
    elif command -v wget >/dev/null 2>&1; then
        if ! wget -q -O "$TMPFILE" "$DOWNLOAD_URL"; then
            echo "Error: failed to download devproxy from ${DOWNLOAD_URL}" >&2
            exit 1
        fi
    else
        echo "Error: neither curl nor wget found. Please install one and try again." >&2
        exit 1
    fi

    # Copy and set permissions using only POSIX-guaranteed commands.
    # chmod before cp would not help since cp creates a new inode;
    # instead we cp then chmod, keeping the window minimal.
    if ! cp "$TMPFILE" "${DEVPROXY_INSTALL_DIR}/devproxy"; then
        echo "Error: failed to copy binary to ${DEVPROXY_INSTALL_DIR}/devproxy" >&2
        exit 1
    fi
    if ! chmod 755 "${DEVPROXY_INSTALL_DIR}/devproxy"; then
        echo "Error: failed to set executable permissions on ${DEVPROXY_INSTALL_DIR}/devproxy" >&2
        exit 1
    fi
    # On macOS, clear quarantine attributes and ad-hoc sign the binary
    # to prevent Gatekeeper from killing it on first run.
    # These are best-effort: if they fail (e.g., signing tools not available),
    # the binary is still installed and may work; warn but do not abort.
    if [ "$(uname -s)" = "Darwin" ]; then
        xattr -cr "${DEVPROXY_INSTALL_DIR}/devproxy" 2>/dev/null || true
        if ! codesign --force --sign - "${DEVPROXY_INSTALL_DIR}/devproxy" 2>/dev/null; then
            echo "Warning: failed to ad-hoc sign binary; Gatekeeper may kill the binary on first run" >&2
        fi
    fi
    rm -f "$TMPFILE"
    trap - EXIT
}

verify_installation() {
    if [ ! -x "${DEVPROXY_INSTALL_DIR}/devproxy" ]; then
        echo "Error: installation failed — binary not found at ${DEVPROXY_INSTALL_DIR}/devproxy" >&2
        exit 1
    fi
}

# __DEVPROXY_INSTALL_MAIN__
main
