#!/bin/bash
# tests/linux-docker/test-setcap.sh
# Tests the setcap fallback path (when systemd is not available).
# Run inside the Docker container as root (setcap requires root).

set -euo pipefail

echo "=== Test: setcap fallback ==="

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-setcap"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Generate certs
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Copy binary so we can setcap it without affecting other tests
cp /usr/local/bin/devproxy /tmp/devproxy-setcap-test
chmod 755 /tmp/devproxy-setcap-test

# Apply setcap
setcap cap_net_bind_service=+ep /tmp/devproxy-setcap-test

# Verify capability is set
CAPS=$(getcap /tmp/devproxy-setcap-test)
echo "Capabilities: $CAPS"
if echo "$CAPS" | grep -q "cap_net_bind_service"; then
    echo "PASS: capability applied"
else
    echo "FAIL: capability not applied"
    exit 1
fi

# Start daemon on port 443 as non-root user with the setcap binary
# (This proves setcap allows binding port 443 without root)
su testuser -c "
    export DEVPROXY_CONFIG_DIR='$DEVPROXY_CONFIG_DIR'
    export DEVPROXY_NO_SOCKET_ACTIVATION=1
    /tmp/devproxy-setcap-test daemon --port 443 &
    DAEMON_PID=\$!
    sleep 2

    STATUS=\$(/tmp/devproxy-setcap-test status 2>&1 || true)
    echo \"Status: \$STATUS\"
    if echo \"\$STATUS\" | grep -q 'running'; then
        echo 'PASS: daemon running on port 443 as non-root (setcap)'
    else
        echo 'FAIL: daemon not running on port 443'
        kill \$DAEMON_PID 2>/dev/null || true
        exit 1
    fi

    kill \$DAEMON_PID 2>/dev/null || true
    wait \$DAEMON_PID 2>/dev/null || true
"

rm -f /tmp/devproxy-setcap-test
rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: setcap test complete ==="
