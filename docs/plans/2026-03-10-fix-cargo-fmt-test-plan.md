# Fix cargo fmt CI Failure — Test Plan

## Strategy reconciliation

The testing strategy (run `cargo fmt -- --check`, `cargo clippy`, `cargo test`) maps 1:1 to the CI workflow steps in `.github/workflows/ci.yml`. The implementation plan makes formatting changes via `cargo fmt` and fixes pre-existing `clippy::collapsible_if` warnings in `tests/e2e.rs` that were previously masked by the format failure. No new interfaces or external dependencies.

**Source of truth:** `.github/workflows/ci.yml` defines the exact commands and flags the CI pipeline runs. All assertions below mirror those commands.

## Harness requirements

None. All tests use the local Rust toolchain (`cargo fmt`, `cargo clippy`, `cargo test`) which is already available. No test harnesses need to be built.

## Test plan

### 1. Formatting check passes with zero diffs

- **Name:** `cargo fmt --check` produces no diffs after formatting
- **Type:** invariant
- **Harness:** `cargo fmt -- --check` (same command as CI)
- **Preconditions:** `cargo fmt` has been run on the worktree
- **Actions:** Run `cargo fmt -- --check`
- **Expected outcome:** Exit code 0, empty stdout. Source of truth: `.github/workflows/ci.yml` line 27 (`cargo fmt -- --check`). Any non-zero exit or diff output means the fix is incomplete.
- **Interactions:** None. Pure formatting check, no compilation or linking.

### 2. Clippy passes with no warnings

- **Name:** Clippy lints pass after formatting changes
- **Type:** integration
- **Harness:** `cargo clippy --all-targets -- -D warnings` (same command as CI)
- **Preconditions:** Source files have been reformatted by `cargo fmt`
- **Actions:** Run `cargo clippy --all-targets -- -D warnings`
- **Expected outcome:** Exit code 0, no warnings or errors. Source of truth: `.github/workflows/ci.yml` line 29 (`cargo clippy --all-targets -- -D warnings`). This step was previously blocked by the format failure. Pre-existing clippy warnings (e.g., `clippy::collapsible_if` in `tests/e2e.rs`) must also be fixed to achieve a clean pass.
- **Interactions:** Exercises the full Rust compilation pipeline. Any pre-existing clippy issue unrelated to formatting will surface here and must be addressed.

### 3. Test suite passes after formatting changes

- **Name:** All tests pass after formatting changes
- **Type:** integration
- **Harness:** `cargo test` (same command as CI)
- **Preconditions:** Source files have been reformatted by `cargo fmt`
- **Actions:** Run `cargo test`
- **Expected outcome:** Exit code 0, all tests pass. Source of truth: `.github/workflows/ci.yml` line 31 (`cargo test`). Formatting changes cannot alter logic, but this confirms the third CI step passes end-to-end.
- **Interactions:** Compiles and runs all unit and integration tests including `tests/e2e.rs` (which is one of the reformatted files). The e2e tests invoke the compiled binary as a subprocess, so this exercises the full build-and-run path.

### 4. Only formatting and clippy-lint changes in the diff

- **Name:** Diff contains only whitespace/line-wrapping changes and semantically equivalent clippy lint fixes
- **Type:** invariant
- **Harness:** `git diff` inspection
- **Preconditions:** `cargo fmt` has been run and clippy warnings have been fixed, but changes are not yet committed (or compare the commit diff)
- **Actions:** Run `git diff -- src/ tests/` and inspect the output
- **Expected outcome:** In `src/` files, all hunks contain only whitespace, indentation, and line-break changes. In `tests/e2e.rs`, hunks also include collapsing nested `if`/`if let` into `if ... && let ...` chains (a semantically equivalent AST-level change to satisfy `clippy::collapsible_if`). The affected files are exactly: `src/commands/init.rs`, `src/commands/ls.rs`, `src/config.rs`, `src/platform.rs`, `tests/e2e.rs`.
- **Interactions:** None.

### 5. Correct files are modified

- **Name:** Exactly the 5 expected files are changed, no others
- **Type:** boundary
- **Harness:** `git diff --name-only`
- **Preconditions:** `cargo fmt` has been run
- **Actions:** Run `git diff --name-only` (or `git diff --name-only HEAD~1` after commit)
- **Expected outcome:** Output contains exactly these 5 paths (in any order): `src/commands/init.rs`, `src/commands/ls.rs`, `src/config.rs`, `src/platform.rs`, `tests/e2e.rs`. No other files are modified. Source of truth: the `cargo fmt -- --check` output which identified diffs in exactly these files.
- **Interactions:** None.

## Coverage summary

**Covered:**
- All three CI `check` job steps (`fmt --check`, `clippy`, `test`) are verified with the exact same commands and flags used in CI.
- Diff correctness: only formatting and semantically equivalent clippy-lint changes, only expected files.

**Excluded (per strategy):**
- The `install-script` CI job (`tests/test_install.sh`) is not affected by Rust source formatting and is excluded. Risk: none — that job is independent and was not failing.
- No cross-platform or Docker-based Linux test verification. The CI runs on `ubuntu-latest`; local verification runs on macOS. Risk: negligible for formatting-only changes, since `rustfmt` output is platform-independent.
