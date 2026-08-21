// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

/// Expands enables left after clock gating and enabled-cell selection.
pub(crate) fn expand_unsupported_enables(
    module: &mut word::WordModule,
    sequential_catalog: &super::SequentialCellCatalog,
    state_feedback: &std::collections::BTreeMap<word::OpId, word::ValueId>,
) -> Result<(), crate::SynthError> {
    let mut candidates = Vec::new();
    for index in 0..module.operations().len() {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let model = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "enable candidate references unknown {operation:?}"
            ))
        })?;
        let word::OpKind::Register(register) = &model.kind else {
            continue;
        };
        let Some(enable) = register.enable else {
            continue;
        };
        let result = model.result;
        let has_enable_cell = uniform_async_reset_requests(module, &register.resets)?
            .is_some_and(|requests| sequential_catalog.has_enable_cell(register.edge, &requests));
        if has_enable_cell {
            continue;
        }
        candidates.push((
            operation,
            register.clone(),
            enable,
            result,
            model.source.clone(),
        ));
    }
    for (operation, mut register, enable, result, source) in candidates {
        let held = state_feedback.get(&operation).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "private enabled state {result:?} has no exact feedback boundary"
            ))
        })?;
        let (then_value, else_value) = if enable.active_high {
            (register.d, held)
        } else {
            (held, register.d)
        };
        register.d = module
            .mux(enable.value, then_value, else_value, source.clone())
            .map_err(crate::SynthError::from)?;
        register.enable = None;
        module
            .operation_mut(operation)
            .expect("candidate register remains present")
            .kind = word::OpKind::Register(register);
    }
    Ok(())
}

/// Rewrites retained enable controls to the polarity of the selected target
/// state cell so the ordinary Boolean mapper owns every required inverter.
pub(crate) fn normalize_enable_polarities(
    module: &mut word::WordModule,
    sequential_catalog: &super::SequentialCellCatalog,
    combinational_catalog: &crate::mapping::library::CombinationalCellCatalog,
) -> Result<(), crate::SynthError> {
    let mut rewrites = Vec::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let operation_id = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let (enable, selected_active_high) = match &operation.kind {
            word::OpKind::Register(register) => {
                let Some(enable) = register.enable else {
                    continue;
                };
                let super::SelectedRegisterCell::Enabled(cell) =
                    sequential_catalog.select_register(module, register, combinational_catalog)?
                else {
                    return Err(crate::SynthError::invariant(
                        "retained register enable selected a simple DFF",
                    ));
                };
                (enable, cell.enable_active_high())
            }
            word::OpKind::Latch(latch) => {
                let requests = super::async_reset_requests(module, &latch.resets)?;
                let cell = sequential_catalog
                    .best_latch(
                        &requests,
                        latch.enable.active_high,
                        false,
                        super::enable_inverter_cost(
                            module,
                            latch.enable.value,
                            combinational_catalog,
                        ),
                    )
                    .ok_or_else(|| {
                        crate::SynthError::mapping("target library has no compatible latch")
                    })?;
                (latch.enable, cell.enable_active_high())
            }
            _ => continue,
        };
        if enable.active_high != selected_active_high {
            rewrites.push((
                operation_id,
                enable,
                selected_active_high,
                operation.source.clone(),
            ));
        }
    }
    for (operation, enable, active_high, source) in rewrites {
        let value = module
            .unary(word::UnaryOp::BitNot, enable.value, source)
            .map_err(crate::SynthError::from)?;
        let stored = module.operation_mut(operation).ok_or_else(|| {
            crate::SynthError::invariant("sequential enable-polarity target disappeared")
        })?;
        let normalized = word::Enable { value, active_high };
        match &mut stored.kind {
            word::OpKind::Register(register) => register.enable = Some(normalized),
            word::OpKind::Latch(latch) => latch.enable = normalized,
            _ => {
                return Err(crate::SynthError::invariant(
                    "sequential enable-polarity target changed operation kind",
                ));
            }
        }
    }
    Ok(())
}

