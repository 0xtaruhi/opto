// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Exact per-bit producer identity over immutable Word IR.
//!
//! Partition placement is deliberately absent from this analysis. Signal
//! connects and width-only projections are semantic connectivity, so clients
//! must resolve them before consulting any region owner. Ordinary operations,
//! state, unresolved physical boundaries, and resolved nets remain explicit
//! producer endpoints.

use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;

// Exact aliases can revisit one packed value at different bit offsets, so the
// walk needs a deterministic bound independent of host resources.
const MAX_BIT_ALIAS_STEPS: usize = 1 << 16;

/// Canonical source of one Word bit before placement or implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitSource {
    /// One bit produced by a non-projection Word value.
    Value { value: word::ValueId, bit: u32 },
    /// One source-domain constant bit.
    Constant(opto_ir::BitVal),
}

/// Read-only bit connectivity frozen from one Word module.
pub(crate) struct BitConnectivity<'a> {
    module: &'a word::WordModule,
    drivers: super::signal_driver::SignalDriverIndex,
    canonical_signal_views: BTreeMap<word::SignalId, Box<[SignalView]>>,
    source_cache: RwLock<BTreeMap<(word::ValueId, u32), BitSource>>,
}

#[derive(Clone, Copy)]
struct SignalView {
    value: word::ValueId,
    lsb: u32,
    width: u32,
}

enum ResolutionStep {
    Source(BitSource),
    Alias(word::ValueId, u32),
}

impl<'a> BitConnectivity<'a> {
    pub(crate) fn new(module: &'a word::WordModule) -> Result<Self, crate::SynthError> {
        let mut canonical_signal_views = BTreeMap::<_, Vec<_>>::new();
        for (index, stored) in module.values().iter().enumerate() {
            let word::ValueKind::Signal(reference) = stored.kind else {
                continue;
            };
            let value = word::ValueId::from_index(index).map_err(crate::SynthError::from)?;
            canonical_signal_views
                .entry(reference.signal)
                .or_default()
                .push(SignalView {
                    value,
                    lsb: reference.lsb,
                    width: stored.ty.width(),
                });
        }
        Ok(Self {
            module,
            drivers: super::signal_driver::SignalDriverIndex::new(module)?,
            canonical_signal_views: canonical_signal_views
                .into_iter()
                .map(|(signal, views)| (signal, views.into_boxed_slice()))
                .collect(),
            source_cache: RwLock::new(BTreeMap::new()),
        })
    }

    /// Resolve `value[bit]` through exact connects and width-only projections.
    pub(crate) fn source(
        &self,
        value: word::ValueId,
        bit: u32,
    ) -> Result<BitSource, crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("bit connectivity references an unknown Word value")
        })?;
        if bit >= stored.ty.width() {
            return Err(crate::SynthError::invariant(
                "bit connectivity exceeds its Word value",
            ));
        }
        let mut active = BTreeSet::new();
        let mut path = Vec::new();
        let mut current = (value, bit);
        let source = loop {
            if let Some(source) = self
                .source_cache
                .read()
                .map_err(|_| crate::SynthError::invariant("bit connectivity cache is poisoned"))?
                .get(&current)
                .copied()
            {
                break source;
            }
            if path.len() == MAX_BIT_ALIAS_STEPS {
                return Err(crate::SynthError::invariant(format!(
                    "bit connectivity exceeds the {MAX_BIT_ALIAS_STEPS}-step alias limit"
                )));
            }
            if !active.insert(current) {
                return Err(crate::SynthError::invariant(
                    "bit connectivity contains an exact-alias cycle",
                ));
            }
            path.push(current);
            match resolution_step(
                self.module,
                &self.drivers,
                &self.canonical_signal_views,
                current.0,
                current.1,
            )? {
                ResolutionStep::Source(source) => break source,
                ResolutionStep::Alias(value, bit) => current = (value, bit),
            }
        };
        let mut sources = self
            .source_cache
            .write()
            .map_err(|_| crate::SynthError::invariant("bit connectivity cache is poisoned"))?;
        for bit in path {
            sources.insert(bit, source);
        }
        Ok(source)
    }

    /// Resolves the unique structural driver of one physical signal bit.
    pub(crate) fn signal_source(
        &self,
        signal: word::SignalId,
        bit: u32,
    ) -> Result<Option<BitSource>, crate::SynthError> {
        self.drivers
            .resolve_bit(signal, bit)
            .map(|(value, bit)| self.source(value, bit))
            .transpose()
    }

    pub(crate) fn exact_reference_driver(
        &self,
        reference: word::SignalRef,
        ty: word::WordType,
    ) -> Option<word::ValueId> {
        self.drivers
            .exact_reference_driver(self.module, reference, ty)
    }

    pub(crate) fn reference_drivers(
        &self,
        reference: word::SignalRef,
    ) -> Option<Vec<word::ValueId>> {
        self.drivers.reference_drivers(reference)
    }
}

