#!/usr/bin/env bash
set -euo pipefail

# A separate workspace prevents product defaults from unifying into the
# library under test. Reuse the documented examples as downstream binaries.
#
# The base profile's normal and build dependencies must match the committed
# snapshot in engine-base-dependencies.txt. After a reviewed dependency
# change, rewrite it with:
#   bash .github/scripts/check-engine.sh --update-base-dependencies
repo=$(cd "$(dirname "$0")/../.." && pwd)
allowlist="$repo/.github/scripts/engine-base-dependencies.txt"
update=false
case "${1:-}" in
    "") ;;
    --update-base-dependencies) update=true ;;
    *)
        echo "usage: $0 [--update-base-dependencies]" >&2
        exit 2
        ;;
esac
cd "$repo"
mkdir -p target
consumer=$(mktemp -d "$repo/target/engine-consumer.XXXXXX")
trap 'rm -rf "$consumer"' EXIT
mkdir -p "$consumer/src/bin"
# Start from the reviewed dependency versions; the consumer has its own graph.
cp Cargo.lock "$consumer/Cargo.lock"
cp crates/engine/examples/native_pattern.rs "$consumer/src/bin/native_pattern.rs"
cp crates/engine/examples/session_callback.rs "$consumer/src/bin/session_callback.rs"
cat > "$consumer/Cargo.toml" <<'TOML'
[workspace]

[package]
name = "rustel-engine-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[features]
default = []
javascript = ["rustel-engine/javascript"]
session = ["rustel-engine/session"]

[dependencies]
rustel-engine = { path = "../../crates/engine", default-features = false }

[[bin]]
name = "session_callback"
required-features = ["session"]
TOML

cargo_ci() {
    bash "$repo/.github/scripts/cargo-ci.sh" "$@" --manifest-path "$consumer/Cargo.toml"
}

# Package names in a profile's graph on every target, build scripts included.
dependencies() {
    cargo_ci tree "$@" --target all --edges normal,build --prefix none --format '{p}' \
        | cut -d ' ' -f 1 | grep -vx 'rustel-engine-consumer' | LC_ALL=C sort -u
}

if [[ "$update" == true ]]; then
    dependencies --no-default-features > "$allowlist"
    echo "Wrote $(wc -l < "$allowlist") base profile packages to $allowlist"
    exit 0
fi

dependencies --no-default-features > "$consumer/base-dependencies"
if ! diff -u "$allowlist" "$consumer/base-dependencies"; then
    echo "The base engine profile's dependencies differ from $allowlist." >&2
    echo "Review the change, then run: bash .github/scripts/check-engine.sh --update-base-dependencies" >&2
    exit 1
fi
cargo_ci run --no-default-features --bin native_pattern
cargo_ci run --no-default-features --features javascript --bin native_pattern
cargo_ci run --no-default-features --features session --bin session_callback
