//! `--watch` mode: run the suite once, then re-run the affected tests on
//! every debounced burst of filesystem events under the watch roots.
//!
//! The loop is single-threaded and never returns on its own — it is the
//! `hnl test --watch` shape: discover → run → block on
//! [`hnl_engine_watch::Debouncer`] → re-discover → re-run. Each finished
//! run is handed to a caller-supplied sink ([`WatchRound`]) so the CLI can
//! stream reports instead of returning one-shot JSON.
//!
//! # Rerun granularity
//!
//! Every round **re-discovers** the plan (adds/deletes/renames are picked
//! up for free), then runs only the *affected* cases: a [`TestCase`] is
//! affected when its module path matches a changed path, or its eval
//! `preamble` (the rewritten file imports — see
//! `TestCase::preamble`) names a changed path. When a
//! batch cannot be attributed to any case — a `.handl.toml` edit, a
//! helper file no test imports, a directory event — the round falls back
//! to a full rerun rather than silently skipping.
//!
//! The watcher itself only surfaces source-relevant events
//! ([`is_source_path`]: `.hnl`/`.handl` files and `.handl.toml`), so build
//! output, `.git`, and editor scratch never trigger a round.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use handl_plugin_lib_scheduler::SchedulePolicy;
use hnl_engine_watch::{Debouncer, Error as WatchBackendError, FsEvent, Watcher};

use crate::config::{RunConfig, run_with_config};
use crate::runner::{DiscoverError, TestCase, TestKitSource, TestPlan};
use crate::{Filter, RunReport, discover};

/// Default quiescence window for rerun batching — long enough to collapse
/// an editor's write+rename pair, short enough to feel instant.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Configuration for [`run_watch`].
///
/// `paths` are the discovery roots (the same paths `hnl test` takes);
/// `watch_roots` are the filesystem roots the watcher subscribes to. An
/// empty `watch_roots` defaults to the discovery paths themselves — pass
/// the project root explicitly for vitest-style coverage of helpers that
/// live outside the test directories.
#[derive(Clone)]
pub struct WatchRunOptions {
    /// Discovery roots (files or directories), resolved to absolute paths
    /// by [`run_watch`] before use.
    pub paths: Vec<PathBuf>,
    /// Filesystem watch roots. Empty = [`default_watch_roots`]`(&paths)`.
    pub watch_roots: Vec<PathBuf>,
    /// Selection predicate ANDed into every round's [`RunConfig`].
    pub predicate: Filter,
    /// Scheduler policy for each round.
    pub policy: SchedulePolicy,
    /// Debounce window; `Duration::ZERO` falls back to
    /// [`DEFAULT_DEBOUNCE`].
    pub debounce: Duration,
    /// Rerun rounds to perform after the initial run. `None` (the CLI
    /// default) watches until the process is killed; `Some(n)` returns
    /// `Ok(())` after `n` rerun rounds — the test hook.
    pub max_rounds: Option<usize>,
}

impl Default for WatchRunOptions {
    fn default() -> Self {
        WatchRunOptions {
            paths: Vec::new(),
            watch_roots: Vec::new(),
            predicate: Filter::all(),
            policy: SchedulePolicy::default(),
            debounce: DEFAULT_DEBOUNCE,
            max_rounds: None,
        }
    }
}

/// One completed watch round.
#[derive(Debug)]
pub struct WatchRound {
    /// Round index: `0` is the initial run (before any fs event);
    /// `1..` are reruns in event order.
    pub index: usize,
    /// Source paths that triggered this round (empty for the initial
    /// run), deduplicated in event order.
    pub changed: Vec<PathBuf>,
    /// The plan actually run — the full re-discovered plan on round 0,
    /// the affected subset (or full plan on fallback) afterwards.
    pub plan: TestPlan,
    /// The run report for `plan`.
    pub report: RunReport,
}

