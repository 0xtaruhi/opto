// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Receiver-specific wire delay and consistency of forward/backward STA.

use super::*;
use opto_library::{TimingLibraryUnits, WireLoadModel, WireLoadTree};

fn fanout_fixture(tree: WireLoadTree) -> (crate::TimingDesign, TimingLibrary) {
    let mut cells = test_target_cells(vec![
        TimingCell {
            name: "LIGHT".into(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.2)],
            pin_capacitance: BTreeMap::from([("A".into(), 2.0)]),
            ..TimingCell::default()
        },
        TimingCell {
            name: "HEAVY".into(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.2)],
            pin_capacitance: BTreeMap::from([("A".into(), 6.0)]),
            ..TimingCell::default()
        },
    ]);
    for cell in &mut cells {
        let input = cell.pins.iter_mut().find(|pin| pin.name == "A").unwrap();
        input.fall_capacitance = input.capacitance.map(|cap| cap * 1.5);
        input.fanout_load = Some(7.0);
    }
    let library = TimingLibrary {
        cells: cells.into(),
        wire_load_tree: tree,
        wire_load_model: Some(WireLoadModel::new("wl".into(), 1.0, 1.0, 1.0, Vec::new()).unwrap()),
        units: TimingLibraryUnits {
            time_seconds: Some(1e-9),
            capacitance_farads: Some(1e-15),
            resistance_ohms: Some(1e3),
        },
        ..TimingLibrary::default()
    };
    let design = crate::TimingDesign {
        id: test_design_id(),
        name: "top".into(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("light", TimingPortDirection::Output),
            test_port("heavy", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "L", "LIGHT", [("A", "a"), ("Y", "light")]),
            test_instance(1, "H", "HEAVY", [("A", "a"), ("Y", "heavy")]),
        ],
    };
    (design, library)
}

#[test]
fn wire_tree_propagates_receiver_loads_in_both_timing_directions() {
    for (tree, rise, fall, output_wire) in [
        (
            WireLoadTree::Balanced,
            [0.003, 0.007],
            [0.004, 0.010],
            0.005,
        ),
        (WireLoadTree::WorstCase, [0.020; 2], [0.028; 2], 0.005),
        (WireLoadTree::BestCase, [0.0; 2], [0.0; 2], 0.0),
    ] {
        let (design, library) = fanout_fixture(tree);
        let model = crate::test_timing_model(&design, &library);
        for drive in [0.0, 2.0] {
            let mut timing = TimingContext::new();
            timing
                .set_load(4.0, &[test_port_id("light"), test_port_id("heavy")])
                .unwrap();
            timing
                .set_drive(
                    drive,
                    EdgeSelection::Both,
                    CornerSelection::Both,
                    &[test_port_id("a")],
                )
                .unwrap();
            timing
                .set_max_delay(
                    10.0,
                    Vec::new(),
                    vec![
                        TimingEndpoint::Port(test_port_id("light")),
                        TimingEndpoint::Port(test_port_id("heavy")),
                    ],
                )
                .unwrap();
            for (index, port) in ["light", "heavy"].into_iter().enumerate() {
                for (delay_type, branch, load) in [
                    (DelayType::Min, rise[index], 10.0),
                    (DelayType::Max, fall[index], 14.0),
                ] {
                    let options = ReportTimingOptions {
                        to: vec![port.into()],
                        delay_type,
                        ..ReportTimingOptions::default()
                    };
                    let report = analyze_timing(&timing, &model, &options).unwrap();
                    let expected = branch + drive * 0.001 * load + 0.2 + output_wire;
                    assert!(
                        (report.arrival() - expected).abs() < 1e-12,
                        "{tree:?} {port} {delay_type:?}: {} != {expected}",
                        report.arrival()
                    );
                }
            }
            let engine = crate::IncrementalTiming::new(
                timing,
                crate::test_timing_model(&design, &library),
                ReportTimingOptions::default(),
            )
            .unwrap();
            let input = engine.net_state("a").unwrap();
            assert!((input.fanout - 14.0).abs() < 1e-12);
            assert!((input.wire_fanout - 2.0).abs() < 1e-12);
            assert!((input.capacitance - 14.0).abs() < 1e-12);
            let expected_required = 10.0 - (fall[1] + drive * 0.001 * 14.0 + 0.2 + output_wire);
            assert!((input.required.unwrap() - expected_required).abs() < 1e-12);
        }
    }
}

