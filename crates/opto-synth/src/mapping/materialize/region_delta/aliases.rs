// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Static boundary aliases and lowered-Word substrate bindings.

use crate::mapping::RegionPlanBinding;
use crate::mapping::cover::LibraryCoverSource;
use opto_ir::mapped::{ConnectionRef, NetId};
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

/// A scalar lowered Word value resolved against one mapped substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MappedValueSignal {
    Net(NetId),
    Constant(bool),
}

impl MappedValueSignal {
    pub(crate) const fn connection(self) -> ConnectionRef {
        match self {
            Self::Net(net) => ConnectionRef::Net(net),
            Self::Constant(value) => ConnectionRef::Constant(value),
        }
    }
}

/// Explicit lowered-Word-to-mapped-signal bindings created with the substrate.
#[derive(Debug, Clone, Default)]
pub(crate) struct WordMappedSignals {
    signals: BTreeMap<word::ValueId, MappedValueSignal>,
    boundary_aliases: BTreeMap<word::ValueId, BoundaryAliasSource>,
}

impl WordMappedSignals {
    pub(crate) fn from_observations_with_aliases(
        module: &word::WordModule,
        values: &[word::ValueId],
        nets: &[Option<NetId>],
        aliases: &[BoundaryAlias],
    ) -> Result<Self, crate::SynthError> {
        if values.len() != nets.len() {
            return Err(crate::SynthError::invariant(
                "mapped substrate observations have inconsistent lengths",
            ));
        }
        let mut signals = BTreeMap::new();
        for (&value, net) in values.iter().zip(nets) {
            let signal = match net {
                Some(net) => MappedValueSignal::Net(*net),
                None => MappedValueSignal::Constant(scalar_constant(module, value)?),
            };
            if signals
                .insert(value, signal)
                .is_some_and(|previous| previous != signal)
            {
                return Err(crate::SynthError::invariant(format!(
                    "lowered value {value:?} resolved to two mapped signals"
                )));
            }
        }
        let mut boundary_aliases = BTreeMap::new();
        for alias in aliases {
            if boundary_aliases
                .insert(alias.target, alias.source)
                .is_some_and(|previous| previous != alias.source)
            {
                return Err(crate::SynthError::invariant(
                    "one substrate value has conflicting boundary aliases",
                ));
            }
        }
        Ok(Self {
            signals,
            boundary_aliases,
        })
    }

    pub(crate) fn require(
        &self,
        value: word::ValueId,
    ) -> Result<MappedValueSignal, crate::SynthError> {
        self.signals.get(&value).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "regional binding value {value:?} is absent from the mapped substrate"
            ))
        })
    }

    pub(crate) fn boundary_alias(&self, target: word::ValueId) -> Option<BoundaryAliasSource> {
        self.boundary_aliases.get(&target).copied()
    }

    pub(crate) fn validate_alias(
        &self,
        target: word::ValueId,
        source: Option<BoundaryAliasSource>,
    ) -> Result<(), crate::SynthError> {
        let normalize = |source| {
            if let Some(MappedValueSignal::Constant(value)) = self.signals.get(&target) {
                BoundaryAliasSource::Constant(*value)
            } else if let BoundaryAliasSource::Value(value) = source
                && let Some(MappedValueSignal::Constant(value)) = self.signals.get(&value)
            {
                BoundaryAliasSource::Constant(*value)
            } else {
                source
            }
        };
        let expected = self.boundary_aliases.get(&target).copied().map(normalize);
        let source = source.map(normalize);
        (expected == source).then_some(()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "regional boundary alias for {target:?} differs from the frozen substrate"
            ))
        })
    }
}

