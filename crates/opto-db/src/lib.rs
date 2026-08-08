// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Persistent design identity, hierarchy, and collection storage.
//!
//! `opto-db` is the session-facing database layer. It assigns permanent
//! [`ObjectUid`] values to designs and their visible
//! objects, resolves typed IDs through [`ObjectRegistry`], and records
//! collections without embedding borrowed object data. Definition and
//! occurrence graphs represent hierarchy separately: a definition is stored
//! once, while each elaborated occurrence has its own compact identity.
//!
//! Mutations are prepared against a revision and committed atomically. Removed
//! UIDs are never recycled, and restoring a checkpoint cannot silently bind an
//! old handle to a newly created object with the same name.

use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use thiserror::Error;

pub use opto_core::{NameError, NameId, NameTable, ObjectUid, RevisionId};

mod collection;
mod hierarchy;
mod object;

pub use collection::Collection;
pub use hierarchy::{
    DefinitionGraph, DefinitionGraphError, DefinitionId, DefinitionInput, DefinitionInstance,
    InstanceInput, LinkBinding, LinkProvider, LinkProviderInput, LinkProviderKind, OccurrenceGraph,
    OccurrenceGraphError, OccurrenceId, ProviderId, UnresolvedOccurrence,
};
pub use object::{
    AnyObjectId, CellId, CellObject, ClockId, ClockObject, DesignId, DesignObject, NetId,
    NetObject, ObjectClass, ObjectId, ObjectIdSet, ObjectKind, ObjectLocator,
    ObjectReconcileDesign, ObjectReconcileMode, ObjectReconcileSource, ObjectRegistry,
    ObjectRegistryCheckpoint, ObjectRegistryMarker, ObjectRegistryReconcilePlan,
    ObjectRegistrySnapshot, ObjectRegistrySnapshotRef, ObjectRemovalView, PinId, PinObject, PortId,
    PortObject, PreparedObjectReconcile, RegistryError, ResolvedObject,
};

/// Direction of a design port as seen from the containing definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// The environment drives the design.
    Input,
    /// The design drives the environment.
    Output,
    /// Both the design and environment may drive the resolved signal.
    Inout,
}

impl Direction {
    /// Returns the canonical lowercase Verilog spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Input => "input",
            Direction::Output => "output",
            Direction::Inout => "inout",
        }
    }
}

/// Named, fixed-width interface port in a [`DesignIndex`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// Interned port name owned by the containing design's name table.
    pub name: NameId,
    /// Direction relative to the containing design.
    pub direction: Direction,
    /// Number of scalar bits; zero-width ports are invalid before indexing.
    pub width: u32,
}

/// Named, fixed-width internal net in a [`DesignIndex`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Net {
    /// Interned net name owned by the containing design's name table.
    pub name: NameId,
    /// Number of scalar bits.
    pub width: u32,
}

/// Design or library-cell instance stored in a [`DesignIndex`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Interned instance name.
    pub name: NameId,
    /// Interned referenced definition or target-cell name.
    pub reference: NameId,
    /// Port connections in source order.
    pub connections: Vec<CellConnection>,
}

impl Cell {
    /// Creates an instance without connections.
    #[must_use]
    pub fn new(name: NameId, reference: NameId) -> Self {
        Self {
            name,
            reference,
            connections: Vec::new(),
        }
    }

    /// Appends a scalar connection and returns the updated instance.
    #[must_use]
    pub fn with_connection(mut self, port: NameId, signal: NameId) -> Self {
        self.connections.push(CellConnection {
            port,
            signals: vec![signal],
        });
        self
    }
}

/// Connection between one instance port and one or more named signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellConnection {
    /// Interned name of the referenced definition's port.
    pub port: NameId,
    /// Connected scalar signal names, ordered least-significant bit first.
    pub signals: Vec<NameId>,
}

/// Compact structural inventory used to populate the persistent object registry.
///
/// Every [`NameId`] in the public vectors belongs to the private name table in
/// this value. Call [`DesignIndex::validate`] after deserialization and before
/// admitting the index into live session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignIndex {
    /// Name of the indexed design definition.
    pub name: String,
    names: NameTable,
    /// Ports in stable source order.
    pub ports: DesignRows<Port>,
    /// Nets in stable source order.
    pub nets: DesignRows<Net>,
    /// Instances in stable source order.
    pub cells: DesignRows<Cell>,
    /// Additional signal names referenced by structural expressions.
    pub used_signals: DesignRows<NameId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExactNameRow {
    name: NameId,
    row: u32,
}

/// Source-ordered design rows with a lazily built compact exact-name index.
///
/// Immutable iteration retains source order. Any mutable access invalidates
/// the derived index before exposing the backing slice, so exact lookup can
/// never depend on whether a query happened before an edit.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DesignRows<T> {
    rows: Vec<T>,
    #[serde(skip)]
    exact_names: OnceLock<Box<[ExactNameRow]>>,
}

impl<T> Default for DesignRows<T> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            exact_names: OnceLock::new(),
        }
    }
}

