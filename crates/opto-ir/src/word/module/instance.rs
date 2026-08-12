// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    InstId, Instance, InstanceConnection, MemoryId, NameId, OpKind, PackedInstanceSpec,
    SignalFragment, SignalId, SourceSpan, ValueId, ValueKind, WordError, WordModule, dense_id,
    insert_dense_id,
};
#[cfg(test)]
use super::{PortDirection, WordType};

impl WordModule {
    /// Adds an instance whose connection names are owned strings.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for empty or duplicate names, duplicate port
    /// bindings, foreign values, name-table failure, or arena capacity failure.
    pub fn add_instance(
        &mut self,
        name: impl AsRef<str>,
        module: impl AsRef<str>,
        connections: Vec<(String, ValueId, SourceSpan)>,
        source: SourceSpan,
    ) -> Result<InstId, WordError> {
        self.add_instance_parts(name, module, connections, source)
    }

    /// Appends an ordered instance range after one complete semantic and
    /// capacity preflight. Returned IDs are dense and align with `instances`.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] before structural mutation for capacity overflow,
    /// empty or duplicate names, duplicate port bindings, unknown values, or
    /// name-table capacity exhaustion.
    pub fn add_instances_packed(
        &mut self,
        instances: Vec<PackedInstanceSpec>,
    ) -> Result<Box<[InstId]>, WordError> {
        let first_count = self.instances.len();
        let final_count = first_count
            .checked_add(instances.len())
            .ok_or_else(|| WordError::new("instance arena exceeds host capacity"))?;
        if final_count != 0 {
            let _ = InstId::from_index(final_count - 1)?;
        }
        let mut names = std::collections::BTreeSet::new();
        for instance in &instances {
            if instance.name.trim().is_empty() {
                return Err(WordError::new("RTL instance name cannot be empty"));
            }
            if instance.module.trim().is_empty() {
                return Err(WordError::new(format!(
                    "RTL instance '{}' has empty module reference",
                    instance.name
                )));
            }
            if self.instance_id(&instance.name).is_some() || !names.insert(instance.name.as_str()) {
                return Err(WordError::new(format!(
                    "duplicate RTL instance name '{}'",
                    instance.name
                )));
            }
            let mut ports = std::collections::BTreeSet::new();
            for (port, value, _) in &instance.connections {
                if port.trim().is_empty() {
                    return Err(WordError::new(format!(
                        "RTL instance '{}' has empty connection port",
                        instance.name
                    )));
                }
                if !ports.insert(port.as_str()) {
                    return Err(WordError::new(format!(
                        "RTL instance '{}' has duplicate connection port '{}'",
                        instance.name, port
                    )));
                }
                self.value_ty(*value)?;
            }
        }
        let name_checkpoint = self.names.checkpoint();
        let prepared = instances
            .into_iter()
            .map(|instance| {
                let name = self.names.intern(&instance.name)?;
                let module = self.names.intern(&instance.module)?;
                let connections = instance
                    .connections
                    .into_iter()
                    .map(|(port, value, source)| {
                        Ok(InstanceConnection {
                            port: self.names.intern(&port)?,
                            value,
                            source,
                        })
                    })
                    .collect::<Result<Vec<_>, WordError>>()?;
                Ok(Instance {
                    name,
                    module,
                    connections,
                    source: instance.source,
                })
            })
            .collect::<Result<Vec<_>, WordError>>();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.names.rollback(name_checkpoint)?;
                return Err(error);
            }
        };
        let required_name_slots = prepared
            .iter()
            .map(|instance| instance.name.raw() as usize + 1)
            .max()
            .unwrap_or(0);
        if self.named_instances.len() < required_name_slots {
            self.named_instances.resize(required_name_slots, None);
        }
        debug_assert!(
            prepared
                .iter()
                .all(|instance| { self.named_instances[instance.name.raw() as usize].is_none() })
        );

        self.instances.reserve(prepared.len());
        let mut ids = Vec::with_capacity(prepared.len());
        for (offset, instance) in prepared.into_iter().enumerate() {
            let id = InstId::from_index(self.instances.len())?;
            self.named_instances[instance.name.raw() as usize] = Some(id);
            self.instances.push(instance);
            debug_assert_eq!(id.index(), first_count + offset);
            ids.push(id);
        }
        debug_assert_eq!(self.instances.len(), final_count);
        Ok(ids.into_boxed_slice())
    }

    fn add_instance_parts<N, M, P>(
        &mut self,
        name: N,
        module: M,
        connections: Vec<(P, ValueId, SourceSpan)>,
        source: SourceSpan,
    ) -> Result<InstId, WordError>
    where
        N: AsRef<str>,
        M: AsRef<str>,
        P: AsRef<str>,
    {
        let name = name.as_ref();
        let module = module.as_ref();
        if name.trim().is_empty() {
            return Err(WordError::new("RTL instance name cannot be empty"));
        }
        if module.trim().is_empty() {
            return Err(WordError::new(format!(
                "RTL instance '{name}' has empty module reference"
            )));
        }
        let name_id = self.names.intern(name)?;
        if dense_id(&self.named_instances, name_id).is_some() {
            return Err(WordError::new(format!(
                "duplicate RTL instance name '{name}'"
            )));
        }
        let module_id = self.names.intern(module)?;
        let mut ports = Vec::<NameId>::with_capacity(connections.len());
        let mut interned_connections = Vec::with_capacity(connections.len());
        for (port, value, source) in connections {
            let port = port.as_ref();
            if port.trim().is_empty() {
                return Err(WordError::new(format!(
                    "RTL instance '{name}' has empty connection port"
                )));
            }
            let port_id = self.names.intern(port)?;
            if ports.contains(&port_id) {
                return Err(WordError::new(format!(
                    "RTL instance '{name}' has duplicate connection port '{port}'"
                )));
            }
            ports.push(port_id);
            self.value_ty(value)?;
            interned_connections.push(InstanceConnection {
                port: port_id,
                value,
                source,
            });
        }
        let id = InstId::from_index(self.instances.len())?;
        self.instances.push(Instance {
            name: name_id,
            module: module_id,
            connections: interned_connections,
            source,
        });
        insert_dense_id(&mut self.named_instances, name_id, id)?;
        Ok(id)
    }

    /// Looks up a signal by exact interned name.
    #[must_use]
    pub fn signal_id(&self, name: &str) -> Option<SignalId> {
        self.names
            .get(name)
            .and_then(|name| dense_id(&self.named_signals, name))
    }

    /// Looks up a memory by exact interned name.
    #[must_use]
    pub fn memory_id(&self, name: &str) -> Option<MemoryId> {
        self.names
            .get(name)
            .and_then(|name| dense_id(&self.named_memories, name))
    }

    /// Looks up an instance by exact interned name.
    #[must_use]
    pub fn instance_id(&self, name: &str) -> Option<InstId> {
        self.names
            .get(name)
            .and_then(|name| dense_id(&self.named_instances, name))
    }

    /// Retargets an instance to another definition name.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the reference is empty, cannot be interned,
    /// or `instance` is foreign.
    pub fn set_instance_module(
        &mut self,
        instance: InstId,
        module: impl AsRef<str>,
    ) -> Result<(), WordError> {
        let module = module.as_ref();
        if module.trim().is_empty() {
            return Err(WordError::new("RTL instance reference cannot be empty"));
        }
        let module = self.names.intern(module)?;
        let instance = self
            .instances
            .get_mut(instance.index())
            .ok_or_else(|| WordError::new(format!("unknown RTL instance {instance:?}")))?;
        instance.module = module;
        Ok(())
    }

    /// Replaces one named instance connection and returns its previous value.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the value or instance ID is foreign, the port
    /// name is unknown, or that port is not connected.
    pub fn set_instance_connection_value(
        &mut self,
        instance: InstId,
        port: &str,
        value: ValueId,
    ) -> Result<ValueId, WordError> {
        self.value_ty(value)?;
        let port = self
            .names
            .get(port)
            .ok_or_else(|| WordError::new(format!("unknown instance port '{port}'")))?;
        let instance = self
            .instances
            .get_mut(instance.index())
            .ok_or_else(|| WordError::new(format!("unknown RTL instance {instance:?}")))?;
        let connection = instance
            .connections
            .iter_mut()
            .find(|connection| connection.port == port)
            .ok_or_else(|| WordError::new("instance port is not connected"))?;
        Ok(std::mem::replace(&mut connection.value, value))
    }

    /// Decomposes an instance output connection into signal references in
    /// least-significant-first order. Only signals and concatenations of
    /// signals are valid structural output targets.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if `value` is unknown or contains anything other
    /// than signal references and concatenations of signal references.
    pub fn signal_fragments(&self, value: ValueId) -> Result<Vec<SignalFragment>, WordError> {
        fn collect(
            module: &WordModule,
            value: ValueId,
            fragments: &mut Vec<SignalFragment>,
        ) -> Result<(), WordError> {
            let stored = module
                .value(value)
                .ok_or_else(|| WordError::new(format!("unknown structural value {value:?}")))?;
            match stored.kind {
                ValueKind::Signal(reference) => {
                    fragments.push(SignalFragment {
                        reference,
                        ty: stored.ty,
                    });
                    Ok(())
                }
                ValueKind::Operation(operation) => {
                    let operation = module.operation(operation).ok_or_else(|| {
                        WordError::new(format!("unknown structural operation {operation:?}"))
                    })?;
                    let OpKind::Concat { parts } = &operation.kind else {
                        return Err(WordError::new(
                            "structural connection must be a signal or signal concatenation",
                        ));
                    };
                    for &part in parts.iter().rev() {
                        collect(module, part, fragments)?;
                    }
                    Ok(())
                }
                ValueKind::Constant(_) => Err(WordError::new(
                    "structural connection cannot target a constant",
                )),
            }
        }

        let mut fragments = Vec::new();
        collect(self, value, &mut fragments)?;
        Ok(fragments)
    }
}