/// Collects values to observe while building the one-time mapped substrate.
pub(crate) fn regional_binding_values(bindings: &[RegionPlanBinding]) -> Box<[word::ValueId]> {
    bindings
        .iter()
        .flat_map(RegionPlanBinding::lowered_values)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BoundaryAliasSource {
    Value(word::ValueId),
    Constant(bool),
}

/// Static pass-through or constant output folded into the substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BoundaryAlias {
    pub(crate) target: word::ValueId,
    pub(crate) source: BoundaryAliasSource,
}

pub(crate) fn regional_boundary_aliases(
    module: &word::WordModule,
    plans: &[crate::RegionCoverPlan],
    bindings: &[RegionPlanBinding],
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<Box<[BoundaryAlias]>, crate::SynthError> {
    if plans.len() != bindings.len() {
        return Err(crate::SynthError::invariant(
            "regional alias inputs have inconsistent row counts",
        ));
    }
    let mut aliases = Vec::new();
    for (plan, binding) in plans.iter().zip(bindings) {
        let payload = plan.payload();
        if payload.is_empty() {
            continue;
        }
        if !payload.starts_with(b"ORCP\x02") {
            return Err(crate::SynthError::invariant(
                "regional plan payload has an unknown alias ABI",
            ));
        }
        aliases.extend(library_boundary_aliases(plan, binding, ownership)?);
    }
    aliases.sort_unstable();
    aliases.dedup();
    deduplicate_equivalent_aliases(module, &aliases).map(Vec::into_boxed_slice)
}

fn deduplicate_equivalent_aliases(
    module: &word::WordModule,
    aliases: &[BoundaryAlias],
) -> Result<Vec<BoundaryAlias>, crate::SynthError> {
    let mut known_bits = word::KnownBitsAnalysis::new(module);
    let mut deduplicated = Vec::with_capacity(aliases.len());
    let mut start = 0;
    while start < aliases.len() {
        let target = aliases[start].target;
        let end = aliases[start..]
            .iter()
            .position(|alias| alias.target != target)
            .map_or(aliases.len(), |offset| start + offset);
        let group = &aliases[start..end];
        let expected = effective_alias_source(module, &mut known_bits, group[0]);
        if let Some(conflicting) = group
            .iter()
            .copied()
            .find(|&alias| effective_alias_source(module, &mut known_bits, alias) != expected)
        {
            return Err(crate::SynthError::invariant(format!(
                "regional output {target:?} ({}) has conflicting static aliases {:?} ({}) and {:?} ({})",
                value_description(module, target),
                group[0].source,
                alias_source_description(module, group[0].source),
                conflicting.source,
                alias_source_description(module, conflicting.source),
            )));
        }
        let representative = group
            .iter()
            .copied()
            .min_by_key(|alias| alias_source_rank(module, alias.source))
            .expect("an alias group is non-empty");
        deduplicated.push(representative);
        start = end;
    }
    Ok(deduplicated)
}

fn effective_alias_source(
    module: &word::WordModule,
    known_bits: &mut word::KnownBitsAnalysis,
    alias: BoundaryAlias,
) -> BoundaryAliasSource {
    match known_bits.bit(module, alias.target, 0) {
        word::KnownBit::Zero => return BoundaryAliasSource::Constant(false),
        word::KnownBit::One => return BoundaryAliasSource::Constant(true),
        word::KnownBit::Unknown => {}
    }
    let BoundaryAliasSource::Value(value) = alias.source else {
        return alias.source;
    };
    match known_bits.bit(module, value, 0) {
        word::KnownBit::Zero => BoundaryAliasSource::Constant(false),
        word::KnownBit::One => BoundaryAliasSource::Constant(true),
        word::KnownBit::Unknown => alias.source,
    }
}

fn alias_source_rank(module: &word::WordModule, source: BoundaryAliasSource) -> u8 {
    match source {
        BoundaryAliasSource::Value(value)
            if module
                .value(value)
                .is_some_and(|stored| matches!(stored.kind, word::ValueKind::Constant(_))) =>
        {
            0
        }
        BoundaryAliasSource::Constant(_) => 1,
        BoundaryAliasSource::Value(_) => 2,
    }
}

fn alias_source_description(module: &word::WordModule, source: BoundaryAliasSource) -> String {
    match source {
        BoundaryAliasSource::Value(value) => value_description(module, value),
        BoundaryAliasSource::Constant(value) => format!("constant {value}"),
    }
}

fn value_description(module: &word::WordModule, value: word::ValueId) -> String {
    match module.value(value).map(|stored| &stored.kind) {
        Some(word::ValueKind::Operation(operation)) => format!(
            "operation {:?}",
            module
                .operation(*operation)
                .map(|operation| &operation.kind)
        ),
        Some(kind) => format!("{kind:?}"),
        None => "unknown value".to_string(),
    }
}

pub(crate) fn library_boundary_aliases(
    plan: &crate::RegionCoverPlan,
    binding: &RegionPlanBinding,
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<Box<[BoundaryAlias]>, crate::SynthError> {
    let cover = crate::mapping::cover::decode_portable_cover(plan.payload())?;
    let inputs = binding.resolve_inputs(ownership)?;
    let outputs = binding.resolve_output_groups(ownership)?;
    if cover.outputs().len() != outputs.len() {
        return Err(crate::SynthError::invariant(
            "portable regional outputs disagree with their alias binding",
        ));
    }
    let mut aliases = Vec::new();
    for (targets, source) in outputs.iter().zip(cover.outputs()) {
        let terminal = match *source {
            LibraryCoverSource::Constant(value) => Some(BoundaryAliasSource::Constant(value)),
            LibraryCoverSource::Input(index) => Some(BoundaryAliasSource::Value(
                inputs.get(index).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional alias input exceeds its revision binding",
                    )
                })?,
            )),
            LibraryCoverSource::Cell(_) | LibraryCoverSource::CellSecond(_) => None,
        };
        append_output_aliases(&mut aliases, targets, terminal);
    }
    aliases.sort_unstable();
    aliases.dedup();
    Ok(aliases.into_boxed_slice())
}

