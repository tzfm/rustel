#!/usr/bin/env bash
set -euo pipefail

# Runs the tests of the hand-written unsafe code under Miri. Miri interprets
# the code and stops on undefined behavior: a read outside a buffer, a freed
# pointer, uninitialized memory, a data race or a leak.
#
# Miri does not run C code or inline assembly. A test with QuickJS, an audio
# device or an OS call stays out of this list. When you add unsafe code, add
# the module of its tests here.

# Miri needs a nightly toolchain. The date is fixed so a new nightly does not
# break a pull request. Move the date by hand.
toolchain="${MIRI_TOOLCHAIN:-nightly-2026-10-07}"
rustup toolchain install "$toolchain" --profile minimal --no-self-update \
  --component miri --component rust-src

# The tests compare float bits. Without this flag, Miri changes the sign of a
# NaN and the last digit of some math functions on purpose.
export MIRIFLAGS="-Zmiri-deterministic-floats"

# A filter with no match passes with zero tests, so refuse an empty run.
log=$(mktemp)
trap 'rm -f "$log"' EXIT
miri() {
  cargo "+$toolchain" miri test "$@" 2>&1 | tee "$log"
  if grep -q '^running 0 tests' "$log"; then
    echo "error: a filter matches no test: cargo miri test $*" >&2
    exit 1
  fi
}

# Callback host, metrics and cancel pointers.
miri -p rustel-core --lib -- callback_query_metrics_tests value::tests
miri -p rustel-core --test js_query_refusal --test stack_failure_isolation \
  --test review_remediations

# Event ring, asset ring, input ring and unchecked sample reads.
miri -p rustel-audio --lib -- ring:: assets:: input:: \
  sample::interpolation_tests sample::reversed_tests
miri -p rustel-audio --test ring_roles --test no_alloc_callback -- \
  released_roles tripwire ring_callback_path

# The producer and the consumer on two threads, under 16 thread schedules.
MIRIFLAGS="$MIRIFLAGS -Zmiri-many-seeds=0..16" \
  miri -p rustel-audio --lib -- observed_depth_stays_bounded ownership_tests

# AVX2 kernels. Miri reports only the CPU features of the build, so this build
# turns AVX2 on.
RUSTFLAGS="-C target-feature=+avx2" miri -p rustel-audio --lib -- _kernel::

# QuickJS allocator and bridge frame stack.
miri -p rustel-jsruntime --lib -- alloc:: bridge:: runtime::inspection::
