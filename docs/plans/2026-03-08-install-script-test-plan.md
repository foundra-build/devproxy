# devproxy — Install Script Test Plan

## Harness requirements

### 1. uname wrapper harness

- **What it does:** Creates a temporary directory containing a `uname` script that returns configurable OS and ARCH values, allowing platform detection to be tested on any host.
- **Exposes:** A function `make_uname_wrapper(os, arch)` that returns a directory path. Prepending this directory to `PATH` overrides the system `uname`.
- **Estimated complexity:** Low — ~15 lines of shell.
- **Tests that depend on it:** Tests 1, 2, 3, 5.

### 2. Detection harness (run_detection)

- **What it does:** Extracts `detect_platform` and `construct_url` functions from `install.sh` (by stripping the `main` call), appends print statements, and runs them with the overridden uname and env vars. This lets tests inspect intermediate values (`TARGET`, `DOWNLOAD_URL`) without running the full install flow.
- **Exposes:** A function `run_detection(uname_dir, base_url, version)` that prints `TARGET=...` and `DOWNLOAD_URL=...` lines to stdout. A variant `run_detection_with_stderr(uname_dir)` that captures stderr for error-path tests.
- **Estimated complexity:** Low — ~20 lines of shell using `sed` to strip the main call.
- **Tests that depend on it:** Tests 1, 2, 3.

### 3. Mock HTTP server

- **What it does:** Serves a directory tree via Python 3's `http.server` on a random free port. The directory contains mock binary files (shell scripts that echo a version string) at paths matching the GitHub Releases URL structure (`latest/download/devproxy-<triple>`).
- **Exposes:** `MOCK_PORT` variable and `MOCK_SERVER_PID` for cleanup.
- **Estimated complexity:** Low — Python 3 is available on all target platforms. ~10 lines to start, 1 line to stop.
- **Tests that depend on it:** Tests 4, 5.

### 4. Minimal PATH harness

- **What it does:** Creates a temporary `bin` directory with symlinks to only essential system commands (sh, uname, mktemp, chmod, mkdir, mv, rm, cat, etc.) but explicitly excludes `curl` and `wget`. Setting `PATH` to only this directory simulates a system without any HTTP download tool.
- **Exposes:** `MINIMAL_BIN` directory path.
- **Estimated complexity:** Low — ~10 lines of shell.
- **Tests that depend on it:** Test 6.

---

## Test plan

### Test 1: OS/arch detection produces correct target triple for all supported platforms

- **Type:** scenario
- **Harness:** uname wrapper + detection harness
- **Preconditions:** `install.sh` exists at repo root. No network access needed.
- **Actions:** For each of the 4 supported platform combinations:
  1. `Darwin` + `arm64` (macOS Apple Silicon)
  2. `Darwin` + `x86_64` (macOS Intel)
  3. `Linux` + `x86_64` (Linux AMD64)
  4. `Linux` + `aarch64` (Linux ARM64)

  Create a uname wrapper returning the given OS/arch. Run the detection harness. Extract the `TARGET` value from stdout.
- **Expected outcome:** Each combo produces the correct target triple, per the binary naming convention defined in the implementation plan:
  - `Darwin`/`arm64` -> `aarch64-apple-darwin`
  - `Darwin`/`x86_64` -> `x86_64-apple-darwin`
  - `Linux`/`x86_64` -> `x86_64-unknown-linux-gnu`
  - `Linux`/`aarch64` -> `aarch64-unknown-linux-gnu`

  Source of truth: implementation plan key design decisions — binary naming convention `devproxy-<target-triple>`.
- **Interactions:** Exercises the `uname` wrapper harness; no real system calls to `uname`.

### Test 2: Unsupported OS or architecture exits non-zero with error message

- **Type:** boundary
- **Harness:** uname wrapper + detection harness (with stderr variant)
- **Preconditions:** `install.sh` exists at repo root.
- **Actions:**
  1. Create uname wrapper returning `FreeBSD` / `x86_64`. Run detection harness with stderr capture.
  2. Create uname wrapper returning `Linux` / `mips`. Run detection harness with stderr capture.
- **Expected outcome:**
  - Both invocations exit with non-zero status.
  - Stderr output contains the word "unsupported" (case-insensitive).
  - The unsupported value (OS name or arch name) appears in the error message.

  Source of truth: implementation plan `detect_platform()` function — explicit `*) echo "Error: unsupported..."` cases.
- **Interactions:** None beyond the uname wrapper.

### Test 3: URL construction matches GitHub Releases pattern for latest and versioned downloads

- **Type:** integration
- **Harness:** uname wrapper + detection harness
- **Preconditions:** `install.sh` exists at repo root.
- **Actions:**
  1. With `Darwin`/`arm64`, `DEVPROXY_VERSION=latest`, `DEVPROXY_INSTALL_BASE_URL=https://example.com/releases`: run detection harness, extract `DOWNLOAD_URL`.
  2. Same platform with `DEVPROXY_VERSION=v1.0.0`: run detection harness, extract `DOWNLOAD_URL`.
  3. With `Linux`/`x86_64`, `DEVPROXY_VERSION=latest`: run detection harness, extract `DOWNLOAD_URL`.
- **Expected outcome:**
  - Latest Darwin/arm64: `https://example.com/releases/latest/download/devproxy-aarch64-apple-darwin`
  - Versioned Darwin/arm64: `https://example.com/releases/download/v1.0.0/devproxy-aarch64-apple-darwin`
  - Latest Linux/x86_64: `https://example.com/releases/latest/download/devproxy-x86_64-unknown-linux-gnu`

  Source of truth: implementation plan `construct_url()` function — latest uses `{base}/latest/download/{binary}`, versioned uses `{base}/download/{version}/{binary}`. This matches standard GitHub Releases URL patterns.
