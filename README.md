# tool-test

`hnl-tool-test` — the bootstrap test runner.

A `tool-*` crate: test discovery, tag/filter predicates, scheduling, and
reporting for `.hnl` test suites, driven by language data rather than a
hardcoded test format.

## Crates

- `hnl-tool-test` (root) — the bootstrap test runner.
- `handl-kit-test` (`crates/kit-test`) — the test-framework kit binding:
  the pure-Handl `test`/`property`/`benchmark`/`fuzz`/`snapshot`/`assert`
  modules plus the deterministic `rand-stub` backend, registered as host
  modules via `handl_kit_core`. Absorbed from the `kit-test` repo (the
  Rust crate is test-framework machinery, not a kit; `kit-test` keeps only
  its `kit.hnl` manifest).

## Development

```bash
cargo xtask gate        # fmt --check → clippy -D warnings → test → doc
```

`cargo xtask` wraps the repo's `xtask/` crate (`fmt`, `clippy`, `test`, `doc`,
`check`, `gate`); `workflows/gate.yaml` is the gate entry. Deps resolve from git, so no
sibling checkouts are needed; a plain clone test-runs as-is.

## License

MIT.
