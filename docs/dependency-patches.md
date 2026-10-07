# Dependency patches

- **Crossterm:** pinned to [tzfm/crossterm](https://github.com/tzfm/crossterm/commit/519c3ed7b9b561b8bbb1a3fa15ccc93b81fe8892) for iTerm2 function keys and Unix F15-F20 key offsets.
  Replace the pin when an official release includes [PR #1125](https://github.com/crossterm-rs/crossterm/pull/1125) and [PR #1127](https://github.com/crossterm-rs/crossterm/pull/1127).
- **rquickjs 0.14:** pinned to [tzfm/rquickjs](https://github.com/tzfm/rquickjs/commit/ed2337c6f187996ef97f7e4e44458cfbc33266f4) so cancellation can discard queued JavaScript jobs without executing them.
  Its QuickJS submodule pins [tzfm/quickjs](https://github.com/tzfm/quickjs/commit/9d69e9274835604995d05682796ff96db4c29a47).
  This revision preserves new jobs that finalizers queue while old jobs are discarded.
  Replace the pin when a compatible official rquickjs release includes the cancellation API and this finalizer fix.
  The API proposals are [QuickJS-NG #1745](https://github.com/quickjs-ng/quickjs/pull/1745) and [rquickjs #763](https://github.com/DelSkayn/rquickjs/pull/763).