fn resolution_step(
    module: &word::WordModule,
    drivers: &super::signal_driver::SignalDriverIndex,
    canonical_signal_views: &BTreeMap<word::SignalId, Box<[SignalView]>>,
    value: word::ValueId,
    bit: u32,
) -> Result<ResolutionStep, crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant("bit connectivity references an unknown Word value")
    })?;
    if bit >= stored.ty.width() {
        return Err(crate::SynthError::invariant(
            "bit connectivity exceeds its Word value",
        ));
    }
    match &stored.kind {
        word::ValueKind::Constant(bits) => Ok(ResolutionStep::Source(BitSource::Constant(
            bits.bit_lsb(bit).ok_or_else(|| {
                crate::SynthError::invariant("bit connectivity constant bit is absent")
            })?,
        ))),
        word::ValueKind::Signal(reference) => {
            let physical_bit = reference
                .lsb
                .checked_add(bit)
                .ok_or_else(|| crate::SynthError::invariant("signal bit offset overflow"))?;
            if let Some((driver, driver_bit)) = drivers.resolve_bit(reference.signal, physical_bit)
            {
                Ok(ResolutionStep::Alias(driver, driver_bit))
            } else {
                let source = canonical_signal_views
                    .get(&reference.signal)
                    .and_then(|views| {
                        views.iter().find_map(|view| {
                            let end = view.lsb.checked_add(view.width)?;
                            (view.lsb <= physical_bit && physical_bit < end).then(|| {
                                BitSource::Value {
                                    value: view.value,
                                    bit: physical_bit - view.lsb,
                                }
                            })
                        })
                    })
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "undriven signal bit has no canonical Word value",
                        )
                    })?;
                Ok(ResolutionStep::Source(source))
            }
        }
        word::ValueKind::Operation(operation) => {
            let operation = module.operation(*operation).ok_or_else(|| {
                crate::SynthError::invariant("bit connectivity operation is unknown")
            })?;
            match &operation.kind {
                word::OpKind::Extract { value, lsb, .. } => Ok(ResolutionStep::Alias(
                    *value,
                    lsb.checked_add(bit).ok_or_else(|| {
                        crate::SynthError::invariant("bit connectivity extract overflow")
                    })?,
                )),
                word::OpKind::Concat { parts } => {
                    let mut remaining = bit;
                    for &part in parts.iter().rev() {
                        let width = module
                            .value(part)
                            .ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "bit connectivity concatenation part is unknown",
                                )
                            })?
                            .ty
                            .width();
                        if remaining < width {
                            return Ok(ResolutionStep::Alias(part, remaining));
                        }
                        remaining -= width;
                    }
                    Err(crate::SynthError::invariant(
                        "bit connectivity exceeds its concatenation",
                    ))
                }
                word::OpKind::Cast { kind, value, .. } => {
                    let width = module
                        .value(*value)
                        .ok_or_else(|| {
                            crate::SynthError::invariant("bit connectivity cast input is unknown")
                        })?
                        .ty
                        .width();
                    if bit < width {
                        Ok(ResolutionStep::Alias(*value, bit))
                    } else if *kind == word::CastKind::SignExtend {
                        Ok(ResolutionStep::Alias(*value, width - 1))
                    } else {
                        Ok(ResolutionStep::Source(BitSource::Constant(
                            opto_ir::BitVal::Zero,
                        )))
                    }
                }
                word::OpKind::Unary { .. }
                | word::OpKind::Binary { .. }
                | word::OpKind::Mux { .. }
                | word::OpKind::TriState { .. }
                | word::OpKind::Register(_)
                | word::OpKind::Latch(_)
                | word::OpKind::DynamicExtract { .. }
                | word::OpKind::DynamicInsert { .. } => {
                    Ok(ResolutionStep::Source(BitSource::Value { value, bit }))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_views_share_one_canonical_physical_bit() {
        let mut module = word::WordModule::new("canonical_signal_views");
        let signal = module
            .add_wire(
                "value",
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let full = module
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let slice = module
            .read_signal_slice(signal, 2, 1, word::SourceSpan::default())
            .unwrap();

        let connectivity = BitConnectivity::new(&module).unwrap();
        assert_eq!(
            connectivity.source(full, 2).unwrap(),
            connectivity.source(slice, 0).unwrap()
        );
    }

    #[test]
    fn canonical_signal_view_skips_a_disjoint_higher_slice() {
        let mut module = word::WordModule::new("disjoint_signal_views");
        let signal = module
            .add_wire(
                "value",
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal_slice(signal, 2, 1, word::SourceSpan::default())
            .unwrap();
        let low = module
            .read_signal_slice(signal, 0, 1, word::SourceSpan::default())
            .unwrap();

        let connectivity = BitConnectivity::new(&module).unwrap();
        assert_eq!(
            connectivity.source(low, 0).unwrap(),
            BitSource::Value { value: low, bit: 0 }
        );
    }
}
