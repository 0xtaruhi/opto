// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod atomic_file;
mod checkpoint;
mod constraints;
mod directives;
mod frontend;
mod parasitics;
mod power;
mod report;
mod synthesis;
pub(crate) mod timing_model;
mod write;

pub use frontend::HdlCatalog;
pub use parasitics::{ReadParasiticsCompletion, ReadParasiticsOptions};
pub use synthesis::{SynthesisEvent, SynthesisTrace, SynthesisTraceSink};

#[cfg(test)]
pub(crate) use frontend::CurrentDesignPolicy;

use crate::{Session, SessionError, design_graph};
use opto_db::{DefinitionId, LinkBinding};
use std::collections::BTreeSet;

impl Session {
    /// Return clocks whose source set contains `port`.
    pub fn clocks_on_port(&self, port: opto_db::PortId) -> Vec<opto_db::ClockId> {
        self.state
            .timing
            .clocks()
            .iter()
            .filter(|clock| clock.sources.contains(&port))
            .map(|clock| clock.id)
            .collect()
    }

    /// Borrows path exceptions in stable insertion order.
    pub fn path_exceptions(&self) -> opto_timing::TimingRows<'_, opto_timing::PathException> {
        self.state.timing.path_exceptions()
    }

    #[cfg(test)]
    pub(crate) fn current_timing_model(
        &self,
    ) -> Result<std::sync::Arc<opto_timing::TimingModel>, SessionError> {
        timing_model::current_timing_model(self)
    }

    pub(crate) fn definition_graph(
        &self,
        command: &str,
    ) -> Result<std::sync::Arc<design_graph::LinkedHierarchy>, SessionError> {
        let current = self.current_design_name()?.to_string();
        let selection = self.resolution_library_selection();
        let providers = self
            .process
            .libraries
            .current()
            .selection_fingerprint(&selection)?;
        let key = crate::DefinitionGraphCacheKey {
            root: current.clone(),
            providers,
            designs: self
                .state
                .designs
                .iter()
                .map(|(name, record)| (name.clone(), record.source_revision))
                .collect(),
        };
        if let Some(graph) = self
            .process
            .definition_graph_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map(|cache| std::sync::Arc::clone(&cache.graph))
        {
            return Ok(graph);
        }
        let plan = self.active_link_plan()?;
        let graph = std::sync::Arc::new(design_graph::build_definition_graph(
            &self.state.designs,
            plan,
            &current,
            command,
        )?);
        *self.process.definition_graph_cache.borrow_mut() = Some(crate::DefinitionGraphCache {
            key,
            graph: std::sync::Arc::clone(&graph),
        });
        Ok(graph)
    }

    pub(crate) fn collect_design_modules(
        &self,
        command: &str,
        roots: &[String],
        hierarchy: bool,
    ) -> Result<Vec<String>, SessionError> {
        self.collect_modules(command, roots, hierarchy, false)
    }

    pub(crate) fn collect_source_design_modules(
        &self,
        command: &str,
        roots: &[String],
        hierarchy: bool,
    ) -> Result<Vec<String>, SessionError> {
        self.collect_modules(command, roots, hierarchy, true)
    }

    fn collect_modules(
        &self,
        command: &str,
        roots: &[String],
        hierarchy: bool,
        source_hierarchy: bool,
    ) -> Result<Vec<String>, SessionError> {
        if !hierarchy {
            let mut seen = BTreeSet::new();
            return Ok(roots
                .iter()
                .filter(|root| seen.insert((*root).clone()))
                .cloned()
                .collect());
        }
        let plan = self.active_link_plan()?;
        let mut visited = BTreeSet::new();
        let mut ordered = Vec::new();
        for root in roots {
            let graph = design_graph::build_definition_graph(
                &self.state.designs,
                plan.clone(),
                root,
                command,
            )?;
            collect_linked_design_modules(
                self,
                &graph,
                graph.root(),
                command,
                source_hierarchy,
                &mut visited,
                &mut ordered,
            )?;
        }
        Ok(ordered)
    }
}

fn collect_linked_design_modules(
    session: &Session,
    graph: &design_graph::LinkedHierarchy,
    definition: DefinitionId,
    command: &str,
    source_hierarchy: bool,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), SessionError> {
    let name = graph.definition_name(definition);
    if !visited.insert(name.to_string()) {
        return Ok(());
    }
    ordered.push(name.to_string());
    let record = session.state.designs.get(name).ok_or_else(|| {
        SessionError::state(format!("{command}: design '{name}' is missing from store"))
    })?;
    if !source_hierarchy && let Some(synthesis) = &record.synthesized {
        for child_name in synthesis
            .mapped()
            .design_instance_ids()
            .filter_map(|instance| synthesis.mapped().design_instance_module(instance))
        {
            let child = graph.definition_id(child_name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: mapped child design '{child_name}' is missing from store"
                ))
            })?;
            collect_linked_design_modules(
                session,
                graph,
                child,
                command,
                source_hierarchy,
                visited,
                ordered,
            )?;
        }
    } else {
        for instance in graph.instances(definition) {
            if let LinkBinding::Design {
                definition: child, ..
            } = instance.binding()
            {
                collect_linked_design_modules(
                    session,
                    graph,
                    child,
                    command,
                    source_hierarchy,
                    visited,
                    ordered,
                )?;
            }
        }
    }
    Ok(())
}
