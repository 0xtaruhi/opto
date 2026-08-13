// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    ArtifactBinding, MappedObjectIndex, Session, SessionError, synthesis::ArtifactSynthesisOutput,
    transaction,
};
use opto_db::RevisionId;
use opto_library::LibrarySelection;
use opto_synth::SynthesisReport;

/// Revisions and library selections captured before synthesis starts.
///
/// Publication rejects the result if any member no longer matches the live
/// session, preventing a long-running synthesis from overwriting newer intent.
pub(super) struct SynthesisInputSnapshot {
    pub(super) revision: RevisionId,
    pub(super) timing_revision: RevisionId,
    pub(super) library_revision: RevisionId,
    pub(super) mapping_libraries: LibrarySelection,
    pub(super) resolution_libraries: LibrarySelection,
}

/// Fully preflighted synthesis output awaiting atomic session publication.
///
/// The mapped object sidecar is constructed during preparation. Commit first
/// performs the remaining fallible registry reconciliation, then installs
/// artifact owners and advances the session generation through infallible
/// moves.
pub(super) struct CompilationPublication {
    current_name: String,
    revision: Option<RevisionId>,
    outputs: Vec<ArtifactSynthesisOutput>,
    current_object_index: Option<MappedObjectIndex>,
    report: SynthesisReport,
}

impl CompilationPublication {
    /// Validates captured inputs, output identities, and the current report.
    pub(super) fn prepare(
        session: &Session,
        inputs: &SynthesisInputSnapshot,
        command: &'static str,
        current_name: String,
        revision: Option<RevisionId>,
        outputs: Vec<ArtifactSynthesisOutput>,
    ) -> Result<Self, SessionError> {
        inputs.validate(session, command)?;
        if outputs.is_empty() != revision.is_none() {
            return Err(SessionError::state(format!(
                "{command}: compilation publication has inconsistent revision state"
            )));
        }

        let mut current_object_index = None;
        for output in &outputs {
            let record = session.state.designs.get(&output.name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{}' disappeared before publication",
                    output.name
                ))
            })?;
            if record.source_revision != output.source_revision
                || output.synthesis.mapped().base_revision() != inputs.revision
            {
                return Err(SessionError::state(format!(
                    "{command}: synthesis result for '{}' is stale and cannot be committed",
                    output.name
                )));
            }
            if output.synthesis.mapped().name() != output.name {
                return Err(SessionError::state(format!(
                    "{command}: synthesis result for '{}' has a mismatched mapped design name",
                    output.name
                )));
            }
            if output.name == current_name {
                current_object_index = Some(MappedObjectIndex::new(
                    output.synthesis.mapped(),
                    &session.process.runtime,
                )?);
            }
        }

        let report = outputs
            .iter()
            .find(|output| output.name == current_name)
            .map(|output| output.synthesis.report().clone())
            .or_else(|| {
                session
                    .state
                    .designs
                    .get(&current_name)
                    .and_then(|record| record.synthesized.as_ref())
                    .map(|synthesis| synthesis.report().clone())
            })
            .ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: current design '{current_name}' has no synthesized artifact"
                ))
            })?;

        Ok(Self {
            current_name,
            revision,
            outputs,
            current_object_index,
            report,
        })
    }

    /// Publishes the prepared outputs without exposing a partial generation.
    ///
    /// Registry reconciliation remains fallible and therefore precedes every
    /// artifact move. Once it succeeds, preparation guarantees that all design
    /// lookups and ownership transfers below are infallible.
    pub(super) fn commit(mut self, session: &mut Session) -> Result<(), SessionError> {
        if let Some(index) = self.current_object_index.as_ref() {
            let mapped = self
                .outputs
                .iter()
                .find(|output| output.name == self.current_name)
                .expect("mapped object sidecar belongs to the current output")
                .synthesis
                .mapped();
            transaction::reconcile_mapped_objects(session, mapped, index)?;
        }
        let publishes_artifacts = self.revision.is_some();
        if let Some(revision) = self.revision {
            for output in self.outputs {
                let is_current = output.name == self.current_name;
                let design = session
                    .state
                    .designs
                    .get_mut(&output.name)
                    .expect("publication preflight validated every output design");
                design.incremental_snapshot = None;
                design.synthesized = Some(output.synthesis);
                design.synthesis_binding = Some(ArtifactBinding {
                    content_key: output.key,
                    published_revision: revision,
                });
                design.mapped_object_index = is_current.then(|| {
                    self.current_object_index
                        .take()
                        .expect("current output owns its prepared mapped object sidecar")
                });
            }
            session.state.revision = revision;
        }
        session.state.last_synthesis = Some(self.report);
        if publishes_artifacts {
            session.clear_stale_analysis_generation();
        }
        Ok(())
    }
}

impl SynthesisInputSnapshot {
    fn validate(&self, session: &Session, command: &str) -> Result<(), SessionError> {
        if session.state.revision != self.revision
            || session.state.timing.revision() != self.timing_revision
            || session.process.libraries.current().id() != self.library_revision
            || session.mapping_library_selection() != self.mapping_libraries
            || session.resolution_library_selection() != self.resolution_libraries
        {
            return Err(SessionError::state(format!(
                "{command}: synthesis inputs changed before publication"
            )));
        }
        Ok(())
    }
}
