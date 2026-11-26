//! Run configuration: the planner/policy seam (ADR-0089 §"Planner +
//! Scheduler").
//!
//! [`RunConfig`] bundles everything the run stage needs — the selection
//! [`Filter`], the [`SchedulePolicy`] for the generic
//! `BoundedScheduler`, an optional [`TestRunCache`] (`--cache`), and an
//! event sink — so the `hnl test --filter/--jobs/--cache` flags map onto a
//! single entry point, [`run_with_config`].
//!
//! The existing [`crate::runner::run`], [`crate::runner::run_with_filter`],
//! [`crate::runner::run_selected`], and
//! [`crate::runner::run_selected_cached`] functions are thin wrappers over
//! this entry point.

use std::time::Duration;

use handl_plugin_lib_scheduler::SchedulePolicy;

use crate::events::TestEvent;
use crate::runner::{
    self, Filter, RunReport, TestKitSource, TestPlan, TestRunCache, filter_predicate, tag_predicate,
};

/// Configuration for a single test run.
///
/// Fields are public for composition; [`RunConfig::default`] runs
/// everything sequentially with no cache and collects no events.
///
/// `events` is an output sink: [`run_with_config`] appends every
/// [`TestEvent`] to it (in canonical plan order — see
/// [`crate::events`]). Clear it between runs if the config is reused.
#[derive(Default)]
pub struct RunConfig {
    /// Selection filter (`--filter`/`--tag`). [`Filter::all`]
    /// selects all tests.
    pub predicate: Filter,
    /// Scheduler policy (`--jobs`). Defaults to available parallelism via
    /// [`SchedulePolicy::default`].
    pub policy: SchedulePolicy,
    /// Opt-in result cache (`--cache`). When `Some`, selected tests hit or
    /// populate it; when `None`, nothing is cached.
    pub cache: Option<TestRunCache>,
    /// Opt-in `.hnl` source coverage (`--coverage`). When `Some`, the
    /// session is installed on each scheduler worker and collects
    /// line/decl hits; read the merged [`hnl_exec::coverage::CoverageReport`]
    /// via `config.coverage.as_ref().map(|s| s.report())` after the run.
    pub coverage: Option<hnl_exec::coverage::CoverageSession>,
    /// Opt-in per-test timeout. When `Some`, each selected test's
    /// evaluation is bounded; a test that exceeds the limit is reported
    /// as [`crate::runner::TestResult::Timeout`] (counted as a failure)
    /// and the run continues with the remaining tests.
    ///
    /// **Default: `None` (no timeout).** Opt-in like `--cache`/`--coverage`:
    /// a wrong default kills legitimate slow tests, which is worse than a
    /// hang a patient user can interrupt. Enforcement cannot kill the
    /// Rust thread running the eval — the timed-out evaluation is
    /// abandoned and its thread leaks until process exit (bounded by the
    /// number of timed-out tests; see `runner::eval_with_timeout`).
    pub timeout: Option<Duration>,
    /// Structured events emitted during the run (output sink).
    pub events: Vec<TestEvent>,
}

impl RunConfig {
    /// A config that selects all tests and runs sequentially.
    pub fn new() -> Self {
        Self {
            policy: SchedulePolicy::sequential(),
            ..Self::default()
        }
    }

    /// Set the selection predicate from a filter pattern
    /// (substring or glob — see [`filter_predicate`]).
    pub fn with_filter(mut self, pattern: &str) -> Self {
        self.predicate = filter_predicate(pattern);
        self
    }

    /// Add a tag predicate to the selection (AND with the existing predicate).
    ///
    /// Tags are discovered from `-- tags: ...` doc-comment lines above a
    /// test declaration. Repeated calls are combined with AND.
    pub fn with_tag(mut self, tag: &str) -> Self {
        self.predicate = self.predicate.and(tag_predicate(tag));
        self
    }

    /// Set the scheduler policy (`--jobs`).
    pub fn with_policy(mut self, policy: SchedulePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Enable result caching with the given cache (`--cache`).
    pub fn with_cache(mut self, cache: TestRunCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Enable `.hnl` source coverage with the given session (`--coverage`).
    pub fn with_coverage(mut self, session: hnl_exec::coverage::CoverageSession) -> Self {
        self.coverage = Some(session);
        self
    }

    /// Bound each test's evaluation to `timeout` (`--timeout`).
    ///
    /// Tests exceeding the limit report [`crate::runner::TestResult::Timeout`]
    /// and count as failures; the run continues. See
    /// [`RunConfig::timeout`] for the leak-on-timeout caveat.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// Run a test plan under a [`RunConfig`].
///
/// This is the single run entry point: it applies `config.predicate`,
/// dispatches selected tests under `config.policy`, consults/populates
/// `config.cache` when present, and appends structured events to
/// `config.events` in canonical plan order (deterministic regardless of
/// parallelism — ADR-0089 §"Determinism").
///
/// `source` is the injected base-runtime factory — invoked on each
/// scheduler worker thread (see [`TestKitSource`]).
pub fn run_with_config(
    source: &TestKitSource,
    plan: &TestPlan,
    config: &mut RunConfig,
) -> RunReport {
    runner::run_selected_impl(
        source,
        plan,
        &config.predicate,
        &config.policy,
        config.timeout,
        config.cache.as_mut(),
        config.coverage.as_ref(),
        Some(&mut config.events),
    )
}
