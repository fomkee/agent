# Working on fomkee-agent

The repository root `AGENTS.md`, ADR-006, and this directory's `DESIGN.md` are
authoritative. This is an independent Rust workspace and process. It must not
depend on the Fomkee server or WASM crates.

All external input is parsed into validated types. Collector failures are data;
critical scheduler or server task failures are supervised process failures.
Never expose secrets, raw source payloads, or unbounded labels in snapshots.

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo nextest run` from this directory before committing.
