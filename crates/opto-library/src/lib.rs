// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Liberty import and immutable target-library views.
//!
//! Parsers convert Liberty text into typed cell, pin, timing, power, and
//! wire-load models. [`LibraryRevision`] owns immutable imported records;
//! ordered selections derive [`TargetCellSet`] and timing views without copying
//! cell names or changing the published revision.
//!
//! Selection order is semantically significant. When several libraries provide
//! the same cell name, the first selected provider wins, while ambiguous
//! definitions inside one effective provider are rejected. Fingerprints cover
//! effective synthesis semantics rather than allocation order, making them
//! suitable for cache and checkpoint validation.

mod error;
mod function;
mod liberty;
mod lookup_table;
mod parser;
mod power;
mod selection;
mod target_cells;
mod timing;
mod timing_model;

pub use error::{BooleanFunctionErrorKind, LibraryError, LibrarySyntaxErrorKind};
pub use function::BooleanFunction;
pub use lookup_table::{LookupTable, max_optional_f64};
pub use parser::*;
pub use power::*;
pub use selection::*;
pub use target_cells::*;
pub use timing::*;
pub use timing_model::*;

use opto_core::RevisionId;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Stable semantic digest of a library revision or effective selection.
///
/// Fingerprints exclude arena grouping and allocation order. Equal values mean
/// that the library semantics relevant to the requested operation are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct LibraryFingerprint([u8; 32]);

impl LibraryFingerprint {
    /// Returns the raw 256-bit digest.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Immutable, revisioned set of imported Liberty libraries.
///
/// Revisions share unchanged records through [`Arc`]. Loading libraries or
/// changing `dont_use` policy publishes a new revision instead of mutating
/// views already held by synthesis or timing.
#[derive(Debug)]
pub struct LibraryRevision {
    id: RevisionId,
    records: Arc<[Arc<LibraryRecord>]>,
}

/// User-visible identity of one imported Liberty library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryInfo {
    /// Liberty `library(...)` name.
    pub name: String,
    /// Source path supplied by the caller.
    pub source: String,
}

/// One provider in the ordered design-link search plan.
#[derive(Debug, Clone)]
pub enum LibraryLinkProvider {
    /// Search analyzed design definitions at this position.
    DesignMemory,
    /// Search cells from one selected Liberty library.
    Library {
        /// Identity of the selected library.
        library: LibraryInfo,
        /// Effective target cells after policy filtering.
        cells: TargetCellSet,
    },
}

/// Resolved ordered providers used to link design instances.
#[derive(Debug, Clone)]
pub struct LibraryLinkPlan {
    providers: Vec<LibraryLinkProvider>,
}

impl LibraryLinkPlan {
    /// Borrows providers in link-search order.
    #[must_use]
    pub fn providers(&self) -> &[LibraryLinkProvider] {
        &self.providers
    }

    /// Consumes the plan and returns its ordered providers.
    #[must_use]
    pub fn into_providers(self) -> Vec<LibraryLinkProvider> {
        self.providers
    }
}

enum ResolvedLibraryEntry<'a> {
    DesignMemory,
    Library(&'a LibraryRecord),
}

struct ResolvedLibrarySelection<'a> {
    entries: Vec<ResolvedLibraryEntry<'a>>,
}

impl<'a> ResolvedLibrarySelection<'a> {
    fn records(&self) -> Vec<&'a LibraryRecord> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                ResolvedLibraryEntry::DesignMemory => None,
                ResolvedLibraryEntry::Library(record) => Some(*record),
            })
            .collect()
    }
}

#[derive(Debug)]
struct LibraryRecord {
    name: String,
    source: String,
    default_operating_conditions: Option<String>,
    default_wire_load: Option<String>,
    default_wire_load_mode: Option<String>,
    wire_loads: BTreeMap<String, WireLoadModel>,
    wire_load_tree: WireLoadTree,
    units: TimingLibraryUnits,
    power_units: PowerLibraryUnits,
    target_cells: TargetCellSet,
    power_cells: Arc<[PowerCell]>,
}

#[derive(Serialize)]
struct FingerprintRecord<'a> {
    name: &'a str,
    default_operating_conditions: &'a Option<String>,
    default_wire_load: &'a Option<String>,
    default_wire_load_mode: &'a Option<String>,
    wire_loads: &'a BTreeMap<String, WireLoadModel>,
    wire_load_tree: WireLoadTree,
    units: TimingLibraryUnits,
    power_units: PowerLibraryUnits,
    target_cells: LibraryFingerprint,
    power_cells: &'a [PowerCell],
}

