# Utils

Standalone Rust workspace for Copilot harness utilities. Each utility is its own crate and binary; the workspace has no shared library crate.

## Commands

Run from this directory:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The Copilot Makefile builds these binaries in release mode and installs them under `../bin`.

## Conventions

- Validate input at parse boundaries and represent valid states with types. Return `Result` for expected failures instead of panicking.
- Keep pure state transitions separate from filesystem, process, and other I/O.
- Document public items and non-obvious invariants. Write for an advanced beginner in Rust; omit narration that restates the code.