pub(crate) fn append_output_aliases(
    aliases: &mut Vec<BoundaryAlias>,
    targets: &[word::ValueId],
    terminal: Option<BoundaryAliasSource>,
) {
    let Some((&canonical, remaining)) = targets.split_first() else {
        return;
    };
    if terminal.is_some_and(|source| source != BoundaryAliasSource::Value(canonical)) {
        aliases.push(BoundaryAlias {
            target: canonical,
            source: terminal.expect("checked terminal is present"),
        });
    }
    aliases.extend(remaining.iter().copied().map(|target| BoundaryAlias {
        target,
        source: BoundaryAliasSource::Value(canonical),
    }));
}

fn scalar_constant(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let Some(word::ValueKind::Constant(bits)) = module.value(value).map(|stored| &stored.kind)
    else {
        return Err(crate::SynthError::invariant(format!(
            "non-constant substrate value {value:?} has no mapped net"
        )));
    };
    crate::boolean::logic::logic_constant(bits).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "substrate constant {value:?} is not a known scalar bit"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_equivalent_alias_sources_keep_the_original_union_topology() {
        let mut module = word::WordModule::new("aliases");
        let bit = word::WordType::bits(1).unwrap();
        let zero = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .add_port(
                "input",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .read_signal(
                module.port(input).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let mut aliases = vec![
            BoundaryAlias {
                target: zero,
                source: BoundaryAliasSource::Value(zero),
            },
            BoundaryAlias {
                target: zero,
                source: BoundaryAliasSource::Value(input),
            },
        ];

        aliases.sort_unstable();
        aliases.dedup();
        let aliases = deduplicate_equivalent_aliases(&module, &aliases).unwrap();

        assert_eq!(
            aliases,
            [BoundaryAlias {
                target: zero,
                source: BoundaryAliasSource::Value(zero),
            }]
        );
    }
}
