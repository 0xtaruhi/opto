// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Static driver map used to substitute signal reads during FSM proofs.
//!
//! A signal may be driven by several connects that each cover a disjoint static
//! bit range, so resolution is per bit rather than per signal. A signal is
//! opaque when any of its connects covers bits that cannot be determined
//! statically; every read of such a signal is then unresolved, because a
//! runtime-indexed target may alias any bit.

use opto_core::PackedRows;
use opto_ir::word;

#[derive(Debug, Clone, Copy)]
struct DriverSpan {
    base: u32,
    descending: bool,
    width: u32,
}

#[derive(Debug, Clone, Copy, Default)]
enum ResolvedDriver {
    #[default]
    Missing,
    Unique {
        value: word::ValueId,
        bit: u32,
    },
    Multiple,
    Opaque,
}

#[derive(Debug)]
pub(crate) struct SignalDriverIndex {
    rows: PackedRows<word::ValueId>,
    resolved: PackedRows<ResolvedDriver>,
}

impl SignalDriverIndex {
    pub(crate) fn new(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        let mut entries = Vec::with_capacity(module.connects().len());
        let mut resolved = module
            .signals()
            .iter()
            .map(|signal| vec![ResolvedDriver::Missing; signal.ty.width() as usize])
            .collect::<Vec<_>>();
        for connect in module.connects() {
            let row = connect.target.signal.index();
            if row >= module.signals().len() {
                return Err(crate::SynthError::invariant(format!(
                    "connect targets unknown signal {:?}",
                    connect.target.signal
                )));
            }
            if module.signals()[row].resolution == word::SignalResolution::TriState {
                // A resolved tri-state signal is a physical boundary net. Its
                // local contributions are materialized as driver cells and
                // must never replace reads that can also observe an external
                // or another enabled driver.
                continue;
            }
            let span = driver_span(module, connect);
            entries.push((row, connect.value));
            let signal_bits = resolved.get_mut(row).ok_or_else(|| {
                crate::SynthError::invariant("connect target has no driver-resolution row")
            })?;
            let Some(span) = span else {
                signal_bits.fill(ResolvedDriver::Opaque);
                if let Some(dynamic) = connect.target.dynamic {
                    entries.push((row, dynamic.offset));
                }
                continue;
            };
            if signal_bits
                .first()
                .is_some_and(|driver| matches!(driver, ResolvedDriver::Opaque))
            {
                continue;
            }
            for driver_bit in 0..span.width {
                let signal_bit = if span.descending {
                    span.base.checked_sub(driver_bit)
                } else {
                    span.base.checked_add(driver_bit)
                }
                .and_then(|bit| signal_bits.get_mut(bit as usize))
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "static connect span exceeds its target signal width",
                    )
                })?;
                *signal_bit = match *signal_bit {
                    ResolvedDriver::Missing => ResolvedDriver::Unique {
                        value: connect.value,
                        bit: driver_bit,
                    },
                    ResolvedDriver::Unique { .. } | ResolvedDriver::Multiple => {
                        ResolvedDriver::Multiple
                    }
                    ResolvedDriver::Opaque => ResolvedDriver::Opaque,
                };
            }
            if let Some(dynamic) = connect.target.dynamic {
                entries.push((row, dynamic.offset));
            }
        }
        let rows = PackedRows::try_from_entries(module.signals().len(), entries)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        let resolved = PackedRows::try_from_rows(resolved)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        Ok(Self { rows, resolved })
    }

    /// Every value feeding `signal`, including runtime index operands.
    pub(crate) fn values(
        &self,
        signal: word::SignalId,
    ) -> impl Iterator<Item = word::ValueId> + '_ {
        self.rows
            .get(signal.index())
            .unwrap_or_default()
            .iter()
            .copied()
    }

    /// Resolve every bit of `reference` to the driver value and driver bit that
    /// supplies it, or `None` when any bit is opaque, undriven, or driven more
    /// than once.
    pub(crate) fn resolve_reference(
        &self,
        reference: word::SignalRef,
    ) -> Option<Vec<(word::ValueId, u32)>> {
        let row = self.resolved.get(reference.signal.index())?;
        let mut bits = Vec::with_capacity(reference.width() as usize);
        for offset in 0..reference.width() {
            let bit = reference.lsb.checked_add(offset)?;
            let ResolvedDriver::Unique { value, bit } = *row.get(bit as usize)? else {
                return None;
            };
            bits.push((value, bit));
        }
        Some(bits)
    }

    /// Resolves one signal bit to its unique driver and source-bit offset.
    pub(crate) fn resolve_bit(
        &self,
        signal: word::SignalId,
        bit: u32,
    ) -> Option<(word::ValueId, u32)> {
        let row = self.resolved.get(signal.index())?;
        match *row.get(bit as usize)? {
            ResolvedDriver::Unique { value, bit } => Some((value, bit)),
            ResolvedDriver::Missing | ResolvedDriver::Multiple | ResolvedDriver::Opaque => None,
        }
    }

    /// Resolves an exact scalar signal read to its scalar driving value.
    pub(crate) fn scalar_driver(
        &self,
        module: &word::WordModule,
        reference: word::SignalRef,
    ) -> Option<word::ValueId> {
        let resolved = self.resolve_reference(reference)?;
        let [(driver, 0)] = resolved.as_slice() else {
            return None;
        };
        module
            .value(*driver)
            .is_some_and(|stored| stored.ty.width() == 1)
            .then_some(*driver)
    }

    /// The distinct driver values feeding `reference`, in first-use order, or
    /// `None` when the reference is unresolved.
    pub(crate) fn reference_drivers(
        &self,
        reference: word::SignalRef,
    ) -> Option<Vec<word::ValueId>> {
        let mut drivers = Vec::new();
        for (value, _) in self.resolve_reference(reference)? {
            if drivers.last().copied() != Some(value) && !drivers.contains(&value) {
                drivers.push(value);
            }
        }
        Some(drivers)
    }

    /// Resolves a reference that is a complete, ordered projection of one
    /// equal-typed driver value.
    pub(crate) fn exact_reference_driver(
        &self,
        module: &word::WordModule,
        reference: word::SignalRef,
        ty: word::WordType,
    ) -> Option<word::ValueId> {
        let bits = self.resolve_reference(reference)?;
        let &(driver, _) = bits.first()?;
        bits.iter()
            .enumerate()
            .all(|(offset, &(candidate, bit))| {
                candidate == driver && usize::try_from(bit).ok() == Some(offset)
            })
            .then(|| module.value(driver))
            .flatten()
            .filter(|value| value.ty == ty)
            .map(|_| driver)
    }
}

fn driver_span(module: &word::WordModule, connect: &word::Connect) -> Option<DriverSpan> {
    if connect.target.dynamic.is_some() {
        return None;
    }
    let width = module.value(connect.value)?.ty.width();
    match connect.target.range {
        Some(range) if range.width() == width => Some(DriverSpan {
            base: range.lsb,
            descending: range.msb < range.lsb,
            width,
        }),
        Some(_) => None,
        None => (module.signal(connect.target.signal)?.ty.width() == width).then_some(DriverSpan {
            base: 0,
            descending: false,
            width,
        }),
    }
}

#[cfg(test)]
mod tests;
