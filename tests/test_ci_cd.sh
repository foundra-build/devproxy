#!/bin/sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
FAIL=0

assert_file_exists() {
    desc="$1"
    filepath="$2"
    if [ -f "$filepath" ]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (file not found: $filepath)"
        FAIL=$((FAIL + 1))
    fi
}

# Searches for a pattern in a file using grep (regex by default).
assert_file_contains() {
    desc="$1"
    filepath="$2"
    needle="$3"
    if grep -q "$needle" "$filepath" 2>/dev/null; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (file does not contain '$needle')"
        FAIL=$((FAIL + 1))
    fi
}

# Searches for a fixed (literal) string in a file.
assert_file_contains_fixed() {
    desc="$1"
    filepath="$2"
    needle="$3"
    if grep -qF -- "$needle" "$filepath" 2>/dev/null; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (file does not contain '$needle')"
        FAIL=$((FAIL + 1))
    fi
}

assert_line_before() {
    desc="$1"
    filepath="$2"
    first="$3"
    second="$4"
    first_line=$(grep -n "$first" "$filepath" 2>/dev/null | head -1 | cut -d: -f1)
    second_line=$(grep -n "$second" "$filepath" 2>/dev/null | head -1 | cut -d: -f1)
    if [ -z "$first_line" ]; then
        echo "  FAIL: $desc (pattern '$first' not found in file)"
        FAIL=$((FAIL + 1))
    elif [ -z "$second_line" ]; then
        echo "  FAIL: $desc (pattern '$second' not found in file)"
        FAIL=$((FAIL + 1))
    elif [ "$first_line" -lt "$second_line" ]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc ('$first' at line $first_line, '$second' at line $second_line)"
        FAIL=$((FAIL + 1))
    fi
}

# Test numbering matches the test plan. Tests 4, 7, and 8 are skipped here
# because they require GitHub Actions execution (push, PR, workflow run)
# and can only be verified post-merge on GitHub.

CI_YML="$REPO_DIR/.github/workflows/ci.yml"
RELEASE_YML="$REPO_DIR/.github/workflows/release.yml"

# ── Test 1: CI workflow YAML is valid and triggers on PRs and pushes to main ──
echo ""
echo "Test 1: CI workflow YAML is valid and triggers on PRs and pushes to main"

assert_file_exists "ci.yml exists" "$CI_YML"
assert_file_contains "triggers on pull_request" "$CI_YML" "pull_request"
assert_file_contains "triggers on push" "$CI_YML" "push"
assert_file_contains "targets main branch" "$CI_YML" "branches:.*main"
assert_file_contains "has check job" "$CI_YML" "check:"
assert_file_contains "has install-script job" "$CI_YML" "install-script:"
assert_file_contains "has concurrency group" "$CI_YML" "concurrency"
assert_file_contains_fixed "CI cancels in-progress runs" "$CI_YML" "cancel-in-progress: true"

# ── Test 2: CI check job runs fmt, clippy, test in correct order ──
echo ""
echo "Test 2: CI check job runs fmt, clippy, test in correct order"

assert_file_contains_fixed "has cargo fmt --check" "$CI_YML" "cargo fmt -- --check"
assert_file_contains_fixed "has cargo clippy with -D warnings" "$CI_YML" "cargo clippy --all-targets -- -D warnings"
assert_file_contains_fixed "has cargo test" "$CI_YML" "cargo test"
assert_line_before "fmt before clippy" "$CI_YML" "cargo fmt" "cargo clippy"
assert_line_before "clippy before test" "$CI_YML" "cargo clippy" "cargo test"

# ── Test 3: CI install-script job runs test_install.sh ──
echo ""
echo "Test 3: CI install-script job runs test_install.sh"

assert_file_contains "install-script job runs test_install.sh" "$CI_YML" "tests/test_install.sh"

# ── Test 5: Release workflow YAML is valid and uses manual dispatch ──
echo ""
echo "Test 5: Release workflow YAML is valid and uses manual dispatch"

