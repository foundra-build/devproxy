# Custom Slugs & Docker Compose Command Parity

## Summary

Add `--slug` flag to `devproxy up` for predictable URLs, and introduce `stop`, `start`, `daemon restart` commands to mirror docker compose's lifecycle API. This enables workflows where apps need a known `BASE_URL` before startup.

**Breaking change:** `devproxy restart` changes from daemon restart to app stack restart. Daemon restart moves to `devproxy daemon restart`. Bump minor version.

## Motivation

When apps require environment variables like `BASE_URL`, the current random slug generation means the URL isn't known until after `devproxy up` runs. A `--slug` flag lets users lock in a predictable URL. Additionally, the current command set lacks `stop`/`start` (non-destructive pause/resume), and `restart` incorrectly targets the daemon instead of the app stack.

This supersedes the "pinned slugs" open question in `docs/spec.md` (line ~199).

## Command Structure

| Command | Behavior | Override/Slug files |
|---------|----------|---------------------|
| `devproxy up [--slug NAME]` | Create override + slug if none exist, reuse if they do. Start stack with `docker compose up -d`. | Creates if missing, preserves if present |
| `devproxy down` | Stop stack with `docker compose down`. Remove override + slug files. | Removes both |
| `devproxy stop` | Stop stack with `docker compose stop`. Leave files intact. | Preserves both |
| `devproxy start` | Start stopped stack with `docker compose start`. | Requires both to exist |
| `devproxy restart` | Restart stack with `docker compose restart`. | Requires both to exist |
| `devproxy daemon restart` | Restart the background daemon process (launchd/systemd). | No effect |

## `--slug` Flag

### Usage

```bash
devproxy up --slug dirty-panda
# URL: https://dirty-panda-myrepo.{configured_domain}
```

The custom slug replaces the random `{adjective}-{animal}` prefix. The app name is still appended via `compose_slug()`, producing `{custom_slug}-{app_name}`.

### Validation

Custom slugs are **validated and rejected** if invalid (not sanitized/transformed like app names). Applied before any Docker or file operations:

- Lowercase alphanumeric and hyphens only
- No leading or trailing hyphens
- Non-empty
- Combined result (with app name) must be <= 63 characters (DNS label limit)

Validation lives in `src/config.rs` alongside the existing `compose_slug()` and `sanitize_subdomain()`.

### Slug Collisions

Custom slugs are not checked for uniqueness against running routes. If two projects use the same `--slug` and have the same app name, Docker Compose will treat them as the same project. This is the user's responsibility — the same way `docker compose -p` works.

### Reuse Behavior

**This is a behavioral change to `up`.** Currently `up` always generates a new slug and overwrites files. After this change, `up` checks for existing state first.

When `.devproxy-project` and `.devproxy-override.yml` already exist:

- `--slug` is ignored with a warning: "Ignoring --slug, reusing existing slug. Run `devproxy down` first to change slug."
- Existing slug and port binding are reused
- `docker compose up -d` is run with existing configuration

When files don't exist (fresh start or after `down`):

- `--slug` value is used as prefix (or random if not provided)
- New port is allocated, override and project files are written

## New Commands

### `devproxy stop`

1. Read slug from `.devproxy-project` (error if missing)
2. Find compose file
3. Run `docker compose -f <compose> -f <override> -p <slug> stop`
4. Print confirmation
5. Leave `.devproxy-project` and `.devproxy-override.yml` in place

Idempotent — running on already-stopped containers is a no-op.

### `devproxy start`

1. Read slug from `.devproxy-project` (error if missing)
2. Verify `.devproxy-override.yml` exists (error if missing)
3. Verify daemon is running via IPC ping
4. Run `docker compose -f <compose> -f <override> -p <slug> start`
5. Print URL

The daemon's Docker event watcher already handles container `start` events and inserts routes automatically. No additional IPC needed.

### `devproxy restart`

1. Same precondition checks as `start`
2. Run `docker compose -f <compose> -f <override> -p <slug> restart`
3. Print URL

### `devproxy daemon restart`

Existing daemon restart logic (`platform::restart_daemon`) moved to this subcommand.

## `daemon` Subcommand Structure

The existing hidden `devproxy daemon --port <PORT>` command (which runs the daemon process) becomes a subcommand group:

```
devproxy daemon run [--port PORT]   # hidden, called by launchd/systemd
devproxy daemon restart             # visible, restarts the daemon
```

Clap structure: `Daemon` variant contains a `DaemonCommand` enum with `Run { port }` (hidden) and `Restart` variants. The `Run` subcommand replaces the current top-level `Daemon` variant. Launchd plists and systemd unit files must be updated to use `devproxy daemon run --port <PORT>`.

Since `devproxy init` writes fresh plist/unit files, existing installations will get the updated invocation on next `devproxy init`. No migration needed — the daemon is always launched by the platform service manager using the plist/unit file that `init` writes.

## Docker Event Watcher Compatibility

The daemon's Docker event watcher (`proxy/docker.rs`) listens for `start`, `die`, `stop`, `kill` events. This already handles:

- **`devproxy stop`**: Container `stop` events trigger route removal
- **`devproxy start`**: Container `start` events trigger route insertion
- **`devproxy restart`**: Container `stop` + `start` events handled in sequence

No changes needed to the event watcher.

## Files Changed

| File | Change |
|------|--------|
| `src/cli.rs` | Add `Stop`, `Start` variants; restructure `Daemon` as subcommand group with `Run` (hidden) and `Restart`; add `--slug` arg on `Up` |
| `src/commands/up.rs` | Restructure: check for existing override/project files before generating new slug/port |
| `src/commands/stop.rs` | New file |
| `src/commands/start.rs` | New file |
| `src/commands/restart.rs` | Rewritten: replaces daemon restart logic with app stack restart logic |
| `src/commands/daemon.rs` | Existing file updated: add `restart` handling alongside existing `run` logic |
| `src/commands/mod.rs` | Register new modules |
| `src/main.rs` | Dispatch new commands, update `Daemon` dispatch for subcommands |
| `src/config.rs` | Add `validate_custom_slug()` alongside existing `compose_slug()` |
| `docs/spec.md` | Mark "pinned slugs" open question as resolved |
| `README.md` | Document new commands and `--slug` flag |
| `skills/devproxy/SKILL.md` | Update command table and triggers |
| `skills/setup/SKILL.md` | Update if relevant |
| `.claude-plugin/plugin.json` | Bump version |

## Edge Cases

- **`--slug` on existing project**: Warn and ignore, user must `down` first to change slug
- **`start`/`restart` with no project file**: Error with guidance to run `up` first
- **`start` with missing override but existing project file**: Error with guidance to run `up` to reconfigure
- **Custom slug fails DNS validation**: Error before any side effects
- **Custom slug + app name exceeds 63 chars**: Error with the computed length shown
- **`stop` on already-stopped stack**: No-op (idempotent)
- **Slug collision across projects**: User's responsibility, same as `docker compose -p`
