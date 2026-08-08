// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_library::{
    TargetCell, TargetCellSet, TargetCellUsage, TargetClockGateKind, TargetClockGateRole,
    TargetPin, TargetPinDirection,
};

fn gate_pin(name: &str, direction: TargetPinDirection, role: TargetClockGateRole) -> TargetPin {
    TargetPin {
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
        clock_gate_role: Some(role),
        timing_arcs: Vec::new(),
    }
}

fn gate_cell(name: &str, kind: TargetClockGateKind) -> TargetCell {
    TargetCell {
        name: name.to_string(),
        area: Some(4.0),
        dont_use: false,
        usage: TargetCellUsage::INTEGRATED_CLOCK_GATING,
        pins: vec![
            gate_pin("CP", TargetPinDirection::Input, TargetClockGateRole::Clock),
            gate_pin("E", TargetPinDirection::Input, TargetClockGateRole::Enable),
            gate_pin("Q", TargetPinDirection::Output, TargetClockGateRole::Output),
        ],
        sequential: Vec::new(),
        clock_gate: Some(kind),
        memory: None,
    }
}

fn library() -> TargetCellSet {
    vec![gate_cell("CKLNQ", TargetClockGateKind::LatchPosedge)].into()
}

fn catalog(cells: TargetCellSet) -> ClockGatingCatalog {
    ClockGatingCatalog::new(&crate::SynthesisOptions {
        target_cells: cells,
    })
}

fn enable_register_module(width: u32, active_high: bool) -> word::WordModule {
    let mut module = word::WordModule::new("top");
    let source = word::SourceSpan::default();
    let bits = word::WordType::bits(width).unwrap();
    let scalar = word::WordType::bits(1).unwrap();
    let clock = module
        .add_port("clk", word::PortDirection::Input, scalar, source.clone())
        .unwrap();
    let enable = module
        .add_port("en", word::PortDirection::Input, scalar, source.clone())
        .unwrap();
    let data = module
        .add_port("d", word::PortDirection::Input, bits, source.clone())
        .unwrap();
    let output = module
        .add_port("q", word::PortDirection::Output, bits, source.clone())
        .unwrap();
    let mut read = |port| {
        let signal = module.port(port).unwrap().signal;
        module.read_signal(signal, source.clone()).unwrap()
    };
    let clock_value = read(clock);
    let enable_value = read(enable);
    let data_value = read(data);
    let target = module.port(output).unwrap().signal;
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: data_value,
                clock: clock_value,
                edge: word::Edge::Pos,
                enable: Some(word::Enable {
                    value: enable_value,
                    active_high,
                }),
                resets: Vec::new(),
            },
            source.clone(),
        )
        .unwrap();
    module
        .connect(
            word::LValue {
                signal: target,
                range: None,
                dynamic: None,
            },
            register,
            source,
        )
        .unwrap();
    module
}

fn register_enables(module: &word::WordModule) -> Vec<Option<word::Enable>> {
    module
        .operations()
        .iter()
        .filter_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register.enable),
            _ => None,
        })
        .collect()
}

fn gate_instances(module: &word::WordModule) -> usize {
    module
        .instances()
        .iter()
        .filter(|instance| module.name_str(instance.module) == "CKLNQ")
        .count()
}

#[test]
fn gates_a_register_wider_than_the_threshold() {
    let catalog = catalog(library());
    let mut module = enable_register_module(8, true);
    let summary = gate_register_clocks(&mut module, &catalog, ClockGatingStyle::default()).unwrap();
    assert_eq!(
        summary,
        ClockGatingSummary {
            gates: 1,
            registers: 1,
            gated_bits: 8
        }
    );
    assert_eq!(gate_instances(&module), 1);
    assert_eq!(register_enables(&module), vec![None]);
}

#[test]
fn keeps_registers_narrower_than_the_threshold() {
    let catalog = catalog(library());
    let mut module = enable_register_module(2, true);
    let summary = gate_register_clocks(&mut module, &catalog, ClockGatingStyle::default()).unwrap();
    assert_eq!(summary, ClockGatingSummary::default());
    assert_eq!(gate_instances(&module), 0);
    assert!(register_enables(&module)[0].is_some());
}

#[test]
fn gated_register_reads_the_generated_clock() {
    let catalog = catalog(library());
    let mut module = enable_register_module(8, true);
    gate_register_clocks(&mut module, &catalog, ClockGatingStyle::default()).unwrap();
    let gate = module
        .instances()
        .iter()
        .find(|instance| module.name_str(instance.module) == "CKLNQ")
        .expect("clock gate was inserted");
    let gated = gate
        .connections
        .iter()
        .find(|connection| module.name_str(connection.port) == "Q")
        .expect("clock gate drives its output")
        .value;
    let clock = module
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register.clock),
            _ => None,
        })
        .expect("module retains its register");
    assert_eq!(clock, gated);
}

#[test]
fn inverts_active_low_enables_into_the_gate() {
    let catalog = catalog(library());
    let mut module = enable_register_module(8, false);
    let before = module.operations().len();
    gate_register_clocks(&mut module, &catalog, ClockGatingStyle::default()).unwrap();
    assert_eq!(gate_instances(&module), 1);
    assert!(
        module.operations().len() > before,
        "an active-low enable requires an inverter"
    );
}

#[test]
fn skips_edges_without_a_matching_gate() {
    let catalog = catalog(vec![gate_cell("CKLHQ", TargetClockGateKind::LatchNegedge)].into());
    let mut module = enable_register_module(8, true);
    let summary = gate_register_clocks(&mut module, &catalog, ClockGatingStyle::default()).unwrap();
    assert_eq!(summary, ClockGatingSummary::default());
    assert_eq!(gate_instances(&module), 0);
}

#[test]
fn latch_free_style_rejects_latch_based_gates() {
    let catalog = catalog(library());
    let mut module = enable_register_module(8, true);
    let style = ClockGatingStyle {
        minimum_bitwidth: 3,
        latch_based: false,
    };
    let summary = gate_register_clocks(&mut module, &catalog, style).unwrap();
    assert_eq!(summary, ClockGatingSummary::default());
    assert_eq!(gate_instances(&module), 0);
}
