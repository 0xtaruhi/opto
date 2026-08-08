// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::roots::MappingRoot;
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

type BoundaryBits = ([u8; 32], Box<[word::ValueId]>);
type BoundaryBit = ([u8; 32], u32);

const INPUT_BINDING_DOMAIN: &[u8] = b"opto/regional/input-binding/v1\0";
const ROOT_BINDING_DOMAIN: &[u8] = b"opto/regional/root-binding/v1\0";
const BINDING_LAYOUT_DOMAIN: &[u8] = b"opto/regional/binding-layout/v1\0";
const LOCAL_VALUE_BINDING_DOMAIN: &[u8] = b"opto/regional/local-value-binding/v1\0";

/// One way to name a region-interface input.
///
/// The same physical bit reaches the interface under several Word values: the
/// operation that drives it, the scalar signal it is connected to, and any
/// value that reads that signal. The Boolean subject reports whichever one its
/// traversal met first, while a frozen boundary contract names another. Every
/// alias is therefore registered against one slot instead of picking a winner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InputAlias {
    Value(word::ValueId),
    SignalBit { signal: u32, lsb: u32 },
}

/// Collects every alias one interface value answers to.
fn input_aliases_of(module: &word::WordModule, value: word::ValueId, out: &mut Vec<InputAlias>) {
    out.push(InputAlias::Value(value));
    if let Some(word::ValueKind::Signal(reference)) = module.value(value).map(|stored| &stored.kind)
        && reference.width() == 1
    {
        out.push(InputAlias::SignalBit {
            signal: reference.signal.raw(),
            lsb: reference.lsb,
        });
    }
}

/// Revision-local values indexed by semantic ordinals sealed into `digest`.
#[derive(Debug)]
struct StableBindingLayout {
    input_rows: Box<[(InputAlias, u32)]>,
    root_rows: Box<[(word::ValueId, u32)]>,
    digest: [u8; 32],
}

impl StableBindingLayout {
    fn build(
        module: &word::WordModule,
        driven_bits: &BTreeMap<word::ValueId, Vec<(u32, u32)>>,
        inputs: &[word::ValueId],
        input_keys: &[[u8; 32]],
        roots: &[MappingRoot],
        root_keys: &[[u8; 32]],
    ) -> Result<Self, crate::SynthError> {
        if inputs.len() != input_keys.len() || roots.len() != root_keys.len() {
            return Err(crate::SynthError::invariant(
                "regional stable binding columns have inconsistent lengths",
            ));
        }
        let mut digest = blake3::Hasher::new();
        digest.update(BINDING_LAYOUT_DOMAIN);
        digest.update(&(input_keys.len() as u64).to_le_bytes());
        for key in input_keys {
            digest.update(key);
        }
        digest.update(&(root_keys.len() as u64).to_le_bytes());
        for key in root_keys {
            digest.update(key);
        }
        Ok(Self {
            input_rows: input_alias_rows(module, inputs, driven_bits)?,
            root_rows: binding_rows(
                roots.iter().map(|root| root.value),
                "regional stable root binding",
            )?,
            digest: *digest.finalize().as_bytes(),
        })
    }

    fn input_ordinal(&self, alias: InputAlias) -> Option<u32> {
        self.input_rows
            .binary_search_by_key(&alias, |&(alias, _)| alias)
            .ok()
            .map(|row| self.input_rows[row].1)
    }

    fn root_ordinal(&self, value: word::ValueId) -> Option<u32> {
        self.root_rows
            .binary_search_by_key(&value, |&(value, _)| value)
            .ok()
            .map(|row| self.root_rows[row].1)
    }
}

