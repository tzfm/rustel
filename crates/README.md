# Workspace crates

Each crate's `Cargo.toml` states its purpose in its `description`. Crates live
in separate directories so each layer keeps a narrow public API and an
independently testable contract.

For a native desktop app or another host, start with
[Embedding the engine](../docs/embedding.md). `rustel-engine` re-exports the
pattern, scheduling, voice and audio layers without enabling the terminal
application's defaults. A browser host builds on its base profile.
