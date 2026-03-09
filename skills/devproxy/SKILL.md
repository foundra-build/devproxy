---
name: devproxy
description: Use when a Docker Compose project needs HTTPS dev subdomains, when setting up local dev environments with TLS, or when the user mentions devproxy, dev subdomains, or local HTTPS proxy
---

# devproxy

Local HTTPS dev subdomains for Docker Compose projects. Single Rust binary — no Caddy, Traefik, nginx, or mkcert.

## When to Use

- User has a Docker Compose project and wants HTTPS locally
- User mentions "devproxy", "dev subdomain", or "local HTTPS"
- Setting up a new project that needs a dev URL like `https://slug.mysite.dev`
- User asks about routing Docker services through HTTPS

**Prerequisites:** Docker and Docker Compose must be installed.

## Quick Start (Minimum Viable Setup)

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

To update an existing installation:

```bash
devproxy update
```

### 2. One-time init

```bash
devproxy init --domain mysite.dev
```

This generates a local CA + wildcard TLS cert, trusts the CA in the system keychain, and starts the background daemon. On macOS, the daemon runs via launchd socket activation as your user — **no sudo required**. On Linux, it uses systemd socket activation (or setcap fallback).

**DNS setup (required):** Wildcard DNS must point `*.mysite.dev` to 127.0.0.1. `/etc/hosts` does NOT support wildcards, so use dnsmasq:

```bash
# macOS (via Homebrew)
brew install dnsmasq
echo "address=/mysite.dev/127.0.0.1" >> $(brew --prefix)/etc/dnsmasq.conf
sudo brew services restart dnsmasq
sudo mkdir -p /etc/resolver
echo "nameserver 127.0.0.1" | sudo tee /etc/resolver/mysite.dev
```

For a single project, a manual `/etc/hosts` entry works: `127.0.0.1 swift-penguin.mysite.dev`

### 3. Add one label to docker-compose.yml

```yaml
services:
  web:
    build: .
    labels:
      - devproxy.port=3000   # the container-side port to proxy
```

The port value is the **container-side** port the service listens on (e.g., 80 for nginx, 3000 for Node, 8080 for Spring). Only the service that serves HTTP needs the label. Database, cache, etc. stay private — no `ports:` needed.

### 4. Start (from the project directory containing docker-compose.yml)

```bash
devproxy up
# => https://swift-penguin.mysite.dev
```

Add `.devproxy-override.yml` to your `.gitignore` — it's a generated port-mapping file.

The proxy daemon watches Docker events and self-heals — `docker compose down` or `devproxy down` both work.

### 5. Verify

```bash
devproxy status   # daemon running?
devproxy ls       # route registered?
devproxy open     # opens URL in browser
```

## Commands Reference

| Command                          | What it does                                    |
|----------------------------------|-------------------------------------------------|
| `devproxy init --domain X`       | One-time: certs, CA trust, start daemon         |
| `devproxy init --port 8443`      | Use non-privileged port (avoids sudo on Linux)  |
| `devproxy up`                    | Assign slug, bind port, `docker compose up -d`  |
| `devproxy down`                  | `docker compose down` + remove override file    |
| `devproxy ls`                    | List running projects with slugs and URLs       |
| `devproxy open`                  | Open this project's URL in browser              |
| `devproxy update`                | Check for updates and self-update the binary    |
| `devproxy --version`             | Show installed version                          |
| `devproxy status`                | Daemon health + active route count              |

## How It Works (For Debugging)

- `devproxy up` finds a free host port, writes `.devproxy-override.yml` (port mapping), and runs `docker compose -f docker-compose.yml -f .devproxy-override.yml --project-name <slug> up -d`
- The daemon listens on :443 (HTTPS), reads the `Host` header, looks up the slug in an in-memory router, and reverse-proxies to `127.0.0.1:<host-port>`
- The daemon watches `docker events --filter label=devproxy.port` — container start inserts a route, container die removes it
- On daemon restart, it rebuilds routes from `docker ps`
- No state files — Docker is the source of truth
- Config lives at `~/.config/devproxy/` (certs, socket, config)

### Daemon Lifecycle

- **macOS**: `devproxy init` installs a LaunchAgent plist at `~/Library/LaunchAgents/com.devproxy.daemon.plist`. launchd binds port 443 and passes the socket fd to the daemon, which runs as your user (no sudo).
- **Linux**: Uses systemd user socket activation (`~/.config/systemd/user/devproxy.socket` + `devproxy.service`). Falls back to `setcap cap_net_bind_service` if systemd is unavailable.
- `devproxy update` replaces the binary and uses `launchctl kickstart -k` (macOS) or `systemctl --user restart` (Linux) to restart the daemon with the new version.
- `sudo` is only needed for one-time DNS setup and CA trust — never for daemon startup.

## Common Issues

| Problem | Fix |
|---------|-----|
| "Connection refused" on HTTPS | Check daemon: `devproxy status`. Restart with `devproxy init` |
| Port 443 requires sudo (Linux) | Normally handled by systemd socket activation. Fallback: `sudo setcap cap_net_bind_service=+ep $(which devproxy)` or use `devproxy init --port 8443` |
| DNS not resolving `*.mysite.dev` | Add `127.0.0.1 slug.mysite.dev` to `/etc/hosts` or configure dnsmasq |
| `.devproxy-override.yml` in git | Add it to `.gitignore` |
| Slug changed after restart | Slugs are random per `devproxy up`. Pin not yet supported |
| Binary "killed" (exit code 137) on macOS | Gatekeeper quarantine. Re-run the install script or run: `xattr -cr $(which devproxy) && codesign --force --sign - $(which devproxy)` |