impl<T: Clone> Clone for DesignRows<T> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
            exact_names: OnceLock::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for DesignRows<T> {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
    }
}

impl<T: Eq> Eq for DesignRows<T> {}

impl<T> From<Vec<T>> for DesignRows<T> {
    fn from(rows: Vec<T>) -> Self {
        Self {
            rows,
            exact_names: OnceLock::new(),
        }
    }
}

impl<T> Deref for DesignRows<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl<T> DerefMut for DesignRows<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.exact_names = OnceLock::new();
        &mut self.rows
    }
}

impl<'a, T> IntoIterator for &'a DesignRows<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut DesignRows<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.exact_names = OnceLock::new();
        self.rows.iter_mut()
    }
}

impl<T> DesignRows<T> {
    fn row(&self, name: NameId, row_name: impl Fn(&T) -> NameId) -> Option<usize> {
        let rows = self
            .exact_names
            .get_or_init(|| exact_name_rows(self.rows.iter().map(row_name)));
        rows.binary_search_by_key(&name, |row| row.name)
            .ok()
            .map(|index| rows[index].row as usize)
    }

    fn contains_name(&self, name: NameId, row_name: impl Fn(&T) -> NameId) -> bool {
        self.row(name, row_name).is_some()
    }
}

/// Validation failure for a serialized [`DesignIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DesignIndexError {
    /// A compact name ID does not exist in the index's name table.
    #[error("design {object} {index} has an invalid {field} name identifier")]
    InvalidName {
        /// The kind of indexed object containing the invalid ID.
        object: &'static str,
        /// The object's zero-based index in its arena.
        index: usize,
        /// The field that contains the invalid name ID.
        field: &'static str,
    },
}

impl DesignIndex {
    /// Creates an empty index with a fresh name table.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            names: NameTable::new(),
            ports: DesignRows::default(),
            nets: DesignRows::default(),
            cells: DesignRows::default(),
            used_signals: DesignRows::default(),
        }
    }

    /// Creates an empty index that takes ownership of an existing name table.
    ///
    /// This is used when a preceding IR phase already interned the canonical
    /// names and the index must preserve their IDs.
    pub fn with_name_table(name: impl Into<String>, names: NameTable) -> Self {
        Self {
            name: name.into(),
            names,
            ports: DesignRows::default(),
            nets: DesignRows::default(),
            cells: DesignRows::default(),
            used_signals: DesignRows::default(),
        }
    }

    /// Interns a design-local name and returns its compact ID.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if the compact ID space is exhausted.
    pub fn intern_name(&mut self, name: &str) -> Result<NameId, NameError> {
        self.names.intern(name)
    }

    /// Resolves a design-local name ID, returning `None` for a foreign ID.
    pub fn resolve_name(&self, name: NameId) -> Option<&str> {
        self.names.resolve(name)
    }

    /// Resolves a name ID known to belong to this design.
    ///
    /// # Panics
    ///
    /// Panics when `name` is not present. Use [`Self::resolve_name`] at
    /// deserialization or external-input boundaries.
    pub fn name_str(&self, name: NameId) -> &str {
        self.resolve_name(name)
            .expect("design object name ID must resolve")
    }

    /// Compares an interned name with a string without allocating.
    pub fn name_eq(&self, name: NameId, expected: &str) -> bool {
        self.name_str(name) == expected
    }

    /// Appends a port in stable source order.
    pub fn add_port(&mut self, port: Port) {
        self.ports.rows.push(port);
        self.ports.exact_names = OnceLock::new();
    }

    /// Appends a net in stable source order.
    pub fn add_net(&mut self, net: Net) {
        self.nets.rows.push(net);
        self.nets.exact_names = OnceLock::new();
    }

    /// Appends an instance in stable source order.
    pub fn add_cell(&mut self, cell: Cell) {
        self.cells.rows.push(cell);
        self.cells.exact_names = OnceLock::new();
    }

    /// Looks up the first port row with an exact interned name.
    pub fn port_by_name(&self, name: &str) -> Option<&Port> {
        self.names
            .get(name)
            .and_then(|name| self.ports.row(name, |port| port.name))
            .and_then(|row| self.ports.get(row))
    }

    /// Looks up the first explicit net row with an exact interned name.
    pub fn net_by_name(&self, name: &str) -> Option<&Net> {
        self.names
            .get(name)
            .and_then(|name| self.nets.row(name, |net| net.name))
            .and_then(|row| self.nets.get(row))
    }

    /// Looks up the first cell row with an exact interned name.
    pub fn cell_by_name(&self, name: &str) -> Option<&Cell> {
        self.names
            .get(name)
            .and_then(|name| self.cells.row(name, |cell| cell.name))
            .and_then(|row| self.cells.get(row))
    }

    /// Returns whether an exact name is a visible logical net.
    ///
    /// Explicit nets and signals referenced by structural expressions are
    /// independently visible, matching the object-registry inventory.
    pub fn is_visible_net_name(&self, name: &str) -> bool {
        let Some(name) = self.names.get(name) else {
            return false;
        };
        self.nets.contains_name(name, |net| net.name)
            || self.used_signals.contains_name(name, |name| *name)
    }

    /// Validates every compact name reference before a deserialized index is
    /// admitted into the live design database.
    ///
    /// # Errors
    ///
    /// Returns [`DesignIndexError`] on the first invalid object/reference name
    /// ID or a mismatch between a row and its secondary name index.
    pub fn validate(&self) -> Result<(), DesignIndexError> {
        for (index, port) in self.ports.iter().enumerate() {
            self.require_name(port.name, "port", index, "object")?;
        }
        for (index, net) in self.nets.iter().enumerate() {
            self.require_name(net.name, "net", index, "object")?;
        }
        for (index, cell) in self.cells.iter().enumerate() {
            self.require_name(cell.name, "cell", index, "object")?;
            self.require_name(cell.reference, "cell", index, "reference")?;
            for connection in &cell.connections {
                self.require_name(connection.port, "cell", index, "connection port")?;
                for &signal in &connection.signals {
                    self.require_name(signal, "cell", index, "connection signal")?;
                }
            }
        }
        for (index, &signal) in self.used_signals.iter().enumerate() {
            self.require_name(signal, "used signal", index, "object")?;
        }
        Ok(())
    }

    fn require_name(
        &self,
        name: NameId,
        object: &'static str,
        index: usize,
        field: &'static str,
    ) -> Result<(), DesignIndexError> {
        self.resolve_name(name)
            .map(|_| ())
            .ok_or(DesignIndexError::InvalidName {
                object,
                index,
                field,
            })
    }
}

