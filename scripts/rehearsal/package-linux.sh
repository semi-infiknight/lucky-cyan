#!/bin/sh
# Build a private operator rehearsal installer from the repository root.
set -eu
cd "$(dirname "$0")/../.."
npm run build:rehearsal
cargo build --release --locked --manifest-path src-tauri/Cargo.toml
scripts/rehearsal/bundle-deb.sh
cargo build --release --locked --manifest-path tools/inkue-inspect/Cargo.toml
