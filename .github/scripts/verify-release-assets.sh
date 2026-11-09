#!/usr/bin/env bash
set -euo pipefail

dist=${1:?asset directory required}
tag=${2:?release tag required}
sha=${3:?release commit required}
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 1
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || exit 1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$dist"

installers=(install install.ps1 rustelup rustelup.ps1)
assets=("${installers[@]}")
for platform in linux_amd64 linux_arm64 darwin_amd64 darwin_arm64 win32_amd64; do
  assets+=("rustel_${tag}_${platform}.tar.gz")
done
expected=(installers.sigstore.json)
for asset in "${assets[@]}"; do
  expected+=("$asset" "$asset.sha256")
  if [[ "$asset" = *.tar.gz ]]; then expected+=("$asset.sigstore.json"); fi
done
shopt -s nullglob dotglob
files=(*)
[ "${#files[@]}" -eq "${#expected[@]}" ] || { echo 'Unexpected release asset count' >&2; exit 1; }
for file in "${expected[@]}"; do
  [ -f "$file" ] && [ ! -L "$file" ] || { echo "Missing or unsafe asset: $file" >&2; exit 1; }
done

# The installers must be the reviewed files of the release commit. A build
# job can sign files too, so a valid signature alone does not prove this.
for installer in "${installers[@]}"; do
  cmp -s "$root/rustelup/$installer" "$installer" || {
    echo "Installer differs from rustelup/$installer" >&2
    exit 1
  }
done

for asset in "${assets[@]}"; do
  digest=$(cat "$asset.sha256")
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo "Invalid checksum: $asset" >&2; exit 1; }
  # BSD sha256sum reads a checklist only from a named operand, so pass
  # stdin explicitly. GNU reads stdin the same way.
  printf '%s  %s\n' "$digest" "$asset" | sha256sum --check --status - || {
    echo "Checksum mismatch: $asset" >&2
    exit 1
  }
  bundle=installers.sigstore.json
  if [[ "$asset" = *.tar.gz ]]; then bundle="$asset.sigstore.json"; fi
  gh attestation verify "$asset" --bundle "$bundle" \
    --repo tzfm/rustel \
    --signer-workflow tzfm/rustel/.github/workflows/release.yml \
    --source-ref "refs/tags/$tag" --source-digest "$sha" --signer-digest "$sha" \
    --deny-self-hosted-runners || {
    echo "Provenance check failed: $asset" >&2
    exit 1
  }
done
