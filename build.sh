#!/usr/bin/env bash
# Cross-compile Switch.exe from Linux/macOS (needs: rustup target add x86_64-pc-windows-gnu, mingw-w64).
# On Windows, just run:  cargo build --release
set -euo pipefail
cargo build --release --target x86_64-pc-windows-gnu "$@"
echo "Built target/x86_64-pc-windows-gnu/release/Switch.exe"
