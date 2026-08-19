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

#[derive(Debug, Clone)]
struct InstanceOutputConnection {
    instance_index: usize,
    instance_name: String,
    port: String,
    direction: word::PortDirection,
    value: word::ValueId,
    source: word::SourceSpan,
}

pub(super) fn lower_resolved_nets(
    module: &mut word::WordModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), crate::SynthError> {
    let instance_outputs = instance_output_connections(module, reference_ports);
    validate_supply_instance_outputs(module, &instance_outputs)?;
    materialize_supply_nets(module)?;
    scalarize_tri_state_drivers(module)?;
    if !module
        .signals()
        .iter()
        .any(|signal| is_wired_resolution(signal.resolution))
    {
        return Ok(());
    }

    let mut drivers = BTreeMap::<word::SignalId, Vec<Driver>>::new();
    redirect_instance_output_drivers(module, &instance_outputs, &mut drivers)?;

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
            word::SignalResolution::SingleDriver
            | word::SignalResolution::TriState
            | word::SignalResolution::PullZero
            | word::SignalResolution::PullOne
            | word::SignalResolution::SupplyZero
            | word::SignalResolution::SupplyOne => {
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
                        word::SignalResolution::SingleDriver
                        | word::SignalResolution::TriState
                        | word::SignalResolution::PullZero
                        | word::SignalResolution::PullOne
                        | word::SignalResolution::SupplyZero
                        | word::SignalResolution::SupplyOne => {
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

/// Replaces supply nets with one ordinary constant driver. Explicit drivers
/// would require strength resolution against the supply strength and are
/// rejected rather than being silently assigned Boolean priority.
fn materialize_supply_nets(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    let supplies = module
        .signals()
        .iter()
        .enumerate()
        .filter_map(|(index, signal)| {
            let bit = match signal.resolution {
                word::SignalResolution::SupplyZero => BitVal::Zero,
                word::SignalResolution::SupplyOne => BitVal::One,
                _ => return None,
            };
            Some((
                word::SignalId::from_index(index).expect("signal index must fit"),
                signal.ty,
                signal.source.clone(),
                bit,
            ))
        })
        .collect::<Vec<_>>();
    if supplies.is_empty() {
        return Ok(());
    }
    let supply_ids = supplies
        .iter()
        .map(|(signal, _, _, _)| *signal)
        .collect::<std::collections::BTreeSet<_>>();
    for connect in module.connects() {
        if supply_ids.contains(&connect.target.signal) {
            let name = module
                .signal(connect.target.signal)
                .and_then(|signal| signal.name)
                .map_or("<unnamed>", |name| module.name_str(name));
            return Err(crate::SynthError::unsupported(format!(
                "explicit driver on supply net '{name}' requires strength resolution"
            )));
        }
    }
    for (signal, ty, source, bit) in supplies {
        let value = module
            .constant(
                ConstBits::from_bits(vec![bit; ty.width() as usize])
                    .map_err(crate::SynthError::from)?,
                ty,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?;
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
        .any(|signal| is_physical_resolution(signal.resolution))
    {
        return Ok(());
    }
    let mut active_drivers = BTreeMap::<(word::SignalId, u32), Vec<word::ValueId>>::new();
    for connect in module.take_connects() {
        let resolution = module
            .signal(connect.target.signal)
            .ok_or_else(|| {
                crate::SynthError::invariant("tri-state connect target signal disappeared")
            })?
            .resolution;
        if !is_physical_resolution(resolution) {
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
            let active = if enable.active_high {
                enable.value
            } else {
                let source =
                    super::derived_source(&connect.source, "tri-state active enable", role)?;
                module
                    .unary(word::UnaryOp::BitNot, enable.value, source)
                    .map_err(crate::SynthError::from)?
            };
            if matches!(
                resolution,
                word::SignalResolution::PullZero | word::SignalResolution::PullOne
            ) {
                active_drivers
                    .entry((connect.target.signal, target_bit))
                    .or_default()
                    .push(active);
            }
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
    let pulls = module
        .signals()
        .iter()
        .enumerate()
        .filter_map(|(index, signal)| {
            let bit = match signal.resolution {
                word::SignalResolution::PullZero => BitVal::Zero,
                word::SignalResolution::PullOne => BitVal::One,
                _ => return None,
            };
            Some((
                word::SignalId::from_index(index).expect("signal index must fit"),
                signal.ty.width(),
                signal.source.clone(),
                bit,
            ))
        })
        .collect::<Vec<_>>();
    for (signal, width, source, pull_bit) in pulls {
        for bit in 0..width {
            let role = bit.to_le_bytes();
            let derived = super::derived_source(&source, "default pull data", role)?;
            let ty = word::WordType::new(1, false, word::LogicStateKind::FourState)
                .map_err(crate::SynthError::from)?;
            let data = module
                .constant(
                    ConstBits::from_bits(vec![pull_bit]).map_err(crate::SynthError::from)?,
                    ty,
                    derived.clone(),
                )
                .map_err(crate::SynthError::from)?;
            let enable_value = if let Some(active) = active_drivers.remove(&(signal, bit)) {
                let mut active = active.into_iter();
                let first = active
                    .next()
                    .expect("recorded pull-net driver list must not be empty");
                let any_active = active.try_fold(first, |left, right| {
                    module
                        .binary(word::BinaryOp::BitOr, left, right, derived.clone())
                        .map_err(crate::SynthError::from)
                })?;
                module
                    .unary(word::UnaryOp::BitNot, any_active, derived.clone())
                    .map_err(crate::SynthError::from)?
            } else {
                module
                    .constant(
                        ConstBits::from_bits(vec![BitVal::One]).map_err(crate::SynthError::from)?,
                        ty,
                        derived.clone(),
                    )
                    .map_err(crate::SynthError::from)?
            };
            let driver = module
                .tri_state(
                    data,
                    word::Enable {
                        value: enable_value,
                        active_high: true,
                    },
                    derived.clone(),
                )
                .map_err(crate::SynthError::from)?;
            module
                .connect(
                    word::LValue::signal(signal).with_range(word::BitRange { msb: bit, lsb: bit }),
                    driver,
                    derived,
                )
                .map_err(crate::SynthError::from)?;
        }
        module
            .set_signal_resolution(signal, word::SignalResolution::TriState)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

const fn is_physical_resolution(resolution: word::SignalResolution) -> bool {
    matches!(
        resolution,
        word::SignalResolution::TriState
            | word::SignalResolution::PullZero
            | word::SignalResolution::PullOne
    )
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
        word::SignalResolution::SingleDriver
        | word::SignalResolution::TriState
        | word::SignalResolution::PullZero
        | word::SignalResolution::PullOne
        | word::SignalResolution::SupplyZero
        | word::SignalResolution::SupplyOne => {
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

fn instance_output_connections(
    module: &word::WordModule,
    reference_ports: &ReferencePortMap,
) -> Vec<InstanceOutputConnection> {
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
                connections.push(InstanceOutputConnection {
                    instance_index,
                    instance_name: module.name_str(instance.name).to_string(),
                    port: port.to_string(),
                    direction: ports[port].direction,
                    value: connection.value,
                    source: connection.source.clone(),
                });
            }
        }
    }
    connections
}

fn validate_supply_instance_outputs(
    module: &word::WordModule,
    connections: &[InstanceOutputConnection],
) -> Result<(), crate::SynthError> {
    for connection in connections {
        for fragment in module
            .signal_fragments(connection.value)
            .map_err(crate::SynthError::from)?
        {
            let signal = module.signal(fragment.reference.signal).ok_or_else(|| {
                crate::SynthError::invariant("instance output signal disappeared")
            })?;
            if matches!(
                signal.resolution,
                word::SignalResolution::SupplyZero | word::SignalResolution::SupplyOne
            ) {
                let name = signal
                    .name
                    .map_or("<unnamed>", |name| module.name_str(name));
                return Err(crate::SynthError::unsupported(format!(
                    "instance output connection '{}.{}' drives supply net '{name}' and requires strength resolution",
                    connection.instance_name, connection.port
                )));
            }
        }
    }
    Ok(())
}

fn redirect_instance_output_drivers(
    module: &mut word::WordModule,
    connections: &[InstanceOutputConnection],
    drivers: &mut BTreeMap<word::SignalId, Vec<Driver>>,
) -> Result<(), crate::SynthError> {
    for connection in connections {
        let fragments = module
            .signal_fragments(connection.value)
            .map_err(crate::SynthError::from)?;
        if !fragments.iter().any(|fragment| {
            module
                .signal(fragment.reference.signal)
                .is_some_and(|signal| is_wired_resolution(signal.resolution))
        }) {
            continue;
        }
        if connection.direction == word::PortDirection::Inout {
            return Err(crate::SynthError::unsupported(format!(
                "inout connection '{}.{}' cannot drive a resolved net",
                connection.instance_name, connection.port
            )));
        }
        let connection_ty = module
            .value(connection.value)
            .ok_or_else(|| crate::SynthError::invariant("missing instance output value"))?
            .ty;
        let hidden = add_unique_wire(
            module,
            &format!(
                "$resolve${instance}${port}",
                instance = connection.instance_name,
                port = connection.port
            ),
            connection_ty,
            connection.source.clone(),
        )?;
        let hidden_value = module
            .read_signal(hidden, connection.source.clone())
            .map_err(crate::SynthError::from)?;
        let instance =
            word::InstId::from_index(connection.instance_index).map_err(crate::SynthError::Word)?;
        module
            .set_instance_connection_value(instance, &connection.port, hidden_value)
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
                        connection.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?
            };
            let value = coerce_value(module, value, fragment.ty, &connection.source)?;
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
                        source: connection.source.clone(),
                    });
            } else {
                module
                    .connect(target, value, connection.source.clone())
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

    #[test]
    fn materializes_undriven_pull_and_supply_defaults() {
        let mut module = word::WordModule::new("default_nets");
        let ty = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
        let source = word::SourceSpan::stable("default net test");
        let pull = module.add_wire("pull", ty, source.clone()).unwrap();
        let supply = module.add_wire("supply", ty, source.clone()).unwrap();
        module
            .set_signal_resolution(pull, word::SignalResolution::PullOne)
            .unwrap();
        module
            .set_signal_resolution(supply, word::SignalResolution::SupplyZero)
            .unwrap();

        lower_resolved_nets(&mut module, &ReferencePortMap::new()).unwrap();

        assert_eq!(
            module.signal(pull).unwrap().resolution,
            word::SignalResolution::TriState
        );
        assert_eq!(
            module.signal(supply).unwrap().resolution,
            word::SignalResolution::SingleDriver
        );
        let pull_driver = module
            .connects()
            .iter()
            .find(|connect| connect.target.signal == pull)
            .unwrap();
        let word::ValueKind::Operation(operation) = module.value(pull_driver.value).unwrap().kind
        else {
            panic!("default pull must remain an explicit physical driver");
        };
        let word::OpKind::TriState { data, enable } = module.operation(operation).unwrap().kind
        else {
            panic!("default pull must lower to a tri-state driver");
        };
        assert!(matches!(
            module.value(data).unwrap().kind,
            word::ValueKind::Constant(ref bits) if bits.bit_lsb(0) == Some(BitVal::One)
        ));
        assert!(matches!(
            module.value(enable.value).unwrap().kind,
            word::ValueKind::Constant(ref bits) if bits.bit_lsb(0) == Some(BitVal::One)
        ));
        let supply_driver = module
            .connects()
            .iter()
            .find(|connect| connect.target.signal == supply)
            .unwrap();
        assert!(matches!(
            module.value(supply_driver.value).unwrap().kind,
            word::ValueKind::Constant(ref bits) if bits.bit_lsb(0) == Some(BitVal::Zero)
        ));
    }

    #[test]
    fn pull_default_is_active_exactly_when_explicit_drivers_are_disabled() {
        let mut module = word::WordModule::new("pull_override");
        let ty = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
        let source = word::SourceSpan::stable("pull override test");
        let enable_port = module
            .add_port("enable", word::PortDirection::Input, ty, source.clone())
            .unwrap();
        let enable = module
            .read_signal(module.port(enable_port).unwrap().signal, source.clone())
            .unwrap();
        let pull = module.add_wire("pull", ty, source.clone()).unwrap();
        module
            .set_signal_resolution(pull, word::SignalResolution::PullOne)
            .unwrap();
        let zero = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero]).unwrap(),
                ty,
                source.clone(),
            )
            .unwrap();
        let explicit = module
            .tri_state(
                zero,
                word::Enable {
                    value: enable,
                    active_high: true,
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(pull), explicit, source)
            .unwrap();

        lower_resolved_nets(&mut module, &ReferencePortMap::new()).unwrap();

        let drivers = module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == pull)
            .map(|connect| {
                let word::ValueKind::Operation(operation) =
                    module.value(connect.value).unwrap().kind
                else {
                    panic!("pull contribution must remain physical");
                };
                let word::OpKind::TriState { data, enable } =
                    module.operation(operation).unwrap().kind
                else {
                    panic!("pull contribution must remain tri-state");
                };
                (data, enable)
            })
            .collect::<Vec<_>>();
        assert_eq!(drivers.len(), 2);
        let (_, explicit_enable) = drivers
            .iter()
            .find(|(data, _)| *data == zero)
            .expect("explicit zero driver must be preserved");
        assert_eq!(explicit_enable.value, enable);
        assert!(explicit_enable.active_high);
        let (_, pull_enable) = drivers
            .iter()
            .find(|(data, _)| *data != zero)
            .expect("default pull-one driver must be present");
        assert!(pull_enable.active_high);
        assert!(matches!(
            module.value(pull_enable.value).unwrap().kind,
            word::ValueKind::Operation(operation)
                if matches!(module.operation(operation).unwrap().kind,
                    word::OpKind::Unary { op: word::UnaryOp::BitNot, arg } if arg == enable)
        ));
    }

    #[test]
    fn rejects_direct_and_instance_output_supply_net_drivers() {
        let ty = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
        let source = word::SourceSpan::stable("supply driver rejection test");

        let mut direct = word::WordModule::new("direct_supply_driver");
        let supply = direct.add_wire("supply", ty, source.clone()).unwrap();
        direct
            .set_signal_resolution(supply, word::SignalResolution::SupplyOne)
            .unwrap();
        let zero = direct
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero]).unwrap(),
                ty,
                source.clone(),
            )
            .unwrap();
        direct
            .connect(word::LValue::signal(supply), zero, source.clone())
            .unwrap();
        let error = lower_resolved_nets(&mut direct, &ReferencePortMap::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("explicit driver on supply net 'supply'")
        );

        let mut instance = word::WordModule::new("instance_supply_driver");
        let supply = instance.add_wire("supply", ty, source.clone()).unwrap();
        instance
            .set_signal_resolution(supply, word::SignalResolution::SupplyZero)
            .unwrap();
        let supply_value = instance.read_signal(supply, source.clone()).unwrap();
        instance
            .add_instance(
                "u_source",
                "SOURCE",
                vec![("Y".to_string(), supply_value, source.clone())],
                source,
            )
            .unwrap();
        let references = ReferencePortMap::from([(
            "SOURCE".to_string(),
            BTreeMap::from([(
                "Y".to_string(),
                crate::ReferencePort {
                    direction: word::PortDirection::Output,
                    width: 1,
                    exact_width: true,
                },
            )]),
        )]);
        let error = lower_resolved_nets(&mut instance, &references).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("instance output connection 'u_source.Y' drives supply net 'supply'")
        );
    }
}
