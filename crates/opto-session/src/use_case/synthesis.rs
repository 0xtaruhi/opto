// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Session, SessionError, SynthesisKey, design_graph, synthesis};
use opto_db::LinkBinding;
use opto_synth::{SourceFingerprint, SynthesisEffort, SynthesisMetrics};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod publication;

#[derive(Debug, Clone)]
/// Per-design synthesis progress forwarded by a traced hierarchy synthesis.
pub struct SynthesisTrace {
    /// Design definition whose pipeline emitted the observation.
    pub design: Arc<str>,
    /// Pipeline lifecycle or committed-candidate observation.
    pub progress: opto_synth::SynthesisProgress,
}

/// Thread-safe callback receiving per-design synthesis traces.
pub type SynthesisTraceSink<'a> = dyn Fn(SynthesisTrace) + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Coarse hierarchy-synthesis lifecycle event.
pub enum SynthesisEvent {
    /// Compilation of one design definition is about to begin.
    Started {
        /// Design definition entering compilation.
        design: String,
        /// Optimization effort selected for the hierarchy synthesis.
        effort: SynthesisEffort,
        /// Runtime workers available to this synthesis.
        parallelism: usize,
    },
    /// A sealed synthesis artifact has completed before session publication.
    ArtifactCompleted {
        /// Design definition represented by the artifact.
        design: String,
        /// Size and incremental-reuse metrics from the artifact.
        metrics: Box<SynthesisMetrics>,
    },
    /// Session design information is being refreshed from an artifact.
    DesignInformationUpdateStarted {
        /// Design definition whose session metadata is being refreshed.
        design: String,
        /// Optimization effort that produced the artifact.
        effort: SynthesisEffort,
    },
    /// Processing of one design definition reached its terminal state.
    Completed {
        /// Design definition that reached the terminal state.
        design: String,
        /// Whether this invocation synthesized rather than reused an artifact.
        synthesized: bool,
    },
}

fn reference_ports(
    session: &Session,
    graph: &design_graph::LinkedHierarchy,
    command: &str,
) -> Result<Arc<opto_synth::ReferencePortMap>, SessionError> {
    let mut references = opto_synth::ReferencePortMap::new();
    for &id in graph.postorder() {
        for instance in graph.instances(id) {
            if references.contains_key(instance.reference()) {
                continue;
            }
            let ports = match instance.binding() {
                LinkBinding::Design { definition, .. } => {
                    design_reference_ports(session, graph.definition_name(definition), command)?
                }
                LinkBinding::External { provider } => {
                    let cell = graph
                        .library_cell(provider, instance.reference())
                        .ok_or_else(|| {
                            SessionError::state(format!(
                                "{command}: linked provider '{}' does not contain cell '{}'",
                                graph.provider(provider).label(),
                                instance.reference()
                            ))
                        })?;
                    target_cell_reference_ports(cell)
                }
                LinkBinding::Unresolved => {
                    return Err(SessionError::state(format!(
                        "{command}: unresolved reference '{}' has no port interface",
                        instance.reference()
                    )));
                }
            };
            references.insert(instance.reference().to_string(), ports);
        }
    }
    Ok(Arc::new(references))
}

fn design_reference_ports(
    session: &Session,
    name: &str,
    command: &str,
) -> Result<BTreeMap<String, opto_synth::ReferencePort>, SessionError> {
    let design = session.state.designs.get(name).ok_or_else(|| {
        SessionError::state(format!("{command}: design '{name}' is missing from store"))
    })?;
    let module = design.source.word();
    module
        .ports()
        .iter()
        .map(|port| {
            let signal = module.signal(port.signal).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{name}' port '{}' references a missing signal",
                    module.name_str(port.name)
                ))
            })?;
            Ok((
                module.name_str(port.name).to_string(),
                opto_synth::ReferencePort {
                    direction: port.direction,
                    width: signal.ty.width(),
                    exact_width: false,
                },
            ))
        })
        .collect()
}

