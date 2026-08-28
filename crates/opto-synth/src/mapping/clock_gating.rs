// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod catalog;

use opto_ir::word;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) use catalog::ClockGatingCatalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Clock-gating circuitry selection, mirroring `set_clock_gating_style`.
pub struct ClockGatingStyle {
    /// Smallest register-bank width eligible for gating.
    pub minimum_bitwidth: usize,
    /// Whether the gate must contain an enable latch.
    pub latch_based: bool,
}

impl Default for ClockGatingStyle {
    fn default() -> Self {
        Self {
            minimum_bitwidth: 3,
            latch_based: true,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClockGatingSummary {
    pub(crate) gates: usize,
    pub(crate) registers: usize,
    pub(crate) gated_bits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BankKey {
    region: Option<crate::RegionRowId>,
    clock: word::ValueId,
    enable: word::ValueId,
    active_high: bool,
    rising: bool,
}

struct BankMember {
    operation: word::OpId,
    width: usize,
    source: word::SourceSpan,
    register_name: Option<String>,
}

#[cfg(test)]
pub(crate) fn gate_register_clocks(
    module: &mut word::WordModule,
    catalog: &ClockGatingCatalog,
    style: ClockGatingStyle,
) -> Result<ClockGatingSummary, crate::SynthError> {
    let operation_regions = vec![None; module.operations().len()];
    gate_register_clocks_in_regions(module, catalog, style, &operation_regions)
}

pub(super) fn gate_register_clocks_in_regions(
    module: &mut word::WordModule,
    catalog: &ClockGatingCatalog,
    style: ClockGatingStyle,
    operation_regions: &[Option<crate::RegionRowId>],
) -> Result<ClockGatingSummary, crate::SynthError> {
    if operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "clock-gating region snapshot does not cover the operation arena",
        ));
    }
    let mut summary = ClockGatingSummary::default();
    if !catalog.gates_any_edge() || style.minimum_bitwidth == 0 {
        return Ok(summary);
    }
    let driven = driven_operations(module);
    let mut banks: BTreeMap<BankKey, Vec<BankMember>> = BTreeMap::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let word::OpKind::Register(register) = &operation.kind else {
            continue;
        };
        let Some(enable) = register.enable else {
            continue;
        };
        let operation_id = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        if !driven.contains(&operation_id) {
            continue;
        }
        let Some(width) = module
            .value(register.d)
            .map(|value| value.ty.width() as usize)
        else {
            continue;
        };
        banks
            .entry(BankKey {
                region: operation_regions[operation_id.index()],
                clock: register.clock,
                enable: enable.value,
                active_high: enable.active_high,
                rising: matches!(register.edge, word::Edge::Pos),
            })
            .or_default()
            .push(BankMember {
                operation: operation_id,
                width,
                source: operation.source.clone(),
                register_name: register.name.map(|name| module.name_str(name).to_string()),
            });
    }
    let mut generated = crate::mapping::word_util::GeneratedNames::new(module)?;
    for (key, members) in banks {
        let bits = members.iter().map(|member| member.width).sum::<usize>();
        if bits < style.minimum_bitwidth {
            continue;
        }
        let edge = if key.rising {
            word::Edge::Pos
        } else {
            word::Edge::Neg
        };
        let Some(gate) = catalog.gate_for(edge, style.latch_based) else {
            continue;
        };
        let source = members[0].source.clone();
        let enable = if key.active_high {
            key.enable
        } else {
            module
                .unary(word::UnaryOp::BitNot, key.enable, source.clone())
                .map_err(crate::SynthError::from)?
        };
        let gated_clock =
            crate::mapping::word_util::add_generated_wire_value(&mut generated, module, &source)?;
        let cell = crate::mapping::MappedCell {
            cell_name: gate.name.clone(),
            input_connections: [
                crate::mapping::MappedInputConnection {
                    pin: gate.clock_pin.clone(),
                    value: key.clock,
                },
                crate::mapping::MappedInputConnection {
                    pin: gate.enable_pin.clone(),
                    value: enable,
                },
            ]
            .into_iter()
            .collect(),
            output_connections: [crate::mapping::MappedOutputConnection {
                pin: gate.output_pin.clone(),
                value: gated_clock,
            }]
            .into_iter()
            .collect(),
        };
        let instance = match members
            .iter()
            .find_map(|member| member.register_name.as_deref())
        {
            Some(register) => {
                super::word_util::GeneratedNames::preferred_instance(module, "clk_gate_", register)?
            }
            None => generated.instance()?,
        };
        let connections = cell
            .input_connections
            .into_iter()
            .map(|connection| (connection.pin, connection.value, source.clone()))
            .chain(
                cell.output_connections
                    .into_iter()
                    .map(|connection| (connection.pin, connection.value, source.clone())),
            )
            .collect();
        module
            .add_instance(instance, cell.cell_name, connections, source.clone())
            .map_err(crate::SynthError::from)?;
        for member in &members {
            let operation = module.operation_mut(member.operation).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "gated register {:?} disappeared during clock gating",
                    member.operation
                ))
            })?;
            let word::OpKind::Register(register) = &mut operation.kind else {
                return Err(crate::SynthError::invariant(format!(
                    "gated candidate {:?} is not a register",
                    member.operation
                )));
            };
            register.clock = gated_clock;
            register.enable = None;
        }
        summary.gates += 1;
        summary.registers += members.len();
        summary.gated_bits += bits;
    }
    Ok(summary)
}

fn driven_operations(module: &word::WordModule) -> BTreeSet<word::OpId> {
    module
        .connects()
        .iter()
        .filter_map(|connect| match module.value(connect.value)?.kind {
            word::ValueKind::Operation(operation) => Some(operation),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
