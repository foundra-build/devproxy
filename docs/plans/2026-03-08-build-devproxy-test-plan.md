# devproxy — Test Plan

**Source of truth:** `docs/spec.md` (v0.1), user conversation (testing strategy), implementation plan (`docs/plans/2026-03-08-build-devproxy.md`).

**Strategy reconciliation:** The implementation plan is fully compatible with the agreed testing strategy. Two additions not explicitly discussed but required by the plan's architecture:
- `DEVPROXY_CONFIG_DIR` env var for test config isolation (since `dirs::config_dir()` on macOS ignores `HOME`). This is transparent to test design.
- `init --no-daemon` flag for clean cert generation without spawning a daemon. Tests that need a daemon start one explicitly on an ephemeral port. This gives better control than the strategy's original assumption of a single daemon.

No strategy changes requiring user approval.

---

## Harness Requirements

### E2E Test Harness (`tests/e2e.rs`)

**What it does:** Provides isolated environments for each e2e test, managing daemon lifecycle, Docker Compose projects, and cleanup.

**What it exposes:**
- `devproxy_bin()` — path to the compiled binary
- `find_free_port()` — allocates an ephemeral port for the daemon
- `copy_fixtures(test_name)` — copies `tests/fixtures/` into an isolated temp dir
- `create_test_config_dir(test_name)` — creates an isolated config dir and runs `init --no-daemon` to generate certs
- `start_test_daemon(config_dir, port)` — starts a daemon and waits for IPC socket readiness; returns `DaemonGuard` (kills on drop)
- `ComposeGuard` — runs `docker compose down` and cleans up temp dir on drop
- All test isolation via `DEVPROXY_CONFIG_DIR` env var and unique ephemeral ports

**Estimated complexity:** Low-medium. The harness is mostly process management and temp directory handling. The implementation plan provides complete code.

**Which tests depend on it:** Tests 1-8 (all scenario and integration tests). Tests 9-17 (boundary/error/unit) do not need the full harness.

### Test Fixture (`tests/fixtures/`)

**What it does:** A minimal Docker Compose project with a Python HTTP server that responds on port 3000.

**Contents:**
- `Dockerfile` — `python:3.12-alpine` running `python -m http.server 3000`
- `docker-compose.yml` — single `web` service with `devproxy.port=3000` label

**Estimated complexity:** Trivial.

---

## Test Plan

### Scenario Tests

#### 1. Full e2e workflow: init, up, curl through proxy, ls, status, down

- **Name:** User can init, start a project, access it via HTTPS proxy, list it, check status, and stop it
- **Type:** scenario
- **Harness:** E2E harness (create_test_config_dir, start_test_daemon, copy_fixtures, ComposeGuard)
- **Preconditions:** Docker running. No prior devproxy state. Binary compiled.
- **Actions:**
  1. `devproxy init --domain test.devproxy.dev --no-daemon` (via create_test_config_dir)
  2. Start daemon on ephemeral port (via start_test_daemon)
  3. Copy fixtures to isolated temp dir
  4. `docker compose build` in fixtures dir
  5. `devproxy up` in fixtures dir (with DEVPROXY_CONFIG_DIR set)
  6. Verify `.devproxy-project` file exists and contains the slug
  7. Wait 3s for container readiness
  8. `devproxy status` — verify output contains "running"
  9. `devproxy ls` — verify output contains the slug
  10. `curl -s -f --max-time 5 --resolve <slug>.test.devproxy.dev:<port>:127.0.0.1 --cacert <ca-cert> https://<slug>.test.devproxy.dev:<port>/` — verify HTTP 200
  11. `devproxy down` in fixtures dir — verify success
  12. Verify `.devproxy-project` and `.devproxy-override.yml` are removed
- **Expected outcome:**
  - `init` exits 0, creates ca-cert.pem, ca-key.pem, tls-cert.pem, tls-key.pem, config.json (spec: "Generates a local CA and wildcard TLS cert")
  - `up` exits 0, prints URL `https://<slug>.test.devproxy.dev`, writes `.devproxy-project` with slug (spec: "devproxy up … assign slug, bind random host port, docker compose up")
  - `status` reports daemon running (spec: "Show daemon health")
  - `ls` lists slug and port (spec: "List all running projects with slugs and URLs")
  - curl returns HTTP 200 through the HTTPS proxy (spec: "reachable at https://<slug>.mysite.dev")
  - `down` exits 0, removes `.devproxy-override.yml` and `.devproxy-project` (spec: "compose down + remove override file")
