// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::collection::is_collection_value;
use crate::Session;
use opto_db::{AnyObjectId, ResolvedObject};

impl Session {
    /// Decode a collection value as annotatable port or net objects.
    pub fn power_objects_if_handle(
        &self,
        command: &str,
        value: &str,
    ) -> Result<Option<Vec<AnyObjectId>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        self.collection_ids(value)?
            .map(|id| {
                validate_power_object(command, id)?;
                Ok(id)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Resolve exact, unambiguous current-design port or net names.
    ///
    /// Occurrence-specific hierarchy names are rejected because switching
    /// annotations currently belong to root-design durable objects.
    pub fn resolve_power_objects(
        &mut self,
        command: &str,
        names: &[String],
    ) -> Result<Vec<AnyObjectId>, crate::SessionError> {
        let design = self.current_design_name()?.to_string();
        let index = self.design_by_name(&design)?;
        let mut objects = Vec::with_capacity(names.len());
        for name in names {
            if name.contains('/') {
                return Err(crate::SessionError::object(format!(
                    "{command}: occurrence-specific activity annotation is not supported by the current Tcl object model; annotate a root port or net"
                )));
            }
            let port = index.port_by_name(name).is_some();
            let net = index.is_visible_net_name(name);
            let object = match (port, net) {
                (true, false) => ResolvedObject::Port {
                    design: &design,
                    name,
                },
                (false, true) => ResolvedObject::Net {
                    design: &design,
                    name,
                },
                (false, false) => {
                    return Err(crate::SessionError::object(format!(
                        "{command}: port or net '{name}' not found"
                    )));
                }
                (true, true) => {
                    return Err(crate::SessionError::object(format!(
                        "{command}: object name '{name}' is ambiguous; use get_ports or get_nets"
                    )));
                }
            };
            objects.push(object);
        }
        objects
            .into_iter()
            .map(|object| {
                let id = self
                    .state
                    .objects
                    .intern_resolved(object)
                    .map_err(crate::SessionError::Registry)?;
                validate_power_object(command, id)?;
                Ok(id)
            })
            .collect()
    }
}

fn validate_power_object(command: &str, id: AnyObjectId) -> Result<(), crate::SessionError> {
    if matches!(id, AnyObjectId::Port(_) | AnyObjectId::Net(_)) {
        Ok(())
    } else {
        Err(crate::SessionError::object(format!(
            "{command}: object class '{:?}' is not implemented; expected port or net",
            id.class()
        )))
    }
}
