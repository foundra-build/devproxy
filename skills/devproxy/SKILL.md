---
name: devproxy
description: This skill should be used when the user mentions "devproxy", "dev subdomain", "local HTTPS proxy", asks about HTTPS for Docker Compose, wants to route Docker services through HTTPS subdomains, needs to troubleshoot devproxy issues like certificate errors or daemon problems, or asks about devproxy commands like "devproxy up", "devproxy down", "devproxy ls", "devproxy status", "devproxy init", "devproxy open", or "devproxy update".
---

# devproxy

Local HTTPS dev subdomains for Docker Compose projects. Single Rust binary — no Caddy, Traefik, nginx, or mkcert.

**Prerequisites:** Docker and Docker Compose must be installed. For first-time setup, use the `setup` skill (`/devproxy:setup`).

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

## Per-Project Usage

Add one label to `docker-compose.yml`:

```yaml
services:
  web:
    build: .
    labels:
      - devproxy.port=3000   # the container-side port to proxy
```

The port value is the **container-side** port the service listens on (e.g., 80 for nginx, 3000 for Node, 8080 for Spring). Only the service that serves HTTP needs the label. Database, cache, etc. stay private — no `ports:` needed.

Start from the project directory:

```bash
devproxy up
# => https://swift-penguin.mysite.dev
```

Add `.devproxy-override.yml` to `.gitignore` — it's a generated port-mapping file.

Verify:

```bash
devproxy status   # daemon running?
devproxy ls       # route registered?
devproxy open     # opens URL in browser
```

**Testing with curl on macOS:** `curl` does not use `/etc/resolver/` for DNS, so bare `curl https://<slug>.<domain>` will fail with exit code 6 even though browsers work fine. Use `--resolve` to bypass DNS:

```bash
curl --resolve <slug>.<domain>:443:127.0.0.1 https://<slug>.<domain>
```

## How It Works

- `devproxy up` finds a free host port, writes `.devproxy-override.yml`, and runs `docker compose -f docker-compose.yml -f .devproxy-override.yml --project-name <slug> up -d`
- The daemon listens on :443 (HTTPS), reads the `Host` header, looks up the slug, and reverse-proxies to `127.0.0.1:<host-port>`
- The daemon watches `docker events --filter label=devproxy.port` — container start inserts a route, container die removes it
- On daemon restart, routes rebuild from `docker ps`
- No state files — Docker is the source of truth
- Config lives at `~/.config/devproxy/` (certs, socket, config)

### Daemon Lifecycle

- **macOS**: `devproxy init` installs a LaunchAgent plist. launchd binds port 443 and passes the socket fd to the daemon running as the current user (no sudo).
- **Linux**: Uses systemd user socket activation. Falls back to `setcap cap_net_bind_service` if systemd is unavailable.
- `devproxy update` replaces the binary and restarts the daemon via `launchctl kickstart -k` (macOS) or `systemctl --user restart` (Linux).
- `sudo` is only needed for one-time DNS setup and CA trust — never for daemon startup.

## Common Issues

| Problem | Fix |
|---------|-----|
| "Connection refused" on HTTPS | Check daemon: `devproxy status`. Restart with `devproxy init` |
| Port 443 requires sudo (Linux) | Normally handled by systemd socket activation. Fallback: `sudo setcap cap_net_bind_service=+ep $(which devproxy)` or use `devproxy init --port 8443` |
| DNS not resolving `*.mysite.dev` | Add `127.0.0.1 slug.mysite.dev` to `/etc/hosts` or configure dnsmasq |
| `curl` fails but browser works (macOS) | `curl` doesn't use `/etc/resolver/`. Use `curl --resolve <slug>.<domain>:443:127.0.0.1 https://<slug>.<domain>` |
| `.devproxy-override.yml` in git | Add it to `.gitignore` |
| Slug changed after restart | Slugs are random per `devproxy up`. Pin not yet supported |
| Binary "killed" (exit code 137) on macOS | Gatekeeper quarantine. Re-run the install script or run: `xattr -cr $(which devproxy) && codesign --force --sign - $(which devproxy)` |
