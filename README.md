# devproxy

Local HTTPS dev subdomains for Docker Compose projects. One label, one command, real TLS.

```yaml
# docker-compose.yml
services:
  web:
    build: .
    labels:
      - devproxy.port=3000
```

```bash
devproxy up
# → https://swift-penguin-myapp.mysite.dev

devproxy up --slug my-app
# → https://my-app-myapp.mysite.dev
```

## Features

- **Zero config** — one Docker label is all you need
- **Real HTTPS** — auto-generated CA + wildcard cert via `rcgen` (no mkcert)
- **Self-healing** — watches Docker events, no manual cleanup
- **No external proxy** — single Rust binary, no Caddy/Traefik/nginx
- **Human-readable slugs** — random adjective-animal subdomains

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

To install a specific version:

```bash
export DEVPROXY_VERSION=v0.0.1
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

## Quick Start

```bash
# one-time setup (no sudo needed — uses launchd/systemd socket activation)
devproxy init --domain mysite.dev

# in any project with devproxy.port label
devproxy up
```

## Commands

| Command              | Description                                       |
|----------------------|---------------------------------------------------|
| `devproxy init`      | One-time setup: certs, CA trust, daemon            |
| `devproxy up`        | Start project, assign slug, proxy it               |
| `devproxy up --slug` | Start project with a custom slug prefix            |
| `devproxy down`      | Stop project, remove override and slug             |
| `devproxy stop`      | Stop containers (preserves slug for restart)       |
| `devproxy start`     | Start previously stopped containers                |
| `devproxy restart`   | Restart app containers                             |
| `devproxy ls`        | List running projects with URLs                    |
| `devproxy open`      | Open project URL in browser                        |
| `devproxy status`    | Daemon health check                                |
| `devproxy daemon restart` | Restart the background daemon               |
| `devproxy update`    | Check for updates and self-update                  |
| `devproxy --version` | Show installed version                             |

## Claude Code Plugin

devproxy includes a [Claude Code](https://claude.com/claude-code) plugin with skills for guided setup and usage help.

### Install the plugin

```
/plugin marketplace add foundra-build/devproxy
/plugin install devproxy@devproxy
```

### Available skills

| Skill | Trigger | What it does |
|-------|---------|--------------|
| `devproxy` | Mention "devproxy", Docker HTTPS, dev subdomains | Commands reference, troubleshooting, how-it-works |
| `setup` | "set up devproxy", "install devproxy" | Guided interactive walkthrough for first-time setup |

Use `/devproxy:setup` for a step-by-step guided installation, or just ask about devproxy and the general skill will activate automatically.

## Development

```bash
just setup    # bootstrap: cargo build
just dev      # cargo watch for dev
just check    # clippy + tests
```

## How It Works

See [docs/spec.md](docs/spec.md) for the full specification.
