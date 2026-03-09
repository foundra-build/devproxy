# devproxy — Install Script Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Add a `curl -fsSL ... | sh` install script that downloads pre-built devproxy binaries from GitHub Releases, plus a comprehensive shell-based test suite.

**Architecture:** A single POSIX shell install script (`install.sh`) at the repo root that detects the user's OS/arch, constructs a GitHub Release download URL, downloads the appropriate binary, and installs it to a configurable location. A test script (`tests/test_install.sh`) exercises all code paths using uname overrides, a local HTTP mock server, and temp directories.

**Key design decisions:**
- The install script is POSIX sh (not bash) for maximum portability across macOS and Linux.
- Binary naming convention: `devproxy-<target-triple>` where triples are `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.
- `DEVPROXY_INSTALL_BASE_URL` env var overrides the default GitHub Releases base URL, enabling testing and mirror deployments.
- `DEVPROXY_INSTALL_DIR` env var overrides the default install directory (`/usr/local/bin`).
- The test suite uses Python's `http.server` as a lightweight mock HTTP server (available on all target platforms without extra dependencies).
- The install script prefers `curl` but falls back to `wget`.

---

## Task 1: Create the install script

**Files:**
- Create: `install.sh`

**Step 1: Write the install script**

Create `install.sh` at the repo root with the following content:

```sh
#!/bin/sh
set -eu

DEVPROXY_VERSION="${DEVPROXY_VERSION:-latest}"
DEVPROXY_INSTALL_DIR="${DEVPROXY_INSTALL_DIR:-/usr/local/bin}"
DEVPROXY_INSTALL_BASE_URL="${DEVPROXY_INSTALL_BASE_URL:-https://github.com/foundra-build/devproxy/releases}"

main() {
    detect_platform
    construct_url
    create_install_dir
    download_binary
    make_executable
    verify_installation
    echo "devproxy installed successfully to ${DEVPROXY_INSTALL_DIR}/devproxy"
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
        mkdir -p "$DEVPROXY_INSTALL_DIR"
    fi
}

download_binary() {
    TMPFILE="$(mktemp)"
    trap 'rm -f "$TMPFILE"' EXIT

    if command -v curl >/dev/null 2>&1; then
        HTTP_CODE=$(curl -fsSL -w '%{http_code}' -o "$TMPFILE" "$DOWNLOAD_URL" 2>/dev/null) || true
        if [ ! -s "$TMPFILE" ]; then
            echo "Error: failed to download devproxy from ${DOWNLOAD_URL}" >&2
            echo "HTTP status: ${HTTP_CODE:-unknown}" >&2
            exit 1
        fi
    elif command -v wget >/dev/null 2>&1; then
        if ! wget -q -O "$TMPFILE" "$DOWNLOAD_URL" 2>/dev/null; then
            echo "Error: failed to download devproxy from ${DOWNLOAD_URL}" >&2
            exit 1
        fi
    else
        echo "Error: neither curl nor wget found. Please install one and try again." >&2
        exit 1
    fi

    mv "$TMPFILE" "${DEVPROXY_INSTALL_DIR}/devproxy"
    trap - EXIT
}

make_executable() {
    chmod +x "${DEVPROXY_INSTALL_DIR}/devproxy"
}

verify_installation() {
    if [ ! -x "${DEVPROXY_INSTALL_DIR}/devproxy" ]; then
        echo "Error: installation failed — binary not found at ${DEVPROXY_INSTALL_DIR}/devproxy" >&2
        exit 1
    fi
}

main
```

**Step 2: Make it executable**

```bash
chmod +x install.sh
```

**Step 3: Verify syntax**

```bash
sh -n install.sh
```

Expected: No output (no syntax errors).

**Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: add curl|sh install script for devproxy"
```

---

## Task 2: Create the test script

**Files:**
- Create: `tests/test_install.sh`

**Step 1: Write the test script**

Create `tests/test_install.sh` with these test cases:

1. **OS/arch detection** — For each of the 4 platform combos (Darwin/arm64, Darwin/x86_64, Linux/x86_64, Linux/aarch64), override `uname` with a wrapper script that returns the expected values, run the install script in a mode that exits after detection, and verify the correct target triple.

2. **Unsupported platform error** — Override `uname` to return `FreeBSD`/`mips` and verify non-zero exit + error message on stderr.

3. **URL construction** — For each platform, verify the constructed download URL matches the expected pattern including the base URL override.

