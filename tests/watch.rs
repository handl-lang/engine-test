//! Integration tests for `--watch` mode (`hnl_tool_watch`-backed reruns).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hnl_tool_test::runner::{TestKitSource, base_runtime};
use hnl_tool_test::watch::{
    WatchRunOptions, default_watch_roots, is_source_path, run_watch_loop, select_affected,
};
use hnl_tool_watch::Watcher;

thread_local! {
    /// Thread-cached canonical kit — same shape as `tests/runner.rs`.
    static TEST_KIT: std::cell::RefCell<Option<handl_kit_prelude::Kit>> =
        const { std::cell::RefCell::new(None) };
}

/// The canonical base-runtime factory: a capture-less closure over the
/// thread-local kit, so it is `Send + Sync`.
fn src() -> TestKitSource {
    Arc::new(|| {
        TEST_KIT.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(handl_kit_prelude::Kit::new().expect("canonical kit assembles"));
            }
            let kit = slot.as_ref().expect("kit populated above");
            Ok::<_, String>(base_runtime(
                kit.runtime(),
                kit.module_root().map(Path::to_path_buf),
            ))
        })
    })
}

/// A unique temp dir per test.
fn tmpdir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("hnl-tool-test-watch-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn default_watch_roots_dirs_and_files() {
    let dir = tmpdir("roots");
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("a.hnl");
    std::fs::write(&file, "test_a := 1").unwrap();

    let roots = default_watch_roots(&[sub.clone(), file.clone()]);
    // The directory watches itself; the file watches its parent — which is
    // the same `sub`, so the result dedupes to one root.
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], std::fs::canonicalize(&sub).unwrap());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn source_path_classification() {
    assert!(is_source_path(Path::new("x.hnl")));
    assert!(!is_source_path(Path::new("x.txt")));
}

#[test]
fn select_affected_full_plan_on_empty_change_set() {
    let dir = tmpdir("affected");
    let f = dir.join("t.hnl");
    std::fs::write(&f, "test_t := 1").unwrap();
    let plan = hnl_tool_test::discover(&src(), std::slice::from_ref(&f)).unwrap();
    assert_eq!(plan.tests.len(), 1);

    let out = select_affected(plan.clone(), &[]);
    assert_eq!(out.tests.len(), 1);

    // Unrelated source path → unattributed → full plan (safe fallback).
    let out = select_affected(plan.clone(), &[dir.join("other.hnl")]);
    assert_eq!(out.tests.len(), 1);

    // The module itself → affected subset.
    let out = select_affected(plan, &[f]);
    assert_eq!(out.tests.len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// End-to-end: initial run, then a file write triggers exactly one rerun
/// round carrying the changed path.
#[test]
fn watch_loop_reruns_on_change() {
    let dir = tmpdir("e2e");
    let test_file = dir.join("watchme.hnl");
    std::fs::write(&test_file, "test_watch := 1 + 1\n").unwrap();

    let watcher = Watcher::watch(&dir).expect("watch tempdir");
    let debouncer = watcher.debounced(Duration::from_millis(50));

    // Mutate the file shortly after the loop starts waiting.
    let writer = {
        let f = test_file.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(&f, "test_watch := 2 + 2\n").unwrap();
        })
    };

    let mut options = WatchRunOptions {
        paths: vec![dir.clone()],
        debounce: Duration::from_millis(50),
        max_rounds: Some(1),
        ..WatchRunOptions::default()
    };
    options.policy = handl_plugin_lib_scheduler::SchedulePolicy::sequential();

    let rounds = std::cell::RefCell::new(Vec::new());
    let source = src();
    run_watch_loop(&source, &options, &debouncer, &mut |round| {
        rounds
            .borrow_mut()
            .push((round.index, round.changed.clone()));
    })
    .expect("watch loop");
    writer.join().unwrap();

    let rounds = rounds.into_inner();
    assert_eq!(rounds.len(), 2, "initial run + one rerun: {rounds:?}");
    assert_eq!(rounds[0].0, 0);
    assert!(rounds[0].1.is_empty());
    assert_eq!(rounds[1].0, 1);
    assert!(
        rounds[1].1.iter().any(|p| p.ends_with("watchme.hnl")),
        "changed paths: {:?}",
        rounds[1].1
    );

    std::fs::remove_dir_all(&dir).ok();
}
