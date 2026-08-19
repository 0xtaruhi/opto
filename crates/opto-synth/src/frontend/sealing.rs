// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Seals the source-observable SSA domain after procedural and net lowering.

use crate::ReferencePortMap;
use opto_ir::{BitVal, ConstBits, word};

/// Gives source-undefined bits an explicit care-free SSA value.
///
/// This is a source-semantic completion step, not a driver repair. After all
/// source drivers have been lowered, it completes holes in partially driven
/// internal single-driver aggregates and otherwise-undefined output bits.
/// Wholly undriven internals, dynamic targets, resolved nets, and generated
/// drivers remain invalid or retain their existing boundary semantics.
pub(super) fn seal_observable_dont_cares(
    module: &mut word::WordModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), crate::SynthError> {
    let mut driven = module
        .signals()
        .iter()
        .map(|signal| vec![false; signal.ty.width() as usize])
        .collect::<Vec<_>>();

    for read in module.memory_read_ports() {
        mark_all(&mut driven, read.data)?;
    }
    for instance in module.instances() {
        let reference = module.name_str(instance.module);
        let Some(ports) = reference_ports.get(reference) else {
            continue;
        };
        for connection in &instance.connections {
            let port = module.name_str(connection.port);
            if !ports.get(port).is_some_and(|port| {
                matches!(
                    port.direction,
                    word::PortDirection::Output | word::PortDirection::Inout
                )
            }) {
                continue;
            }
            for fragment in module
                .signal_fragments(connection.value)
                .map_err(crate::SynthError::from)?
            {
                mark_range(
                    &mut driven,
                    fragment.reference.signal,
                    fragment.reference.lsb,
                    fragment.reference.width(),
                )?;
            }
        }
    }
    let mut has_dynamic_target = vec![false; module.signals().len()];
    for connect in module.connects() {
        if connect.target.dynamic.is_some() {
            let dynamic = has_dynamic_target
                .get_mut(connect.target.signal.index())
                .ok_or_else(|| {
                    crate::SynthError::invariant("dynamic driver targets an unknown signal")
                })?;
            *dynamic = true;
            continue;
        }
        let signal = module
            .signal(connect.target.signal)
            .ok_or_else(|| crate::SynthError::invariant("SSA driver targets an unknown signal"))?;
        let (lsb, width) = connect
            .target
            .range
            .map_or((0, signal.ty.width()), |range| {
                (range.lsb.min(range.msb), range.width())
            });
        mark_range(&mut driven, connect.target.signal, lsb, width)?;
    }

    let internal = module
        .signals()
        .iter()
        .enumerate()
        .filter(|(index, signal)| {
            matches!(
                signal.kind,
                word::SignalKind::Wire | word::SignalKind::Register
            ) && signal.resolution == word::SignalResolution::SingleDriver
                && !has_dynamic_target[*index]
                && driven[*index].iter().any(|bit| *bit)
                && driven[*index].iter().any(|bit| !*bit)
        })
        .map(|(index, signal)| {
            Ok((
                word::SignalId::from_index(index).map_err(crate::SynthError::from)?,
                signal.ty,
                signal.source.clone(),
                missing_ranges(&driven[index])?,
            ))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    for (signal, ty, source, ranges) in internal {
        seal_ranges(module, signal, ty, &source, ranges)?;
    }

    let outputs = module
        .ports()
        .iter()
        .filter(|port| port.direction == word::PortDirection::Output)
        .map(|port| (port.signal, port.ty, port.source.clone()))
        .collect::<Vec<_>>();
    for (signal, ty, source) in outputs {
        let row = driven.get(signal.index()).ok_or_else(|| {
            crate::SynthError::invariant("output port references an unknown signal")
        })?;
        seal_ranges(module, signal, ty, &source, missing_ranges(row)?)?;
    }
    Ok(())
}

fn missing_ranges(row: &[bool]) -> Result<Vec<(u32, u32)>, crate::SynthError> {
    let mut ranges = Vec::new();
    let mut bit = 0usize;
    while bit < row.len() {
        if row[bit] {
            bit += 1;
            continue;
        }
        let start = bit;
        while bit < row.len() && !row[bit] {
            bit += 1;
        }
        ranges.push((
            u32::try_from(start)
                .map_err(|_| crate::SynthError::capacity("signal bit index exceeds u32"))?,
            u32::try_from(bit - start)
                .map_err(|_| crate::SynthError::capacity("signal range width exceeds u32"))?,
        ));
    }
    Ok(ranges)
}

fn seal_ranges(
    module: &mut word::WordModule,
    signal: word::SignalId,
    ty: word::WordType,
    source: &word::SourceSpan,
    ranges: Vec<(u32, u32)>,
) -> Result<(), crate::SynthError> {
    let fill = if ty.state() == word::LogicStateKind::FourState {
        BitVal::X
    } else {
        BitVal::Zero
    };
    for (lsb, width) in ranges {
        let value_ty = if lsb == 0 && width == ty.width() {
            ty
        } else {
            word::WordType::new(width, false, ty.state()).map_err(crate::SynthError::from)?
        };
        let value = module
            .constant(
                ConstBits::from_bits(vec![fill; width as usize])
                    .map_err(|error| crate::SynthError::invalid(error.to_string()))?,
                value_ty,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let target = if lsb == 0 && width == ty.width() {
            word::LValue::signal(signal)
        } else {
            word::LValue::signal(signal).with_range(word::BitRange {
                msb: lsb
                    .checked_add(width - 1)
                    .ok_or_else(|| crate::SynthError::capacity("signal range exceeds u32"))?,
                lsb,
            })
        };
        module
            .connect(target, value, source.clone())
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

fn mark_all(driven: &mut [Vec<bool>], signal: word::SignalId) -> Result<(), crate::SynthError> {
    let row = driven
        .get_mut(signal.index())
        .ok_or_else(|| crate::SynthError::invariant("driver references an unknown signal"))?;
    row.fill(true);
    Ok(())
}

fn mark_range(
    driven: &mut [Vec<bool>],
    signal: word::SignalId,
    lsb: u32,
    width: u32,
) -> Result<(), crate::SynthError> {
    let row = driven
        .get_mut(signal.index())
        .ok_or_else(|| crate::SynthError::invariant("driver references an unknown signal"))?;
    let start = usize::try_from(lsb)
        .map_err(|_| crate::SynthError::capacity("driver bit index exceeds usize"))?;
    let width = usize::try_from(width)
        .map_err(|_| crate::SynthError::capacity("driver width exceeds usize"))?;
    let end = start
        .checked_add(width)
        .ok_or_else(|| crate::SynthError::capacity("driver range exceeds usize"))?;
    let range = row
        .get_mut(start..end)
        .ok_or_else(|| crate::SynthError::invariant("driver range exceeds signal width"))?;
    range.fill(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_only_missing_observable_output_bits() {
        let mut module = word::WordModule::new("partial_output");
        let ty = word::WordType::bits(4).unwrap();
        let output = module
            .add_port(
                "out",
                word::PortDirection::Output,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module.port(output).unwrap().signal;
        let internal = module
            .add_wire("internal", ty, word::SourceSpan::default())
            .unwrap();
        let one = module
            .constant(
                ConstBits::from_bits(vec![BitVal::One]).unwrap(),
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(output).with_range(word::BitRange { msb: 1, lsb: 1 }),
                one,
                word::SourceSpan::default(),
            )
            .unwrap();

        seal_observable_dont_cares(&mut module, &ReferencePortMap::new()).unwrap();

        let output_drivers = module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == output)
            .collect::<Vec<_>>();
        assert_eq!(output_drivers.len(), 3);
        assert!(output_drivers.iter().skip(1).all(|connect| {
            matches!(
                &module.value(connect.value).unwrap().kind,
                word::ValueKind::Constant(bits)
                    if bits.as_slice().iter().all(|bit| *bit == BitVal::X)
            )
        }));
        assert!(
            module
                .connects()
                .iter()
                .all(|connect| connect.target.signal != internal)
        );
    }

    #[test]
    fn seals_missing_bits_in_partially_driven_internal_aggregate() {
        let mut module = word::WordModule::new("partial_internal");
        let ty = word::WordType::bits(4).unwrap();
        let internal = module
            .add_wire("internal", ty, word::SourceSpan::default())
            .unwrap();
        let one = module
            .constant(
                ConstBits::from_bits(vec![BitVal::One]).unwrap(),
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(internal).with_range(word::BitRange { msb: 1, lsb: 1 }),
                one,
                word::SourceSpan::default(),
            )
            .unwrap();

        seal_observable_dont_cares(&mut module, &ReferencePortMap::new()).unwrap();

        let drivers = module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == internal)
            .collect::<Vec<_>>();
        assert_eq!(drivers.len(), 3);
        assert!(drivers.iter().skip(1).all(|connect| {
            matches!(
                &module.value(connect.value).unwrap().kind,
                word::ValueKind::Constant(bits)
                    if bits.as_slice().iter().all(|bit| *bit == BitVal::X)
            )
        }));
    }

    #[test]
    fn does_not_seal_signals_with_dynamic_targets() {
        let mut module = word::WordModule::new("dynamic_internal");
        let ty = word::WordType::bits(4).unwrap();
        let internal = module
            .add_wire("internal", ty, word::SourceSpan::default())
            .unwrap();
        let bit = module
            .constant(
                ConstBits::from_bits(vec![BitVal::One]).unwrap(),
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let offset = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero, BitVal::Zero]).unwrap(),
                word::WordType::bits(2).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(internal).with_range(word::BitRange { msb: 1, lsb: 1 }),
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(internal)
                    .with_dynamic_range(offset, std::num::NonZeroU32::new(1).unwrap()),
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();

        seal_observable_dont_cares(&mut module, &ReferencePortMap::new()).unwrap();

        assert_eq!(
            module
                .connects()
                .iter()
                .filter(|connect| connect.target.signal == internal)
                .count(),
            2
        );
    }
}
