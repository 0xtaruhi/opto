// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Resolved-net normalization before single-driver synthesis.
//!
//! Resolved nets remain explicit multi-driver objects through elaboration and
//! process lowering. This pass is their single normalization boundary: every
//! driver becomes a full-width contribution with the resolution identity in
//! undriven positions, then all contributions are reduced to one ordinary Word
//! IR driver.

use crate::ReferencePortMap;
use opto_ir::{BitVal, ConstBits, word};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct Driver {
    target: word::LValue,
    value: word::ValueId,
    source: word::SourceSpan,
}

pub(super) fn lower_resolved_nets(
    module: &mut word::WordModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), crate::SynthError> {
    scalarize_tri_state_drivers(module)?;
    if !module
        .signals()
        .iter()
        .any(|signal| is_wired_resolution(signal.resolution))
    {
        return Ok(());
    }

    let mut drivers = BTreeMap::<word::SignalId, Vec<Driver>>::new();
    redirect_instance_output_drivers(module, reference_ports, &mut drivers)?;

    for connect in module.take_connects() {
        let signal = module.signal(connect.target.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "resolved-net driver references unknown signal {:?}",
                connect.target.signal
            ))
        })?;
        if is_wired_resolution(signal.resolution) {
            drivers
                .entry(connect.target.signal)
                .or_default()
                .push(Driver {
                    target: connect.target,
                    value: connect.value,
                    source: connect.source,
                });
        } else {
            module
                .connect(connect.target, connect.value, connect.source)
                .map_err(crate::SynthError::from)?;
        }
    }

    let resolved = module
        .signals()
        .iter()
        .enumerate()
        .filter(|(_, signal)| is_wired_resolution(signal.resolution))
        .map(|(index, signal)| {
            Ok((
                word::SignalId::from_index(index)?,
                signal.resolution,
                signal.ty,
                signal.source.clone(),
            ))
        })
        .collect::<Result<Vec<_>, opto_ir::word::WordError>>()?;
    for (signal, resolution, ty, source) in resolved {
        let identity = match resolution {
            word::SignalResolution::WiredAnd => BitVal::One,
            word::SignalResolution::WiredOr => BitVal::Zero,
            word::SignalResolution::SingleDriver | word::SignalResolution::TriState => {
                unreachable!()
            }
        };
        let identity = module
            .constant(
                ConstBits::from_bits(vec![identity; ty.width() as usize])
                    .map_err(|error| crate::SynthError::invalid(error.to_string()))?,
                ty,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let mut value = identity;
        for mut driver in drivers.remove(&signal).unwrap_or_default() {
            driver.value = lower_wired_driver(module, resolution, driver.value, &driver.source)?;
            let contribution = full_width_contribution(module, signal, identity, &driver)?;
            value = module
                .binary(
                    match resolution {
                        word::SignalResolution::WiredAnd => word::BinaryOp::BitAnd,
                        word::SignalResolution::WiredOr => word::BinaryOp::BitOr,
                        word::SignalResolution::SingleDriver | word::SignalResolution::TriState => {
                            unreachable!()
                        }
                    },
                    value,
                    contribution,
                    driver.source,
                )
                .map_err(crate::SynthError::from)?;
        }
        module
            .connect(word::LValue::signal(signal), value, source)
            .map_err(crate::SynthError::from)?;
        module
            .set_signal_resolution(signal, word::SignalResolution::SingleDriver)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

/// Converts every physically resolved contribution to one scalar tri-state
/// driver per target bit. Ordinary contributions on a multi-driver wire use a
/// constant active enable; explicit high-impedance contributions retain their
/// source enable and polarity.
fn scalarize_tri_state_drivers(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    if !module
        .signals()
        .iter()
        .any(|signal| signal.resolution == word::SignalResolution::TriState)
    {
        return Ok(());
    }
    for connect in module.take_connects() {
        let signal = module.signal(connect.target.signal).ok_or_else(|| {
            crate::SynthError::invariant("tri-state connect target signal disappeared")
        })?;
        if signal.resolution != word::SignalResolution::TriState {
            module
                .connect(connect.target, connect.value, connect.source)
                .map_err(crate::SynthError::from)?;
            continue;
        }
        if connect.target.dynamic.is_some() {
            return Err(crate::SynthError::unsupported(
                "dynamic assignment to a physically resolved tri-state net is not supported",
            ));
        }
        let explicit = match module
            .value(connect.value)
            .ok_or_else(|| crate::SynthError::invariant("tri-state connect value disappeared"))?
            .kind
        {
            word::ValueKind::Operation(operation) => {
                module
                    .operation(operation)
                    .and_then(|operation| match operation.kind {
                        word::OpKind::TriState { data, enable } => Some((data, enable)),
                        _ => None,
                    })
            }
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
        };
        let (data, enable) = if let Some(driver) = explicit {
            driver
        } else {
            let source =
                super::derived_source(&connect.source, "tri-state constant enable", b"active")?;
            let ty = word::WordType::new(1, false, word::LogicStateKind::FourState)
                .map_err(crate::SynthError::from)?;
            let enable = module
                .constant(
                    ConstBits::from_bits(vec![BitVal::One]).map_err(crate::SynthError::from)?,
                    ty,
                    source,
                )
                .map_err(crate::SynthError::from)?;
            (
                connect.value,
                word::Enable {
                    value: enable,
                    active_high: true,
                },
            )
        };
        let width = module
            .value(data)
            .ok_or_else(|| crate::SynthError::invariant("tri-state data disappeared"))?
            .ty
            .width();
        for bit in 0..width {
            let role = bit.to_le_bytes();
            let source = super::derived_source(&connect.source, "tri-state scalar data", role)?;
            let data_bit = if width == 1 {
                data
            } else {
                module
                    .extract(data, bit, 1, source)
                    .map_err(crate::SynthError::from)?
            };
            let source = super::derived_source(&connect.source, "tri-state scalar driver", role)?;
            let driver = if width == 1 && explicit.is_some() {
                connect.value
            } else {
                module
                    .tri_state(data_bit, enable, source.clone())
                    .map_err(crate::SynthError::from)?
            };
            let target_bit = match connect.target.range {
                Some(range) if range.msb < range.lsb => range
                    .lsb
                    .checked_sub(bit)
                    .ok_or_else(|| crate::SynthError::invariant("tri-state target underflow"))?,
                Some(range) => range
                    .lsb
                    .checked_add(bit)
                    .ok_or_else(|| crate::SynthError::capacity("tri-state target overflow"))?,
                None => bit,
            };
            module
                .connect(
                    word::LValue::signal(connect.target.signal).with_range(word::BitRange {
                        msb: target_bit,
                        lsb: target_bit,
                    }),
                    driver,
                    source,
                )
                .map_err(crate::SynthError::from)?;
        }
    }
    Ok(())
}

const fn is_wired_resolution(resolution: word::SignalResolution) -> bool {
    matches!(
        resolution,
        word::SignalResolution::WiredAnd | word::SignalResolution::WiredOr
    )
}

/// Converts an explicitly enabled high-impedance driver into the identity of
/// a wired resolution function. This is exact for `wand` and `wor`: a disabled
/// contribution is respectively all ones or all zeros, while the ordinary
/// data path remains unchanged.
fn lower_wired_driver(
    module: &mut word::WordModule,
    resolution: word::SignalResolution,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let word::ValueKind::Operation(operation) = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("resolved-net driver value disappeared"))?
        .kind
    else {
        return Ok(value);
    };
    let word::OpKind::TriState { data, enable } = module
        .operation(operation)
        .ok_or_else(|| crate::SynthError::invariant("tri-state driver operation disappeared"))?
        .kind
    else {
        return Ok(value);
    };
    let data_ty = module
        .value(data)
        .ok_or_else(|| crate::SynthError::invariant("tri-state driver data disappeared"))?
        .ty;
    let identity_bit = match resolution {
        word::SignalResolution::WiredAnd => BitVal::One,
        word::SignalResolution::WiredOr => BitVal::Zero,
        word::SignalResolution::SingleDriver | word::SignalResolution::TriState => {
            return Err(crate::SynthError::invariant(
                "single-driver signal reached wired-net normalization",
            ));
        }
    };
    let identity = module
        .constant(
            ConstBits::from_bits(vec![identity_bit; data_ty.width() as usize])
                .map_err(crate::SynthError::from)?,
            data_ty,
            source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    module
        .mux(
            enable.value,
            if enable.active_high { data } else { identity },
            if enable.active_high { identity } else { data },
            source.clone(),
        )
        .map_err(crate::SynthError::from)
}

fn redirect_instance_output_drivers(
    module: &mut word::WordModule,
    reference_ports: &ReferencePortMap,
    drivers: &mut BTreeMap<word::SignalId, Vec<Driver>>,
) -> Result<(), crate::SynthError> {
    let mut connections = Vec::new();
    for (instance_index, instance) in module.instances().iter().enumerate() {
        let reference = module.name_str(instance.module);
        let Some(ports) = reference_ports.get(reference) else {
            continue;
        };
        for connection in &instance.connections {
            let port = module.name_str(connection.port);
            if ports.get(port).is_some_and(|port| {
                matches!(
                    port.direction,
                    word::PortDirection::Output | word::PortDirection::Inout
                )
            }) {
                connections.push((
                    instance_index,
                    module.name_str(instance.name).to_string(),
                    port.to_string(),
                    ports[port].direction,
                    connection.value,
                    connection.source.clone(),
                ));
            }
        }
    }

    for (instance_index, instance_name, port, direction, connection, source) in connections {
        let fragments = module
            .signal_fragments(connection)
            .map_err(crate::SynthError::from)?;
        if !fragments.iter().any(|fragment| {
            module
                .signal(fragment.reference.signal)
                .is_some_and(|signal| is_wired_resolution(signal.resolution))
        }) {
            continue;
        }
        if direction == word::PortDirection::Inout {
            return Err(crate::SynthError::unsupported(format!(
                "inout connection '{instance_name}.{port}' cannot drive a resolved net"
            )));
        }
        let connection_ty = module
            .value(connection)
            .ok_or_else(|| crate::SynthError::invariant("missing instance output value"))?
            .ty;
        let hidden = add_unique_wire(
            module,
            &format!("$resolve${instance_name}${port}"),
            connection_ty,
            source.clone(),
        )?;
        let hidden_value = module
            .read_signal(hidden, source.clone())
            .map_err(crate::SynthError::from)?;
        let instance = word::InstId::from_index(instance_index).map_err(crate::SynthError::Word)?;
        module
            .set_instance_connection_value(instance, &port, hidden_value)
            .map_err(crate::SynthError::from)?;

        let mut offset = 0u32;
        for fragment in fragments {
            let value = if offset == 0 && fragment.reference.width() == connection_ty.width() {
                hidden_value
            } else {
                module
                    .extract(
                        hidden_value,
                        offset,
                        fragment.reference.width(),
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?
            };
            let value = coerce_value(module, value, fragment.ty, &source)?;
            let target = fragment_lvalue(module, fragment)?;
            let resolved = module
                .signal(fragment.reference.signal)
                .is_some_and(|signal| is_wired_resolution(signal.resolution));
            if resolved {
                drivers
                    .entry(fragment.reference.signal)
                    .or_default()
                    .push(Driver {
                        target,
                        value,
                        source: source.clone(),
                    });
            } else {
                module
                    .connect(target, value, source.clone())
                    .map_err(crate::SynthError::from)?;
            }
            offset = offset
                .checked_add(fragment.reference.width())
                .ok_or_else(|| {
                    crate::SynthError::capacity("instance output width exceeds 32-bit capacity")
                })?;
        }
    }
    Ok(())
}

fn full_width_contribution(
    module: &mut word::WordModule,
    signal: word::SignalId,
    identity: word::ValueId,
    driver: &Driver,
) -> Result<word::ValueId, crate::SynthError> {
    if driver.target.dynamic.is_some() {
        return Err(crate::SynthError::unsupported(
            "dynamic assignment to a resolved net is not supported",
        ));
    }
    let signal_ty = module
        .signal(signal)
        .ok_or_else(|| crate::SynthError::invariant("resolved signal disappeared"))?
        .ty;
    let Some(range) = driver.target.range else {
        return coerce_value(module, driver.value, signal_ty, &driver.source);
    };
    let lsb = range.lsb.min(range.msb);
    let width = range.width();
    let mut parts = Vec::with_capacity(3);
    let upper_lsb = lsb.checked_add(width).ok_or_else(|| {
        crate::SynthError::capacity("resolved-net driver range exceeds 32-bit capacity")
    })?;
    if upper_lsb < signal_ty.width() {
        parts.push(
            module
                .extract(
                    identity,
                    upper_lsb,
                    signal_ty.width() - upper_lsb,
                    driver.source.clone(),
                )
                .map_err(crate::SynthError::from)?,
        );
    }
    parts.push(driver.value);
    if lsb > 0 {
        parts.push(
            module
                .extract(identity, 0, lsb, driver.source.clone())
                .map_err(crate::SynthError::from)?,
        );
    }
    let value = if parts.len() == 1 {
        parts[0]
    } else {
        module
            .concat(parts, driver.source.clone())
            .map_err(crate::SynthError::from)?
    };
    coerce_value(module, value, signal_ty, &driver.source)
}

fn fragment_lvalue(
    module: &word::WordModule,
    fragment: word::SignalFragment,
) -> Result<word::LValue, crate::SynthError> {
    let width = module
        .signal(fragment.reference.signal)
        .ok_or_else(|| crate::SynthError::invariant("instance output signal disappeared"))?
        .ty
        .width();
    if fragment.reference.lsb == 0 && fragment.reference.width() == width {
        Ok(word::LValue::signal(fragment.reference.signal))
    } else {
        let msb = fragment
            .reference
            .lsb
            .checked_add(fragment.reference.width() - 1)
            .ok_or_else(|| crate::SynthError::capacity("instance output range overflow"))?;
        Ok(
            word::LValue::signal(fragment.reference.signal).with_range(word::BitRange {
                msb,
                lsb: fragment.reference.lsb,
            }),
        )
    }
}

fn coerce_value(
    module: &mut word::WordModule,
    value: word::ValueId,
    target: word::WordType,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let actual = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("resolved-net value disappeared"))?
        .ty;
    if actual == target {
        return Ok(value);
    }
    if actual.width() != target.width() || actual.state() != target.state() {
        return Err(crate::SynthError::invalid(
            "resolved-net driver has an incompatible type",
        ));
    }
    module
        .cast(word::CastKind::ZeroExtend, value, target, source.clone())
        .map_err(crate::SynthError::from)
}

fn add_unique_wire(
    module: &mut word::WordModule,
    base: &str,
    ty: word::WordType,
    source: word::SourceSpan,
) -> Result<word::SignalId, crate::SynthError> {
    for suffix in 0u64.. {
        let name = if suffix == 0 {
            base.to_string()
        } else {
            format!("{base}${suffix}")
        };
        if module.signal_id(&name).is_none() {
            return module
                .add_wire(name, ty, source)
                .map_err(crate::SynthError::from);
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wired_and_consumes_disabled_tri_state_as_resolution_identity() {
        let mut module = word::WordModule::new("wired_tri_state");
        let ty = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
        let source = word::SourceSpan::stable("wired tri-state test");
        let data_port = module
            .add_port("data", word::PortDirection::Input, ty, source.clone())
            .unwrap();
        let enable_port = module
            .add_port("enable", word::PortDirection::Input, ty, source.clone())
            .unwrap();
        let output_port = module
            .add_port("y", word::PortDirection::Output, ty, source.clone())
            .unwrap();
        let output = module.port(output_port).unwrap().signal;
        module
            .set_signal_resolution(output, word::SignalResolution::WiredAnd)
            .unwrap();
        let data = module
            .read_signal(module.port(data_port).unwrap().signal, source.clone())
            .unwrap();
        let enable_value = module
            .read_signal(module.port(enable_port).unwrap().signal, source.clone())
            .unwrap();
        let driver = module
            .tri_state(
                data,
                word::Enable {
                    value: enable_value,
                    active_high: true,
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(output), driver, source)
            .unwrap();

        lower_resolved_nets(&mut module, &ReferencePortMap::new()).unwrap();

        assert_eq!(
            module.signal(output).unwrap().resolution,
            word::SignalResolution::SingleDriver
        );
        let root = module
            .connects()
            .iter()
            .find(|connect| connect.target.signal == output)
            .unwrap()
            .value;
        assert!(matches!(
            module.value(root).unwrap().kind,
            word::ValueKind::Operation(operation)
                if matches!(module.operation(operation).unwrap().kind, word::OpKind::Binary {
                    op: word::BinaryOp::BitAnd,
                    ..
                })
        ));
        assert!(
            !crate::word::operation_inputs(
                &module
                    .operation(match module.value(root).unwrap().kind {
                        word::ValueKind::Operation(operation) => operation,
                        _ => unreachable!(),
                    })
                    .unwrap()
                    .kind,
            )
            .contains(&driver)
        );
    }

    #[test]
    fn scalarizes_each_vector_tri_state_contribution_without_resolving_the_net() {
        let mut module = word::WordModule::new("vector_tri_state");
        let data_ty = word::WordType::new(2, false, word::LogicStateKind::FourState).unwrap();
        let enable_ty = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
        let source = word::SourceSpan::stable("vector tri-state test");
        let data_port = module
            .add_port("data", word::PortDirection::Input, data_ty, source.clone())
            .unwrap();
        let enable_port = module
            .add_port(
                "enable",
                word::PortDirection::Input,
                enable_ty,
                source.clone(),
            )
            .unwrap();
        let pad_port = module
            .add_port("pad", word::PortDirection::Inout, data_ty, source.clone())
            .unwrap();
        let pad = module.port(pad_port).unwrap().signal;
        module
            .set_signal_resolution(pad, word::SignalResolution::TriState)
            .unwrap();
        let data = module
            .read_signal(module.port(data_port).unwrap().signal, source.clone())
            .unwrap();
        let enable_value = module
            .read_signal(module.port(enable_port).unwrap().signal, source.clone())
            .unwrap();
        let driver = module
            .tri_state(
                data,
                word::Enable {
                    value: enable_value,
                    active_high: false,
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(pad), driver, source)
            .unwrap();

        lower_resolved_nets(&mut module, &ReferencePortMap::new()).unwrap();

        assert_eq!(
            module.signal(pad).unwrap().resolution,
            word::SignalResolution::TriState
        );
        assert_eq!(module.connects().len(), 2);
        for (bit, connect) in module.connects().iter().enumerate() {
            let bit = u32::try_from(bit).unwrap();
            assert_eq!(
                connect.target.range,
                Some(word::BitRange { msb: bit, lsb: bit })
            );
            let word::ValueKind::Operation(operation) = module.value(connect.value).unwrap().kind
            else {
                panic!("scalar physical contribution must remain a tri-state operation");
            };
            let word::OpKind::TriState { data, enable } = module.operation(operation).unwrap().kind
            else {
                panic!("scalar physical contribution lost its data/enable contract");
            };
            assert_eq!(module.value(data).unwrap().ty.width(), 1);
            assert_eq!(enable.value, enable_value);
            assert!(!enable.active_high);
        }
    }
}
