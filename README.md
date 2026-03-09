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
# → https://swift-penguin.mysite.dev
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

| Command              | Description                                  |
|----------------------|----------------------------------------------|
| `devproxy init`      | One-time setup: certs, CA trust, daemon      |
| `devproxy up`        | Start project, assign slug, proxy it         |
| `devproxy down`      | Stop project, clean up override              |
| `devproxy ls`        | List running projects with URLs              |
| `devproxy open`      | Open project URL in browser                  |
| `devproxy status`    | Daemon health check                          |
| `devproxy update`    | Check for updates and self-update            |
| `devproxy --version` | Show installed version                       |

## Development

```bash
just setup    # bootstrap: cargo build
just dev      # cargo watch for dev
just check    # clippy + tests
```

## How It Works

See [docs/spec.md](docs/spec.md) for the full specification.
