//! Reporting pipeline (ADR-0089 §"Reporting pipeline").
//!
//! Reports are produced in stages and stay **structured until the very
//! end**:
//!
//! ```text
//! Result[] → Summary → Report → Formatter
//! ```
//!
//! - [`summarize`] reduces a [`RunReport`]'s outcomes into a [`Summary`]
//!   (counts, total duration, slowest test).
//! - A [`Reporter`] renders `(&RunReport, &Summary)` into a final string.
//!   [`HumanReporter`] is the console formatter; [`JsonReporter`] is the
//!   machine formatter. Future `JunitReporter`/`TapReporter`/`CiReporter`
//!   implement the same trait — `--report json|junit` changes no
//!   execution.
//!
//! The existing [`crate::runner::format_report_human`] and
//! [`crate::runner::format_report_json`] functions are thin wrappers over
//! these reporters.

use std::time::Duration;

use crate::runner::{RunReport, TestResult};

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------

/// A reduced summary of a [`RunReport`] (ADR-0089 `TestSummary`).
///
/// Computed by [`summarize`]; carries counts, aggregate duration, and the
/// slowest executed test. Skipped and cache-replayed tests contribute zero
/// duration.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Total number of tests in the report.
    pub total: usize,
    /// Number of passed tests.
    pub passed: usize,
    /// Number of failed tests.
    pub failed: usize,
    /// Number of skipped tests.
    pub skipped: usize,
    /// Sum of per-test execution durations.
    pub total_duration: Duration,
    /// The slowest executed test: `(name, duration)`. `None` when no test
    /// executed (all skipped or the plan is empty).
    pub slowest: Option<(String, Duration)>,
}

/// Reduce a [`RunReport`]'s outcomes into a [`Summary`].
///
/// This is the `Result[] → Summary` stage of the reporting pipeline.
pub fn summarize(report: &RunReport) -> Summary {
    let mut total_duration = Duration::ZERO;
    let mut slowest: Option<(String, Duration)> = None;
    for outcome in &report.outcomes {
        total_duration += outcome.duration;
        if outcome.duration > Duration::ZERO
            && slowest
                .as_ref()
                .map(|(_, d)| outcome.duration > *d)
                .unwrap_or(true)
        {
            slowest = Some((outcome.case.name.clone(), outcome.duration));
        }
    }
    Summary {
        total: report.total(),
        passed: report.passed(),
        failed: report.failed(),
        skipped: report.skipped(),
        total_duration,
        slowest,
    }
}

// ---------------------------------------------------------------------------
// Reporter trait and implementations
// ---------------------------------------------------------------------------

/// A report formatter (ADR-0089 `Reporter` API v1).
///
/// The reporter **never executes** — it consumes a finished [`RunReport`]
/// plus its [`Summary`] and renders a string.
pub trait Reporter {
    /// A short reporter name, for `--report <name>` selection.
    fn name(&self) -> &'static str;

    /// Render `report` and its `summary` into a final string.
    fn format(&self, report: &RunReport, summary: &Summary) -> String;
}

/// Human-readable console reporter: one `PASS`/`FAIL`/`SKIP` line per
/// test grouped under a `== <suite> ==` header per module (the suite
/// boundary — see [`crate::events::TestEvent::SuiteStarted`]), then a
/// summary line. Failures print their populated
/// [`crate::runner::FailureDetail`] fields (`assertion`, `expected`,
/// `actual`, `diff`, `cause`) on indented detail lines.
#[derive(Debug, Default, Clone, Copy)]
pub struct HumanReporter;

