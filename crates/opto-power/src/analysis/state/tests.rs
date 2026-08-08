// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_db::ObjectUid;
use opto_library::{
    ArcDelayModel, BooleanFunction, LookupTable, NldmTimingModel, PowerLibraryUnits, TargetCell,
    TargetCellUsage, TargetPin, TargetTimingArc, TargetTimingType, TimingSense,
};
use opto_runtime::ExecutionConfig;
use opto_timing::{
    DelayType, DesignId, PortId, TimingConnection, TimingContext, TimingDesign,
    TimingElectricalSnapshot, TimingEngine, TimingInstance, TimingInstanceId, TimingLibrary,
    TimingNet, TimingPort, TimingPortDirection,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn runtime(workers: usize) -> ExecutionContext {
    ExecutionContext::new(&ExecutionConfig {
        max_threads: workers,
    })
    .unwrap()
}

fn target_pin(
    name: &str,
    direction: TargetPinDirection,
    function: Option<BooleanFunction>,
) -> TargetPin {
    TargetPin {
        name: name.to_string(),
        direction,
        function,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        timing_arcs: (name == "Y")
            .then(|| TargetTimingArc {
                related_pin: "A".to_string(),
                timing_type: TargetTimingType::Combinational,
                timing_sense: TimingSense::PositiveUnate,
                delay_model: Some(ArcDelayModel::Nldm(NldmTimingModel::new(
                    Some(LookupTable::scalar(0.1)),
                    Some(LookupTable::scalar(0.1)),
                    None,
                    None,
                ))),
                rise_constraint: None,
                fall_constraint: None,
            })
            .into_iter()
            .collect(),
        clock_gate_role: None,
    }
}

fn buffer_cell() -> TargetCell {
    TargetCell {
        name: "BUF".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: TargetCellUsage::default(),
        pins: vec![
            target_pin("A", TargetPinDirection::Input, None),
            target_pin(
                "Y",
                TargetPinDirection::Output,
                Some(BooleanFunction::Pin("A".to_string())),
            ),
        ],
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn and_cell() -> TargetCell {
    let mut output = target_pin(
        "Y",
        TargetPinDirection::Output,
        Some(BooleanFunction::And(
            Box::new(BooleanFunction::Pin("A".to_string())),
            Box::new(BooleanFunction::Pin("B".to_string())),
        )),
    );
    let mut second_arc = output.timing_arcs[0].clone();
    second_arc.related_pin = "B".to_string();
    output.timing_arcs.push(second_arc);
    TargetCell {
        name: "AND2".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: TargetCellUsage::default(),
        pins: vec![
            target_pin("A", TargetPinDirection::Input, None),
            target_pin("B", TargetPinDirection::Input, None),
            output,
        ],
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn fixture() -> (
    Arc<TimingModel>,
    TimingElectricalSnapshot,
    ActivityAnnotations,
) {
    let mut library = TimingLibrary::default();
    library.power.units = PowerLibraryUnits {
        time_seconds: Some(1e-9),
        capacitance_farads: Some(1e-12),
        voltage_volts: Some(1.0),
        leakage_power_watts: Some(1e-9),
        nominal_voltage: Some(1.0),
    };
    library.cells = vec![buffer_cell(), and_cell()].into();
    let port = |raw: u64, name: &str, direction: TimingPortDirection| TimingPort {
        id: PortId::from_uid(ObjectUid::from_raw(raw).unwrap()),
        name: name.to_string(),
        net: TimingNet::named(name),
        direction,
    };
    let buffer = |raw, name: &str, input: &str, output: &str| TimingInstance {
        id: TimingInstanceId::from_raw(raw),
        name: name.to_string(),
        cell: "BUF".to_string(),
        connections: vec![
            TimingConnection {
                pin: "A".to_string(),
                net: input.to_string(),
            },
            TimingConnection {
                pin: "Y".to_string(),
                net: output.to_string(),
            },
        ],
    };
    let model = Arc::new(
        TimingModel::new(
            TimingDesign {
                id: DesignId::from_uid(ObjectUid::from_raw(1).unwrap()),
                name: "top".to_string(),
                ports: vec![
                    port(2, "a", TimingPortDirection::Input),
                    port(3, "b", TimingPortDirection::Input),
                    port(4, "ya", TimingPortDirection::Output),
                    port(5, "yb", TimingPortDirection::Output),
                    port(6, "y", TimingPortDirection::Output),
                ],
                instances: vec![
                    buffer(0, "a0", "a", "na"),
                    buffer(1, "b0", "b", "nb"),
                    buffer(2, "a1", "na", "ya"),
                    buffer(3, "b1", "nb", "yb"),
                    TimingInstance {
                        id: TimingInstanceId::from_raw(4),
                        name: "join".to_string(),
                        cell: "AND2".to_string(),
                        connections: vec![
                            TimingConnection {
                                pin: "A".to_string(),
                                net: "ya".to_string(),
                            },
                            TimingConnection {
                                pin: "B".to_string(),
                                net: "yb".to_string(),
                            },
                            TimingConnection {
                                pin: "Y".to_string(),
                                net: "y".to_string(),
                            },
                        ],
                    },
                ],
            },
            library,
        )
        .unwrap(),
    );
    let timing = TimingEngine::new(runtime(1))
        .electrical_snapshot(
            &TimingContext::default(),
            Arc::clone(&model),
            DelayType::Max,
        )
        .unwrap();
    let annotations = ActivityAnnotations::new(
        model.generation(),
        [
            (
                model.net_id("a").unwrap(),
                SwitchingActivity::new(0.25, 0.1, 0.5).unwrap(),
            ),
            (
                model.net_id("b").unwrap(),
                SwitchingActivity::new(0.75, 0.3, 0.5).unwrap(),
            ),
        ],
    )
    .unwrap();
    (model, timing, annotations)
}

#[test]
fn activity_annotations_revalidate_directly_constructed_values() {
    let (model, _, _) = fixture();
    let invalid = SwitchingActivity {
        static_probability: 0.5,
        toggle_rate: -1.0,
        rise_ratio: 0.5,
    };
    let error =
        ActivityAnnotations::new(model.generation(), [(model.net_id("a").unwrap(), invalid)])
            .unwrap_err();
    assert!(matches!(error, PowerError::InvalidActivity { .. }));
}

#[test]
fn propagation_is_stable_across_worker_counts() {
    let (model, timing, annotations) = fixture();
    let mut serial =
        PowerAnalysisState::analyze(&runtime(1), &model, &timing, &annotations).unwrap();
    let mut parallel =
        PowerAnalysisState::analyze(&runtime(4), &model, &timing, &annotations).unwrap();
    assert_eq!(serial.analysis, parallel.analysis);
    assert_eq!(
        serial.analysis.data.activities,
        parallel.analysis.data.activities
    );

    let updated = ActivityAnnotations::new(
        model.generation(),
        [
            (
                model.net_id("a").unwrap(),
                SwitchingActivity::new(0.6, 0.2, 0.5).unwrap(),
            ),
            (
                model.net_id("b").unwrap(),
                SwitchingActivity::new(0.4, 0.4, 0.5).unwrap(),
            ),
        ],
    )
    .unwrap();
    let serial_counts = serial
        .update_activities(&runtime(1), &model, &timing, &annotations, &updated)
        .unwrap();
    let parallel_counts = parallel
        .update_activities(&runtime(4), &model, &timing, &annotations, &updated)
        .unwrap();
    assert_eq!(serial_counts, parallel_counts);
    assert_eq!(serial.analysis, parallel.analysis);
    assert_eq!(
        serial.analysis.data.activities,
        parallel.analysis.data.activities
    );
}

#[test]
fn annotated_net_cuts_an_incremental_dependency_when_both_sides_are_dirty() {
    let (model, timing, inputs) = fixture();
    let internal = model.net_id("na").unwrap();
    let updated = ActivityAnnotations::new(
        model.generation(),
        [
            (
                model.net_id("a").unwrap(),
                SwitchingActivity::new(0.6, 0.2, 0.5).unwrap(),
            ),
            (
                model.net_id("b").unwrap(),
                SwitchingActivity::new(0.75, 0.3, 0.5).unwrap(),
            ),
            (internal, SwitchingActivity::new(0.7, 0.25, 0.5).unwrap()),
        ],
    )
    .unwrap();
    let mut state = PowerAnalysisState::analyze(&runtime(1), &model, &timing, &inputs).unwrap();
    let downstream_finished = Arc::new(AtomicBool::new(false));
    let observe = Arc::clone(&downstream_finished);
    let counts = state
        .update_activities_with_hook(
            &runtime(2),
            &model,
            &timing,
            &inputs,
            &updated,
            move |row| {
                if row == 2 {
                    observe.store(true, Ordering::Release);
                } else if row == 0 {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !observe.load(Ordering::Acquire) {
                        assert!(
                            Instant::now() < deadline,
                            "annotated net retained a false activity dependency"
                        );
                        std::thread::yield_now();
                    }
                }
            },
        )
        .unwrap();
    assert_eq!(counts, PowerUpdateCounts { nets: 4, cells: 3 });

    let removed = ActivityAnnotations::new(
        model.generation(),
        [
            (
                model.net_id("a").unwrap(),
                SwitchingActivity::new(0.2, 0.05, 0.5).unwrap(),
            ),
            (
                model.net_id("b").unwrap(),
                SwitchingActivity::new(0.75, 0.3, 0.5).unwrap(),
            ),
        ],
    )
    .unwrap();
    let upstream_finished = Arc::new(AtomicBool::new(false));
    let observe = Arc::clone(&upstream_finished);
    let counts = state
        .update_activities_with_hook(
            &runtime(2),
            &model,
            &timing,
            &updated,
            &removed,
            move |row| {
                if row == 0 {
                    observe.store(true, Ordering::Release);
                } else if row == 2 {
                    assert!(
                        observe.load(Ordering::Acquire),
                        "removing an annotation did not restore its exact dependency"
                    );
                }
            },
        )
        .unwrap();
    assert_eq!(counts, PowerUpdateCounts { nets: 4, cells: 3 });
}

#[test]
fn reconvergent_instance_waits_for_every_exact_dependency() {
    let (model, _timing, annotations) = fixture();
    let topology = PowerTopology::new(&model).unwrap();
    let mut activities = vec![NetActivity::default(); model.net_count()];
    let left_finished = Arc::new(AtomicBool::new(false));
    let right_finished = Arc::new(AtomicBool::new(false));
    let observe_left = Arc::clone(&left_finished);
    let observe_right = Arc::clone(&right_finished);
    propagate_initial_with_hook(
        &runtime(3),
        &model,
        &topology,
        &annotations,
        &mut activities,
        move |row| match row {
            2 => {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !observe_right.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < deadline,
                        "independent reconvergent branch did not complete"
                    );
                    std::thread::yield_now();
                }
                observe_left.store(true, Ordering::Release);
            }
            3 => observe_right.store(true, Ordering::Release),
            4 => {
                assert!(observe_left.load(Ordering::Acquire));
                assert!(observe_right.load(Ordering::Acquire));
            }
            _ => {}
        },
    )
    .unwrap();

    let output = activities[index(model.net_id("y").unwrap())].value;
    assert!((output.static_probability - 0.1875).abs() < 1e-12);
}

#[test]
fn multiple_output_pins_on_one_instance_cannot_drive_one_net() {
    let mut cell = buffer_cell();
    cell.name = "DUAL".to_string();
    let mut second_output = cell.pins[1].clone();
    second_output.name = "Z".to_string();
    cell.pins.push(second_output);
    let library = TimingLibrary {
        cells: vec![cell].into(),
        ..TimingLibrary::default()
    };
    let model = Arc::new(
        TimingModel::new(
            TimingDesign {
                id: DesignId::from_uid(ObjectUid::from_raw(20).unwrap()),
                name: "top".to_string(),
                ports: vec![
                    TimingPort {
                        id: PortId::from_uid(ObjectUid::from_raw(21).unwrap()),
                        name: "a".to_string(),
                        net: TimingNet::named("a"),
                        direction: TimingPortDirection::Input,
                    },
                    TimingPort {
                        id: PortId::from_uid(ObjectUid::from_raw(22).unwrap()),
                        name: "y".to_string(),
                        net: TimingNet::named("y"),
                        direction: TimingPortDirection::Output,
                    },
                ],
                instances: vec![TimingInstance {
                    id: TimingInstanceId::from_raw(0),
                    name: "u0".to_string(),
                    cell: "DUAL".to_string(),
                    connections: vec![
                        TimingConnection {
                            pin: "A".to_string(),
                            net: "a".to_string(),
                        },
                        TimingConnection {
                            pin: "Y".to_string(),
                            net: "y".to_string(),
                        },
                        TimingConnection {
                            pin: "Z".to_string(),
                            net: "y".to_string(),
                        },
                    ],
                }],
            },
            library,
        )
        .unwrap(),
    );
    let _timing = TimingEngine::new(runtime(1))
        .net_states(
            &TimingContext::default(),
            Arc::clone(&model),
            DelayType::Max,
        )
        .unwrap();

    assert!(matches!(
        PowerTopology::new(&model),
        Err(PowerError::MultipleNetDrivers { .. })
    ));
}

#[test]
fn propagation_is_stable_under_injected_physical_completion_orders() {
    let (model, _timing, annotations) = fixture();
    let run = |delayed: usize, fast: usize| {
        let topology = PowerTopology::new(&model).unwrap();
        let mut activities = vec![NetActivity::default(); model.net_count()];
        let successor_finished = Arc::new(AtomicBool::new(false));
        let order = Arc::new(Mutex::new(Vec::new()));
        let observe_successor = Arc::clone(&successor_finished);
        let observe_order = Arc::clone(&order);
        propagate_initial_with_hook(
            &runtime(2),
            &model,
            &topology,
            &annotations,
            &mut activities,
            move |row| {
                if row == fast + 2 {
                    observe_successor.store(true, Ordering::Release);
                } else if row == delayed {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !observe_successor.load(Ordering::Acquire) {
                        assert!(
                            Instant::now() < deadline,
                            "independent successor was held behind a completion wave"
                        );
                        std::thread::yield_now();
                    }
                }
                observe_order.lock().unwrap().push(row);
            },
        )
        .unwrap();
        let order = Arc::into_inner(order).unwrap().into_inner().unwrap();
        (activities, order)
    };

    let (a_first, a_order) = run(1, 0);
    let (b_first, b_order) = run(0, 1);
    assert_eq!(a_order[0], 0);
    assert_eq!(b_order[0], 1);
    assert!(a_order.iter().position(|&row| row == 2) < a_order.iter().position(|&row| row == 1));
    assert!(b_order.iter().position(|&row| row == 3) < b_order.iter().position(|&row| row == 0));
    assert_eq!(a_first, b_first);
}
