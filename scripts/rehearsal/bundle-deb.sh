#!/bin/sh
# Bundle the already-built Linux desktop with its source identity and libc floor.
set -eu
cd "$(dirname "$0")/../.."
binary="${CARGO_TARGET_DIR:-src-tauri/target}/release/inkue"
glibc=$(LC_ALL=C readelf --version-info --wide "$binary" |
  grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | cut -d_ -f2 | sort -Vu | tail -1)
test -n "$glibc" || { echo "Could not determine the required glibc version in $binary" >&2; exit 1; }
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
revision=$(git rev-parse --verify HEAD 2>/dev/null || true)
timestamp=$(git log -1 --format=%ct 2>/dev/null || true)
dirty=false
# Tauri regenerates schema snapshots during compilation. Its input capability
# definitions are tracked separately under src-tauri/capabilities.
if test -n "$revision" && ! git diff --quiet HEAD -- . ':(exclude)src-tauri/gen/schemas/**'; then dirty=true; fi
config=$(node - "$glibc" "$scratch/desk-build.json" "$revision" "$timestamp" "$dirty" <<'JS'
const fs = require("node:fs");
const source = require("./src-tauri/tauri.conf.json");
const [glibc, file, revision, timestamp, dirty] = process.argv.slice(2);
const stamp = revision ? new Date(Number(timestamp) * 1000).toISOString().slice(0, 19).replace(/[-T:]/g, "") : "";
const suffix = revision ? `${stamp}.g${revision.slice(0, 12)}${dirty === "true" ? ".dirty" : ""}` : "local";
const version = `${source.version}+desk.${suffix}`;
fs.writeFileSync(file, JSON.stringify({version, source_revision: revision || null, modified_sources: dirty === "true"}, null, 2) + "\n");
const depends = [...source.bundle.linux.deb.depends, `libc6 (>= ${glibc})`];
const files = {"/usr/share/doc/inkue/desk-build.json": file};
const desktopTemplate = require("node:path").resolve("scripts/rehearsal/inkue.desktop.hbs");
process.stdout.write(JSON.stringify({version, bundle: {createUpdaterArtifacts: false, linux: {deb: {depends, files, desktopTemplate}}}}));
JS
)
node node_modules/@tauri-apps/cli/tauri.js bundle -b deb --config "$config"