/// Indexes every alias of every interface input against its ordinal.
///
/// `driven_bits` carries the scalar signal bits each input drives through a
/// connect, which is how an operation-valued interface slot answers to the
/// signal reads its consumers use.
fn input_alias_rows(
    module: &word::WordModule,
    inputs: &[word::ValueId],
    driven_bits: &BTreeMap<word::ValueId, Vec<(u32, u32)>>,
) -> Result<Box<[(InputAlias, u32)]>, crate::SynthError> {
    let mut rows = Vec::with_capacity(inputs.len());
    let mut aliases = Vec::new();
    for (ordinal, &value) in inputs.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| crate::SynthError::capacity("regional stable input binding"))?;
        aliases.clear();
        input_aliases_of(module, value, &mut aliases);
        for &(signal, lsb) in driven_bits.get(&value).map_or(&[][..], Vec::as_slice) {
            aliases.push(InputAlias::SignalBit { signal, lsb });
        }
        rows.extend(aliases.iter().map(|&alias| (alias, ordinal)));
    }
    rows.sort_unstable_by_key(|&(alias, _)| alias);
    // One physical bit may reach the interface through several slots only if the
    // frozen contract and the dataflow view disagree, which the union above
    // already reconciles; keep the first slot so lookup stays a total function.
    rows.dedup_by_key(|&mut (alias, _)| alias);
    Ok(rows.into_boxed_slice())
}

/// Maps each driving value to the scalar signal bits its connects target.
fn driven_signal_bits(module: &word::WordModule) -> BTreeMap<word::ValueId, Vec<(u32, u32)>> {
    let mut driven = BTreeMap::<word::ValueId, Vec<(u32, u32)>>::new();
    for connect in module.connects() {
        let lsb = match connect.target.range {
            Some(range) if range.msb == range.lsb => range.lsb,
            // A whole-signal connect names one bit only when the signal is
            // scalar; a wider target needs the per-bit lowering instead.
            None if module
                .signal(connect.target.signal)
                .is_some_and(|signal| signal.ty.width() == 1) =>
            {
                0
            }
            _ => continue,
        };
        driven
            .entry(connect.value)
            .or_default()
            .push((connect.target.signal.raw(), lsb));
    }
    driven
}

fn binding_rows<T: Copy + Ord>(
    values: impl IntoIterator<Item = T>,
    resource: &'static str,
) -> Result<Box<[(T, u32)]>, crate::SynthError> {
    let mut rows = values
        .into_iter()
        .enumerate()
        .map(|(ordinal, value)| {
            u32::try_from(ordinal)
                .map(|ordinal| (value, ordinal))
                .map_err(|_| crate::SynthError::capacity(resource))
        })
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_unstable_by_key(|&(value, _)| value);
    if rows.windows(2).any(|rows| rows[0].0 == rows[1].0) {
        return Err(crate::SynthError::invariant(format!(
            "{resource} contains duplicate values"
        )));
    }
    Ok(rows.into_boxed_slice())
}

#[derive(Debug)]
pub(crate) struct RegionLogicSlice {
    inputs: Box<[word::ValueId]>,
    topology_roots: Box<[MappingRoot]>,
    roots: Box<[MappingRoot]>,
    binding_layout: StableBindingLayout,
    search_input_arrivals: Box<[(word::ValueId, f64)]>,
    search_input_transitions: Box<[(word::ValueId, f64)]>,
    boundary_inputs: Box<[BoundaryBits]>,
    boundary_outputs: Box<[BoundaryBits]>,
}

struct ContractProjection {
    roots: Box<[MappingRoot]>,
    input_arrivals: Box<[(word::ValueId, f64)]>,
    input_transitions: Box<[(word::ValueId, f64)]>,
}

