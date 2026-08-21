// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::OperatorId;
use crate::artifact::MappedCellSource;
use crate::artifact::implementation::{
    FragmentFootprint, ImplementationDb, ImplementationRegion, ImplementationRegionMetadata,
    ImplementationRegionSource, OriginSetId, implementation_origin_hash,
};
use crate::planning::operator::ArchitectureDecisions;
use opto_ir::mapped::CellId;
use opto_ir::word;
use smallvec::{SmallVec, smallvec};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
struct SourceInstanceIdentity {
    name: opto_ir::NameId,
    reference: opto_ir::NameId,
}

/// Identifies instances that were present at the synthesis input boundary.
///
/// `WordModule` instance IDs are dense and append-only. Keeping interned name
/// IDs as well as the dense slot lets materialization distinguish preserved
/// source instances from cells introduced by mapping without duplicating
/// instance or cell-name strings. The identity check deliberately fails if a
/// future transform starts reordering or replacing instance slots without
/// updating this provenance.
#[derive(Debug, Clone)]
pub(crate) struct SourceInstanceProvenance {
    identities: Box<[SourceInstanceIdentity]>,
}

impl SourceInstanceProvenance {
    pub(crate) fn capture(module: &word::WordModule) -> Self {
        Self {
            identities: module
                .instances()
                .iter()
                .map(|instance| SourceInstanceIdentity {
                    name: instance.name,
                    reference: instance.module,
                })
                .collect(),
        }
    }

    pub(crate) fn is_source_instance(
        &self,
        module: &word::WordModule,
        instance: word::InstId,
    ) -> Result<bool, crate::SynthError> {
        let Some(expected) = self.identities.get(instance.index()) else {
            return Ok(false);
        };
        let actual = module.instances().get(instance.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "source-instance provenance references missing instance {instance:?}"
            ))
        })?;
        if actual.name != expected.name || actual.module != expected.reference {
            return Err(crate::SynthError::invariant(format!(
                "source-instance identity changed for instance {instance:?}"
            )));
        }
        Ok(true)
    }
}

#[derive(Clone)]
pub(crate) struct ProvenanceBuilder {
    value_origins: Vec<OriginSetId>,
    origin_sets: Vec<Vec<OperatorId>>,
    origin_ids: HashMap<u64, SmallVec<[OriginSetId; 1]>>,
    // Epoch marks avoid clearing a module-sized visited table for every mapped cover.
    visit_epochs: Vec<u32>,
    next_visit_epoch: u32,
    operators: Vec<PublishedOperator>,
}

#[derive(Clone)]
struct PublishedOperator {
    id: OperatorId,
    candidate: crate::ImplementationCandidateId,
    owner: Option<crate::RegionAnchorId>,
    source_operations: Box<[word::OpId]>,
    width: u32,
    lines: Box<[Option<u32>]>,
    file: Option<Box<str>>,
    line: Option<u32>,
    recipe: Box<str>,
    implementation: Box<str>,
    module: Box<str>,
    mnemonic: Box<str>,
}

pub(crate) struct PrivateArchitecturePublication {
    operators: Box<[PublishedOperator]>,
}

pub(crate) fn resolve_private_operator_sources(
    source: &word::WordModule,
    local: &word::WordModule,
    decisions: &ArchitectureDecisions,
    owned_operations: &[word::OpId],
    operation_sources: &crate::planning::regional::LocalOperationProvenance,
) -> Result<Box<[Box<[word::OpId]>]>, crate::SynthError> {
    let owned_operations = owned_operations
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    for &operation_id in &owned_operations {
        source.operation(operation_id).ok_or_else(|| {
            crate::SynthError::invariant(
                "private architecture owner references an unknown source operation",
            )
        })?;
    }
    decisions
        .operators()
        .iter()
        .map(|semantic| {
            let mut sources = std::collections::BTreeSet::new();
            let mut pending = decisions.source_operations(semantic.id()).to_vec();
            let mut visited = std::collections::BTreeSet::new();
            while let Some(operation_id) = pending.pop() {
                if !visited.insert(operation_id) {
                    continue;
                }
                if let Some(source_operations) = operation_sources.sources(operation_id)
                    && !source_operations.is_empty()
                {
                    sources.extend(source_operations.iter().copied());
                    continue;
                }
                let operation = local.operation(operation_id).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "private architecture references an unknown local operation",
                    )
                })?;
                pending.extend(
                    crate::word::operation_inputs(&operation.kind)
                        .iter()
                        .filter_map(|&value| {
                            let value = local.value(value)?;
                            let word::ValueKind::Operation(operation) = value.kind else {
                                return None;
                            };
                            Some(operation)
                        }),
                );
            }
            if sources.is_empty() {
                return Err(crate::SynthError::invariant(
                    "private semantic operator has no source-operation provenance",
                ));
            }
            if !sources.is_subset(&owned_operations) {
                return Err(crate::SynthError::invariant(
                    "private semantic operator provenance crosses its frozen owner",
                ));
            }
            Ok(sources.into_iter().collect())
        })
        .collect()
}

