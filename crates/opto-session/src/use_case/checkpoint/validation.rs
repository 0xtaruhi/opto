// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::designs::ValidatedDesignStore;
use crate::{DesignRecord, DesignStore, DesignView};
use opto_db::{ObjectRegistry, ResolvedObject, RevisionId};

pub(super) fn validate_design_relationships(
    designs: &ValidatedDesignStore,
    session_revision: RevisionId,
) -> Result<(), crate::SessionError> {
    designs
        .as_store()
        .iter()
        .try_for_each(|(name, record)| validate_design_relationship(name, record, session_revision))
}

fn validate_design_relationship(
    name: &str,
    record: &DesignRecord,
    session_revision: RevisionId,
) -> Result<(), crate::SessionError> {
    if record.source_revision > session_revision
        || record
            .synthesis_binding
            .as_ref()
            .is_some_and(|binding| binding.published_revision > session_revision)
    {
        return Err(crate::SessionError::checkpoint(format!(
            "design '{name}' contains a revision newer than the saved session"
        )));
    }
    match (
        record.synthesized.as_ref(),
        record.synthesis_binding.as_ref(),
        record.incremental_snapshot.as_ref(),
    ) {
        (Some(synthesis), Some(binding), None) => {
            let snapshot = synthesis.source_snapshot();
            if snapshot.effort() != binding.content_key.effort {
                return Err(crate::SessionError::checkpoint(format!(
                    "design '{name}' synthesis artifact and binding disagree"
                )));
            }
            if synthesis.mapped().name() != name || synthesis.report().design != name {
                return Err(crate::SessionError::checkpoint(format!(
                    "design '{name}' synthesis artifact has a mismatched design identity"
                )));
            }
            let base_revision = synthesis.mapped().base_revision();
            if base_revision < record.source_revision
                || base_revision >= binding.published_revision
                || base_revision > session_revision
            {
                return Err(crate::SessionError::checkpoint(format!(
                    "design '{name}' synthesis artifact has an invalid base revision"
                )));
            }
        }
        (None, None, Some(_) | None) => {}
        _ => unreachable!("validated design-store ownership invariant was broken"),
    }
    Ok(())
}

pub(super) fn validate_checkpoint_objects(
    designs: &DesignStore,
    objects: &ObjectRegistry,
) -> Result<(), crate::SessionError> {
    let mut validation = CheckpointObjectValidation::new(objects);
    let mut pin_full_name = String::new();
    let mut net_name = String::new();

    for (_, record) in designs.iter() {
        let design = DesignView::from_record(record);
        validation.require(ResolvedObject::Design {
            name: design.name(),
        })?;
        for port in design.ports() {
            validation.require(ResolvedObject::Port {
                design: design.name(),
                name: port.name,
            })?;
        }
        if record.mapped_object_index.is_none() {
            continue;
        }

        for cell in design.cells() {
            validation.require(ResolvedObject::Cell {
                design: design.name(),
                name: cell.name,
            })?;
            if let Some(reference) = designs.get(cell.reference) {
                for port in DesignView::from_record(reference).ports() {
                    require_pin(
                        &mut validation,
                        design.name(),
                        cell.name,
                        port.name,
                        &mut pin_full_name,
                    )?;
                }
            }
            for connection in cell.connections() {
                require_pin(
                    &mut validation,
                    design.name(),
                    cell.name,
                    connection.port,
                    &mut pin_full_name,
                )?;
            }
        }
        for net in design.nets() {
            net.name.with_str(&mut net_name, |name| {
                validation.require(ResolvedObject::Net {
                    design: design.name(),
                    name,
                })
            })?;
        }
    }
    validation.finish()
}

struct CheckpointObjectValidation<'a> {
    objects: &'a ObjectRegistry,
    seen: Vec<u8>,
    expected: usize,
}

impl<'a> CheckpointObjectValidation<'a> {
    fn new(objects: &'a ObjectRegistry) -> Self {
        Self {
            objects,
            seen: vec![0; objects.marker_capacity()],
            expected: 0,
        }
    }

    fn require(&mut self, object: ResolvedObject<'_>) -> Result<(), crate::SessionError> {
        let marker = self.objects.resolved_marker(object).ok_or_else(|| {
            crate::SessionError::checkpoint(format!(
                "restored object registry is missing {:?} '{}'",
                object.class(),
                object.object_name()
            ))
        })?;
        let seen = &mut self.seen[marker.index()];
        if *seen == 0 {
            *seen = 1;
            self.expected += 1;
        }
        Ok(())
    }

    fn finish(self) -> Result<(), crate::SessionError> {
        let mut actual = 0;
        let mut extra = None;
        for (marker, object) in self.objects.live_resolved() {
            if object.design_name().is_none() {
                continue;
            }
            actual += 1;
            if self.seen[marker.index()] == 0 && extra.is_none() {
                extra = Some(object);
            }
        }
        if actual == self.expected && extra.is_none() {
            return Ok(());
        }
        Err(crate::SessionError::checkpoint(match extra {
            Some(object) => format!(
                "restored object registry contains stale {:?} '{}'",
                object.class(),
                object.object_name()
            ),
            None => "restored object registry disagrees with design indexes".to_string(),
        }))
    }
}

fn require_pin(
    validation: &mut CheckpointObjectValidation<'_>,
    design: &str,
    cell: &str,
    name: &str,
    full_name: &mut String,
) -> Result<(), crate::SessionError> {
    full_name.clear();
    full_name.push_str(cell);
    full_name.push('/');
    full_name.push_str(name);
    validation.require(ResolvedObject::Pin {
        design,
        cell,
        name,
        full_name,
    })
}