struct FingerprintRecords<'a>(&'a [Arc<LibraryRecord>]);

impl Serialize for FingerprintRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&record.fingerprint_record())?;
        }
        sequence.end()
    }
}

struct SelectionFingerprintEntries<'a>(&'a [ResolvedLibraryEntry<'a>]);

impl Serialize for SelectionFingerprintEntries<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            sequence.serialize_element(&SelectionFingerprintEntry(entry))?;
        }
        sequence.end()
    }
}

struct SelectionFingerprintEntry<'a>(&'a ResolvedLibraryEntry<'a>);

impl Serialize for SelectionFingerprintEntry<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            ResolvedLibraryEntry::DesignMemory => {
                serializer.serialize_unit_variant("SelectionFingerprintEntry", 0, "DesignMemory")
            }
            ResolvedLibraryEntry::Library(record) => serializer.serialize_newtype_variant(
                "SelectionFingerprintEntry",
                1,
                "Library",
                &record.fingerprint_record(),
            ),
        }
    }
}

impl LibraryRecord {
    fn new(library: LibraryImport) -> Self {
        let LibraryImport {
            name,
            source,
            default_operating_conditions,
            default_wire_load,
            default_wire_load_mode,
            wire_loads,
            wire_load_tree,
            units,
            power_units,
            target_cells,
            power_cells,
            timing_models: _,
            cell_count: _,
            pin_count: _,
        } = library;
        Self {
            name,
            source,
            default_operating_conditions,
            default_wire_load,
            default_wire_load_mode,
            wire_loads,
            wire_load_tree,
            units,
            power_units,
            target_cells,
            power_cells,
        }
    }
}

impl LibraryRevision {
    /// Returns the publication revision of this immutable library set.
    #[must_use]
    pub fn id(&self) -> RevisionId {
        self.id
    }

    /// Resolves an ordered selection into unique effective target cells.
    ///
    /// The first selected provider wins when different libraries define the
    /// same cell name.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the selection names an unknown library or an
    /// effective provider sequence cannot be resolved.
    pub fn target_cells(
        &self,
        selection: &LibrarySelection,
    ) -> Result<TargetCellSet, LibraryError> {
        let resolved = self.resolve_selection(selection)?;
        let records = resolved.records();
        Ok(unique_target_cells(&records))
    }

    /// Fingerprints all loaded library content in publication order.
    #[must_use]
    pub fn content_fingerprint(&self) -> LibraryFingerprint {
        fingerprint_serializable(&FingerprintRecords(&self.records))
    }

    /// Fingerprints the effective ordered provider plan rather than raw
    /// selector spelling or unrelated loaded libraries.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the selection contains an unknown library.
    pub fn selection_fingerprint(
        &self,
        selection: &LibrarySelection,
    ) -> Result<LibraryFingerprint, LibraryError> {
        let resolved = self.resolve_selection(selection)?;
        Ok(fingerprint_serializable(&SelectionFingerprintEntries(
            &resolved.entries,
        )))
    }

    /// Returns selected library identities in effective provider order.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the selection contains an unknown library.
    pub fn selected_libraries(
        &self,
        selection: &LibrarySelection,
    ) -> Result<Vec<LibraryInfo>, LibraryError> {
        Ok(self
            .resolve_selection(selection)?
            .records()
            .into_iter()
            .map(LibraryRecord::info)
            .collect())
    }

    /// Resolves the ordered providers needed by the design linker.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the selection contains an unknown library.
    pub fn link_plan(&self, selection: &LibrarySelection) -> Result<LibraryLinkPlan, LibraryError> {
        let resolved = self.resolve_selection(selection)?;
        let providers = resolved
            .entries
            .into_iter()
            .map(|entry| match entry {
                ResolvedLibraryEntry::DesignMemory => LibraryLinkProvider::DesignMemory,
                ResolvedLibraryEntry::Library(record) => LibraryLinkProvider::Library {
                    library: record.info(),
                    cells: record.target_cells.clone(),
                },
            })
            .collect();
        Ok(LibraryLinkPlan { providers })
    }

