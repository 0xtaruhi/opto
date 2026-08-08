// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn cell(
    name: &str,
    dont_use: bool,
    usage: opto_library::TargetCellUsage,
    output_pin: bool,
) -> opto_library::TargetCell {
    opto_library::TargetCell {
        name: name.to_string(),
        area: Some(1.0),
        dont_use,
        usage,
        pins: output_pin
            .then(|| opto_library::TargetPin {
                name: "Y".to_string(),
                direction: opto_library::TargetPinDirection::Output,
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
            })
            .into_iter()
            .collect(),
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn source_provenance(module: &word::WordModule) -> SourceInstanceProvenance {
    SourceInstanceProvenance::capture(module)
}

#[test]
fn lowers_scalar_signedness_casts_as_wire_aliases() {
    let mut module = word::WordModule::new("scalar_cast");
    let signed = word::WordType::new(1, true, word::LogicStateKind::FourState).unwrap();
    let unsigned = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            signed,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "y",
            word::PortDirection::Output,
            unsigned,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let cast = module
        .cast(
            word::CastKind::ZeroExtend,
            input,
            unsigned,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            cast,
            word::SourceSpan::default(),
        )
        .unwrap();

    let source_instances = source_provenance(&module);
    let mapped = build_test_substrate(
        &module,
        &SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    assert_eq!(mapped.cell_count(), 0);
    let input_net = mapped
        .port_nets(opto_ir::mapped::PortId::from_index(0).unwrap())
        .unwrap()[0];
    let output_net = mapped
        .port_nets(opto_ir::mapped::PortId::from_index(1).unwrap())
        .unwrap()[0];
    assert_eq!(input_net, output_net);
}

#[test]
fn materialization_preserves_preexisting_special_and_dont_use_cells() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let iso_net = module
        .add_wire("iso_unused", bit, word::SourceSpan::default())
        .unwrap();
    let dont_use_net = module
        .add_wire("dont_use_unused", bit, word::SourceSpan::default())
        .unwrap();
    let iso_output = module
        .read_signal(iso_net, word::SourceSpan::default())
        .unwrap();
    let dont_use_output = module
        .read_signal(dont_use_net, word::SourceSpan::default())
        .unwrap();
    module
        .add_instance(
            "u_iso",
            "ISO",
            vec![("Y".to_string(), iso_output, word::SourceSpan::default())],
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .add_instance(
            "u_dont_use",
            "DONT_USE",
            vec![(
                "Y".to_string(),
                dont_use_output,
                word::SourceSpan::default(),
            )],
            word::SourceSpan::default(),
        )
        .unwrap();
    let options = SynthesisOptions {
        target_cells: vec![
            cell("ISO", false, opto_library::TargetCellUsage::ISOLATION, true),
            cell(
                "DONT_USE",
                true,
                opto_library::TargetCellUsage::default(),
                true,
            ),
        ]
        .into(),
    };
    let references = crate::target_cell_reference_ports(&options.target_cells);
    let source_instances = source_provenance(&module);

    let mapped = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &references,
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    assert_eq!(mapped.cell_count(), 2);
    assert_eq!(
        mapped
            .cell_ids()
            .map(|id| mapped.cell_type(id).unwrap())
            .collect::<Vec<_>>(),
        ["ISO", "DONT_USE"]
    );
}

#[test]
fn source_link_only_cell_is_preserved_and_linked_by_timing_name() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let input = module
        .add_wire("macro_input", bit, word::SourceSpan::default())
        .unwrap();
    let output = module
        .add_wire("macro_output", bit, word::SourceSpan::default())
        .unwrap();
    let input = module
        .read_signal(input, word::SourceSpan::default())
        .unwrap();
    let output = module
        .read_signal(output, word::SourceSpan::default())
        .unwrap();
    module
        .add_instance(
            "u_macro",
            "MACRO",
            vec![
                ("A".to_string(), input, word::SourceSpan::default()),
                ("Y".to_string(), output, word::SourceSpan::default()),
            ],
            word::SourceSpan::default(),
        )
        .unwrap();
    let target = cell(
        "BUF",
        false,
        opto_library::TargetCellUsage::default(),
        false,
    );
    let mut link = cell(
        "MACRO",
        false,
        opto_library::TargetCellUsage::default(),
        true,
    );
    link.pins.insert(
        0,
        opto_library::TargetPin {
            name: "A".to_string(),
            direction: opto_library::TargetPinDirection::Input,
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
        },
    );
    link.pins[1].function = Some(opto_library::BooleanFunction::parse("A").unwrap());
    link.pins[1]
        .timing_arcs
        .push(opto_library::TargetTimingArc {
            related_pin: "A".to_string(),
            timing_type: opto_library::TargetTimingType::Combinational,
            timing_sense: opto_library::TimingSense::PositiveUnate,
            delay_model: None,
            rise_constraint: None,
            fall_constraint: None,
        });
    let options = SynthesisOptions {
        target_cells: vec![target].into(),
    };
    let link_cells: opto_library::TargetCellSet = vec![link].into();
    let references = crate::target_cell_reference_ports(&link_cells);
    let source_instances = source_provenance(&module);

    let mapped = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &references,
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    let mapped_cell = mapped.cell_ids().next().unwrap();
    assert_eq!(mapped.cell_type(mapped_cell), Some("MACRO"));
    assert_eq!(mapped.cell(mapped_cell).unwrap().library_cell, None);
    let timing = opto_timing::TimingModel::from_mapped(
        &mapped,
        opto_timing::DesignId::from_uid(
            opto_core::ObjectUid::from_raw(1).expect("test design ID is nonzero"),
        ),
        &opto_timing::PortBindings::new([]),
        opto_timing::TimingLibrary {
            cells: link_cells,
            ..opto_timing::TimingLibrary::default()
        },
    )
    .unwrap();
    assert_eq!(
        timing
            .instance_library_cell_id(opto_timing::TimingInstanceId::from_raw(0))
            .map(opto_timing::LibraryCellId::raw),
        Some(0)
    );
}

#[test]
fn unknown_source_cell_is_rejected_even_when_mapping_library_is_empty() {
    let mut module = word::WordModule::new("top");
    module
        .add_instance(
            "u_unknown",
            "UNKNOWN",
            Vec::new(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let source_instances = source_provenance(&module);

    let error = build_test_substrate(
        &module,
        &SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unknown resolution-library cell 'UNKNOWN'")
    );
}

#[test]
fn generated_link_only_and_special_cells_are_rejected() {
    for (cell_type, target_cells, link_cells) in [
        (
            "MACRO",
            vec![cell(
                "BUF",
                false,
                opto_library::TargetCellUsage::default(),
                false,
            )],
            vec![cell(
                "MACRO",
                false,
                opto_library::TargetCellUsage::default(),
                false,
            )],
        ),
        (
            "ISO",
            vec![cell(
                "ISO",
                false,
                opto_library::TargetCellUsage::ISOLATION,
                false,
            )],
            vec![cell(
                "ISO",
                false,
                opto_library::TargetCellUsage::ISOLATION,
                false,
            )],
        ),
    ] {
        let mut module = word::WordModule::new("top");
        let source_instances = source_provenance(&module);
        module
            .add_instance(
                "u_generated",
                cell_type,
                Vec::new(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let options = SynthesisOptions {
            target_cells: target_cells.into(),
        };
        let link_cells: opto_library::TargetCellSet = link_cells.into();
        let references = crate::target_cell_reference_ports(&link_cells);

        let error = build_test_substrate(
            &module,
            &options,
            &BTreeSet::new(),
            &references,
            &source_instances,
            opto_ir::RevisionId::INITIAL,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("outside the eligible target-cell set"),
            "{error}"
        );
    }
}