fn target_cell_reference_ports(
    cell: opto_library::TargetCellRef<'_>,
) -> BTreeMap<String, opto_synth::ReferencePort> {
    cell.pins()
        .filter_map(|pin| {
            let direction = match pin.direction() {
                opto_library::TargetPinDirection::Input => opto_ir::word::PortDirection::Input,
                opto_library::TargetPinDirection::Output => opto_ir::word::PortDirection::Output,
                opto_library::TargetPinDirection::Inout => opto_ir::word::PortDirection::Inout,
                opto_library::TargetPinDirection::Internal => return None,
            };
            Some((
                pin.name().to_string(),
                opto_synth::ReferencePort {
                    direction,
                    width: 1,
                    exact_width: true,
                },
            ))
        })
        .collect()
}
impl Session {
    /// Run structural validation on the current resolved design.
    pub fn check_design(&self) -> Result<String, SessionError> {
        let graph = self.definition_graph("check_design")?;
        design_graph::require_linked(&graph, "check_design")?;
        let reference_ports = reference_ports(self, &graph, "check_design")?;
        for id in graph.postorder() {
            let name = graph.definition_name(*id);
            let design = self.state.designs.get(name).ok_or_else(|| {
                SessionError::state(format!(
                    "check_design: design '{name}' is missing from store"
                ))
            })?;
            design.source.validate()?;
            opto_synth::check_design_with_references(design.source.word(), &reference_ports)?;
        }
        Ok("1".to_string())
    }

    /// Synthesize the current design using the typed database policy.
    pub fn synthesize(&mut self) -> Result<String, SessionError> {
        self.synthesize_observed(self.state.settings.synth_effort, &mut |_| {})
    }

    /// Synthesize with explicit effort and observe design and pipeline events.
    pub fn synthesize_observed(
        &mut self,
        effort: SynthesisEffort,
        observer: &mut dyn FnMut(SynthesisEvent),
    ) -> Result<String, SessionError> {
        self.run_synthesis(effort, false, observer, &|_| {})
    }

    /// Synthesize while forwarding per-design progress to a trace sink.
    pub fn synthesize_traced(
        &mut self,
        observer: &mut dyn FnMut(SynthesisEvent),
        trace: &SynthesisTraceSink<'_>,
    ) -> Result<String, SessionError> {
        self.run_synthesis(
            self.state.settings.synth_effort,
            self.state.settings.clock_gating,
            observer,
            trace,
        )
    }