    /// Builds a timing-and-power view from the effective selection.
    ///
    /// Global units and defaults come from the first selected library, matching
    /// the first-provider semantics used for cells.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the selection contains an unknown library.
    pub fn timing_library(
        &self,
        selection: &LibrarySelection,
    ) -> Result<TimingLibrary, LibraryError> {
        let records = self.resolve_selection(selection)?.records();
        let metadata = records.first().copied();
        Ok(TimingLibrary {
            name: metadata.map(|record| record.name.clone()),
            operating_conditions: metadata
                .and_then(|record| record.default_operating_conditions.clone()),
            wire_load: metadata.and_then(|record| record.default_wire_load.clone()),
            wire_load_mode: metadata.and_then(|record| record.default_wire_load_mode.clone()),
            wire_load_model: metadata.and_then(|record| {
                record
                    .default_wire_load
                    .as_ref()
                    .and_then(|name| record.wire_loads.get(name))
                    .cloned()
            }),
            units: metadata.map_or_else(TimingLibraryUnits::default, |record| record.units),
            wire_load_tree: metadata
                .map_or_else(WireLoadTree::default, |record| record.wire_load_tree),
            power: PowerLibrary {
                units: metadata
                    .map_or_else(PowerLibraryUnits::default, |record| record.power_units),
                cells: PowerCellSet::from_groups(unique_power_cell_groups(&records)),
            },
            cells: unique_target_cells(&records),
        })
    }

    #[must_use]
    /// Returns the number of loaded source libraries in this revision.
    pub fn library_count(&self) -> usize {
        self.records.len()
    }

    /// Select every loaded library in deterministic publication order.
    #[must_use]
    pub fn all_libraries(&self, include_design_memory: bool) -> LibrarySelection {
        LibrarySelection::from_library_names(
            self.records.iter().map(|record| record.name.as_str()),
            include_design_memory,
        )
    }

    /// Return loaded library metadata in deterministic publication order.
    #[must_use]
    pub fn libraries(&self) -> Vec<LibraryInfo> {
        self.records.iter().map(|record| record.info()).collect()
    }

    /// Conservative resident bytes owned by the canonical loaded-library
    /// revision, including native arena storage and nested model payloads.
    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<Arc<LibraryRecord>>(self.records.len()).saturating_add(
            self.records
                .iter()
                .map(|record| record.resident_memory_bytes())
                .sum::<usize>(),
        )
    }

    fn resolve_selection(
        &self,
        selection: &LibrarySelection,
    ) -> Result<ResolvedLibrarySelection<'_>, LibraryError> {
        let mut seen_selectors = BTreeSet::new();
        let mut selected_records = BTreeSet::new();
        let mut entries = Vec::new();

        for selector in selection.selectors() {
            let token = selector.token();
            if !seen_selectors.insert(token) {
                continue;
            }
            let LibrarySelector::Library(name) = selector else {
                entries.push(ResolvedLibraryEntry::DesignMemory);
                continue;
            };
            let matches = self
                .records
                .iter()
                .enumerate()
                .filter(|(_, record)| record.matches_selector(name))
                .collect::<Vec<_>>();
            let Some(&(index, record)) = matches.first() else {
                continue;
            };
            if matches.len() != 1 {
                return Err(LibraryError::AmbiguousLibrarySelector {
                    selector: name.clone(),
                    libraries: matches
                        .into_iter()
                        .map(|(_, record)| record.description())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
            if !selected_records.insert(index) {
                continue;
            }
            entries.push(ResolvedLibraryEntry::Library(record.as_ref()));
        }
        Ok(ResolvedLibrarySelection { entries })
    }
}

fn fingerprint_serializable(value: &impl Serialize) -> LibraryFingerprint {
    let mut digest = blake3::Hasher::new();
    opto_archive::encode_into_std_write(value, &mut digest)
        .expect("typed library content must be serializable");
    LibraryFingerprint(*digest.finalize().as_bytes())
}

fn serialized_size(value: &impl Serialize) -> usize {
    opto_archive::serialized_size(value).expect("validated library content must be serializable")
}

impl LibraryRecord {
    fn matches_selector(&self, selector: &str) -> bool {
        selector == self.name || library_source_names(&self.source).contains(selector)
    }

    fn description(&self) -> String {
        format!("'{}' from '{}'", self.name, self.source)
    }

    fn info(&self) -> LibraryInfo {
        LibraryInfo {
            name: self.name.clone(),
            source: self.source.clone(),
        }
    }

    fn fingerprint_record(&self) -> FingerprintRecord<'_> {
        FingerprintRecord {
            name: &self.name,
            default_operating_conditions: &self.default_operating_conditions,
            default_wire_load: &self.default_wire_load,
            default_wire_load_mode: &self.default_wire_load_mode,
            wire_loads: &self.wire_loads,
            wire_load_tree: self.wire_load_tree,
            units: self.units,
            power_units: self.power_units,
            target_cells: self.target_cells.content_fingerprint(),
            power_cells: &self.power_cells,
        }
    }

    fn resident_memory_bytes(&self) -> usize {
        library_resident_memory_bytes(&LibraryMemoryView {
            name: &self.name,
            source: &self.source,
            default_operating_conditions: self.default_operating_conditions.as_deref(),
            default_wire_load: self.default_wire_load.as_deref(),
            default_wire_load_mode: self.default_wire_load_mode.as_deref(),
            wire_loads: &self.wire_loads,
            target_cells: &self.target_cells,
            power_cells: &self.power_cells,
        })
    }
}