- **Interactions:** Exercises `DEVPROXY_INSTALL_BASE_URL` env var override, confirming it is respected.

### Test 4: Full e2e install downloads binary from mock server, installs it, and is idempotent

- **Type:** scenario
- **Harness:** Mock HTTP server
- **Preconditions:** Python 3 available. Mock server running on localhost with a mock binary (shell script echoing `devproxy mock 0.0.1-test`) at the correct path for the current host platform.
- **Actions:**
  1. Start mock HTTP server serving directory with `latest/download/devproxy-<host-triple>` mock binary.
  2. Run `install.sh` with `DEVPROXY_INSTALL_BASE_URL=http://localhost:<port>` and `DEVPROXY_INSTALL_DIR=<temp-dir>`.
  3. Check that `<temp-dir>/devproxy` exists and is executable (`-x` test).
  4. Execute `<temp-dir>/devproxy` and capture output.
  5. Run `install.sh` again with the same env vars (idempotency check).
- **Expected outcome:**
  - Step 2: install script exits 0.
  - Step 3: binary exists at `<temp-dir>/devproxy` and is executable.
  - Step 4: output contains `devproxy mock`.
  - Step 5: second install exits 0 (no error on overwrite).

  Source of truth: implementation plan Task 2 test case 4 — "verify the binary exists, is executable, and produces expected output. Run a second time to verify idempotency." Also implementation plan `create_install_dir()` handles existing dir, and `download_binary()` uses mv to overwrite.
- **Interactions:** Exercises the full install pipeline: platform detection (real host uname), URL construction, HTTP download (curl or wget against localhost), file placement, chmod. The mock server replaces GitHub Releases.

### Test 5: Download failure (404) exits non-zero with error message

- **Type:** boundary
- **Harness:** uname wrapper + mock HTTP server
- **Preconditions:** Mock server running. `DEVPROXY_INSTALL_BASE_URL` pointed at a nonexistent path on the mock server.
- **Actions:**
  1. Override uname to `Linux`/`aarch64` (a platform whose mock binary does NOT exist on the mock server at the nonexistent path).
  2. Run `install.sh` with `DEVPROXY_INSTALL_BASE_URL=http://localhost:<port>/nonexistent` and `DEVPROXY_INSTALL_DIR=<temp-dir>`.
  3. Capture exit code and combined stdout/stderr.
- **Expected outcome:**
  - Exit code is non-zero.
  - Output contains "error" or "fail" (case-insensitive).

  Source of truth: implementation plan `download_binary()` function — `curl -fsSL` returns non-zero on 404, script prints "Error: failed to download" and exits 1.
- **Interactions:** Exercises curl's `-f` flag (fail on HTTP errors) or wget's equivalent behavior against a real HTTP 404 response.

### Test 6: Missing downloader (no curl or wget) exits non-zero with descriptive error

- **Type:** boundary
- **Harness:** Minimal PATH harness
- **Preconditions:** A restricted `PATH` containing only essential commands, with `curl` and `wget` excluded.
- **Actions:**
  1. Set `PATH` to the minimal bin directory.
  2. Run `install.sh` with `DEVPROXY_INSTALL_BASE_URL` and `DEVPROXY_INSTALL_DIR` set.
  3. Capture exit code and combined stdout/stderr.
- **Expected outcome:**
  - Exit code is non-zero.
  - Output mentions "curl" or "wget" (case-insensitive), informing the user what to install.

  Source of truth: implementation plan `download_binary()` function — `else echo "Error: neither curl nor wget found..." >&2; exit 1`.
- **Interactions:** The script must reach the download phase (platform detection and URL construction succeed with the minimal PATH, since `uname` is included). Only the downloader check fails.

---

## Coverage summary

### Covered

- **Platform detection:** All 4 supported OS/arch combinations tested (Test 1).
- **Unsupported platforms:** Both unsupported OS and unsupported arch rejected with clear error (Test 2).
- **URL construction:** Both latest and versioned URL patterns verified, plus base URL override (Test 3).
- **Happy-path install:** Full end-to-end download, placement, permissions, and execution verified (Test 4).
- **Idempotency:** Reinstall over existing binary succeeds (Test 4).
- **Download failure:** HTTP 404 handled gracefully (Test 5).
- **Missing downloader:** Absence of curl/wget detected with user-friendly error (Test 6).
- **DEVPROXY_INSTALL_BASE_URL:** Exercised in Tests 3, 4, 5 — confirms the override works for testing and mirrors.
- **DEVPROXY_INSTALL_DIR:** Exercised in Tests 4, 5, 6 — confirms custom install directory works.

### Explicitly excluded (per agreed strategy)

- **wget fallback path:** The e2e test uses whichever downloader is available on the host (likely curl on macOS). Testing wget specifically would require hiding curl, which adds complexity for low risk. Risk: wget codepath could have a subtle behavioral difference (e.g., different redirect handling). Mitigated by the script's simple wget invocation.
- **DEVPROXY_VERSION specific version download e2e:** URL construction is tested (Test 3) but no e2e download with a versioned path. Risk: low, since the URL pattern is the only difference.
- **Sudo/permissions:** Installing to `/usr/local/bin` (the default) may require sudo. Tests use `DEVPROXY_INSTALL_DIR` to avoid this. Risk: a user who runs without sudo to a protected directory gets a permission error. This is standard Unix behavior, not a script bug.
- **Network/TLS:** Real GitHub Releases downloads are not tested. Risk: mitigated by curl/wget being battle-tested HTTP clients.
