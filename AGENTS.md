# devproxy — Agent Guidelines

## Project Overview

devproxy is a single Rust binary that provides local HTTPS dev subdomains for Docker Compose
projects. No external proxy, no mkcert. See `docs/spec.md` for the full specification.

## Build & Test

```bash
just setup    # install dependencies, cargo build
just dev      # start dev workflow (cargo watch)
just check    # cargo clippy + cargo test
just build    # cargo build --release
```

## Architecture

- **Single binary** with a background daemon mode (`devproxy daemon` — hidden subcommand)
- **CLI → daemon IPC** over Unix domain socket (`~/.config/devproxy/devproxy.sock`)
- **Docker is source of truth** — no persistent state files
- **Async runtime**: tokio with two main tasks joined via `tokio::join!`
  - HTTPS reverse proxy (tokio-rustls + hyper)
  - Docker event watcher (streams `docker events`)

## Key Conventions

- Use `anyhow::Result` for error handling throughout
- Use `clap` derive macros for CLI argument parsing
- Keep modules focused — see `docs/spec.md` Module Layout section
- No `.unwrap()` in library code; use `?` or explicit error handling
- Prefer `eprintln!` with `colored` for user-facing CLI output

## File Structure

```
src/
├── main.rs          — entry point, clap dispatch
├── cli.rs           — CLI definitions
├── config.rs        — configuration and compose file parsing
├── slugs.rs         — random slug generator
├── ipc.rs           — Unix socket IPC client
├── proxy/           — daemon internals (cert, router, docker watcher)
└── commands/        — CLI command implementations
docs/
└── spec.md          — full project specification
```

## Dependencies

See `Cargo.toml`. Key crates: clap, tokio, hyper, tokio-rustls, rcgen, serde, anyhow.
