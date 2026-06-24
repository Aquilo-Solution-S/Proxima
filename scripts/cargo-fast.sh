#!/bin/bash
# Fast cargo build wrapper for Proxima workspace
# Usage:
#   ./scripts/cargo-fast.sh [cargo args]  # Run directly
#   source scripts/cargo-fast.sh          # Source to set env vars, then run cargo manually

# If sourced, just set environment variables and return
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    # We are being sourced, not executed
    CCACHE_DIR="/opt/homebrew/opt/ccache"
    if [ -d "$CCACHE_DIR/libexec" ]; then
        export PATH="$CCACHE_DIR/libexec:$PATH"
        export CCACHE_SLOPPINESS="pch_defines,time_macros"
        export CCACHE_MAXSIZE="5G"
        echo "Using ccache for C/C++ compilation (via PATH)"
        echo "Run: cargo build"
        return 0
    elif command -v ccache &>/dev/null; then
        export CC="ccache"
        export CXX="ccache++"
        export CCACHE_SLOPPINESS="pch_defines,time_macros"
        export CCACHE_MAXSIZE="5G"
        echo "Using ccache for C/C++ compilation (via CC env var)"
        echo "Run: cargo build"
        return 0
    else
        echo "ccache not installed. Install with: brew install ccache"
        return 1
    fi
fi

# If executed directly, run cargo
set -euo pipefail

CCACHE_DIR="/opt/homebrew/opt/ccache"
if [ -d "$CCACHE_DIR/libexec" ]; then
    export PATH="$CCACHE_DIR/libexec:$PATH"
    export CCACHE_SLOPPINESS="pch_defines,time_macros"
    export CCACHE_MAXSIZE="5G"
    echo "Using ccache for C/C++ compilation (via PATH)"
elif command -v ccache &>/dev/null; then
    export CC="ccache"
    export CXX="ccache++"
    export CCACHE_SLOPPINESS="pch_defines,time_macros"
    export CCACHE_MAXSIZE="5G"
    echo "Using ccache for C/C++ compilation (via CC env var)"
else
    echo "ccache not installed. Install with: brew install ccache"
    echo "Falling back to default compilation"
fi

# Enable parallel compilation
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"

# Enable pipelining (Rust 1.70+)
export CARGO_TERM_VERBOSE="true"

# Run cargo with the original arguments
exec cargo "${@:-build}"
