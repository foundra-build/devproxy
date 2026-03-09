#!/bin/bash
# tests/linux-docker/run-tests.sh
# Builds and runs the Linux integration tests in Docker.
# Run from the repo root: bash tests/linux-docker/run-tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "Building Linux test container..."
docker build -t devproxy-linux-test -f "$SCRIPT_DIR/Dockerfile" "$REPO_ROOT"

echo "Starting container with systemd..."
CONTAINER_ID=$(docker run -d --privileged \
    --tmpfs /run --tmpfs /run/lock \
    -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    devproxy-linux-test)

# Wait for systemd to fully initialize (system + user manager)
echo "Waiting for systemd to initialize..."
for i in $(seq 1 30); do
    if docker exec "$CONTAINER_ID" systemctl is-system-running 2>/dev/null | grep -q "running\|degraded"; then
        break
    fi
    sleep 1
done

# Wait for testuser's user manager to start (enabled via lingering).
# This is required for `systemctl --user` to work.
echo "Waiting for testuser's systemd user manager..."
for i in $(seq 1 15); do
    if docker exec "$CONTAINER_ID" systemctl is-active user@1000.service 2>/dev/null | grep -q "active"; then
        break
    fi
    sleep 1
done

# Helper: run a command as testuser with proper user session environment.
# XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS are required for
# `systemctl --user` to communicate with the user's systemd manager.
run_as_testuser() {
    docker exec -u testuser \
        -e XDG_RUNTIME_DIR=/run/user/1000 \
        -e DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
        "$CONTAINER_ID" bash "$@"
}

FAILED=0

echo ""
echo "=========================================="
echo "Running LISTEN_FDS test..."
echo "=========================================="
if run_as_testuser /tests/test-listen-fds.sh; then
    echo ">>> LISTEN_FDS: PASS"
else
    echo ">>> LISTEN_FDS: FAIL"
    FAILED=1
fi

echo ""
echo "=========================================="
echo "Running systemd test..."
echo "=========================================="
if run_as_testuser /tests/test-systemd.sh; then
    echo ">>> systemd: PASS"
else
    echo ">>> systemd: FAIL"
    FAILED=1
fi

echo ""
echo "=========================================="
echo "Running setcap test..."
echo "=========================================="
if docker exec "$CONTAINER_ID" bash /tests/test-setcap.sh; then
    echo ">>> setcap: PASS"
else
    echo ">>> setcap: FAIL"
    FAILED=1
fi

# Cleanup
docker stop "$CONTAINER_ID" >/dev/null 2>&1
docker rm "$CONTAINER_ID" >/dev/null 2>&1

echo ""
if [ $FAILED -eq 0 ]; then
    echo "All Linux integration tests PASSED"
else
    echo "Some Linux integration tests FAILED"
    exit 1
fi