4. **Full install e2e** — Start a Python `http.server` serving a mock binary (a simple shell script that prints a version string), point `DEVPROXY_INSTALL_BASE_URL` at localhost, run the install script with `DEVPROXY_INSTALL_DIR` set to a temp dir, verify the binary exists, is executable, and produces expected output. Run a second time to verify idempotency.

5. **Download failure (404)** — Point at the mock server but request a URL that doesn't exist, verify non-zero exit + error message.

6. **Missing downloader** — Override `PATH` to exclude curl and wget, verify non-zero exit + error message about missing downloader.

The test script structure:

```sh
#!/bin/sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SCRIPT="$REPO_ROOT/install.sh"

PASS=0
FAIL=0
TOTAL=0

pass() {
    PASS=$((PASS + 1))
    TOTAL=$((TOTAL + 1))
    echo "  PASS: $1"
}

fail() {
    FAIL=$((FAIL + 1))
    TOTAL=$((TOTAL + 1))
    echo "  FAIL: $1"
    if [ -n "${2:-}" ]; then
        echo "        $2"
    fi
}

cleanup() {
    if [ -n "${MOCK_SERVER_PID:-}" ]; then
        kill "$MOCK_SERVER_PID" 2>/dev/null || true
        wait "$MOCK_SERVER_PID" 2>/dev/null || true
    fi
    if [ -n "${TMPDIR_ROOT:-}" ]; then
        rm -rf "$TMPDIR_ROOT"
    fi
}
trap cleanup EXIT

TMPDIR_ROOT="$(mktemp -d)"

# --- Helper: create a uname wrapper that returns custom OS/ARCH ---
make_uname_wrapper() {
    _os="$1"
    _arch="$2"
    _dir="$TMPDIR_ROOT/uname-wrapper-${_os}-${_arch}"
    mkdir -p "$_dir"
    cat > "$_dir/uname" <<WRAPPER
#!/bin/sh
case "\$1" in
    -s) echo "$_os" ;;
    -m) echo "$_arch" ;;
    *)  /usr/bin/uname "\$@" ;;
esac
WRAPPER
    chmod +x "$_dir/uname"
    echo "$_dir"
}

# --- Helper: extract detect_platform + construct_url and print TARGET/URL ---
# We source a modified version of install.sh that calls detect_platform + construct_url then prints
run_detection() {
    _uname_dir="$1"
    _base_url="${2:-https://github.com/foundra-build/devproxy/releases}"
    _version="${3:-latest}"
    # Create a test harness that sources the functions and calls them
    _harness="$TMPDIR_ROOT/harness-$$.sh"
    # Extract function definitions from install.sh, replace main call
    sed 's/^main$//' "$INSTALL_SCRIPT" > "$_harness"
    cat >> "$_harness" <<'HARNESS'
detect_platform
construct_url
echo "TARGET=$TARGET"
echo "DOWNLOAD_URL=$DOWNLOAD_URL"
HARNESS
    PATH="$_uname_dir:$PATH" \
        DEVPROXY_INSTALL_BASE_URL="$_base_url" \
        DEVPROXY_VERSION="$_version" \
        sh "$_harness" 2>/dev/null
    rm -f "$_harness"
}

# ============================================================
# Test 1: OS/arch detection — all 4 platform combos
# ============================================================
echo "=== Test 1: OS/arch detection ==="

for combo in "Darwin:arm64:aarch64-apple-darwin" \
             "Darwin:x86_64:x86_64-apple-darwin" \
             "Linux:x86_64:x86_64-unknown-linux-gnu" \
             "Linux:aarch64:aarch64-unknown-linux-gnu"; do
    os="$(echo "$combo" | cut -d: -f1)"
    arch="$(echo "$combo" | cut -d: -f2)"
    expected="$(echo "$combo" | cut -d: -f3)"

    wrapper_dir="$(make_uname_wrapper "$os" "$arch")"
    result="$(run_detection "$wrapper_dir" | grep '^TARGET=' | cut -d= -f2)"

    if [ "$result" = "$expected" ]; then
        pass "$os/$arch -> $expected"
    else
        fail "$os/$arch -> expected $expected, got $result"
    fi
done

# ============================================================
# Test 2: Unsupported platform error
# ============================================================
echo "=== Test 2: Unsupported platform error ==="

# Unsupported OS
wrapper_dir="$(make_uname_wrapper "FreeBSD" "x86_64")"
if output="$(run_detection "$wrapper_dir" 2>&1)"; then
    fail "FreeBSD should fail but exited 0"
else
    if echo "$output" | grep -qi "unsupported"; then
        pass "FreeBSD rejected with error message"
    else
        fail "FreeBSD rejected but no 'unsupported' in message" "$output"
    fi
fi

# Unsupported arch
wrapper_dir="$(make_uname_wrapper "Linux" "mips")"
if output="$(run_detection "$wrapper_dir" 2>&1)"; then
    fail "mips should fail but exited 0"
else
    if echo "$output" | grep -qi "unsupported"; then
        pass "mips rejected with error message"
    else
        fail "mips rejected but no 'unsupported' in message" "$output"
    fi
fi

# ============================================================
# Test 3: URL construction
# ============================================================
echo "=== Test 3: URL construction ==="

BASE="https://example.com/releases"

# Latest version
wrapper_dir="$(make_uname_wrapper "Darwin" "arm64")"
url="$(run_detection "$wrapper_dir" "$BASE" "latest" | grep '^DOWNLOAD_URL=' | cut -d= -f2-)"
expected_url="https://example.com/releases/latest/download/devproxy-aarch64-apple-darwin"
if [ "$url" = "$expected_url" ]; then
    pass "latest URL for Darwin/arm64"
else
    fail "latest URL: expected $expected_url, got $url"
fi

# Specific version
url="$(run_detection "$wrapper_dir" "$BASE" "v1.0.0" | grep '^DOWNLOAD_URL=' | cut -d= -f2-)"
expected_url="https://example.com/releases/download/v1.0.0/devproxy-aarch64-apple-darwin"
if [ "$url" = "$expected_url" ]; then
    pass "versioned URL for Darwin/arm64"
else
    fail "versioned URL: expected $expected_url, got $url"
fi

# Linux x86_64
wrapper_dir="$(make_uname_wrapper "Linux" "x86_64")"
url="$(run_detection "$wrapper_dir" "$BASE" "latest" | grep '^DOWNLOAD_URL=' | cut -d= -f2-)"
expected_url="https://example.com/releases/latest/download/devproxy-x86_64-unknown-linux-gnu"
if [ "$url" = "$expected_url" ]; then
    pass "latest URL for Linux/x86_64"
else
    fail "latest URL: expected $expected_url, got $url"
fi

# ============================================================
# Test 4: Full install e2e with mock server
# ============================================================
echo "=== Test 4: Full install e2e ==="

# Set up mock server directory structure
MOCK_DIR="$TMPDIR_ROOT/mock-server"
mkdir -p "$MOCK_DIR/latest/download"

# Create a mock binary (shell script that echoes version)
MOCK_BINARY="$MOCK_DIR/latest/download/devproxy-$(uname -m | sed 's/arm64/aarch64/')-$(case "$(uname -s)" in Darwin) echo apple-darwin;; Linux) echo unknown-linux-gnu;; esac)"
cat > "$MOCK_BINARY" <<'MOCKBIN'
#!/bin/sh
echo "devproxy mock 0.0.1-test"
MOCKBIN
chmod +x "$MOCK_BINARY"

# Start mock HTTP server
MOCK_PORT=0
# Find a free port
MOCK_PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()")
cd "$MOCK_DIR"
python3 -m http.server "$MOCK_PORT" >/dev/null 2>&1 &
MOCK_SERVER_PID=$!
cd "$REPO_ROOT"
# Give the server a moment to start
sleep 1

INSTALL_DIR="$TMPDIR_ROOT/install-target"
mkdir -p "$INSTALL_DIR"

# Run install
if DEVPROXY_INSTALL_BASE_URL="http://localhost:${MOCK_PORT}" \
   DEVPROXY_INSTALL_DIR="$INSTALL_DIR" \
   sh "$INSTALL_SCRIPT" >/dev/null 2>&1; then
    # Check binary exists and is executable
    if [ -x "$INSTALL_DIR/devproxy" ]; then
        pass "binary installed and executable"
    else
        fail "binary not found or not executable at $INSTALL_DIR/devproxy"
    fi

    # Check binary works
    mock_output="$("$INSTALL_DIR/devproxy" 2>&1 || true)"
    if echo "$mock_output" | grep -q "devproxy mock"; then
        pass "installed binary produces expected output"
    else
        fail "binary output unexpected" "$mock_output"
    fi

    # Idempotency: run again
    if DEVPROXY_INSTALL_BASE_URL="http://localhost:${MOCK_PORT}" \
       DEVPROXY_INSTALL_DIR="$INSTALL_DIR" \
       sh "$INSTALL_SCRIPT" >/dev/null 2>&1; then
        pass "idempotent reinstall succeeds"
    else
        fail "idempotent reinstall failed"
    fi
else
    fail "install script failed"
fi

# ============================================================
# Test 5: Download failure (404)
# ============================================================
echo "=== Test 5: Download failure (404) ==="

INSTALL_DIR_404="$TMPDIR_ROOT/install-404"
mkdir -p "$INSTALL_DIR_404"

# Point at a path that doesn't exist on the mock server
wrapper_dir="$(make_uname_wrapper "Linux" "aarch64")"
if output="$(PATH="$wrapper_dir:$PATH" \
   DEVPROXY_INSTALL_BASE_URL="http://localhost:${MOCK_PORT}/nonexistent" \
   DEVPROXY_INSTALL_DIR="$INSTALL_DIR_404" \
   sh "$INSTALL_SCRIPT" 2>&1)"; then
    fail "404 should cause non-zero exit"
else
    if echo "$output" | grep -qi "error\|fail"; then
        pass "404 produces error message"
    else
        fail "404 exited non-zero but no error in output" "$output"
    fi
fi

# ============================================================
# Test 6: Missing downloader
# ============================================================
echo "=== Test 6: Missing downloader ==="

INSTALL_DIR_NODL="$TMPDIR_ROOT/install-nodl"
mkdir -p "$INSTALL_DIR_NODL"

# Create a minimal PATH with only essential commands but no curl/wget
MINIMAL_BIN="$TMPDIR_ROOT/minimal-bin"
mkdir -p "$MINIMAL_BIN"
# Link only the essentials the script needs (sh, uname, mktemp, chmod, mkdir, etc.)
for cmd in sh uname mktemp chmod mkdir mv rm cat sed grep printf echo test tr cut; do
    cmd_path="$(command -v "$cmd" 2>/dev/null || true)"
    if [ -n "$cmd_path" ]; then
        ln -sf "$cmd_path" "$MINIMAL_BIN/$cmd" 2>/dev/null || true
    fi
done
# Also need [ for test
if [ -f /bin/[ ]; then
    ln -sf /bin/[ "$MINIMAL_BIN/[" 2>/dev/null || true
fi
# Need env and python3 is not needed here
ln -sf "$(command -v env)" "$MINIMAL_BIN/env" 2>/dev/null || true

if output="$(PATH="$MINIMAL_BIN" \
   DEVPROXY_INSTALL_BASE_URL="http://localhost:${MOCK_PORT}" \
   DEVPROXY_INSTALL_DIR="$INSTALL_DIR_NODL" \
   sh "$INSTALL_SCRIPT" 2>&1)"; then
    fail "missing downloader should cause non-zero exit"
else
    if echo "$output" | grep -qi "curl\|wget"; then
        pass "missing downloader error mentions curl/wget"
    else
        fail "missing downloader exited non-zero but no curl/wget mention" "$output"
    fi
fi

# ============================================================
# Summary
# ============================================================
echo ""
echo "============================================================"
echo "Results: $PASS passed, $FAIL failed, $TOTAL total"
echo "============================================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
```

