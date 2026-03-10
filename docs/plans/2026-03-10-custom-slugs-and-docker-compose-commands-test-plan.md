# Test Plan: Custom Slugs & Docker Compose Command Parity

## Strategy reconciliation

The implementation plan describes changes across six chunks: slug validation (config.rs), CLI restructuring (cli.rs, main.rs), command implementations (up.rs, stop.rs, start.rs, restart.rs, daemon.rs), platform template updates (platform.rs), e2e test updates (e2e.rs), and documentation/version bumps.

The testing strategy embedded in the plan holds without modification. Key observations:

- **Harnesses match**: All tests use either in-process unit testing (clap parsing, slug validation) or the existing e2e harness (binary invocation with `DEVPROXY_CONFIG_DIR` isolation). No new harnesses are needed.
- **External dependencies**: Docker/Docker Compose is required only for `#[ignore]`-marked tests, consistent with the existing pattern. No paid APIs or new infrastructure.
- **Interaction surface**: The `stop`/`start`/`restart` commands delegate to `docker compose` subprocesses, which the daemon's event watcher already handles. No new daemon IPC or protocol changes.
- **One area the plan doesn't test explicitly**: the `up` command's reuse behavior (running `up` twice, or `up` after `stop`). These are Docker-dependent scenarios and belong in the `#[ignore]` tier, which is appropriate given the existing pattern.

No strategy changes requiring user approval.

## Harness requirements

No new harnesses needed. Existing infrastructure is sufficient:

- **Unit test harness**: `#[cfg(test)]` modules in `config.rs`, `cli.rs`, `platform.rs` for in-process testing
- **E2E binary harness**: `tests/e2e.rs` with `devproxy_bin()`, `copy_fixtures()`, `create_test_config_dir()`, `start_test_daemon()`, `DaemonGuard`, `ComposeGuard`

## Test plan

### 1. Full e2e workflow with `--slug` flag: up with custom slug produces predictable URL

- **Name**: Running `up --slug dirty-panda` produces a URL with the custom slug prefix
- **Type**: scenario
- **Harness**: E2E binary invocation with Docker (`#[ignore]`)
- **Preconditions**: Test config dir with certs generated. Test daemon running on ephemeral port. Fixture compose project with `devproxy.port` label copied to temp dir.
- **Actions**:
  1. Run `devproxy up --slug dirty-panda` in the fixture directory
  2. Extract slug from output URL
  3. Verify `.devproxy-project` contains the slug
  4. Verify the URL contains `dirty-panda-e2e-fixture`
  5. Run `devproxy down` to clean up
- **Expected outcome**: The output URL is `https://dirty-panda-e2e-fixture.{domain}`. The `.devproxy-project` file contains `dirty-panda-e2e-fixture`. Source of truth: design spec section "Usage" shows `{custom_slug}-{app_name}` format. `compose_slug()` in `config.rs` joins as `{slug_prefix}-{app_name}`.
- **Interactions**: Docker Compose, daemon event watcher (route insertion on container start).

### 2. `up` reuses existing slug and ignores `--slug` on second run

- **Name**: Running `up --slug new-name` on an already-configured project reuses the existing slug with a warning
- **Type**: scenario
- **Harness**: E2E binary invocation with Docker (`#[ignore]`)
- **Preconditions**: Same as test 1. First `up --slug dirty-panda` has already run and containers are up.
- **Actions**:
  1. Run `devproxy up --slug new-name` in the same fixture directory
  2. Capture stderr
  3. Read `.devproxy-project`
- **Expected outcome**: Stderr contains "ignoring --slug" or "reusing existing slug". The `.devproxy-project` file still contains the original slug (`dirty-panda-e2e-fixture`), not `new-name-e2e-fixture`. Source of truth: design spec "Reuse Behavior" section.
- **Interactions**: Docker Compose (re-runs `up -d` with existing config).

### 3. Stop preserves slug and override, start resumes with same URL

- **Name**: `stop` pauses containers without removing state files; `start` resumes them
- **Type**: scenario
- **Harness**: E2E binary invocation with Docker (`#[ignore]`)
- **Preconditions**: Project is running via `devproxy up`.
- **Actions**:
  1. Run `devproxy stop` in the fixture directory
  2. Verify `.devproxy-project` and `.devproxy-override.yml` still exist
  3. Run `devproxy start` in the fixture directory
  4. Verify output contains the same URL as the original `up`
- **Expected outcome**: After `stop`, both state files remain. `start` succeeds and prints the same URL. Source of truth: design spec command table shows `stop` "Preserves both" and `start` "Requires both to exist".
- **Interactions**: Docker Compose stop/start, daemon event watcher (route removal on stop, re-insertion on start).

### 4. `daemon restart` reports no platform daemon when socket activation is disabled

