#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/changelog.md" <<'CHANGELOG'
# Changelog

## Unreleased

- An unpublished change.

## v1.2.30

- Another version with the same prefix.

## v1.2.3 - 2026-10-01

- A reviewed change.

### Sound changes

- A reviewed sound change.

## v1.2.2

- An older change.
CHANGELOG

cat > "$WORK/expected.md" <<'NOTES'
## v1.2.3 - 2026-10-01

- A reviewed change.

### Sound changes

- A reviewed sound change.

NOTES
bash "$ROOT/.github/scripts/release-notes.sh" v1.2.3 "$WORK/changelog.md" > "$WORK/actual.md"
cmp "$WORK/expected.md" "$WORK/actual.md"

for tag in v9.0.0 v1x2x3; do
  bash "$ROOT/.github/scripts/release-notes.sh" "$tag" "$WORK/changelog.md" > "$WORK/actual.md"
  [ ! -s "$WORK/actual.md" ]
done

printf '## v1.2.2\n\n- An older change.\n' > "$WORK/expected.md"
bash "$ROOT/.github/scripts/release-notes.sh" v1.2.2 "$WORK/changelog.md" > "$WORK/actual.md"
cmp "$WORK/expected.md" "$WORK/actual.md"

echo "release notes tests passed"
