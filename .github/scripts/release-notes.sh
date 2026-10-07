#!/usr/bin/env bash
set -euo pipefail

# Print the tagged changelog section, or nothing when no section matches.
awk -v tag="$1" '
  /^## / {
    if (matched) exit
    matched = ($2 == tag)
  }
  matched { print }
' "${2:-CHANGELOG.md}"