/// Normalizes resets while retaining enables for their owning passes.
pub(crate) fn lower_controls(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    let mut controlled = Vec::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let word::OpKind::Register(register) = &operation.kind else {
            continue;
        };
        if register.enable.is_none() && register.resets.is_empty() {
            continue;
        }
        let operation_id = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        controlled.push((
            operation_id,
            ControlledRegister {
                register: register.clone(),
                source: operation.source.clone(),
            },
        ));
    }

    for (operation_id, controlled) in controlled {
        let mut data = controlled.register.d;
        let asynchronous_resets =
            normalize_async_resets(module, &controlled.register.resets, &controlled.source)?;
        let synchronous_resets = controlled
            .register
            .resets
            .iter()
            .copied()
            .filter(|reset| reset.kind == word::ResetKind::Sync)
            .collect::<Vec<_>>();
        // A synchronous reset forces both a clock event and the reset value.
        let retained_enable = match controlled.register.enable {
            None => None,
            Some(enable) if synchronous_resets.is_empty() => Some(enable),
            Some(enable) => {
                let enable_active = active_high_control(
                    module,
                    enable.value,
                    enable.active_high,
                    &controlled.source,
                )?;
                let reset_active =
                    combined_reset_condition(module, &synchronous_resets, &controlled.source)?
                        .expect("non-empty synchronous reset list has a condition");
                let composed = module
                    .binary(
                        word::BinaryOp::BitOr,
                        enable_active,
                        reset_active,
                        controlled.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                Some(word::Enable {
                    value: composed,
                    active_high: true,
                })
            }
        };

        for reset in synchronous_resets.iter().rev() {
            data = if reset.active_high {
                module.mux(
                    reset.value,
                    reset.reset_value,
                    data,
                    controlled.source.clone(),
                )
            } else {
                module.mux(
                    reset.value,
                    data,
                    reset.reset_value,
                    controlled.source.clone(),
                )
            }
            .map_err(crate::SynthError::from)?;
        }

        let operation = module.operation_mut(operation_id).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "controlled register operation {operation_id:?} disappeared"
            ))
        })?;
        operation.kind = word::OpKind::Register(word::RegisterOp {
            d: data,
            enable: retained_enable,
            resets: asynchronous_resets,
            ..controlled.register
        });
    }
    Ok(())
}

