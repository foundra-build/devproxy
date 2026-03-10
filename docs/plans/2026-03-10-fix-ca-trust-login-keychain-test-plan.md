# Test Plan: Fix CA Trust to Use Login Keychain

## Strategy reconciliation

The implementation plan has four tasks: (1) add a `login_keychain_path` helper with a unit test, (2) update `trust_ca_in_system` to use the login keychain, (3) update user-facing messages in `init.rs`, and (4) run the full test suite.

The approved testing strategy calls for:
- **Unit test in cert.rs** for command argument verification
- **Functional integration test marked `#[ignore]`** that touches the real login keychain with cleanup

The implementation plan already includes a unit test for the `login_keychain_path` helper (Task 1). The strategy adds two things the plan does not cover: (a) a unit test verifying the `security` command arguments assembled by `trust_ca_in_system`, and (b) a functional integration test that actually adds/removes a cert from the login keychain.

For (a), `trust_ca_in_system` calls `Command::new("security")` directly and returns a `Result` based on the exit code — there is no seam to intercept the command arguments without refactoring. Rather than introduce a trait/mock boundary for a one-off fix, we will verify the command arguments **indirectly** by confirming the `login_keychain_path` helper returns the correct path (unit test) and that the assembled command succeeds against a real keychain (functional test). This keeps the scope minimal and avoids speculative abstraction.

For the init.rs message changes (Task 3), the messages are `eprintln!` output with no structured return value. The e2e test infrastructure runs `devproxy init` and captures stderr, but the init flow requires Docker and creates real daemon state. Adding a new e2e test just for message text would be heavy. Instead, we rely on code review of the five specific lines called out in the plan and verify compilation via `cargo clippy`.

No changes to the approved strategy are needed.

## Test plan

### 1. `login_keychain_path_points_to_login_keychain` — helper returns correct absolute path

- **Name**: `login_keychain_path` returns an absolute path ending in `Library/Keychains/login.keychain-db`
- **Type**: unit
- **Location**: `src/proxy/cert.rs` — `#[cfg(test)] mod tests` block
- **Gate**: `#[cfg(target_os = "macos")]`
- **Preconditions**: None (uses `dirs::home_dir()` which works in test context).
- **Actions**: Call `super::login_keychain_path()`.
- **Expected outcome**: Returns `Ok(path)` where `path.is_absolute()` is true and `path.to_string_lossy().ends_with("Library/Keychains/login.keychain-db")`.
- **Source of truth**: Implementation plan Task 1 specifies this exact test.

### 2. `trust_ca_login_keychain_roundtrip` — functional test adds and removes cert from real login keychain

- **Name**: Generate a CA cert, trust it in the login keychain, verify it appears, then remove it
- **Type**: functional / integration
- **Location**: `tests/e2e.rs` (or a new `tests/keychain.rs` — see note below)
- **Gate**: `#[cfg(target_os = "macos")]`, `#[ignore]` (requires interactive keychain access, touches real system state)
- **Preconditions**: Running on macOS. Login keychain is unlocked (normal developer workstation). No pre-existing devproxy CA in the login keychain.
- **Actions**:
  1. Generate a fresh CA cert via `cert::generate_ca()` and write it to a temp file.
  2. Call `cert::trust_ca_in_system(&temp_cert_path)` — this should use the new login keychain path.
  3. Verify the cert is present by running `security find-certificate -c "devproxy Local CA" -a ~/Library/Keychains/login.keychain-db` and checking exit code 0.
  4. **Cleanup** (in a `Drop` guard or `defer!`-style scope to ensure it runs even on assertion failure): run `security remove-trusted-cert <temp_cert_path>` and `security delete-certificate -c "devproxy Local CA" ~/Library/Keychains/login.keychain-db`.
- **Expected outcome**: `trust_ca_in_system` returns `Ok(())`. The certificate is findable in the login keychain. Cleanup succeeds.
- **Interactions**: This test will trigger the macOS Keychain Access password dialog. It is marked `#[ignore]` so it does not run in CI or `cargo test`. Run manually with `cargo test --test keychain -- --ignored`.
- **Note on test file location**: A separate `tests/keychain.rs` is preferred over adding to `tests/e2e.rs` because: (a) e2e.rs has Docker/daemon dependencies baked into its helper functions, (b) this test has completely different prerequisites (macOS keychain, no Docker), and (c) it keeps the `#[ignore]` scope narrow. The file will be small (one test function plus cleanup).

### 3. Verify no regressions — existing cert unit tests still pass

- **Name**: Existing `cert::tests` pass after changes
- **Type**: regression (existing tests)
- **Location**: `src/proxy/cert.rs` — existing `#[cfg(test)] mod tests` block
- **Actions**: Run `cargo test --lib -- cert::tests` after each task.
- **Expected outcome**: All three existing tests (`generate_ca_produces_valid_pem`, `generate_wildcard_cert_produces_valid_pem`, `tls_config_loads_from_generated_certs`) pass.

### 4. Verify compilation and lint — full clippy + test suite

- **Name**: Full `cargo clippy` and `cargo test` pass
- **Type**: regression / gate
- **Location**: Entire crate
- **Actions**: Run `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test`.
- **Expected outcome**: Zero warnings, zero failures. This catches any compile errors in the `#[cfg(target_os = "macos")]` / `#[cfg(target_os = "linux")]` branches, and ensures init.rs message changes compile.

## Test execution order

1. Task 1 (impl) then **Test 1** — unit test for `login_keychain_path`
2. Task 2 (impl) then **Test 3** — regression check existing cert tests
3. Task 3 (impl) then **Test 4** — full clippy + test gate
4. Task 4 (impl plan's final verification)
5. **Test 2** — manual functional test (run once locally with `--ignored` before merging)
