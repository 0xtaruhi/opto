// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{CollectionFilter, FilterOperator, ObjectHandleCodec, Session};
use opto_db::{
    AnyObjectId, ClockObject, Collection, ObjectKind, ObjectLocator, PortId, PortObject,
    ResolvedObject, matches_pattern,
};
use std::collections::BTreeSet;

pub(in crate::objects) enum CollectionIds {
    Empty(std::iter::Empty<AnyObjectId>),
    Member(std::array::IntoIter<AnyObjectId, 1>),
    Members(std::vec::IntoIter<AnyObjectId>),
}

impl CollectionIds {
    fn into_vec(self) -> Vec<AnyObjectId> {
        self.collect()
    }
}

impl Iterator for CollectionIds {
    type Item = AnyObjectId;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty(objects) => objects.next(),
            Self::Member(object) => object.next(),
            Self::Members(objects) => objects.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Empty(objects) => objects.size_hint(),
            Self::Member(object) => object.size_hint(),
            Self::Members(objects) => objects.size_hint(),
        }
    }
}

impl ExactSizeIterator for CollectionIds {}

impl Session {
    /// Convert a typed collection into an ordinary Tcl-list payload of stable handles.
    pub fn collection_handles<T: ObjectKind>(&self, collection: Collection<T>) -> Vec<String> {
        collection
            .into_objects()
            .into_iter()
            .map(opto_db::ObjectId::erase)
            .map(|object| self.process.handles.member_handle(object))
            .collect()
    }

    /// Filter a typed collection and return stable handles in source order.
    pub fn collection_handles_filtered<T: ObjectKind>(
        &self,
        collection: Collection<T>,
        filter: Option<&CollectionFilter>,
    ) -> Vec<String> {
        collection
            .into_objects()
            .into_iter()
            .map(opto_db::ObjectId::erase)
            .filter(|object| {
                filter.is_none_or(|filter| self.object_matches_filter(*object, filter))
            })
            .map(|object| self.process.handles.member_handle(object))
            .collect()
    }

    /// Store a singleton design collection for the current design.
    pub fn store_current_design_collection(&mut self) -> Result<String, crate::SessionError> {
        let name = self.current_design_name()?;
        let uid = self
            .state
            .objects
            .get_resolved(ResolvedObject::Design { name })
            .ok_or_else(|| {
                crate::SessionError::state(format!("design '{name}' has no object identity"))
            })?;
        Ok(self.process.handles.member_handle(uid))
    }

    /// Resolve a collection or member handle to durable object IDs.
    pub fn collection_members(
        &self,
        handle: &str,
    ) -> Result<Vec<AnyObjectId>, crate::SessionError> {
        Ok(self.collection_ids(handle)?.into_vec())
    }

    /// Format the lightweight singleton handle for one durable object.
    pub fn collection_member_handle(&self, object: AnyObjectId) -> String {
        self.process.handles.member_handle(object)
    }

    /// Return the number of objects referenced by a collection value.
    pub fn collection_len(&self, handle: &str) -> Result<usize, crate::SessionError> {
        Ok(self.collection_ids(handle)?.len())
    }

    /// Resolve collection members to user-visible object names in handle order.
    pub fn collection_object_names(
        &self,
        handle: &str,
    ) -> Result<Vec<String>, crate::SessionError> {
        self.collection_ids(handle)?
            .map(|uid| Ok(self.resolve_object(uid)?.object_name().to_string()))
            .collect()
    }

    /// Query one attribute for every collection member in handle order.
    pub fn collection_attribute_values(
        &self,
        handle: &str,
        attribute: &str,
    ) -> Result<Vec<String>, crate::SessionError> {
        self.collection_ids(handle)?
            .map(|id| self.object_attribute_id(id, attribute))
            .collect()
    }

