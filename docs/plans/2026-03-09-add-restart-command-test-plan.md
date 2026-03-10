# Test Plan: `devproxy restart` command

## Strategy reconciliation

The implementation plan describes a thin wrapper around `platform::restart_daemon()` with three touchpoints: CLI parsing (`cli.rs`), command dispatch (`main.rs`), and the command module (`commands/restart.rs`). The agreed medium-fidelity strategy assumed exactly this shape and holds without modification. No external dependencies, no new harnesses needed.

## Test plan

### 1. `test_restart_no_daemon` — restart reports error when no platform daemon exists

- **Name**: Restart without a platform-managed daemon reports error and exits non-zero
- **Type**: scenario
- **Harness**: Direct binary invocation with `DEVPROXY_NO_SOCKET_ACTIVATION=1`
- **Preconditions**: No platform-managed daemon (launchd/systemd) is running. Socket activation is disabled via env var. A valid config dir with `config.json` exists.
- **Actions**: Run `devproxy restart` with isolated config dir and `DEVPROXY_NO_SOCKET_ACTIVATION=1`.
- **Expected outcome**: Exit code is non-zero. Stderr contains "no platform-managed daemon found". Source of truth: `commands/restart.rs` prints this message when `restart_daemon()` returns `Ok(false)`, and the implementation plan specifies this behavior.
- **Interactions**: `platform::restart_daemon()` checks env var and returns `Ok(false)` without touching launchd/systemd.

### 2. `test_restart_running_daemon` — restart reports error when daemon is running but not platform-managed

- **Name**: Restart with a running non-platform daemon still reports no platform daemon
- **Type**: scenario
- **Harness**: Test daemon harness (existing `start_test_daemon`) + direct binary invocation
- **Preconditions**: A daemon is running on an ephemeral port via direct spawn (not launchd/systemd). `DEVPROXY_NO_SOCKET_ACTIVATION=1` is set.
- **Actions**: Start a test daemon. Run `devproxy restart` against the same config dir.
- **Expected outcome**: Exit code is non-zero. Stderr contains "no platform-managed daemon found". The running daemon is unaffected. Source of truth: implementation plan states "run restart, verify it reports 'no platform-managed daemon'" for this scenario.
- **Interactions**: Tests that `restart` does not accidentally kill a non-platform-managed daemon process.

### 3. `test_parse_restart_command` — CLI parses "restart" subcommand

- **Name**: `devproxy restart` parses as the Restart command variant
- **Type**: unit
- **Harness**: In-process clap parsing via `Cli::try_parse_from`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from(["devproxy", "restart"])`.
- **Expected outcome**: Parsing succeeds. Returned `Commands` variant matches `Commands::Restart`. Source of truth: implementation plan specifies `Restart` variant with no arguments.
- **Interactions**: None.

### 4. `test_cli_help` — help output includes restart (existing test, extended)

- **Name**: Help output lists the restart command
- **Type**: boundary
- **Harness**: Direct binary invocation
- **Preconditions**: Binary is built.
- **Actions**: Run `devproxy --help`.
- **Expected outcome**: Stdout contains "restart". Source of truth: the `Restart` variant in `Commands` enum has no `#[command(hide = true)]` attribute, so clap will list it.
- **Interactions**: None beyond clap help generation.

## Coverage summary

**Covered:**
- User-visible restart command (both success-path messaging and error-path messaging)
- CLI parsing of the `restart` subcommand
- Behavior when daemon is running but not platform-managed
- Behavior when no daemon exists at all
- Discoverability via `--help`

**Explicitly excluded per agreed strategy:**
- Actual launchd/systemd restart (would require platform-managed daemon installation, which is impractical in CI and covered by the existing `platform::restart_daemon()` unit-level logic)
- Testing on Linux (systemd path) -- covered by existing `tests/linux-docker/` infrastructure for the `update` command which exercises the same `restart_daemon()` codepath

**Residual risk:**
- Low. The command is a direct delegation to `platform::restart_daemon()` which is already exercised by the `update` command's e2e tests. The only new code is the 20-line `commands/restart.rs` wrapper.