#[test]
fn prepared_views_resolve_their_own_wire_tree() {
    let (design, library) = fanout_fixture(WireLoadTree::Balanced);
    let source = crate::test_timing_model(&design, &library);
    let prepared = source.prepared_topology();
    for (tree, arrival) in [
        (WireLoadTree::WorstCase, 0.229),
        (WireLoadTree::BestCase, 0.2),
    ] {
        let mut follower_library = library.clone();
        follower_library.wire_load_tree = tree;
        let follower = TimingModel::fork_prepared_view(
            &prepared,
            follower_library,
            crate::Parasitics::default(),
        )
        .unwrap();
        let report = analyze_timing(
            &TimingContext::new(),
            &follower,
            &ReportTimingOptions::default(),
        )
        .unwrap();
        assert!((report.arrival() - arrival).abs() < 1e-12);
        assert_ne!(source.generation(), follower.generation());
    }
}

#[test]
fn extracted_sink_delays_replace_every_wire_tree() {
    use crate::{Parasitics, RcCapacitor, RcConnection, RcConnectionRole, RcNetwork, RcResistor};
    let (design, mut library) = fanout_fixture(WireLoadTree::Balanced);
    let parasitics = Parasitics::from_rc_networks(
        vec![RcNetwork {
            name: "a".into(),
            total_capacitance_farads: 8e-15,
            connections: [
                ("a", "a", RcConnectionRole::Driver),
                ("l", "L/A", RcConnectionRole::Sink),
                ("h", "H/A", RcConnectionRole::Sink),
            ]
            .into_iter()
            .map(|(node, object, role)| RcConnection {
                node: node.into(),
                object: object.into(),
                role,
                pin_capacitance_farads: [0.0; 2],
            })
            .collect(),
            capacitors: [("l", 2e-15), ("h", 6e-15)]
                .into_iter()
                .map(|(node, capacitance_farads)| RcCapacitor {
                    first: node.into(),
                    second: None,
                    capacitance_farads,
                })
                .collect(),
            resistors: [("l", 1000.0), ("h", 2000.0)]
                .into_iter()
                .map(|(node, resistance_ohms)| RcResistor {
                    first: "a".into(),
                    second: node.into(),
                    resistance_ohms,
                })
                .collect(),
            source_waveforms: [None, None],
        }],
        library.units,
        crate::ParasiticAnalysisOptions {
            delay_model: crate::ParasiticDelayModel::Elmore,
            ..crate::ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    for tree in [
        WireLoadTree::Balanced,
        WireLoadTree::WorstCase,
        WireLoadTree::BestCase,
    ] {
        library.wire_load_tree = tree;
        let model =
            TimingModel::new_with_parasitics(design.clone(), library.clone(), parasitics.clone())
                .unwrap();
        let mut timing = TimingContext::new();
        timing
            .set_drive(
                2.0,
                EdgeSelection::Both,
                CornerSelection::Both,
                &[test_port_id("a")],
            )
            .unwrap();
        let net = model.graph.net_id("a").unwrap();
        assert!((model.graph.wire_resistance(net) - 0.0).abs() < 1e-12);
        assert!((model.graph.wire_capacitance(net) - 0.0).abs() < 1e-12);
        for instance in ["L", "H"] {
            for edge in TimingEdge::ALL {
                let extracted = model
                    .graph
                    .parasitic_sink_delay_parts(net, instance, "A", edge);
                assert!(extracted > 0.0);
                let expected = extracted
                    + 0.002
                        * timing_load(&timing, &model.graph, net, edge, DelayType::Max).unwrap();
                let actual = sink_interconnect_delay_parts(
                    &timing,
                    &model,
                    &model.graph,
                    net,
                    instance,
                    "A",
                    InterconnectDelayMode::data(edge, DelayType::Max),
                );
                assert!((actual - expected).abs() < 1e-12);
            }
        }
    }
}
