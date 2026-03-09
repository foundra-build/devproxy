# CI/CD Pipeline Test Plan

## Harness Requirements

### Harness 1: YAML Structure Validator

- **What it does:** Validates that workflow YAML files are syntactically valid and contain required structural elements (jobs, triggers, steps).
- **What it exposes:** Python-based YAML parsing + key existence checks via shell script.
- **Estimated complexity:** Low — uses `python3 -c "import yaml; ..."` one-liners.
- **Tests that depend on it:** Tests 1, 2, 3, 5.

### Harness 2: GitHub Actions Workflow Execution (via `gh workflow run` + `gh run watch`)

- **What it does:** Triggers the actual workflows on GitHub and observes their outcomes.
- **What it exposes:** Exit codes and status from `gh run view`.
- **Estimated complexity:** None to build — uses existing `gh` CLI. Depends on the PR being pushed.
- **Tests that depend on it:** Tests 4, 6, 7, 8.

No custom harnesses need to be built. All testing uses existing tools (python3 for YAML parsing, `gh` CLI for GitHub interaction, existing `test_install.sh` for install script validation).

---

## Test Plan

### Test 1: CI workflow YAML is valid and triggers on PRs to main

- **Type:** integration
- **Harness:** YAML Structure Validator
- **Preconditions:** `.github/workflows/ci.yml` exists in the worktree.
- **Actions:**
  1. Run `python3 -c "import yaml; y=yaml.safe_load(open('.github/workflows/ci.yml')); assert y is not None"` — validates YAML syntax.
  2. Assert the parsed YAML contains `on.pull_request.branches` with `main`.
  3. Assert the parsed YAML contains a `jobs.check` job.
  4. Assert the parsed YAML contains a `jobs.install-script` job.
- **Expected outcome:** All assertions pass. The CI workflow is syntactically valid and structurally correct per the implementation plan. Source of truth: implementation plan Task 1.
- **Interactions:** None (pure file parsing).

### Test 2: CI workflow check job runs fmt, clippy, and test in correct order

- **Type:** integration
- **Harness:** YAML Structure Validator
- **Preconditions:** `.github/workflows/ci.yml` exists with a `check` job.
- **Actions:**
  1. Parse the YAML and extract step names/run commands from `jobs.check.steps`.
  2. Assert there is a step containing `cargo fmt -- --check`.
  3. Assert there is a step containing `cargo clippy --all-targets -- -D warnings`.
  4. Assert there is a step containing `cargo test`.
  5. Assert fmt appears before clippy, and clippy appears before test (by step index).
- **Expected outcome:** All three CI checks are present and ordered correctly (fmt -> clippy -> test). Source of truth: implementation plan Task 1 and the agreed testing strategy ("cargo fmt --check, clippy, test on ubuntu-latest").
- **Interactions:** None.

### Test 3: CI workflow install-script job runs test_install.sh

- **Type:** integration
- **Harness:** YAML Structure Validator
- **Preconditions:** `.github/workflows/ci.yml` exists with an `install-script` job.
- **Actions:**
  1. Parse the YAML and extract the `jobs.install-script.steps`.
  2. Assert there is a step whose `run` field contains `tests/test_install.sh`.
- **Expected outcome:** The install-script job invokes the existing test suite. Source of truth: implementation plan Task 1 (`sh tests/test_install.sh`).
- **Interactions:** Depends on `tests/test_install.sh` existing — verified by the existing install script test suite.

### Test 4: CI workflow passes when triggered on the PR

- **Type:** scenario
- **Harness:** GitHub Actions Workflow Execution
- **Preconditions:** Branch is pushed to origin. PR is open against `main`.
- **Actions:**
  1. Push the branch with `git push -u origin add-ci-cd-pipeline`.
  2. Create the PR with `gh pr create`.
  3. Wait for CI checks to complete with `gh pr checks --watch`.
  4. Verify all checks pass.
- **Expected outcome:** The CI workflow triggers automatically on the PR and all jobs (check, install-script) succeed. Source of truth: implementation plan Task 1 trigger (`on: pull_request: branches: [main]`).
- **Interactions:** GitHub Actions runners, Rust toolchain installation, crate downloads, existing test suite.

### Test 5: Release workflow YAML is valid and uses manual dispatch with version input