impl PrivateArchitecturePublication {
    pub(crate) fn capture_resolved(
        source: &word::WordModule,
        decisions: &ArchitectureDecisions,
        owner: crate::RegionAnchorId,
        resolved_sources: &[Box<[word::OpId]>],
    ) -> Result<Self, crate::SynthError> {
        if resolved_sources.len() != decisions.operators().len() {
            return Err(crate::SynthError::invariant(
                "private architecture source records do not align with semantic operators",
            ));
        }
        let mut published = Vec::with_capacity(decisions.operators().len());
        for (semantic, source_operations) in decisions.operators().iter().zip(resolved_sources) {
            let candidate = decisions.selected_candidate(semantic.id()).ok_or_else(|| {
                crate::SynthError::invariant("private semantic operator has no selected candidate")
            })?;
            let spans = source_operations
                .iter()
                .filter_map(|&operation| source.operation(operation))
                .map(|operation| &operation.source)
                .collect::<Vec<_>>();
            let first = spans.first().copied();
            published.push(PublishedOperator {
                id: semantic.id(),
                candidate: candidate.id(),
                owner: Some(owner),
                source_operations: source_operations.clone(),
                width: semantic.width(),
                lines: spans.iter().map(|span| span.line()).collect(),
                file: first.and_then(word::SourceSpan::file).map(Into::into),
                line: first.and_then(word::SourceSpan::line),
                recipe: decisions
                    .candidate_recipe_name(candidate.id())
                    .ok_or_else(|| crate::SynthError::invariant("candidate has no recipe name"))?
                    .into(),
                implementation: decisions
                    .candidate_implementation_name(candidate.id())
                    .ok_or_else(|| {
                        crate::SynthError::invariant("candidate has no implementation name")
                    })?
                    .into(),
                module: decisions
                    .candidate_module_name(candidate.id())
                    .ok_or_else(|| crate::SynthError::invariant("candidate has no module name"))?
                    .into(),
                mnemonic: decisions
                    .candidate_operation_mnemonic(candidate.id())
                    .ok_or_else(|| {
                        crate::SynthError::invariant("candidate has no operation mnemonic")
                    })?
                    .into(),
            });
        }
        Ok(Self {
            operators: published.into_boxed_slice(),
        })
    }
}

impl ProvenanceBuilder {
    pub(crate) fn for_regional_candidate(module: &word::WordModule) -> Self {
        let empty = OriginSetId::EMPTY;
        let mut origin_ids = HashMap::new();
        origin_ids.insert(implementation_origin_hash(&[]), smallvec![empty]);
        Self {
            value_origins: vec![empty; module.values().len()],
            origin_sets: vec![Vec::new()],
            origin_ids,
            visit_epochs: vec![0; module.values().len()],
            next_visit_epoch: 1,
            operators: Vec::new(),
        }
    }