assert_file_exists "release.yml exists" "$RELEASE_YML"
assert_file_contains "uses workflow_dispatch" "$RELEASE_YML" "workflow_dispatch"
assert_file_contains "has version input" "$RELEASE_YML" "version:"
assert_file_contains "version input is required" "$RELEASE_YML" "required: true"
assert_file_contains "has x86_64-apple-darwin target" "$RELEASE_YML" "x86_64-apple-darwin"
assert_file_contains "has aarch64-apple-darwin target" "$RELEASE_YML" "aarch64-apple-darwin"
assert_file_contains "has x86_64-unknown-linux-gnu target" "$RELEASE_YML" "x86_64-unknown-linux-gnu"
assert_file_contains "has aarch64-unknown-linux-gnu target" "$RELEASE_YML" "aarch64-unknown-linux-gnu"
assert_file_contains "release job needs build" "$RELEASE_YML" "needs: build"
assert_file_contains "uses gh release create" "$RELEASE_YML" "gh release create"
assert_file_contains "validates version format" "$RELEASE_YML" "semver format"
assert_file_contains_fixed "checks for existing tag via ls-remote" "$RELEASE_YML" "ls-remote --tags"
assert_file_contains_fixed "pins cross to a specific commit rev" "$RELEASE_YML" "--rev f8151ae"
assert_file_contains "pins checkout to github.sha" "$RELEASE_YML" "github.sha"
assert_file_contains "verifies binary count" "$RELEASE_YML" "EXPECTED_COUNT=4"
assert_file_contains "runs tests before release build" "$RELEASE_YML" "cargo test"
assert_file_contains "re-checks tag before creation" "$RELEASE_YML" "Re-check.*tag"
assert_file_contains "uses macos-latest for all macOS targets" "$RELEASE_YML" "macos-latest"
assert_file_contains_fixed "has release concurrency group" "$RELEASE_YML" 'group: "release"'
assert_file_contains_fixed "release concurrency does not cancel" "$RELEASE_YML" "cancel-in-progress: false"
assert_file_contains_fixed "build jobs have timeout" "$RELEASE_YML" "timeout-minutes: 30"
assert_file_contains "cross pinned with version comment" "$RELEASE_YML" "# cross v0.2.5"

# ── Test 6: Release binary naming matches install script ──
echo ""
echo "Test 6: Release binary naming matches install script"

assert_file_contains_fixed "release renames binary with target" "$RELEASE_YML" 'devproxy-${{ matrix.target }}'
assert_file_contains_fixed "install.sh uses BINARY_NAME with TARGET" "$REPO_DIR/install.sh" 'BINARY_NAME="devproxy-${TARGET}"'

# ── Test 9: README includes version pinning documentation ──
echo ""
echo "Test 9: README includes version pinning documentation"

README="$REPO_DIR/README.md"
assert_file_contains_fixed "README has install command" "$README" "curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh"
assert_file_contains_fixed "README has version pinning" "$README" "DEVPROXY_VERSION="

# ── Test 10: Release documentation exists and is accurate ──
echo ""
echo "Test 10: Release documentation exists and is accurate"

RELDOC="$REPO_DIR/docs/releasing.md"
assert_file_exists "docs/releasing.md exists" "$RELDOC"
assert_file_contains_fixed "Mentions x86_64-apple-darwin" "$RELDOC" "x86_64-apple-darwin"
assert_file_contains_fixed "Mentions aarch64-apple-darwin" "$RELDOC" "aarch64-apple-darwin"
assert_file_contains_fixed "Mentions x86_64-unknown-linux-gnu" "$RELDOC" "x86_64-unknown-linux-gnu"
assert_file_contains_fixed "Mentions aarch64-unknown-linux-gnu" "$RELDOC" "aarch64-unknown-linux-gnu"
assert_file_contains "Mentions manual release process" "$RELDOC" "Run workflow"
assert_file_contains_fixed "Mentions DEVPROXY_VERSION" "$RELDOC" "DEVPROXY_VERSION"
assert_file_contains "Mentions CI must be green before release" "$RELDOC" "CI.*green"

# ── Summary ──
echo ""
echo "================================"
echo "Results: $PASS passed, $FAIL failed"
echo "================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
