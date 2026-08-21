// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::indexed::range_tasks;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn runtime(threads: usize) -> ExecutionContext {
    ExecutionContext::new(&ExecutionConfig {
        max_threads: threads,
    })
    .unwrap()
}

#[test]
fn rejects_zero_workers() {
    assert!(matches!(
        ExecutionContext::new(&ExecutionConfig { max_threads: 0 }),
        Err(RuntimeError::NoWorkerThreads)
    ));
}

#[test]
fn ordered_tasks_are_deterministic_across_worker_counts() {
    let run = |threads| {
        runtime(threads)
            .map_ordered(
                (0..257)
                    .rev()
                    .map(|index| Task::new(TaskKey::new(7, index), index))
                    .collect(),
                |value| Ok::<_, RuntimeError>(value * value),
            )
            .unwrap()
    };
    assert_eq!(run(1), run(4));
}

#[test]
fn indexed_analysis_preserves_dense_order() {
    let expected = runtime(1)
        .analyze_indexed(1024, |index| Ok::<_, RuntimeError>(index.rotate_left(3)))
        .unwrap();
    let actual = runtime(4)
        .analyze_indexed(1024, |index| Ok::<_, RuntimeError>(index.rotate_left(3)))
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn explicit_indexed_grain_changes_only_scheduling() {
    let run = |minimum_grain| {
        let calls = AtomicUsize::new(0);
        let results = runtime(4)
            .analyze_indexed_with_grain(257, minimum_grain, |index| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, RuntimeError>(index.rotate_left(3))
            })
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 257);
        results
    };

    assert_eq!(
        run(std::num::NonZeroUsize::MIN),
        run(std::num::NonZeroUsize::new(17).unwrap())
    );
}

#[test]
fn composite_scheduler_elastically_shares_one_pool_and_preserves_order() {
    let runtime = runtime(16);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let initial_wave = Barrier::new(15);
    let outputs = runtime
        .map_ordered_composite(
            (0_u64..40)
                .rev()
                .map(|index| {
                    Task::new(TaskKey::new(11, index), index)
                        .with_estimated_work(index.saturating_add(1))
                })
                .collect(),
            |index, nested| {
                assert!(runtime.is_same_runtime(nested));
                assert_eq!(nested.parallelism(), 16);
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                if index >= 25 {
                    initial_wave.wait();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, RuntimeError>(index)
            },
        )
        .unwrap();

    assert_eq!(outputs, (0_u64..40).collect::<Vec<_>>());
    assert!(peak.load(Ordering::SeqCst) >= 15);
}

#[test]
fn composite_scheduler_admits_tasks_within_the_memory_limit() {
    let runtime = runtime(4).with_memory_limit(std::num::NonZeroU64::new(10).unwrap());
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let wave = Barrier::new(2);
    let outputs = runtime
        .map_ordered_composite(
            (0_u64..4)
                .map(|index| Task::new(TaskKey::new(12, index), index).with_estimated_memory(5))
                .collect(),
            |index, _| {
                let now = active.fetch_add(5, Ordering::SeqCst) + 5;
                peak.fetch_max(now, Ordering::SeqCst);
                wave.wait();
                active.fetch_sub(5, Ordering::SeqCst);
                Ok::<_, RuntimeError>(index)
            },
        )
        .unwrap();

    assert_eq!(outputs, (0_u64..4).collect::<Vec<_>>());
    assert_eq!(peak.load(Ordering::SeqCst), 10);
}

#[test]
fn composite_scheduler_rejects_an_oversized_task_before_execution() {
    let runtime = runtime(2).with_memory_limit(std::num::NonZeroU64::new(4).unwrap());
    let error = runtime
        .map_ordered_composite(
            vec![Task::new(TaskKey::new(12, 0), ()).with_estimated_memory(5)],
            |(), _| Ok::<_, RuntimeError>(()),
        )
        .unwrap_err();

    assert!(matches!(error, RuntimeError::TaskMemoryExceedsLimit { .. }));
    assert_eq!(runtime.metrics().completed_task_callbacks, 0);
}

#[test]
fn comparator_sort_is_deterministic_across_worker_counts() {
    let input = (0_u32..4096)
        .rev()
        .map(|value| (value % 31, value))
        .collect::<Vec<_>>();
    let sort = |threads| {
        let mut values = input.clone();
        runtime(threads).sort_unstable_by(&mut values, |left, right| {
            left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1))
        });
        values
    };

    assert_eq!(sort(1), sort(4));
}

#[test]
fn cancellation_stops_new_work() {
    let runtime = runtime(2);
    runtime.cancel();
    assert!(matches!(
        runtime.analyze_indexed(8, Ok::<_, RuntimeError>),
        Err(RuntimeError::Cancelled)
    ));
}

#[test]
fn range_tasks_cover_input_in_stable_grains() {
    let ranges = runtime(4)
        .map_ordered(range_tasks(9, 10, 4), Ok::<_, RuntimeError>)
        .unwrap();

    assert_eq!(ranges, [0..4, 4..8, 8..10]);
    assert!(range_tasks(9, 0, 4).is_empty());
}

