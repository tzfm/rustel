# Dependency patches

- **Crossterm:** pinned to [tzfm/crossterm](https://github.com/tzfm/crossterm/commit/88dc698277fa9470a5d1a268244c95069af39bb6) for iTerm2 function keys, Unix F15-F20 key offsets, and a key that stays unread when it arrives with a terminal resize.
  Replace the pin when an official release includes [PR #1125](https://github.com/crossterm-rs/crossterm/pull/1125), [PR #1127](https://github.com/crossterm-rs/crossterm/pull/1127) and [PR #1128](https://github.com/crossterm-rs/crossterm/pull/1128).
- **rquickjs 0.14:** pinned to [tzfm/rquickjs](https://github.com/tzfm/rquickjs/commit/ed2337c6f187996ef97f7e4e44458cfbc33266f4) so cancellation can discard queued JavaScript jobs without executing them.
  Its QuickJS submodule pins [tzfm/quickjs](https://github.com/tzfm/quickjs/commit/9d69e9274835604995d05682796ff96db4c29a47).
  This revision preserves new jobs that finalizers queue while old jobs are discarded.
  Replace the pin when a compatible official rquickjs release includes the cancellation API and this finalizer fix.
  The API proposals are [QuickJS-NG #1745](https://github.com/quickjs-ng/quickjs/pull/1745) and [rquickjs #763](https://github.com/DelSkayn/rquickjs/pull/763).
