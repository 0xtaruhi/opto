// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::word::{self, BinaryOp, PortDirection, SourceSpan, WordModule, WordType};

fn binary_fragment(
    name: &str,
    operator: BinaryOp,
) -> (WordModule, [word::ValueId; 2], word::ValueId) {
    let mut module = WordModule::new(name);
    let bit = WordType::bits(1).unwrap();
    let inputs = ["a", "b"].map(|name| {
        let port = module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let output = module
        .binary(operator, inputs[0], inputs[1], SourceSpan::default())
        .unwrap();
    (module, inputs, output)
}

#[test]
fn cross_module_miter_shares_only_the_explicit_stable_cut() {
    let (reference, reference_inputs, reference_output) =
        binary_fragment("reference", BinaryOp::BitXor);
    let (equivalent, equivalent_inputs, equivalent_output) =
        binary_fragment("equivalent", BinaryOp::BitXor);
    let (wrong, wrong_inputs, wrong_output) = binary_fragment("wrong", BinaryOp::BitAnd);

    prove_module_values_equivalent_at_cut(
        &reference,
        &[reference_output],
        &equivalent,
        &[equivalent_output],
        &reference_inputs
            .into_iter()
            .zip(equivalent_inputs)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .require_proved()
    .unwrap();
    assert!(matches!(
        prove_module_values_equivalent_at_cut(
            &reference,
            &[reference_output],
            &wrong,
            &[wrong_output],
            &reference_inputs
                .into_iter()
                .zip(wrong_inputs)
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn proves_equivalent_xor_structures_and_rejects_wrong_logic() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let ports = ["a", "b"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap()
    });
    let [a, b] = ports.map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let reference = module
        .binary(BinaryOp::BitXor, a, b, SourceSpan::default())
        .unwrap();
    let not_a = module
        .unary(word::UnaryOp::BitNot, a, SourceSpan::default())
        .unwrap();
    let not_b = module
        .unary(word::UnaryOp::BitNot, b, SourceSpan::default())
        .unwrap();
    let left = module
        .binary(BinaryOp::BitAnd, not_a, b, SourceSpan::default())
        .unwrap();
    let right = module
        .binary(BinaryOp::BitAnd, a, not_b, SourceSpan::default())
        .unwrap();
    let equivalent = module
        .binary(BinaryOp::BitOr, left, right, SourceSpan::default())
        .unwrap();
    let wrong = module
        .binary(BinaryOp::BitAnd, a, b, SourceSpan::default())
        .unwrap();

    assert!(
        prove_value_bits(&module, reference, &[equivalent])
            .unwrap()
            .require_proved()
            .is_ok()
    );
    assert!(matches!(
        prove_value_bits(&module, reference, &[wrong]).unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn operand_cuts_exclude_the_upstream_cone_from_the_miter() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let ports = ["a", "b"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap()
    });
    let [a, b] = ports.map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    // A deep chain drives the operand; a correct cut proof must not
    // encode it.
    let mut chained = a;
    for _ in 0..64 {
        chained = module
            .binary(BinaryOp::BitXor, chained, b, SourceSpan::default())
            .unwrap();
    }
    let reference = module
        .binary(BinaryOp::BitXor, chained, b, SourceSpan::default())
        .unwrap();
    let not_chained = module
        .unary(word::UnaryOp::BitNot, chained, SourceSpan::default())
        .unwrap();
    let not_b = module
        .unary(word::UnaryOp::BitNot, b, SourceSpan::default())
        .unwrap();
    let left = module
        .binary(BinaryOp::BitAnd, not_chained, b, SourceSpan::default())
        .unwrap();
    let right = module
        .binary(BinaryOp::BitAnd, chained, not_b, SourceSpan::default())
        .unwrap();
    let equivalent = module
        .binary(BinaryOp::BitOr, left, right, SourceSpan::default())
        .unwrap();
    let wrong = module
        .binary(BinaryOp::BitAnd, chained, b, SourceSpan::default())
        .unwrap();
    let cuts = vec![(chained, vec![chained]), (b, vec![b])];

    let proof = prove_value_bits_at_cut(&module, reference, &[equivalent], &cuts)
        .unwrap()
        .require_proved()
        .unwrap();
    let full = prove_value_bits(&module, reference, &[equivalent])
        .unwrap()
        .require_proved()
        .unwrap();
    assert!(
        proof.encoded_values < 16,
        "cut miter still encoded {} values",
        proof.encoded_values
    );
    assert!(proof.encoded_values < full.encoded_values);
    assert!(matches!(
        prove_value_bits_at_cut(&module, reference, &[wrong], &cuts).unwrap(),
        ProofOutcome::Disproved(_)
    ));

    let mismatched = vec![(chained, vec![chained, b])];
    assert!(
        prove_value_bits_at_cut(&module, reference, &[equivalent], &mismatched)
            .unwrap_err()
            .to_string()
            .contains("width mismatch")
    );
}

#[test]
fn word_assumptions_constrain_conditional_equivalence() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let ports = ["a", "b"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap()
    });
    let [a, b] = ports.map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let not_b = module
        .unary(word::UnaryOp::BitNot, b, SourceSpan::default())
        .unwrap();

    assert!(
        prove_value_equivalence_under_assumptions(&module, a, b, &[(a, b)])
            .unwrap()
            .require_proved()
            .is_ok()
    );
    assert!(matches!(
        prove_value_equivalence_under_assumptions(&module, a, not_b, &[(a, b)]).unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn relational_state_proofs_share_environment_inputs() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let ports = ["state", "input"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap()
    });
    let [state, input] = ports.map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let inverted = module
        .unary(word::UnaryOp::BitNot, input, SourceSpan::default())
        .unwrap();
    let observation = module
        .mux(state, input, inverted, SourceSpan::default())
        .unwrap();
    let zero = opto_ir::ConstBits::from_bin_str("0").unwrap();
    let one = opto_ir::ConstBits::from_bin_str("1").unwrap();
    let state_signal = module.port(ports[0]).unwrap().signal;

    assert!(matches!(
        prove_values_equivalent_between_signal_states(
            &module,
            &[observation],
            state_signal,
            &zero,
            &one,
        )
        .unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn finite_transition_relation_leaves_inputs_free() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let state_type = WordType::bits(2).unwrap();
    let state_port = module
        .add_port(
            "state",
            PortDirection::Input,
            state_type,
            SourceSpan::default(),
        )
        .unwrap();
    let advance_port = module
        .add_port("advance", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let state = module
        .read_signal(
            module.port(state_port).unwrap().signal,
            SourceSpan::default(),
        )
        .unwrap();
    let advance = module
        .read_signal(
            module.port(advance_port).unwrap().signal,
            SourceSpan::default(),
        )
        .unwrap();
    let one = module
        .constant(
            opto_ir::ConstBits::from_bin_str("01").unwrap(),
            state_type,
            SourceSpan::default(),
        )
        .unwrap();
    let next = module
        .mux(advance, one, state, SourceSpan::default())
        .unwrap();
    let states = ["00", "01", "10"].map(|state| opto_ir::ConstBits::from_bin_str(state).unwrap());

    let relation = enumerate_finite_transitions(&module, state, next, &states).unwrap();

    assert_eq!(relation.successors(0), Some([0, 1].as_slice()));
    assert_eq!(relation.successors(1), Some([1].as_slice()));
    assert_eq!(relation.successors(2), Some([1, 2].as_slice()));
    assert!(relation.report().encoded_values > 0);
}

#[test]
fn constant_proof_does_not_follow_unmodeled_signal_connections() {
    let mut module = WordModule::new("proof");
    let ty = WordType::bits(2).unwrap();
    let signal = module.add_wire("alias", ty, SourceSpan::default()).unwrap();
    let constant = opto_ir::ConstBits::from_bin_str("10").unwrap();
    let value = module
        .constant(constant.clone(), ty, SourceSpan::default())
        .unwrap();
    module
        .connect(word::LValue::signal(signal), value, SourceSpan::default())
        .unwrap();
    let alias = module.read_signal(signal, SourceSpan::default()).unwrap();

    assert!(
        prove_value_constant(&module, value, &constant)
            .unwrap()
            .require_proved()
            .is_ok()
    );
    assert!(matches!(
        prove_value_constant(&module, alias, &constant).unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn detached_logic_is_proved_before_word_ir_commit() {
    let mut module = WordModule::new("proof");
    let bit = WordType::bits(1).unwrap();
    let ports = ["a", "b"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit, SourceSpan::default())
            .unwrap()
    });
    let [a, b] = ports.map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    });
    let reference = module
        .binary(BinaryOp::BitXor, a, b, SourceSpan::default())
        .unwrap();

    let mut logic = opto_ir::logic::LogicBuilder::new();
    let logic_a = logic.input(0).unwrap();
    let logic_b = logic.input(1).unwrap();
    let equivalent = logic.xor(logic_a, logic_b, 0).unwrap();
    let wrong = logic.and(logic_a, logic_b, 0).unwrap();
    let logic = logic.freeze();

    assert!(
        prove_value_against_logic_at_cut(&module, reference, &logic, &[equivalent], &[a, b])
            .unwrap()
            .require_proved()
            .is_ok()
    );
    assert!(matches!(
        prove_value_against_logic_at_cut(&module, reference, &logic, &[wrong], &[a, b]).unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn cut_miter_proves_mixed_logic_and_rejects_counterexamples() {
    let mut reference = opto_ir::logic::LogicBuilder::new();
    let a = reference.input(10).unwrap();
    let b = reference.input(20).unwrap();
    let reference_output = reference.xor(a, b, 0).unwrap();
    let reference = reference.freeze();

    let mut implementation = opto_ir::logic::LogicBuilder::new();
    let a = implementation.input(10).unwrap();
    let b = implementation.input(20).unwrap();
    let left = implementation.and(a, b.inverted(), 0).unwrap();
    let right = implementation.and(a.inverted(), b, 0).unwrap();
    let equivalent = implementation.or(left, right, 0).unwrap();
    let wrong = implementation.and(a, b, 0).unwrap();
    let implementation = implementation.freeze();

    let proof = prove_logic_network_equivalence(
        &reference,
        &[reference_output],
        &implementation,
        &[equivalent],
    )
    .unwrap()
    .require_proved()
    .unwrap();
    assert!(proof.clauses > 0);
    assert!(matches!(
        prove_logic_network_equivalence(
            &reference,
            &[reference_output],
            &implementation,
            &[wrong],
        )
        .unwrap(),
        ProofOutcome::Disproved(_)
    ));
}

#[test]
fn cut_miter_uses_the_union_of_boundary_inputs() {
    let mut reference = opto_ir::logic::LogicBuilder::new();
    let extra = reference.input(1).unwrap();
    let common = reference.input(2).unwrap();
    let mixed = reference.xor(extra, common, 0).unwrap();
    let reference_output = reference.xor(mixed, extra, 0).unwrap();
    let reference = reference.freeze();

    let mut implementation = opto_ir::logic::LogicBuilder::new();
    let implementation_output = implementation.input(2).unwrap();
    let implementation = implementation.freeze();

    prove_logic_network_equivalence(
        &reference,
        &[reference_output],
        &implementation,
        &[implementation_output],
    )
    .unwrap()
    .require_proved()
    .unwrap();
}

#[test]
fn partitions_simulation_candidates_with_incremental_sat() {
    let mut builder = opto_ir::logic::LogicBuilder::new();
    let a = builder.input(1).unwrap();
    let b = builder.input(2).unwrap();
    let xor = builder.xor(a, b, 0).unwrap();
    let left = builder.and(a, b.inverted(), 0).unwrap();
    let right = builder.and(a.inverted(), b, 0).unwrap();
    let sop = builder.or(left, right, 0).unwrap();
    let and = builder.and(a, b, 0).unwrap();
    let demorgan_and = builder
        .or(a.inverted(), b.inverted(), 0)
        .unwrap()
        .inverted();
    let network = builder.freeze();

    assert_eq!(
        prove_logic_literal_partitions(
            &network,
            &[vec![xor, and, sop, demorgan_and]],
            4,
            64,
            usize::MAX,
            &mut Vec::new()
        )
        .unwrap(),
        Some(vec![vec![None, None, Some(0), Some(1)]])
    );
}

#[test]
fn literal_partition_proof_skips_an_oversized_encoding() {
    let mut builder = opto_ir::logic::LogicBuilder::new();
    let a = builder.input(1).unwrap();
    let b = builder.input(2).unwrap();
    let left = builder.and(a, b, 0).unwrap();
    let right = builder
        .or(a.inverted(), b.inverted(), 0)
        .unwrap()
        .inverted();
    let network = builder.freeze();

    assert_eq!(
        prove_logic_literal_partitions(&network, &[vec![left, right]], 2, 2, 1, &mut Vec::new(),)
            .unwrap(),
        None
    );
}

#[test]
fn cut_miter_finds_a_counterexample_across_different_boundary_inputs() {
    let mut reference = opto_ir::logic::LogicBuilder::new();
    let reference_output = reference.input(1).unwrap();
    let reference = reference.freeze();
    let mut implementation = opto_ir::logic::LogicBuilder::new();
    let implementation_output = implementation.input(2).unwrap();
    let implementation = implementation.freeze();

    let outcome = prove_logic_network_equivalence(
        &reference,
        &[reference_output],
        &implementation,
        &[implementation_output],
    )
    .unwrap();
    assert!(matches!(outcome, ProofOutcome::Disproved(_)));
}
