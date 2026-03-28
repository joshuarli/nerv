#!/bin/bash
# Copy the real nerv source tree into the eval workdir.
# The eval harness runs from the project root, so NERV_ROOT is passed
# via the environment, or we detect it from this script's location.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NERV_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

cp -r "$NERV_ROOT/src" .
cp -r "$NERV_ROOT/Cargo.toml" .
cp -r "$NERV_ROOT/Cargo.lock" .
cp "$NERV_ROOT/AGENTS.md" .
