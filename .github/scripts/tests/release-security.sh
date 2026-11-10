#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export FIXTURE="$work"
export GITHUB_REPOSITORY=tzfm/rustel
mkdir -p "$work/bin" "$work/repo/.github" "$work/repo/rustelup"
cp -R "$root/.github/scripts" "$work/repo/.github/"
cp "$root"/rustelup/{install,install.ps1,rustelup,rustelup.ps1} "$work/repo/rustelup/"
export PATH="$work/bin:$PATH"
if ! command -v sha256sum >/dev/null 2>&1; then
  printf '#!/usr/bin/env bash\nexec shasum -a 256 "$@"\n' > "$work/bin/sha256sum"
  chmod +x "$work/bin/sha256sum"
fi

# The CLI owns signature verification. These doubles test failure propagation
# and publication order without credentials or a release on GitHub.
cat > "$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE/calls"
case "$1 $2" in
  'api repos/tzfm/rustel/actions/'*)
    if [ -d "$FIXTURE/uploaded" ] && [ -n "${CI_AFTER_UPLOAD:-}" ]; then echo "$CI_AFTER_UPLOAD"
    else echo "${CI_COUNT:-1}"; fi ;;
  'api repos/tzfm/rustel/releases'*) echo "${EXISTING_RELEASES:-0}" ;;
  'attestation verify')
    [[ " $* " = *' --repo tzfm/rustel '* ]]
    [[ " $* " = *' --signer-workflow tzfm/rustel/.github/workflows/release.yml '* ]]
    [[ " $* " = *' --source-ref refs/tags/v1.2.3 '* ]]
    [[ " $* " = *" --source-digest $RELEASE_SHA "* ]]
    [[ " $* " = *" --signer-digest $RELEASE_SHA "* ]]
    [[ " $* " = *' --deny-self-hosted-runners '* ]]
    if [ -n "${VERIFY_FAILURE:-}" ]; then echo "$VERIFY_FAILURE" >&2; exit 1; fi
    if [ "${FAIL_DOWNLOAD:-0}" = 1 ] && [[ "$PWD" = */downloaded ]]; then exit 1; fi
    ;;
  'release create')
    [[ " $* " = *' --draft '* ]]
    [[ " $* " = *' --verify-tag '* ]]
    mkdir "$FIXTURE/uploaded"
    cp dist/* "$FIXTURE/uploaded/"
    ;;
  'release download')
    while [ "$1" != --dir ]; do shift; done
    mkdir "$2"
    cp "$FIXTURE/uploaded/"* "$2/"
    ;;
  'release edit') [[ " $* " = *' --draft=false '* ]] ;;
  *) echo "unexpected gh command: $*" >&2; exit 1 ;;
esac
MOCK
cat > "$work/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[ "${FETCH_FAILURE:-0}" = 0 ] || exit 1
for arg in "$@"; do
  case "$arg" in https://rustel.cc/*) name=${arg##*/} ;; esac
done
while [ "$1" != -o ]; do shift; done
cp "$FIXTURE/repo/rustelup/$name" "$2"
if [ "${SITE_CHANGED:-0}" = 1 ]; then echo changed >> "$2"; fi
MOCK
chmod +x "$work/bin/gh" "$work/bin/curl"

fail() { echo "$*" >&2; exit 1; }
# Runs a command that must fail, and checks that it failed for the reason given.
reject() {
  local reason=$1
  shift
  if "$@" > "$work/output" 2>&1; then fail "Accepted invalid input: $*"; fi
  grep -q -- "$reason" "$work/output" ||
    fail "Rejected for another reason than '$reason': $(cat "$work/output")"
}
not_published() {
  if grep -q '^release edit ' "$work/calls"; then fail 'Published an invalid release'; fi
}

