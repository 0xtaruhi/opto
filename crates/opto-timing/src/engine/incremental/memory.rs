// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Deterministic retained and construction-peak memory model for one incremental view.
pub struct IncrementalTimingMemory {
    /// Current resident bytes.
    pub resident_bytes: usize,
    /// Peak temporary construction bytes.
    pub construction_scratch_high_water_bytes: usize,
    /// Peak resident plus construction-scratch bytes.
    pub construction_high_water_bytes: usize,
}

impl IncrementalTiming {
    #[must_use]
    /// Returns complete logical memory for this view, including its shared Arc payloads.
    pub fn memory_usage(&self) -> IncrementalTimingMemory {
        IncrementalTimingMemory {
            resident_bytes: self.resident_memory_bytes(),
            construction_scratch_high_water_bytes: self.construction_scratch_high_water_bytes,
            construction_high_water_bytes: self.construction_high_water_bytes,
        }
    }

    #[must_use]
    /// Returns the engine's estimated resident bytes.
    pub fn resident_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.model
                    .resident_memory_bytes()
                    .saturating_sub(std::mem::size_of::<TimingModel>()),
            )
            .saturating_add(self.timing.arc_resident_memory_bytes())
            .saturating_add(report_options_owned_memory_bytes(&self.options))
            .saturating_add(self.propagation.owned_memory_bytes())
            .saturating_add(self.closure.owned_memory_bytes())
            .saturating_add(self.constraints.owned_memory_bytes())
            .saturating_add(self.design_rules.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<bool>(
                self.required_dirty.capacity(),
            ))
    }

    #[must_use]
    /// Returns whether two views share their timing context.
    pub fn shares_timing_context(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.timing, &other.timing)
    }

    #[must_use]
    /// Returns resident bytes of the shared timing context.
    pub fn shared_timing_context_resident_memory_bytes(&self) -> usize {
        self.timing.arc_resident_memory_bytes()
    }

    #[must_use]
    /// Describes model allocations shared by characterized views.
    pub fn shared_model_components(&self) -> Vec<crate::SharedTimingComponent> {
        self.model.shared_components()
    }

    #[must_use]
    /// Returns whether two views share persistent object bindings.
    pub fn shares_object_bindings(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.model.object_bindings, &other.model.object_bindings)
    }

    #[must_use]
    /// Returns resident bytes of shared object bindings.
    pub fn shared_object_bindings_resident_memory_bytes(&self) -> usize {
        self.model.object_bindings.resident_memory_bytes()
    }
}

fn report_options_owned_memory_bytes(options: &ReportTimingOptions) -> usize {
    let strings = |values: &[String], capacity| {
        opto_core::resident::slice_bytes::<String>(capacity).saturating_add(
            values
                .iter()
                .map(|value| opto_core::resident::allocation_bytes(value.capacity()))
                .sum::<usize>(),
        )
    };
    strings(&options.from, options.from.capacity())
        .saturating_add(strings(&options.to, options.to.capacity()))
}