    pub(crate) fn new(
        module: &word::WordModule,
        plan: &ArchitectureDecisions,
    ) -> Result<Self, crate::SynthError> {
        let empty = OriginSetId::EMPTY;
        let mut origin_ids = HashMap::new();
        origin_ids.insert(implementation_origin_hash(&[]), smallvec![empty]);
        let mut operators = Vec::with_capacity(plan.operators().len());
        for operator in plan.operators() {
            if operator.id().raw() as usize != operators.len() {
                return Err(crate::SynthError::invariant(
                    "architecture operator IDs are not dense and ordered",
                ));
            }
            let candidate = plan.selected_candidate(operator.id()).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "operator {} has no selected implementation candidate",
                    operator.id().raw()
                ))
            })?;
            let source_operations = plan.source_operations(operator.id());
            let spans = source_operations
                .iter()
                .map(|operation| {
                    module
                        .operation(*operation)
                        .map(|operation| &operation.source)
                })
                .collect::<Vec<_>>();
            let first = spans.first().copied().flatten();
            operators.push(PublishedOperator {
                id: operator.id(),
                candidate: candidate.id(),
                owner: None,
                source_operations: source_operations.into(),
                width: operator.width(),
                lines: spans
                    .iter()
                    .map(|span| span.and_then(opto_ir::word::SourceSpan::line))
                    .collect(),
                file: first.and_then(|span| span.file()).map(Into::into),
                line: first.and_then(opto_ir::word::SourceSpan::line),
                recipe: plan
                    .candidate_recipe_name(candidate.id())
                    .ok_or_else(|| crate::SynthError::invariant("candidate has no recipe name"))?
                    .into(),
                implementation: plan
                    .candidate_implementation_name(candidate.id())
                    .ok_or_else(|| {
                        crate::SynthError::invariant("candidate has no implementation name")
                    })?
                    .into(),
                module: plan
                    .candidate_module_name(candidate.id())
                    .ok_or_else(|| crate::SynthError::invariant("candidate has no module name"))?
                    .into(),
                mnemonic: plan
                    .candidate_operation_mnemonic(candidate.id())
                    .ok_or_else(|| {
                        crate::SynthError::invariant("candidate has no operation mnemonic")
                    })?
                    .into(),
            });
        }
        let mut builder = Self {
            value_origins: vec![empty; module.values().len()],
            origin_sets: vec![Vec::new()],
            origin_ids,
            visit_epochs: vec![0; module.values().len()],
            next_visit_epoch: 1,
            operators,
        };
        for operator in plan.operators() {
            builder.set_value_operator(operator.result(), operator.id())?;
        }
        Ok(builder)
    }

    pub(crate) fn import_private_architecture(
        &mut self,
        publication: PrivateArchitecturePublication,
        module: &word::WordModule,
    ) -> Result<(), crate::SynthError> {
        for mut operator in publication.operators.into_vec() {
            let raw = u32::try_from(self.operators.len()).map_err(|_| {
                crate::SynthError::capacity("published operator ID exceeds 32-bit capacity")
            })?;
            operator.id = OperatorId::from_raw(raw);
            operator.candidate = crate::ImplementationCandidateId::from_raw(raw);
            for &source in &operator.source_operations {
                let result = module
                    .operation(source)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "private architecture references an unknown source operation",
                        )
                    })?
                    .result;
                self.add_value_operator(result, operator.id)?;
            }
            self.operators.push(operator);
        }
        Ok(())
    }

    pub(crate) fn set_value_operator(
        &mut self,
        value: word::ValueId,
        operator: OperatorId,
    ) -> Result<(), crate::SynthError> {
        let singleton = self.intern(vec![operator])?;
        self.set_value_origin(value, singleton);
        Ok(())
    }

    fn add_value_operator(
        &mut self,
        value: word::ValueId,
        operator: OperatorId,
    ) -> Result<(), crate::SynthError> {
        let mut operators = self.operators_for_value(value).to_vec();
        operators.push(operator);
        let origin = self.intern(operators)?;
        self.set_value_origin(value, origin);
        Ok(())
    }

    pub(crate) fn copy_value_origin(
        &mut self,
        source: word::ValueId,
        destination: word::ValueId,
    ) -> Result<(), crate::SynthError> {
        let origin = self
            .value_origins
            .get(source.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant("provenance source is outside the value-origin arena")
            })?;
        self.set_value_origin(destination, origin);
        Ok(())
    }

    pub(crate) fn origins_for_operation_cover(
        &mut self,
        module: &word::WordModule,
        roots: &[word::ValueId],
        leaves: &[word::ValueId],
    ) -> Result<OriginSetId, crate::SynthError> {
        let mut pending = roots.to_vec();
        if self.visit_epochs.len() < module.values().len() {
            self.visit_epochs.resize(module.values().len(), 0);
        }
        let visit_epoch = self.take_visit_epoch();
        for &leaf in leaves {
            let Some(visited_epoch) = self.visit_epochs.get_mut(leaf.index()) else {
                return Err(crate::SynthError::invariant(format!(
                    "provenance leaf references unknown RTL value {leaf:?}"
                )));
            };
            *visited_epoch = visit_epoch;
        }
        let mut operators = Vec::new();
        while let Some(value_id) = pending.pop() {
            let index = value_id.index();
            let Some(visited_epoch) = self.visit_epochs.get_mut(index) else {
                return Err(crate::SynthError::invariant(format!(
                    "provenance references unknown RTL value {value_id:?}"
                )));
            };
            if *visited_epoch == visit_epoch {
                continue;
            }
            *visited_epoch = visit_epoch;
            operators.extend_from_slice(self.operators_for_value(value_id));
            let value = module.value(value_id).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "provenance references unknown RTL value {value_id:?}"
                ))
            })?;
            let word::ValueKind::Operation(operation_id) = value.kind else {
                continue;
            };
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "provenance references unknown RTL operation {operation_id:?}"
                ))
            })?;
            pending.extend(crate::word::operation_inputs(&operation.kind));
        }
        self.intern(operators)
    }

    pub(crate) fn finish(
        mut self,
        synthesis_regions: &crate::SynthesisRegionGraph,
        mapped_module: &word::WordModule,
        mapped: &opto_ir::mapped::MappedNetlist,
        cell_sources: &[(CellId, MappedCellSource)],
    ) -> Result<ImplementationDb, crate::SynthError> {
        if self.value_origins.len() > mapped_module.values().len() {
            return Err(crate::SynthError::invariant(
                "provenance contains values absent from the final working module",
            ));
        }
        self.value_origins
            .resize(mapped_module.values().len(), OriginSetId::EMPTY);
        let cell_slots = mapped.cell_slot_count();
        let mut cell_origins = vec![OriginSetId::EMPTY; cell_slots];
        let mut cell_fragments = std::iter::repeat_with(|| None)
            .take(cell_slots)
            .collect::<Vec<_>>();
        let mut seen = vec![false; cell_slots];
        for &(cell, cell_source) in cell_sources {
            let Some(seen) = seen.get_mut(cell.index()) else {
                return Err(crate::SynthError::invariant(format!(
                    "mapped cell provenance references out-of-range slot {cell:?}"
                )));
            };
            if !mapped.is_live_cell(cell) {
                return Err(crate::SynthError::invariant(format!(
                    "mapped cell provenance references removed slot {cell:?}"
                )));
            }
            if std::mem::replace(seen, true) {
                return Err(crate::SynthError::invariant(format!(
                    "mapped cell {cell:?} has multiple provenance sources"
                )));
            }
            let (origin, fragment) = match cell_source {
                MappedCellSource::Instance(_) => (OriginSetId::EMPTY, FragmentFootprint::Global),
                MappedCellSource::StructuralValue(value) => (
                    self.value_origins
                        .get(value.index())
                        .copied()
                        .ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "mapped cell {cell:?} references structural value {value:?} without provenance"
                            ))
                        })?,
                    FragmentFootprint::Global,
                ),
                MappedCellSource::Value { value, region } => (
                    self.value_origins
                        .get(value.index())
                        .copied()
                        .ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "mapped cell {cell:?} references value {value:?} without provenance"
                            ))
                        })?,
                    FragmentFootprint::Region(region),
                ),
                MappedCellSource::Region { origins, region } => {
                    (origins, FragmentFootprint::Region(region))
                }
            };
            if origin.0 as usize >= self.origin_sets.len() {
                return Err(crate::SynthError::invariant(format!(
                    "mapped cell {cell:?} references unknown provenance origin {}",
                    origin.0
                )));
            }
            cell_origins[cell.index()] = origin;
            cell_fragments[cell.index()] = Some(fragment);
        }
        if let Some(cell) = mapped.cell_ids().find(|cell| !seen[cell.index()]) {
            return Err(crate::SynthError::invariant(format!(
                "live mapped cell {cell:?} has no provenance source"
            )));
        }

        let mut cells_by_operator = vec![Vec::new(); self.operators.len()];
        for (index, &origin) in cell_origins.iter().enumerate() {
            let cell = CellId::from_index(index).map_err(|_| {
                crate::SynthError::capacity("mapped cell ID exceeds 32-bit capacity")
            })?;
            for &operator in self.operators_for_origin(origin) {
                cells_by_operator[operator.raw() as usize].push(cell);
            }
        }

        let mut regions = Vec::with_capacity(self.operators.len());
        for operator in &self.operators {
            let raw_id = regions.len().try_into().map_err(|_| {
                crate::SynthError::capacity("implementation region ID exceeds 32-bit capacity")
            })?;
            let synthesis_region = match operator.owner {
                Some(owner) => owner,
                None => operator_region_id(&operator.source_operations, synthesis_regions)?,
            };
            let source_operations = operator
                .source_operations
                .iter()
                .map(|&operation| {
                    synthesis_regions
                        .operation_anchor(operation)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "implementation source operation has no stable anchor",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            regions.push(ImplementationRegion::new(
                raw_id,
                crate::artifact::implementation::ImplementationRegionIdentity {
                    operator: operator.id,
                    candidate: operator.candidate,
                    synthesis_region,
                    width: operator.width,
                },
                ImplementationRegionSource {
                    operations: &source_operations,
                    lines: operator.lines.to_vec(),
                },
                ImplementationRegionMetadata {
                    recipe: &operator.recipe,
                    implementation: &operator.implementation,
                    module: &operator.module,
                    mnemonic: &operator.mnemonic,
                    source_file: operator.file.as_deref(),
                    source_line: operator.line,
                },
                std::mem::take(&mut cells_by_operator[operator.id.raw() as usize]),
            ));
        }

        let mut used_origins = std::collections::BTreeSet::from([OriginSetId::EMPTY]);
        used_origins.extend(cell_origins.iter().copied());
        let mut origin_remap = vec![None; self.origin_sets.len()];
        let mut origin_offsets = Vec::with_capacity(used_origins.len() + 1);
        let mut origin_operators = Vec::new();
        origin_offsets.push(0);
        for origin in used_origins {
            let remapped = OriginSetId(u32::try_from(origin_offsets.len() - 1).map_err(|_| {
                crate::SynthError::capacity("provenance origin-set count exceeds 32-bit capacity")
            })?);
            origin_remap[origin.0 as usize] = Some(remapped);
            origin_operators.extend_from_slice(&self.origin_sets[origin.0 as usize]);
            origin_offsets.push(origin_operators.len().try_into().map_err(|_| {
                crate::SynthError::capacity("provenance operator table exceeds 32-bit capacity")
            })?);
        }
        for origin in &mut cell_origins {
            *origin = origin_remap[origin.0 as usize].ok_or_else(|| {
                crate::SynthError::invariant("live mapped provenance origin was not compacted")
            })?;
        }
        ImplementationDb::new(
            mapped.generation_id(),
            regions.into_boxed_slice(),
            cell_origins,
            origin_offsets,
            origin_operators,
            cell_fragments,
        )
    }

    fn set_value_origin(&mut self, value: word::ValueId, origin: OriginSetId) {
        let index = value.index();
        if self.value_origins.len() <= index {
            self.value_origins.resize(index + 1, OriginSetId::EMPTY);
        }
        self.value_origins[index] = origin;
    }

    fn operators_for_value(&self, value: word::ValueId) -> &[OperatorId] {
        self.value_origins
            .get(value.index())
            .map_or(&[], |&origin| self.operators_for_origin(origin))
    }

    fn operators_for_origin(&self, origin: OriginSetId) -> &[OperatorId] {
        self.origin_sets
            .get(origin.0 as usize)
            .map_or(&[], Vec::as_slice)
    }

    fn intern(&mut self, mut operators: Vec<OperatorId>) -> Result<OriginSetId, crate::SynthError> {
        operators.sort_unstable();
        operators.dedup();
        let hash = implementation_origin_hash(&operators);
        if let Some(ids) = self.origin_ids.get(&hash)
            && let Some(&id) = ids
                .iter()
                .find(|&&id| self.operators_for_origin(id) == operators)
        {
            return Ok(id);
        }
        let id = OriginSetId(self.origin_sets.len().try_into().map_err(|_| {
            crate::SynthError::capacity("provenance origin-set ID exceeds 32-bit capacity")
        })?);
        self.origin_sets.push(operators);
        self.origin_ids.entry(hash).or_default().push(id);
        Ok(id)
    }

    fn take_visit_epoch(&mut self) -> u32 {
        let epoch = self.next_visit_epoch;
        self.next_visit_epoch = self.next_visit_epoch.wrapping_add(1);
        if self.next_visit_epoch == 0 {
            self.visit_epochs.fill(0);
            self.next_visit_epoch = 1;
        }
        epoch
    }
}