    fn run_synthesis(
        &mut self,
        effort: SynthesisEffort,
        gate_clock: bool,
        observer: &mut dyn FnMut(SynthesisEvent),
        trace: &SynthesisTraceSink<'_>,
    ) -> Result<String, SessionError> {
        let command = "synth";
        let clock_gating = gate_clock.then_some(self.state.settings.clock_gating_style);
        let current_name = self.current_design_name()?.to_string();
        let options = self.synthesis_options()?;
        let graph = self.definition_graph(command)?;
        design_graph::require_linked(&graph, command)?;
        let synthesis_revision = self.state.revision;
        let timing = Arc::new(self.state.timing.clone());
        let timing_revision = timing.revision();
        let timing_library = Arc::new(self.timing_library()?);
        let library_revision = self.process.libraries.current().id();
        let mapping_libraries = self.mapping_library_selection();
        let resolution_libraries = self.resolution_library_selection();
        let resolution_provider_fingerprint = self
            .process
            .libraries
            .current()
            .selection_fingerprint(&resolution_libraries)?;
        let mapping_library_fingerprint = options.target_cells.content_fingerprint();
        let timing_library_fingerprint = timing_library.content_fingerprint();
        let timing_fingerprint = timing.synthesis_fingerprint();
        let synthesis_references = synthesis::SynthesisReferences {
            names: Arc::new(
                graph
                    .postorder()
                    .iter()
                    .flat_map(|id| graph.instances(*id))
                    .filter_map(|instance| match instance.binding() {
                        LinkBinding::Design { definition, .. } => {
                            Some(graph.definition_name(definition).to_string())
                        }
                        LinkBinding::External { .. } | LinkBinding::Unresolved => None,
                    })
                    .collect::<BTreeSet<_>>(),
            ),
            ports: reference_ports(self, &graph, command)?,
        };
        let root = self.state.designs.get(&current_name).ok_or_else(|| {
            SessionError::state(format!(
                "{command}: current design '{current_name}' is missing from store"
            ))
        })?;
        let mut definitions = graph
            .postorder()
            .iter()
            .map(|definition| {
                let name = graph.definition_name(*definition);
                self.state
                    .designs
                    .get(name)
                    .map(|record| (&record.source, graph.occurrence_count(*definition)))
                    .ok_or_else(|| {
                        SessionError::state(format!(
                            "{command}: design '{name}' is missing from store"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        definitions
            .sort_unstable_by(|(left, _), (right, _)| left.word().name().cmp(right.word().name()));
        let source_fingerprint =
            SourceFingerprint::capture_hierarchy(&current_name, definitions.iter().copied());
        let mut publish_revision = None;
        let mut pending = Vec::new();
        let parasitics = self
            .state
            .parasitics
            .get(&current_name)
            .map_or_else(opto_timing::Parasitics::default, |(_, value)| value.clone());
        let key = SynthesisKey {
            source: source_fingerprint,
            timing: timing_fingerprint,
            parasitics: parasitics.content_fingerprint(),
            resolution_providers: resolution_provider_fingerprint,
            mapping_library: mapping_library_fingerprint,
            timing_library: timing_library_fingerprint,
            activity: self.state.power.synthesis_fingerprint(),
            synthesis_config: self.process.synthesis_config,
            effort,
            clock_gating,
        };
        let cached = root
            .synthesis_binding
            .as_ref()
            .is_some_and(|binding| binding.content_key == key)
            && root.synthesized.is_some();
        if !cached {
            publish_revision = Some(self.next_revision()?);
            let request = synthesis::synthesis_request(
                self,
                synthesis::SynthesisInputs {
                    source: &root.source,
                    previous_incremental: root.incremental_snapshot(),
                    references: synthesis_references,
                    options,
                    effort,
                    clock_gating,
                },
                &timing,
                &timing_library,
            )?;
            let request = synthesis::ArtifactSynthesisRequest {
                source_revision: root.source_revision,
                key,
                request,
                definitions: definitions
                    .iter()
                    .map(|(definition, _)| *definition)
                    .filter(|definition| definition.word().name() != current_name)
                    .collect(),
            };
            observer(SynthesisEvent::Started {
                design: current_name.clone(),
                effort,
                parallelism: self.process.runtime.parallelism(),
            });
            let output = synthesis::synthesis_artifact(
                &self.process.synthesis_engine,
                &self.process.runtime,
                command,
                current_name.clone(),
                request,
                trace,
            )?;
            observer(SynthesisEvent::ArtifactCompleted {
                design: output.name.clone(),
                metrics: Box::new(output.synthesis.metrics()),
            });
            pending.push(output);
        }

        let has_updates = !pending.is_empty();
        let inputs = publication::SynthesisInputSnapshot {
            revision: synthesis_revision,
            timing_revision,
            library_revision,
            mapping_libraries,
            resolution_libraries,
        };
        let publication = publication::CompilationPublication::prepare(
            self,
            &inputs,
            command,
            current_name.clone(),
            publish_revision,
            pending,
        )?;
        if has_updates {
            observer(SynthesisEvent::DesignInformationUpdateStarted {
                design: current_name.clone(),
                effort,
            });
        }
        publication.commit(self)?;
        observer(SynthesisEvent::Completed {
            design: current_name,
            synthesized: has_updates,
        });
        Ok("1".to_string())
    }
}
