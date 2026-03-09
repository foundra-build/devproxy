# devproxy — development commands

default:
    @just --list

# Bootstrap the project (install tools, build)
setup:
    cargo build
    @echo "Setup complete."

# Run dev workflow with cargo-watch
dev:
    cargo watch -x 'clippy --all-targets' -x 'test' -x 'run -- --help'

# Run clippy and tests
check:
    cargo clippy --all-targets -- -D warnings
    cargo test

# Build release binary
build:
    cargo build --release

# Format code
fmt:
    cargo fmt

# Format check (CI)
fmt-check:
    cargo fmt -- --check

# Run e2e tests (requires Docker)
e2e:
    cargo test --test e2e -- --include-ignored --nocapture