- **Name**: `daemon restart` without a platform-managed daemon reports error and exits non-zero
- **Type**: scenario
- **Harness**: E2E binary invocation with `DEVPROXY_NO_SOCKET_ACTIVATION=1`
- **Preconditions**: No platform-managed daemon. Socket activation disabled via env var. Valid config dir with `config.json`.
- **Actions**: Run `devproxy daemon restart` with isolated config dir.
- **Expected outcome**: Exit code is non-zero. Stderr contains "no platform-managed daemon found". Source of truth: `commands/daemon.rs::restart()` delegates to `platform::restart_daemon()` which returns `Ok(false)` when env var is set.
- **Interactions**: `platform::restart_daemon()` checks env var and short-circuits.

### 5. `daemon restart` reports no platform daemon even when a direct-spawn daemon is running

- **Name**: `daemon restart` with a running non-platform daemon still reports no platform daemon
- **Type**: scenario
- **Harness**: Test daemon harness (`start_test_daemon`) + binary invocation
- **Preconditions**: A daemon is running on an ephemeral port via direct spawn. `DEVPROXY_NO_SOCKET_ACTIVATION=1` is set.
- **Actions**: Start a test daemon. Run `devproxy daemon restart` against the same config dir.
- **Expected outcome**: Exit code is non-zero. Stderr contains "no platform-managed daemon found". Source of truth: same as test 4.
- **Interactions**: Verifies `daemon restart` does not accidentally affect a non-platform daemon.

### 6. Help output lists new commands (`stop`, `start`, `daemon`) and keeps `restart` visible

- **Name**: `--help` lists all new commands and the daemon subcommand group
- **Type**: integration
- **Harness**: E2E binary invocation
- **Preconditions**: Binary is built.
- **Actions**: Run `devproxy --help`.
- **Expected outcome**: Stdout contains "stop", "start", "restart", "daemon". The "daemon" line is visible (not hidden). Source of truth: CLI definition in `cli.rs` -- `Stop`, `Start`, `Restart` have no `hide` attribute, and `Daemon` variant has no `hide` attribute (only its `Run` subcommand is hidden).
- **Interactions**: clap help generation.

### 7. Platform plist includes `daemon run` subcommand in ProgramArguments

- **Name**: Generated LaunchAgent plist invokes `daemon run --port` not `daemon --port`
- **Type**: integration
- **Harness**: In-process unit test in `platform.rs`
- **Preconditions**: None.
- **Actions**: Call `generate_launchagent_plist("/usr/local/bin/devproxy", 443, None)`.
- **Expected outcome**: The returned plist contains `<string>run</string>` between `<string>daemon</string>` and `<string>--port</string>`. Source of truth: implementation plan Task 7 Step 3.
- **Interactions**: None.

### 8. Platform systemd service unit includes `daemon run` subcommand in ExecStart

- **Name**: Generated systemd service unit invokes `daemon run --port` not `daemon --port`
- **Type**: integration
- **Harness**: In-process unit test in `platform.rs`
- **Preconditions**: None.
- **Actions**: Call `generate_systemd_service_unit("/usr/local/bin/devproxy", 443, None)`.
- **Expected outcome**: The ExecStart line contains `daemon run --port 443`. Source of truth: implementation plan Task 7 Step 4.
- **Interactions**: None.

### 9. Systemd service unit with custom port uses `daemon run --port`

- **Name**: Generated systemd service unit with port 8443 uses `daemon run --port 8443`
- **Type**: boundary
- **Harness**: In-process unit test in `platform.rs`
- **Preconditions**: None.
- **Actions**: Call `generate_systemd_service_unit("/usr/local/bin/devproxy", 8443, None)`.
- **Expected outcome**: The ExecStart line contains `daemon run --port 8443`. Source of truth: implementation plan Task 7 Step 1.
- **Interactions**: None.

### 10. `start_test_daemon` helper uses `daemon run` subcommand

- **Name**: E2E test daemon helper starts daemon with `daemon run --port` command
- **Type**: integration
- **Harness**: E2E test infrastructure (verified by all `#[ignore]` tests passing)
- **Preconditions**: Binary is built.
- **Actions**: Any `#[ignore]` test that calls `start_test_daemon()`.
- **Expected outcome**: The daemon starts successfully (socket becomes connectable within 5s). Source of truth: implementation plan Task 8 Step 1.
- **Interactions**: All Docker-dependent e2e tests depend on this helper.

### 11. `validate_custom_slug` accepts valid slugs

- **Name**: Valid slug strings pass validation
- **Type**: unit
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug()` with "dirty-panda", "my-app", "a", "abc123".
- **Expected outcome**: All return `Ok(())`. Source of truth: design spec validation rules -- "Lowercase alphanumeric and hyphens only, no leading/trailing hyphens, non-empty".
- **Interactions**: None.

### 12. `validate_custom_slug` rejects empty string

- **Name**: Empty slug is rejected
- **Type**: boundary
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug("")`.
- **Expected outcome**: Returns `Err` with message containing "empty". Source of truth: design spec -- "Non-empty".
- **Interactions**: None.

