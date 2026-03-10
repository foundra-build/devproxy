# Fix CA Trust: Use Login Keychain on macOS

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Make devproxy's CA certificate trusted by all TLS clients (curl, reqwest/native-tls, browsers) on macOS without requiring sudo, by adding it to the login keychain instead of the system keychain.

**Architecture:** The fix is entirely within `src/proxy/cert.rs` (the `trust_ca_in_system` function) and `src/commands/init.rs` (user-facing messages). No new commands, no new dependencies.

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

## Scope of User-Facing Message Changes

The `init.rs` file has three places that reference the system keychain:
1. Line 363: `"trusting CA in system keychain (requires sudo)..."` — change to `"trusting CA in login keychain..."`
2. Line 372: fallback manual command with `sudo security add-trusted-cert ... /Library/Keychains/System.keychain` — update to new command (no sudo)
3. Line 549: "Next steps" fallback with same manual command — update to new command (no sudo)

All three must be updated consistently.

---

### Task 1: Change macOS trust to use login keychain in cert.rs

**Files:**
- Modify: `src/proxy/cert.rs`

**Step 1: Write a unit test that validates the security command arguments**

The actual `security add-trusted-cert` call is a side-effecting system command and cannot be tested in CI. However, we can extract the logic that builds the keychain path and test that. Add a helper function `login_keychain_path()` and a test for it.

Add to `src/proxy/cert.rs`:

```rust
/// Return the path to the current user's login keychain on macOS.
#[cfg(target_os = "macos")]
fn login_keychain_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("Library/Keychains/login.keychain-db"))
}
```

Add test in the existing `#[cfg(test)] mod tests`:

```rust
#[cfg(target_os = "macos")]
#[test]
fn login_keychain_path_points_to_login_keychain() {
    let path = super::login_keychain_path().unwrap();
    assert!(path.to_string_lossy().ends_with("Library/Keychains/login.keychain-db"));
    assert!(path.to_string_lossy().starts_with("/Users/"));
}
```

**Step 2: Update trust_ca_in_system to use login keychain**

Replace the macOS block in `trust_ca_in_system` (lines 140-160) with:

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

Key changes from current code:
- Removed `-d` flag (no admin trust domain)
- Changed keychain from `/Library/Keychains/System.keychain` to dynamic login keychain path
- Updated error message (no more "may need sudo" since login keychain doesn't need it)

**Step 3: Verify compilation and tests**

```bash
cargo test --lib -- cert::tests
```

Expected: all cert tests pass, including the new `login_keychain_path_points_to_login_keychain` test.

---

### Task 2: Update user-facing messages in init.rs

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

**Step 2: Update the fallback manual command (line 370-374)**

Change the `#[cfg(target_os = "macos")]` fallback instructions from:
```rust
eprintln!(
    "    sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
    ca_cert_path.display()
);
```
To:
```rust
eprintln!(
    "    security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db {}",
    ca_cert_path.display()
);
```

**Step 3: Update the "Next steps" fallback (line 547-550)**

Change the `#[cfg(target_os = "macos")]` next-steps instructions from:
```rust
eprintln!(
    "     sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
    ca_cert_path.display()
);
```
To:
```rust
eprintln!(
    "     security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db {}",
    ca_cert_path.display()
);
```

**Step 4: Verify compilation**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: clean build, no warnings.

---

### Task 3: Run full test suite and verify

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
