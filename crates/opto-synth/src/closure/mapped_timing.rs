// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! One transaction spanning mapped IR and every incremental timing view.
//!
//! Initial mapping and post-map optimization both publish [`RegionDelta`]s.
//! This module is the single owner of their cross-domain apply, commit, and
//! rollback order; callers may add provenance or ownership work at commit time
//! without duplicating recovery logic.

use opto_ir::mapped::{AppliedRegionDelta, MappedNetlist, RegionConflict, RegionDelta};
use opto_timing::{IncrementalTiming, RegionEdit, TimingRegionDelta};

struct AppliedEdit {
    mapped: AppliedRegionDelta,
    timing: Vec<RegionEdit>,
}

type ApplyTiming =
    fn(&mut IncrementalTiming, TimingRegionDelta) -> Result<RegionEdit, opto_timing::TimingError>;

/// A speculative mapped edit kept aligned with every MMMC timing owner.
///
/// The mapped edit is visible through [`Self::mapped`] until this value is
/// committed or rolled back. A stale mapped snapshot returns `Ok(None)` from
/// the constructors without changing either domain.
#[must_use = "a mapped timing transaction must be committed or rolled back"]
pub(crate) struct MappedTimingTransaction<'a> {
    mapped: &'a mut MappedNetlist,
    timing: &'a mut [IncrementalTiming],
    edit: AppliedEdit,
}

impl<'a> MappedTimingTransaction<'a> {
    /// Applies a mapped delta and eagerly updates required-time state in every
    /// timing view. This is the normal entry point for initial mapping.
    pub(crate) fn begin(
        mapped: &'a mut MappedNetlist,
        timing: &'a mut [IncrementalTiming],
        delta: RegionDelta,
    ) -> Result<Option<Self>, crate::SynthError> {
        Self::apply(mapped, timing, delta, IncrementalTiming::apply_region_delta)
    }

    /// Applies a mapped delta while allowing timing owners to defer backward
    /// required-time propagation. Post-map candidate evaluation uses this path.
    pub(crate) fn begin_optimization(
        mapped: &'a mut MappedNetlist,
        timing: &'a mut [IncrementalTiming],
        delta: RegionDelta,
    ) -> Result<Option<Self>, crate::SynthError> {
        Self::apply(
            mapped,
            timing,
            delta,
            IncrementalTiming::apply_optimization_region_delta,
        )
    }

    fn apply(
        mapped: &'a mut MappedNetlist,
        timing: &'a mut [IncrementalTiming],
        delta: RegionDelta,
        apply_timing: ApplyTiming,
    ) -> Result<Option<Self>, crate::SynthError> {
        let mapped_edit = match mapped.apply_region_delta(delta) {
            Ok(edit) => edit,
            Err(RegionConflict::StaleCell(_) | RegionConflict::StaleNet(_)) => return Ok(None),
            Err(error @ RegionConflict::Invalid(_)) => return Err(error.into()),
        };

        if timing.is_empty() {
            return Ok(Some(Self {
                mapped,
                timing,
                edit: AppliedEdit {
                    mapped: mapped_edit,
                    timing: Vec::new(),
                },
            }));
        }
        let timing_delta =
            match TimingRegionDelta::from_mapped_region(mapped, &mapped_edit, timing[0].model()) {
                Ok(delta) => delta,
                Err(error) => {
                    return match mapped.rollback_region_delta(mapped_edit) {
                        Ok(()) => Err(error.into()),
                        Err(rollback) => Err(crate::SynthError::Rollback {
                            operation: "timing region preparation",
                            primary: Box::new(error.into()),
                            rollback: Box::new(rollback.into()),
                        }),
                    };
                }
            };
        let mut timing_edits = Vec::with_capacity(timing.len());
        if !timing_delta.is_empty() {
            for owner in timing.iter_mut() {
                let applied = apply_timing(owner, timing_delta.clone());
                match applied {
                    Ok(edit) => timing_edits.push(edit),
                    Err(error) => {
                        let timing_rollback = rollback_timing(timing, &mut timing_edits);
                        let mapped_rollback = mapped
                            .rollback_region_delta(mapped_edit)
                            .map_err(crate::SynthError::from);
                        return Err(application_failure(
                            error.into(),
                            timing_rollback,
                            mapped_rollback,
                        ));
                    }
                }
            }
        }
        Ok(Some(Self {
            mapped,
            timing,
            edit: AppliedEdit {
                mapped: mapped_edit,
                timing: timing_edits,
            },
        }))
    }