- **Interactions:** Docker Compose (build, up, down), Docker events (daemon route registration), TLS (cert validation), DNS (bypassed via --resolve)

#### 2. Self-healing: externally killed container removes route from daemon

- **Name:** Route is automatically removed when a container is killed outside of devproxy
- **Type:** scenario
- **Harness:** E2E harness (full setup: config, daemon, fixtures, compose up)
- **Preconditions:** Daemon running. Project up with active route visible in `ls`.
- **Actions:**
  1. Set up daemon and run `devproxy up` (same as test 1 steps 1-7)
  2. `devproxy ls` — verify route is present
  3. `docker compose --project-name <slug> kill` (kill container externally, NOT via devproxy)
  4. Wait 3s for Docker event watcher to process die event
  5. `devproxy ls` — check route list
- **Expected outcome:**
  - After external kill, `ls` no longer shows the slug, or shows "no active projects" (spec: "container die → remove route from Router", "proxy daemon self-heals: it watches Docker events and needs no manual cleanup")
- **Interactions:** Docker events stream (`docker events --filter label=devproxy.port`), daemon event processing

#### 3. Daemon restart rebuilds routes from running containers

- **Name:** Restarting the daemon rediscovers routes from containers that are still running
- **Type:** scenario
- **Harness:** E2E harness (full setup)
- **Preconditions:** Daemon running. Project up with container running.
- **Actions:**
  1. Set up daemon and run `devproxy up` (same as test 1 steps 1-7)
  2. Wait for route to appear in `ls`
  3. Kill the daemon process (not the container)
  4. Wait 500ms
  5. Start a new daemon on a fresh ephemeral port (using same config dir)
  6. `devproxy ls` — check route list
- **Expected outcome:**
  - The new daemon's `ls` output contains the original slug, proving routes were rebuilt from `docker ps --filter label=devproxy.port` (spec: "On daemon restart it re-runs docker ps --filter label=devproxy.port to rebuild from scratch")
- **Interactions:** Docker ps (container discovery), Docker inspect (route extraction), daemon startup sequence

### Integration Tests

#### 4. IPC ping/pong between CLI and daemon

- **Name:** `devproxy status` successfully communicates with a running daemon via IPC
- **Type:** integration
- **Harness:** E2E harness (config dir + daemon, no compose project needed)
- **Preconditions:** Daemon running on ephemeral port. No compose projects up.
- **Actions:**
  1. `devproxy status` with DEVPROXY_CONFIG_DIR set
- **Expected outcome:**
  - Exit code 0. Output contains "running" and "active routes: 0" (spec: IPC `{"cmd":"ping"}` → `{"status":"pong"}`)
- **Interactions:** Unix domain socket IPC, JSON serialization/deserialization

#### 5. IPC list returns correct routes after up

- **Name:** `devproxy ls` shows the correct slug and port after starting a project
- **Type:** integration
- **Harness:** E2E harness (full setup with compose project)
- **Preconditions:** Daemon running. One project up.
- **Actions:**
  1. Set up and run `devproxy up`
  2. `devproxy ls`
- **Expected outcome:**
  - Output shows `<slug>.test.devproxy.dev` and a port number (spec: IPC `{"cmd":"list"}` → `{"status":"routes","routes":[...]}`)
- **Interactions:** IPC, Docker event processing (route must have been registered before ls is called)

#### 6. TLS certificate chain is valid for wildcard domain

- **Name:** Generated certs form a valid chain that curl accepts for any subdomain
- **Type:** integration
- **Harness:** E2E harness (config dir + daemon + compose project)
- **Preconditions:** `init` has generated certs. Daemon running with a routed project.
- **Actions:**
  1. curl with `--cacert <ca-cert.pem>` to `https://<slug>.test.devproxy.dev:<port>/`
- **Expected outcome:**
  - curl exits 0 (no TLS errors). The wildcard cert (`*.test.devproxy.dev`) is accepted by curl when the CA cert is provided (spec: "Generates a local CA and wildcard TLS cert using rcgen")
- **Interactions:** rcgen cert generation, rustls TLS termination, curl TLS validation
- **Note:** This is implicitly covered by test 1's curl step, but the assertion focus here is TLS validity, not HTTP content.

#### 7. Docker Compose override file binds the correct port

- **Name:** `devproxy up` generates a valid override file that Docker Compose accepts
- **Type:** integration
- **Harness:** E2E harness (config dir + fixtures copy, no daemon needed for this check)
- **Preconditions:** Config exists. Fixtures copied.
- **Actions:**
  1. `devproxy up` in fixtures dir
  2. Read `.devproxy-override.yml`
  3. Parse as YAML, verify structure
