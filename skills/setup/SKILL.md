---
name: setup
description: This skill should be used when the user asks to "set up devproxy", "install devproxy", "configure HTTPS for Docker", "set up local HTTPS subdomains", "get started with devproxy", "configure DNS for devproxy", or needs a guided walkthrough for first-time devproxy installation and configuration.
---

# devproxy Setup

Guided walkthrough for installing and configuring devproxy from scratch. Walk through each step interactively, verifying success before proceeding to the next.

## Step 1: Check Prerequisites

Verify Docker and Docker Compose are installed:

```bash
docker --version
docker compose version
```

If either is missing, guide the user to install Docker Desktop (macOS/Windows) or Docker Engine (Linux) before continuing.

## Step 2: Install devproxy

Check if devproxy is already installed:

```bash
devproxy --version
```

If not installed, install via the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh
```

If already installed, check for updates:

```bash
devproxy update
```

Verify the binary works after installation:

```bash
devproxy --version
```

If the binary is killed on macOS (exit code 137), Gatekeeper is blocking it. Fix with:

```bash
xattr -cr $(which devproxy)
codesign --force --sign - $(which devproxy)
```

## Step 3: Choose a Domain

Ask the user what domain they want to use. Common choices:
- `mysite.dev` — generic
- `<company>.dev` — company-specific
- `local.dev` — simple

The domain is used as the suffix for all project subdomains: `https://<slug>.<domain>`

## Step 4: Initialize devproxy

Run init with the chosen domain:

```bash
devproxy init --domain <chosen-domain>
```

This will:
- Generate a local CA and wildcard TLS certificate
- Trust the CA in the login keychain on macOS (prompts for keychain password once)
- Install a LaunchAgent (macOS) or systemd unit (Linux) to run the daemon
- Start the daemon via socket activation (no sudo needed for the daemon itself)

Verify the daemon started:

```bash
devproxy status
```

## Step 5: Configure Wildcard DNS

This is the most involved step. Wildcard DNS (`*.<domain>`) must resolve to `127.0.0.1`. Standard `/etc/hosts` does NOT support wildcards.

### macOS (dnsmasq via Homebrew)

```bash
brew install dnsmasq
echo "address=/.<chosen-domain>/127.0.0.1" >> $(brew --prefix)/etc/dnsmasq.conf
sudo brew services restart dnsmasq
sudo mkdir -p /etc/resolver
echo "nameserver 127.0.0.1" | sudo tee /etc/resolver/<chosen-domain>
```

Verify DNS resolves:

```bash
dig test.<chosen-domain> @127.0.0.1
```

Expect `127.0.0.1` in the response. Note: `dig @127.0.0.1` queries dnsmasq directly and confirms it's working. Browsers will resolve correctly via `/etc/resolver/`, but `curl` on macOS does **not** use `/etc/resolver/` — see the curl note in Step 6.

### Linux (dnsmasq via systemd-resolved or standalone)

```bash
sudo apt install dnsmasq   # or equivalent for the distro
echo "address=/.<chosen-domain>/127.0.0.1" | sudo tee -a /etc/dnsmasq.conf
sudo systemctl restart dnsmasq
```

Configure the system to use dnsmasq for the chosen domain. The approach varies by distro (systemd-resolved, NetworkManager, or direct `/etc/resolv.conf`).

### Single-project shortcut

For testing a single project without dnsmasq, a manual `/etc/hosts` entry works:

```
127.0.0.1 swift-penguin.<chosen-domain>
```

But this must be updated for each new project slug.

## Step 6: Test with a Project

If the user has a Docker Compose project ready, add the devproxy label:

```yaml
services:
  web:
    build: .
    labels:
      - devproxy.port=3000   # container-side port
```

Start the project:

```bash
devproxy up
```

Open the URL shown in the output. Verify HTTPS works with no certificate warnings.

If the user doesn't have a project ready, create a minimal test:

```yaml
# docker-compose.yml
services:
  web:
    image: nginx:alpine
    labels:
      - devproxy.port=80
```

```bash
devproxy up
# => https://<slug>.<domain>
devproxy open  # opens in browser — best way to verify on macOS
devproxy down  # clean up test
```

**Note on curl (macOS):** `curl` does not use `/etc/resolver/` for DNS, so it will fail with exit code 6 even when browsers resolve fine. To test with curl, use `--resolve`:

```bash
curl --resolve <slug>.<domain>:443:127.0.0.1 https://<slug>.<domain>
```

## Step 7: Verify Everything Works

Run through the verification checklist:

```bash
devproxy status      # daemon running
devproxy ls          # routes listed
devproxy --version   # version shown
```

If everything passes, the setup is complete. Remind the user:
- Add `.devproxy-override.yml` to `.gitignore` in each project
- Use `devproxy up` / `devproxy down` to manage projects
- Use `devproxy daemon restart` to restart the daemon if needed
- Use `devproxy update` to stay current
