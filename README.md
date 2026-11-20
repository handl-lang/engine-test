# engine-test

`hnl-engine-test` — the bootstrap test runner.

An `engine-*` crate: test discovery, tag/filter predicates, scheduling, and
reporting for `.hnl` test suites, driven by language data rather than a
hardcoded test format.

## Development

```bash
cargo xtask gate        # fmt --check → clippy -D warnings → test → doc
```

`cargo xtask` wraps the repo's `xtask/` crate (`fmt`, `clippy`, `test`, `doc`,
`check`, `gate`); `ci.sh` is the gate entry. Deps resolve from git, so no
sibling checkouts are needed; a plain clone test-runs as-is.

## License

MIT.
