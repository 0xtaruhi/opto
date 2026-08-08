// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use smallvec::SmallVec;

const FALSE_PATH_PRIORITY: u16 = 4000;
const PATH_DELAY_PRIORITY: u16 = 3000;
const MULTICYCLE_PRIORITY: u16 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExceptionCandidate {
    pub(crate) slot: PathExceptionSlot,
    pub(crate) through_progress: u16,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedPathException<'a> {
    pub(crate) slot: PathExceptionSlot,
    pub(crate) priority: u16,
    pub(crate) exception: &'a PathException,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedMulticycle<'a> {
    arbitration: ResolvedPathException<'a>,
    pub(crate) cycles: u32,
    pub(crate) use_end_clock: bool,
}

pub(crate) fn initial_candidates(
    timing: &TimingContext,
    points: &[(TimingEndpoint, TimingEdge)],
    delay_type: DelayType,
) -> SmallVec<[ExceptionCandidate; 1]> {
    timing
        .path_exception_entries()
        .filter(|(_, exception)| {
            filter_matches_with_edge(&exception.from, exception.edges.from, points)
                && propagates_for(exception, delay_type)
        })
        .map(|(slot, _)| ExceptionCandidate {
            slot,
            through_progress: 0,
        })
        .collect()
}

pub(crate) fn advance_candidates(
    timing: &TimingContext,
    candidates: &[ExceptionCandidate],
    points: &[TimingEndpoint],
    edge: TimingEdge,
) -> Result<SmallVec<[ExceptionCandidate; 1]>, crate::TimingError> {
    candidates
        .iter()
        .map(|candidate| {
            let exception = timing.path_exception_by_slot(candidate.slot).ok_or(
                crate::TimingAnalysisError::UnknownPathException {
                    index: u32::try_from(candidate.slot.index())
                        .expect("timing constraint slots originate from nonzero u32 values"),
                },
            )?;
            let progress = usize::from(candidate.through_progress);
            let through_progress = if let Some(filter) = exception.through.get(progress)
                && filter.matches_any(points)
                && exception.edges.through[progress].matches(edge)
            {
                candidate.through_progress + 1
            } else {
                candidate.through_progress
            };
            Ok(ExceptionCandidate {
                slot: candidate.slot,
                through_progress,
            })
        })
        .collect()
}

pub(crate) fn resolve_path_exception<'a>(
    timing: &'a TimingContext,
    candidates: &[ExceptionCandidate],
    points: &[(TimingEndpoint, TimingEdge)],
    end_edge: TimingEdge,
    delay_type: DelayType,
) -> Result<Option<ResolvedPathException<'a>>, crate::TimingError> {
    let mut winner = None;
    for candidate in candidates {
        let exception = timing.path_exception_by_slot(candidate.slot).ok_or(
            crate::TimingAnalysisError::UnknownPathException {
                index: u32::try_from(candidate.slot.index())
                    .expect("timing constraint slots originate from nonzero u32 values"),
            },
        )?;
        if usize::from(candidate.through_progress) != exception.through.len()
            || !filter_matches_with_edge(&exception.to, exception.edges.to, points)
            || !exception.edges.end.matches(end_edge)
            || !applies_at_endpoint(exception, delay_type)
        {
            continue;
        }
        let candidate = ResolvedPathException {
            slot: candidate.slot,
            priority: exception_priority(exception, delay_type),
            exception,
        };
        if winner.is_none_or(|current| outranks(candidate, current, delay_type)) {
            winner = Some(candidate);
        }
    }
    Ok(winner)
}

