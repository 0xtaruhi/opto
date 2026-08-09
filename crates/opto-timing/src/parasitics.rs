// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Validated RC parasitic networks and analysis configuration.
//!
//! Inputs use SI units at the import boundary. Sealed [`Parasitics`] convert
//! them to the active timing-library units and retain deterministic annotations
//! keyed by logical net name.

mod arnoldi;
mod elmore;
mod network;
mod storage;

pub(crate) use storage::{ParasiticNetId, ParasiticNetRef};
pub use storage::{Parasitics, ParasiticsFingerprint};

#[cfg(test)]
mod tests;

use crate::{TimingEdge, TimingLibraryUnits, TimingModelError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Interconnect-delay approximation applied to RC networks.
pub enum ParasiticDelayModel {
    /// Use capacitance annotation without interconnect delay.
    #[default]
    None,
    /// First-moment Elmore delay.
    Elmore,
    /// Reduced-order Arnoldi approximation.
    Arnoldi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Controls how imported parasitics contribute to timing analysis.
pub struct ParasiticAnalysisOptions {
    /// Interconnect delay algorithm.
    pub delay_model: ParasiticDelayModel,
    /// Whether SPEF capacitance already includes library pin capacitance.
    pub pin_capacitance_included: bool,
    /// Whether to annotate total net capacitance without RC topology delays.
    pub net_capacitance_only: bool,
}

impl Default for ParasiticAnalysisOptions {
    fn default() -> Self {
        Self {
            delay_model: ParasiticDelayModel::None,
            pin_capacitance_included: false,
            net_capacitance_only: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// One imported resistor-capacitor network in SI units.
pub struct RcNetwork {
    /// Parasitic net name used to bind the network to the timing graph.
    pub name: String,
    /// Declared total capacitance in farads.
    pub total_capacitance_farads: f64,
    /// Design objects attached to RC nodes.
    pub connections: Vec<RcConnection>,
    /// Grounded and coupling capacitors.
    pub capacitors: Vec<RcCapacitor>,
    /// Resistors between RC nodes.
    pub resistors: Vec<RcResistor>,
    /// Optional normalized source waveforms by edge.
    pub source_waveforms: [Option<RcSourceWaveform>; 2],
}

#[derive(Debug, Clone, PartialEq)]
/// Normalized driver waveform attached to an RC source.
pub struct RcSourceWaveform {
    /// Strictly increasing time samples in seconds.
    pub times: Vec<f64>,
    /// Normalized voltage samples in `[0, 1]`.
    pub normalized_voltage: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
/// Association between an RC node and a design object.
pub struct RcConnection {
    /// RC node name.
    pub node: String,
    /// Design pin or port name.
    pub object: String,
    /// Driver or sink role.
    pub role: RcConnectionRole,
    /// Rise/fall pin capacitance in farads.
    pub pin_capacitance_farads: [f64; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Signal-flow role of an RC network connection.
pub enum RcConnectionRole {
    /// Network source.
    Driver,
    /// Network load.
    Sink,
}

#[derive(Debug, Clone, PartialEq)]
/// Grounded or coupling capacitor in an RC network.
pub struct RcCapacitor {
    /// First RC node endpoint.
    pub first: String,
    /// Second node, or ground when absent.
    pub second: Option<String>,
    /// Capacitance in farads.
    pub capacitance_farads: f64,
}

#[derive(Debug, Clone, PartialEq)]
/// Resistor between two RC nodes.
pub struct RcResistor {
    /// First RC node endpoint.
    pub first: String,
    /// Second RC node endpoint.
    pub second: String,
    /// Resistance in ohms.
    pub resistance_ohms: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Counts produced while applying parasitic annotations.
pub struct ParasiticAnnotationSummary {
    /// Annotated driver-to-sink delay pairs.
    pub pin_to_pin_delays: usize,
    /// Nets successfully annotated.
    pub annotated_nets: usize,
    /// Nets skipped because no timing-model match exists.
    pub skipped_nets: usize,
}

#[derive(Debug, Clone, PartialEq)]
/// Report row for one parasitic driver-to-sink annotation.
pub struct ParasiticAnnotationRow {
    /// Timing-net name receiving the annotation.
    pub net: String,
    /// Driver object.
    pub from: String,
    /// Sink object.
    pub to: String,
    /// Rise/fall delay in timing-library units.
    pub delay: Option<[f64; 2]>,
    /// Effective load in timing-library capacitance units.
    pub load: f64,
}

fn checked_count(value: usize, resource: &'static str) -> Result<u32, crate::TimingError> {
    u32::try_from(value).map_err(|_| TimingModelError::Capacity { resource }.into())
}

fn invalid_net(net: &str, detail: impl Into<String>) -> crate::TimingError {
    TimingModelError::InvalidParasiticNet {
        net: net.to_string(),
        detail: detail.into(),
    }
    .into()
}
