#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"

# Ask Cargo for the effective release panic strategy, including config,
# environment, and rustflags overrides. A small workspace library avoids a
# product build.
cfg=$(cargo rustc --locked --release -p rustel-fraction --lib "$@" -- --print cfg)
if ! grep -Fxq 'panic="unwind"' <<< "$cfg"; then
    echo 'Release builds require panic="unwind" for score panic recovery.' >&2
    echo 'Remove any panic="abort" profile, Cargo config, or rustflags override.' >&2
    exit 1
fi
echo 'Release panic strategy is unwind.'
