// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{Session, SynthesisKey};
use crate::activity::{ActivityTarget, resolve_activity_annotations};
use opto_db::AnyObjectId;
use opto_ir::rtl::RtlModule;
use opto_runtime::ExecutionContext;
use opto_synth::{
    SynthesisEffort, SynthesisEngine, SynthesisOptions, SynthesisRequest, SynthesisResult,
};
use opto_timing::{
    Scenario, ScenarioActivityTarget, ScenarioPowerView, ScenarioSet, ScenarioSwitchingActivity,
    TimingContext, TimingLibrary,
};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

pub(super) struct ArtifactSynthesisRequest<'a> {
    pub(super) source_revision: opto_db::RevisionId,
    pub(super) key: SynthesisKey,
    pub(super) request: SynthesisRequest<'a>,
    pub(super) definitions: Vec<&'a RtlModule>,
}

#[derive(Clone)]
pub(super) struct SynthesisReferences {
    pub(super) names: Arc<BTreeSet<String>>,
    pub(super) ports: Arc<opto_synth::ReferencePortMap>,
}

pub(super) struct ArtifactSynthesisOutput {
    pub(super) name: String,
    pub(super) source_revision: opto_db::RevisionId,
    pub(super) key: SynthesisKey,
    pub(super) synthesis: SynthesisResult,
}

pub(super) struct SynthesisInputs<'a> {
    pub(super) source: &'a RtlModule,
    pub(super) previous_incremental: Option<&'a opto_synth::IncrementalSnapshot>,
    pub(super) references: SynthesisReferences,
    pub(super) options: SynthesisOptions,
    pub(super) effort: SynthesisEffort,
    pub(super) clock_gating: Option<opto_synth::ClockGatingStyle>,
}

pub(super) fn synthesis_request<'a>(
    session: &Session,
    inputs: SynthesisInputs<'a>,
    timing: &Arc<TimingContext>,
    timing_library: &Arc<TimingLibrary>,
) -> Result<SynthesisRequest<'a>, crate::SessionError> {
    let SynthesisInputs {
        source,
        previous_incremental,
        references,
        options,
        effort,
        clock_gating,
    } = inputs;
    let name = source.word().name();
    let design_id = session.design_uid(name)?;
    let port_bindings = session.port_bindings(session.design_by_name(name)?)?;
    let object_bindings = Arc::new(crate::use_case::timing_model::timing_object_bindings(
        session, name,
    )?);
    let activities = scenario_activities(session, name)?;
    let parasitics = session
        .state
        .parasitics
        .get(name)
        .map_or_else(opto_timing::Parasitics::default, |(_, parasitics)| {
            parasitics.clone()
        });
    Ok(SynthesisRequest {
        base_revision: session.state.revision,
        design_id,
        port_bindings,
        object_bindings,
        source: Cow::Borrowed(source),
        design_references: Arc::clone(&references.names),
        reference_ports: references.ports,
        options,
        effort,
        clock_gating,
        scenarios: ScenarioSet::new(vec![
            Scenario::single(Arc::clone(timing), Arc::clone(timing_library), parasitics)
                .with_power(
                    ScenarioPowerView::new(Arc::new(timing_library.power.clone()), activities)
                        .map_err(|error| crate::SessionError::state(error.to_string()))?,
                ),
        ])
        .expect("the session's explicit default scenario is valid"),
        power_evaluator: Arc::new(SessionSynthesisPowerEvaluator),
        previous_incremental,
    })
}

#[derive(Debug)]
struct SessionSynthesisPowerEvaluator;

impl opto_synth::SynthesisPowerEvaluator for SessionSynthesisPowerEvaluator {
    fn dynamic_power_watts(
        &self,
        runtime: &ExecutionContext,
        scenario: &Scenario,
        model: &opto_timing::TimingModel,
        electrical: &dyn Fn() -> Result<opto_timing::TimingElectricalSnapshot, String>,
    ) -> Result<Option<f64>, String> {
        let Some(annotations) = power_annotations(scenario, model)? else {
            return Ok(None);
        };
        let timing_nets = electrical()?;
        opto_power::PowerAnalysis::analyze(runtime, model, &timing_nets, &annotations)
            .map(|analysis| Some(analysis.summary().dynamic_watts()))
            .map_err(|error| error.to_string())
    }
}

