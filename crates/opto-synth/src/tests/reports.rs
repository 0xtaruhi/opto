// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn reports_are_derived_from_rtl_module() {
    let module = structural_module();
    let mut area = AreaReportContext::default();
    area.library_cell_area.insert("child".to_string(), 2.5);

    assert!(
        report_area(&module, &area)
            .render_plain()
            .contains("Number of cells: 1")
    );
    assert!(
        report_area(&module, &area)
            .render_plain()
            .contains("Total cell area: 2.500000")
    );

    area.library_cell_kind
        .insert("child".to_string(), AreaCellKind::Combinational);
    assert!(
        report_qor(&module, &area, None)
            .render_plain()
            .contains("Combinational cells: 2")
    );
    let report = report_area(&module, &area).render_plain();
    assert!(report.contains("Number of combinational cells: 1"));
    assert!(report.contains("Number of macros/black boxes: 0"));
    assert!(report.contains("Combinational area: 2.500000"));
}

#[test]
fn area_report_counts_port_and_net_bits() {
    let mut module = WordModule::new("top");
    let vector = WordType::new(2, false, LogicStateKind::FourState).unwrap();
    let input = module
        .add_port(
            "input_bus",
            PortDirection::Input,
            vector,
            SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "output_bus",
            PortDirection::Output,
            vector,
            SourceSpan::default(),
        )
        .unwrap();
    let input_value = module
        .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(output).unwrap().signal),
            input_value,
            SourceSpan::default(),
        )
        .unwrap();

    let report = report_area(&module, &AreaReportContext::default()).render_plain();

    assert!(report.contains("Number of ports: 4"));
    assert!(report.contains("Number of nets: 2"));
}