impl ContractProjection {
    fn build<'contract, 'bits>(
        topology_roots: &[MappingRoot],
        contracts: impl IntoIterator<
            Item = (&'contract crate::BoundaryContract, &'bits [word::ValueId]),
        >,
    ) -> Self {
        let mut roots = topology_roots.to_vec();
        let mut input_arrivals = BTreeMap::<word::ValueId, f64>::new();
        let mut input_transitions = BTreeMap::<word::ValueId, f64>::new();
        for (contract, bits) in contracts {
            match contract.port().direction() {
                crate::RegionPortDirection::Input => {
                    let arrival = contract
                        .rows()
                        .iter()
                        .filter_map(|row| row.input)
                        .flat_map(|input| [input.arrival.late.rise, input.arrival.late.fall])
                        .flatten()
                        .map(crate::FiniteValue::get)
                        .max_by(f64::total_cmp);
                    let transition = contract
                        .rows()
                        .iter()
                        .filter_map(|row| row.input)
                        .flat_map(|input| [input.transition.late.rise, input.transition.late.fall])
                        .flatten()
                        .map(crate::FiniteValue::get)
                        .max_by(f64::total_cmp);
                    for &bit in bits {
                        if let Some(arrival) = arrival {
                            input_arrivals
                                .entry(bit)
                                .and_modify(|current| *current = current.max(arrival))
                                .or_insert(arrival);
                        }
                        if let Some(transition) = transition {
                            input_transitions
                                .entry(bit)
                                .and_modify(|current| *current = current.max(transition))
                                .or_insert(transition);
                        }
                    }
                }
                crate::RegionPortDirection::Output => {
                    let required = contract
                        .rows()
                        .iter()
                        .filter_map(|row| row.output)
                        .flat_map(|output| [output.required.late.rise, output.required.late.fall])
                        .flatten()
                        .map(crate::FiniteValue::get)
                        .min_by(f64::total_cmp);
                    let load = contract
                        .rows()
                        .iter()
                        .filter_map(|row| row.output)
                        .flat_map(|output| [output.capacitance.early, output.capacitance.late])
                        .flatten()
                        .map(crate::FiniteValue::get)
                        .max_by(f64::total_cmp);
                    for root in &mut roots {
                        if !bits.contains(&root.value) {
                            continue;
                        }
                        if let Some(required) = required {
                            root.required_time = Some(
                                root.required_time
                                    .map_or(required, |current| current.min(required)),
                            );
                        }
                        if let Some(load) = load {
                            root.output_load =
                                Some(root.output_load.map_or(load, |current| current.max(load)));
                        }
                    }
                }
            }
        }
        Self {
            roots: roots.into_boxed_slice(),
            input_arrivals: input_arrivals.into_iter().collect(),
            input_transitions: input_transitions.into_iter().collect(),
        }
    }
}

