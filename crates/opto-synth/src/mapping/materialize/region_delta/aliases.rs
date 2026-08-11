// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Lowered-Word bindings resolved against the frozen global substrate.

use crate::mapping::RegionPlanBinding;
use opto_ir::mapped::{ConnectionRef, NetId};
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

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

#[derive(Debug, Clone, Default)]
pub(crate) struct WordMappedSignals {
    signals: BTreeMap<word::ValueId, MappedValueSignal>,
}

impl WordMappedSignals {
    pub(crate) fn from_observations(
        module: &word::WordModule,
        values: &[word::ValueId],
        nets: &[Option<NetId>],
    ) -> Result<Self, crate::SynthError> {
        if values.len() != nets.len() {
            return Err(crate::SynthError::invariant(
                "mapped substrate observations have inconsistent lengths",
            ));
        }
        let mut signals = BTreeMap::new();
        for (value, net) in values.iter().copied().zip(nets.iter().copied()) {
            let signal = match net {
                Some(net) => MappedValueSignal::Net(net),
                None => MappedValueSignal::Constant(scalar_constant(module, value)?),
            };
            if signals
                .insert(value, signal)
                .is_some_and(|old| old != signal)
            {
                return Err(crate::SynthError::invariant(format!(
                    "mapped substrate value {value:?} has conflicting observations"
                )));
            }
        }
        Ok(Self { signals })
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
}

pub(crate) fn regional_binding_values<'a>(
    bindings: impl IntoIterator<Item = &'a RegionPlanBinding>,
) -> Box<[word::ValueId]> {
    bindings
        .into_iter()
        .flat_map(RegionPlanBinding::lowered_values)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scalar_constant(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "non-constant substrate value {value:?} has no mapped net"
        ))
    })?;
    let word::ValueKind::Constant(bits) = &stored.kind else {
        return Err(crate::SynthError::invariant(format!(
            "substrate value {value:?} without a mapped net is not constant"
        )));
    };
    let [bit] = bits.as_slice() else {
        return Err(crate::SynthError::invariant(format!(
            "substrate constant {value:?} is not scalar"
        )));
    };
    crate::boolean::resolve_synthesis_bit(*bit, module.name(), &stored.source)
        .map(|resolved| resolved == opto_ir::BitVal::One)
}
