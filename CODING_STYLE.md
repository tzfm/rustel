# Rustel coding style

These conventions apply to Rustel's code, tests, and documentation. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the codebase layout, development
workflow, and required checks.

## General principles

Keep changes focused. Existing code should generally be left alone unless
there's a specific reason to change it. A small, well-understood improvement
is preferable to a broad cleanup that touches unrelated code.

Readability changes should not affect behavior. This includes logic, DSP
calculations, execution order, public APIs, types, ownership, dependencies,
and feature gates. Audio output, numerical precision, event timing,
cancellation, and real-time behavior all matter. Even mathematically
equivalent DSP expressions can produce different floating-point results.

If a cleanup reveals a deeper problem, document it and address it separately.

New functionality should fit the existing design. Add abstractions when they
solve a real problem, not in anticipation of future requirements. Avoid
unnecessary renaming, mechanical rewrites, and formatting changes unrelated
to the work at hand.

## Naming and comments

Names should reflect what something actually does. Renaming a local variable
is worthwhile when the result is clearly easier to understand, but
consistency alone is not a reason to rename things.

Use `generated` only for files or data produced by a generator, and keep the
instructions needed to regenerate them. Before renaming a public module or
file, check its callers and any documented paths.

Comments are most useful when they explain something the code cannot
communicate on its own: intent, units, invariants, safety requirements, or a
non-obvious design decision.

In particular:

- Identify units such as samples, frames, channels, seconds, and cycles
  wherever confusion is possible.
- Explain allocation bounds, overflow handling, cursor margins, and memory
  ordering when those details are important to correctness.
- Keep the safety contracts and assumptions associated with `unsafe` code.
- Use a small ASCII diagram when it makes signal flow, buffer layout, or state
  transitions easier to understand.
- Avoid comments that merely restate the code.

Technical writing should be clear and precise. ASD-STE100 is a useful
reference for simple technical English, although Rustel does not need to
follow it formally.

Prefer short sentences, present tense, and established Rust and audio
terminology. For example, "The reader needs room for the whole callback
block" is more useful than an abstract description of the buffer margin.

A longer explanation is fine when the reasoning warrants it. Comments should
contain enough information for someone unfamiliar with the original
implementation to make a safe change later.

Avoid jokes, slogans, exaggerated language, and unnecessary commentary in
technical explanations.

## Modules and file organization

Organize code around responsibilities rather than file size.

A large file may be worth examining, but splitting it should make the code
easier to navigate and maintain. Closely related state, execution order, and
lifetime constraints are often better kept together.

The studio `App` is an example of the preferred organization: the main type
and entry points remain in the parent module, while related methods live in
focused child modules. Visibility between modules should remain as narrow as
possible.

Moving code into another file should not require new public APIs, shared
state, or changes to ownership. When relocating functions, leave their bodies
unchanged except for necessary path adjustments.

Keep structural changes separate from readability edits whenever practical.
This makes reviewing the diff much easier.

Large catalogues, maintained data, and generated tables do not need to be
split simply because they contain many lines.

When reviewing file sizes, distinguish production code, tests, maintained
data, and generated output. Specify whether the counts include comments and
blank lines.

## Tests

Unit tests belong at the bottom of the source file they exercise, inside an
inline `#[cfg(test)]` module. Use `use super::*;` so tests can access private
implementation details without exposing them through the public API.

Keep unit tests focused on the behavior of the module. If a test suite
becomes large, organize related cases into named child modules within the
same inline test block.

Avoid separate `#[path]` test modules or `src/tests/` directories for unit
tests.

A crate's top-level `tests/` directory is reserved for integration tests that
exercise its public API, often across longer scenarios.

Shared test fixtures and process-wide synchronization should remain shared.
Splitting a test suite must not accidentally replace one synchronization lock
with several independent ones.

When reorganizing tests:

- Preserve feature gates, platform-specific conditions, ignored tests, and
  existing helpers.
- Keep tests that cover different boundaries, even when they exercise the
  same feature.
- Remove duplicates only after confirming that the remaining tests cover the
  same behavior, conditions, and assertions.
- Preserve expected values and golden fixtures for retained tests.
- Keep test names descriptive enough to identify the behavior or regression
  being checked.

Repeated test cases can be combined when they share setup and expectations.
Each case should still identify its input, expected result, and a useful
failure message.