impl RegionLogicSlice {
    pub(crate) fn build_candidate(
        module: &word::WordModule,
        region: crate::RegionAnchorId,
        decision_key: [u8; 32],
        source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
        contracts: &[crate::BoundaryContract],
        roots: &[(MappingRoot, word::ValueId)],
    ) -> Result<Self, crate::SynthError> {
        let mut inputs = BTreeSet::new();
        let topology_roots = roots
            .iter()
            .flat_map(|&(root, local)| {
                ownership
                    .lowered_bits(local)
                    .map_or_else(|| vec![local], <[word::ValueId]>::to_vec)
                    .into_iter()
                    .map(move |value| MappingRoot {
                        value,
                        required_time: root.required_time,
                        output_load: root.output_load,
                    })
            })
            .collect::<Vec<_>>();
        let mut topology_roots = super::roots::merge_by_value(topology_roots);
        let resolved = contracts
            .iter()
            .map(|contract| {
                let bits = match source_to_local.get(&contract.port().value()).copied() {
                    Some(local) => ownership
                        .lowered_bits(local)
                        .map_or_else(|| vec![local], <[word::ValueId]>::to_vec),
                    // A source boundary outside the task-local cone is explicitly
                    // represented by an empty materialization row. Its semantic
                    // key remains present and cannot be confused with corruption.
                    None => Vec::new(),
                };
                (contract, bits.into_boxed_slice())
            })
            .collect::<Vec<_>>();
        for (_, bits) in resolved.iter().filter(|(contract, _)| {
            contract.port().direction() == crate::RegionPortDirection::Input
        }) {
            inputs.extend(bits.iter().copied());
        }
        topology_roots.retain(|root| !inputs.contains(&root.value));
        Self::from_resolved(module, &inputs, topology_roots, &resolved, |value| {
            let stored = module.value(value).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional binding identity references an unknown local value",
                )
            })?;
            let mut digest = blake3::Hasher::new();
            digest.update(LOCAL_VALUE_BINDING_DOMAIN);
            digest.update(&region.bytes());
            digest.update(&decision_key);
            digest.update(&value.raw().to_le_bytes());
            digest.update(&stored.ty.width().to_le_bytes());
            digest.update(&[u8::from(stored.ty.is_signed()), stored.ty.state() as u8]);
            Ok(*digest.finalize().as_bytes())
        })
    }

    fn from_resolved(
        module: &word::WordModule,
        inputs: &BTreeSet<word::ValueId>,
        topology_roots: Vec<MappingRoot>,
        resolved: &[(&crate::BoundaryContract, Box<[word::ValueId]>)],
        value_key: impl Fn(word::ValueId) -> Result<[u8; 32], crate::SynthError>,
    ) -> Result<Self, crate::SynthError> {
        let mut boundary_inputs = Vec::new();
        let mut boundary_outputs = Vec::new();
        let mut boundary_keys = BTreeMap::new();
        for (contract, bits) in resolved {
            let port = contract.port();
            let direction = port.direction();
            let key = port.semantic_key();
            if let Some(first) = boundary_keys.insert((direction, key), port.value()) {
                return Err(crate::SynthError::invariant(format!(
                    "regional contracts alias source values {first:?} and {:?} to one semantic boundary",
                    port.value()
                )));
            }
            let row = (key, bits.clone());
            match direction {
                crate::RegionPortDirection::Input => boundary_inputs.push(row),
                crate::RegionPortDirection::Output => boundary_outputs.push(row),
            }
        }
        boundary_inputs.sort_by_key(|(key, _)| *key);
        boundary_outputs.sort_by_key(|(key, _)| *key);
        let input_aliases = boundary_aliases(&boundary_inputs)?;
        // The canonical layout below indexes inputs by their boundary binding,
        // so the dataflow inputs and the frozen input contract bits must name
        // exactly the same values.
        // The interface is what the Boolean subject will actually see. Cross-
        // region dataflow and the frozen contract each describe part of it, but
        // neither can name a region-local wire read, so the leaf rule the
        // subject applies is the authority. Enumerating it here means a cover
        // input can never be absent from its own slice.
        let boundary_values = input_aliases.keys().copied().collect::<BTreeSet<_>>();
        let mut inputs = inputs
            .union(&boundary_values)
            .copied()
            .collect::<BTreeSet<_>>();
        let declared = inputs.iter().copied().collect::<Vec<_>>();
        let root_values = topology_roots
            .iter()
            .map(|root| root.value)
            .collect::<Vec<_>>();
        inputs.extend(crate::boolean::logic::subject_leaves(
            module,
            &root_values,
            &declared,
        ));
        let mut input_occurrences = ContentOccurrences::default();
        let mut canonical_inputs = inputs
            .into_iter()
            .map(|value| {
                // A slot with a boundary row takes its stable identity from
                // that row; a region-local leaf such as a register or memory
                // output falls back to content, exactly as roots already do.
                let aliases = input_aliases.get(&value).map_or(&[][..], Vec::as_slice);
                let content = if aliases.is_empty() {
                    let key = value_key(value)?;
                    Some(input_occurrences.claim(key))
                } else {
                    None
                };
                Ok((binding_key(INPUT_BINDING_DOMAIN, content, aliases), value))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        canonical_inputs.sort_unstable_by_key(|&(key, _)| key);
        reject_duplicate_binding_keys(&canonical_inputs, "input")?;
        let input_keys = canonical_inputs
            .iter()
            .map(|&(key, _)| key)
            .collect::<Box<[_]>>();
        let inputs = canonical_inputs
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Box<[_]>>();

        let output_aliases = boundary_aliases(&boundary_outputs)?;
        let mut root_occurrences = ContentOccurrences::default();
        let mut canonical_roots = topology_roots
            .into_iter()
            .map(|root| {
                let aliases = output_aliases
                    .get(&root.value)
                    .map_or(&[][..], Vec::as_slice);
                // A hard output boundary is already the root's stable semantic
                // identity. Content is the fail-closed fallback only for
                // region-local infrastructure roots with no boundary row.
                let content = if aliases.is_empty() {
                    let key = value_key(root.value)?;
                    Some(root_occurrences.claim(key))
                } else {
                    None
                };
                Ok((binding_key(ROOT_BINDING_DOMAIN, content, aliases), root))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        canonical_roots.sort_unstable_by_key(|&(key, _)| key);
        reject_duplicate_binding_keys(&canonical_roots, "root")?;
        let root_keys = canonical_roots
            .iter()
            .map(|&(key, _)| key)
            .collect::<Box<[_]>>();
        let topology_roots = canonical_roots
            .into_iter()
            .map(|(_, root)| root)
            .collect::<Box<[_]>>();
        let driven_bits = driven_signal_bits(module);
        let binding_layout = StableBindingLayout::build(
            module,
            &driven_bits,
            &inputs,
            &input_keys,
            &topology_roots,
            &root_keys,
        )?;
        let projection = ContractProjection::build(
            &topology_roots,
            resolved
                .iter()
                .map(|(contract, bits)| (*contract, bits.as_ref())),
        );
        Ok(Self {
            inputs,
            topology_roots,
            roots: projection.roots,
            binding_layout,
            search_input_arrivals: projection.input_arrivals,
            search_input_transitions: projection.input_transitions,
            boundary_inputs: boundary_inputs.into_boxed_slice(),
            boundary_outputs: boundary_outputs.into_boxed_slice(),
        })
    }

    fn project_contracts(
        &self,
        contracts: &[crate::BoundaryContract],
    ) -> Result<ContractProjection, crate::SynthError> {
        if contracts.len() != self.boundary_inputs.len() + self.boundary_outputs.len() {
            return Err(crate::SynthError::invariant(
                "regional contract topology changed after partition freeze",
            ));
        }
        let mut seen = BTreeSet::new();
        let resolved = contracts
            .iter()
            .map(|contract| {
                let key = contract.port().semantic_key();
                let direction = contract.port().direction() as u8;
                if !seen.insert((direction, key)) {
                    return Err(crate::SynthError::invariant(
                        "regional contract projection contains a duplicate boundary",
                    ));
                }
                let bits = match contract.port().direction() {
                    crate::RegionPortDirection::Input => self.boundary_input_bits(key)?,
                    crate::RegionPortDirection::Output => self.boundary_output_bits(key)?,
                };
                Ok((contract, bits))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        Ok(ContractProjection::build(&self.topology_roots, resolved))
    }

    fn apply_projection(&mut self, projection: ContractProjection) {
        self.roots = projection.roots;
        self.search_input_arrivals = projection.input_arrivals;
        self.search_input_transitions = projection.input_transitions;
    }

    pub(crate) fn inputs(&self) -> &[word::ValueId] {
        &self.inputs
    }

    pub(crate) fn roots(&self) -> &[MappingRoot] {
        &self.roots
    }

    pub(crate) const fn binding_layout_digest(&self) -> [u8; 32] {
        self.binding_layout.digest
    }

    /// Resolves one subject leaf to its canonical interface ordinal.
    ///
    /// The leaf is identified by the scalar signal bit it reads, so a value the
    /// subject happened to report and the value a frozen contract names resolve
    /// to the same interface slot.
    pub(crate) fn input_binding_ordinal(
        &self,
        module: &word::WordModule,
        value: word::ValueId,
    ) -> Option<u32> {
        let mut aliases = Vec::new();
        input_aliases_of(module, value, &mut aliases);
        aliases
            .into_iter()
            .find_map(|alias| self.binding_layout.input_ordinal(alias))
    }

    pub(crate) fn root_binding_ordinal(&self, value: word::ValueId) -> Option<u32> {
        self.binding_layout.root_ordinal(value)
    }

    pub(crate) fn binding_input(&self, ordinal: u32) -> Option<word::ValueId> {
        self.inputs.get(ordinal as usize).copied()
    }

    pub(crate) fn binding_root(&self, ordinal: u32) -> Option<word::ValueId> {
        self.roots.get(ordinal as usize).map(|root| root.value)
    }

    /// Conservative late-mode projection used only to guide cover search.
    /// Exact sparse rows remain authoritative for response scoring.
    pub(crate) fn search_input_arrival(&self, value: word::ValueId) -> Option<f64> {
        self.search_input_arrivals
            .binary_search_by_key(&value, |&(value, _)| value)
            .ok()
            .map(|index| self.search_input_arrivals[index].1)
    }

    pub(crate) fn search_input_transition(&self, value: word::ValueId) -> Option<f64> {
        self.search_input_transitions
            .binary_search_by_key(&value, |&(value, _)| value)
            .ok()
            .map(|index| self.search_input_transitions[index].1)
    }

    pub(crate) fn boundary_output_bits(
        &self,
        semantic_key: [u8; 32],
    ) -> Result<&[word::ValueId], crate::SynthError> {
        self.boundary_outputs
            .binary_search_by_key(&semantic_key, |(key, _)| *key)
            .ok()
            .map(|index| self.boundary_outputs[index].1.as_ref())
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional logic slice has no row for an output boundary contract",
                )
            })
    }

    pub(crate) fn boundary_input_bits(
        &self,
        semantic_key: [u8; 32],
    ) -> Result<&[word::ValueId], crate::SynthError> {
        self.boundary_inputs
            .binary_search_by_key(&semantic_key, |(key, _)| *key)
            .ok()
            .map(|index| self.boundary_inputs[index].1.as_ref())
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional logic slice has no row for an input boundary contract",
                )
            })
    }
}