impl Reporter for HumanReporter {
    fn name(&self) -> &'static str {
        "console"
    }

    fn format(&self, report: &RunReport, _summary: &Summary) -> String {
        let mut out = String::new();
        let mut current_suite = String::new();

        for outcome in &report.outcomes {
            let module = outcome.case.module.display().to_string();
            // Suite boundary: outcomes are in canonical plan order, so a
            // module change opens a new suite group.
            if current_suite != module {
                if !current_suite.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("== {module} ==\n"));
                current_suite = module.clone();
            }
            match &outcome.result {
                TestResult::Pass => {
                    out.push_str(&format!("PASS  {} ({})\n", outcome.case.name, module));
                }
                TestResult::Fail { message } => {
                    out.push_str(&format!(
                        "FAIL  {} ({}): {}\n",
                        outcome.case.name, module, message
                    ));
                    // Structured failure detail (ADR-0089 §"Structured
                    // failures"): emitted only when populated.
                    let detail = &outcome.failure;
                    for (key, value) in [
                        ("assertion", &detail.assertion),
                        ("expected", &detail.expected),
                        ("actual", &detail.actual),
                        ("diff", &detail.diff),
                        ("cause", &detail.cause),
                    ] {
                        if let Some(v) = value {
                            out.push_str(&format!("      {key}: {v}\n"));
                        }
                    }
                }
                TestResult::Skip => {
                    out.push_str(&format!("SKIP  {} ({})\n", outcome.case.name, module));
                }
            }
        }

        // Summary line.
        out.push_str(&format!(
            "\n---\n{} tests: {} passed, {} failed, {} skipped\n",
            report.total(),
            report.passed(),
            report.failed(),
            report.skipped()
        ));

        out
    }
}

/// Machine-readable JSON reporter for CI consumers.
///
/// Emits a `suites` array (per-module grouping mirroring
/// `SuiteStarted`/`SuiteFinished`, with per-suite counts), the same
/// per-test information as [`HumanReporter`], plus the structured fields:
/// per-test `duration_ms` and, for failures, any populated
/// [`crate::runner::FailureDetail`] fields (`expected`, `actual`, `diff`,
/// `cause`, `assertion`) — no information is lost crossing the report
/// boundary (ADR-0089 §"Structured failures").
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn format(&self, report: &RunReport, summary: &Summary) -> String {
        let tests: Vec<serde_json::Value> = report
            .outcomes
            .iter()
            .map(|outcome| {
                let status = match &outcome.result {
                    TestResult::Pass => "pass",
                    TestResult::Fail { .. } => "fail",
                    TestResult::Skip => "skip",
                };
                let mut entry = serde_json::json!({
                    "name": outcome.case.name,
                    "module": outcome.case.module.display().to_string(),
                    "status": status,
                    "duration_ms": outcome.duration.as_secs_f64() * 1000.0,
                });
                if let TestResult::Fail { message } = &outcome.result {
                    entry["message"] = serde_json::Value::String(message.clone());
                }
                // Structured failure fields (ADR-0089). Only emitted when
                // populated — the pure-handl `assert.*` helpers
                // (`handl-packages/test/assert.hnl`) and the engine's
                // `assert` keyword both carry them.
                let detail = &outcome.failure;
                for (key, value) in [
                    ("assertion", &detail.assertion),
                    ("expected", &detail.expected),
                    ("actual", &detail.actual),
                    ("diff", &detail.diff),
                    ("cause", &detail.cause),
                ] {
                    if let Some(v) = value {
                        entry[key] = serde_json::Value::String(v.clone());
                    }
                }
                entry
            })
            .collect();

        // Suite grouping (ADR-0089 `SuiteStarted`/`SuiteFinished`): the
        // suite boundary is the module — outcomes are in canonical plan
        // order, so contiguous runs of equal `module` form one suite.
        let mut suites: Vec<serde_json::Value> = Vec::new();
        for outcome in &report.outcomes {
            let module = outcome.case.module.display().to_string();
            let status = match &outcome.result {
                TestResult::Pass => "passed",
                TestResult::Fail { .. } => "failed",
                TestResult::Skip => "skipped",
            };
            let needs_new = suites.last().and_then(|s| s["name"].as_str()) != Some(module.as_str());
            if needs_new {
                suites.push(serde_json::json!({
                    "name": module,
                    "total": 0,
                    "passed": 0,
                    "failed": 0,
                    "skipped": 0,
                }));
            }
            let suite = suites.last_mut().expect("suite pushed above");
            suite["total"] = serde_json::json!(suite["total"].as_u64().unwrap_or(0) + 1);
            suite[status] = serde_json::json!(suite[status].as_u64().unwrap_or(0) + 1);
        }

        let mut summary_json = serde_json::json!({
            "total": summary.total,
            "passed": summary.passed,
            "failed": summary.failed,
            "skipped": summary.skipped,
            "duration_ms": summary.total_duration.as_secs_f64() * 1000.0,
        });
        if let Some((name, d)) = &summary.slowest {
            summary_json["slowest"] = serde_json::json!({
                "name": name,
                "duration_ms": d.as_secs_f64() * 1000.0,
            });
        }

        serde_json::json!({
            "suites": suites,
            "tests": tests,
            "summary": summary_json,
        })
        .to_string()
    }
}

