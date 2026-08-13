// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Atomic reconciliation of design objects and their dependent session state.
//!
//! The registry is the identity authority, but timing and power retain reverse
//! references into it. Every operation therefore prepares and validates all
//! fallible dependent edits before committing those edits and publishing the
//! registry change last. Design records remain the caller's responsibility so
//! a failed reconciliation cannot expose a partial source or mapped artifact.

use crate::objects::locators::{DesignObjectBatch, DesignObjectScope};
use crate::{DesignView, MappedObjectIndex, Session, SessionError};
use opto_db::{
    AnyObjectId, DesignIndex, ObjectReconcileMode, ObjectReconcileSource, ObjectRegistry,
};
use std::collections::BTreeSet;

fn apply_removals(
    session: &mut Session,
    removed: &BTreeSet<AnyObjectId>,
) -> Result<(), SessionError> {
    let timing = session.state.timing.prepare_object_removal(removed)?;
    let power = session.state.power.prepare_object_removal(removed)?;
    let timing = session.state.timing.validate_object_removal(timing)?;
    session
        .state
        .objects
        .apply_edit(removed, std::iter::empty())
        .map_err(SessionError::Registry)?;
    timing.commit();
    session.state.power.apply_object_removal(power);
    Ok(())
}

fn apply_reconcile(
    objects: &mut ObjectRegistry,
    timing: &mut opto_timing::TimingContext,
    power: &mut crate::power::PowerContext,
    source: &dyn ObjectReconcileSource,
) -> Result<(), SessionError> {
    let plan = objects
        .plan_reconcile(source)
        .map_err(SessionError::Registry)?;
    let removed = plan.removed(objects).map_err(SessionError::Registry)?;
    let timing_edit = timing.prepare_object_removal(&removed)?;
    let power_edit = power.prepare_object_removal(&removed)?;

    // The timing token exclusively binds its owner and revision before the
    // registry performs its final fallible preflight.
    let timing = timing.validate_object_removal(timing_edit)?;
    let registry = objects
        .prepare_reconcile(plan, source)
        .map_err(SessionError::Registry)?;

    // Every remaining operation is deterministic and infallible. Dependent
    // owners commit sparse edits prepared from the same slot-backed view;
    // registry identities remain live until those edits are complete.
    timing.commit();
    power.apply_object_removal(power_edit);
    registry.commit();
    Ok(())
}

/// Deletes registry objects after preflighting every dependent owner.
pub(crate) fn delete_objects(
    session: &mut Session,
    removed: &BTreeSet<AnyObjectId>,
) -> Result<(), SessionError> {
    apply_removals(session, removed)
}

/// Reconciles the current design against a borrowed mapped artifact.
///
/// This function updates only registry-dependent owners. The caller must not
/// install the mapped artifact or its sidecar until this fallible phase has
/// succeeded.
pub(crate) fn reconcile_mapped_objects(
    session: &mut Session,
    mapped: &opto_ir::mapped::MappedNetlist,
    index: &MappedObjectIndex,
) -> Result<(), SessionError> {
    let name = mapped.name();
    if !session.state.designs.contains_key(name) {
        return Err(SessionError::state(format!(
            "mapped object reconciliation references missing design '{name}'"
        )));
    }
    {
        let mut source = DesignObjectBatch::default();
        source.push_design(
            &session.state.designs,
            DesignView::mapped(mapped, index),
            ObjectReconcileMode::Update,
            DesignObjectScope::Complete,
        )?;
        source.seal()?;
        apply_reconcile(
            &mut session.state.objects,
            &mut session.state.timing,
            &mut session.state.power,
            &source,
        )?;
    }

    Ok(())
}

