# Plan: Add `devproxy restart` command

## Summary

Add a user-facing `devproxy restart` command that wraps the existing `platform::restart_daemon()` function. This provides users a direct way to restart the daemon without going through `devproxy init` or `devproxy update`.

## Changes

### 1. `src/cli.rs` — Add `Restart` variant to `Commands` enum
- Add `Restart` variant with doc comment `/// Restart the daemon`
- Add unit test verifying clap parses `["devproxy", "restart"]`

### 2. `src/commands/restart.rs` — New command module
- Call `platform::restart_daemon()`
- On `Ok(true)`: print success message
- On `Ok(false)`: print "no platform-managed daemon found" and exit 1
- On `Err(e)`: propagate error

### 3. `src/commands/mod.rs` — Register module
- Add `pub mod restart;`

### 4. `src/main.rs` — Dispatch command
- Add `Commands::Restart => commands::restart::run()` match arm

### 5. `tests/e2e.rs` — E2e tests
- `test_restart_no_daemon`: run restart with `DEVPROXY_NO_SOCKET_ACTIVATION=1`, verify non-zero exit and error message
- `test_restart_running_daemon`: start a daemon via the test harness, run restart, verify it reports "no platform-managed daemon" (because the test daemon is not platform-managed)
- Update `test_cli_help` to assert "restart" appears in help output

## Testing strategy

Medium fidelity. The command is a thin wrapper around `platform::restart_daemon()` which is already well-tested via `devproxy update`.

- **Unit**: clap parsing test for `Restart` variant
- **E2e**: two tests covering the no-daemon and running-but-not-platform-managed scenarios
- **E2e**: help output verification
