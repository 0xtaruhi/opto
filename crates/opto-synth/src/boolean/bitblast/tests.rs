// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod multiplier;

fn bitblast_area(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(module)?;
    let mut provenance = crate::artifact::provenance::ProvenanceBuilder::new(module, &plan)?;
    bitblast_module_with_plan(module, &plan, &mut provenance)
}

fn select_recipe(plan: &mut crate::planning::operator::ArchitectureDecisions, recipe: &str) {
    let operator = plan.operators()[0].id();
    let candidate = plan
        .candidates(operator)
        .iter()
        .find(|candidate| plan.candidate_recipe_name(candidate.id()) == Some(recipe))
        .unwrap()
        .id();
    plan.select_candidate(candidate).unwrap();
}

mod arithmetic;
mod basic_multiply;
mod division;
mod dynamic;
mod operations;

#[test]
fn resolves_synthesis_x_constants_deterministically() {
    let mut module = word::WordModule::new("top");
    let ty = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::located("x_constant.sv", Some(7), Some(11), "constant");
    let value = module
        .constant(ConstBits::from_bin_str("x").unwrap(), ty, source)
        .unwrap();
    let output = add_output(&mut module, "y", 1, value);

    bitblast_area(&mut module).unwrap();

    let connect = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == output)
        .unwrap();
    let word::ValueKind::Constant(bits) = &module.value(connect.value).unwrap().kind else {
        panic!("resolved X must be a constant");
    };
    assert_eq!(bits.bit_lsb(0), Some(BitVal::Zero));
}

#[test]
fn rejects_tri_state_constants_during_bitblast() {
    let mut module = word::WordModule::new("top");
    let ty = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::located("z_constant.sv", Some(9), Some(13), "constant");
    let value = module
        .constant(ConstBits::from_bin_str("z").unwrap(), ty, source)
        .unwrap();
    add_output(&mut module, "y", 1, value);

    let error = bitblast_area(&mut module).unwrap_err();

    assert!(error.to_string().contains("tri-state constant"));
    assert!(error.to_string().contains("z_constant.sv"));
}