    /// Return the first object name if `value` is a collection value.
    pub fn collection_first_object_name(
        &self,
        value: &str,
    ) -> Result<Option<String>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        let mut objects = self.collection_ids(value)?;
        let Some(uid) = objects.next() else {
            return Ok(None);
        };
        Ok(Some(self.resolve_object(uid)?.object_name().to_string()))
    }

    /// Resolve all object names if `value` is a collection value.
    pub fn collection_object_names_if_handle(
        &self,
        value: &str,
    ) -> Result<Option<Vec<String>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        self.collection_object_names(value).map(Some)
    }

    /// Decode a collection value as ports, rejecting mixed object classes.
    pub fn port_ids_if_handle(
        &self,
        command: &str,
        value: &str,
    ) -> Result<Option<Vec<PortId>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        self.collection_ids(value)?
            .map(|id| {
                id.downcast::<PortObject>().ok_or_else(|| {
                    crate::SessionError::state(format!(
                        "{command}: object class '{:?}' is not valid; expected port",
                        id.class()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Resolve exact port names in the current design to durable IDs.
    pub fn resolve_port_ids(
        &mut self,
        command: &str,
        names: &[String],
    ) -> Result<Vec<PortId>, crate::SessionError> {
        let design = self.current_design_name()?.to_string();
        let index = self.design_by_name(&design)?;
        if let Some(name) = names.iter().find(|name| index.port_by_name(name).is_none()) {
            return Err(crate::SessionError::state(format!(
                "{command}: port '{name}' not found"
            )));
        }
        names
            .iter()
            .map(|name| {
                self.state
                    .objects
                    .intern_resolved(ResolvedObject::Port {
                        design: &design,
                        name,
                    })
                    .map_err(crate::SessionError::Registry)?
                    .downcast::<PortObject>()
                    .ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "{command}: object '{name}' is not a port"
                        ))
                    })
            })
            .collect()
    }

    /// Resolve a typed port ID to its current user-visible name.
    pub fn port_name(&self, id: PortId) -> Result<String, crate::SessionError> {
        let locator = self.resolve_object(id.erase())?;
        match locator {
            ResolvedObject::Port { name, .. } => Ok(name.to_string()),
            _ => Err(crate::SessionError::state(format!(
                "typed port ID {id:?} resolved to the wrong object class"
            ))),
        }
    }

    /// Decode a collection value as clocks, rejecting mixed object classes.
    pub fn clock_ids_if_handle(
        &self,
        command: &str,
        value: &str,
    ) -> Result<Option<Vec<opto_db::ClockId>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        self.collection_ids(value)?
            .map(|id| {
                id.downcast::<ClockObject>().ok_or_else(|| {
                    crate::SessionError::state(format!(
                        "{command}: object class '{:?}' is not valid; expected clock",
                        id.class()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Resolve exact clock names to durable IDs.
    pub fn resolve_clock_ids(
        &self,
        command: &str,
        names: &[String],
    ) -> Result<Vec<opto_db::ClockId>, crate::SessionError> {
        names
            .iter()
            .map(|name| {
                self.state
                    .objects
                    .get_resolved(ResolvedObject::Clock { name })
                    .and_then(AnyObjectId::downcast)
                    .ok_or_else(|| {
                        crate::SessionError::state(format!("{command}: clock '{name}' not found"))
                    })
            })
            .collect()
    }

    /// Decode a collection value as typed path-exception points.
    pub fn timing_endpoints_if_handle(
        &self,
        command: &str,
        value: &str,
    ) -> Result<Option<Vec<opto_timing::TimingEndpoint>>, crate::SessionError> {
        if !is_collection_value(value) {
            return Ok(None);
        }
        self.collection_ids(value)?
            .map(|id| match id {
                AnyObjectId::Port(id) => Ok(opto_timing::TimingEndpoint::Port(id)),
                AnyObjectId::Cell(id) => Ok(opto_timing::TimingEndpoint::Cell(id)),
                AnyObjectId::Pin(id) => Ok(opto_timing::TimingEndpoint::Pin(id)),
                AnyObjectId::Net(id) => Ok(opto_timing::TimingEndpoint::Net(id)),
                AnyObjectId::Clock(id) => Ok(opto_timing::TimingEndpoint::Clock(id)),
                AnyObjectId::Design(_) => Err(crate::SessionError::state(format!(
                    "{command}: object class '{:?}' is not valid for a timing endpoint",
                    id.class()
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Resolve untyped names to unambiguous path-exception points.
    pub fn resolve_timing_endpoints(
        &self,
        command: &str,
        names: &[String],
    ) -> Result<Vec<opto_timing::TimingEndpoint>, crate::SessionError> {
        let current = self.current_design();
        names
            .iter()
            .map(|name| {
                let mut matched = None;
                let mut ambiguous = false;
                let mut consider = |locator| {
                    let Some(candidate) = self
                        .state
                        .objects
                        .get_resolved(locator)
                        .and_then(timing_endpoint)
                    else {
                        return;
                    };
                    match matched {
                        None => matched = Some(candidate),
                        Some(previous) if previous == candidate => {}
                        Some(_) => ambiguous = true,
                    }
                };

                if let Some(design) = current {
                    consider(ResolvedObject::Port { design, name });
                    consider(ResolvedObject::Cell { design, name });
                    consider(ResolvedObject::Net { design, name });
                    for (separator, _) in name.match_indices('/') {
                        consider(ResolvedObject::Pin {
                            design,
                            cell: &name[..separator],
                            name: &name[separator + 1..],
                            full_name: name,
                        });
                    }
                }
                consider(ResolvedObject::Clock { name });

                if ambiguous {
                    Err(crate::SessionError::state(format!(
                        "{command}: timing endpoint '{name}' is ambiguous; use a typed collection"
                    )))
                } else {
                    matched.ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "{command}: timing endpoint '{name}' not found"
                        ))
                    })
                }
            })
            .collect()
    }

    pub(in crate::objects) fn intern_collection<T: ObjectKind>(
        &mut self,
        objects: Vec<ObjectLocator>,
    ) -> Result<Collection<T>, crate::SessionError> {
        let mut ids = Vec::with_capacity(objects.len());
        let mut seen = BTreeSet::new();
        for object in objects {
            let id = self
                .state
                .objects
                .intern(object)
                .map_err(crate::SessionError::Registry)?;
            let typed = id.downcast::<T>().ok_or_else(|| {
                crate::SessionError::state(format!(
                    "object registry returned {:?} while building a {:?} collection",
                    id.class(),
                    T::CLASS
                ))
            })?;
            if seen.insert(typed) {
                ids.push(typed);
            }
        }
        Ok(Collection::new(ids))
    }

    pub(in crate::objects) fn collection_ids(
        &self,
        handle: &str,
    ) -> Result<CollectionIds, crate::SessionError> {
        if handle.is_empty() {
            return Ok(CollectionIds::Empty(std::iter::empty()));
        }
        if let Some(object) = self.process.handles.member_id(handle) {
            let Some(locator) = self.state.objects.resolve(object) else {
                return Err(crate::SessionError::state(format!(
                    "collection references removed {object:?}"
                )));
            };
            if locator.class() != object.class() {
                return Err(crate::SessionError::state(format!(
                    "invalid collection member handle '{handle}'"
                )));
            }
            return Ok(CollectionIds::Member([object].into_iter()));
        }
        let words = handle.split_ascii_whitespace().collect::<Vec<_>>();
        if words.len() > 1 {
            let objects = words
                .into_iter()
                .map(|word| {
                    self.process.handles.member_id(word).ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "invalid database object handle '{word}'"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            for object in &objects {
                if self.state.objects.resolve(*object).is_none() {
                    return Err(crate::SessionError::state(format!(
                        "database handle references removed {object:?}"
                    )));
                }
            }
            return Ok(CollectionIds::Members(objects.into_iter()));
        }
        Err(crate::SessionError::state(format!(
            "invalid database object handle '{handle}'"
        )))
    }

    pub(crate) fn collection_objects(
        &self,
        handle: &str,
    ) -> Result<Vec<ObjectLocator>, crate::SessionError> {
        self.collection_ids(handle)?
            .map(|uid| Ok(self.resolve_object(uid)?.to_locator()))
            .collect()
    }

    pub(in crate::objects) fn resolve_object(
        &self,
        id: AnyObjectId,
    ) -> Result<ResolvedObject<'_>, crate::SessionError> {
        self.state.objects.resolve(id).ok_or_else(|| {
            crate::SessionError::state(format!("collection references removed {id:?}"))
        })
    }

    pub(in crate::objects) fn object_matches_filter(
        &self,
        id: AnyObjectId,
        filter: &CollectionFilter,
    ) -> bool {
        self.object_attribute_id(id, &filter.attribute)
            .is_ok_and(|value| match filter.operator {
                FilterOperator::Eq => value == filter.value,
                FilterOperator::Ne => value != filter.value,
                FilterOperator::Glob => matches_pattern(&value, &filter.value),
                FilterOperator::NotGlob => !matches_pattern(&value, &filter.value),
            })
    }

    pub(in crate::objects) fn object_attribute_id(
        &self,
        id: AnyObjectId,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        self.object_attribute(self.resolve_object(id)?, attribute)
    }
}

fn timing_endpoint(id: AnyObjectId) -> Option<opto_timing::TimingEndpoint> {
    match id {
        AnyObjectId::Port(id) => Some(opto_timing::TimingEndpoint::Port(id)),
        AnyObjectId::Cell(id) => Some(opto_timing::TimingEndpoint::Cell(id)),
        AnyObjectId::Pin(id) => Some(opto_timing::TimingEndpoint::Pin(id)),
        AnyObjectId::Net(id) => Some(opto_timing::TimingEndpoint::Net(id)),
        AnyObjectId::Clock(id) => Some(opto_timing::TimingEndpoint::Clock(id)),
        AnyObjectId::Design(_) => None,
    }
}

pub(in crate::objects) fn is_collection_value(value: &str) -> bool {
    value.is_empty()
        || ObjectHandleCodec::is_handle(value)
        || value
            .split_ascii_whitespace()
            .all(ObjectHandleCodec::is_handle)
}