// ---------------------------------------------------------------------------
// Coverage reporting (`--coverage`)
// ---------------------------------------------------------------------------

/// Human-readable coverage reporter — the sibling of [`Reporter`] for
/// [`hnl_exec::coverage::CoverageReport`] (which is produced by the
/// eval instrumentation, not by a [`RunReport`]).
///
/// Renders per-source line and declaration coverage. **Line numbers are
/// prepared-source coordinates** — `.hnl` modules are re-serialized by
/// module prep before parsing, so they do not map faithfully to file
/// lines (see `hnl_exec::coverage` module docs). When the producer
/// attached a provenance map, a resolved *file-line* summary is shown as
/// well; `origin` shows the on-disk path for reference.
#[derive(Debug, Default, Clone, Copy)]
pub struct CoverageReporter;

impl CoverageReporter {
    /// Render `report` as human-readable text.
    pub fn format(&self, report: &hnl_exec::coverage::CoverageReport) -> String {
        let mut out = String::new();
        out.push_str(
            "coverage (line numbers are prepared-source coordinates;\n\
             file-line summaries appear when the producer attached a\n\
             provenance map):\n",
        );
        if report.sources.is_empty() {
            out.push_str("  (no sources recorded)\n");
            return out;
        }
        for src in &report.sources {
            let pct = if src.total_lines > 0 {
                src.lines_covered() as f64 * 100.0 / src.total_lines as f64
            } else {
                0.0
            };
            out.push_str(&format!(
                "\n== {} ==\n  lines: {} / {} ({:.1}%)\n",
                src.name,
                src.lines_covered(),
                src.total_lines,
                pct
            ));
            if let Some(origin) = &src.origin {
                out.push_str(&format!("  origin: {origin}\n"));
            }
            // Resolved file-line coverage: hits merged through the
            // provenance map. The denominator is the set of file lines
            // the map can attribute (distinct origin lines), not the
            // file's total — comments/blank lines are never coverable.
            if let Some(file_hits) = &src.file_line_hits {
                let mapped: std::collections::BTreeSet<u32> = src
                    .line_map
                    .iter()
                    .flatten()
                    .flatten()
                    .copied()
                    .collect();
                let file_pct = if mapped.is_empty() {
                    0.0
                } else {
                    file_hits.len() as f64 * 100.0 / mapped.len() as f64
                };
                out.push_str(&format!(
                    "  file lines: {} / {} ({:.1}%)\n",
                    file_hits.len(),
                    mapped.len(),
                    file_pct
                ));
                if let Some(origin_lines) = src.origin_lines {
                    out.push_str(&format!("  origin file: {origin_lines} lines\n"));
                }
            }
            let decls_covered = src.decls.iter().filter(|d| d.covered()).count();
            out.push_str(&format!(
                "  declarations: {} / {} covered ({} arrow applications)\n",
                decls_covered,
                src.decls.len(),
                src.applications
            ));
            for d in &src.decls {
                let mark = if d.covered() { "x" } else { " " };
                let kind = if d.is_arrow { "arrow" } else { "decl" };
                let file = d
                    .file_line
                    .map(|l| format!(" file:{l}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "    [{}] {:<4} {}{} (any_hits={}, body_hits={})\n",
                    mark, kind, d.name, file, d.any_hits, d.body_hits
                ));
            }
        }
        out
    }
}
