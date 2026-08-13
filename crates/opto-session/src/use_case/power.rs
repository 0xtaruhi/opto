// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::timing_model;
use crate::activity::{ActivityTarget, resolve_activity_annotations};
use crate::{
    DelayType, PowerEngineMetrics, ReportPowerOptions, Session, SessionError,
    SwitchingActivityUpdate,
};
use opto_db::{AnyObjectId, ResolvedObject};
use opto_power::{ActivityAnnotations, SwitchingActivity};
use opto_timing::TimingModel;
use std::sync::Arc;

fn power_analysis(
    session: &Session,
) -> Result<(Arc<TimingModel>, opto_power::PowerAnalysis), SessionError> {
    let design = session.current_design_name()?.to_string();
    let record = session.state.designs.get(&design).ok_or_else(|| {
        SessionError::state(format!("current design '{design}' is missing from store"))
    })?;
    if record.synthesized.is_none() {
        return Err(SessionError::state(
            "report_power: current design is not synthesized",
        ));
    }
    let model = timing_model::current_timing_model(session)?;
    let timing_nets = session.process.timing_engine.electrical_snapshot(
        &session.state.timing,
        Arc::clone(&model),
        DelayType::Max,
    )?;
    let annotations = power_annotations(session, &design, &model)?;
    let analysis = session
        .process
        .power_engine
        .analyze(
            &session.process.runtime,
            Arc::clone(&model),
            timing_nets,
            annotations,
        )
        .map_err(SessionError::from)?;
    let selection = session.resolution_library_selection();
    let libraries = session
        .process
        .libraries
        .current()
        .selected_libraries(&selection)?
        .into_iter()
        .map(|library| opto_power::PowerLibraryReference {
            name: library.name,
            source: Some(library.source),
        })
        .collect();
    Ok((model, analysis.with_libraries(libraries)))
}

fn power_annotations(
    session: &Session,
    design: &str,
    model: &TimingModel,
) -> Result<ActivityAnnotations, SessionError> {
    let mut targets = Vec::new();
    for (&object, &activity) in &session.state.power.activities {
        let locator = session.state.objects.resolve(object).ok_or_else(|| {
            SessionError::state(format!(
                "report_power: switching activity references missing object {object:?}"
            ))
        })?;
        if locator.design_name() != Some(design) {
            continue;
        }
        match object {
            AnyObjectId::Port(port) => targets.push((ActivityTarget::Port(port), activity)),
            AnyObjectId::Net(net) => targets.push((ActivityTarget::Net(net), activity)),
            _ => {
                return Err(SessionError::state(format!(
                    "report_power: switching activity references unsupported object {object:?}"
                )));
            }
        }
    }
    resolve_activity_annotations(model, targets).map_err(SessionError::state)
}
impl Session {
    /// Apply explicit switching activity to resolved power objects.
    pub fn set_switching_activity(
        &mut self,
        update: SwitchingActivityUpdate,
        objects: &[AnyObjectId],
    ) -> Result<String, SessionError> {
        if objects.is_empty() {
            return Err(SessionError::object(
                "set_switching_activity: no objects specified",
            ));
        }
        let design = self.current_design_name()?.to_string();
        for object in objects {
            let locator = self.state.objects.resolve(*object).ok_or_else(|| {
                SessionError::object(format!(
                    "set_switching_activity: object {object:?} no longer exists"
                ))
            })?;
            if !matches!(
                locator,
                ResolvedObject::Port { .. } | ResolvedObject::Net { .. }
            ) {
                return Err(SessionError::object(format!(
                    "set_switching_activity: '{}' is not a port or net in current design '{design}'",
                    locator.object_name()
                )));
            }
            if locator.design_name() != Some(design.as_str()) {
                return Err(SessionError::object(format!(
                    "set_switching_activity: definition-level object '{}' cannot be broadcast to occurrences of current design '{design}'; annotate a root port or net",
                    locator.object_name()
                )));
            }
        }
        let updates = objects
            .iter()
            .map(|object| {
                let current = self
                    .state
                    .power
                    .activities
                    .get(object)
                    .copied()
                    .unwrap_or_else(SwitchingActivity::quiescent);
                update.apply(current).map(|activity| (*object, activity))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed = updates.iter().any(|(object, activity)| {
            self.state.power.activities.get(object).copied() != Some(*activity)
        });
        if changed {
            let revision = self.state.power.revision.next()?;
            for (object, activity) in updates {
                self.state.power.activities.insert(object, activity);
            }
            self.state.power.revision = revision;
        }
        Ok(String::new())
    }

    /// Remove explicit switching activity from resolved power objects.
    pub fn reset_switching_activity(
        &mut self,
        objects: &[AnyObjectId],
    ) -> Result<String, SessionError> {
        let design = self.current_design_name()?.to_string();
        let changed = if objects.is_empty() {
            let old_len = self.state.power.activities.len();
            self.state.power.activities.retain(|object, _| {
                self.state
                    .objects
                    .resolve(*object)
                    .is_some_and(|locator| locator.design_name() != Some(design.as_str()))
            });
            self.state.power.activities.len() != old_len
        } else {
            let mut changed = false;
            for object in objects {
                changed |= self.state.power.activities.remove(object).is_some();
            }
            changed
        };
        if changed {
            self.state.power.revision = self.state.power.revision.next()?;
        }
        Ok(String::new())
    }

    /// Analyze and render power for the current design.
    pub fn report_power(&self, options: &ReportPowerOptions) -> Result<String, SessionError> {
        let (model, analysis) = power_analysis(self)?;
        Ok(opto_formats::report_power(&model, &analysis, options)?.render_plain())
    }

    /// Return cache and recomputation metrics from the power engine.
    pub fn power_engine_metrics(&self) -> Result<PowerEngineMetrics, SessionError> {
        self.process.power_engine.metrics().map_err(Into::into)
    }
}
