// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::roots::MappingRoot;
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

type BoundaryBits = ([u8; 32], Box<[word::ValueId]>);
type BoundaryBit = ([u8; 32], u32);

const INPUT_BINDING_DOMAIN: &[u8] = b"opto/regional/input-binding/v1\0";
const ROOT_BINDING_DOMAIN: &[u8] = b"opto/regional/root-binding/v1\0";
const LOCAL_VALUE_BINDING_DOMAIN: &[u8] = b"opto/regional/local-value-binding/v1\0";

#[derive(Debug)]
pub(crate) struct RegionLogicSlice {
    inputs: Box<[word::ValueId]>,
    roots: Box<[MappingRoot]>,
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
        let mut topology_roots = Vec::new();
        for &(root, local) in roots {
            let bits = ownership
                .lowered_bits(local)
                .map_or_else(|| vec![local], <[word::ValueId]>::to_vec);
            for value in bits {
                topology_roots.push(MappingRoot {
                    value,
                    required_time: root.required_time,
                    output_load: root.output_load,
                    requires_combinational_cover: root.requires_combinational_cover
                        && !module.value(value).is_some_and(|stored| {
                            matches!(stored.kind, word::ValueKind::Constant(_))
                        }),
                });
            }
        }
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
        let output_bits = resolved
            .iter()
            .filter(|(contract, _)| {
                contract.port().direction() == crate::RegionPortDirection::Output
            })
            .flat_map(|(_, bits)| bits.iter().copied())
            .collect::<BTreeSet<_>>();
        // A root that is also an input normally contributes no logic. Keep it
        // when it names an output boundary, however: the portable cover must
        // record that pass-through so mapped-netlist construction connects the
        // observable output to its input instead of silently dropping it.
        topology_roots
            .retain(|root| !inputs.contains(&root.value) || output_bits.contains(&root.value));
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
        )?);
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
        let topology_roots = canonical_roots
            .into_iter()
            .map(|(_, root)| root)
            .collect::<Box<[_]>>();
        let projection = ContractProjection::build(
            &topology_roots,
            resolved
                .iter()
                .map(|(contract, bits)| (*contract, bits.as_ref())),
        );
        Ok(Self {
            inputs,
            roots: projection.roots,
            search_input_arrivals: projection.input_arrivals,
            search_input_transitions: projection.input_transitions,
            boundary_inputs: boundary_inputs.into_boxed_slice(),
            boundary_outputs: boundary_outputs.into_boxed_slice(),
        })
    }

    pub(crate) fn inputs(&self) -> &[word::ValueId] {
        &self.inputs
    }

    pub(crate) fn roots(&self) -> &[MappingRoot] {
        &self.roots
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