- **Type:** integration
- **Harness:** YAML Structure Validator
- **Preconditions:** `.github/workflows/release.yml` exists in the worktree.
- **Actions:**
  1. Run `python3 -c "import yaml; y=yaml.safe_load(open('.github/workflows/release.yml')); assert y is not None"`.
  2. Assert `on.workflow_dispatch.inputs.version` exists and is required.
  3. Assert `jobs.build.strategy.matrix.include` contains exactly 4 entries.
  4. Assert the 4 targets are: `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.
  5. Assert `jobs.release` exists and has `needs: build`.
  6. Assert the release job creates a tag and a GitHub release using `gh release create`.
- **Expected outcome:** The release workflow is syntactically valid and structurally matches the spec. Source of truth: implementation plan Task 2.
- **Interactions:** None.

### Test 6: Release workflow binary naming matches install script expectations

- **Type:** integration
- **Harness:** YAML Structure Validator + install.sh source inspection
- **Preconditions:** Both `.github/workflows/release.yml` and `install.sh` exist.
- **Actions:**
  1. Parse `release.yml` and extract the binary rename step. Verify it produces `devproxy-${{ matrix.target }}`.
  2. Read `install.sh` and verify `BINARY_NAME="devproxy-${TARGET}"` where TARGET is `{ARCH_TARGET}-{OS_TARGET}`.
  3. Assert the target triples in the release matrix correspond 1:1 with the targets the install script can produce (Darwin arm64 -> aarch64-apple-darwin, Darwin x86_64 -> x86_64-apple-darwin, Linux x86_64 -> x86_64-unknown-linux-gnu, Linux aarch64 -> aarch64-unknown-linux-gnu).
- **Expected outcome:** Binary naming in the release workflow and the install script are consistent — the install script will find the binary it expects. Source of truth: implementation plan Task 2 ("binaries named devproxy-{TARGET}") and `install.sh` source.
- **Interactions:** Coupling between release workflow and install script. This is a critical integration boundary.

### Test 7: Release workflow creates a GitHub release with correct artifacts (post-merge, manual)

- **Type:** scenario
- **Harness:** GitHub Actions Workflow Execution
- **Preconditions:** PR is merged to main. Release workflow exists on main.
- **Actions:**
  1. Trigger the release workflow: `gh workflow run release.yml -f version=0.0.1`.
  2. Wait for the workflow run to complete: poll with `gh run list --workflow=release.yml -L 1` then `gh run watch`.
  3. Verify the release exists: `gh release view v0.0.1`.
  4. Verify 4 binary assets are attached: `gh release view v0.0.1 --json assets -q '.assets[].name'` should list `devproxy-x86_64-apple-darwin`, `devproxy-aarch64-apple-darwin`, `devproxy-x86_64-unknown-linux-gnu`, `devproxy-aarch64-unknown-linux-gnu`.
- **Expected outcome:** The release workflow creates a tagged release with all 4 binaries attached. Source of truth: implementation plan Task 2.
- **Interactions:** GitHub Actions runners (macOS + Ubuntu), cross compilation for aarch64-linux, GitHub Releases API, git tagging.

### Test 8: Install script downloads v0.0.1 release binary successfully (post-release, manual)

- **Type:** scenario
- **Harness:** Direct shell execution
- **Preconditions:** Release v0.0.1 exists on GitHub with the binary for the current platform.
- **Actions:**
  1. Run: `DEVPROXY_VERSION=v0.0.1 DEVPROXY_INSTALL_DIR=/tmp/devproxy-test curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh`
  2. Verify the binary exists: `test -x /tmp/devproxy-test/devproxy`.
  3. Run the binary: `/tmp/devproxy-test/devproxy --help`.
  4. Clean up: `rm -rf /tmp/devproxy-test`.
- **Expected outcome:** The install script successfully downloads the v0.0.1 binary for the current platform, installs it, and `devproxy --help` produces usage output. Source of truth: user's stated goal ("test the install script") and implementation plan post-merge steps.
- **Interactions:** GitHub Releases CDN, install script platform detection, network connectivity.

### Test 9: README includes version pinning documentation

- **Type:** boundary
- **Harness:** Content inspection (grep)
- **Preconditions:** `README.md` exists.
- **Actions:**
  1. Verify `README.md` contains the base install command: `curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh`.
  2. Verify `README.md` contains `DEVPROXY_VERSION=` showing version pinning.
- **Expected outcome:** Users can discover both the default install and version-pinned install from the README. Source of truth: implementation plan Task 3.
- **Interactions:** None.

### Test 10: Release documentation exists and is accurate

- **Type:** boundary
- **Harness:** Content inspection (grep)
- **Preconditions:** `docs/releasing.md` exists.
- **Actions:**
  1. Verify `docs/releasing.md` mentions all 4 target triples.
  2. Verify it documents the manual release process (Actions -> Release -> Run workflow).
  3. Verify it mentions `DEVPROXY_VERSION` for version pinning.
- **Expected outcome:** Release documentation covers the workflow and is consistent with the implementation. Source of truth: implementation plan Task 4.
- **Interactions:** None.

---

## Coverage Summary

### Covered

- **CI workflow structure and correctness:** Tests 1-4 verify the CI workflow is valid YAML, has the right triggers, runs the right checks in the right order, includes install script tests, and actually passes on GitHub.
- **Release workflow structure and correctness:** Tests 5-7 verify the release workflow is valid YAML, uses manual dispatch, targets all 4 platforms, produces correctly named binaries, and creates a real release.
- **Install script / release integration boundary:** Test 6 verifies naming consistency between the release workflow and install script. Test 8 verifies end-to-end download works.
- **Documentation:** Tests 9-10 verify README and release docs are updated.

### Explicitly Excluded (per agreed strategy)

- **Local cross-compilation testing:** Cross-compilation is tested only via the GitHub Actions release workflow (Test 7). We do not attempt to run `cross` locally. Risk: if `cross` has issues with the Cargo.toml configuration or dependencies, this will only surface during the release workflow run.
- **Performance testing:** Not applicable — CI/CD pipelines are not performance-sensitive beyond "completes in a reasonable time." GitHub Actions has its own timeouts.
- **Workflow failure modes:** We do not test what happens if GitHub Actions runners are unavailable, if the Rust toolchain action fails, etc. These are GitHub infrastructure concerns outside the project's control.

### Risks from Exclusions

- **Cross-compilation failures** are the primary risk. The `aarch64-unknown-linux-gnu` target requires `cross` and Docker-in-Docker on the GitHub runner. If the project's dependencies (e.g., `rcgen` with ring crypto) have cross-compilation issues, this will only be caught during the first real release workflow run (Test 7).
- **macOS runner availability** — `macos-latest` runners can be slow or have different Xcode versions. The release workflow depends on them for the two Darwin targets.
