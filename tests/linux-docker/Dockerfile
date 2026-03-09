# tests/linux-docker/Dockerfile
# Builds devproxy and runs Linux-specific integration tests.
# Requires: docker build --tag devproxy-linux-test .
# Run:      docker run --rm --privileged devproxy-linux-test

FROM rust:1.84-bookworm AS builder

# Install libcap2-bin for setcap
RUN apt-get update && apt-get install -y libcap2-bin && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release
RUN cp target/release/devproxy /usr/local/bin/devproxy

FROM debian:bookworm

# Install systemd + dbus + libcap2-bin (for setcap/getcap) + python3 (for LISTEN_FDS test)
# dbus is required for systemd user manager communication (systemctl --user).
RUN apt-get update && \
    apt-get install -y systemd systemd-sysv dbus dbus-user-session \
        libcap2-bin curl procps python3 && \
    rm -rf /var/lib/apt/lists/*

# Copy the built binary and test scripts
COPY --from=builder /usr/local/bin/devproxy /usr/local/bin/devproxy
COPY tests/linux-docker/test-systemd.sh /tests/test-systemd.sh
COPY tests/linux-docker/test-listen-fds.sh /tests/test-listen-fds.sh
COPY tests/linux-docker/test-setcap.sh /tests/test-setcap.sh
RUN chmod +x /tests/*.sh

# Create a non-root user for testing user-level systemd/setcap
RUN useradd -m -s /bin/bash testuser

# Enable lingering so systemd starts a user manager for testuser at boot
# (required for `systemctl --user` to work without an interactive login).
RUN mkdir -p /var/lib/systemd/linger && touch /var/lib/systemd/linger/testuser

# Create XDG_RUNTIME_DIR for testuser (uid 1000).
# systemd user manager and dbus session bus need this directory.
RUN mkdir -p /run/user/1000 && chown testuser:testuser /run/user/1000 && chmod 700 /run/user/1000

# Use systemd as init (PID 1) so systemctl --user works
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/sbin/init"]