fn power_annotations(
    scenario: &Scenario,
    model: &opto_timing::TimingModel,
) -> Result<Option<opto_power::ActivityAnnotations>, String> {
    if scenario.power().activity_fingerprint().is_none() {
        return Ok(None);
    }
    let mut targets = Vec::new();
    for (target, activity) in scenario.power().activities() {
        let activity = opto_power::SwitchingActivity::new(
            activity.static_probability(),
            activity.toggle_rate(),
            activity.rise_ratio(),
        )
        .map_err(|error| error.to_string())?;
        match target {
            ScenarioActivityTarget::Port(port) => {
                targets.push((ActivityTarget::Port(*port), activity));
            }
            ScenarioActivityTarget::Net(net) => {
                targets.push((ActivityTarget::Net(*net), activity));
            }
        }
    }
    let annotations = resolve_activity_annotations(model, targets)?;
    if model
        .net_ids()
        .filter(|&net| model.net_is_input_port(net))
        .any(|net| !annotations.contains(net))
    {
        return Ok(None);
    }
    Ok(Some(annotations))
}

fn scenario_activities(
    session: &Session,
    design: &str,
) -> Result<Vec<(ScenarioActivityTarget, ScenarioSwitchingActivity)>, crate::SessionError> {
    let mut activities = Vec::new();
    for (&object, &activity) in &session.state.power.activities {
        let locator = session.state.objects.resolve(object).ok_or_else(|| {
            crate::SessionError::state(format!(
                "synthesis: switching activity references missing object {object:?}"
            ))
        })?;
        if locator.design_name() != Some(design) {
            continue;
        }
        let target = match object {
            AnyObjectId::Port(port) => ScenarioActivityTarget::Port(port),
            AnyObjectId::Net(net) => ScenarioActivityTarget::Net(net),
            _ => {
                return Err(crate::SessionError::state(format!(
                    "synthesis: switching activity references unsupported object {object:?}"
                )));
            }
        };
        let activity = ScenarioSwitchingActivity::new(
            activity.static_probability(),
            activity.toggle_rate(),
            activity.rise_ratio(),
        )
        .ok_or_else(|| crate::SessionError::state("synthesis: switching activity is invalid"))?;
        activities.push((target, activity));
    }
    Ok(activities)
}

pub(super) fn synthesis_artifact(
    engine: &SynthesisEngine,
    runtime: &ExecutionContext,
    command: &'static str,
    name: String,
    task: ArtifactSynthesisRequest<'_>,
    trace: &crate::SynthesisTraceSink<'_>,
) -> Result<ArtifactSynthesisOutput, crate::SessionError> {
    let design: Arc<str> = Arc::from(name.as_str());
    trace(crate::SynthesisTrace {
        design: Arc::clone(&design),
        progress: opto_synth::SynthesisProgress::started(opto_synth::StageId::LINKED_ELABORATION),
    });
    let source = match opto_ir::rtl::elaborate_linked_root(
        task.request.source.as_ref(),
        task.definitions.iter().copied(),
    ) {
        Ok(source) => {
            trace(crate::SynthesisTrace {
                design: Arc::clone(&design),
                progress: opto_synth::SynthesisProgress::completed(
                    opto_synth::StageId::LINKED_ELABORATION,
                ),
            });
            source
        }
        Err(error) => {
            trace(crate::SynthesisTrace {
                design: Arc::clone(&design),
                progress: opto_synth::SynthesisProgress::failed(
                    opto_synth::StageId::LINKED_ELABORATION,
                ),
            });
            return Err(error.into());
        }
    };
    let synthesis = engine
        .synthesize(
            task.request.with_linked_source(&source),
            runtime,
            &mut |progress| {
                trace(crate::SynthesisTrace {
                    design: Arc::clone(&design),
                    progress,
                });
            },
        )
        .map_err(|source| crate::SessionError::synthesis(command, design.as_ref(), source))?;
    Ok(ArtifactSynthesisOutput {
        name,
        source_revision: task.source_revision,
        key: task.key,
        synthesis,
    })
}