#[allow(
    clippy::redundant_closure_for_method_calls,
    reason = "the method's defining module is private, so its method-item path is not nameable here"
)]
fn operator_region_id(
    sources: &[word::OpId],
    graph: &crate::SynthesisRegionGraph,
) -> Result<crate::RegionAnchorId, crate::SynthError> {
    let (&first, rest) = sources
        .split_first()
        .ok_or_else(|| crate::SynthError::invariant("semantic operator has no source operation"))?;
    let owner = graph
        .operation_region(first)
        .map(|region| region.id())
        .ok_or_else(|| {
            crate::SynthError::invariant("semantic operator source has no synthesis region")
        })?;
    if rest
        .iter()
        .any(|&source| graph.operation_region(source).map(|region| region.id()) != Some(owner))
    {
        return Err(crate::SynthError::invariant(
            "resource-affinity operator crosses synthesis regions",
        ));
    }
    Ok(owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::mapped::MappedBuilder;
    use opto_ir::word::{BinaryOp, LValue, PortDirection, SourceSpan, WordModule, WordType};

    fn test_span() -> SourceSpan {
        SourceSpan::stable("test")
    }

    fn input(module: &mut WordModule, name: &str, ty: WordType) -> word::ValueId {
        let port = module
            .add_port(name, PortDirection::Input, ty, test_span())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, test_span())
            .unwrap()
    }

    fn operation(module: &WordModule, value: word::ValueId) -> word::OpId {
        match module.value(value).unwrap().kind {
            word::ValueKind::Operation(operation) => operation,
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => unreachable!(),
        }
    }

    #[test]
    fn records_each_arithmetic_operator_without_global_fusion() {
        let mut module = WordModule::new("sum");
        let ty = WordType::bits(4).unwrap();
        let ports = ["a", "b", "c", "d"].map(|name| {
            module
                .add_port(name, PortDirection::Input, ty, test_span())
                .unwrap()
        });
        let inputs = ports.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, test_span())
                .unwrap()
        });
        let sum = inputs[1..]
            .iter()
            .try_fold(inputs[0], |sum, &input| {
                module.binary(BinaryOp::Add, sum, input, test_span())
            })
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, ty, test_span())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                sum,
                test_span(),
            )
            .unwrap();

        let plan = ArchitectureDecisions::for_unfused_module(&module).unwrap();
        let synthesis_regions = crate::SynthesisRegionGraph::build(&module).unwrap();
        let provenance = ProvenanceBuilder::new(&module, &plan).unwrap();
        let mapped = MappedBuilder::new("sum", opto_ir::RevisionId::INITIAL)
            .unwrap()
            .freeze()
            .unwrap();
        let implementations = provenance
            .finish(&synthesis_regions, &module, &mapped, &[])
            .unwrap();
        assert_eq!(plan.operators().len(), 3);
        for operator in plan.operators() {
            let region = implementations.region_for_operator(operator.id()).unwrap();
            assert_eq!(
                region.source_operations(),
                [synthesis_regions
                    .operation_anchor(operator.source_operation())
                    .unwrap()]
            );
        }
    }

    #[test]
    fn resolves_generated_operators_only_through_explicit_provenance() {
        let ty = WordType::bits(8).unwrap();
        let mut source = WordModule::new("source");
        let source_inputs = ["a", "b", "c", "d"].map(|name| input(&mut source, name, ty));
        let represented_value = source
            .binary(
                BinaryOp::Mul,
                source_inputs[0],
                source_inputs[1],
                test_span(),
            )
            .unwrap();
        let unrelated_value = source
            .binary(
                BinaryOp::Mul,
                source_inputs[2],
                source_inputs[3],
                test_span(),
            )
            .unwrap();
        let represented = operation(&source, represented_value);
        let unrelated = operation(&source, unrelated_value);

        let mut local = WordModule::new("local");
        let local_inputs = ["a", "b", "c"].map(|name| input(&mut local, name, ty));
        let copied = local
            .binary(BinaryOp::Mul, local_inputs[0], local_inputs[1], test_span())
            .unwrap();
        let generated = local
            .binary(BinaryOp::Add, copied, local_inputs[2], test_span())
            .unwrap();
        let output = local
            .add_port("y", PortDirection::Output, ty, test_span())
            .unwrap();
        local
            .connect(
                LValue::signal(local.port(output).unwrap().signal),
                generated,
                test_span(),
            )
            .unwrap();

        let decisions = ArchitectureDecisions::for_module(&local).unwrap();
        let mut operation_sources = crate::planning::regional::LocalOperationProvenance::default();
        operation_sources
            .set(operation(&local, copied), [represented])
            .unwrap();
        operation_sources
            .set(operation(&local, generated), [represented, unrelated])
            .unwrap();
        let sources = resolve_private_operator_sources(
            &source,
            &local,
            &decisions,
            &[represented, unrelated],
            &operation_sources,
        )
        .unwrap();

        assert_eq!(sources.as_ref(), &[Box::from([represented, unrelated])]);
    }

    #[test]
    fn stores_many_to_many_direct_origins_in_compact_ids() {
        assert_eq!(std::mem::size_of::<OriginSetId>(), 4);

        let mut module = WordModule::new("top");
        let ty = WordType::bits(1).unwrap();
        let ports = ["a", "b", "c", "d"].map(|name| {
            module
                .add_port(name, PortDirection::Input, ty, test_span())
                .unwrap()
        });
        let inputs = ports.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, test_span())
                .unwrap()
        });
        let first = module
            .binary(BinaryOp::Add, inputs[0], inputs[1], test_span())
            .unwrap();
        let second = module
            .binary(BinaryOp::Add, inputs[2], inputs[3], test_span())
            .unwrap();
        for (name, value) in [("y0", first), ("y1", second)] {
            let output = module
                .add_port(name, PortDirection::Output, ty, test_span())
                .unwrap();
            module
                .connect(
                    LValue::signal(module.port(output).unwrap().signal),
                    value,
                    test_span(),
                )
                .unwrap();
        }
        let plan = ArchitectureDecisions::for_module(&module).unwrap();
        let mut provenance = ProvenanceBuilder::new(&module, &plan).unwrap();
        let origins = provenance
            .origins_for_operation_cover(&module, &[first, second], &inputs)
            .unwrap();
        let mut mapped = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let cell = mapped.add_cell("U0", "DUAL", None, &[]).unwrap();
        let mapped = mapped.freeze().unwrap();
        let synthesis_regions = crate::SynthesisRegionGraph::build(&module).unwrap();
        let owner = synthesis_regions.regions()[0].id();

        let implementations = provenance
            .finish(
                &synthesis_regions,
                &module,
                &mapped,
                &[(
                    cell,
                    MappedCellSource::Region {
                        origins,
                        region: owner,
                    },
                )],
            )
            .unwrap();
        let operators = plan
            .operators()
            .iter()
            .map(|operator| operator.id())
            .collect::<Vec<_>>();

        assert_eq!(
            implementations.operators_for_cell(cell),
            Some(operators.as_slice())
        );
        assert!(operators.iter().all(|&operator| {
            implementations
                .region_for_operator(operator)
                .unwrap()
                .mapped_cells()
                == [cell]
        }));
    }
}
