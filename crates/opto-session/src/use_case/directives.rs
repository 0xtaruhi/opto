// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Session, SessionError};
use opto_db::ObjectLocator;
use opto_ir::rtl::RtlModule;
use opto_ir::word::{AnnotationTarget, InstId, SignalId, SourceSpan, SynthesisDirectiveKind};
use std::collections::BTreeMap;

fn directive_target(
    command: &str,
    source: &RtlModule,
    locator: &ObjectLocator,
    kind: SynthesisDirectiveKind,
) -> Result<AnnotationTarget, SessionError> {
    let word = source.word();
    match locator {
        ObjectLocator::Design { name }
            if matches!(
                kind,
                SynthesisDirectiveKind::DontTouch | SynthesisDirectiveKind::Ungroup
            ) && name == word.name() =>
        {
            Ok(AnnotationTarget::Module)
        }
        ObjectLocator::Cell { name, .. }
            if matches!(
                kind,
                SynthesisDirectiveKind::DontTouch | SynthesisDirectiveKind::Ungroup
            ) =>
        {
            let instance = word
                .instances()
                .iter()
                .enumerate()
                .find(|(_, instance)| word.name_str(instance.name) == name)
                .map(|(index, _)| InstId::from_index(index))
                .transpose()?
                .ok_or_else(|| {
                    SessionError::object(format!(
                        "{command}: cell '{name}' is not a source design instance"
                    ))
                })?;
            Ok(AnnotationTarget::Instance(instance))
        }
        ObjectLocator::Net { name, .. } if kind == SynthesisDirectiveKind::DontTouch => {
            let signal = word
                .signals()
                .iter()
                .enumerate()
                .find(|(_, signal)| signal.name.is_some_and(|id| word.name_str(id) == name))
                .map(|(index, _)| SignalId::from_index(index))
                .transpose()?
                .ok_or_else(|| {
                    SessionError::object(format!(
                        "{command}: net '{name}' is not a named source signal"
                    ))
                })?;
            Ok(AnnotationTarget::Signal(signal))
        }
        _ => Err(SessionError::Command(format!(
            "{command}: {} objects do not support this directive",
            object_class_name(locator)
        ))),
    }
}

fn object_class_name(locator: &ObjectLocator) -> &'static str {
    match locator {
        ObjectLocator::Design { .. } => "design",
        ObjectLocator::Port { .. } => "port",
        ObjectLocator::Cell { .. } => "cell",
        ObjectLocator::Pin { .. } => "pin",
        ObjectLocator::Net { .. } => "net",
        ObjectLocator::Clock { .. } => "clock",
    }
}
impl Session {
    /// Apply one typed optimization directive to a collection of source objects.
    pub fn set_synthesis_directive(
        &mut self,
        command: &'static str,
        objects: &str,
        kind: SynthesisDirectiveKind,
        enabled: bool,
    ) -> Result<usize, SessionError> {
        let locators = self.collection_objects(objects)?;
        let mut updates = BTreeMap::<String, Vec<AnnotationTarget>>::new();

        for locator in &locators {
            let design_name = locator.design_name().ok_or_else(|| {
                SessionError::Command(format!(
                    "{command}: {} objects do not support this directive",
                    object_class_name(locator)
                ))
            })?;
            let record = self.state.designs.get(design_name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{design_name}' is missing from the design store"
                ))
            })?;
            if record.mapped_object_index.is_some() {
                return Err(SessionError::Command(format!(
                    "{command}: cannot change source directives on synthesized design '{design_name}'"
                )));
            }
            let target = directive_target(command, &record.source, locator, kind)?;
            let targets = updates.entry(design_name.to_string()).or_default();
            if !targets.contains(&target) {
                targets.push(target);
            }
        }

        let mut changed = BTreeMap::new();
        let mut changed_targets = 0usize;
        for (design_name, targets) in updates {
            let record = self.state.designs.get(&design_name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{design_name}' disappeared before commit"
                ))
            })?;
            let mut source = record.source.clone();
            let mut design_changed = false;
            for target in targets {
                if source.word().synthesis_directive(target, kind) != Some(enabled) {
                    source.set_synthesis_directive(target, kind, enabled, SourceSpan::default())?;
                    design_changed = true;
                    changed_targets += 1;
                }
            }
            if design_changed {
                source.validate()?;
                changed.insert(design_name, source);
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }

        let next_revision = self.next_revision()?;
        let mut detachments = changed
            .keys()
            .map(|name| {
                let record = self.state.designs.get(name).ok_or_else(|| {
                    SessionError::state(format!(
                        "{command}: design '{name}' disappeared before detach"
                    ))
                })?;
                Ok((name.clone(), record.prepare_synthesis_detach()?))
            })
            .collect::<Result<BTreeMap<_, _>, SessionError>>()?;

        for (name, source) in changed {
            let record = self.state.designs.get_mut(&name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{name}' disappeared during commit"
                ))
            })?;
            record.commit_synthesis_detach(detachments.remove(&name).ok_or_else(|| {
                SessionError::state(format!(
                    "{command}: design '{name}' has no prepared detachment"
                ))
            })?);
            record.source = source;
            record.source_revision = next_revision;
        }
        self.state.last_synthesis = None;
        self.state.revision = next_revision;
        self.clear_stale_analysis_generation();
        debug_assert!(detachments.is_empty());
        Ok(changed_targets)
    }
}