cd "$work/repo"
git init -q -b main
git config core.autocrlf false
git config user.name 'Release Fixture'
git config user.email fixture@example.invalid
printf '[workspace.package]\nversion = "1.2.3"\n' > Cargo.toml
printf '# Changelog\n\n## v1.2.3\n\n- Release fixture.\n' > CHANGELOG.md
git add .
git -c commit.gpgsign=false commit -qm 'Release fixture'
export RELEASE_SHA
RELEASE_SHA=$(git rev-parse HEAD)
git -c tag.gpgsign=false tag -a v1.2.3 -m 'Release fixture'
git clone -q --bare . "$work/origin.git"
git remote add origin "$work/origin.git"
scripts="$PWD/.github/scripts"
bash "$scripts/validate-release.sh" v1.2.3 "$RELEASE_SHA" > "$work/output" 2>&1
reject 'must use vX.Y.Z' bash "$scripts/validate-release.sh" 'v1.2.3;bad' "$RELEASE_SHA"
reject 'full SHA' bash "$scripts/validate-release.sh" v1.2.3 HEAD
reject 'needs a successful CI push run' env CI_COUNT=0 bash "$scripts/validate-release.sh" v1.2.3 "$RELEASE_SHA"
reject 'only in tzfm/rustel' env GITHUB_REPOSITORY=other/repo bash "$scripts/validate-release.sh" v1.2.3 "$RELEASE_SHA"
reject 'no longer points' bash "$scripts/validate-release.sh" v1.2.3 0000000000000000000000000000000000000000
reject 'missing on origin' bash "$scripts/validate-release.sh" v1.2.5 "$RELEASE_SHA"
git -c tag.gpgsign=false tag v1.2.4
git push -q origin v1.2.4
reject 'version differ' bash "$scripts/validate-release.sh" v1.2.4 "$RELEASE_SHA"
git checkout -qb side
git -c commit.gpgsign=false commit --allow-empty -qm 'Side branch fixture'
side_sha=$(git rev-parse HEAD)
reject 'not the release commit' bash "$scripts/validate-release.sh" v1.2.3 "$RELEASE_SHA"
git -c tag.gpgsign=false tag -f v1.2.3 >/dev/null
git push -q --force origin v1.2.3
reject 'must be on main' bash "$scripts/validate-release.sh" v1.2.3 "$side_sha"
git checkout -q main
reject 'no longer points' bash "$scripts/validate-release.sh" v1.2.3 "$RELEASE_SHA"
git -c tag.gpgsign=false tag -f v1.2.3 >/dev/null
git push -q --force origin v1.2.3

make_assets() {
  mkdir -p dist
  for name in install install.ps1 rustelup rustelup.ps1 \
    rustel_v1.2.3_{linux_amd64,linux_arm64,darwin_amd64,darwin_arm64,win32_amd64}.tar.gz; do
    if [[ "$name" = *.tar.gz ]]; then
      printf 'fixture: %s\n' "$name" > "dist/$name"
      echo '{}' > "dist/$name.sigstore.json"
    else
      cp "rustelup/$name" "dist/$name"
    fi
    sha256sum "dist/$name" | cut -d ' ' -f1 > "dist/$name.sha256"
  done
  echo '{}' > dist/installers.sigstore.json
  : > "$work/calls"
}
make_assets
bash "$scripts/verify-release-assets.sh" dist v1.2.3 "$RELEASE_SHA"
[ "$(grep -c '^attestation verify ' "$work/calls")" = 9 ] || fail 'Some assets were not verified'
echo changed >> dist/rustelup
sha256sum dist/rustelup | cut -d ' ' -f1 > dist/rustelup.sha256
reject 'Installer differs' bash "$scripts/verify-release-assets.sh" dist v1.2.3 "$RELEASE_SHA"
make_assets
echo tampered >> dist/rustel_v1.2.3_linux_amd64.tar.gz
reject 'Checksum mismatch' bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
make_assets
rm dist/rustel_v1.2.3_linux_amd64.tar.gz.sigstore.json
reject 'asset count' bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
make_assets
echo unexpected > dist/extra
reject 'asset count' bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
rm dist/extra
for reason in 'invalid signature' 'wrong repository' 'wrong workflow' 'wrong source commit' 'wrong source ref'; do
  : > "$work/calls"
  reject "$reason" env VERIFY_FAILURE="$reason" bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
  not_published
  if grep -q '^release create ' "$work/calls"; then fail 'Created a draft before verification'; fi
done
reject 'differs from the reviewed' env SITE_CHANGED=1 bash "$scripts/check-installers.sh"
reject 'download of rustel.cc' env FETCH_FAILURE=1 bash "$scripts/check-installers.sh"
: > "$work/calls"
reject 'differs from the reviewed' env SITE_CHANGED=1 bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
: > "$work/calls"
reject 'already has a release or draft' env EXISTING_RELEASES=1 bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
if grep -q '^release create ' "$work/calls"; then fail 'Created a second draft for the tag'; fi
make_assets
reject 'Provenance check failed' env FAIL_DOWNLOAD=1 bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
[ -d "$work/uploaded" ] || fail 'Failure did not reach the uploaded draft'
rm -r "$work/uploaded"
make_assets
reject 'needs a successful CI push run' env CI_AFTER_UPLOAD=0 bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA"
not_published
rm -r "$work/uploaded"
make_assets
bash "$scripts/publish-release.sh" v1.2.3 "$RELEASE_SHA" > "$work/output" 2>&1
[ "$(grep -c '^attestation verify ' "$work/calls")" = 18 ] || fail 'Uploaded assets were not rechecked'
tail -1 "$work/calls" | grep -q '^release edit .* --draft=false$'
echo 'release security tests passed'