### 13. `validate_custom_slug` rejects uppercase characters

- **Name**: Slugs with uppercase letters are rejected
- **Type**: boundary
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug("Dirty-Panda")`.
- **Expected outcome**: Returns `Err`. Source of truth: design spec -- "Lowercase alphanumeric and hyphens only".
- **Interactions**: None.

### 14. `validate_custom_slug` rejects special characters

- **Name**: Slugs with underscores, dots, or spaces are rejected
- **Type**: boundary
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug()` with "dirty_panda", "dirty.panda", "dirty panda".
- **Expected outcome**: All return `Err`. Source of truth: design spec -- "Lowercase alphanumeric and hyphens only".
- **Interactions**: None.

### 15. `validate_custom_slug` rejects leading/trailing hyphens

- **Name**: Slugs starting or ending with hyphens are rejected
- **Type**: boundary
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug()` with "-dirty", "dirty-", "-dirty-".
- **Expected outcome**: All return `Err`. Source of truth: design spec -- "No leading or trailing hyphens".
- **Interactions**: None.

### 16. `validate_custom_slug_with_app` rejects slug+app exceeding 63 chars

- **Name**: Slug combined with app name exceeding DNS label limit is rejected before truncation
- **Type**: boundary
- **Harness**: In-process unit test in `config.rs`
- **Preconditions**: None.
- **Actions**: Call `validate_custom_slug_with_app("a".repeat(60), "my-app")`. The raw composite is 67 chars ("a"*60 + "-" + "my-app").
- **Expected outcome**: Returns `Err` with a message containing the computed length. Source of truth: design spec -- "Combined result must be <= 63 characters". Implementation plan D1 notes custom slugs should be rejected when too long, not silently truncated.
- **Interactions**: None.

### 17. CLI parses `up` without `--slug`

- **Name**: `devproxy up` parses with slug as None
- **Type**: unit
- **Harness**: In-process clap parsing in `cli.rs`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from(["devproxy", "up"])`.
- **Expected outcome**: Parses as `Commands::Up { slug: None }`. Source of truth: `cli.rs` defines `slug` as `Option<String>` with `#[arg(long)]`.
- **Interactions**: None.

### 18. CLI parses `up --slug dirty-panda`

- **Name**: `devproxy up --slug dirty-panda` parses with the slug value
- **Type**: unit
- **Harness**: In-process clap parsing in `cli.rs`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from(["devproxy", "up", "--slug", "dirty-panda"])`.
- **Expected outcome**: Parses as `Commands::Up { slug: Some("dirty-panda") }`. Source of truth: CLI definition.
- **Interactions**: None.

### 19. CLI parses `stop`, `start`, `restart` commands

- **Name**: New lifecycle commands parse correctly
- **Type**: unit
- **Harness**: In-process clap parsing in `cli.rs`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from` with `["devproxy", "stop"]`, `["devproxy", "start"]`, `["devproxy", "restart"]`.
- **Expected outcome**: Each parses as the corresponding `Commands` variant. Source of truth: CLI definition.
- **Interactions**: None.

### 20. CLI parses `daemon run` and `daemon run --port 8443`

