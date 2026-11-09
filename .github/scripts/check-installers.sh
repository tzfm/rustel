#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for name in install install.ps1; do
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL \
    --connect-timeout 15 --max-time 60 --retry 2 \
    "https://rustel.cc/$name" -o "$work/$name" || {
    echo "The download of rustel.cc/$name failed" >&2
    exit 1
  }
  cmp "$root/rustelup/$name" "$work/$name" || {
    echo "rustel.cc/$name differs from the reviewed repository file" >&2
    exit 1
  }
done
