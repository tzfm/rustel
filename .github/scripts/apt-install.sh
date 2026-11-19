#!/usr/bin/env bash
set -euo pipefail

# Install Ubuntu packages on a CI runner. The Ubuntu mirrors fail now and
# then for some minutes, so each try waits longer before the next one.
attempts=6
for attempt in $(seq 1 "$attempts"); do
  if sudo apt-get -o Acquire::Retries=3 update \
    && sudo apt-get -o Acquire::Retries=3 install --yes "$@"; then
    exit 0
  fi
  if [[ "$attempt" -lt "$attempts" ]]; then
    wait=$((attempt * 30))
    echo "apt-get failed (try $attempt of $attempts), next try in ${wait}s"
    sleep "$wait"
  fi
done
echo "apt-get failed $attempts times" >&2
exit 1