pub(crate) fn normalize_sequential_controls(
    module: &mut word::WordModule,
) -> Result<(), crate::SynthError> {
    let controls = module
        .operations()
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| match &operation.kind {
            word::OpKind::Register(register) if register.resets.len() > 1 => Some((
                index,
                register.resets.clone(),
                operation.source.clone(),
                false,
            )),
            word::OpKind::Latch(latch) if latch.resets.len() > 1 => {
                Some((index, latch.resets.clone(), operation.source.clone(), true))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, resets, source, latch) in controls {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        let async_count = resets
            .iter()
            .take_while(|reset| reset.kind == word::ResetKind::Async)
            .count();
        if resets[async_count..]
            .iter()
            .any(|reset| reset.kind != word::ResetKind::Sync)
        {
            return Err(crate::SynthError::invariant(
                "sequential resets are not ordered as asynchronous then synchronous controls",
            ));
        }
        if latch && async_count != resets.len() {
            return Err(crate::SynthError::invalid(
                "latches cannot use synchronous reset controls",
            ));
        }
        let asynchronous = normalize_async_resets(module, &resets[..async_count], &source)?;
        let synchronous = normalize_synchronous_resets(module, &resets[async_count..], &source)?;
        let stored = module
            .operation(operation)
            .ok_or_else(|| crate::SynthError::invariant("sequential operation disappeared"))?
            .clone();
        match stored.kind {
            word::OpKind::Register(mut register) => {
                if asynchronous.is_empty() {
                    register.resets = synchronous;
                } else if let Some(synchronous) = synchronous.first().copied() {
                    let active = active_high_control(
                        module,
                        synchronous.value,
                        synchronous.active_high,
                        &source,
                    )?;
                    register.d = module
                        .mux(active, synchronous.reset_value, register.d, source.clone())
                        .map_err(crate::SynthError::from)?;
                    register.enable = register
                        .enable
                        .map(|enable| {
                            let enable = active_high_control(
                                module,
                                enable.value,
                                enable.active_high,
                                &source,
                            )?;
                            Ok::<_, crate::SynthError>(word::Enable {
                                value: module
                                    .binary(
                                        word::BinaryOp::LogicalOr,
                                        enable,
                                        active,
                                        source.clone(),
                                    )
                                    .map_err(crate::SynthError::from)?,
                                active_high: true,
                            })
                        })
                        .transpose()?;
                    register.resets = asynchronous;
                } else {
                    register.resets = asynchronous;
                }
                module
                    .operation_mut(operation)
                    .expect("sequential operation remains present")
                    .kind = word::OpKind::Register(register);
            }
            word::OpKind::Latch(mut latch) => {
                latch.resets = asynchronous;
                module
                    .operation_mut(operation)
                    .expect("sequential operation remains present")
                    .kind = word::OpKind::Latch(latch);
            }
            _ => {
                return Err(crate::SynthError::invariant(
                    "sequential control target changed operation kind",
                ));
            }
        }
    }
    Ok(())
}

fn normalize_synchronous_resets(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Vec<word::Reset>, crate::SynthError> {
    if resets.len() <= 1 {
        return Ok(resets.to_vec());
    }
    let mut reset_value = resets
        .last()
        .expect("multiple synchronous resets are non-empty")
        .reset_value;
    for reset in resets[..resets.len() - 1].iter().rev() {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        reset_value = module
            .mux(asserted, reset.reset_value, reset_value, source.clone())
            .map_err(crate::SynthError::from)?;
    }
    let condition = combined_reset_condition(module, resets, source)?
        .expect("multiple synchronous resets have a condition");
    Ok(vec![word::Reset {
        kind: word::ResetKind::Sync,
        value: condition,
        active_high: true,
        reset_value,
    }])
}

fn active_high_control(
    module: &mut word::WordModule,
    value: word::ValueId,
    active_high: bool,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    if active_high {
        Ok(value)
    } else {
        module
            .unary(word::UnaryOp::BitNot, value, source.clone())
            .map_err(crate::SynthError::from)
    }
}

fn async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<super::AsyncResetRequests, crate::SynthError> {
    resets
        .iter()
        .copied()
        .map(|reset| async_reset_request_for(module, reset))
        .collect()
}

fn async_reset_request_for(
    module: &word::WordModule,
    reset: word::Reset,
) -> Result<super::AsyncResetRequest, crate::SynthError> {
    let value = module.value(reset.reset_value).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown asynchronous reset value {:?}",
            reset.reset_value
        ))
    })?;
    let word::ValueKind::Constant(bits) = &value.kind else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value must be constant",
        ));
    };
    let reset_value = crate::boolean::logic::logic_constant(bits).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "asynchronous register reset value must be a two-state scalar constant, got width {} and bits {bits:?}",
            value.ty.width()
        ))
    })?;
    Ok(super::AsyncResetRequest {
        active_high: reset.active_high,
        reset_value,
    })
}

fn uniform_async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Option<super::AsyncResetRequests>, crate::SynthError> {
    resets
        .iter()
        .copied()
        .map(|reset| uniform_async_reset_request_for(module, reset))
        .collect()
}

fn uniform_async_reset_request_for(
    module: &word::WordModule,
    reset: word::Reset,
) -> Result<Option<super::AsyncResetRequest>, crate::SynthError> {
    let value = module.value(reset.reset_value).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown asynchronous reset value {:?}",
            reset.reset_value
        ))
    })?;
    let word::ValueKind::Constant(bits) = &value.kind else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value must be constant",
        ));
    };
    let Some(first) = bits.as_slice().first().copied() else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value is empty",
        ));
    };
    let reset_value = match first {
        opto_ir::BitVal::Zero => false,
        opto_ir::BitVal::One => true,
        opto_ir::BitVal::X | opto_ir::BitVal::Z => return Ok(None),
    };
    if !bits.as_slice().iter().all(|&bit| bit == first) {
        return Ok(None);
    }
    Ok(Some(super::AsyncResetRequest {
        active_high: reset.active_high,
        reset_value,
    }))
}

