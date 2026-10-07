# Rustel coding style

Use these guidelines when writing or reviewing Rustel code and documentation.
Read [CONTRIBUTING.md](CONTRIBUTING.md) for the code map and check commands.

## Scope and behavior

Treat the existing implementation as trusted unless the task identifies a
specific defect. Leave clear code alone. Prefer small changes with an obvious
purpose, and ask when the intended behavior or scope is unclear.

A readability pass preserves logic, DSP expressions, execution order, APIs,
types, ownership, dependencies, and feature gates. Numerical behavior, event
timing, cancellation, and real-time constraints are part of the contract.
Even algebraically equivalent DSP expressions can round differently. Report
deeper issues separately instead of fixing them during cleanup.

For new behavior, follow the surrounding design. Introduce an abstraction
only when it solves a concrete problem. Avoid speculative extension points,
broad renaming, mechanical rewrites, and unrelated formatting changes.

## Names and comments

Use names that describe the current role of an item. Rename a local variable
only when the improvement is clear. Reserve `generated` for output that is
actually produced by a generator, and keep its regeneration instructions.
Check public paths and callers before renaming a file or module.

Comments explain intent, units, invariants, safety, or reasoning that the code
does not make apparent. Check the explanation against the implementation.
Do not infer an author's intent or promise guarantees the code cannot provide.

- State whether a value is in samples, frames, channels, seconds, or cycles
  when that distinction matters.
- Explain allocation bounds, overflow cases, cursor margins, and memory
  ordering where a future edit could break them.
- Preserve safety contracts and the conditions that justify unsafe code.
- Use a compact ASCII diagram only when it clarifies signal flow, a buffer
  layout, or a state transition better than prose.
- Do not narrate obvious syntax or add a comment to every constant or branch.

Take inspiration from ASD-STE100 for clear technical English. Keep comments
concise and explain what the code alone does not make clear. Use consistent
terms and established Rust and audio terminology.

Write short, direct sentences in the present tense. For example, "The reader
needs room for the whole callback block" explains a constraint more clearly
than "The margin is the load-bearing part." A longer explanation is useful
when the reasoning needs it; length alone is not a defect.

Avoid slogans, theatrical phrasing, generic praise, repeated restatements,
and forced wit. Do not replace useful technical detail with vague brevity or
deliberately inconsistent prose. Clarity matters more than a uniform voice.

## Files and responsibilities

Split a large production module only when the task permits structural changes
and there are clear responsibilities to separate. File length is a reason to
inspect, not a target to meet. Stop when further splitting would scatter
closely related state, ordering, or lifetime rules across files.

The studio App provides one established pattern: keep the type and entry
points in the parent, with related methods in focused child modules. Use the
narrowest visibility needed between those modules. A split should not require
a new public API, new shared state, or a redesign of ownership.

Keep moved function bodies unchanged apart from path corrections required by
the move. Review structural moves separately from wording edits so behavior
changes are easy to spot. Do not split a large catalogue or generated table
merely because it tops a line-count report. Separate production code, tests,
maintained data, and generated output in those reports; say whether comments
and blank lines are included.

## Tests

Unit tests live in the file they test, at the bottom, in a `#[cfg(test)]`
module - the layout the Rust book teaches. They use `use super::*;` and may
exercise private behavior directly. Keep them small and focused on the
unit's logic; group a large suite into named child modules inside the same
inline block rather than splitting it into separate files. Do not extract
unit tests into `#[path]` modules or `src/tests/` trees.

A crate's top-level `tests/` directory is for integration tests that
exercise its public API across longer scenarios. Keep shared fixtures and
process-wide synchronization shared there: splitting files must not turn
one lock into several independent locks. Do not expose internals just to
move a test into the integration suite.

Preserve feature gates, platform conditions, ignored tests, and test helpers.

Remove a test only after proving that retained coverage exercises the same
behavior, conditions, and assertions. Unit and end-to-end tests can exercise
the same feature at different boundaries and still provide distinct coverage.
Test names should identify the behavior or regression; descriptive names are
useful even when long.

Do not add tests that merely repeat the implementation for a comment or file
move. Verify those changes with existing tests and comparisons of moved code.
Preserve golden expectations for retained tests; deleting a proven duplicate
fixture may also remove its matching golden entry. Never change expected
results or weaken assertions to make a readability pass green.

## Documentation and attribution

Start user documentation with what someone needs to do and a small working
example. Put detailed internals, platform troubleshooting, and contributor
procedures in the relevant reference pages. Link to existing explanations
instead of duplicating them. Update paths and commands when files move.

Use `https://github.com/tzfm/rustel` for project repository links.

Describe Rustel's current implementation directly. Mention Strudel, WebAudio,
or worklets when they explain a compatibility rule, a reference fixture, or
source provenance. Avoid browser terminology for a native component with a
different implementation. State the limits of parity and performance claims,
including feature, hardware, and platform conditions.

Remove obsolete plans, internal work notes, and implementation history that
no longer helps a reader. Keep still-valid rationale, documented limitations,
and reproduction details needed to understand a remaining issue. Useful ignore
rules and accurate generated-file notices should stay.

Preserve licenses, attribution, and notices for copied or adapted material.
Do not add Strudel copyright to unrelated original work merely because the
project began as a port. Do not remove existing credits on the assumption that
a substantial rewrite makes them unnecessary. Check provenance and the
applicable license before changing attribution; flag uncertainty for review.

## Review and checks

Use the project's formatter, lint settings, and relevant tests without changing
their configuration. Run focused checks during development and the required
project checks before completion. Include affected feature combinations when
moving gated code or tests. For test extraction, compare test inventories and
moved bodies as well as running the suites.

Review the final diff for accidental changes to expressions, ordering,
visibility, ownership, synchronization, and feature gates. For moves,
`git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change`
helps distinguish relocation from edits.

Report what changed, why it helps, which checks actually ran, and any remaining
limitations. Investigate failures enough to distinguish regressions from
existing problems. Keep unrelated fixes out of the change, and do not describe
skipped or blocked checks as passed.