#[derive(Debug)]
pub(crate) struct RegionalLogicPartition {
    slices: Box<[RegionLogicSlice]>,
}

impl RegionalLogicPartition {
    pub(crate) fn build(
        module: &word::WordModule,
        regions: &crate::SynthesisRegionGraph,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
        contracts: &crate::regional::RegionContractSet,
        roots: &[MappingRoot],
    ) -> Result<Self, crate::SynthError> {
        let value_keys = crate::regional::region_graph::partition::semantic_value_keys(module)?;
        let mut inputs = vec![BTreeSet::new(); regions.regions().len()];
        let mut outputs = vec![BTreeSet::new(); regions.regions().len()];
        let observed_values = roots.iter().map(|root| root.value).collect::<Vec<_>>();
        let live_operations = super::word_util::live_operation_mask(module, &observed_values)?;
        for (operation_index, operation) in module.operations().iter().enumerate() {
            if !live_operations[operation_index] {
                continue;
            }
            let Some(sink) = ownership.owner(operation.result) else {
                continue;
            };
            for input in crate::word::operation_inputs(&operation.kind) {
                if module
                    .value(input)
                    .is_some_and(|value| matches!(value.kind, word::ValueKind::Constant(_)))
                {
                    continue;
                }
                match ownership.owner(input) {
                    Some(source) if source == sink => {}
                    Some(source) => {
                        inputs[sink.index()].insert(input);
                        outputs[source.index()].insert(input);
                    }
                    None => {
                        if module.value(input).is_some_and(|value| {
                            !matches!(value.kind, word::ValueKind::Constant(_))
                        }) {
                            inputs[sink.index()].insert(input);
                        }
                    }
                }
            }
        }
        let mut roots_by_region = vec![Vec::new(); regions.regions().len()];
        for &root in roots {
            let Some(owner) = ownership.owner(root.value) else {
                continue;
            };
            outputs[owner.index()].insert(root.value);
            roots_by_region[owner.index()].push(root);
        }
        for row in 0..regions.regions().len() {
            roots_by_region[row].extend(outputs[row].iter().map(|&value| MappingRoot {
                value,
                required_time: None,
                output_load: None,
            }));
            let mut row_roots =
                super::roots::merge_by_value(std::mem::take(&mut roots_by_region[row]));
            row_roots.sort_by_key(|root| root.value);
            roots_by_region[row] = row_roots;
        }
        let slices = inputs
            .into_iter()
            .zip(roots_by_region)
            .zip(regions.regions())
            .map(|((inputs, roots), region)| {
                let resolved = contracts
                    .contracts(region.row())
                    .iter()
                    .map(|contract| {
                        let bits = match ownership.lowered_bits(contract.port().value()) {
                            Some(bits) => bits,
                            // A frozen source boundary can be proven dead by
                            // global lowering; its typed row remains explicitly empty.
                            None => &[],
                        }
                        .to_vec()
                        .into_boxed_slice();
                        (contract, bits)
                    })
                    .collect::<Vec<_>>();
                RegionLogicSlice::from_resolved(module, &inputs, roots, &resolved, |value| {
                    value_keys.get(value.index()).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional binding value is outside the semantic value index",
                        )
                    })
                })
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?
            .into_boxed_slice();
        Ok(Self { slices })
    }

    /// Reprojects only rows whose immutable boundary contracts changed.
    /// Word topology, ownership, and every clean slice remain untouched.
    pub(crate) fn update_contracts(
        &mut self,
        contracts: &crate::regional::RegionContractSet,
        dirty: &[crate::RegionRowId],
    ) -> Result<(), crate::SynthError> {
        let dirty = dirty.iter().copied().collect::<BTreeSet<_>>();
        let projections = dirty
            .iter()
            .copied()
            .map(|row| {
                let slice = self.slices.get(row.index()).ok_or_else(|| {
                    crate::SynthError::invariant("dirty regional partition row is out of range")
                })?;
                Ok((row, slice.project_contracts(contracts.contracts(row))?))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        for (row, projection) in projections {
            self.slices[row.index()].apply_projection(projection);
        }
        Ok(())
    }

    pub(crate) fn slice(&self, row: crate::RegionRowId) -> &RegionLogicSlice {
        &self.slices[row.index()]
    }
}

fn boundary_aliases(
    boundaries: &[BoundaryBits],
) -> Result<BTreeMap<word::ValueId, Vec<BoundaryBit>>, crate::SynthError> {
    let mut positions = BTreeSet::new();
    let mut aliases = BTreeMap::<word::ValueId, Vec<BoundaryBit>>::new();
    for &(semantic_key, ref bits) in boundaries {
        for (ordinal, &value) in bits.iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| crate::SynthError::capacity("regional boundary bit ordinal"))?;
            if !positions.insert((semantic_key, ordinal)) {
                return Err(crate::SynthError::invariant(
                    "regional stable binding contains a duplicate semantic bit position",
                ));
            }
            aliases
                .entry(value)
                .or_default()
                .push((semantic_key, ordinal));
        }
    }
    for aliases in aliases.values_mut() {
        aliases.sort_unstable();
    }
    Ok(aliases)
}