pub(crate) fn resolve_multicycle<'a>(
    timing: &'a TimingContext,
    candidates: &[ExceptionCandidate],
    points: &[(TimingEndpoint, TimingEdge)],
    end_edge: TimingEdge,
    corner: ExceptionCorner,
) -> Result<Option<ResolvedMulticycle<'a>>, crate::TimingError> {
    let delay_type = match corner {
        ExceptionCorner::Setup => DelayType::Max,
        ExceptionCorner::Hold => DelayType::Min,
        ExceptionCorner::Both => return Ok(None),
    };
    let mut winner = None;
    for candidate in candidates {
        let exception = timing.path_exception_by_slot(candidate.slot).ok_or(
            crate::TimingAnalysisError::UnknownPathException {
                index: u32::try_from(candidate.slot.index())
                    .expect("timing constraint slots originate from nonzero u32 values"),
            },
        )?;
        let PathExceptionKind::MultiCycle {
            cycles,
            use_end_clock,
        } = exception.kind
        else {
            continue;
        };
        if !matches!(
            (corner, exception.corner),
            (
                ExceptionCorner::Setup,
                ExceptionCorner::Setup | ExceptionCorner::Both
            ) | (ExceptionCorner::Hold, ExceptionCorner::Hold)
        ) || usize::from(candidate.through_progress) != exception.through.len()
            || !filter_matches_with_edge(&exception.to, exception.edges.to, points)
            || !exception.edges.end.matches(end_edge)
        {
            continue;
        }
        let candidate = ResolvedMulticycle {
            arbitration: ResolvedPathException {
                slot: candidate.slot,
                priority: exception_priority(exception, delay_type),
                exception,
            },
            cycles,
            use_end_clock,
        };
        if winner.is_none_or(|current: ResolvedMulticycle<'_>| {
            outranks(candidate.arbitration, current.arbitration, delay_type)
        }) {
            winner = Some(candidate);
        }
    }
    Ok(winner)
}

fn filter_matches_with_edge(
    filter: &ExceptionFilter,
    selection: EdgeSelection,
    points: &[(TimingEndpoint, TimingEdge)],
) -> bool {
    points
        .iter()
        .any(|(point, edge)| selection.matches(*edge) && filter.matches_any(&[*point]))
}

fn propagates_for(exception: &PathException, delay_type: DelayType) -> bool {
    match exception.kind {
        PathExceptionKind::FalsePath => exception.corner.matches(delay_type),
        PathExceptionKind::MaxDelay { .. } => delay_type == DelayType::Max,
        PathExceptionKind::MinDelay { .. } => delay_type == DelayType::Min,
        PathExceptionKind::MultiCycle { .. } => {
            exception.corner.matches(delay_type) || delay_type == DelayType::Min
        }
    }
}

fn applies_at_endpoint(exception: &PathException, delay_type: DelayType) -> bool {
    match exception.kind {
        PathExceptionKind::FalsePath | PathExceptionKind::MultiCycle { .. } => {
            exception.corner.matches(delay_type)
        }
        PathExceptionKind::MaxDelay { .. } => delay_type == DelayType::Max,
        PathExceptionKind::MinDelay { .. } => delay_type == DelayType::Min,
    }
}

fn exception_priority(exception: &PathException, delay_type: DelayType) -> u16 {
    let type_priority = match exception.kind {
        PathExceptionKind::FalsePath => FALSE_PATH_PRIORITY,
        PathExceptionKind::MaxDelay { .. } | PathExceptionKind::MinDelay { .. } => {
            PATH_DELAY_PRIORITY
        }
        PathExceptionKind::MultiCycle { .. } => MULTICYCLE_PRIORITY,
    };
    let from_pin_or_instance = exception.from.contains_class(|point| {
        matches!(
            point,
            TimingEndpoint::Port(_) | TimingEndpoint::Cell(_) | TimingEndpoint::Pin(_)
        )
    });
    let to_pin_or_instance = exception.to.contains_class(|point| {
        matches!(
            point,
            TimingEndpoint::Port(_) | TimingEndpoint::Cell(_) | TimingEndpoint::Pin(_)
        )
    });
    let specificity = (u16::from(from_pin_or_instance) << 6)
        | (u16::from(to_pin_or_instance) << 5)
        | (u16::from(!exception.through.is_empty()) << 4)
        | (u16::from(
            exception
                .from
                .contains_class(|point| matches!(point, TimingEndpoint::Clock(_))),
        ) << 3)
        | (u16::from(
            exception
                .to
                .contains_class(|point| matches!(point, TimingEndpoint::Clock(_))),
        ) << 2);
    let corner_priority = if matches!(exception.kind, PathExceptionKind::MultiCycle { .. }) {
        match exception.corner {
            ExceptionCorner::Both => 1,
            ExceptionCorner::Setup if delay_type == DelayType::Max => 2,
            ExceptionCorner::Hold if delay_type == DelayType::Min => 2,
            ExceptionCorner::Setup | ExceptionCorner::Hold => 0,
        }
    } else {
        0
    };
    type_priority + specificity + corner_priority
}

