// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{CellId, NetId, PinId, TimingEndpoint};
use opto_core::{NameId, NameTable};
use std::mem::size_of;

#[derive(Debug, Clone, Copy)]
struct NamedBinding<T> {
    name: NameId,
    object: T,
}

#[derive(Debug, Clone, Copy)]
struct PinBinding {
    // Zero denotes a delimiter-free name. A split instance stores NameId + 1,
    // preserving the builder's pre-existing acceptance of arbitrary strings
    // without making the common row wider.
    instance: u32,
    pin: NameId,
    object: PinId,
}

/// Immutable persistent database identities indexed by one compact name arena.
///
/// Text is stored once in [`NameTable`]. Cells and nets keep one name ID per
/// row; pins keep separate instance and pin-component IDs instead of storing
/// every repeated `instance/pin` full name. Each object class is sorted by its
/// compact key, so lookup performs intern-table probes followed by one binary
/// search. There is no heap node per object.
#[derive(Debug)]
pub struct TimingObjectBindings {
    names: NameTable,
    cells: Box<[NamedBinding<CellId>]>,
    pins: Box<[PinBinding]>,
    nets: Box<[NamedBinding<NetId>]>,
}

impl Default for TimingObjectBindings {
    fn default() -> Self {
        Self {
            names: NameTable::new(),
            cells: Box::new([]),
            pins: Box::new([]),
            nets: Box::new([]),
        }
    }
}

impl TimingObjectBindings {
    #[must_use]
    /// Creates an empty, already sealed binding table.
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    /// Starts append-only construction of a binding table.
    pub fn builder() -> TimingObjectBindingsBuilder {
        TimingObjectBindingsBuilder::default()
    }

    #[must_use]
    /// Resolves a persistent cell endpoint by its current flat instance name.
    pub fn cell_endpoint(&self, name: &str) -> Option<TimingEndpoint> {
        lookup(&self.names, &self.cells, name).map(TimingEndpoint::Cell)
    }

    #[must_use]
    /// Resolves a persistent pin endpoint by its `instance/pin` full name.
    pub fn pin_endpoint(&self, full_name: &str) -> Option<TimingEndpoint> {
        let key = match full_name.rsplit_once('/') {
            Some((instance, pin)) => (
                self.names.get(instance)?.raw().checked_add(1)?,
                self.names.get(pin)?,
            ),
            None => (0, self.names.get(full_name)?),
        };
        self.pins
            .binary_search_by_key(&key, |row| (row.instance, row.pin))
            .ok()
            .map(|index| TimingEndpoint::Pin(self.pins[index].object))
    }

    #[must_use]
    /// Resolves a persistent logical-net endpoint by its current name.
    pub fn net_endpoint(&self, name: &str) -> Option<TimingEndpoint> {
        lookup(&self.names, &self.nets, name).map(TimingEndpoint::Net)
    }

    pub(crate) fn cell(&self, name: &str) -> Option<TimingEndpoint> {
        self.cell_endpoint(name)
    }

    pub(crate) fn pin(&self, full_name: &str) -> Option<TimingEndpoint> {
        self.pin_endpoint(full_name)
    }

    pub(crate) fn net(&self, name: &str) -> Option<TimingEndpoint> {
        self.net_endpoint(name)
    }

    #[must_use]
    pub(crate) fn resident_memory_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.names.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<NamedBinding<CellId>>(
                self.cells.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<PinBinding>(
                self.pins.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<NamedBinding<NetId>>(
                self.nets.len(),
            ))
    }
}

fn lookup<T: Copy>(names: &NameTable, rows: &[NamedBinding<T>], name: &str) -> Option<T> {
    let name = names.get(name)?;
    rows.binary_search_by_key(&name, |row| row.name)
        .ok()
        .map(|index| rows[index].object)
}

/// Append-only builder for [`TimingObjectBindings`].
///
/// Construction keeps only compact name-ID keys and typed objects. `finish`
/// sorts in place, rejects conflicting duplicate names, compacts the name
/// arena, and returns the only type accepted by analysis. The finished table
/// is shared by `Arc`; it deliberately has no deep-clone path.
#[derive(Debug, Default)]
pub struct TimingObjectBindingsBuilder {
    names: NameTable,
    cells: Vec<NamedBinding<CellId>>,
    pins: Vec<PinBinding>,
    nets: Vec<NamedBinding<NetId>>,
}