fn normalize_async_resets(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Vec<word::Reset>, crate::SynthError> {
    let asynchronous = resets
        .iter()
        .copied()
        .filter(|reset| reset.kind == word::ResetKind::Async)
        .collect::<Vec<_>>();
    if asynchronous.len() <= 1 {
        return Ok(asynchronous);
    }

    let requests = async_reset_requests(module, &asynchronous)?;
    if requests
        .iter()
        .all(|request| request.reset_value == requests[0].reset_value)
    {
        let condition = combined_reset_condition(module, &asynchronous, source)?
            .expect("multiple asynchronous resets have a condition");
        return Ok(vec![word::Reset {
            kind: word::ResetKind::Async,
            value: condition,
            active_high: true,
            reset_value: asynchronous[0].reset_value,
        }]);
    }

    let mut blocked = None;
    let mut groups = [None, None];
    let mut group_values = [None, None];
    for (reset, request) in asynchronous.iter().zip(requests) {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        let effective = if let Some(blocked) = blocked {
            let available = module
                .unary(word::UnaryOp::BitNot, blocked, source.clone())
                .map_err(crate::SynthError::from)?;
            module
                .binary(
                    word::BinaryOp::LogicalAnd,
                    asserted,
                    available,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?
        } else {
            asserted
        };
        let group = usize::from(request.reset_value);
        groups[group] = Some(match groups[group] {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    effective,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => effective,
        });
        group_values[group].get_or_insert(reset.reset_value);
        blocked = Some(match blocked {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    asserted,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => asserted,
        });
    }
    Ok(groups
        .into_iter()
        .zip(group_values)
        .filter_map(|(value, reset_value)| {
            Some(word::Reset {
                kind: word::ResetKind::Async,
                value: value?,
                active_high: true,
                reset_value: reset_value?,
            })
        })
        .collect())
}

fn combined_reset_condition(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Option<word::ValueId>, crate::SynthError> {
    let mut combined = None;
    for reset in resets {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        combined = Some(match combined {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    asserted,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => asserted,
        });
    }
    Ok(combined)
}

struct ControlledRegister {
    register: word::RegisterOp,
    source: word::SourceSpan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::{BitVal, ConstBits};

    fn input(module: &mut word::WordModule, name: &str) -> word::ValueId {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    }

    #[test]
    fn normalizes_controlled_registers_independently_of_current_observability() {
        let mut module = word::WordModule::new("unobserved_controlled_register");
        let clock = input(&mut module, "clock");
        let reset = input(&mut module, "reset");
        let data = input(&mut module, "data");
        let zero = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero]).unwrap(),
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let result = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: data,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: vec![word::Reset {
                        kind: word::ResetKind::Sync,
                        value: reset,
                        active_high: true,
                        reset_value: zero,
                    }],
                },
                word::SourceSpan::default(),
            )
            .unwrap();
        lower_controls(&mut module).unwrap();

        let word::ValueKind::Operation(operation) = module.value(result).unwrap().kind else {
            panic!("register result lost its operation identity");
        };
        let word::OpKind::Register(register) = &module.operation(operation).unwrap().kind else {
            panic!("controlled operation is no longer a register");
        };
        assert!(register.resets.is_empty());
        assert!(matches!(
            module.value(register.d).unwrap().kind,
            word::ValueKind::Operation(operation)
                if matches!(module.operation(operation).unwrap().kind, word::OpKind::Mux { .. })
        ));
    }

    #[test]
    fn uniform_vector_reset_has_one_catalog_request() {
        let mut module = word::WordModule::new("uniform_reset");
        let reset_value = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero; 4]).unwrap(),
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let reset = input(&mut module, "reset");
        let request = uniform_async_reset_request_for(
            &module,
            word::Reset {
                kind: word::ResetKind::Async,
                value: reset,
                active_high: true,
                reset_value,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            request,
            super::super::AsyncResetRequest {
                active_high: true,
                reset_value: false,
            }
        );
    }

    #[test]
    fn mixed_vector_reset_defers_catalog_selection_until_bitblast() {
        let mut module = word::WordModule::new("mixed_reset");
        let reset_value = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero, BitVal::One, BitVal::Zero, BitVal::One])
                    .unwrap(),
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let reset = input(&mut module, "reset");

        assert_eq!(
            uniform_async_reset_request_for(
                &module,
                word::Reset {
                    kind: word::ResetKind::Async,
                    value: reset,
                    active_high: false,
                    reset_value,
                },
            )
            .unwrap(),
            None
        );
    }
}
