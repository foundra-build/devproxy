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

# --- Helper: build a harness script from install.sh ---
# Strips everything from the sentinel marker line onward, then appends custom code.
# This is more robust than matching a bare "main" line.
make_harness() {
    _harness_file="$1"
    # Guard: verify the sentinel marker exists in install.sh
    if ! grep -q '^# __DEVPROXY_INSTALL_MAIN__$' "$INSTALL_SCRIPT"; then
        echo "FATAL: install.sh is missing the # __DEVPROXY_INSTALL_MAIN__ sentinel marker" >&2
        exit 2
    fi
    sed '/^# __DEVPROXY_INSTALL_MAIN__$/,$d' "$INSTALL_SCRIPT" > "$_harness_file"
}

# --- Helper: extract detect_platform + construct_url and print TARGET/URL ---
run_detection() {
    _uname_dir="$1"
    _base_url="${2:-https://github.com/foundra-build/devproxy/releases}"
    _version="${3:-latest}"
    _harness="$TMPDIR_ROOT/harness-$$.sh"
    make_harness "$_harness"
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

# Helper for unsupported-platform tests: runs the harness in a subshell
# with set +e so the non-zero exit is captured rather than aborting.
run_detection_with_stderr() {
    _uname_dir="$1"
    _harness="$TMPDIR_ROOT/harness-unsup-$$.sh"
    make_harness "$_harness"
    cat >> "$_harness" <<'HARNESS'
detect_platform
HARNESS
    # Run in a subshell with set +e to capture the exit code properly
    # and ensure cleanup of the harness file on both success and failure.
    _rc=0
    PATH="$_uname_dir:$PATH" \
        DEVPROXY_INSTALL_BASE_URL="https://example.com" \
        DEVPROXY_VERSION="latest" \
        sh "$_harness" 2>&1 || _rc=$?
    rm -f "$_harness"
    return $_rc
}

# Unsupported OS
wrapper_dir="$(make_uname_wrapper "FreeBSD" "x86_64")"
if output="$(run_detection_with_stderr "$wrapper_dir")"; then
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
if output="$(run_detection_with_stderr "$wrapper_dir")"; then
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
# Determine the current platform's target triple for the mock binary filename
_mock_arch="$(uname -m | sed 's/arm64/aarch64/')"
case "$(uname -s)" in
    Darwin) _mock_os="apple-darwin" ;;
    Linux)  _mock_os="unknown-linux-gnu" ;;
esac
MOCK_BINARY="$MOCK_DIR/latest/download/devproxy-${_mock_arch}-${_mock_os}"
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
# Wait for mock server to be ready (retry up to 5 seconds)
_retries=0
while ! curl -s -o /dev/null "http://localhost:${MOCK_PORT}/" 2>/dev/null; do
    _retries=$((_retries + 1))
    if [ "$_retries" -ge 50 ]; then
        echo "FATAL: mock HTTP server failed to start on port $MOCK_PORT" >&2
        exit 2
    fi
    sleep 0.1
done

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
# Link only the essentials the script needs (sh, uname, mktemp, chmod, mkdir, mv, rm, cat, sed, grep, printf, echo, test, tr, cut)
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
# Need env
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
