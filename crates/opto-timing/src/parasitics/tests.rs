// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn tree() -> RcNetwork {
    RcNetwork {
        name: "n".to_string(),
        total_capacitance_farads: 3e-15,
        connections: vec![
            RcConnection {
                node: "driver".to_string(),
                object: "U1/Y".to_string(),
                role: RcConnectionRole::Driver,
                pin_capacitance_farads: [0.0; 2],
            },
            RcConnection {
                node: "sink".to_string(),
                object: "U2/A".to_string(),
                role: RcConnectionRole::Sink,
                pin_capacitance_farads: [1e-15; 2],
            },
        ],
        capacitors: vec![RcCapacitor {
            first: "sink".to_string(),
            second: None,
            capacitance_farads: 3e-15,
        }],
        resistors: vec![RcResistor {
            first: "driver".to_string(),
            second: "sink".to_string(),
            resistance_ohms: 1e3,
        }],
        source_waveforms: [None, None],
    }
}

#[test]
fn computes_scalar_elmore_delay_on_an_rc_tree() {
    let parasitics = Parasitics::from_rc_networks(
        vec![tree()],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();

    let net = parasitics.net("n").unwrap();
    assert!((net.total_capacitance() - 3.0).abs() < 1e-12);
    assert!((net.sink_delay("U2/A", TimingEdge::Rise).unwrap() - 4.0).abs() < 1e-12);
}

#[test]
fn rc_only_mode_preserves_a_cyclic_network_without_annotating_delay() {
    let mut network = tree();
    network.resistors.extend([
        RcResistor {
            first: "driver".to_string(),
            second: "branch".to_string(),
            resistance_ohms: 1e3,
        },
        RcResistor {
            first: "branch".to_string(),
            second: "sink".to_string(),
            resistance_ohms: 1e3,
        },
    ]);
    let parasitics = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions::default(),
    )
    .unwrap();
    let net = parasitics.net("n").unwrap();
    assert_eq!(net.delay_model(), ParasiticDelayModel::None);
    assert_eq!(net.sink_delay("U2/A", TimingEdge::Rise), None);
}

#[test]
fn partial_network_is_kept_without_backannotating_delay() {
    let mut network = tree();
    network.resistors.clear();
    let parasitics = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();

    let net = parasitics.net("n").unwrap();
    assert!((net.total_capacitance() - 3.0).abs() < 1e-12);
    assert_eq!(net.annotated_capacitance(), None);
    assert_eq!(net.sink_delay("U2/A", TimingEdge::Rise), None);
}

#[test]
fn incremental_overlay_updates_rc_but_retains_existing_delays() {
    let original = Parasitics::from_rc_networks(
        vec![tree()],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    let mut changed = tree();
    changed.total_capacitance_farads = 9e-15;
    changed.resistors[0].resistance_ohms = 2e3;
    let changed = Parasitics::from_rc_networks(
        vec![changed],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();

    let incremental = original.overlay(changed.clone(), true).unwrap();
    let net = incremental.net("n").unwrap();
    assert!((net.total_capacitance() - 9.0).abs() < 1e-12);
    assert!((net.sink_delay("U2/A", TimingEdge::Rise).unwrap() - 4.0).abs() < 1e-12);
    let replaced = original.overlay(changed, false).unwrap();
    assert!(
        (replaced
            .net("n")
            .unwrap()
            .sink_delay("U2/A", TimingEdge::Rise)
            .unwrap()
            - 8.0)
            .abs()
            < 1e-12
    );
}

#[test]
fn rejects_every_two_node_coupling_capacitor() {
    for second in ["driver", "external_net:1"] {
        let mut network = tree();
        network.capacitors.push(RcCapacitor {
            first: "sink".to_string(),
            second: Some(second.to_string()),
            capacitance_farads: 1e-15,
        });
        let error = Parasitics::from_rc_networks(
            vec![network],
            crate::test_library_units(),
            ParasiticAnalysisOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("coupling capacitor"));
    }
}

#[test]
fn rejects_cyclic_rc_networks_in_elmore_mode() {
    let mut network = tree();
    network.resistors.extend([
        RcResistor {
            first: "driver".to_string(),
            second: "branch".to_string(),
            resistance_ohms: 1e3,
        },
        RcResistor {
            first: "branch".to_string(),
            second: "sink".to_string(),
            resistance_ohms: 1e3,
        },
    ]);
    let error = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cyclic"));
}

#[test]
fn multiple_drivers_use_the_conservative_sink_delay_envelope() {
    let mut network = tree();
    network.connections.push(RcConnection {
        node: "other_driver".to_string(),
        object: "U3/Y".to_string(),
        role: RcConnectionRole::Driver,
        pin_capacitance_farads: [0.0; 2],
    });
    network.resistors.push(RcResistor {
        first: "other_driver".to_string(),
        second: "sink".to_string(),
        resistance_ohms: 2e3,
    });
    let parasitics = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();

    let delay = parasitics
        .net("n")
        .unwrap()
        .sink_delay("U2/A", TimingEdge::Rise)
        .unwrap();
    assert!((delay - 8.0).abs() < 1e-12, "{delay}");
}

#[test]
fn arnoldi_matches_the_single_pole_rc_step_response() {
    let parasitics = Parasitics::from_rc_networks(
        vec![tree()],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Arnoldi,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    let net = parasitics.net("n").unwrap();
    let delay = net.sink_delay("U2/A", TimingEdge::Rise).unwrap();
    let transition = net.sink_transition("U2/A", TimingEdge::Rise).unwrap();
    assert!((delay - 4.0 * 2.0_f64.ln()).abs() < 0.03, "{delay}");
    assert!(
        (transition - 4.0 * 4.0_f64.ln()).abs() < 0.04,
        "{transition}"
    );
}

#[test]
fn arnoldi_accepts_and_analyzes_a_resistor_loop() {
    let mut network = tree();
    network.resistors.extend([
        RcResistor {
            first: "driver".to_string(),
            second: "branch".to_string(),
            resistance_ohms: 1e3,
        },
        RcResistor {
            first: "branch".to_string(),
            second: "sink".to_string(),
            resistance_ohms: 1e3,
        },
    ]);
    let parasitics = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Arnoldi,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    assert!(
        parasitics
            .net("n")
            .unwrap()
            .sink_delay("U2/A", TimingEdge::Rise)
            .unwrap()
            > 0.0
    );
}

#[test]
fn arnoldi_consumes_edge_specific_receiver_capacitance_and_driver_waveforms() {
    let mut network = tree();
    network.connections[1].pin_capacitance_farads = [1e-15, 5e-15];
    network.source_waveforms[0] = Some(RcSourceWaveform {
        times: vec![0.0, 4e-12],
        normalized_voltage: vec![0.0, 1.0],
    });
    let parasitics = Parasitics::from_rc_networks(
        vec![network],
        crate::test_library_units(),
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Arnoldi,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    let net = parasitics.net("n").unwrap();
    assert!(
        net.sink_delay("U2/A", TimingEdge::Fall).unwrap()
            > net.sink_delay("U2/A", TimingEdge::Rise).unwrap()
    );
    assert!(net.sink_transition("U2/A", TimingEdge::Rise).unwrap() > 4.0);
}