- **Expected outcome:**
  - File exists, contains `services.web.ports` with a mapping `"<host-port>:3000"` where host-port is a valid port number (spec: override file format in "Port binding" section)
- **Interactions:** config::find_free_port, config::write_override_file, Docker Compose (accepts the override)

### Boundary and Edge-Case Tests

#### 8. `init` is idempotent — running twice does not error or regenerate certs

- **Name:** Running `devproxy init` twice succeeds and does not overwrite existing certs
- **Type:** boundary
- **Harness:** Direct binary invocation with isolated config dir (no daemon needed, uses --no-daemon)
- **Preconditions:** Empty temp config dir.
- **Actions:**
  1. `devproxy init --domain test.devproxy.dev --no-daemon`
  2. Record file sizes/mtimes of cert files
  3. `devproxy init --domain test.devproxy.dev --no-daemon`
  4. Check stderr output and cert files
- **Expected outcome:**
  - Both invocations exit 0. Second invocation's stderr contains "already exists" for both CA and TLS certs. Cert files are not modified on second run. (Spec: implied by init being safe to re-run; implementation plan: "Idempotent: skips CA if it exists, skips wildcard cert if it exists")
- **Interactions:** Filesystem (file existence checks)

#### 9. `devproxy status` reports daemon not running when no daemon is started

- **Name:** `status` gracefully reports when daemon is not running
- **Type:** boundary
- **Harness:** Direct binary invocation with isolated config dir (no daemon)
- **Preconditions:** Config dir exists with config.json but no daemon running (no socket file).
- **Actions:**
  1. `devproxy status`
- **Expected outcome:**
  - Output contains "not running" or "could not connect" (spec: "Show daemon health" — should handle the unhealthy case gracefully)
- **Interactions:** Unix domain socket (connection failure path)

#### 10. `devproxy up` fails with clear error when no `devproxy.port` label exists

- **Name:** `up` errors clearly when compose file has no devproxy.port label
- **Type:** boundary
- **Harness:** Direct binary invocation with isolated config dir and temp compose dir
- **Preconditions:** Config dir with config.json. Temp dir with `docker-compose.yml` containing a service without `devproxy.port` label.
- **Actions:**
  1. `devproxy up` in the temp dir
- **Expected outcome:**
  - Exit code non-zero. Stderr contains "no service" indicating no devproxy.port label was found (spec: "One label in docker-compose.yml is all a project needs to opt in" — absence should error)
- **Interactions:** YAML parsing, label detection

#### 11. `devproxy up` fails when no compose file exists in current directory

- **Name:** `up` errors clearly when no docker-compose.yml is found
- **Type:** boundary
- **Harness:** Direct binary invocation with isolated config dir and empty temp dir
- **Preconditions:** Config dir with config.json. Empty temp directory.
- **Actions:**
  1. `devproxy up` in the empty temp dir
- **Expected outcome:**
  - Exit code non-zero. Stderr contains "no docker-compose.yml" (spec: implied — compose file is required for `up`)
- **Interactions:** Filesystem (compose file search)

#### 12. `devproxy down` fails with clear error when no `.devproxy-project` file exists

- **Name:** `down` errors clearly when project was not started via devproxy
- **Type:** boundary
- **Harness:** Direct binary invocation with temp dir containing a compose file but no `.devproxy-project`
- **Preconditions:** Temp dir with docker-compose.yml but no `.devproxy-project` file.
- **Actions:**
  1. `devproxy down` in the temp dir
- **Expected outcome:**
  - Exit code non-zero. Stderr contains ".devproxy-project" or "Is this project running" (spec: implied — down needs the project slug to target the correct compose project)
- **Interactions:** Filesystem (project file read)

#### 13. Proxy returns 502 for unknown host

- **Name:** Requests to an unregistered subdomain get a 502 error
- **Type:** boundary
- **Harness:** E2E harness (config dir + daemon, no compose project needed)
- **Preconditions:** Daemon running on ephemeral port. No routes registered.
- **Actions:**
  1. `curl --resolve nonexistent.test.devproxy.dev:<port>:127.0.0.1 --cacert <ca-cert> https://nonexistent.test.devproxy.dev:<port>/`
- **Expected outcome:**
  - curl receives HTTP 502. Response body contains "no route for host" (spec: "reads Host header → Router lookup → hyper reverse proxy" — missing route should return error, not crash)