**Step 2: Make it executable**

```bash
chmod +x tests/test_install.sh
```

**Step 3: Run the tests**

```bash
sh tests/test_install.sh
```

Expected: All tests pass (the e2e test uses the real platform's uname for the mock binary name).

**Step 4: Commit**

```bash
git add tests/test_install.sh
git commit -m "test: add shell-based install script test suite"
```

---

## Task 3: Add just recipe and update README

**Files:**
- Modify: `justfile`
- Modify: `README.md`

**Step 1: Add `test-install` recipe to justfile**

Add after the `e2e` recipe:

```just
# Run install script tests
test-install:
    sh tests/test_install.sh
```

**Step 2: Update README with install instructions**

Add an "Install" section before "Quick Start" with:

```markdown
## Install

```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```
```

**Step 3: Verify just recipe works**

```bash
just test-install
```

Expected: All tests pass.

**Step 4: Commit**

```bash
git add justfile README.md
git commit -m "docs: add install command to README and test-install just recipe"
```

---

## Task 4: Final verification

**Step 1: Run full test suite**

```bash
just test-install
```

Expected: All 6 test categories pass, output shows pass/fail summary.

**Step 2: Run existing project checks**

```bash
just check
```

Expected: Existing clippy + tests still pass (no Rust code changed).

**Step 3: Verify install script syntax on sh**

```bash
sh -n install.sh
```

Expected: No errors.
