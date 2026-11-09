#!/usr/bin/env bash
set -euo pipefail

tag=${1:?release tag required}
sha=${2:?release commit required}
scripts=$(cd "$(dirname "$0")" && pwd)
bash "$scripts/validate-release.sh" "$tag" "$sha"
bash "$scripts/check-installers.sh"
bash "$scripts/verify-release-assets.sh" dist "$tag" "$sha"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bash "$scripts/release-notes.sh" "$tag" > "$work/notes.md"
notes=(--generate-notes)
if [ -s "$work/notes.md" ]; then notes=(--notes-file "$work/notes.md"); fi

# Keep failed drafts for inspection. Never overwrite an existing release, and
# never add a second draft next to a failed one: later steps find the draft
# by its tag.
existing=$(gh api "repos/tzfm/rustel/releases?per_page=100" \
  --jq "[.[] | select(.tag_name == \"$tag\")] | length")
[ "$existing" = 0 ] || { echo "Tag $tag already has a release or draft" >&2; exit 1; }
gh release create "$tag" dist/* --repo tzfm/rustel --draft --verify-tag "${notes[@]}"
gh release download "$tag" --repo tzfm/rustel --dir "$work/downloaded"
bash "$scripts/verify-release-assets.sh" "$work/downloaded" "$tag" "$sha"
bash "$scripts/validate-release.sh" "$tag" "$sha"
gh release edit "$tag" --repo tzfm/rustel --draft=false
