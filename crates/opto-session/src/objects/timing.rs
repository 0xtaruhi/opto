// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{collection::is_collection_value, validate_design_rule_object_class};
use crate::{Session, TimingObject};
use opto_db::{AnyObjectId, Direction, ResolvedObject};
use opto_timing::TimingPortDirection;

impl Session {
    /// Resolve untyped names to design, port, or clock rule targets.
    pub fn resolve_design_rule_objects(
        &mut self,
        command: &str,
        values: &[String],
    ) -> Result<Vec<TimingObject>, crate::SessionError> {
        values
            .iter()
            .map(|value| self.resolve_design_rule_object(command, value))
            .collect()
    }

    /// Decode a collection value as valid design-rule targets.
    pub fn design_rule_objects_if_handle(
        &self,
        command: &str,
        value: &str,
    ) -> Result<Option<Vec<TimingObject>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        let objects = self
            .collection_ids(value)?
            .map(|uid| {
                let locator = self.resolve_object(uid)?;
                self.design_rule_object(command, uid, locator)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(objects))
    }

    pub(in crate::objects) fn resolve_design_rule_object(
        &self,
        command: &str,
        name: &str,
    ) -> Result<TimingObject, crate::SessionError> {
        let mut matched = None;
        let mut ambiguous = false;
        let mut consider = |object| {
            let Some(candidate) = self.state.objects.get_resolved(object) else {
                return;
            };
            match matched {
                None => matched = Some(candidate),
                Some(previous) if previous == candidate => {}
                Some(_) => ambiguous = true,
            }
        };
        consider(ResolvedObject::Design { name });
        if let Some(design) = self.current_design() {
            consider(ResolvedObject::Port { design, name });
        }
        consider(ResolvedObject::Clock { name });

        if ambiguous {
            return Err(crate::SessionError::state(format!(
                "{command}: object name '{name}' is ambiguous; use a typed collection"
            )));
        }
        let uid = matched.ok_or_else(|| {
            crate::SessionError::state(format!("{command}: object '{name}' not found"))
        })?;
        self.design_rule_object(command, uid, self.resolve_object(uid)?)
    }

    pub(in crate::objects) fn design_rule_object(
        &self,
        command: &str,
        id: AnyObjectId,
        locator: ResolvedObject<'_>,
    ) -> Result<TimingObject, crate::SessionError> {
        let object = match (id, locator) {
            (AnyObjectId::Design(id), ResolvedObject::Design { .. }) => TimingObject::design(id),
            (AnyObjectId::Port(id), ResolvedObject::Port { design, name }) => {
                let index = self.design_by_name(design)?;
                let port = index.port_by_name(name).ok_or_else(|| {
                    crate::SessionError::state(format!("{command}: port '{name}' no longer exists"))
                })?;
                let direction = match port.direction {
                    Direction::Input => TimingPortDirection::Input,
                    Direction::Output => TimingPortDirection::Output,
                    Direction::Inout | Direction::Ref => TimingPortDirection::Inout,
                };
                let design_uid = self
                    .state
                    .objects
                    .get_resolved(ResolvedObject::Design { name: design })
                    .and_then(AnyObjectId::downcast)
                    .ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "{command}: design '{design}' has no object identity"
                        ))
                    })?;
                TimingObject::port(id, design_uid, direction)
            }
            (AnyObjectId::Clock(id), ResolvedObject::Clock { .. }) => TimingObject::clock(id),
            _ => {
                return Err(crate::SessionError::state(format!(
                    "{command}: object '{}' has unsupported class",
                    locator.object_name()
                )));
            }
        };
        validate_design_rule_object_class(command, object.kind())?;
        Ok(object)
    }
}
