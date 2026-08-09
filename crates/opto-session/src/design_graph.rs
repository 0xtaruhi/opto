// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::DesignStore;
use opto_db::{DefinitionGraph, DefinitionInput, InstanceInput, LinkProviderInput, ProviderId};
use opto_library::{LibraryLinkPlan, LibraryLinkProvider, TargetCellRef, TargetCellSet};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

#[derive(Clone)]
enum LinkedProviderSource {
    DesignMemory,
    Library {
        cells: TargetCellSet,
        matched_cells: BTreeMap<String, usize>,
    },
}

#[derive(Clone)]
pub(crate) struct LinkedHierarchy {
    graph: DefinitionGraph,
    providers: Box<[LinkedProviderSource]>,
}

impl LinkedHierarchy {
    pub(crate) fn library_cell(
        &self,
        provider: ProviderId,
        reference: &str,
    ) -> Option<TargetCellRef<'_>> {
        let LinkedProviderSource::Library {
            cells,
            matched_cells,
            ..
        } = self.providers.get(provider.index())?
        else {
            return None;
        };
        cells.get(*matched_cells.get(reference)?)
    }
}

impl Deref for LinkedHierarchy {
    type Target = DefinitionGraph;

    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

pub(crate) fn build_definition_graph(
    modules: &DesignStore,
    plan: LibraryLinkPlan,
    root: &str,
    command: &str,
) -> Result<LinkedHierarchy, crate::SessionError> {
    let definitions = modules
        .iter()
        .map(|(name, design)| {
            let module = design.source.word();
            DefinitionInput::new(
                name,
                module
                    .instances()
                    .iter()
                    .map(|instance| {
                        InstanceInput::new(
                            module.name_str(instance.name),
                            module.name_str(instance.module),
                        )
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let references = definitions
        .iter()
        .flat_map(|definition| &definition.instances)
        .map(|instance| instance.reference.as_str())
        .collect::<BTreeSet<_>>();
    let providers = plan.into_providers();
    let mut provider_inputs = Vec::with_capacity(providers.len());
    let mut provider_sources = Vec::with_capacity(providers.len());
    for provider in providers {
        match provider {
            LibraryLinkProvider::DesignMemory => {
                provider_inputs.push(LinkProviderInput::definitions("*"));
                provider_sources.push(LinkedProviderSource::DesignMemory);
            }
            LibraryLinkProvider::Library { library, cells } => {
                let mut matched_cells = BTreeMap::new();
                for (index, cell) in cells.iter().enumerate() {
                    if references.contains(cell.name()) {
                        matched_cells
                            .entry(cell.name().to_string())
                            .or_insert(index);
                    }
                }
                provider_inputs.push(LinkProviderInput::external(
                    format!("{} ({})", library.name, library.source),
                    matched_cells.keys().cloned(),
                ));
                provider_sources.push(LinkedProviderSource::Library {
                    cells,
                    matched_cells,
                });
            }
        }
    }
    let graph = DefinitionGraph::build(definitions, provider_inputs, root).map_err(|source| {
        crate::SessionError::DefinitionGraphContext {
            command: command.to_string(),
            source,
        }
    })?;
    debug_assert_eq!(graph.providers().len(), provider_sources.len());
    Ok(LinkedHierarchy {
        graph,
        providers: provider_sources.into_boxed_slice(),
    })
}

pub(crate) fn require_linked(
    graph: &DefinitionGraph,
    command: &str,
) -> Result<(), crate::SessionError> {
    let Some(unresolved) = graph.first_unresolved() else {
        return Ok(());
    };
    let additional = graph.unresolved_occurrence_count() - 1;
    let suffix = if additional == 0 {
        String::new()
    } else {
        format!(" (and {additional} more)")
    };
    Err(crate::SessionError::state(format!(
        "{command}: hierarchy resolution failed at cell '{}': unresolved reference '{}'{}",
        unresolved.path(),
        unresolved.reference(),
        suffix
    )))
}
