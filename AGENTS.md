<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->

# Agent guide — engine-test

The test-framework side of the engine family: `hnl-engine-test` (root
package — the ADR-0089 bootstrap test runner: discovery, tag/filter
predicates, scheduling, reporting for `.hnl` suites) plus
`handl-kit-test` (`crates/kit-test` — the test-framework kit binding:
pure-Handl `test`/`property`/`benchmark`/`fuzz`/`snapshot`/`assert`
modules + the deterministic `rand-stub` backend, vendored from
`lib-test`/`lib-rand-stub`/`lib-posix-stub` by `build.rs` via
`handl-pkg-vendor`). `handl-kit-test` was absorbed from the `kit-test`
repo — the Rust crate is test machinery, not a kit; `kit-test` keeps
only its `kit.hnl` manifest. Crate name kept as `handl-kit-test` (a
rename is a separate follow-up).

`src/watch.rs` is the `--watch` mode (`hnl test --watch`, wired in the
`ext-test` repo): `run_watch`/`run_watch_loop` re-discover and re-run
affected cases per debounced `hnl-engine-watch` event batch — see the
module docs for the rerun-granularity contract.