impl LibraryImport {
    /// Conservative native bytes retained by this parsed library artifact.
    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        library_resident_memory_bytes(&LibraryMemoryView {
            name: &self.name,
            source: &self.source,
            default_operating_conditions: self.default_operating_conditions.as_deref(),
            default_wire_load: self.default_wire_load.as_deref(),
            default_wire_load_mode: self.default_wire_load_mode.as_deref(),
            wire_loads: &self.wire_loads,
            target_cells: &self.target_cells,
            power_cells: &self.power_cells,
        })
    }
}

struct LibraryMemoryView<'a> {
    name: &'a str,
    source: &'a str,
    default_operating_conditions: Option<&'a str>,
    default_wire_load: Option<&'a str>,
    default_wire_load_mode: Option<&'a str>,
    wire_loads: &'a BTreeMap<String, WireLoadModel>,
    target_cells: &'a TargetCellSet,
    power_cells: &'a [PowerCell],
}

fn library_resident_memory_bytes(library: &LibraryMemoryView<'_>) -> usize {
    let LibraryMemoryView {
        name,
        source,
        default_operating_conditions,
        default_wire_load,
        default_wire_load_mode,
        wire_loads,
        target_cells,
        power_cells,
    } = library;
    let text = [
        Some(*name),
        Some(*source),
        default_operating_conditions.as_ref().copied(),
        default_wire_load.as_ref().copied(),
        default_wire_load_mode.as_ref().copied(),
    ]
    .into_iter()
    .flatten()
    .map(|value| opto_core::resident::allocation_bytes(value.len()))
    .sum::<usize>();
    let wire_loads = serialized_size(wire_loads);
    let power_payload = serialized_size(&power_cells);
    text.saturating_add(opto_core::resident::allocation_bytes(wire_loads))
        .saturating_add(target_cells.canonical_resident_memory_bytes())
        .saturating_add(opto_core::resident::slice_bytes::<PowerCell>(
            power_cells.len(),
        ))
        .saturating_add(opto_core::resident::allocation_bytes(power_payload))
}

fn unique_target_cells(records: &[&LibraryRecord]) -> TargetCellSet {
    TargetCellSet::first_by_name(records.iter().map(|record| record.target_cells.clone()))
}