- **Name**: Daemon run subcommand parses with default and custom port
- **Type**: unit
- **Harness**: In-process clap parsing in `cli.rs`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from` with `["devproxy", "daemon", "run"]` and `["devproxy", "daemon", "run", "--port", "8443"]`.
- **Expected outcome**: First parses as `Daemon { Run { port: 443 } }`, second as `Daemon { Run { port: 8443 } }`. Source of truth: CLI definition with `default_value = "443"`.
- **Interactions**: None.

### 21. CLI parses `daemon restart`

- **Name**: Daemon restart subcommand parses correctly
- **Type**: unit
- **Harness**: In-process clap parsing in `cli.rs`
- **Preconditions**: None.
- **Actions**: Call `Cli::try_parse_from(["devproxy", "daemon", "restart"])`.
- **Expected outcome**: Parses as `Daemon { Restart }`. Source of truth: CLI definition.
- **Interactions**: None.

### 22. `up` without compose file fails with helpful error

- **Name**: Running `up` in a directory without a compose file reports missing file
- **Type**: regression
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Empty temp directory. Config dir with `config.json`.
- **Actions**: Run `devproxy up` in the empty directory.
- **Expected outcome**: Exit non-zero. Stderr contains "no docker-compose.yml". Source of truth: `config::find_compose_file()` returns this error. This is existing behavior that must be preserved.
- **Interactions**: None.

### 23. `up` without `devproxy.port` label fails with helpful error

- **Name**: Running `up` with a compose file lacking `devproxy.port` reports no service found
- **Type**: regression
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Temp directory with compose file that has no `devproxy.port` label. Config dir with `config.json`.
- **Actions**: Run `devproxy up` in the temp directory.
- **Expected outcome**: Exit non-zero. Stderr contains "no service". Source of truth: `config::find_devproxy_service()` returns this error. Existing behavior preserved.
- **Interactions**: None.

### 24. `up` fails fast with dead daemon (stale socket)

- **Name**: Running `up` with a stale daemon socket fails within 5 seconds, not hanging
- **Type**: regression
- **Harness**: E2E binary invocation with stale Unix socket
- **Preconditions**: Config dir with `config.json`. Compose project with `devproxy.port` label. A stale Unix socket file at the socket path (created by binding and immediately dropping a `UnixListener`).
- **Actions**: Run `devproxy up` and measure elapsed time.
- **Expected outcome**: Exit non-zero within 5 seconds. Stderr contains "not running" or "no response". Cleanup: `.devproxy-override.yml` and `.devproxy-project` should NOT exist after failure (cleaned up by the `!reusing` guard). Source of truth: implementation plan Task 3 cleanup guards.
- **Interactions**: IPC ping timeout (2 seconds).

### 25. `stop` without project file fails with helpful error

- **Name**: Running `stop` in a directory without `.devproxy-project` reports error
- **Type**: boundary
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Temp directory with compose file but no `.devproxy-project`.
- **Actions**: Run `devproxy stop`.
- **Expected outcome**: Exit non-zero. Stderr contains ".devproxy-project" or "Is this project running". Source of truth: `config::read_project_file()` error message.
- **Interactions**: None.

### 26. `start` without project file fails with helpful error

- **Name**: Running `start` in a directory without `.devproxy-project` reports error
- **Type**: boundary
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Temp directory with compose file but no `.devproxy-project`.
- **Actions**: Run `devproxy start`.
- **Expected outcome**: Exit non-zero. Stderr contains ".devproxy-project" or "Is this project running". Source of truth: `config::read_project_file()` error message.
- **Interactions**: None.

### 27. `start` with project file but missing override fails with helpful error

- **Name**: Running `start` with a project file but no override file reports error
- **Type**: boundary
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Temp directory with compose file and `.devproxy-project` but no `.devproxy-override.yml`.
- **Actions**: Run `devproxy start`.
- **Expected outcome**: Exit non-zero. Stderr contains "override file missing" or "devproxy up". Source of truth: `commands/start.rs` checks for override existence.
- **Interactions**: None.

### 28. `restart` (app stack) without project file fails with helpful error

- **Name**: Running `restart` in a directory without `.devproxy-project` reports error
- **Type**: boundary
- **Harness**: E2E binary invocation (no Docker required)
- **Preconditions**: Temp directory with compose file but no `.devproxy-project`.
- **Actions**: Run `devproxy restart`.
- **Expected outcome**: Exit non-zero. Stderr contains ".devproxy-project" or "Is this project running". Source of truth: `commands/restart.rs` calls `config::read_project_file()`.
- **Interactions**: None.

## Coverage summary

### Covered

- **Custom slug flag**: Validation (accept/reject), CLI parsing, URL composition, DNS label length enforcement
- **Slug reuse**: `up` reuses existing state files, warns when `--slug` is ignored
- **New commands**: `stop` (preserves files), `start` (requires files, checks daemon), `restart` (app stack)
- **CLI restructuring**: `daemon run`/`daemon restart` subcommands, all new variants parseable, help output updated
- **Platform templates**: Both launchd plist and systemd unit updated to `daemon run --port`
- **Regression protection**: Existing `up` error paths (no compose file, no label, dead daemon), `down` without project file, version output, init generates certs
- **E2E infrastructure**: `start_test_daemon()` updated for `daemon run`

### Explicitly excluded

- **Actual launchd/systemd restart**: Requires platform service manager. Covered by existing `platform::restart_daemon()` logic and the `test_launchd_socket_activation` e2e test.
- **Linux systemd path**: Covered by existing `tests/linux-docker/` infrastructure.
- **Slug collision between projects**: Design spec explicitly states this is the user's responsibility, same as `docker compose -p`.
- **`down` after `stop`**: Standard compose behavior, no devproxy-specific logic beyond what `down` already does (remove files + `docker compose down`).

### Residual risk

- **Low**: The `stop`/`start`/`restart` commands are thin wrappers around `docker compose` subcommands with file-existence checks. The daemon's event watcher already handles container lifecycle events correctly.
- **Medium**: The `up` reuse path (test 2) depends on Docker being available. If Docker-dependent tests are not run, this path is only verified by code review and the dead-daemon regression test (test 24) which exercises the reuse-path cleanup guard.
