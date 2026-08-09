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
    if !module
        .signals()
        .iter()
        .any(|signal| signal.resolution != word::SignalResolution::SingleDriver)
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
        if signal.resolution == word::SignalResolution::SingleDriver {
            module
                .connect(connect.target, connect.value, connect.source)
                .map_err(crate::SynthError::from)?;
        } else {
            drivers
                .entry(connect.target.signal)
                .or_default()
                .push(Driver {
                    target: connect.target,
                    value: connect.value,
                    source: connect.source,
                });
        }
    }

    let resolved = module
        .signals()
        .iter()
        .enumerate()
        .filter(|(_, signal)| signal.resolution != word::SignalResolution::SingleDriver)
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
            word::SignalResolution::SingleDriver => unreachable!(),
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
        for driver in drivers.remove(&signal).unwrap_or_default() {
            let contribution = full_width_contribution(module, signal, identity, &driver)?;
            value = module
                .binary(
                    match resolution {
                        word::SignalResolution::WiredAnd => word::BinaryOp::BitAnd,
                        word::SignalResolution::WiredOr => word::BinaryOp::BitOr,
                        word::SignalResolution::SingleDriver => unreachable!(),
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
                .is_some_and(|signal| signal.resolution != word::SignalResolution::SingleDriver)
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
                .is_some_and(|signal| signal.resolution != word::SignalResolution::SingleDriver);
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