impl TimingObjectBindingsBuilder {
    /// Adds a flat instance-name binding to a persistent cell ID.
    ///
    /// Duplicate names are accepted during construction and diagnosed by
    /// [`Self::finish`], which can distinguish identical rows from conflicts.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::Capacity`] if the compact interned-name table
    /// cannot represent another distinct name.
    pub fn bind_cell(
        &mut self,
        name: impl AsRef<str>,
        id: CellId,
    ) -> Result<(), crate::TimingError> {
        let name = self.intern(name.as_ref())?;
        self.cells.push(NamedBinding { name, object: id });
        Ok(())
    }

    /// Adds a persistent pin binding from `instance/pin` or a flat pin name.
    ///
    /// The final slash separates the instance and library-pin components;
    /// names without a slash are stored as top-level pin bindings.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::Capacity`] if either interned name or the
    /// nonzero encoded instance-name key exceeds compact representation.
    pub fn bind_pin(
        &mut self,
        full_name: impl AsRef<str>,
        id: PinId,
    ) -> Result<(), crate::TimingError> {
        let full_name = full_name.as_ref();
        let (instance, pin) = if let Some((instance, pin)) = full_name.rsplit_once('/') {
            let instance = self.intern(instance)?.raw().checked_add(1).ok_or(
                crate::TimingModelError::Capacity {
                    resource: "timing pin instance-name key",
                },
            )?;
            (instance, self.intern(pin)?)
        } else {
            (0, self.intern(full_name)?)
        };
        self.pins.push(PinBinding {
            instance,
            pin,
            object: id,
        });
        Ok(())
    }

    /// Adds a flat timing-net name binding to a persistent database net ID.
    ///
    /// Conflicting duplicate names are rejected when [`Self::finish`] seals
    /// the canonical sorted table.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::Capacity`] if the compact interned-name table
    /// cannot represent another distinct name.
    pub fn bind_net(&mut self, name: impl AsRef<str>, id: NetId) -> Result<(), crate::TimingError> {
        let name = self.intern(name.as_ref())?;
        self.nets.push(NamedBinding { name, object: id });
        Ok(())
    }

    /// Seals canonical rows without allocating a tree or a second string set.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-object-binding error when one canonical cell, pin,
    /// or net name maps to conflicting persistent IDs.
    pub fn finish(mut self) -> Result<TimingObjectBindings, crate::TimingError> {
        canonicalize(&mut self.cells, "cell")?;
        canonicalize_pins(&mut self.pins)?;
        canonicalize(&mut self.nets, "net")?;
        self.names.compact();
        Ok(TimingObjectBindings {
            names: self.names,
            cells: self.cells.into_boxed_slice(),
            pins: self.pins.into_boxed_slice(),
            nets: self.nets.into_boxed_slice(),
        })
    }

    fn intern(&mut self, name: &str) -> Result<NameId, crate::TimingError> {
        self.names.intern(name).map_err(|_| {
            crate::TimingModelError::Capacity {
                resource: "timing object name table",
            }
            .into()
        })
    }
}

fn canonicalize<T: Copy + PartialEq>(
    rows: &mut Vec<NamedBinding<T>>,
    kind: &'static str,
) -> Result<(), crate::TimingError> {
    rows.sort_unstable_by_key(|row| row.name);
    if rows
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name && pair[0].object != pair[1].object)
    {
        return Err(crate::TimingModelError::DuplicateObjectBinding { kind }.into());
    }
    rows.dedup_by(|left, right| left.name == right.name);
    Ok(())
}

fn canonicalize_pins(rows: &mut Vec<PinBinding>) -> Result<(), crate::TimingError> {
    rows.sort_unstable_by_key(|row| (row.instance, row.pin));
    if rows.windows(2).any(|pair| {
        pair[0].instance == pair[1].instance
            && pair[0].pin == pair[1].pin
            && pair[0].object != pair[1].object
    }) {
        return Err(crate::TimingModelError::DuplicateObjectBinding { kind: "pin" }.into());
    }
    rows.dedup_by(|left, right| left.instance == right.instance && left.pin == right.pin);
    Ok(())
}
