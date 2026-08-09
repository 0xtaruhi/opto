// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::ObjectUid;
use opto_db::{AnyObjectId, CellId, ClockId, DesignId, NetId, PinId, PortId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Comparison operator in a database-object filter expression.
pub enum FilterOperator {
    /// Exact string equality.
    Eq,
    /// Exact string inequality.
    Ne,
    /// Shell-style pattern match.
    Glob,
    /// Negated shell-style pattern match.
    NotGlob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed single-attribute database-object predicate.
pub struct CollectionFilter {
    /// Object attribute to query.
    pub attribute: String,
    /// Comparison applied to the attribute value.
    pub operator: FilterOperator,
    /// Literal or glob pattern on the right-hand side.
    pub value: String,
}

/// Generates lightweight process-local handles for durable database objects.
#[derive(Debug, Clone)]
pub(crate) struct ObjectHandleCodec {
    generation: u64,
}

impl Default for ObjectHandleCodec {
    fn default() -> Self {
        Self { generation: 1 }
    }
}

impl ObjectHandleCodec {
    pub(crate) fn is_handle(text: &str) -> bool {
        Self::parse_handle(text).is_some()
    }

    pub(crate) fn member_handle(&self, object: AnyObjectId) -> String {
        let class = match object {
            AnyObjectId::Design(_) => 'd',
            AnyObjectId::Port(_) => 'p',
            AnyObjectId::Cell(_) => 'c',
            AnyObjectId::Pin(_) => 'i',
            AnyObjectId::Net(_) => 'n',
            AnyObjectId::Clock(_) => 'k',
        };
        format!("_obj{}_{class}{}", self.generation, object.uid().get())
    }

    pub(crate) fn member_id(&self, handle: &str) -> Option<AnyObjectId> {
        let (generation, object) = Self::parse_handle(handle)?;
        (generation == self.generation).then_some(object)
    }

    pub(crate) fn invalidate_for_registry_replacement(
        &mut self,
    ) -> Result<(), crate::SessionError> {
        self.validate_registry_replacement()?;
        self.generation += 1;
        Ok(())
    }

    pub(crate) fn validate_registry_replacement(&self) -> Result<(), crate::SessionError> {
        self.generation
            .checked_add(1)
            .map(|_| ())
            .ok_or_else(|| crate::SessionError::state("database handle generation is exhausted"))
    }

    fn parse_handle(handle: &str) -> Option<(u64, AnyObjectId)> {
        let encoded = handle.strip_prefix("_obj")?;
        let (generation, member) = encoded.split_once('_')?;
        let generation = Self::parse_decimal(generation)?;
        let (class, raw) = member.split_at_checked(1)?;
        let uid = ObjectUid::from_raw(Self::parse_decimal(raw)?)?;
        let object = match class {
            "d" => AnyObjectId::Design(DesignId::from_uid(uid)),
            "p" => AnyObjectId::Port(PortId::from_uid(uid)),
            "c" => AnyObjectId::Cell(CellId::from_uid(uid)),
            "i" => AnyObjectId::Pin(PinId::from_uid(uid)),
            "n" => AnyObjectId::Net(NetId::from_uid(uid)),
            "k" => AnyObjectId::Clock(ClockId::from_uid(uid)),
            _ => return None,
        };
        Some((generation, object))
    }

    fn parse_decimal(raw: &str) -> Option<u64> {
        if raw.is_empty() || raw.starts_with('0') || !raw.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        raw.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_handles_round_trip_without_collection_storage() {
        let uid = ObjectUid::from_raw(42).unwrap();
        let object = AnyObjectId::Port(PortId::from_uid(uid));
        let store = ObjectHandleCodec::default();
        let handle = store.member_handle(object);
        assert_eq!(handle, "_obj1_p42");
        assert_eq!(store.member_id(&handle), Some(object));
        assert!(ObjectHandleCodec::is_handle(&handle));
        assert!(store.member_id("_obj1_p042").is_none());
        assert!(store.member_id("_obj1_x42").is_none());
    }

    #[test]
    fn registry_replacement_invalidates_object_handles() {
        let uid = ObjectUid::from_raw(42).unwrap();
        let object = AnyObjectId::Port(PortId::from_uid(uid));
        let mut store = ObjectHandleCodec::default();
        let stale = store.member_handle(object);
        store.invalidate_for_registry_replacement().unwrap();
        assert!(store.member_id(&stale).is_none());
        assert_eq!(store.member_handle(object), "_obj2_p42");
    }
}