fn exact_name_rows(names: impl IntoIterator<Item = NameId>) -> Box<[ExactNameRow]> {
    let mut rows = names
        .into_iter()
        .enumerate()
        .map(|(row, name)| ExactNameRow {
            name,
            row: u32::try_from(row).expect("design object rows fit in the compact name capacity"),
        })
        .collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| (row.name, row.row));
    rows.dedup_by_key(|row| row.name);
    rows.into_boxed_slice()
}

/// Matches a DC-style `*`/`?` wildcard pattern against a complete name.
///
/// An empty pattern and `"*"` both select every name. Matching is byte-wise,
/// deterministic, and does not implement regular-expression syntax.
#[must_use]
pub fn matches_pattern(name: &str, pattern: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    wildcard_match(name.as_bytes(), pattern.as_bytes())
}

fn wildcard_match(name: &[u8], pattern: &[u8]) -> bool {
    let (mut ni, mut pi) = (0usize, 0usize);
    let mut star = None;
    let mut retry = 0usize;
    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == name[ni]) {
            ni += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            retry = ni;
            pi += 1;
        } else if let Some(si) = star {
            pi = si + 1;
            retry += 1;
            ni = retry;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_patterns_match_common_dc_shapes() {
        assert!(matches_pattern("clk", "*"));
        assert!(matches_pattern("data_in", "data*"));
        assert!(matches_pattern("data_in", "data_??"));
        assert!(!matches_pattern("rst_n", "clk*"));
    }

    #[test]
    fn design_builder_stores_structural_hdl_objects() {
        let mut design = DesignIndex::new("top");
        let a = design.intern_name("a").unwrap();
        let n = design.intern_name("n").unwrap();
        let u_child = design.intern_name("u_child").unwrap();
        let child = design.intern_name("child").unwrap();
        let i = design.intern_name("i").unwrap();
        design.add_port(Port {
            name: a,
            direction: Direction::Input,
            width: 1,
        });
        design.add_net(Net { name: n, width: 1 });
        design.add_cell(Cell::new(u_child, child).with_connection(i, n));
        design.used_signals = vec![a, n].into();

        assert_eq!(design.name_str(design.cells[0].reference), "child");
        assert_eq!(design.cells[0].connections[0].signals, vec![n]);
        assert_eq!(design.used_signals.as_slice(), [a, n]);
    }

    #[test]
    fn structural_objects_store_compact_name_ids() {
        assert_eq!(size_of::<NameId>(), 4);
        assert!(size_of::<Port>() <= 12);
        assert!(size_of::<Net>() <= 8);
        assert!(size_of::<Cell>() <= 32);
        assert!(size_of::<CellConnection>() <= 32);
    }

    #[test]
    fn design_index_validation_covers_every_name_reference() {
        let mut design = DesignIndex::new("top");
        let a = design.intern_name("a").unwrap();
        let u_child = design.intern_name("u_child").unwrap();
        let child = design.intern_name("child").unwrap();
        let i = design.intern_name("i").unwrap();
        design.add_port(Port {
            name: a,
            direction: Direction::Input,
            width: 1,
        });
        design.add_net(Net { name: a, width: 1 });
        design.add_cell(Cell::new(u_child, child).with_connection(i, a));
        design.used_signals.push(a);
        assert_eq!(design.validate(), Ok(()));

        design.cells[0].connections[0].signals[0] = NameId::from_index(u32::MAX as usize).unwrap();
        assert_eq!(
            design.validate(),
            Err(DesignIndexError::InvalidName {
                object: "cell",
                index: 0,
                field: "connection signal",
            })
        );
    }
}
