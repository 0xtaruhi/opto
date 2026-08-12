// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Explicit typed fixtures shared by timing-domain unit tests.

use super::*;

pub(crate) fn test_object_uid(raw: u64) -> opto_db::ObjectUid {
    opto_db::ObjectUid::from_raw(raw).unwrap()
}

pub(crate) fn test_design_id() -> DesignId {
    DesignId::from_uid(test_object_uid(1))
}

pub(crate) fn test_port_id(name: &str) -> PortId {
    let raw = name.bytes().fold(17u64, |hash, byte| {
        hash.wrapping_mul(257).wrapping_add(u64::from(byte))
    });
    PortId::from_uid(test_object_uid(raw.max(2)))
}

pub(crate) fn test_clock_id(raw: u64) -> ClockId {
    ClockId::from_uid(test_object_uid(raw))
}

pub(crate) fn test_library_units() -> TimingLibraryUnits {
    TimingLibraryUnits {
        time_seconds: Some(1e-12),
        capacitance_farads: Some(1e-15),
        resistance_ohms: None,
    }
}

/// Builds one top-level timing port bound to a net of the same name.
pub(crate) fn test_port(name: &str, direction: TimingPortDirection) -> TimingPort {
    TimingPort {
        id: test_port_id(name),
        name: name.to_string(),
        net: TimingNet::named(name),
        direction,
    }
}

pub(crate) fn test_timing_model(design: &TimingDesign, library: &TimingLibrary) -> TimingModel {
    TimingModel::new(design.clone(), library.clone()).unwrap()
}

