# Working on fomkee-agent

`DESIGN.md` is authoritative for the public agent architecture. In the Fomkee
monorepo, the repository-root `AGENTS.md` and ADR-006 also apply. This is an
independent Rust workspace and process. It must not depend on the Fomkee server
or WASM crates.

All external input is parsed into validated types. Collector failures are data;
critical scheduler or server task failures are supervised process failures.
Never expose secrets, raw source payloads, or unbounded labels in snapshots.

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo nextest run` from this directory before committing.