/// Tracks how many slots have already claimed each content key.
///
/// A content key is a semantic digest, so structurally identical logic — the
/// equivalent next-state cones of an FSM, say — deliberately shares one. It
/// names logic, not a slot, and so needs the same occurrence disambiguation a
/// boundary row already carries in its alias ordinal.
#[derive(Default)]
struct ContentOccurrences(BTreeMap<[u8; 32], u32>);

impl ContentOccurrences {
    fn claim(&mut self, content: [u8; 32]) -> ([u8; 32], u32) {
        let next = self.0.entry(content).or_default();
        let ordinal = *next;
        *next += 1;
        (content, ordinal)
    }
}

fn binding_key(
    domain: &[u8],
    content: Option<([u8; 32], u32)>,
    aliases: &[BoundaryBit],
) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    if let Some((content, ordinal)) = content {
        digest.update(&content);
        digest.update(&ordinal.to_le_bytes());
    }
    digest.update(&(aliases.len() as u64).to_le_bytes());
    for &(semantic_key, ordinal) in aliases {
        digest.update(&semantic_key);
        digest.update(&ordinal.to_le_bytes());
    }
    *digest.finalize().as_bytes()
}

fn reject_duplicate_binding_keys<T>(
    bindings: &[([u8; 32], T)],
    kind: &'static str,
) -> Result<(), crate::SynthError> {
    if bindings.windows(2).any(|rows| rows[0].0 == rows[1].0) {
        return Err(crate::SynthError::invariant(format!(
            "regional stable {kind} bindings are semantically ambiguous"
        )));
    }
    Ok(())
}