/// Activates the canonical mapped artifact already owned by one design.
///
/// The compact index is built by the caller before any owner changes. The
/// reconciliation borrows `synthesized.mapped()` directly and publishes the
/// sidecar only after every fallible dependent-owner preflight succeeds.
pub(crate) fn activate_stored_mapped_objects(
    session: &mut Session,
    name: &str,
    index: MappedObjectIndex,
) -> Result<(), SessionError> {
    let state = &mut session.state;
    {
        let record = state.designs.get(name).ok_or_else(|| {
            SessionError::state(format!(
                "mapped object reconciliation references missing design '{name}'"
            ))
        })?;
        let mapped = record.synthesized.as_ref().ok_or_else(|| {
            SessionError::state(format!(
                "mapped object reconciliation references unsynthesized design '{name}'"
            ))
        })?;
        let mut source = DesignObjectBatch::default();
        source.push_design(
            &state.designs,
            DesignView::mapped(mapped.mapped(), &index),
            ObjectReconcileMode::Update,
            DesignObjectScope::Complete,
        )?;
        source.seal()?;
        apply_reconcile(
            &mut state.objects,
            &mut state.timing,
            &mut state.power,
            &source,
        )?;
    }
    state
        .designs
        .get_mut(name)
        .expect("mapped reconciliation validated the design")
        .mapped_object_index = Some(index);
    Ok(())
}

/// Reconciles every source design's stable objects as one atomic batch.
/// Design records are intentionally not touched here: callers publish them
/// only after this fallible phase succeeds.
pub(crate) fn reconcile_source_objects(
    session: &mut Session,
    designs: &[DesignIndex],
) -> Result<(), SessionError> {
    let mut source = DesignObjectBatch::default();
    for design in designs {
        source.push_design(
            &session.state.designs,
            DesignView::source(design),
            ObjectReconcileMode::Replace,
            DesignObjectScope::DesignAndPorts,
        )?;
    }
    source.seal()?;
    apply_reconcile(
        &mut session.state.objects,
        &mut session.state.timing,
        &mut session.state.power,
        &source,
    )
}