#[cfg(test)]
mod duplicate_name_tests {
    use super::*;

    #[test]
    fn rejects_duplicate_rtl_instance_names_before_mutation() {
        let mut module = WordModule::new("top");
        module
            .add_instance(
                "u0",
                "child",
                Vec::<(String, ValueId, SourceSpan)>::new(),
                SourceSpan::default(),
            )
            .unwrap();

        let error = module
            .add_instance(
                "u0",
                "child",
                Vec::<(String, ValueId, SourceSpan)>::new(),
                SourceSpan::default(),
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("duplicate RTL instance name 'u0'")
        );
        assert_eq!(module.instances().len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_instances_receive_dense_ids_after_one_preflight() {
        let mut module = WordModule::new("top");
        let source = SourceSpan::default();
        let input = module
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                source.clone(),
            )
            .unwrap();
        let value = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        let ids = module
            .add_instances_packed(vec![
                PackedInstanceSpec {
                    name: "U0".to_string(),
                    module: "BUF".to_string(),
                    connections: vec![("A".to_string(), value, source.clone())],
                    source: source.clone(),
                },
                PackedInstanceSpec {
                    name: "U1".to_string(),
                    module: "BUF".to_string(),
                    connections: vec![("A".to_string(), value, source.clone())],
                    source,
                },
            ])
            .unwrap();

        assert_eq!(ids[0].index(), 0);
        assert_eq!(ids[1].index(), 1);
        assert_eq!(module.instance_id("U0"), Some(ids[0]));
        assert_eq!(module.instance_id("U1"), Some(ids[1]));
    }
}
