# Fix CA Trust: Use Login Keychain on macOS

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Make devproxy's CA certificate trusted by all TLS clients (curl, reqwest/native-tls, browsers) on macOS without requiring sudo, by adding it to the login keychain instead of the system keychain.

**Architecture:** The fix is entirely within `src/proxy/cert.rs` (the `trust_ca_in_system` function) and `src/commands/init.rs` (user-facing messages). No new commands, no new dependencies. The `dirs` crate is already in `Cargo.toml`.

**Tech Stack:** Rust, macOS `security` CLI tool

## Root Cause

`trust_ca_in_system()` in `src/proxy/cert.rs` uses:
```
security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain <cert>
```

This targets the **system keychain** (`/Library/Keychains/System.keychain`), which requires `sudo`. Since devproxy runs as the current user (socket activation, no sudo), this command always fails unless the user manually runs it with sudo. The error is caught and degraded to a warning, leaving the CA untrusted.

## Fix

Change the macOS trust command to target the **login keychain** instead:
```
security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db <cert>
```

Key differences from the current code:
1. **`-k login.keychain-db`** instead of `-k /Library/Keychains/System.keychain` — the login keychain is writable by the current user without sudo.
2. **Drop the `-d` flag** — the `-d` flag means "add to the admin trust settings domain" which requires admin privileges. Without `-d`, the trust setting is stored in the user's trust settings, which is sufficient for all TLS clients running as the current user.
3. **Resolve the path dynamically** using `dirs::home_dir()` to get `~/Library/Keychains/login.keychain-db`, since the `~` tilde is not expanded by `Command::new`.

**Why login keychain and not system keychain:** The system keychain requires root. The login keychain is the user's default keychain, is unlocked when the user logs in, and is trusted by all user-space TLS clients including curl, Safari, Chrome, and native-tls/Security.framework. This matches how tools like mkcert work.

**Why drop `-d`:** The `-d` flag writes to the admin cert store (`/Library/Preferences/com.apple.security.admin.plist`), which requires an admin authentication prompt or sudo. Without `-d`, the trust policy is written to `~/Library/Preferences/com.apple.security.trust-settings.<hash>.plist` — the per-user trust store. All TLS clients on macOS check per-user trust settings.

**Why `login.keychain-db`:** Modern macOS (10.12+) uses the `-db` suffix. The `security` command accepts both `login.keychain` and `login.keychain-db`, but using the actual filename is more robust. We resolve the full path via `$HOME/Library/Keychains/login.keychain-db`.

**Why keep the function name `trust_ca_in_system`:** On Linux, this function still targets system-level CA trust (`/usr/local/share/ca-certificates`). Renaming to something like `trust_ca` is tempting but would touch more code for no functional benefit. The doc comment already describes per-platform behavior. Keep the name as-is.

## Scope of User-Facing Message Changes in init.rs

There are **five** places in `src/commands/init.rs` that reference the system keychain or sudo for macOS trust:
1. Line 363: `"trusting CA in system keychain (requires sudo)..."` — change to `"trusting CA in login keychain..."`
2. Line 365: `"CA trusted in system keychain"` — change to `"CA trusted in login keychain"`
3. Line 369: `"run manually with sudo:"` — change to platform-conditional message (macOS no longer needs sudo; Linux still does)
4. Line 372: fallback manual command with `sudo security add-trusted-cert ... /Library/Keychains/System.keychain` — update to new command (no sudo)
5. Line 548-549: "Next steps" fallback with same manual command — update to new command (no sudo)

All five must be updated consistently.

---

### Task 1: Add `login_keychain_path` helper and test in cert.rs

**Files:**
- Modify: `src/proxy/cert.rs`

**Step 1: Add the helper function**

Add just above the existing `trust_ca_in_system` function (before line 135):

```rust
/// Return the path to the current user's login keychain on macOS.
#[cfg(target_os = "macos")]
fn login_keychain_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("Library/Keychains/login.keychain-db"))
}
```

**Step 2: Add a unit test**

Add to the existing `#[cfg(test)] mod tests` block at the end of the file:

```rust
#[cfg(target_os = "macos")]
#[test]
fn login_keychain_path_points_to_login_keychain() {
    let path = super::login_keychain_path().unwrap();
    assert!(path.to_string_lossy().ends_with("Library/Keychains/login.keychain-db"));
    assert!(path.to_string_lossy().starts_with("/Users/"));
}
```

**Step 3: Run the test**

```bash
cargo test --lib -- cert::tests::login_keychain_path
```

Expected: PASS (on macOS).

**Step 4: Commit**

```bash
git add src/proxy/cert.rs
git commit -m "feat: add login_keychain_path helper for macOS CA trust"
```

---

### Task 2: Update `trust_ca_in_system` to use login keychain

**Files:**
- Modify: `src/proxy/cert.rs`

**Step 1: Replace the macOS block in trust_ca_in_system**

Replace lines 140-160 (the `#[cfg(target_os = "macos")]` block inside `trust_ca_in_system`) with:

```rust
#[cfg(target_os = "macos")]
{
    let keychain = login_keychain_path()?;
    let status = std::process::Command::new("security")
        .args([
            "add-trusted-cert",
            "-r",
            "trustRoot",
            "-k",
        ])
        .arg(&keychain)
        .arg(ca_cert_path)
        .status()
        .context("failed to run security command")?;

    if !status.success() {
        anyhow::bail!(
            "failed to trust CA cert in login keychain ({})",
            keychain.display()
        );
    }

    return Ok(());
}
```

Changes from current code:
- Removed `-d` flag (no admin trust domain)
- Changed keychain from `/Library/Keychains/System.keychain` to dynamic login keychain path
- Updated error message (no more "may need sudo")

**Step 2: Verify compilation and existing tests still pass**

```bash
cargo test --lib -- cert::tests
```

Expected: all cert tests pass.

**Step 3: Commit**

```bash
git add src/proxy/cert.rs
git commit -m "fix: use login keychain for macOS CA trust (no sudo required)"
```

---

### Task 3: Update user-facing messages in init.rs

**Files:**
- Modify: `src/commands/init.rs`

**Step 1: Update the trust attempt message (line 363)**

Change:
```rust
eprintln!("trusting CA in system keychain (requires sudo)...");
```
To:
```rust
eprintln!("trusting CA in login keychain...");
```

**Step 2: Update the success message (line 365)**

Change:
```rust
Ok(()) => eprintln!("{} CA trusted in system keychain", "ok:".green()),
```
To:
```rust
Ok(()) => eprintln!("{} CA trusted in login keychain", "ok:".green()),
```

**Step 3: Update the fallback "run manually" text (line 369)**

Change:
```rust
eprintln!("  run manually with sudo:");
```
To:
```rust
eprintln!("  run manually:");
```

Note: On Linux the fallback command still uses `sudo`, but the "run manually:" text itself doesn't need to say "with sudo" since the individual commands below show `sudo` where needed.

**Step 4: Update the fallback manual command (lines 370-374)**

Change:
```rust
#[cfg(target_os = "macos")]
eprintln!(
    "    sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
    ca_cert_path.display()
);
```
To:
```rust
#[cfg(target_os = "macos")]
eprintln!(
    "    security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db {}",
    ca_cert_path.display()
);
```

**Step 5: Update the "Next steps" fallback (lines 547-550)**

Change:
```rust
#[cfg(target_os = "macos")]
eprintln!(
    "     sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
    ca_cert_path.display()
);
```
To:
```rust
#[cfg(target_os = "macos")]
eprintln!(
    "     security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db {}",
    ca_cert_path.display()
);
```

**Step 6: Verify compilation**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: clean build, no warnings.

**Step 7: Commit**

```bash
git add src/commands/init.rs
git commit -m "fix: update init messages to reflect login keychain trust (no sudo)"
```

---

### Task 4: Run full test suite and verify

**Files:** (no modifications)

**Step 1: Run cargo fmt check**

```bash
cargo fmt -- --check
```

Expected: no formatting violations.

**Step 2: Run full check**

```bash
cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: all tests pass, no warnings.
