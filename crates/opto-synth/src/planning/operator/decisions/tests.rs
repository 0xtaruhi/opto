// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::OperatorKind;
use opto_ir::word::{LValue, LogicStateKind, PortDirection, SourceSpan, WordModule, WordType};

#[test]
fn shares_catalog_and_selects_candidates_by_compact_id() {
    let mut module = WordModule::new("top");
    let ty = WordType::new(4, false, LogicStateKind::FourState).unwrap();
    let input = module
        .add_port("a", PortDirection::Input, ty, SourceSpan::default())
        .unwrap();
    let input = module
        .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
        .unwrap();
    module
        .binary(word::BinaryOp::BitAnd, input, input, SourceSpan::default())
        .unwrap();
    let add = module
        .binary(word::BinaryOp::Add, input, input, SourceSpan::default())
        .unwrap();
    let subtract = module
        .binary(word::BinaryOp::Sub, input, input, SourceSpan::default())
        .unwrap();
    for (name, value) in [("add_y", add), ("sub_y", subtract)] {
        let output = module
            .add_port(name, PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();
    }

    let mut plan = ArchitectureDecisions::for_module(&module).unwrap();
    let cloned = plan.clone();

    assert!(Arc::ptr_eq(&plan.catalog, &cloned.catalog));
    assert!(plan.selections.is_empty());
    assert_eq!(std::mem::size_of::<ImplementationCandidateId>(), 4);
    assert_eq!(plan.operators().len(), 2);
    assert_eq!(
        plan.operators()[0].source_operation(),
        word::OpId::from_index(1).unwrap()
    );
    assert_eq!(plan.operators()[0].kind(), OperatorKind::Add);
    assert_eq!(
        plan.operators()[1].source_operation(),
        word::OpId::from_index(2).unwrap()
    );
    assert_eq!(plan.candidates(plan.operators()[0].id()).len(), 4);
    assert_eq!(
        plan.candidate_implementation_name(plan.candidates(plan.operators()[0].id())[0].id()),
        Some("rpl")
    );
    assert_eq!(
        plan.candidate_module_name(plan.candidates(plan.operators()[0].id())[0].id()),
        Some("DW01_add")
    );
    assert_eq!(
        plan.candidate_operation_mnemonic(plan.candidates(plan.operators()[0].id())[0].id()),
        Some("add")
    );
    let kogge_stone = plan
        .candidates(plan.operators()[1].id())
        .iter()
        .find(|candidate| plan.candidate_recipe_name(candidate.id()) == Some("kogge-stone"))
        .unwrap()
        .id();
    plan.select_candidate(kogge_stone).unwrap();
    assert_eq!(plan.selections.len(), 1);
    assert_eq!(
        plan.candidate_recipe_name(
            plan.selected_candidate(plan.operators()[1].id())
                .unwrap()
                .id()
        ),
        Some("kogge-stone")
    );
    assert_eq!(plan.candidate_implementation_name(kogge_stone), Some("cla"));
    let default = plan.candidates(plan.operators()[1].id())[0].id();
    plan.select_candidate(default).unwrap();
    assert!(plan.selections.is_empty());
}

#[test]
fn provider_enumerates_recipes_from_the_operator_signature() {
    fn recipes(width: u32) -> Vec<String> {
        let mut module = WordModule::new("top");
        let ty = WordType::new(width, false, LogicStateKind::FourState).unwrap();
        let input = module
            .add_port("a", PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let add = module
            .binary(word::BinaryOp::Add, input, input, SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                add,
                SourceSpan::default(),
            )
            .unwrap();
        let plan = ArchitectureDecisions::for_module(&module).unwrap();
        let operator = plan.operators()[0].id();
        plan.candidates(operator)
            .iter()
            .map(|candidate| {
                plan.candidate_recipe_name(candidate.id())
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    assert_eq!(recipes(1), ["ripple-carry"]);
    assert_eq!(recipes(2), ["ripple-carry", "brent-kung"]);
    assert_eq!(recipes(3), ["ripple-carry", "brent-kung", "kogge-stone"]);
    assert_eq!(
        recipes(4),
        [
            "ripple-carry",
            "brent-kung",
            "kogge-stone",
            "hybrid-brent-kung-balanced"
        ]
    );
    assert_eq!(
        recipes(5),
        [
            "ripple-carry",
            "brent-kung",
            "kogge-stone",
            "hybrid-brent-kung-balanced",
            "hybrid-brent-kung-area"
        ]
    );
}

#[test]
fn multiplication_enumerates_every_architecture_candidate() {
    fn recipes() -> Vec<String> {
        let mut module = WordModule::new("top");
        let ty = WordType::new(4, false, LogicStateKind::FourState).unwrap();
        let left = module
            .add_port("left", PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        let right = module
            .add_port("right", PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        let left = module
            .read_signal(module.port(left).unwrap().signal, SourceSpan::default())
            .unwrap();
        let right = module
            .read_signal(module.port(right).unwrap().signal, SourceSpan::default())
            .unwrap();
        let product = module
            .binary(word::BinaryOp::Mul, left, right, SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                product,
                SourceSpan::default(),
            )
            .unwrap();
        let plan = ArchitectureDecisions::for_module(&module).unwrap();
        let operator = plan.operators()[0].id();
        plan.candidates(operator)
            .iter()
            .map(|candidate| {
                plan.candidate_recipe_name(candidate.id())
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    assert_eq!(recipes(), ["radix4-wallace", "array-wallace"]);
}

#[test]
fn division_and_modulo_each_have_one_canonical_candidate() {
    for (binary, kind, module_name, mnemonic) in [
        (word::BinaryOp::Div, OperatorKind::Divide, "DW_div", "div"),
        (word::BinaryOp::Mod, OperatorKind::Modulo, "DW_mod", "mod"),
    ] {
        let mut module = WordModule::new("division");
        let ty = WordType::bits(8).unwrap();
        let operands = ["a", "b"].map(|name| {
            let port = module
                .add_port(name, PortDirection::Input, ty, SourceSpan::default())
                .unwrap();
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        });
        let result = module
            .binary(binary, operands[0], operands[1], SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                result,
                SourceSpan::default(),
            )
            .unwrap();
        let plan = ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators().len(), 1);
        let operator = plan.operators()[0];
        assert_eq!(operator.kind(), kind);
        let candidates = plan.candidates(operator.id());
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            plan.candidate_module_name(candidates[0].id()),
            Some(module_name)
        );
        assert_eq!(
            plan.candidate_operation_mnemonic(candidates[0].id()),
            Some(mnemonic)
        );
    }
}

#[test]
fn multi_operand_fusion_exists_only_in_private_region_catalogs() {
    let mut module = WordModule::new("private_sum");
    let ty = WordType::bits(8).unwrap();
    let inputs = ["a", "b", "c", "d"].map(|name| {
        let port = module
            .add_port(name, PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let sum = inputs[1..]
        .iter()
        .try_fold(inputs[0], |sum, &input| {
            module.binary(word::BinaryOp::Add, sum, input, SourceSpan::default())
        })
        .unwrap();
    let output = module
        .add_port("y", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(output).unwrap().signal),
            sum,
            SourceSpan::default(),
        )
        .unwrap();

    let shell = ArchitectureDecisions::for_regional_shell(&module);
    assert!(shell.operators().is_empty());

    let private = ArchitectureDecisions::for_private_region(
        &module,
        &[],
        crate::boolean::bitblast::implementation_providers().into(),
    )
    .unwrap();
    assert_eq!(private.operators().len(), 1);
    let operator = private.operators()[0];
    assert_eq!(operator.kind(), OperatorKind::Sum);
    assert_eq!(operator.term_count(), 4);
    let recipes = private
        .candidates(operator.id())
        .iter()
        .map(|candidate| private.candidate_recipe_name(candidate.id()).unwrap())
        .collect::<Vec<_>>();
    assert!(
        recipes
            .iter()
            .any(|recipe| recipe.starts_with("wallace-csa"))
    );
    assert!(recipes.iter().any(|recipe| recipe.starts_with("dadda-csa")));
}

#[test]
fn explicit_region_root_is_not_absorbed_into_its_arithmetic_consumer() {
    let mut module = WordModule::new("published_intermediate");
    let ty = WordType::bits(8).unwrap();
    let inputs = ["a", "b", "c"].map(|name| {
        let port = module
            .add_port(name, PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let published = module
        .binary(
            word::BinaryOp::Sub,
            inputs[0],
            inputs[1],
            SourceSpan::default(),
        )
        .unwrap();
    let root = module
        .binary(
            word::BinaryOp::Add,
            published,
            inputs[2],
            SourceSpan::default(),
        )
        .unwrap();

    let private = ArchitectureDecisions::for_private_region(
        &module,
        &[published, root],
        crate::boolean::bitblast::implementation_providers().into(),
    )
    .unwrap();

    assert_eq!(private.operators().len(), 2);
    assert!([published, root].into_iter().all(|value| {
        private
            .operators()
            .iter()
            .any(|operator| operator.result() == value)
    }));
}