pub(crate) fn test_analyze_timing(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> TimingAnalysis {
    TimingEngine::analyze_once(timing, model, options).unwrap()
}

pub(crate) fn assert_path_summary(
    analysis: &TimingAnalysis,
    startpoint: &str,
    endpoint: &str,
    arrival: f64,
    required: f64,
    slack: f64,
) {
    assert_eq!(analysis.startpoint(), startpoint);
    assert_eq!(analysis.endpoint(), endpoint);
    assert!((analysis.arrival() - arrival).abs() < 1e-12);
    assert!((analysis.required().unwrap() - required).abs() < 1e-12);
    assert!((analysis.slack().unwrap() - slack).abs() < 1e-12);
}

pub(crate) mod test_library {
    use super::*;

    pub(crate) struct TimingArc {
        pub(crate) from_pin: String,
        pub(crate) to_pin: String,
        pub(crate) timing_sense: TimingSense,
        pub(crate) cell_rise: Option<LookupTable>,
        pub(crate) cell_fall: Option<LookupTable>,
        pub(crate) rise_transition: Option<LookupTable>,
        pub(crate) fall_transition: Option<LookupTable>,
    }

    impl TimingArc {
        pub(crate) fn scalar(
            from_pin: impl Into<String>,
            to_pin: impl Into<String>,
            delay: f64,
        ) -> Self {
            Self {
                from_pin: from_pin.into(),
                to_pin: to_pin.into(),
                timing_sense: TimingSense::PositiveUnate,
                cell_rise: Some(LookupTable::scalar(delay)),
                cell_fall: Some(LookupTable::scalar(delay)),
                rise_transition: None,
                fall_transition: None,
            }
        }
    }

    pub(crate) struct ClockToQArc {
        pub(crate) clock_edge: TimingEdge,
        pub(crate) arc: TimingArc,
    }

    pub(crate) struct TimingConstraintArc {
        pub(crate) data_pin: String,
        pub(crate) clock_pin: String,
        pub(crate) clock_edge: TimingEdge,
        pub(crate) kind: TimingCheckKind,
        pub(crate) rise_constraint: Option<LookupTable>,
        pub(crate) fall_constraint: Option<LookupTable>,
    }

    #[derive(Default)]
    pub(crate) struct TimingCell {
        pub(crate) name: String,
        pub(crate) arcs: Vec<TimingArc>,
        pub(crate) clock_to_q: Vec<ClockToQArc>,
        pub(crate) constraints: Vec<TimingConstraintArc>,
        pub(crate) pin_capacitance: BTreeMap<String, f64>,
    }

    pub(crate) fn test_instance<const N: usize>(
        id: u32,
        name: &str,
        cell: &str,
        connections: [(&str, &str); N],
    ) -> TimingInstance {
        TimingInstance {
            id: TimingInstanceId::from_raw(id),
            name: name.to_string(),
            cell: cell.to_string(),
            connections: connections
                .into_iter()
                .map(|(pin, net)| TimingConnection {
                    pin: pin.to_string(),
                    net: net.to_string(),
                })
                .collect(),
        }
    }

    pub(crate) fn test_cells(cells: Vec<TimingCell>) -> TargetCellSet {
        test_target_cells(cells).into()
    }

    pub(crate) fn test_target_cells(cells: Vec<TimingCell>) -> Vec<TargetCell> {
        cells.into_iter().map(canonical_cell).collect()
    }

    fn canonical_cell(cell: TimingCell) -> TargetCell {
        let mut pins = BTreeMap::<String, TargetPin>::new();
        for (name, capacitance) in cell.pin_capacitance {
            ensure_pin(&mut pins, &name, TargetPinDirection::Input).capacitance = Some(capacitance);
        }
        for arc in cell.arcs {
            ensure_pin(&mut pins, &arc.from_pin, TargetPinDirection::Input);
            ensure_pin(&mut pins, &arc.to_pin, TargetPinDirection::Output)
                .timing_arcs
                .push(canonical_arc(arc, TargetTimingType::Combinational));
        }
        for clock_to_q in cell.clock_to_q {
            ensure_pin(
                &mut pins,
                &clock_to_q.arc.from_pin,
                TargetPinDirection::Input,
            );
            ensure_pin(
                &mut pins,
                &clock_to_q.arc.to_pin,
                TargetPinDirection::Output,
            )
            .timing_arcs
            .push(canonical_arc(
                clock_to_q.arc,
                TargetTimingType::ClockToQ(clock_to_q.clock_edge),
            ));
        }
        for constraint in cell.constraints {
            ensure_pin(&mut pins, &constraint.clock_pin, TargetPinDirection::Input);
            ensure_pin(&mut pins, &constraint.data_pin, TargetPinDirection::Input)
                .timing_arcs
                .push(TargetTimingArc {
                    related_pin: constraint.clock_pin,
                    timing_type: match constraint.kind {
                        TimingCheckKind::Setup | TimingCheckKind::Hold => TargetTimingType::Check {
                            kind: constraint.kind,
                            clock_edge: constraint.clock_edge,
                        },
                        TimingCheckKind::Recovery => {
                            TargetTimingType::Recovery(constraint.clock_edge)
                        }
                        TimingCheckKind::Removal => {
                            TargetTimingType::Removal(constraint.clock_edge)
                        }
                    },
                    timing_sense: TimingSense::NonUnate,
                    delay_model: None,
                    rise_constraint: constraint.rise_constraint,
                    fall_constraint: constraint.fall_constraint,
                });
        }
        TargetCell {
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            name: cell.name,
            area: None,
            pins: pins.into_values().collect(),
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }
    }

    fn canonical_arc(arc: TimingArc, timing_type: TargetTimingType) -> TargetTimingArc {
        let delay_model = (arc.cell_rise.is_some()
            || arc.cell_fall.is_some()
            || arc.rise_transition.is_some()
            || arc.fall_transition.is_some())
        .then(|| {
            ArcDelayModel::Nldm(NldmTimingModel::new(
                arc.cell_rise,
                arc.cell_fall,
                arc.rise_transition,
                arc.fall_transition,
            ))
        });
        TargetTimingArc {
            related_pin: arc.from_pin,
            timing_type,
            timing_sense: arc.timing_sense,
            delay_model,
            rise_constraint: None,
            fall_constraint: None,
        }
    }

    fn ensure_pin<'a>(
        pins: &'a mut BTreeMap<String, TargetPin>,
        name: &str,
        direction: TargetPinDirection,
    ) -> &'a mut TargetPin {
        let pin = pins.entry(name.to_string()).or_insert_with(|| TargetPin {
            name: name.to_string(),
            direction,
            function: None,
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        });
        if direction == TargetPinDirection::Output {
            pin.direction = direction;
        }
        pin
    }
}