    pub(crate) fn mapped(&self) -> &MappedNetlist {
        self.mapped
    }

    pub(crate) fn timing_mut(&mut self) -> &mut [IncrementalTiming] {
        self.timing
    }

    pub(crate) fn mapped_edit(&self) -> &AppliedRegionDelta {
        &self.edit.mapped
    }

    #[cfg(test)]
    pub(crate) fn timing_edit(&self) -> Option<&RegionEdit> {
        self.edit.timing.first()
    }

    /// Commits mapped and timing state after a caller-owned publication step.
    ///
    /// `publish` runs while the speculative mapped/timing state is visible. If
    /// it fails, both built-in domains are restored and the errors are combined.
    /// The callback must therefore either be atomic or leave its own state
    /// unchanged when returning an error.
    pub(crate) fn commit_with(
        self,
        operation: &'static str,
        publish: impl FnOnce(&MappedNetlist, &AppliedRegionDelta) -> Result<(), crate::SynthError>,
    ) -> Result<(), crate::SynthError> {
        let Self {
            mapped,
            timing,
            edit,
        } = self;
        for (owner, timing_edit) in timing.iter_mut().zip(&edit.timing) {
            if let Err(error) = owner.prepare_commit(timing_edit) {
                return match rollback_applied(mapped, timing, edit) {
                    Ok(()) => Err(error.into()),
                    Err(rollback) => Err(crate::SynthError::Rollback {
                        operation,
                        primary: Box::new(error.into()),
                        rollback: Box::new(rollback),
                    }),
                };
            }
        }
        if let Err(error) = publish(mapped, &edit.mapped) {
            return match rollback_applied(mapped, timing, edit) {
                Ok(()) => Err(error),
                Err(rollback) => Err(crate::SynthError::Rollback {
                    operation,
                    primary: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            };
        }
        for (owner, timing_edit) in timing.iter_mut().zip(edit.timing) {
            owner.commit_prepared(timing_edit);
        }
        Ok(())
    }

    pub(crate) fn rollback(self) -> Result<(), crate::SynthError> {
        rollback_applied(self.mapped, self.timing, self.edit)
    }

    pub(crate) fn abort<T>(
        self,
        error: crate::SynthError,
        operation: &'static str,
    ) -> Result<T, crate::SynthError> {
        match self.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(crate::SynthError::Rollback {
                operation,
                primary: Box::new(error),
                rollback: Box::new(rollback),
            }),
        }
    }
}

fn rollback_applied(
    mapped: &mut MappedNetlist,
    timing: &mut [IncrementalTiming],
    mut edit: AppliedEdit,
) -> Result<(), crate::SynthError> {
    let timing_result = rollback_timing(timing, &mut edit.timing);
    let mapped_result = mapped
        .rollback_region_delta(edit.mapped)
        .map_err(crate::SynthError::from);
    combine_rollback(timing_result, mapped_result)
}

fn rollback_timing(
    timing: &mut [IncrementalTiming],
    edits: &mut Vec<RegionEdit>,
) -> Result<(), crate::SynthError> {
    let mut failure = None;
    for (owner, edit) in timing.iter_mut().zip(edits.drain(..)).rev() {
        if let Err(error) = owner.rollback(edit) {
            let error = crate::SynthError::from(error);
            failure = Some(match failure {
                None => error,
                Some(previous) => crate::SynthError::Rollback {
                    operation: "MMMC timing owner rollback",
                    primary: Box::new(previous),
                    rollback: Box::new(error),
                },
            });
        }
    }
    failure.map_or(Ok(()), Err)
}

fn combine_rollback(
    timing: Result<(), crate::SynthError>,
    mapped: Result<(), crate::SynthError>,
) -> Result<(), crate::SynthError> {
    match (timing, mapped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(timing), Err(mapped)) => Err(crate::SynthError::Rollback {
            operation: "mapped timing rollback",
            primary: Box::new(timing),
            rollback: Box::new(mapped),
        }),
    }
}

fn application_failure(
    primary: crate::SynthError,
    timing_rollback: Result<(), crate::SynthError>,
    mapped_rollback: Result<(), crate::SynthError>,
) -> crate::SynthError {
    [timing_rollback.err(), mapped_rollback.err()]
        .into_iter()
        .flatten()
        .fold(primary, |primary, rollback| crate::SynthError::Rollback {
            operation: "MMMC timing region application",
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        })
}
