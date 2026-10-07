#!/usr/bin/env bash
set -euo pipefail

# A self-hosted runner may share its host with interactive work.  Let that
# work pre-empt compilation and tests instead of allowing CI to make the host
# unresponsive.  Keyed on the runner being self-hosted rather than on it being
# Linux: an ephemeral hosted runner has nothing colocated to starve, so
# deprioritising its I/O only makes the job slower.  The fallback keeps the
# same workflow portable to hosted macOS and Windows runners.
if [[ "${RUNNER_ENVIRONMENT:-}" == "self-hosted" ]] \
  && [[ "${RUNNER_OS:-}" == "Linux" ]] \
  && command -v nice >/dev/null 2>&1 \
  && command -v ionice >/dev/null 2>&1; then
  exec nice -n 10 ionice -c 3 cargo "$@"
fi

exec cargo "$@"
