# Build Optimization Guide

This document describes how to optimize compile times for the Proxima workspace.

## Quick Start

For the best compile-time experience:

```bash
# Install ccache (recommended)
brew install ccache

# Or on Linux (Debian/Ubuntu):
sudo apt-get install ccache

# Then run cargo with ccache:
CC=ccache cargo build
```

## Workspace Configuration

The workspace includes `.cargo/config.toml` with the following optimizations:

- **Incremental compilation**: Caches compilation artifacts between builds
- **Parallel compilation**: Uses 8 parallel jobs
- **Pipelining**: Enables pipelined compilation (Rust 1.70+)
- **Optimized debug builds**: `opt-level=1` for faster compiles
- **Reduced debug info**: `split-debuginfo="unpacked"` for dependencies

## Tree-sitter Optimization

The `flavors/code` crate depends on `tree-sitter`, `tree-sitter-rust`, and `tree-sitter-typescript`, which compile C code at build time using the `cc` crate.

### Using ccache (Recommended)

Install ccache and set the `CC` environment variable:

```bash
# macOS
brew install ccache

# Linux (Debian/Ubuntu)
sudo apt-get install ccache

# Then build with ccache
CC=ccache cargo build
```

This can reduce tree-sitter compilation time by **50-80%** on subsequent builds.

### Using sccache (Alternative)

`sccache` is a distributed cache that works well with Rust:

```bash
# Install sccache
cargo install sccache

# Build with sccache
RUSTC_WRAPPER=sccache CC=sccache cargo build
```

### Manual Cache Directory

The `cc` crate caches compiled objects in `target/<triple>/build/<package>/out/` by default. This cache persists across incremental builds.

## Fast Build Script

A convenience script is provided at `scripts/cargo-fast.sh`:

```bash
# Make it executable
chmod +x scripts/cargo-fast.sh

# Use it directly
./scripts/cargo-fast.sh build

# Or source it to set environment variables
source scripts/cargo-fast.sh
cargo build
```

This script automatically:
- Detects if ccache is installed and uses it
- Sets parallel compilation jobs
- Enables verbose output

## Additional Optimizations

### Reduce Debug Info

For even faster debug builds, you can disable debug info entirely:

```bash
# In .cargo/config.toml
[profile.dev]
debug-info = 0
```

Note: This makes debugging harder, so use it only when you don't need a debugger.

### Use Release Mode for Development

If you don't need debug assertions, you can use release mode with debug symbols:

```bash
cargo build --release
```

Or create a custom profile:

```toml
# In .cargo/config.toml
[profile.dev-fast]
inherits = "release"
debug = true
debug-assertions = true
```

Then build with:

```bash
cargo build --profile dev-fast
```

### Parallel Proc-macro Expansion (Nightly)

If you're using nightly Rust, you can enable parallel proc-macro expansion:

```toml
# In .cargo/config.toml
[unstable]
build-std = ["std", "panic_abort"]
```

## Benchmarking

To measure the impact of optimizations:

```bash
# Clean build (baseline)
cargo clean
time cargo build --release

# Incremental build
time cargo build --release

# With ccache
CC=ccache cargo clean
time CC=ccache cargo build --release
```

## Common Issues

### ccache not working

Make sure:
1. ccache is installed and in your PATH
2. The `CC` environment variable is set before running cargo
3. You're not using `cargo clean` between builds (this clears the cache)

### Build still slow

The main bottlenecks are:
1. **tree-sitter C compilation** - Use ccache as described above
2. **sqlx macro expansion** - Consider reducing macro usage
3. **specta/schemars derives** - These are slow proc-macros

### Out of memory

Reduce the number of parallel jobs:

```toml
[build]
jobs = 4
```
