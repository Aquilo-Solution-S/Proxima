#!/bin/bash
# Fast cargo build wrapper for Proxima workspace
# Usage: source scripts/cargo-fast.sh && cargo build
# Or: ./scripts/cargo-fast.sh build

set -euo pipefail

# Check if ccache is installed
if command -v ccache &>/dev/null; then
    export CC="ccache"
    export CXX="ccache++"
    export CCACHE_SLOPPINESS="pch_defines,time_macros"
    export CCACHE_MAXSIZE="5G"
    echo "Using ccache for C/C++ compilation"
else
    echo "ccache not installed. Install with: brew install ccache"
    echo "Falling back to default compilation"
fi

# Enable parallel compilation
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"

# Enable pipelining (Rust 1.70+)
export CARGO_TERM_VERBOSE="true"

# Run cargo with the original arguments
exec cargo "$@"