fn unique_power_cell_groups(records: &[&LibraryRecord]) -> Vec<Arc<[PowerCell]>> {
    let mut names = BTreeSet::new();
    records
        .iter()
        .filter_map(|record| {
            let mut local_names = BTreeSet::new();
            let needs_filter = record.power_cells.iter().any(|cell| {
                names.contains(cell.name.as_str()) || !local_names.insert(cell.name.as_str())
            });
            if !needs_filter {
                names.extend(local_names);
                return Some(Arc::clone(&record.power_cells));
            }
            let cells = record
                .power_cells
                .iter()
                .filter(|cell| names.insert(cell.name.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            (!cells.is_empty()).then(|| Arc::from(cells))
        })
        .collect()
}

fn cell_matches_pattern(library: &str, cell: &str, pattern: &str) -> bool {
    match pattern.split_once('/') {
        Some((library_pattern, cell_pattern)) => {
            glob_match(library_pattern, library) && glob_match(cell_pattern, cell)
        }
        None => glob_match(pattern, cell),
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut restart: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            restart = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = restart {
            p = star_p + 1;
            t = star_t + 1;
            restart = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

/// Returns every stable selector spelling derived from a library source path.
///
/// The set contains the original path, its file name, and its file stem when
/// those components are valid UTF-8.
#[must_use]
pub fn library_source_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    add_source_name_variants(&mut names, source);
    names
}

fn add_source_name_variants(names: &mut BTreeSet<String>, source: &str) {
    names.insert(source.to_string());
    let path = Path::new(source);
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        names.insert(file_name.to_string());
    }
    if let Some(file_stem) = path.file_stem().and_then(|name| name.to_str()) {
        names.insert(file_stem.to_string());
    }
}

/// Mutable publication point for immutable [`LibraryRevision`] values.
#[derive(Debug, Clone)]
pub struct LibraryStore {
    current: Arc<LibraryRevision>,
}

impl Default for LibraryStore {
    fn default() -> Self {
        Self {
            current: Arc::new(LibraryRevision {
                id: RevisionId::INITIAL,
                records: Arc::from([]),
            }),
        }
    }
}

impl LibraryStore {
    #[must_use]
    /// Returns the currently published immutable library revision.
    pub fn current(&self) -> Arc<LibraryRevision> {
        Arc::clone(&self.current)
    }

    /// Applies `dont_use` glob patterns and publishes a new revision if needed.
    ///
    /// The returned pair is `(matched cells, cells whose policy changed)`.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] for an invalid pattern, target-set capacity
    /// failure, or publication revision overflow.
    pub fn set_dont_use(&mut self, patterns: &[String]) -> Result<(usize, usize), LibraryError> {
        let mut matched_cells = 0usize;
        let mut changed_cells = 0usize;
        let mut next = self.current.records.iter().cloned().collect::<Vec<_>>();
        for record in &mut next {
            let (cells, matches, changes) = record.target_cells.with_dont_use(|cell| {
                patterns
                    .iter()
                    .any(|pattern| cell_matches_pattern(&record.name, cell, pattern))
            })?;
            matched_cells += matches;
            if changes == 0 {
                continue;
            }
            changed_cells += changes;
            *record = Arc::new(LibraryRecord {
                name: record.name.clone(),
                source: record.source.clone(),
                default_operating_conditions: record.default_operating_conditions.clone(),
                default_wire_load: record.default_wire_load.clone(),
                default_wire_load_mode: record.default_wire_load_mode.clone(),
                wire_loads: record.wire_loads.clone(),
                wire_load_tree: record.wire_load_tree,
                units: record.units,
                power_units: record.power_units,
                target_cells: cells,
                power_cells: Arc::clone(&record.power_cells),
            });
        }
        if changed_cells == 0 {
            return Ok((matched_cells, 0));
        }
        self.current = Arc::new(LibraryRevision {
            id: self.current.id.next()?,
            records: next.into(),
        });
        Ok((matched_cells, changed_cells))
    }

    /// Appends parsed libraries and publishes one new revision.
    ///
    /// An empty input is a no-op and leaves the revision unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if an imported library violates synthesis
    /// invariants or the publication revision overflows.
    pub fn append(
        &mut self,
        libraries: Vec<LibraryImport>,
    ) -> Result<LibraryLoadReport, LibraryError> {
        let report = LibraryLoadReport {
            libraries: libraries.len(),
            cells: libraries.iter().map(|library| library.cell_count).sum(),
            pins: libraries.iter().map(|library| library.pin_count).sum(),
            timing_models: libraries.iter().map(|library| library.timing_models).sum(),
        };
        if libraries.is_empty() {
            return Ok(report);
        }

        let next_id = self.current.id.next()?;
        let mut next = self.current.records.iter().cloned().collect::<Vec<_>>();
        next.extend(libraries.into_iter().map(LibraryRecord::new).map(Arc::new));
        self.current = Arc::new(LibraryRevision {
            id: next_id,
            records: next.into(),
        });
        Ok(report)
    }
}

/// Inventory of objects admitted by one library-load operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryLoadReport {
    /// Number of Liberty library records.
    pub libraries: usize,
    /// Number of imported cell definitions.
    pub cells: usize,
    /// Number of imported cell pins.
    pub pins: usize,
    /// Imported timing models by representation.
    pub timing_models: TimingModelCounts,
}

/// Counts of imported delay and waveform model families.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimingModelCounts {
    /// Non-linear delay model tables.
    pub nldm: usize,
    /// Composite current source models.
    pub ccs: usize,
    /// Effective current source models.
    pub ecsm: usize,
}

impl std::iter::Sum for TimingModelCounts {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |mut total, counts| {
            total.nldm += counts.nldm;
            total.ccs += counts.ccs;
            total.ecsm += counts.ecsm;
            total
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_publish_immutable_library_revisions() {
        let mut store = LibraryStore::default();
        let original = store.current();
        let library = parser::parse_liberty("library(demo) {}", "demo.lib").unwrap();
        let report = store.append(vec![library]).unwrap();

        assert_eq!(report.libraries, 1);
        assert_eq!(original.library_count(), 0);
        assert_eq!(store.current().library_count(), 1);
        assert_eq!(store.current().id(), original.id().next().unwrap());
    }

    #[test]
    fn set_dont_use_marks_matching_cells_in_a_new_revision() {
        const LIB: &str = r#"
library(demo) {
  cell(INVX1) { pin(A) { direction : input; } pin(Y) { direction : output; function : "!A"; } }
  cell(INVX2) { pin(A) { direction : input; } pin(Y) { direction : output; function : "!A"; } }
  cell(BUFX1) { pin(A) { direction : input; } pin(Y) { direction : output; function : "A"; } }
}
"#;
        let mut store = LibraryStore::default();
        let library = parser::parse_liberty(LIB, "demo.lib").unwrap();
        store.append(vec![library]).unwrap();
        let before = store.current();

        let changed = store.set_dont_use(&["INVX*".to_string()]).unwrap();

        assert_eq!(changed, (2, 2));
        assert_eq!(store.current().id(), before.id().next().unwrap());
        let selection = LibrarySelection::parse("demo");
        let cells = store.current().target_cells(&selection).unwrap();
        let dont_use = cells
            .iter()
            .filter(|cell| cell.dont_use())
            .map(|cell| cell.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(dont_use, ["INVX1", "INVX2"]);
        assert!(
            before
                .target_cells(&selection)
                .unwrap()
                .iter()
                .all(|cell| !cell.dont_use())
        );

        let repeat = store.set_dont_use(&["INVX1".to_string()]).unwrap();
        assert_eq!(repeat, (1, 0));
    }

    #[test]
    fn set_dont_use_matches_library_qualified_patterns() {
        const LIB: &str = r#"
library(demo) {
  cell(BUFX1) { pin(A) { direction : input; } pin(Y) { direction : output; function : "A"; } }
}
"#;
        let mut store = LibraryStore::default();
        let library = parser::parse_liberty(LIB, "demo.lib").unwrap();
        store.append(vec![library]).unwrap();

        assert_eq!(
            store.set_dont_use(&["other/BUFX1".to_string()]).unwrap(),
            (0, 0)
        );
        assert_eq!(
            store.set_dont_use(&["demo/BUF?1".to_string()]).unwrap(),
            (1, 1)
        );
    }

    #[test]
    fn glob_match_covers_star_and_question_forms() {
        assert!(glob_match(
            "sky130_*_isobufsrc_?",
            "sky130_fd_sc_hd__lpflow_isobufsrc_1"
        ));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("INVX1", "INVX1"));
        assert!(!glob_match("INVX1", "INVX16"));
        assert!(!glob_match("INVX?", "INVX"));
        assert!(!glob_match("", "x"));
        assert!(glob_match("**", ""));
    }

    #[test]
    fn selection_order_drives_library_and_cell_order() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty("library(first) { cell(FIRST) { area : 1; } }", "first.lib")
                    .unwrap(),
                parser::parse_liberty(
                    "library(second) { cell(SECOND) { area : 2; } }",
                    "second.lib",
                )
                .unwrap(),
            ])
            .unwrap();

        let selection = LibrarySelection::parse("second first");
        let revision = store.current();
        let libraries = revision.selected_libraries(&selection).unwrap();
        let cells = revision.target_cells(&selection).unwrap();

        assert_eq!(
            libraries
                .iter()
                .map(|library| library.name.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(
            cells.iter().map(TargetCellRef::name).collect::<Vec<_>>(),
            ["SECOND", "FIRST"]
        );
    }

    #[test]
    fn selection_uses_first_duplicate_and_repeated_selector() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty("library(demo) {}", "/tmp/demo.lib").unwrap(),
            ])
            .unwrap();
        let revision = store.current();

        let selection = LibrarySelection::parse("demo demo demo.lib * *");
        assert_eq!(revision.selected_libraries(&selection).unwrap().len(), 1);
        let plan = revision.link_plan(&selection).unwrap();
        assert_eq!(plan.providers().len(), 2);
        assert!(matches!(
            plan.providers(),
            [
                LibraryLinkProvider::Library { library, .. },
                LibraryLinkProvider::DesignMemory
            ] if library.name == "demo"
        ));
    }

    #[test]
    fn selection_rejects_ambiguous_library_aliases() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty("library(shared) {}", "first.lib").unwrap(),
                parser::parse_liberty("library(shared) {}", "second.lib").unwrap(),
            ])
            .unwrap();

        assert!(matches!(
            store
                .current()
                .selected_libraries(&LibrarySelection::parse("shared")),
            Err(LibraryError::AmbiguousLibrarySelector { selector, .. }) if selector == "shared"
        ));
    }

    #[test]
    fn selection_uses_first_cell_definition() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty(
                    "library(first) { cell(DUPLICATE) { area : 1; } }",
                    "first.lib",
                )
                .unwrap(),
                parser::parse_liberty(
                    "library(second) { cell(DUPLICATE) { area : 2; } }",
                    "second.lib",
                )
                .unwrap(),
            ])
            .unwrap();

        let revision = store.current();
        let first_second = LibrarySelection::parse("first second");
        let second_first = LibrarySelection::parse("second first");
        let first_cells = revision.target_cells(&first_second).unwrap();
        let second_cells = revision.target_cells(&second_first).unwrap();
        let timing = revision.timing_library(&first_second).unwrap();

        assert_eq!(first_cells.len(), 1);
        assert_eq!(first_cells.get(0).unwrap().area(), Some(1.0));
        assert_eq!(second_cells.len(), 1);
        assert_eq!(second_cells.get(0).unwrap().area(), Some(2.0));
        assert_eq!(timing.cells.len(), 1);
        assert_eq!(timing.cells.get(0).unwrap().area(), Some(1.0));
        assert_eq!(timing.power.cells.iter().count(), 1);
    }

    #[test]
    fn ordered_selection_changes_target_cell_fingerprint() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty("library(first) { cell(FIRST) { area : 1; } }", "first.lib")
                    .unwrap(),
                parser::parse_liberty(
                    "library(second) { cell(SECOND) { area : 2; } }",
                    "second.lib",
                )
                .unwrap(),
            ])
            .unwrap();
        let revision = store.current();

        let first_second = revision
            .target_cells(&LibrarySelection::parse("first second"))
            .unwrap()
            .content_fingerprint();
        let second_first = revision
            .target_cells(&LibrarySelection::parse("second first"))
            .unwrap()
            .content_fingerprint();

        assert_ne!(first_second, second_first);
    }

    #[test]
    fn selection_fingerprint_uses_resolved_provider_semantics() {
        let mut store = LibraryStore::default();
        store
            .append(vec![
                parser::parse_liberty(
                    "library(demo) { cell(INVX1) { area : 1; } }",
                    "/tmp/demo.lib",
                )
                .unwrap(),
            ])
            .unwrap();
        let before = store
            .current()
            .selection_fingerprint(&LibrarySelection::parse("demo"))
            .unwrap();
        let alias_and_duplicate = store
            .current()
            .selection_fingerprint(&LibrarySelection::parse("demo.lib demo missing"))
            .unwrap();
        assert_eq!(before, alias_and_duplicate);

        let memory_first = store
            .current()
            .selection_fingerprint(&LibrarySelection::parse("* demo"))
            .unwrap();
        let memory_last = store
            .current()
            .selection_fingerprint(&LibrarySelection::parse("demo *"))
            .unwrap();
        assert_ne!(memory_first, memory_last);

        store
            .append(vec![
                parser::parse_liberty("library(unused) {}", "unused.lib").unwrap(),
            ])
            .unwrap();
        assert_eq!(
            before,
            store
                .current()
                .selection_fingerprint(&LibrarySelection::parse("demo"))
                .unwrap()
        );
    }
}