#[test]
fn nested_tasks_receive_a_serial_view_of_the_shared_runtime() {
    let runtime = runtime(4);
    let results = runtime
        .map_ordered_nested(
            (0..4)
                .map(|ordinal| {
                    Task::new(
                        TaskKey::new(12, ordinal),
                        usize::try_from(ordinal).expect("test ordinal fits usize"),
                    )
                })
                .collect(),
            |base, nested| {
                assert!(runtime.is_same_runtime(nested));
                assert_eq!(nested.parallelism(), 1);
                nested
                    .analyze_indexed(4, |index| Ok::<_, RuntimeError>(base + index))
                    .map(|values| values.into_iter().sum::<usize>())
            },
        )
        .unwrap();

    assert_eq!(results, [6, 10, 14, 18]);
}

#[test]
fn limited_handle_shares_runtime_and_executes_serially() {
    let runtime = runtime(4);
    let limited = runtime.with_parallelism_limit(std::num::NonZeroUsize::MIN);
    assert!(runtime.is_same_runtime(&limited));
    assert_eq!(limited.parallelism(), 1);

    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    limited
        .map_ordered(
            (0..8)
                .map(|ordinal| Task::new(TaskKey::new(11, ordinal), ()))
                .collect(),
            |()| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, RuntimeError>(())
            },
        )
        .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

#[test]
fn indexed_commit_is_ordered_and_failed_commit_is_not_counted() {
    let runtime = runtime(4);
    let mut committed = Vec::new();
    runtime
        .commit_indexed(
            64,
            |index| {
                std::thread::sleep(Duration::from_micros((63 - index) as u64));
                Ok::<_, RuntimeError>(index)
            },
            |index, value| {
                assert_eq!(index, value);
                committed.push(value);
                Ok::<_, RuntimeError>(())
            },
        )
        .unwrap();
    assert_eq!(committed, (0..64).collect::<Vec<_>>());
    assert_eq!(runtime.metrics().completed_batches, 1);

    let error = runtime
        .commit_indexed(2, Ok::<_, RuntimeError>, |index, _| {
            if index == 1 {
                Err(RuntimeError::InvalidDependencyPlan {
                    detail: "injected commit failure",
                })
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidDependencyPlan { .. }));
    assert_eq!(runtime.metrics().completed_batches, 1);
}

#[test]
fn indexed_worker_count_matches_the_execution_threshold() {
    assert_eq!(indexed_worker_count(0, 4), 0);
    assert_eq!(indexed_worker_count(63, 4), 1);
    assert_eq!(indexed_worker_count(64, 4), 2);
    assert_eq!(indexed_worker_count(127, 8), 4);
    assert_eq!(indexed_worker_count(256, 32), 8);
    assert_eq!(indexed_worker_count(1_024, 32), 32);
    assert_eq!(indexed_worker_count(64, 1), 1);
}

fn dependency_plan() -> DependencyPlan {
    let predecessors = [vec![], vec![], vec![0], vec![0, 1], vec![2, 3]];
    DependencyPlan::from_topological_order(5, &[0, 1, 2, 3, 4], |item| {
        predecessors[item].iter().copied()
    })
    .unwrap()
}

#[test]
fn dependency_worklist_releases_only_ready_items() {
    let plan = dependency_plan();
    let mut ready = plan.worklist(DependencyDirection::Forward, 0..5).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), Some(vec![0, 1]));
    ready.finish(0).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), Some(vec![2]));
    ready.finish(2).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), Some(vec![]));
    ready.finish(1).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), Some(vec![3]));
    ready.finish(3).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), Some(vec![4]));
    ready.finish(4).unwrap();
    assert_eq!(ready.claim_ready().unwrap(), None);
}

/// A row count the plan cannot represent is a structured error, not an abort.
///
/// The edge iterator is empty, so nothing here needs a large allocation; the
/// point is that the count is rejected before anything is sized from it.
#[test]
fn dependency_publication_reports_an_unrepresentable_row_count() {
    let error = DependencyPublicationPlan::sparse(0, usize::MAX, []).unwrap_err();
    assert!(
        matches!(error, RuntimeError::InvalidDependencyPlan { .. }),
        "unexpected error {error:?}"
    );
}

#[test]
fn dependency_publication_rejects_conflicts_and_rolls_back() {
    assert!(DependencyPublicationPlan::sparse(2, 1, [(0, 0), (1, 0)]).is_err());

    let dependency = DependencyPlan::from_topological_order(2, &[0, 1], |_| []).unwrap();
    let worklist = dependency
        .worklist(DependencyDirection::Forward, 0..2)
        .unwrap();
    let publication = DependencyPublicationPlan::identity(2);
    let mut rows = [7usize, 8];
    let error = runtime(1)
        .publish_dependency_rows(
            worklist,
            &mut rows,
            DependencyRun::new(&publication, DependencyActivation::all()),
            |_, item| Ok::<_, RuntimeError>(item),
            |item| Ok(DependencyPublication::row(usize::from(item == 0), item)),
        )
        .unwrap_err();

    assert!(matches!(error, RuntimeError::InvalidDependencyPlan { .. }));
    assert_eq!(rows, [7, 8]);
}

