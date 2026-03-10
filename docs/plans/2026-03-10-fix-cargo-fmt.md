# Fix cargo fmt CI Failure Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Fix all `cargo fmt` formatting violations so the GitHub Actions CI pipeline passes.

**Architecture:** Run `cargo fmt` to auto-format all Rust source files, then fix any pre-existing `clippy` lint warnings that were previously masked by the format failure, and verify the full CI check sequence (`fmt --check`, `clippy`, `test`) passes.

**Tech Stack:** Rust toolchain (cargo fmt, clippy, cargo test)

**Decision: Use `cargo fmt` auto-format rather than manual edits.**
All 17 diffs across 5 files are standard rustfmt line-length and argument-wrapping reformats. Running `cargo fmt` is idiomatic, deterministic, and guaranteed to produce the exact output `cargo fmt -- --check` expects. Manual editing would be error-prone and pointless.

**Note (discovered during implementation):** After `cargo fmt` fixes the format check, `cargo clippy` reveals 4 pre-existing `clippy::collapsible_if` warnings in `tests/e2e.rs` that were previously masked because CI failed at the earlier `fmt --check` step. These require collapsing nested `if`/`if let` chains into single `if ... && let ...` expressions — a semantically equivalent but AST-level change, not just whitespace.

---

### Task 1: Run cargo fmt to auto-format all files

**Files:**
- Modify: `src/commands/init.rs` (2 formatting diffs — long method chain and long `eprintln!` call)
- Modify: `src/commands/ls.rs` (4 formatting diffs — long `println!` and multi-arg `assert!` calls)
- Modify: `src/config.rs` (5 formatting diffs — long method chains and long `.args()` arrays)
- Modify: `src/platform.rs` (1 formatting diff — `unwrap_or_else` chain wrapping)
- Modify: `tests/e2e.rs` (5 formatting diffs — `.args()` arrays, `temp_dir().join()` chains, compact `assert!`)

**Step 1: Run cargo fmt**

```bash
cargo fmt
```

Expected: exits 0, no output. All 5 files are reformatted in place.

**Step 2: Verify cargo fmt --check passes**

```bash
cargo fmt -- --check
```

Expected: exits 0 with no output (no remaining diffs).

**Step 3: Run cargo clippy**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: exits 0. If pre-existing clippy warnings surface (previously masked by the format failure), fix them before proceeding.

**Step 4: Run cargo test**

```bash
cargo test
```

Expected: exits 0. Formatting changes cannot break tests, but this mirrors the CI step and confirms the full `check` job would pass.

**Step 5: Commit**

```bash
git add src/commands/init.rs src/commands/ls.rs src/config.rs src/platform.rs tests/e2e.rs
git commit -m "style: apply cargo fmt and fix clippy collapsible-if warnings"
```
