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
use std::collections::BTreeSet;

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
}

impl<'a> BitConnectivity<'a> {
    pub(crate) fn new(module: &'a word::WordModule) -> Result<Self, crate::SynthError> {
        Ok(Self {
            module,
            drivers: super::signal_driver::SignalDriverIndex::new(module)?,
        })
    }

    /// Resolve `value[bit]` through exact connects and width-only projections.
    pub(crate) fn source(
        &self,
        value: word::ValueId,
        bit: u32,
    ) -> Result<BitSource, crate::SynthError> {
        self.resolve(value, bit, &mut BTreeSet::new())
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

    fn resolve(
        &self,
        value: word::ValueId,
        bit: u32,
        active: &mut BTreeSet<(word::ValueId, u32)>,
    ) -> Result<BitSource, crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("bit connectivity references an unknown Word value")
        })?;
        if bit >= stored.ty.width() {
            return Err(crate::SynthError::invariant(
                "bit connectivity exceeds its Word value",
            ));
        }
        if !active.insert((value, bit)) {
            return Err(crate::SynthError::invariant(
                "bit connectivity contains an exact-alias cycle",
            ));
        }
        let source = match &stored.kind {
            word::ValueKind::Constant(bits) => {
                BitSource::Constant(bits.bit_lsb(bit).ok_or_else(|| {
                    crate::SynthError::invariant("bit connectivity constant bit is absent")
                })?)
            }
            word::ValueKind::Signal(reference) => {
                if let Some((driver, driver_bit)) = self.drivers.resolve_bit(
                    reference.signal,
                    reference.lsb.checked_add(bit).ok_or_else(|| {
                        crate::SynthError::invariant("signal bit offset overflow")
                    })?,
                ) {
                    self.resolve(driver, driver_bit, active)?
                } else {
                    BitSource::Value { value, bit }
                }
            }
            word::ValueKind::Operation(operation) => {
                let operation = self.module.operation(*operation).ok_or_else(|| {
                    crate::SynthError::invariant("bit connectivity operation is unknown")
                })?;
                match &operation.kind {
                    word::OpKind::Extract { value, lsb, .. } => self.resolve(
                        *value,
                        lsb.checked_add(bit).ok_or_else(|| {
                            crate::SynthError::invariant("bit connectivity extract overflow")
                        })?,
                        active,
                    )?,
                    word::OpKind::Concat { parts } => {
                        let mut remaining = bit;
                        let mut source = None;
                        for &part in parts.iter().rev() {
                            let width = self
                                .module
                                .value(part)
                                .ok_or_else(|| {
                                    crate::SynthError::invariant(
                                        "bit connectivity concatenation part is unknown",
                                    )
                                })?
                                .ty
                                .width();
                            if remaining < width {
                                source = Some(self.resolve(part, remaining, active)?);
                                break;
                            }
                            remaining -= width;
                        }
                        source.ok_or_else(|| {
                            crate::SynthError::invariant(
                                "bit connectivity exceeds its concatenation",
                            )
                        })?
                    }
                    word::OpKind::Cast { kind, value, .. } => {
                        let width = self
                            .module
                            .value(*value)
                            .ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "bit connectivity cast input is unknown",
                                )
                            })?
                            .ty
                            .width();
                        if bit < width {
                            self.resolve(*value, bit, active)?
                        } else if *kind == word::CastKind::SignExtend {
                            self.resolve(*value, width - 1, active)?
                        } else {
                            BitSource::Constant(opto_ir::BitVal::Zero)
                        }
                    }
                    word::OpKind::Unary { .. }
                    | word::OpKind::Binary { .. }
                    | word::OpKind::Mux { .. }
                    | word::OpKind::TriState { .. }
                    | word::OpKind::Register(_)
                    | word::OpKind::Latch(_)
                    | word::OpKind::DynamicExtract { .. }
                    | word::OpKind::DynamicInsert { .. } => BitSource::Value { value, bit },
                }
            }
        };
        active.remove(&(value, bit));
        Ok(source)
    }
}
