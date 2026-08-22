// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::artifact::MappedCellSource;
use crate::artifact::provenance::ProvenanceBuilder;
use crate::planning::operator::ArchitectureDecisions;
use opto_ir::mapped::{CellSpec, MappedBuilder, RegionDelta};
use opto_ir::word::{BinaryOp, LValue, PortDirection, SourceSpan, WordModule, WordType};
use std::collections::BTreeSet;

fn test_span() -> SourceSpan {
    SourceSpan::stable("test")
}

#[test]
fn originless_static_cells_are_contained_by_the_global_fragment() {
    let implementations = ImplementationDb::empty(1);
    let cell = CellId::from_index(0).unwrap();
    let outside = CellId::from_index(1).unwrap();

    assert_eq!(
        implementations.cell_fragment(cell).map(|row| row.1),
        Some(FragmentFootprint::Global)
    );
    assert_eq!(implementations.cell_fragment(outside), None);
}

#[test]
fn accepted_region_replacement_moves_operator_lineage_to_final_cell_ids() {
    let (mut mapped, mut implementations, operator, original) = fixture();
    let snapshot = mapped.snapshot_region([original], []).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_cell(original).unwrap();
    let added = delta.add_cell(CellSpec::new("U1", "CELL", None)).unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let replacement = applied.added_cell(added).unwrap();
    let fragment = implementations.cell_fragment(original).unwrap().1;
    let mut implementation_delta = ImplementationDelta::default();
    implementation_delta
        .record_added_cell(added, [original], fragment)
        .unwrap();

    let prepared = implementations
        .prepare_region_edit(&mapped, &applied, &implementation_delta)
        .unwrap();
    implementations.commit_region_edit(prepared).unwrap();

    assert_eq!(implementations.operators_for_cell(original), None);
    assert_eq!(implementations.cell_fragment(original), None);
    assert_eq!(
        implementations.operators_for_cell(replacement),
        Some(std::slice::from_ref(&operator))
    );
    assert_eq!(
        implementations
            .region_for_operator(operator)
            .unwrap()
            .mapped_cells(),
        [replacement]
    );
    let region = implementations
        .region_for_operator(operator)
        .unwrap()
        .synthesis_region();
    assert_eq!(
        implementations.cell_fragment(replacement).map(|row| row.1),
        Some(FragmentFootprint::Region(region))
    );
    let impact = implementations.take_committed_fragment_impact();
    assert_eq!(impact.regions(), &BTreeSet::from([region]));
    assert!(impact.unknown_cells().is_empty());
}

#[test]
fn added_mapped_cells_require_explicit_lineage() {
    let (mut mapped, implementations, operator, original) = fixture();
    let snapshot = mapped.snapshot_region([original], []).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.add_cell(CellSpec::new("U1", "CELL", None)).unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();

    let error = implementations
        .prepare_region_edit(&mapped, &applied, &ImplementationDelta::default())
        .unwrap_err();

    assert!(error.to_string().contains("provenance lineages"), "{error}");
    assert_eq!(
        implementations.operators_for_cell(original),
        Some(std::slice::from_ref(&operator))
    );
}

fn fixture() -> (MappedNetlist, ImplementationDb, OperatorId, CellId) {
    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, ty, test_span())
        .unwrap();
    let inputs = [a, b].map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, test_span())
            .unwrap()
    });
    let result = module
        .binary(BinaryOp::Add, inputs[0], inputs[1], test_span())
        .unwrap();
    let output = module
        .add_port("y", PortDirection::Output, ty, test_span())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(output).unwrap().signal),
            result,
            test_span(),
        )
        .unwrap();
    let plan = ArchitectureDecisions::for_module(&module).unwrap();
    let operator = plan.operators()[0].id();
    let mut provenance = ProvenanceBuilder::new(&module, &plan).unwrap();
    let origins = provenance
        .origins_for_operation_cover(&module, &[result], &inputs)
        .unwrap();
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let original = builder.add_cell("U0", "CELL", None, &[]).unwrap();
    let mapped = builder.freeze().unwrap();
    let synthesis_regions = crate::SynthesisRegionGraph::build(&module).unwrap();
    let owner = synthesis_regions.regions()[0].id();
    let implementations = provenance
        .finish(
            &synthesis_regions,
            &module,
            &mapped,
            &[(
                original,
                MappedCellSource::Region {
                    origins,
                    region: owner,
                },
            )],
        )
        .unwrap();
    (mapped, implementations, operator, original)
}
