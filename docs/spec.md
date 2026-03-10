# devproxy — Specification v0.1

A single Rust binary that provides local HTTPS dev subdomains for Docker Compose
projects, with no external proxy (no Caddy, no Traefik) and no mkcert dependency.

---

## Goals

- One label in `docker-compose.yml` is all a project needs to opt in
- `devproxy up` starts a project and makes it reachable at `https://<slug>.mysite.dev`
- Slugs are random, human-readable animal names (e.g. `swift-penguin`)
- The proxy daemon self-heals: it watches Docker events and needs no manual cleanup
- No persistent state file — Docker is the source of truth
- Works on macOS and Linux

---

## User-facing workflow

### One-time setup

```bash
devproxy init --domain mysite.dev
```

- Generates a local CA and wildcard TLS cert using `rcgen` (pure Rust, no mkcert)
- Trusts the CA in the OS certificate store (login keychain on macOS, `update-ca-certificates` on Linux)
- Spawns the proxy daemon in the background
- Prints instructions for wildcard DNS (dnsmasq or `/etc/hosts`)

### Per-project

```yaml
# docker-compose.yml — only one devproxy-specific label needed
services:
  web:
    build: .
    labels:
      - devproxy.port=3000

  pg:
    image: postgres:18       # no ports:, no networks: — stays private

  redis:
    image: redis:7-alpine    # same
```

```bash
devproxy up           # assign slug, bind random host port, docker compose up
# → https://swift-penguin.mysite.dev

docker compose down   # or devproxy down — proxy self-heals either way
```

### Other commands

| Command              | Description                                           |
|----------------------|-------------------------------------------------------|
| `devproxy init`      | One-time setup: certs, CA trust, daemon               |
| `devproxy up`        | Start this project, assign slug, write port override  |
| `devproxy down`      | Convenience: compose down + remove override file      |
| `devproxy ls`        | List all running projects with slugs and URLs         |
| `devproxy open`      | Open this project's URL in the browser                |
| `devproxy status`    | Show daemon health and active route count             |
| `devproxy update`    | Check for updates and self-update the binary          |
| `devproxy --version` | Show installed version                                |

---

## Architecture

```
devproxy daemon  (one process, two async tasks via tokio::join!)
│
├── Socket activation (port binding)
│     macOS: launchd passes pre-bound fd via launch_activate_socket()
│     Linux: systemd passes fd via LISTEN_FDS/LISTEN_PID env vars
│     Fallback: direct TcpListener::bind() for dev/test
│
├── HTTPS proxy  :443
│     tokio-rustls accept loop
│     reads Host header → Router lookup → hyper reverse proxy → 127.0.0.1:<host-port>
│
└── Docker event watcher
      streams `docker events --filter label=devproxy.port`
      container start → inspect → insert route into Router
      container die   → remove route from Router
```

The daemon runs as the current user (no sudo). On macOS, `devproxy init` installs a
LaunchAgent plist; launchd binds port 443 and passes the fd. On Linux, systemd user socket
activation does the same. Falls back to `setcap cap_net_bind_service` if systemd is unavailable.

### How routing works

- The daemon watches Docker for any container that carries the `devproxy.port` label
- On `start` it inspects the container to get:
  - `com.docker.compose.project` → the **slug** (set via `--project-name` in `devproxy up`)
  - `devproxy.port` → the container-side port
  - `NetworkSettings.Ports` → the bound **host port** (the random one from the override)
- It inserts `<slug>.<domain> → host-port` into the in-memory Router
- On `die` it removes the route
- On daemon restart it re-runs `docker ps --filter label=devproxy.port` to rebuild from scratch

No state file is written. Docker is the source of truth.

### Port binding

`devproxy up` finds a free ephemeral port by binding `:0` and immediately releasing it,
then writes a minimal override file that binds that port:

```yaml
# .devproxy-override.yml  (generated, safe to .gitignore)
services:
  web:
    ports:
      - "51234:3000"
```

`docker compose` is then invoked with both files:

```bash
docker compose -f docker-compose.yml -f .devproxy-override.yml \
  --project-name swift-penguin up -d
```

### IPC (CLI → daemon)

A Unix domain socket at `~/.config/devproxy/devproxy.sock`.
The CLI only needs two commands (routing is handled by Docker event watch):

```
→ {"cmd": "ping"}
← {"status": "pong"}

→ {"cmd": "list"}
← {"status": "routes", "routes": [{"slug": "swift-penguin.mysite.dev", "port": 51234}]}
```

---

## Module layout

```
src/
├── main.rs               — tokio::main, clap dispatch
├── cli.rs                — clap Command definitions
├── config.rs             — Config (domain), ComposeFile parsing, Labels enum
├── slugs.rs              — random adjective-animal generator
├── ipc.rs                — Unix socket client (send/recv one JSON-line message)
├── platform.rs           — launchd/systemd daemon lifecycle (install, stop, restart)
│
├── proxy/
│   ├── mod.rs            — run_daemon(): joins https_proxy + docker_watcher tasks
│   ├── socket_activation.rs — fd acquisition from launchd/systemd
│   ├── cert.rs           — rcgen CA + wildcard cert generation, OS trust
│   ├── router.rs         — Arc<RwLock<HashMap<host, port>>> with get/insert/remove/list
│   └── docker.rs         — load_routes() + watch_events() via `docker events`
│
└── commands/
    ├── mod.rs
    ├── init.rs            — cert gen, CA trust, platform daemon install (or fallback spawn)
    ├── up.rs              — detect label, free port, write override, compose up
    ├── down.rs            — compose down, remove override file
    ├── ls.rs              — ipc List → pretty table
    ├── open.rs            — ipc List → open URL
    ├── status.rs          — ipc Ping
    ├── update.rs          — self-update from GitHub releases
    └── daemon.rs          — proxy::run_daemon() entry point (hidden subcommand)
```

---

## Dependencies

See `Cargo.toml` for exact versions. Key crates:

```toml
clap, tokio, hyper (1.x), hyper-util, tokio-rustls, rustls,
rcgen, serde, serde_json, serde_yaml, anyhow, colored, dirs,
libc, semver
```

No Caddy. No Traefik. No mkcert. No external proxy process.

---

## Open questions / future work

- ~~**Daemon persistence**: A launchd plist (macOS) or systemd unit (Linux) would survive reboots.~~ **Done** — `devproxy init` installs a LaunchAgent plist (macOS) or systemd user socket + service units (Linux). The daemon runs as the current user via socket activation.
- ~~**Linux privileged port**: binding :443 requires `cap_net_bind_service` on Linux.~~ **Done** — systemd socket activation handles this. Falls back to `setcap cap_net_bind_service=+ep` when systemd is unavailable.
- **Port TOCTOU**: the free-port trick (bind :0, release, use the number) has a small race.
  Acceptable for local dev; could be eliminated by letting the daemon allocate ports.
- **Multiple services**: spec currently errors if more than one service has `devproxy.port`.
  Could support `devproxy.port=3000,devproxy.name=api` to allow multiple routes per project.
- **HTTP → HTTPS redirect**: daemon currently only listens on :443. Add an :80 listener that
  301-redirects to HTTPS.
- ~~**Slug persistence**: slugs are stable for the lifetime of a running container but reset on
  `devproxy up`. Could offer `devproxy pin <slug>` to write the slug into a `.devproxy` file
  so the same slug is always used for a given project.~~ **Done** — `devproxy up --slug NAME` allows predictable slugs. `devproxy stop`/`start` preserves the slug across stop/start cycles without regenerating.
