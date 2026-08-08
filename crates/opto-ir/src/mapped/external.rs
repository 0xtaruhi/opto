// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Generation-local index of nets retained by immutable design boundaries.

use super::{ConnectionSignal, MappedError, MappedNetlist, NetId};

pub(super) fn build_external_net_index(
    port_nets: &[NetId],
    constant_drivers: &[(NetId, bool)],
    design_signals: &[ConnectionSignal],
) -> Vec<NetId> {
    let mut nets = Vec::with_capacity(
        port_nets
            .len()
            .saturating_add(constant_drivers.len())
            .saturating_add(design_signals.len()),
    );
    nets.extend_from_slice(port_nets);
    nets.extend(constant_drivers.iter().map(|&(net, _)| net));
    nets.extend(design_signals.iter().filter_map(|signal| match signal {
        ConnectionSignal::Net(net) => Some(*net),
        ConnectionSignal::Constant(_) => None,
    }));
    nets.sort_unstable();
    nets.dedup();
    nets
}

impl MappedNetlist {
    /// Deterministic upper bound for the visitation arena reused by checkpoint
    /// reference, connectivity, and name validation.
    #[must_use]
    pub fn checkpoint_validation_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<u8>(
            self.connections
                .len()
                .max(self.names.entry_count())
                .max(self.nets.len()),
        )
    }

    pub(super) fn validation_scratch(&self) -> Vec<u8> {
        vec![
            0;
            self.connections
                .len()
                .max(self.names.entry_count())
                .max(self.nets.len())
        ]
    }

    pub(super) fn is_external_net(&self, net: NetId) -> bool {
        self.external_nets.binary_search(&net).is_ok()
    }

    pub(super) fn validate_external_net_index(
        &self,
        referenced: &mut [u8],
    ) -> Result<(), MappedError> {
        if referenced.len() != self.nets.len() {
            return Err(MappedError::invariant(
                "mapped external-net validation arena has the wrong length",
            ));
        }
        if self.external_nets.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(MappedError::invariant(
                "mapped external-net index is not sorted and unique",
            ));
        }

        let mut mark = |net: NetId| -> Result<(), MappedError> {
            let entry = referenced.get_mut(net.index()).ok_or_else(|| {
                MappedError::invariant(format!(
                    "mapped design boundary references unknown net {net:?}"
                ))
            })?;
            *entry = 1;
            Ok(())
        };
        for &net in &self.port_nets {
            mark(net)?;
        }
        for &(net, _) in &self.constant_drivers {
            mark(net)?;
        }
        for signal in &self.design_connection_signals {
            if let ConnectionSignal::Net(net) = signal {
                mark(*net)?;
            }
        }

        for &net in &self.external_nets {
            let entry = referenced.get_mut(net.index()).ok_or_else(|| {
                MappedError::invariant(format!(
                    "mapped external-net index contains unknown net {net:?}"
                ))
            })?;
            if std::mem::replace(entry, 0) == 0 {
                return Err(MappedError::invariant(format!(
                    "mapped external-net index contains unreferenced net {net:?}"
                )));
            }
        }
        if referenced.contains(&1) {
            return Err(MappedError::invariant(
                "mapped external-net index omits a design-boundary reference",
            ));
        }
        Ok(())
    }
}
