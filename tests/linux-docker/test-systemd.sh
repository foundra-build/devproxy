#!/bin/bash
# tests/linux-docker/test-systemd.sh
# Tests the systemd socket activation path.
# Run inside the Docker container as testuser.
# Requires: XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS set by caller.

set -euo pipefail

echo "=== Test: systemd socket activation ==="

# Verify user session prerequisites
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
    echo "FAIL: XDG_RUNTIME_DIR not set (required for systemctl --user)"
    exit 1
fi
if ! systemctl --user is-active default.target >/dev/null 2>&1; then
    echo "FAIL: systemd user manager not running. Is lingering enabled?"
    systemctl --user status 2>&1 || true
    exit 1
fi

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-systemd"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Init with certs only (--no-daemon)
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Install daemon via init (should use systemd path)
INIT_OUTPUT=$(devproxy init --domain test.dev --port 8443 2>&1 || true)
echo "$INIT_OUTPUT"

# Wait for daemon
sleep 3

# Check status
STATUS=$(devproxy status 2>&1 || true)
echo "Status: $STATUS"

if echo "$STATUS" | grep -q "running"; then
    echo "PASS: daemon is running via systemd"
else
    echo "FAIL: daemon not running"
    systemctl --user status devproxy.socket 2>&1 || true
    systemctl --user status devproxy.service 2>&1 || true
    journalctl --user -u devproxy --no-pager -n 20 2>&1 || true
    exit 1
fi

# Verify systemd unit files were created
UNIT_DIR="$HOME/.config/systemd/user"
if [ -f "$UNIT_DIR/devproxy.socket" ]; then
    echo "PASS: socket unit exists"
    # Verify localhost binding
    if grep -q "ListenStream=127.0.0.1:8443" "$UNIT_DIR/devproxy.socket"; then
        echo "PASS: socket binds to localhost only"
    else
        echo "FAIL: socket unit does not bind to localhost"
        cat "$UNIT_DIR/devproxy.socket"
        exit 1
    fi
else
    echo "FAIL: socket unit not created"
    exit 1
fi

# Cleanup
systemctl --user stop devproxy.socket devproxy.service 2>/dev/null || true
systemctl --user disable devproxy.socket 2>/dev/null || true
rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: systemd test complete ==="