- **Interactions:** TLS termination, HTTP request handling, router lookup

### Invariant Tests

#### 14. Cleanup always happens on test teardown

- **Name:** Test harness guards always clean up Docker resources and temp directories
- **Type:** invariant
- **Harness:** Built into the RAII guards (DaemonGuard, ComposeGuard)
- **Preconditions:** Any test that uses Docker.
- **Actions:**
  - DaemonGuard::drop kills daemon process and removes config dir
  - ComposeGuard::drop runs `docker compose --project-name <slug> down --remove-orphans` and removes fixtures copy
- **Expected outcome:**
  - After any test (pass or fail or panic), no orphaned daemon processes, no orphaned containers, no orphaned temp directories. Verified by the RAII pattern — drop is called even on panic.
- **Interactions:** Process management, Docker Compose lifecycle
- **Note:** This is a design invariant of the harness, not a separate test to run. It is verified by the absence of leaked resources across the test suite.

### Unit Tests

#### 15. Slug generator produces `adjective-animal` format

- **Name:** Generated slugs follow the adjective-animal format
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/slugs.rs`
- **Preconditions:** None.
- **Actions:**
  1. Call `generate_slug()`
  2. Split on '-', verify 2 parts
  3. Verify first part is in ADJECTIVES list, second in ANIMALS list
- **Expected outcome:**
  - Slug matches `<adjective>-<animal>` format (spec: "Slugs are random, human-readable animal names (e.g. swift-penguin)")
- **Interactions:** None (pure function)

#### 16. Slug generator produces variety (not always the same slug)

- **Name:** Multiple slug generations produce different values
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/slugs.rs`
- **Preconditions:** None.
- **Actions:**
  1. Generate 20 slugs
  2. Count unique values
- **Expected outcome:**
  - More than 1 unique slug across 20 generations (spec: "random" — should not be deterministic)
- **Interactions:** None (uses rand RNG)

#### 17. Config labels parsed as map format

- **Name:** Compose labels in map format (`devproxy.port: 3000`) are correctly parsed
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/config.rs`
- **Preconditions:** None.
- **Actions:**
  1. Parse YAML string with `labels: { devproxy.port: 3000 }`
  2. Call `find_devproxy_service()`
- **Expected outcome:**
  - Returns service name and port 3000 (spec: compose file label format)
- **Interactions:** serde_yaml deserialization

#### 18. Config labels parsed as list format

- **Name:** Compose labels in list format (`- "devproxy.port=3000"`) are correctly parsed
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/config.rs`
- **Preconditions:** None.
- **Actions:**
  1. Parse YAML string with `labels: ["devproxy.port=3000"]`
  2. Call `find_devproxy_service()`
- **Expected outcome:**
  - Returns service name and port 3000 (spec: Docker Compose supports both label formats)
- **Interactions:** serde_yaml deserialization

#### 19. Multiple devproxy.port labels rejected

- **Name:** Error when multiple services have devproxy.port labels
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/config.rs`
- **Preconditions:** None.
- **Actions:**
  1. Parse YAML with two services both having `devproxy.port`
  2. Call `find_devproxy_service()`
- **Expected outcome:**
  - Returns error containing "multiple" (spec: "spec currently errors if more than one service has devproxy.port")
- **Interactions:** None

#### 20. IPC request/response serialization roundtrips

- **Name:** IPC message types serialize and deserialize correctly
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/ipc.rs`
- **Preconditions:** None.
- **Actions:**
  1. Serialize Ping request, verify JSON
  2. Serialize List request, verify JSON
  3. Deserialize Pong response
  4. Deserialize Routes response with route data
- **Expected outcome:**
  - Ping → `{"cmd":"ping"}`, List → `{"cmd":"list"}`, Pong ← `{"status":"pong"}`, Routes ← correct structure (spec: IPC protocol in spec)
- **Interactions:** serde_json

#### 21. Router insert/get/remove/list operations