#[test]
fn regional_shell_cuts_owned_combinational_cones_without_rewriting_source_values() {
    let mut module = word::WordModule::new("regional_shell");
    let input = add_input(&mut module, "a", 4);
    let input = read_port(&mut module, input);
    let result = module
        .unary(word::UnaryOp::BitNot, input, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 4, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();

    let ownership = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[Some(region)],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    assert_eq!(ownership.lowered_bits(result).unwrap().len(), 4);
    assert!(matches!(
        module.value(result).unwrap().kind,
        word::ValueKind::Operation(_)
    ));
    module.compact_netlist().unwrap();
    module.validate().unwrap();
    assert!(module.operations().is_empty());
    assert_eq!(module.connects().len(), 4);
    assert!(module.connects().iter().all(|connect| matches!(
        module.value(connect.value).unwrap().kind,
        word::ValueKind::Signal(_)
    )));
}

fn wrapping_add(left: u64, right: u64) -> u64 {
    left.wrapping_add(right)
}

fn wrapping_sub(left: u64, right: u64) -> u64 {
    left.wrapping_sub(right)
}

fn signed_value(value: u64, width: u32) -> i64 {
    let shift = u64::BITS - width;
    (value << shift).cast_signed() >> shift
}

fn binary_module(
    op: word::BinaryOp,
    left_width: u32,
    right_width: u32,
    signed: bool,
) -> (
    word::WordModule,
    word::SignalId,
    word::SignalId,
    word::SignalId,
) {
    let mut module = word::WordModule::new("top");
    let state = word::LogicStateKind::FourState;
    let left_ty = word::WordType::new(left_width, signed, state).unwrap();
    let right_ty = word::WordType::new(right_width, signed, state).unwrap();
    let a = module
        .add_port(
            "a",
            word::PortDirection::Input,
            left_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let b = module
        .add_port(
            "b",
            word::PortDirection::Input,
            right_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let left = module
        .read_signal(module.port(a).unwrap().signal, word::SourceSpan::default())
        .unwrap();
    let right = module
        .read_signal(module.port(b).unwrap().signal, word::SourceSpan::default())
        .unwrap();
    let result = module
        .binary(op, left, right, word::SourceSpan::default())
        .unwrap();
    let result_ty = module.value(result).unwrap().ty;
    let y = module
        .add_port(
            "y",
            word::PortDirection::Output,
            result_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let y_signal = module.port(y).unwrap().signal;
    module
        .connect(
            word::LValue::signal(y_signal),
            result,
            word::SourceSpan::default(),
        )
        .unwrap();
    let a_signal = module_signal(&module, a);
    let b_signal = module_signal(&module, b);
    (module, a_signal, b_signal, y_signal)
}

fn add_input(module: &mut word::WordModule, name: &str, width: u32) -> word::PortId {
    module
        .add_port(
            name,
            word::PortDirection::Input,
            word::WordType::bits(width).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap()
}

fn add_output(
    module: &mut word::WordModule,
    name: &str,
    width: u32,
    value: word::ValueId,
) -> word::SignalId {
    let port = module
        .add_port(
            name,
            word::PortDirection::Output,
            word::WordType::bits(width).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let signal = module_signal(module, port);
    module
        .connect(
            word::LValue::signal(signal),
            value,
            word::SourceSpan::default(),
        )
        .unwrap();
    signal
}

fn read_port(module: &mut word::WordModule, port: word::PortId) -> word::ValueId {
    module
        .read_signal(module_signal(module, port), word::SourceSpan::default())
        .unwrap()
}

fn module_signal(module: &word::WordModule, port: word::PortId) -> word::SignalId {
    module.port(port).unwrap().signal
}

fn evaluate_output(
    module: &word::WordModule,
    output: word::SignalId,
    inputs: &[(word::SignalId, u64)],
) -> u64 {
    let mut result = 0u64;
    let mut memo = vec![None; module.values().len()];
    for connect in module
        .connects()
        .iter()
        .filter(|connect| connect.target.signal == output)
    {
        let bit = connect.target.range.map_or(0, |range| {
            assert_eq!(range.msb, range.lsb);
            range.lsb
        });
        if evaluate_value(module, connect.value, inputs, &mut memo) {
            result |= 1u64 << bit;
        }
    }
    result
}

fn evaluate_value(
    module: &word::WordModule,
    value_id: word::ValueId,
    inputs: &[(word::SignalId, u64)],
    memo: &mut [Option<bool>],
) -> bool {
    if let Some(value) = memo[value_id.index()] {
        return value;
    }
    let value = module.value(value_id).unwrap();
    assert_eq!(value.ty.width(), 1);
    let result = match &value.kind {
        word::ValueKind::Signal(reference) => {
            assert_eq!(reference.width(), 1);
            let value = inputs
                .iter()
                .find(|(signal, _)| *signal == reference.signal)
                .unwrap()
                .1;
            ((value >> reference.lsb) & 1) != 0
        }
        word::ValueKind::Constant(bits) => match bits.bit_lsb(0).unwrap() {
            BitVal::Zero => false,
            BitVal::One => true,
            BitVal::X | BitVal::Z => panic!("test evaluator received unknown constant"),
        },
        word::ValueKind::Operation(operation_id) => {
            let operation = module.operation(*operation_id).unwrap();
            match &operation.kind {
                word::OpKind::Unary { op, arg } => match op {
                    word::UnaryOp::LogicalNot | word::UnaryOp::BitNot => {
                        !evaluate_value(module, *arg, inputs, memo)
                    }
                    word::UnaryOp::ReductionAnd
                    | word::UnaryOp::ReductionOr
                    | word::UnaryOp::ReductionXor => evaluate_value(module, *arg, inputs, memo),
                },
                word::OpKind::Binary { op, left, right } => {
                    let left = evaluate_value(module, *left, inputs, memo);
                    let right = evaluate_value(module, *right, inputs, memo);
                    match op {
                        word::BinaryOp::BitAnd | word::BinaryOp::LogicalAnd => left & right,
                        word::BinaryOp::BitOr | word::BinaryOp::LogicalOr => left | right,
                        word::BinaryOp::BitXor | word::BinaryOp::Ne => left ^ right,
                        word::BinaryOp::Eq => left == right,
                        _ => panic!("non-scalarized binary op {op:?}"),
                    }
                }
                word::OpKind::Mux {
                    cond,
                    then_value,
                    else_value,
                } => {
                    if evaluate_value(module, *cond, inputs, memo) {
                        evaluate_value(module, *then_value, inputs, memo)
                    } else {
                        evaluate_value(module, *else_value, inputs, memo)
                    }
                }
                word::OpKind::Cast { value, .. } => evaluate_value(module, *value, inputs, memo),
                kind => panic!("non-scalarized operation {kind:?}"),
            }
        }
    };
    memo[value_id.index()] = Some(result);
    result
}