/// Reconciles removals, source-design updates, and newly created source
/// designs without publishing any design records. The caller can therefore
/// finish the commit with infallible ownership moves after this succeeds.
#[cfg(test)]
pub(crate) fn reconcile_source_changes(
    session: &mut Session,
    removals: &[String],
    updates: &[DesignIndex],
    fresh: &[DesignIndex],
) -> Result<(), SessionError> {
    let mut source = DesignObjectBatch::default();
    for name in removals {
        source.push_remove(name);
    }
    for design in updates {
        source.push_design(
            &session.state.designs,
            DesignView::source(design),
            ObjectReconcileMode::Update,
            DesignObjectScope::Complete,
        )?;
    }
    for design in fresh {
        source.push_design(
            &session.state.designs,
            DesignView::source(design),
            ObjectReconcileMode::Replace,
            DesignObjectScope::DesignAndPorts,
        )?;
    }
    source.seal()?;
    apply_reconcile(
        &mut session.state.objects,
        &mut session.state.timing,
        &mut session.state.power,
        &source,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_db::{Direction, ObjectIdSet, ObjectLocator, Port, RevisionId};
    use opto_ir::mapped::MappedBuilder;
    use opto_ir::rtl::RtlModule;
    use opto_ir::word::WordModule;
    use opto_power::SwitchingActivity;

    fn empty_rtl(name: &str) -> RtlModule {
        RtlModule::structural(WordModule::new(name)).unwrap()
    }

    fn empty_mapped(name: &str) -> opto_ir::mapped::MappedNetlist {
        MappedBuilder::new(name, RevisionId::INITIAL)
            .unwrap()
            .freeze()
            .unwrap()
            .finalize_for_publication()
            .unwrap()
            .0
    }

    #[test]
    fn mapped_object_reconciliation_failure_mutates_no_owner() {
        let mut session = Session::new();
        let mut original = DesignIndex::new("top");
        let port_name = original.intern_name("a").unwrap();
        original.add_port(Port {
            name: port_name,
            direction: Direction::Input,
            width: 1,
        });
        session
            .install_design_fresh(empty_rtl("top"), RevisionId::INITIAL, original)
            .unwrap();
        let port = session
            .state
            .objects
            .get(&ObjectLocator::Port {
                design: "top".to_string(),
                name: "a".to_string(),
            })
            .unwrap();
        let AnyObjectId::Port(port_id) = port else {
            panic!("expected a port object");
        };
        let handle = session.collection_member_handle(port);
        session.set_load(1.5, &[port_id]).unwrap();
        session
            .state
            .power
            .activities
            .insert(port, SwitchingActivity::quiescent());
        let timing_before = session.state.timing.clone();
        let encoded = opto_archive::to_bytes(&u64::MAX).unwrap();
        let exhausted: RevisionId = opto_archive::from_bytes(&encoded).unwrap();
        session.state.power.revision = exhausted;

        let mapped = empty_mapped("top");
        let index = MappedObjectIndex::new(&mapped, &session.process.runtime).unwrap();
        let error = reconcile_mapped_objects(&mut session, &mapped, &index).unwrap_err();

        assert!(error.to_string().contains("revision"));
        assert!(session.state.objects.resolve(port).is_some());
        assert_eq!(session.collection_len(&handle).unwrap(), 1);
        assert_eq!(session.state.timing, timing_before);
        assert!(session.state.power.activities.contains_key(&port));
        assert_eq!(
            session
                .state
                .designs
                .get("top")
                .unwrap()
                .object_index
                .ports
                .len(),
            1
        );
    }

    #[test]
    fn final_registry_failure_discards_validated_owner_edits() {
        let mut session = Session::new();
        let unknown = opto_db::PortId::from_uid(opto_db::ObjectUid::from_raw(1).unwrap());
        let object = AnyObjectId::Port(unknown);
        session.set_load(1.5, &[unknown]).unwrap();
        session
            .state
            .power
            .activities
            .insert(object, SwitchingActivity::quiescent());
        let timing_before = session.state.timing.clone();
        let power_before = session.state.power.clone();
        let error = apply_removals(&mut session, &BTreeSet::from([object])).unwrap_err();

        assert!(matches!(
            error,
            SessionError::Registry(opto_db::RegistryError::InvalidEdit(_))
        ));
        assert_eq!(session.state.timing, timing_before);
        assert_eq!(session.state.power.revision, power_before.revision);
        assert_eq!(session.state.power.activities, power_before.activities);

        session.set_load(2.0, &[unknown]).unwrap();
        assert_eq!(session.state.timing.load_on(unknown), Some(2.0));
    }

    #[test]
    fn successful_commit_removes_object_bound_owner_state_once() {
        let mut session = Session::new();
        let mut original = DesignIndex::new("top");
        let port_name = original.intern_name("a").unwrap();
        original.add_port(Port {
            name: port_name,
            direction: Direction::Input,
            width: 1,
        });
        session
            .install_design_fresh(empty_rtl("top"), RevisionId::INITIAL, original)
            .unwrap();
        let port = session
            .state
            .objects
            .get(&ObjectLocator::Port {
                design: "top".to_string(),
                name: "a".to_string(),
            })
            .unwrap();
        let AnyObjectId::Port(port_id) = port else {
            panic!("expected a port object");
        };
        let handle = session.collection_member_handle(port);
        session.set_load(1.5, &[port_id]).unwrap();
        session
            .state
            .power
            .activities
            .insert(port, SwitchingActivity::quiescent());

        let mapped = empty_mapped("top");
        let index = MappedObjectIndex::new(&mapped, &session.process.runtime).unwrap();
        reconcile_mapped_objects(&mut session, &mapped, &index).unwrap();

        assert!(session.state.objects.resolve(port).is_none());
        assert!(session.collection_len(&handle).is_err());
        assert_eq!(session.state.timing.load_on(port_id), None);
        assert!(!session.state.power.activities.contains_key(&port));
        let record = session.state.designs.get("top").unwrap();
        assert_eq!(record.object_index.ports.len(), 1);
        assert!(record.mapped_object_index.is_none());
    }

    #[test]
    fn source_plan_size_is_independent_of_unrelated_registry_objects() {
        let mut session = Session::new();
        let mut original = DesignIndex::new("top");
        let port_name = original.intern_name("a").unwrap();
        original.add_port(Port {
            name: port_name,
            direction: Direction::Input,
            width: 1,
        });
        session
            .install_design_fresh(empty_rtl("top"), RevisionId::INITIAL, original.clone())
            .unwrap();
        for index in 0..4096 {
            session
                .state
                .objects
                .intern(ObjectLocator::Port {
                    design: "background".to_string(),
                    name: format!("p{index}"),
                })
                .unwrap();
        }

        let mut source = DesignObjectBatch::default();
        source
            .push_design(
                &session.state.designs,
                DesignView::source(&original),
                ObjectReconcileMode::Replace,
                DesignObjectScope::DesignAndPorts,
            )
            .unwrap();
        source.seal().unwrap();
        let plan = session.state.objects.plan_reconcile(&source).unwrap();
        let removed = plan.removed(&session.state.objects).unwrap();

        assert_eq!(removed.iter().count(), 2);
    }
}