- **Name:** Router correctly manages routes with domain-qualified hostnames
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/proxy/router.rs`
- **Preconditions:** None.
- **Actions:**
  1. Create Router with domain "mysite.dev"
  2. Insert "swift-penguin" → port 51234
  3. Get "swift-penguin.mysite.dev" → Some(51234)
  4. Get "nonexistent.mysite.dev" → None
  5. Insert "calm-otter" → port 51235
  6. List → 2 routes
  7. Remove "swift-penguin"
  8. Get "swift-penguin.mysite.dev" → None
- **Expected outcome:**
  - All assertions pass. Router correctly qualifies slugs with domain, handles missing keys, and supports multiple concurrent routes. (Spec: "Arc<RwLock<HashMap<host, port>>> with get/insert/remove/list")
- **Interactions:** None (in-memory data structure)

#### 22. Certificate generation produces valid PEM

- **Name:** CA and wildcard cert generation produces parseable PEM output
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/proxy/cert.rs`
- **Preconditions:** None.
- **Actions:**
  1. Generate CA cert and key
  2. Verify both contain PEM markers
  3. Generate wildcard cert signed by the CA
  4. Verify both contain PEM markers
  5. Load the cert+key pair into a rustls ServerConfig
- **Expected outcome:**
  - All PEM strings contain correct BEGIN/END markers. ServerConfig loads successfully, proving the cert chain is valid for rustls. (Spec: "rcgen CA + wildcard cert generation")
- **Interactions:** rcgen (generation), rustls (validation)

#### 23. Project file roundtrip (write then read)

- **Name:** Writing and reading `.devproxy-project` preserves the slug
- **Type:** unit
- **Harness:** Rust `#[test]` in `src/config.rs`
- **Preconditions:** Temp directory.
- **Actions:**
  1. `write_project_file(dir, "swift-penguin")`
  2. `read_project_file(dir)` → "swift-penguin"
- **Expected outcome:**
  - Slug is preserved exactly through write/read cycle (spec: `.devproxy-project` tracking file)
- **Interactions:** Filesystem

#### 24. CLI help shows all user-facing commands (Daemon hidden)

- **Name:** `--help` displays all public commands and hides the daemon command
- **Type:** unit
- **Harness:** Direct binary invocation
- **Preconditions:** Binary compiled.
- **Actions:**
  1. `devproxy --help`
- **Expected outcome:**
  - Output contains: init, up, down, ls, open, status. Output does NOT contain "daemon" (spec: command table lists 6 commands; daemon is "internal, hidden")
- **Interactions:** clap help generation

---

## Coverage Summary

### Covered

| Action surface area | Tests covering it |
|---|---|
| `devproxy init` (cert gen, CA trust, config save, idempotency) | 1, 8 |
| `devproxy up` (compose parse, label detect, port bind, override write, project file, compose up) | 1, 7, 10, 11 |
| `devproxy down` (project file read, compose down, file cleanup) | 1, 12 |
| `devproxy ls` (IPC list, route display) | 1, 5 |
| `devproxy status` (IPC ping, health report) | 1, 4, 9 |
| `devproxy open` (project file read, route verify, browser launch) | Not directly tested (see exclusions) |
| `devproxy daemon` (HTTPS proxy, Docker watcher, IPC server) | 1, 2, 3, 4, 5, 6, 13 |
| Self-healing (container die → route removed) | 2 |
| Daemon restart (routes rebuilt from running containers) | 3 |
| TLS cert chain validity | 6, 22 |
| Slug generation | 15, 16 |
| Config/compose parsing | 17, 18, 19 |
| IPC protocol | 20 |
| Router data structure | 21 |
| Project file management | 23 |
| CLI surface | 24 |
| Error: no devproxy.port label | 10, 19 |
| Error: no compose file | 11 |
| Error: no project file for down | 12 |
| Error: daemon not running | 9 |
| Error: unknown host → 502 | 13 |

### Explicitly Excluded (per agreed strategy)

| Area | Reason | Risk |
|---|---|---|
| `devproxy open` (browser launch) | Cannot verify browser launch in CI/headless test environment. The IPC and project file logic it depends on are covered by other tests. | Low — `open::that()` is a thin wrapper over OS APIs. The risky parts (project file read, IPC query) are tested. |
| System keychain trust (`security add-trusted-cert`) | Requires sudo and modifies system state. Not safe for automated tests. | Low — the command is a simple shell-out. The cert generation that feeds it is tested. |
| DNS resolution (dnsmasq/hosts) | Tests use `--resolve` to bypass DNS entirely. DNS setup is a one-time manual step per the spec. | None for automated testing. DNS misconfiguration would be caught by user during manual setup. |
| Linux-specific code paths | Tests run on macOS (current platform). Linux paths for cert trust and privileged ports are not exercised. | Medium — Linux cert trust uses different commands. Mitigated by the code being simple shell-outs with clear error messages. |
| Performance | No performance-sensitive operations identified. The proxy adds one TCP hop on localhost. | Low — latency on localhost is negligible for dev workflows. |
