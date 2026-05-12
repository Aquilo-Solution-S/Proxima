# Task 2.1 — Workspace member + Cargo.toml

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/harness/Cargo.toml`
- Create: `crates/harness/src/lib.rs` (skeleton)

- [ ] **Step 1: Add to workspace members**

Edit `Cargo.toml`. Change the `members = [...]` line to include `"crates/harness"`. The new line should read:

```toml
members = ["crates/core", "crates/harness", "crates/mcp-server", "apps/proxima-engine", "apps/proxima-code", "apps/proxima-mcp", "flavors/code", "flavors/mcp", "flavors/goal", "crates/llm-openai-compat", "apps/proxima-shell/src-tauri", "crates/storage-pg", "crates/wire-grpc"]
```

- [ ] **Step 2: Create `crates/harness/Cargo.toml`**

```toml
[package]
name = "proxima-harness"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
proxima-core = { path = "../core" }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["process", "fs", "time", "io-util"] }
reqwest = { workspace = true }
uuid = { workspace = true }
schemars = { workspace = true }
tracing = { workspace = true }
time = { workspace = true }
blake3 = { workspace = true }
futures = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util", "fs"] }
tempfile = "3"
```

- [ ] **Step 3: Create empty submodule stubs first (build order matters)**

`lib.rs` declares submodules; if we run `cargo build` before the submodule files exist the compile fails on missing files. Create the stubs before declaring them in `lib.rs`.

Create `crates/harness/src/conversation.rs`:
```rust
//! Provider-neutral conversation types — filled in Task 2.2.
```

Create `crates/harness/src/program.rs`:
```rust
//! HarnessProgram builder — filled in Task 4.1.
```

Create `crates/harness/src/loop_driver.rs`:
```rust
//! Loop driver — filled in Task 4.3. Stub `HarnessLoop` for now.

#[derive(Debug, Default)]
pub struct HarnessLoop;
```

Create `crates/harness/src/providers/mod.rs`:
```rust
//! ProviderClient trait — filled in Task 2.3.
```

Create `crates/harness/src/tools/mod.rs`:
```rust
//! Tool dispatch — filled in Tasks 3.1 and 4.2.
```

Create `crates/harness/src/trace/mod.rs`:
```rust
//! Trace artifacts.

pub mod jsonl;
```

Create `crates/harness/src/trace/jsonl.rs`:
```rust
//! JSONL transcript buffer — filled in Task 2.4.
```

- [ ] **Step 4: Create `crates/harness/src/lib.rs`**

```rust
//! Proxima Harness — in-process LLM loop driver.
//!
//! Implements `proxima_core::harness::HarnessAdapter` via
//! [`HarnessLoop`]. See
//! `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`.

#![forbid(unsafe_code)]

pub mod conversation;
pub mod loop_driver;
pub mod program;
pub mod providers;
pub mod tools;
pub mod trace;

pub use loop_driver::HarnessLoop;
```

- [ ] **Step 5: Verify the workspace builds**

Run: `cargo build -p proxima-harness`
Expected: builds clean — all submodule files now exist as stubs.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/harness
git commit -m "harness: crate skeleton + workspace registration"
```

