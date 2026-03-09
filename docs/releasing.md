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
