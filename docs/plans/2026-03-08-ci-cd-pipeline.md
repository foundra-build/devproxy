# CI/CD Pipeline Implementation Plan

**Goal:** Add GitHub Actions CI (tests on PRs) and release pipeline (manual dispatch, cross-compile 4 targets, create GitHub release with binaries).

**Architecture:** Two workflow files: `ci.yml` runs on PRs (fmt, clippy, test, install script tests); `release.yml` runs on manual dispatch with a version input, cross-compiles for 4 targets using `cross`, creates a GitHub release, and uploads binaries named `devproxy-{TARGET}`.

**Tech Stack:** GitHub Actions, `cross` (for cross-compilation), `gh` CLI (for release creation), Rust toolchain

---

### Task 1: Create CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

**Step 1: Create the CI workflow file**

```yaml
name: CI

on:
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: Check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format check
        run: cargo fmt -- --check
      - name: Clippy
        run: cargo clippy --all-targets -- -D warnings
      - name: Tests
        run: cargo test

  install-script:
    name: Install script tests
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run install script tests
        run: sh tests/test_install.sh
```

**Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`
Expected: No output (success)

**Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add CI workflow for PRs (fmt, clippy, test, install script)"
```

---

### Task 2: Create release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Step 1: Create the release workflow file**

The release workflow uses `cross` for cross-compilation of all 4 targets. It:
1. Takes a version input (e.g., `0.0.1`) via manual dispatch
2. Creates a git tag `v{version}`
3. Builds binaries for all 4 targets in a matrix
4. Creates a GitHub release and uploads binaries named `devproxy-{TARGET}`

```yaml
name: Release

on:
  workflow_dispatch:
    inputs:
      version:
        description: 'Release version (e.g., 0.0.1)'
        required: true
        type: string

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: Build ${{ matrix.target }}
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}

      - name: Install cross (Linux cross-compile)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: cargo install cross --git https://github.com/cross-rs/cross

      - name: Build (native)
        if: matrix.target != 'aarch64-unknown-linux-gnu'
        run: cargo build --release --target ${{ matrix.target }}

      - name: Build (cross)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: cross build --release --target ${{ matrix.target }}

      - name: Rename binary
        run: cp target/${{ matrix.target }}/release/devproxy devproxy-${{ matrix.target }}

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: devproxy-${{ matrix.target }}
          path: devproxy-${{ matrix.target }}

  release:
    name: Create Release
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts

      - name: Prepare binaries
        run: |
          mkdir -p release
          for dir in artifacts/devproxy-*/; do
            cp "$dir"devproxy-* release/
          done
          chmod +x release/*
          ls -la release/

      - name: Create tag
        run: |
          git config user.name "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"
          git tag -a "v${{ github.event.inputs.version }}" -m "Release v${{ github.event.inputs.version }}"
          git push origin "v${{ github.event.inputs.version }}"

      - name: Create GitHub Release
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          gh release create "v${{ github.event.inputs.version }}" \
            --title "v${{ github.event.inputs.version }}" \
            --generate-notes \
            release/*
```

**Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: No output (success)

**Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add manual release workflow with cross-compiled binaries"
```

---

### Task 3: Update README install instructions

**Files:**
- Modify: `README.md`

**Step 1: Verify README install section is correct**

The README already has the correct install command:
```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

This is correct. No changes needed unless the section is missing version pinning docs.

**Step 2: Add version pinning note to README**

After the install command, add a note about pinning to a specific version:

```markdown
## Install

```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

To install a specific version:

```bash
DEVPROXY_VERSION=v0.0.1 curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```
```

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add version pinning to install instructions"
```

---

### Task 4: Add CI/CD documentation

**Files:**
- Create: `docs/releasing.md`

**Step 1: Create releasing docs**

Document the release process so it's clear for future contributors:

```markdown
# Releasing devproxy

## CI Pipeline

Pull requests to `main` automatically run:
- `cargo fmt -- --check` (formatting)
- `cargo clippy --all-targets -- -D warnings` (linting)
- `cargo test` (unit tests)
- `sh tests/test_install.sh` (install script tests)

## Creating a Release

Releases are created manually via GitHub Actions:

1. Go to **Actions** → **Release** → **Run workflow**
2. Enter the version number (e.g., `0.0.1` — without the `v` prefix)
3. The workflow will:
   - Build binaries for 4 targets:
     - `x86_64-apple-darwin` (macOS Intel)
     - `aarch64-apple-darwin` (macOS Apple Silicon)
     - `x86_64-unknown-linux-gnu` (Linux x86_64)
     - `aarch64-unknown-linux-gnu` (Linux ARM64)
   - Create a git tag `v{version}`
   - Create a GitHub release with all binaries attached

## Binary Naming

Binaries are named `devproxy-{TARGET}` where TARGET is the Rust target triple.
The install script (`install.sh`) expects this naming convention.

## Install Script

The install script at `install.sh` downloads the correct binary for the user's platform.
It supports:
- `DEVPROXY_VERSION` — pin to a specific version (e.g., `v0.0.1`), defaults to `latest`
- `DEVPROXY_INSTALL_DIR` — custom install directory, defaults to `/usr/local/bin`

## Verification

After creating a release, verify the install script works:

```bash
# Test latest
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh

# Test specific version
DEVPROXY_VERSION=v0.0.1 curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```
```

**Step 2: Commit**

```bash
git add docs/releasing.md
git commit -m "docs: add releasing guide"
```

---

### Task 5: Verify everything and push

**Step 1: Run local tests to make sure nothing is broken**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/add-ci-cd-pipeline && sh tests/test_install.sh`
Expected: All tests pass

**Step 2: Push branch and create PR**

```bash
git push -u origin add-ci-cd-pipeline
```

Then create a PR targeting `main`.

---

### Post-merge: Create v0.0.1 release (manual steps)

These steps happen after the PR is merged:

1. Go to GitHub Actions → Release → Run workflow
2. Enter version: `0.0.1`
3. Wait for the workflow to complete
4. Verify the release page has 4 binaries
5. Test the install script:
   ```bash
   DEVPROXY_VERSION=v0.0.1 curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
   devproxy --help
   ```
