// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    Session, SessionError, TimingDesignGeneration, TimingModelCache, TimingModelKey, timing,
};
use opto_db::RevisionId;
use opto_ir::word;
use opto_timing::{Parasitics, TimingDesign, TimingModel};
use std::sync::Arc;

fn timing_design(
    session: &Session,
    module: &word::WordModule,
) -> Result<TimingDesign, SessionError> {
    let design = session.design_by_name(module.name())?;
    timing::design(
        module,
        session.design_uid(module.name())?,
        &session.port_bindings(design)?,
    )
}

fn design_generation(
    session: &Session,
    design: &str,
) -> Result<TimingDesignGeneration, SessionError> {
    let record = session.state.designs.get(design).ok_or_else(|| {
        SessionError::state(format!("current design '{design}' is missing from store"))
    })?;
    match (&record.synthesized, &record.synthesis_binding) {
        (Some(_), Some(binding)) => Ok(TimingDesignGeneration::Artifact {
            source_revision: record.source_revision,
            published_revision: binding.published_revision,
            effort: binding.content_key.effort,
        }),
        (Some(_), None) => Err(SessionError::state(
            "timing: synthesized design has no artifact binding",
        )),
        (None, Some(_)) => Err(SessionError::state(
            "timing: artifact binding has no synthesized design",
        )),
        (None, None) => Ok(TimingDesignGeneration::Source {
            source_revision: record.source_revision,
        }),
    }
}

fn current_timing_model_key(session: &Session) -> Result<TimingModelKey, SessionError> {
    let design = session.current_design_name()?.to_string();
    let resolution_libraries = session.resolution_library_selection();
    let parasitics_revision = session
        .state
        .parasitics
        .get(&design)
        .map_or(RevisionId::INITIAL, |(revision, _)| *revision);
    Ok(TimingModelKey {
        design_generation: design_generation(session, &design)?,
        library: session
            .process
            .libraries
            .current()
            .selection_fingerprint(&resolution_libraries)?,
        parasitics_revision,
        design,
    })
}

impl Session {
    /// Eagerly releases a derived analysis generation only when a successful
    /// semantic publication changed one of its actual inputs.
    pub(crate) fn clear_stale_analysis_generation(&self) {
        let Some(cached_key) = self
            .process
            .timing_model_cache
            .borrow()
            .as_ref()
            .map(|cache| cache.key.clone())
        else {
            return;
        };
        if current_timing_model_key(self).map_or(true, |key| key != cached_key) {
            self.process.clear_analysis_caches();
        }
    }
}

pub(super) fn current_timing_model(session: &Session) -> Result<Arc<TimingModel>, SessionError> {
    let key = current_timing_model_key(session)?;
    if let Some(model) = session
        .process
        .timing_model_cache
        .borrow()
        .as_ref()
        .filter(|cache| cache.key == key)
        .map(|cache| Arc::clone(&cache.model))
    {
        return Ok(model);
    }

    session.process.clear_analysis_caches();
    let parasitics = session
        .state
        .parasitics
        .get(&key.design)
        .map_or_else(Parasitics::default, |(_, parasitics)| parasitics.clone());
    let model = Arc::new(current_timing_model_with_parasitics(
        session,
        &key.design,
        parasitics,
    )?);
    *session.process.timing_model_cache.borrow_mut() = Some(TimingModelCache {
        key,
        model: Arc::clone(&model),
    });
    Ok(model)
}

pub(super) fn install_current_timing_model(
    session: &Session,
    model: TimingModel,
) -> Result<(), SessionError> {
    let key = current_timing_model_key(session)?;
    *session.process.timing_model_cache.borrow_mut() = Some(TimingModelCache {
        key,
        model: Arc::new(model),
    });
    Ok(())
}

pub(super) fn current_timing_model_with_parasitics(
    session: &Session,
    design: &str,
    parasitics: Parasitics,
) -> Result<TimingModel, SessionError> {
    let record = session.state.designs.get(design).ok_or_else(|| {
        SessionError::state(format!("current design '{design}' is missing from store"))
    })?;
    let design_uid = session.design_uid(design)?;
    let port_bindings = session.port_bindings(crate::DesignView::from_record(record))?;
    let object_bindings = timing_object_bindings(session, design)?;
    // LibraryStore materializes the selected immutable view at its own
    // boundary.
    let library = session.timing_library()?;
    match (
        record.synthesized.as_ref(),
        record.synthesis_binding.as_ref(),
    ) {
        (Some(synthesis), Some(_)) => {
            let graph = session.definition_graph("timing")?;
            let references = super::synthesis::reference_ports(session, &graph, "timing")?;
            let design_ports = references
                .iter()
                .map(|(module, ports)| {
                    (
                        module.clone(),
                        ports
                            .iter()
                            .map(|(port, contract)| (port.clone(), contract.direction))
                            .collect(),
                    )
                })
                .collect();
            let mut model = TimingModel::from_mapped_with_parasitics(
                synthesis.mapped(),
                design_uid,
                &port_bindings,
                library,
                parasitics,
                &design_ports,
            )?;
            model.set_object_bindings(object_bindings);
            model.compact()?;
            Ok(model)
        }
        (Some(_), None) => Err(SessionError::state(
            "timing: synthesized design has no artifact binding",
        )),
        (None, Some(_)) => Err(SessionError::state(
            "timing: artifact binding has no synthesized design",
        )),
        (None, None) => {
            let module = record.source.word();
            let mut model = TimingModel::new_with_parasitics(
                timing_design(session, module)?,
                library,
                parasitics,
            )?;
            model.set_object_bindings(object_bindings);
            model.compact()?;
            Ok(model)
        }
    }
}

pub(crate) fn timing_object_bindings(
    session: &Session,
    design: &str,
) -> Result<opto_timing::TimingObjectBindings, SessionError> {
    let mut bindings = opto_timing::TimingObjectBindings::builder();
    for id in session.state.objects.design_objects(design) {
        let Some(object) = session.state.objects.resolve(id) else {
            continue;
        };
        match (id, object) {
            (opto_db::AnyObjectId::Cell(id), opto_db::ResolvedObject::Cell { name, .. }) => {
                bindings.bind_cell(name, id)?;
            }
            (opto_db::AnyObjectId::Pin(id), opto_db::ResolvedObject::Pin { full_name, .. }) => {
                bindings.bind_pin(full_name, id)?;
            }
            (opto_db::AnyObjectId::Net(id), opto_db::ResolvedObject::Net { name, .. }) => {
                bindings.bind_net(name, id)?;
            }
            _ => {}
        }
    }
    Ok(bindings.finish()?)
}
