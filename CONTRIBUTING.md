# Contributing

Thanks for helping improve Fomkee Agent.

Read [DESIGN.md](DESIGN.md) before changing collection, snapshot, scheduling,
or publishing behavior. Keep external input bounded and typed, never expose
credentials or raw source payloads, and preserve the zero-panic production
contract described in [AGENTS.md](AGENTS.md).

Run these checks before opening a pull request:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The canonical source lives in the private Fomkee monorepo and is exported to
this public repository. Public pull requests are welcome and are imported into
the canonical subtree by maintainers. Never add private repository content or
credentials to issues, commits, tests, or workflows.

## Releases

A pushed `vVERSION` tag in `fomkee/agent` runs the Linux x86_64 and ARM64
build/test matrix. The tag must match `fomkee-agent --version`. Only the
organization repository may publish release assets.

Each release contains architecture-specific archives, individual SHA-256
files, the full changelog, and a combined `SHA256SUMS` manifest. Existing tags
and releases are immutable and must never be moved or overwritten.
