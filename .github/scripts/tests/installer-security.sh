#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export FIXTURE="$work"
export RUSTEL_DIR="$work/installed"
mkdir -p "$work/bin" "$work/payload" "$RUSTEL_DIR/bin"
export PATH="$work/bin:$PATH"
if ! command -v sha256sum >/dev/null 2>&1; then
  printf '#!/usr/bin/env bash\nexec shasum -a 256 "$@"\n' > "$work/bin/sha256sum"
  chmod +x "$work/bin/sha256sum"
fi

cat > "$work/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
for arg in "$@"; do
  case "$arg" in https://*) url=$arg ;; esac
done
case "$url" in
  */latest) echo '{"tag_name":"v1.2.3"}'; exit 0 ;;
esac
while [ "$1" != -o ]; do shift; done
case "$url" in
  *.sha256)
    [ "${SCENARIO:-}" != missing ] || exit 1
    if [ "${SCENARIO:-}" = invalid ]; then echo invalid > "$2"
    else cp "$FIXTURE/checksum" "$2"; fi ;;
  */rustelup) cp "$FIXTURE/payload/rustel" "$2" ;;
  *.tar.gz) cp "$FIXTURE/archive" "$2" ;;
  *) exit 1 ;;
esac
MOCK
cat > "$work/bin/uname" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = -s ]; then echo "${TEST_OS:-Linux}"; else echo x86_64; fi
MOCK
printf '#!/usr/bin/env bash\nexit 1\n' > "$work/bin/pgrep"
cat > "$work/payload/rustel" <<'BINARY'
#!/usr/bin/env bash
echo invoked >> "$FIXTURE/executed"
if [ "${SCENARIO:-}" = before ]; then exit 1; fi
if [ "${SCENARIO:-}" = after ] && [[ "$0" != */.rustelup*/* ]]; then exit 1; fi
echo 'rustel fixture'
BINARY
chmod +x "$work/bin/"* "$work/payload/rustel"
tar czf "$work/archive" -C "$work/payload" rustel
printf 'previous installation\n' > "$work/previous"

for program in rustelup install; do
  target=rustel
  payload=archive
  if [ "$program" = install ]; then target=rustelup; payload=payload/rustel; fi
  for scenario in tampered invalid missing before after; do
    cp "$work/previous" "$RUSTEL_DIR/bin/$target"
    sha256sum "$work/$payload" | cut -d ' ' -f1 > "$work/checksum"
    if [ "$scenario" = tampered ]; then printf '%064d\n' 0 > "$work/checksum"; fi
    rm -f "$work/executed"
    if SCENARIO="$scenario" bash "$root/rustelup/$program" > "$work/output" 2>&1; then
      echo "$program accepted $scenario input" >&2
      exit 1
    fi
    cmp "$work/previous" "$RUSTEL_DIR/bin/$target"
    case "$scenario" in
      tampered|invalid|missing) [ ! -e "$work/executed" ] ;;
      after) grep -q 'previous .* restored' "$work/output" ;;
    esac
  done
done

for os in Linux Darwin; do
  sha256sum "$work/archive" | cut -d ' ' -f1 > "$work/checksum"
  TEST_OS="$os" bash "$root/rustelup/rustelup" > "$work/output" 2>&1
  cmp "$work/payload/rustel" "$RUSTEL_DIR/bin/rustel"
done
echo 'installer security tests passed'
