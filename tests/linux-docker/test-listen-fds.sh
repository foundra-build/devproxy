#!/bin/bash
# tests/linux-docker/test-listen-fds.sh
# Tests the LISTEN_FDS protocol directly (simulating systemd socket activation).
# Run inside the Docker container as testuser.

set -euo pipefail

echo "=== Test: LISTEN_FDS protocol ==="

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-listen-fds"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Generate certs
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Use Python to pre-bind a socket and pass fd 3 to the daemon.
# Python's socket module makes this straightforward.
python3 -c "
import socket, os, subprocess, sys, time

# Bind a TCP socket on an ephemeral port
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.bind(('127.0.0.1', 0))
sock.listen(128)
port = sock.getsockname()[1]
print(f'Pre-bound port: {port}')

# dup2 the socket fd to fd 3 (SD_LISTEN_FDS_START)
fd = sock.fileno()
if fd != 3:
    os.dup2(fd, 3)
    sock.close()  # close the original fd; fd 3 is now the socket

# Clear CLOEXEC on fd 3 so it survives exec
import fcntl
flags = fcntl.fcntl(3, fcntl.F_GETFD)
fcntl.fcntl(3, fcntl.F_SETFD, flags & ~fcntl.FD_CLOEXEC)

# LISTEN_PID must match getpid() in the daemon process.
# We can't set it from Python (the child gets a new PID after fork),
# so we use a shell wrapper that sets LISTEN_PID=\$\$ (its own PID).
env = os.environ.copy()
env['LISTEN_FDS'] = '1'
env['DEVPROXY_CONFIG_DIR'] = os.environ['DEVPROXY_CONFIG_DIR']

import tempfile
wrapper = tempfile.NamedTemporaryFile(mode='w', suffix='.sh', delete=False)
wrapper.write(f'''#!/bin/bash
export LISTEN_PID=\$\$
exec devproxy daemon --port {port}
''')
wrapper.close()
os.chmod(wrapper.name, 0o755)

proc = subprocess.Popen(
    [wrapper.name],
    env=env,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.PIPE,
    close_fds=False,
)

# Wait for IPC socket
socket_path = os.path.join(os.environ['DEVPROXY_CONFIG_DIR'], 'devproxy.sock')
for i in range(50):
    if os.path.exists(socket_path):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(socket_path)
            s.close()
            print('Daemon started successfully with LISTEN_FDS')
            break
        except:
            pass
    time.sleep(0.1)
else:
    stderr = proc.stderr.read().decode() if proc.stderr else ''
    print(f'FAIL: daemon did not start. stderr: {stderr}')
    proc.kill()
    os.unlink(wrapper.name)
    sys.exit(1)

# Verify via status command
result = subprocess.run(['devproxy', 'status'], capture_output=True, text=True,
                       env=env)
if 'running' in result.stderr:
    print('PASS: daemon running with activated socket')
else:
    print(f'FAIL: status output: {result.stderr}')
    proc.kill()
    os.unlink(wrapper.name)
    sys.exit(1)

proc.terminate()
proc.wait()
os.unlink(wrapper.name)
print('PASS: LISTEN_FDS test complete')
"

rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: LISTEN_FDS test complete ==="