Tests should verify observable behavior rather than source formatting,
implementation text, or documentation wording. Checks that enforce no
meaningful contract can be removed.

Test prerequisites should be checked at runtime when the suite depends on
them.

Moving functions or editing comments normally does not justify additional
tests that duplicate the implementation. Existing tests and a comparison of
the moved code are more appropriate.

Never weaken assertions or alter expected results simply to make a
readability change pass.

## Documentation

User documentation should begin with the information someone needs to get
started, ideally with a small working example.

Detailed implementation notes, platform-specific troubleshooting, and
contributor procedures belong in their respective reference pages. Link to
existing documentation rather than repeating it.

Use `https://github.com/tzfm/rustel` when linking to the project repository.
Keep paths and commands current when the codebase changes.

Describe Rustel as it exists today. References to Strudel, WebAudio, and
worklets are appropriate when discussing compatibility, reference fixtures,
or the origin of an implementation. Native Rustel components should use
terminology appropriate to their actual design.

Performance and compatibility claims should identify their limits, including
relevant features, platforms, and hardware.

Remove outdated plans, temporary development notes, and implementation
history that no longer helps readers. Keep explanations of design decisions,
known limitations, and reproduction steps that remain relevant.

### Licenses and attribution

Preserve copyright notices, licenses, and attribution for copied or adapted
material.

Rustel's origins as a Strudel port do not mean every original component needs
Strudel copyright attribution. Conversely, rewriting a component does not
automatically eliminate the attribution requirements of its source material.

Check provenance and licensing before changing notices. When ownership or
attribution is uncertain, leave the notice in place and raise the question
for review.

Keep useful ignore rules and accurate generated-file notices.

## Ownership and data flow

Think through ownership and the movement of data before implementing a
feature.

Prefer borrowing and slices when ownership does not need to be transferred.
Cloning, `to_vec()`, `Arc`, and additional lifetime complexity should have a
concrete reason. Where that reason is not obvious, explain it briefly near
the relevant code.

A borrow-checker error is usually a reason to reconsider how data is owned or
when it is accessed. Adding a clone or reference count just to satisfy the
compiler can hide an underlying design problem.

Functionality should live in the component that owns the relevant state and
control flow. If a feature cannot fit naturally without changes to ownership,
lifetimes, or execution flow, make those structural changes rather than
working around the existing design.

Keep module boundaries clear and ownership intentional. Control flow should
be understandable without tracing unrelated helpers or hidden state changes.

## Concurrency and real-time code

Prefer message passing for shared mutable state, with explicit backpressure
where necessary.

When locks are unavoidable, keep critical sections short or partition the
protected state. The audio callback must not block waiting for a lock.

State machines should be recognizable in the code. When booleans, optional
values, or counters represent different phases of operation, make the valid
states and transitions clear.

Be especially careful with code that affects:

- Audio callback deadlines and allocations.
- Evaluation worker performance.
- Event ordering, scheduling, and cancellation.
- Shared-state synchronization and memory ordering.
- DSP results and floating-point precision.

Avoid unnecessary allocations on performance-sensitive paths, particularly
the audio callback and evaluation worker.

Changes in these areas must preserve audio output, timing, and existing
public behavior unless the purpose of the change is explicitly to modify
them.

## Reviewing changes

Use the project's existing formatter, lint configuration, and test commands.
The relevant commands are documented in [CONTRIBUTING.md](CONTRIBUTING.md).

During development, run focused checks for the affected code. Before
completing a change, run the required project checks and include relevant
feature combinations when feature-gated code or tests have moved.

For structural changes, compare the original and moved function bodies. When
relocating tests, compare the test inventory before and after the change to
catch missing cases.

Git's moved-code highlighting is useful for this:

```bash
git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change
```

Review the final diff carefully, especially for unintended changes to:

- DSP expressions and execution order.
- Visibility, ownership, and lifetimes.
- Synchronization and shared state.
- Feature gates and platform-specific behavior.
- Test assertions and expected results.

A change should be accompanied by a short explanation of what changed, why it
was necessary, which checks ran, and any known limitations.

Investigate test failures far enough to distinguish an existing issue from a
regression. Keep unrelated fixes in separate changes, and report checks that
could not run rather than treating them as successful.