/// Errors from [`run_watch`] / [`run_watch_loop`].
#[derive(Debug)]
pub enum WatchError {
    /// The `hnl-engine-watch` backend failed (watch setup, backend error,
    /// channel disconnect).
    Watch(WatchBackendError),
    /// Test discovery failed on a round.
    Discover(DiscoverError),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Watch(e) => write!(f, "watch: {e}"),
            WatchError::Discover(e) => write!(f, "test: discovery error: {e}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Watch(e) => Some(e),
            WatchError::Discover(e) => Some(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Path classification
// ---------------------------------------------------------------------------

/// Whether `path` can affect a test run: a `.hnl`/`.handl` source file or
/// a `.handl.toml` project manifest (dependency/patch edits change module
/// resolution for every test).
pub fn is_source_path(path: &Path) -> bool {
    if path.file_name().is_some_and(|n| n == ".handl.toml") {
        return true;
    }
    path.extension().is_some_and(|e| e == "hnl" || e == "handl")
}

/// Default watch roots for discovery `paths`: directories watch
/// themselves, files watch their parent directory. Absolute, deduplicated,
/// order-preserving; falls back to the current directory when `paths` is
/// empty.
pub fn default_watch_roots(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for p in paths {
        let root = if p.is_dir() {
            p.clone()
        } else {
            p.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| p.clone())
        };
        let root = absolutize(&root);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    if roots.is_empty()
        && let Ok(cwd) = std::env::current_dir()
    {
        roots.push(cwd);
    }
    roots
}

/// Absolute form of `path` without requiring it to exist
/// (`std::path::absolute` — no symlink resolution, so it composes with
/// watcher-reported paths).
fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Canonical form for comparison: `canonicalize` when the file exists
/// (resolves symlinks and `.`/`..`), otherwise the absolutized path.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| absolutize(path))
}

/// The watcher-side event filter: only source-relevant events reach the
/// consumer, so noisy trees never fill the channel.
fn source_filter() -> hnl_engine_watch::Filter {
    hnl_engine_watch::Filter::new(|e| {
        is_source_path(&e.path) || e.to.as_deref().is_some_and(is_source_path)
    })
}

/// Deduplicated, order-preserving list of source paths an event batch
/// touched (`path` plus the rename destination `to`).
fn relevant_dedup(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if is_source_path(&p) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Extract the changed source paths from a debounced event batch.
pub fn changed_paths(batch: &[FsEvent]) -> Vec<PathBuf> {
    relevant_dedup(
        batch
            .iter()
            .flat_map(|e| e.to.iter().chain(std::iter::once(&e.path)).cloned()),
    )
}

// ---------------------------------------------------------------------------
// Affected-case selection
// ---------------------------------------------------------------------------

/// Whether `path` matches any changed path: equal to it, or under it
/// (a changed directory invalidates everything beneath). Compares both
/// the raw and canonicalized forms so relative discovery paths and
/// absolute watcher paths line up.
fn path_matches(path: &Path, changed: &[PathBuf], changed_norm: &[PathBuf]) -> bool {
    let norm = normalize(path);
    changed.iter().zip(changed_norm).any(|(raw, cn)| {
        norm == *cn || norm.starts_with(cn) || path == raw || path.starts_with(raw)
    })
}

/// Whether `case` is affected by `changed`: its module file changed, or a
/// file its eval preamble imports changed (the preamble embeds the
/// resolved absolute import paths collected at discovery).
fn case_affected(case: &TestCase, changed: &[PathBuf], changed_norm: &[PathBuf]) -> bool {
    if path_matches(&case.module, changed, changed_norm) {
        return true;
    }
    changed.iter().zip(changed_norm).any(|(raw, cn)| {
        let raw_s = raw.to_string_lossy();
        let norm_s = cn.to_string_lossy();
        (!raw_s.is_empty() && case.preamble.contains(raw_s.as_ref()))
            || (!norm_s.is_empty() && case.preamble.contains(norm_s.as_ref()))
    })
}

/// Narrow `plan` to the cases affected by `changed` source paths.
///
/// Returns the full plan unchanged when `changed` is empty **or** when no
/// case/parse-error attributes to it — an unattributed source change
/// (e.g. `.handl.toml`, a not-imported helper) is treated as
/// possibly-global and reruns everything rather than skipping.
pub fn select_affected(plan: TestPlan, changed: &[PathBuf]) -> TestPlan {
    if changed.is_empty() {
        return plan;
    }
    let changed_norm: Vec<PathBuf> = changed.iter().map(|p| normalize(p)).collect();
    let tests: Vec<TestCase> = plan
        .tests
        .iter()
        .filter(|c| case_affected(c, changed, &changed_norm))
        .cloned()
        .collect();
    let parse_errors: Vec<(PathBuf, String)> = plan
        .parse_errors
        .iter()
        .filter(|(p, _)| path_matches(p, changed, &changed_norm))
        .cloned()
        .collect();
    if tests.is_empty() && parse_errors.is_empty() {
        return plan;
    }
    TestPlan {
        tests,
        parse_errors,
    }
}

// ---------------------------------------------------------------------------
// The watch loop
// ---------------------------------------------------------------------------

/// Run `options.paths` once, then re-run affected tests on every debounced
/// fs-event batch until `options.max_rounds` reruns complete (or forever
/// when `None`). Each round's [`WatchRound`] is passed to `sink`.
///
/// `source` is the injected base-runtime factory — invoked on scheduler
/// worker threads like any other run (see [`TestKitSource`]).
///
/// # Errors
///
/// Returns [`WatchError::Discover`] when discovery fails on any round, or
/// [`WatchError::Watch`] when the backend reports an error or
/// disconnects.
pub fn run_watch_loop(
    source: &TestKitSource,
    options: &WatchRunOptions,
    debouncer: &Debouncer,
    sink: &mut dyn FnMut(&WatchRound),
) -> Result<(), WatchError> {
    // Round 0: the plain run every `hnl test` invocation performs.
    let plan = discover(source, &options.paths).map_err(WatchError::Discover)?;
    let report = run_round(source, &plan, options);
    sink(&WatchRound {
        index: 0,
        changed: Vec::new(),
        plan,
        report,
    });

    let mut round = 0usize;
    while options.max_rounds.is_none_or(|max| round < max) {
        let batch = debouncer.recv_batch().map_err(WatchError::Watch)?;
        let changed = changed_paths(&batch);
        if changed.is_empty() {
            // Events we filtered out at the watcher never arrive; this is
            // belt-and-braces for paths that stop being source-relevant.
            continue;
        }
        round += 1;
        let plan = discover(source, &options.paths).map_err(WatchError::Discover)?;
        let plan = select_affected(plan, &changed);
        let report = run_round(source, &plan, options);
        sink(&WatchRound {
            index: round,
            changed,
            plan,
            report,
        });
    }
    Ok(())
}

/// Run `plan` under a fresh [`RunConfig`] built from `options` (events are
/// per-round; the cache is deliberately not carried across rounds — a
/// `--cache` watch mode is future work).
fn run_round(source: &TestKitSource, plan: &TestPlan, options: &WatchRunOptions) -> RunReport {
    let mut config = RunConfig {
        predicate: options.predicate.clone(),
        policy: options.policy,
        cache: None,
        coverage: None,
        events: Vec::new(),
    };
    run_with_config(source, plan, &mut config)
}

/// Build the watcher, debounce it, and run the watch loop.
///
/// `options.paths` and `options.watch_roots` are absolutized up front so
/// discovery module paths and watcher-reported event paths share one
/// form. An empty `watch_roots` watches [`default_watch_roots`] of the
/// discovery paths.
///
/// # Errors
///
/// [`WatchError::Watch`] when the OS watcher cannot be set up (missing
/// root, backend failure); discovery errors surface per-round as
/// [`WatchError::Discover`].
pub fn run_watch(
    source: &TestKitSource,
    options: &WatchRunOptions,
    sink: &mut dyn FnMut(&WatchRound),
) -> Result<(), WatchError> {
    let mut opts = options.clone();
    opts.paths = options.paths.iter().map(|p| absolutize(p)).collect();
    let roots = if options.watch_roots.is_empty() {
        default_watch_roots(&opts.paths)
    } else {
        let mut roots: Vec<PathBuf> = Vec::new();
        for p in &options.watch_roots {
            let r = absolutize(p);
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        roots
    };
    let watcher = Watcher::watch_many(
        &roots,
        hnl_engine_watch::WatchOptions::new().with_filter(source_filter()),
    )
    .map_err(WatchError::Watch)?;
    let window = if options.debounce.is_zero() {
        DEFAULT_DEBOUNCE
    } else {
        options.debounce
    };
    let debouncer = watcher.debounced(window);
    run_watch_loop(source, &opts, &debouncer, sink)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use handl_plugin_ext_diag::Span;

    fn case(module: &str, preamble: &str) -> TestCase {
        TestCase {
            module: PathBuf::from(module),
            name: "test_x".into(),
            body_source: "1".into(),
            preamble: preamble.into(),
            span: Span::default(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn source_path_classification() {
        assert!(is_source_path(Path::new("a/b.test.hnl")));
        assert!(is_source_path(Path::new("a/b.handl")));
        assert!(is_source_path(Path::new(".handl.toml")));
        assert!(is_source_path(Path::new("pkg/.handl.toml")));
        assert!(!is_source_path(Path::new("a/b.rs")));
        assert!(!is_source_path(Path::new("a/b.hnl.bak")));
        assert!(!is_source_path(Path::new("target/debug/x")));
    }

    #[test]
    fn relevant_dedup_filters_and_dedupes() {
        let paths = vec![
            PathBuf::from("tests/a.hnl"),
            PathBuf::from("tests/a.hnl"), // dup
            PathBuf::from("tests/b.rs"),  // not source
            PathBuf::from(".handl.toml"),
        ];
        assert_eq!(
            relevant_dedup(paths),
            vec![PathBuf::from("tests/a.hnl"), PathBuf::from(".handl.toml")]
        );
    }

    #[test]
    fn select_affected_empty_changed_returns_plan() {
        let plan = TestPlan {
            tests: vec![case("tests/a.hnl", "")],
            parse_errors: vec![(PathBuf::from("tests/b.hnl"), "boom".into())],
        };
        let out = select_affected(plan.clone(), &[]);
        assert_eq!(out.tests.len(), 1);
        assert_eq!(out.parse_errors.len(), 1);
    }

    #[test]
    fn select_affected_matches_module_and_preamble() {
        let dir = std::env::temp_dir().join(format!("hnl-watch-sel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.hnl");
        let helper = dir.join("helper.hnl");
        std::fs::write(&a, "test_a = 1").unwrap();
        std::fs::write(&helper, "x = 1").unwrap();

        // `b.hnl` is a different module that file-imports `helper.hnl`.
        let mut with_import = case(&dir.join("b.hnl").display().to_string(), "");
        with_import.preamble = format!("import \"{}\";\n", helper.display());
        let plan = TestPlan {
            tests: vec![
                case(&a.display().to_string(), ""),
                with_import,
                case(&dir.join("c.hnl").display().to_string(), ""),
            ],
            parse_errors: vec![(dir.join("bad.hnl"), "boom".into())],
        };

        // Changing the helper reruns only the importing test (module match
        // on `a` doesn't fire for `helper`; the preamble does).
        let out = select_affected(plan.clone(), std::slice::from_ref(&helper));
        assert_eq!(out.tests.len(), 1);
        assert_eq!(out.tests[0].name, "test_x");
        assert!(out.tests[0].preamble.contains("helper.hnl"));
        assert!(out.parse_errors.is_empty());

        // Changing `a` itself matches by module path.
        let out = select_affected(plan.clone(), std::slice::from_ref(&a));
        assert_eq!(out.tests.len(), 1);
        assert!(out.tests[0].preamble.is_empty());

        // An unattributed change falls back to the full plan.
        let out = select_affected(plan.clone(), &[dir.join("unrelated.hnl")]);
        assert_eq!(out.tests.len(), 3);

        std::fs::remove_dir_all(&dir).ok();
    }
}
