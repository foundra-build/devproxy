# Plan: Fix daemon setup flow

## Problem
The daemon setup flow (`devproxy init`) has multiple bugs discovered during end-to-end testing:
1. Port 443 needs sudo but init doesn't warn about it
2. Socket permissions issue when daemon runs as root (partially fixed)
3. Daemon dies silently after init reports success
4. Init output missing DNS setup commands, CA trust path, sudo requirement
5. `devproxy up` hangs when daemon is dead instead of failing fast
6. DNS setup not documented in init flow
7. CA trust needs sudo but fails silently
8. Stale daemon processes not cleaned up on re-init

## Changes

### 1. `src/commands/init.rs` — Major rework
- Kill stale daemon processes before spawning a new one (find by socket, send ping, kill PID)
- Remove stale socket file on re-init
- Wait briefly after daemon spawn and verify it's actually alive (connect to socket + ping)
- Report daemon startup failure clearly instead of "ok: daemon started" then silence
- Print complete DNS setup instructions (dnsmasq install + config + resolver setup for macOS)
- Print CA trust command with correct cert path
- Note that sudo is needed for port 443
- Pass `--port` info to next-steps output

### 2. `src/commands/up.rs` — Add IPC connection timeout
- Add a timeout (2s) to the daemon health check so `up` fails fast instead of hanging
- Improve error message to suggest re-running `devproxy init`

### 3. `src/ipc.rs` — Add connection timeout
- Add a `send_request_with_timeout` function (or add timeout to existing `send_request`)
- Default timeout of 3 seconds for IPC operations

### 4. `src/proxy/mod.rs` — Write PID file
- Write daemon PID to `<config_dir>/daemon.pid` on startup
- Clean up PID file in `SocketCleanupGuard` drop

### 5. `src/config.rs` — Add `pid_path()` helper
- Add `Config::pid_path()` returning `<config_dir>/daemon.pid`

### 6. E2E tests in `tests/e2e.rs`
- **test_init_output_includes_dns_instructions** — verify init stderr contains dnsmasq/resolver instructions
- **test_init_output_includes_sudo_note** — verify init mentions sudo for port 443
- **test_up_fails_fast_with_dead_daemon** — kill daemon, verify `up` fails within timeout (not hang)
- **test_reinit_kills_stale_daemon** — init twice, verify first daemon is killed
- **test_daemon_writes_pid_file** — verify PID file created after daemon start
- **test_init_output_includes_ca_trust_path** — verify CA cert path is printed

### 7. Unit tests
- **ipc::tests::send_request_timeout** — verify IPC timeout works against non-responsive socket

## File change list
- `src/commands/init.rs` — major rework
- `src/commands/up.rs` — add timeout to daemon check
- `src/ipc.rs` — add timeout support
- `src/proxy/mod.rs` — write PID file
- `src/config.rs` — add pid_path()
- `tests/e2e.rs` — add 6 new tests
- `docs/spec.md` — update init description with DNS/sudo info