#[test]
fn dependency_executor_releases_successors_on_physical_completion() {
    let plan = DependencyPlan::from_topological_order(3, &[0, 1, 2], |item| match item {
        2 => vec![0],
        _ => Vec::new(),
    })
    .unwrap();
    let worklist = plan.worklist(DependencyDirection::Forward, 0..3).unwrap();
    let successor_started = Arc::new(AtomicBool::new(false));
    let observe = Arc::clone(&successor_started);
    let mut committed = vec![false; 3];
    let publication = DependencyPublicationPlan::identity(3);
    runtime(2)
        .publish_dependency_rows(
            worklist,
            &mut committed,
            DependencyRun::new(&publication, DependencyActivation::all()),
            |_, item| Ok::<_, RuntimeError>(item),
            move |item| {
                if item == 1 {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !observe.load(Ordering::Acquire) {
                        if Instant::now() >= deadline {
                            return Err(RuntimeError::InvalidDependencyPlan {
                                detail: "successor was held behind a completion wave",
                            });
                        }
                        std::thread::yield_now();
                    }
                } else if item == 2 {
                    successor_started.store(true, Ordering::Release);
                }
                Ok(DependencyPublication::row(item, true))
            },
        )
        .unwrap();
    assert_eq!(committed, vec![true; 3]);
}

#[test]
fn dependency_effects_ignore_worker_count_and_completion_order() {
    let run = |threads: usize, reverse: bool| {
        let plan = DependencyPlan::from_topological_order(4, &[0, 1, 2, 3], |_| []).unwrap();
        let worklist = plan.worklist(DependencyDirection::Forward, 0..4).unwrap();
        let publication = DependencyPublicationPlan::identity(4);
        let mut rows = vec![10usize, 11, 12, 13];
        let mut effects = DependencyEffects::new();
        runtime(threads)
            .publish_dependency_rows(
                worklist,
                &mut rows,
                DependencyRun::new(&publication, DependencyActivation::all())
                    .record_effects(&mut effects),
                |_, item| Ok::<_, RuntimeError>(item),
                |item| {
                    let delay = if reverse { item } else { 3 - item };
                    std::thread::sleep(Duration::from_millis(delay as u64));
                    Ok(DependencyPublication::row(item, item))
                },
            )
            .unwrap();
        let reduced =
            effects
                .into_entries()
                .fold(String::new(), |mut text, (item, row, previous)| {
                    use std::fmt::Write;
                    write!(text, "{item}:{row}:{previous};").unwrap();
                    text
                });
        (rows, reduced)
    };

    let serial = run(1, false);
    assert_eq!(serial, run(4, false));
    assert_eq!(serial, run(4, true));
}

#[test]
fn dependency_cancellation_drains_workers_and_rolls_back_rows() {
    let dependency = DependencyPlan::from_topological_order(3, &[0, 1, 2], |_| []).unwrap();
    let worklist = dependency
        .worklist(DependencyDirection::Forward, 0..3)
        .unwrap();
    let publication = DependencyPublicationPlan::identity(3);
    let runtime = runtime(3);
    let control = runtime.clone();
    let drained = Arc::new(AtomicBool::new(false));
    let observe = Arc::clone(&drained);
    let mut rows = [10usize, 11, 12];
    let error = runtime
        .publish_dependency_rows(
            worklist,
            &mut rows,
            DependencyRun::new(&publication, DependencyActivation::all()),
            |_, item| Ok::<_, RuntimeError>(item),
            move |item| {
                match item {
                    0 => {}
                    1 => {
                        std::thread::sleep(Duration::from_millis(5));
                        control.cancel();
                    }
                    2 => {
                        std::thread::sleep(Duration::from_millis(10));
                        observe.store(true, Ordering::Release);
                    }
                    _ => unreachable!(),
                }
                Ok(DependencyPublication::row(item, 99))
            },
        )
        .unwrap_err();

    assert!(matches!(error, RuntimeError::Cancelled));
    assert!(drained.load(Ordering::Acquire));
    assert_eq!(rows, [10, 11, 12]);
}

#[test]
fn duplicate_ordered_keys_are_rejected_before_execution() {
    let runtime = runtime(2);
    let key = TaskKey::new(1, 0);
    let error = runtime
        .map_ordered(vec![Task::new(key, ()), Task::new(key, ())], |()| {
            Ok::<_, RuntimeError>(())
        })
        .unwrap_err();

    assert!(error.to_string().contains("duplicate execution task key"));
    assert_eq!(runtime.metrics().completed_task_callbacks, 0);
}
