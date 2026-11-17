#!/usr/bin/env bash
set -euxo pipefail

sed -i "s/^version = \".*\"/version = \"$2\"/" Cargo.toml
# Keep Cargo.lock in sync; the distroless build runs `cargo build --locked`.
cargo update -p uptime-checker
