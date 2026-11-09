#!/usr/bin/env bash
set -euo pipefail

tag=${1:?release tag required}
sha=${2:?release commit required}
[[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
  echo 'Release tags must use vX.Y.Z' >&2
  exit 1
}
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || exit 1
[ "${GITHUB_REPOSITORY:-tzfm/rustel}" = tzfm/rustel ] || exit 1

# Fetch again after environment approval to catch moved or deleted tags.
git fetch --no-tags origin '+refs/heads/main:refs/remotes/origin/main'
remote_tag=$(git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}")
[ -n "$remote_tag" ] || { echo 'Release tag is missing on origin' >&2; exit 1; }
git fetch --no-tags origin "refs/tags/$tag"
[ "$(git rev-parse 'FETCH_HEAD^{commit}')" = "$sha" ] || {
  echo 'Release tag no longer points to the workflow commit' >&2
  exit 1
}
[ "$(git rev-parse HEAD)" = "$sha" ] || exit 1
git merge-base --is-ancestor "$sha" refs/remotes/origin/main || {
  echo 'Release commit must be on main' >&2
  exit 1
}
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
[ "$tag" = "v$version" ] || { echo 'Tag and workspace version differ' >&2; exit 1; }
count=$(gh api "repos/tzfm/rustel/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=$sha&status=success&per_page=1" \
  --jq '.workflow_runs | length')
[ "$count" = 1 ] || { echo 'The release commit needs a successful CI push run on main' >&2; exit 1; }