fn outranks(
    candidate: ResolvedPathException<'_>,
    current: ResolvedPathException<'_>,
    delay_type: DelayType,
) -> bool {
    if candidate.priority != current.priority {
        return candidate.priority > current.priority;
    }
    match (&candidate.exception.kind, &current.exception.kind) {
        (
            PathExceptionKind::MaxDelay { delay: candidate },
            PathExceptionKind::MaxDelay { delay: current },
        ) => candidate < current,
        (
            PathExceptionKind::MinDelay { delay: candidate },
            PathExceptionKind::MinDelay { delay: current },
        ) => candidate > current,
        (
            PathExceptionKind::MultiCycle {
                cycles: candidate, ..
            },
            PathExceptionKind::MultiCycle {
                cycles: current, ..
            },
        ) => candidate < current,
        _ => {
            let _ = delay_type;
            candidate.slot < current.slot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_core::ObjectUid;

    fn port(raw: u64) -> PortId {
        PortId::from_uid(ObjectUid::from_raw(raw).unwrap())
    }

    fn exception(kind: PathExceptionKind, from: PortId, to: PortId) -> PathException {
        PathException {
            kind,
            from: ExceptionFilter::new([TimingEndpoint::Port(from)]),
            through: Vec::new().into_boxed_slice(),
            to: ExceptionFilter::new([TimingEndpoint::Port(to)]),
            edges: EdgeQualifier::default(),
            corner: ExceptionCorner::Setup,
            ignore_clock_latency: false,
            comment: String::new(),
        }
    }

    #[test]
    fn false_path_beats_path_delay_and_equal_delays_choose_the_tighter_value() {
        let from = port(91_001);
        let to = port(91_002);
        let points = [(TimingEndpoint::Port(from), TimingEdge::Rise)];
        let endpoint = [(TimingEndpoint::Port(to), TimingEdge::Rise)];
        let mut timing = TimingContext::new();
        timing
            .set_path_exception(exception(
                PathExceptionKind::MaxDelay { delay: 0.4 },
                from,
                to,
            ))
            .unwrap();
        timing
            .set_path_exception(exception(
                PathExceptionKind::MaxDelay { delay: 0.2 },
                from,
                to,
            ))
            .unwrap();
        let candidates = initial_candidates(&timing, &points, DelayType::Max);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Max,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            winner.exception.kind,
            PathExceptionKind::MaxDelay { delay } if delay == 0.2
        ));

        timing
            .set_path_exception(exception(PathExceptionKind::FalsePath, from, to))
            .unwrap();
        let candidates = initial_candidates(&timing, &points, DelayType::Max);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Max,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            winner.exception.kind,
            PathExceptionKind::FalsePath
        ));
        assert_eq!(winner.priority, 4000 + (1 << 6) + (1 << 5));
    }

    fn qualified(
        kind: PathExceptionKind,
        from: impl IntoIterator<Item = TimingEndpoint>,
        through: impl IntoIterator<Item = TimingEndpoint>,
        to: impl IntoIterator<Item = TimingEndpoint>,
    ) -> PathException {
        let through = through
            .into_iter()
            .map(|point| ExceptionFilter::new([point]))
            .collect::<Vec<_>>();
        let edges = EdgeQualifier::new(
            EdgeSelection::default(),
            through.iter().map(|_| EdgeSelection::default()),
            EdgeSelection::default(),
            EdgeSelection::default(),
        );
        PathException {
            kind,
            from: ExceptionFilter::new(from),
            through: through.into_boxed_slice(),
            to: ExceptionFilter::new(to),
            edges,
            corner: ExceptionCorner::Setup,
            ignore_clock_latency: false,
            comment: String::new(),
        }
    }

    fn clock(raw: u64) -> TimingEndpoint {
        TimingEndpoint::Clock(ClockId::from_uid(ObjectUid::from_raw(raw).unwrap()))
    }

    fn pin(raw: u64) -> TimingEndpoint {
        TimingEndpoint::Pin(PinId::from_uid(ObjectUid::from_raw(raw).unwrap()))
    }

    fn cell(raw: u64) -> TimingEndpoint {
        TimingEndpoint::Cell(CellId::from_uid(ObjectUid::from_raw(raw).unwrap()))
    }

    #[test]
    fn each_specificity_bit_occupies_its_verified_position() {
        let none = qualified(PathExceptionKind::FalsePath, [], [], []);
        let base = exception_priority(&none, DelayType::Max);
        assert_eq!(base, FALSE_PATH_PRIORITY);

        let cases = [
            (
                "from pin",
                qualified(PathExceptionKind::FalsePath, [pin(1)], [], []),
                1 << 6,
            ),
            (
                "from cell",
                qualified(PathExceptionKind::FalsePath, [cell(2)], [], []),
                1 << 6,
            ),
            (
                "from port",
                qualified(
                    PathExceptionKind::FalsePath,
                    [TimingEndpoint::Port(port(3))],
                    [],
                    [],
                ),
                1 << 6,
            ),
            (
                "to pin",
                qualified(PathExceptionKind::FalsePath, [], [], [pin(4)]),
                1 << 5,
            ),
            (
                "through",
                qualified(PathExceptionKind::FalsePath, [], [pin(5)], []),
                1 << 4,
            ),
            (
                "from clock",
                qualified(PathExceptionKind::FalsePath, [clock(6)], [], []),
                1 << 3,
            ),
            (
                "to clock",
                qualified(PathExceptionKind::FalsePath, [], [], [clock(7)]),
                1 << 2,
            ),
        ];
        for (label, exception, bit) in cases {
            assert_eq!(
                exception_priority(&exception, DelayType::Max),
                base + bit,
                "{label}"
            );
        }
    }

    #[test]
    fn specificity_is_a_mask_rather_than_a_field_ordering() {
        let from_pin = qualified(PathExceptionKind::FalsePath, [pin(11)], [], []);
        let every_lower_bit = qualified(
            PathExceptionKind::FalsePath,
            [clock(12)],
            [pin(13)],
            [clock(14)],
        );

        assert_eq!(
            exception_priority(&from_pin, DelayType::Max),
            FALSE_PATH_PRIORITY + (1 << 6)
        );
        assert_eq!(
            exception_priority(&every_lower_bit, DelayType::Max),
            FALSE_PATH_PRIORITY + (1 << 4) + (1 << 3) + (1 << 2)
        );
        assert!(
            exception_priority(&from_pin, DelayType::Max)
                > exception_priority(&every_lower_bit, DelayType::Max),
            "a pin-qualified -from outranks every lower bit combined"
        );
    }

    #[test]
    fn path_delay_outranks_multicycle_at_equal_specificity() {
        let from = port(93_001);
        let to = port(93_002);
        let points = [(TimingEndpoint::Port(from), TimingEdge::Rise)];
        let endpoint = [(TimingEndpoint::Port(to), TimingEdge::Rise)];
        let mut timing = TimingContext::new();
        timing
            .set_path_exception(exception(
                PathExceptionKind::MultiCycle {
                    cycles: 3,
                    use_end_clock: true,
                },
                from,
                to,
            ))
            .unwrap();
        timing
            .set_path_exception(exception(
                PathExceptionKind::MaxDelay { delay: 0.9 },
                from,
                to,
            ))
            .unwrap();

        let candidates = initial_candidates(&timing, &points, DelayType::Max);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Max,
        )
        .unwrap()
        .unwrap();

        assert!(matches!(
            winner.exception.kind,
            PathExceptionKind::MaxDelay { .. }
        ));
        assert_eq!(winner.priority, PATH_DELAY_PRIORITY + (1 << 6) + (1 << 5));
    }

    #[test]
    fn an_explicit_corner_multicycle_outranks_one_written_for_both() {
        let from = port(94_001);
        let to = port(94_002);
        let points = [(TimingEndpoint::Port(from), TimingEdge::Rise)];
        let endpoint = [(TimingEndpoint::Port(to), TimingEdge::Rise)];
        let multicycle = PathExceptionKind::MultiCycle {
            cycles: 2,
            use_end_clock: true,
        };
        let mut timing = TimingContext::new();
        timing
            .set_path_exception(PathException {
                corner: ExceptionCorner::Both,
                ..exception(multicycle.clone(), from, to)
            })
            .unwrap();
        timing
            .set_path_exception(PathException {
                corner: ExceptionCorner::Setup,
                ..exception(multicycle, from, to)
            })
            .unwrap();

        let candidates = initial_candidates(&timing, &points, DelayType::Max);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Max,
        )
        .unwrap()
        .unwrap();

        assert_eq!(winner.exception.corner, ExceptionCorner::Setup);
        assert_eq!(
            winner.priority,
            MULTICYCLE_PRIORITY + (1 << 6) + (1 << 5) + 2
        );
    }

    #[test]
    fn equal_priority_minimum_delays_choose_the_larger_value() {
        let from = port(95_001);
        let to = port(95_002);
        let points = [(TimingEndpoint::Port(from), TimingEdge::Rise)];
        let endpoint = [(TimingEndpoint::Port(to), TimingEdge::Rise)];
        let mut timing = TimingContext::new();
        for delay in [0.2, 0.5, 0.3] {
            timing
                .set_path_exception(exception(PathExceptionKind::MinDelay { delay }, from, to))
                .unwrap();
        }

        let candidates = initial_candidates(&timing, &points, DelayType::Min);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Min,
        )
        .unwrap()
        .unwrap();

        assert!(matches!(
            winner.exception.kind,
            PathExceptionKind::MinDelay { delay } if delay == 0.5
        ));
    }

    #[test]
    fn equal_priority_multicycles_choose_the_smaller_multiplier() {
        let from = port(96_001);
        let to = port(96_002);
        let points = [(TimingEndpoint::Port(from), TimingEdge::Rise)];
        let endpoint = [(TimingEndpoint::Port(to), TimingEdge::Rise)];
        let mut timing = TimingContext::new();
        for cycles in [4, 2, 3] {
            timing
                .set_path_exception(exception(
                    PathExceptionKind::MultiCycle {
                        cycles,
                        use_end_clock: true,
                    },
                    from,
                    to,
                ))
                .unwrap();
        }

        let candidates = initial_candidates(&timing, &points, DelayType::Max);
        let winner = resolve_path_exception(
            &timing,
            &candidates,
            &endpoint,
            TimingEdge::Rise,
            DelayType::Max,
        )
        .unwrap()
        .unwrap();

        assert!(matches!(
            winner.exception.kind,
            PathExceptionKind::MultiCycle { cycles: 2, .. }
        ));
    }

    #[test]
    fn reset_path_replaces_all_exception_types_on_the_same_qualified_path() {
        let from = port(92_001);
        let to = port(92_002);
        let mut timing = TimingContext::new();
        timing
            .set_path_exception(exception(
                PathExceptionKind::MaxDelay { delay: 0.4 },
                from,
                to,
            ))
            .unwrap();
        timing
            .set_path_exception_with_reset(exception(PathExceptionKind::FalsePath, from, to), true)
            .unwrap();

        let rows = timing.path_exceptions().iter().collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].kind, PathExceptionKind::FalsePath));
    }
}
