// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::DurableOperatorArena;
use crate::mapping::logic_partition::{RegionLogicDomain, RegionLogicSlice};
use opto_ir::word::{BinaryOp, LValue, PortDirection, SourceSpan, WordModule, WordType};
use std::sync::Arc;

fn input(module: &mut WordModule, name: &str) -> word::ValueId {
    let port = module
        .add_port(
            name,
            PortDirection::Input,
            WordType::bits(16).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    module
        .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
        .unwrap()
}

fn gate_chain(
    module: &mut WordModule,
    mut value: word::ValueId,
    length: usize,
    prefix: &str,
) -> word::ValueId {
    for index in 0..length {
        let other = input(module, &format!("{prefix}{index}"));
        value = module
            .binary(BinaryOp::BitXor, value, other, SourceSpan::default())
            .unwrap();
    }
    value
}

fn target() -> crate::planning::regional::StructuralTargetModel {
    let scenarios = opto_timing::ScenarioSet::single(
        Arc::new(opto_timing::TimingContext::default()),
        Arc::new(opto_timing::TimingLibrary::default()),
        opto_timing::Parasitics::default(),
    );
    crate::planning::regional::StructuralTargetModel::build(&scenarios, |_| {
        Some(crate::planning::mapping_policy::CellCost {
            area: 1.0,
            delay: 1.0,
            transition: 1.0,
            input_capacitance: 1.0,
        })
    })
}

#[test]
fn path_projection_distinguishes_equal_width_operators_and_preserves_unconstrained_logic() {
    let mut module = WordModule::new("path_context");
    let operands = ["a", "b", "c", "d", "e", "f"].map(|name| input(&mut module, name));
    let late = gate_chain(&mut module, operands[2], 24, "before");
    let sums = [
        (operands[0], operands[1]),
        (late, operands[3]),
        (operands[4], operands[5]),
    ]
    .map(|(a, b)| {
        module
            .binary(BinaryOp::Add, a, b, SourceSpan::default())
            .unwrap()
    });
    let tail = gate_chain(&mut module, sums[1], 10, "after");
    let outputs = [sums[0], tail, sums[2]];
    for (index, value) in outputs.into_iter().enumerate() {
        let port = module
            .add_port(
                format!("y{index}"),
                PortDirection::Output,
                WordType::bits(16).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(port).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();
    }
    let mut decisions = ArchitectureDecisions::for_module(&module).unwrap();
    assert_eq!(decisions.operators().len(), 3);
    let target = target();
    decisions.select_for_budgets(&target, |_| None).unwrap();
    let (quality, budgets) = project(
        &mut module,
        &decisions,
        &[
            (outputs[0], Some(40.0)),
            (outputs[1], Some(40.0)),
            (outputs[2], None),
        ],
    );
    assert!(quality.0 > 0);
    assert!(budgets[0].1.unwrap() >= 32.0);
    assert!(budgets[1].1.unwrap() < 10.0);
    assert_eq!(budgets[2].1, None);
    assert!(
        decisions
            .select_for_budgets(&target, |operator| budgets[operator.id().raw() as usize].1)
            .unwrap()
    );
    let recipes = decisions
        .operators()
        .iter()
        .map(|operator| {
            decisions
                .candidate_recipe_name(decisions.selected_candidate(operator.id()).unwrap().id())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(recipes, ["ripple-carry", "kogge-stone", "ripple-carry"]);
}

#[test]
fn exhausted_path_budgets_still_distinguish_faster_recipes() {
    let target = target();
    let estimate = |logic_depth| crate::planning::provider::StructuralEstimate {
        logic_depth,
        logic_units: 100,
        wiring_units: 16,
    };
    for budget in [0.0, -1.0, -32.0] {
        let fast = target.score_for_budget(estimate(6), Some(budget)).unwrap();
        let slow = target.score_for_budget(estimate(32), Some(budget)).unwrap();
        assert!(fast < slow, "budget={budget}, fast={fast:?}, slow={slow:?}");
        assert!(slow.0 < u64::MAX);
    }
}

fn project(
    module: &mut WordModule,
    decisions: &ArchitectureDecisions,
    roots: &[(word::ValueId, Option<f64>)],
) -> (PathQuality, OperatorBudgets) {
    let sources = decisions
        .operators()
        .iter()
        .map(|operator| decisions.source_operations(operator.id()).into())
        .collect::<Vec<Box<[word::OpId]>>>();
    let operators = DurableOperatorArena::capture(module, decisions, &sources, |operation| {
        let mut anchor = [0; 32];
        anchor[..4].copy_from_slice(&operation.raw().to_le_bytes());
        Ok(crate::OperationAnchorId::from_bytes_for_test(anchor))
    })
    .unwrap();
    let tracked = decisions
        .operators()
        .iter()
        .flat_map(|&operator| {
            decisions
                .operator_inputs(operator)
                .chain(std::iter::once(operator.result()))
        })
        .collect::<Vec<_>>();
    let checkpoint = module.speculation_checkpoint();
    let original = (module.values().len(), module.operations().len());
    let mut provenance = ProvenanceBuilder::for_regional_candidate(module);
    let lowering = lower_local_region_boolean(
        module,
        LocalRegionBooleanRequest {
            plan: decisions,
            operators: &operators,
            provenance: &mut provenance,
            owner: crate::RegionRowId::from_index(0).unwrap(),
            boundary_inputs: &[],
            roots: &roots.iter().map(|&(value, _)| value).collect::<Vec<_>>(),
            tracked_values: &tracked,
        },
    )
    .unwrap();
    let roots = roots
        .iter()
        .map(|&(value, required_time)| {
            (
                MappingRoot {
                    value,
                    required_time,
                    output_load: None,
                    requires_combinational_cover: true,
                },
                value,
            )
        })
        .collect::<Vec<_>>();
    let slice = RegionLogicSlice::build_candidate(
        crate::RegionAnchorId::from_bytes_for_test([1; 32]),
        [0; 32],
        RegionLogicDomain {
            module,
            subject_inputs: &lowering.subject.inputs,
            source_to_local: &BTreeMap::new(),
            ownership: &lowering.ownership,
            contracts: &[],
            roots: &roots,
        },
    )
    .unwrap();
    let (quality, budgets) = measure_paths(&lowering, &slice, decisions, 1.0)
        .unwrap()
        .unwrap();
    module.rollback_speculation(checkpoint).unwrap();
    assert_eq!((module.values().len(), module.operations().len()), original);
    (quality, budgets)
}

#[test]
fn chained_operators_share_the_available_path_time() {
    let mut module = WordModule::new("chained_operators");
    let [a, b, c] = ["a", "b", "c"].map(|name| input(&mut module, name));
    let first = module
        .binary(BinaryOp::Add, a, b, SourceSpan::default())
        .unwrap();
    let second = module
        .binary(BinaryOp::Add, first, c, SourceSpan::default())
        .unwrap();
    for (name, value) in [("middle", first), ("result", second)] {
        let port = module
            .add_port(
                name,
                PortDirection::Output,
                WordType::bits(16).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(port).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();
    }
    let mut decisions = ArchitectureDecisions::for_module(&module).unwrap();
    assert_eq!(decisions.operators().len(), 2);
    let target = target();
    decisions.select_for_budgets(&target, |_| None).unwrap();
    let (feasible, margins) = project(
        &mut module,
        &decisions,
        &[(first, None), (second, Some(40.0))],
    );
    assert_eq!(feasible.0, 0);
    assert!(margins.iter().all(|&(_, budget)| budget.unwrap() >= 32.0));
    assert!(
        !decisions
            .select_for_budgets(&target, |operator| margins[operator.id().raw() as usize].1)
            .unwrap()
    );
    let (quality, budgets) = project(
        &mut module,
        &decisions,
        &[(first, None), (second, Some(25.0))],
    );
    assert!(quality.0 > 0);
    assert!(
        budgets
            .iter()
            .all(|&(_, budget)| budget.is_some_and(|budget| budget < 32.0))
    );
    decisions
        .select_for_budgets(&target, |operator| budgets[operator.id().raw() as usize].1)
        .unwrap();
    for operator in decisions.operators() {
        assert_ne!(
            decisions
                .candidate_recipe_name(decisions.selected_candidate(operator.id()).unwrap().id()),
            Some("ripple-carry")
        );
    }
    let (improved, _) = project(
        &mut module,
        &decisions,
        &[(first, None), (second, Some(25.0))],
    );
    assert!(improved.0 < quality.0);
}
