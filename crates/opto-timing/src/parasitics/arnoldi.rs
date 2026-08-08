// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Reduced-order RC transient analysis using an Arnoldi projection.
//!
//! The driver node is treated as a prescribed voltage source and removed from
//! the unknown vector. The remaining nodal system is
//! `C · dv/dt + G · v = b · u(t)`. A bounded Krylov basis projects it to at
//! most eight states; implicit Euler then integrates the dense reduced system.
//! Reported delay is the sink's 50% crossing relative to the source, and slew
//! is its 20–80% interval.

use super::{RcSourceWaveform, invalid_net};
use faer::linalg::solvers::{PartialPivLu, Solve};
use faer::sparse::linalg::solvers::Llt;
use faer::sparse::{SparseColMat, Triplet};
use faer::{Col, Mat, Side};

#[derive(Debug, Clone, Copy)]
pub(super) struct RcResponse {
    pub(super) delay: [f64; 2],
    pub(super) transition: Option<[f64; 2]>,
}

pub(super) fn analyze(
    net: &str,
    node_capacitances: &[Vec<f64>; 2],
    adjacency: &[Vec<(usize, f64)>],
    root: usize,
    source_waveforms: &[Option<RcSourceWaveform>; 2],
    time_unit: f64,
) -> Result<Vec<Option<RcResponse>>, crate::TimingError> {
    let system = if adjacency.len() == 1 {
        None
    } else {
        Some(LinearSystem::new(net, adjacency, root)?)
    };
    let mut responses = vec![
        Some(RcResponse {
            delay: [0.0; 2],
            transition: Some([0.0; 2]),
        });
        adjacency.len()
    ];
    for edge in 0..2 {
        let edge_responses = analyze_edge(
            net,
            &node_capacitances[edge],
            adjacency.len(),
            root,
            system.as_ref(),
            source_waveforms[edge].as_ref(),
            time_unit,
        )?;
        for (node, response) in edge_responses.into_iter().enumerate() {
            let stored = responses[node]
                .as_mut()
                .expect("Arnoldi produces a response for every connected node");
            stored.delay[edge] = response.delay;
            stored
                .transition
                .as_mut()
                .expect("Arnoldi transition array is initialized")[edge] = response.transition;
        }
    }
    Ok(responses)
}

#[derive(Clone, Copy)]
struct EdgeResponse {
    delay: f64,
    transition: f64,
}

struct LinearSystem {
    reduced_nodes: Vec<usize>,
    matrix: ConductanceMatrix,
    source_conductance: Vec<f64>,
    factor: Llt<usize, f64>,
}

impl LinearSystem {
    fn new(
        net: &str,
        adjacency: &[Vec<(usize, f64)>],
        root: usize,
    ) -> Result<Self, crate::TimingError> {
        // Removing the ideal source makes the reduced conductance matrix
        // positive definite for a connected passive network. Edges to the
        // removed node become entries in the source-excitation vector.
        let reduced_nodes = (0..adjacency.len())
            .filter(|node| *node != root)
            .collect::<Vec<_>>();
        let mut reduced_id = vec![None; adjacency.len()];
        for (index, &node) in reduced_nodes.iter().enumerate() {
            reduced_id[node] = Some(index);
        }
        let mut diagonal = vec![0.0; reduced_nodes.len()];
        let mut off_diagonal = Vec::new();
        let mut source_conductance = vec![0.0; reduced_nodes.len()];
        for (row, &node) in reduced_nodes.iter().enumerate() {
            for &(neighbor, resistance) in &adjacency[node] {
                let conductance = 1.0 / resistance;
                diagonal[row] += conductance;
                if neighbor == root {
                    source_conductance[row] += conductance;
                } else if let Some(column) = reduced_id[neighbor] {
                    off_diagonal.push((row, (column, -conductance)));
                }
            }
        }
        off_diagonal.sort_unstable_by_key(|(row, (column, _))| (*row, *column));
        let off_diagonal =
            opto_core::PackedRows::try_from_entries(reduced_nodes.len(), off_diagonal)
                .map_err(|_| invalid_net(net, "Arnoldi conductance adjacency exceeds capacity"))?;
        let matrix = ConductanceMatrix {
            diagonal,
            off_diagonal,
        };
        let factor = matrix.factor(net)?;
        Ok(Self {
            reduced_nodes,
            matrix,
            source_conductance,
            factor,
        })
    }
}

fn analyze_edge(
    net: &str,
    capacitance_farads: &[f64],
    node_count: usize,
    root: usize,
    system: Option<&LinearSystem>,
    source: Option<&RcSourceWaveform>,
    time_unit: f64,
) -> Result<Vec<EdgeResponse>, crate::TimingError> {
    let Some(system) = system else {
        return Ok(vec![EdgeResponse {
            delay: 0.0,
            transition: source.map_or(0.0, source_transition),
        }]);
    };
    let capacitance = system
        .reduced_nodes
        .iter()
        .map(|node| capacitance_farads[*node])
        .collect::<Vec<_>>();
    let steady = solve_sparse(&system.factor, &system.source_conductance);
    // The direct-current response is a useful excitation seed: it points the first Krylov
    // vector toward the input-to-output transfer rather than an arbitrary
    // coordinate axis.
    let basis = arnoldi_basis(&system.matrix, &system.factor, &capacitance, steady);
    let reduced = ReducedModel::project(
        &system.matrix,
        &capacitance,
        &system.source_conductance,
        basis,
    );
    let samples = reduced.simulate(net, source, time_unit)?;
    let source_delay = source.map_or(0.0, |waveform| crossing(waveform, 0.5).unwrap_or(0.0));
    let mut result = vec![
        EdgeResponse {
            delay: 0.0,
            transition: 0.0,
        };
        node_count
    ];
    result[root] = EdgeResponse {
        delay: 0.0,
        transition: source.map_or(0.0, source_transition),
    };
    for (row, &node) in system.reduced_nodes.iter().enumerate() {
        let delay = crossing_response(&reduced, row, &samples, 0.5)
            .ok_or_else(|| invalid_net(net, "Arnoldi response never crosses 50%"))?
            - source_delay;
        let lower = crossing_response(&reduced, row, &samples, 0.2)
            .ok_or_else(|| invalid_net(net, "Arnoldi response never crosses 20%"))?;
        let upper = crossing_response(&reduced, row, &samples, 0.8)
            .ok_or_else(|| invalid_net(net, "Arnoldi response never crosses 80%"))?;
        result[node] = EdgeResponse {
            delay: delay.max(0.0) / time_unit,
            transition: (upper - lower).max(0.0) / time_unit,
        };
    }
    Ok(result)
}

struct ConductanceMatrix {
    diagonal: Vec<f64>,
    off_diagonal: opto_core::PackedRows<(usize, f64)>,
}

impl ConductanceMatrix {
    fn multiply(&self, vector: &[f64]) -> Vec<f64> {
        self.diagonal
            .iter()
            .zip(self.off_diagonal.iter())
            .enumerate()
            .map(|(row, (diagonal, entries))| {
                diagonal * vector[row]
                    + entries
                        .iter()
                        .map(|(column, value)| value * vector[*column])
                        .sum::<f64>()
            })
            .collect()
    }

    fn factor(&self, net: &str) -> Result<Llt<usize, f64>, crate::TimingError> {
        let mut triplets =
            Vec::with_capacity(self.diagonal.len() + self.off_diagonal.value_count() / 2);
        for (row, &diagonal) in self.diagonal.iter().enumerate() {
            triplets.push(Triplet::new(row, row, diagonal));
            for &(column, value) in self.off_diagonal.row(row) {
                if row > column {
                    triplets.push(Triplet::new(row, column, value));
                }
            }
        }
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            self.diagonal.len(),
            self.diagonal.len(),
            &triplets,
        )
        .map_err(|error| {
            invalid_net(
                net,
                format!("cannot build Arnoldi conductance matrix: {error}"),
            )
        })?;
        matrix.sp_cholesky(Side::Lower).map_err(|error| {
            invalid_net(
                net,
                format!("Arnoldi conductance matrix is not positive definite: {error}"),
            )
        })
    }
}

fn solve_sparse(factor: &Llt<usize, f64>, rhs: &[f64]) -> Vec<f64> {
    let rhs = Col::from_fn(rhs.len(), |row| rhs[row]);
    let solution = factor.solve(&rhs);
    (0..solution.nrows()).map(|row| solution[row]).collect()
}

fn arnoldi_basis(
    matrix: &ConductanceMatrix,
    factor: &Llt<usize, f64>,
    capacitance: &[f64],
    mut seed: Vec<f64>,
) -> Vec<Vec<f64>> {
    normalize(&mut seed);
    let mut basis = vec![seed];
    // Cap the projection at eight moments. Modified Gram–Schmidt stops when
    // the next moment is numerically dependent.
    let order = matrix.diagonal.len().min(8);
    while basis.len() < order {
        let rhs = basis
            .last()
            .expect("Arnoldi basis is seeded")
            .iter()
            .zip(capacitance)
            .map(|(value, capacitance)| value * capacitance)
            .collect::<Vec<_>>();
        let mut candidate = solve_sparse(factor, &rhs);
        for vector in &basis {
            let projection = dot(&candidate, vector);
            axpy(&mut candidate, -projection, vector);
        }
        let norm = dot(&candidate, &candidate).sqrt();
        if norm <= 1e-12 {
            break;
        }
        for value in &mut candidate {
            *value /= norm;
        }
        basis.push(candidate);
    }
    basis
}

struct ReducedModel {
    basis: Vec<Vec<f64>>,
    conductance: Vec<Vec<f64>>,
    capacitance: Vec<Vec<f64>>,
    source: Vec<f64>,
}

impl ReducedModel {
    fn project(
        matrix: &ConductanceMatrix,
        capacitance: &[f64],
        source: &[f64],
        basis: Vec<Vec<f64>>,
    ) -> Self {
        // Galerkin projection: Gr = VᵀGV, Cr = VᵀCV, br = Vᵀb. Basis vectors
        // are stored by column conceptually, although Vec layout uses one
        // contiguous vector per basis column.
        let order = basis.len();
        let mut conductance = vec![vec![0.0; order]; order];
        let mut reduced_capacitance = vec![vec![0.0; order]; order];
        let mut reduced_source = vec![0.0; order];
        for row in 0..order {
            let image = matrix.multiply(&basis[row]);
            reduced_source[row] = dot(&basis[row], source);
            for column in 0..order {
                conductance[row][column] = dot(&basis[column], &image);
                reduced_capacitance[row][column] = basis[row]
                    .iter()
                    .zip(&basis[column])
                    .zip(capacitance)
                    .map(|((left, right), capacitance)| left * right * capacitance)
                    .sum();
            }
        }
        Self {
            basis,
            conductance,
            capacitance: reduced_capacitance,
            source: reduced_source,
        }
    }

    fn simulate(
        &self,
        net: &str,
        source: Option<&RcSourceWaveform>,
        time_unit: f64,
    ) -> Result<Vec<StateSample>, crate::TimingError> {
        // Use the projected diagonal C/G ratio as the time scale and cap the
        // grid at 8192 solves.
        let time_scale = self.estimated_time_scale().max(time_unit * 1e-3);
        let source_end = source
            .and_then(|waveform| waveform.times.last().copied())
            .unwrap_or(0.0);
        let end = source_end + 16.0 * time_scale;
        let step = (time_scale / 128.0)
            .min((end / 1024.0).max(time_unit * 1e-5))
            .max(time_unit * 1e-6);
        let count = bounded_sample_count((end / step).ceil());
        let count_f64 = f64::from(
            u32::try_from(count).expect("the transient sample grid is capped at 8192 points"),
        );
        let step = end / count_f64;
        let mut state = vec![0.0; self.basis.len()];
        let mut samples = Vec::with_capacity(count + 1);
        samples.push(StateSample {
            time: 0.0,
            state: state.clone(),
        });
        let order = self.basis.len();
        let system = Mat::from_fn(order, order, |row, column| {
            self.conductance[row][column] + self.capacitance[row][column] / step
        });
        // Backward Euler is unconditionally stable for passive RC systems. The
        // matrix and its LU factor are constant for the uniform time grid.
        let factor = system.partial_piv_lu();
        for index in 1..=count {
            let time = f64::from(
                u32::try_from(index).expect("the transient sample grid is capped at 8192 points"),
            ) * step;
            let voltage = source.map_or(1.0, |waveform| source_value(waveform, time));
            let mut rhs = self
                .source
                .iter()
                .map(|source| source * voltage)
                .collect::<Vec<_>>();
            for (row, rhs) in rhs.iter_mut().enumerate() {
                *rhs += self.capacitance[row]
                    .iter()
                    .zip(&state)
                    .map(|(capacitance, state)| capacitance * state / step)
                    .sum::<f64>();
            }
            state = solve_dense(net, &factor, &rhs)?;
            samples.push(StateSample {
                time,
                state: state.clone(),
            });
        }
        Ok(samples)
    }

    fn estimated_time_scale(&self) -> f64 {
        let total_capacitance = self
            .capacitance
            .iter()
            .enumerate()
            .map(|(index, row)| row[index].abs())
            .sum::<f64>();
        let total_conductance = self
            .conductance
            .iter()
            .enumerate()
            .map(|(index, row)| row[index].abs())
            .sum::<f64>();
        total_capacitance / total_conductance.max(f64::MIN_POSITIVE)
    }

    fn output(&self, node: usize, state: &[f64]) -> f64 {
        self.basis
            .iter()
            .zip(state)
            .map(|(basis, state)| basis[node] * state)
            .sum::<f64>()
            .clamp(0.0, 1.0)
    }
}

struct StateSample {
    time: f64,
    state: Vec<f64>,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the floating-point estimate is explicitly clamped to the closed integer range \
              128..=8192 before conversion"
)]
fn bounded_sample_count(estimate: f64) -> usize {
    if !estimate.is_finite() || estimate >= 8192.0 {
        8192
    } else if estimate <= 128.0 {
        128
    } else {
        estimate as usize
    }
}

fn crossing_response(
    model: &ReducedModel,
    node: usize,
    samples: &[StateSample],
    threshold: f64,
) -> Option<f64> {
    for pair in samples.windows(2) {
        let [first, second] = pair else {
            unreachable!("window size is fixed");
        };
        let first_value = model.output(node, &first.state);
        let second_value = model.output(node, &second.state);
        if first_value <= threshold && second_value >= threshold {
            if (second_value - first_value).abs() <= f64::EPSILON {
                return Some(second.time);
            }
            let ratio = (threshold - first_value) / (second_value - first_value);
            return Some(first.time + ratio * (second.time - first.time));
        }
    }
    None
}

fn solve_dense(
    net: &str,
    factor: &PartialPivLu<f64>,
    rhs: &[f64],
) -> Result<Vec<f64>, crate::TimingError> {
    let order = rhs.len();
    let rhs = Col::from_fn(order, |row| rhs[row]);
    let solution = factor.solve(&rhs);
    let solution = (0..order).map(|row| solution[row]).collect::<Vec<_>>();
    if solution.iter().any(|value| !value.is_finite()) {
        return Err(invalid_net(net, "Arnoldi reduced system is singular"));
    }
    Ok(solution)
}

fn source_transition(source: &RcSourceWaveform) -> f64 {
    let lower = crossing(source, 0.2).unwrap_or(0.0);
    let upper = crossing(source, 0.8).unwrap_or(lower);
    upper - lower
}

fn crossing(source: &RcSourceWaveform, threshold: f64) -> Option<f64> {
    crossing_samples(
        &source
            .times
            .iter()
            .copied()
            .zip(source.normalized_voltage.iter().copied())
            .collect::<Vec<_>>(),
        threshold,
    )
}

fn crossing_samples(samples: &[(f64, f64)], threshold: f64) -> Option<f64> {
    for pair in samples.windows(2) {
        let [(first_time, first_value), (second_time, second_value)] = pair else {
            unreachable!("window size is fixed");
        };
        if *first_value <= threshold && *second_value >= threshold {
            if (second_value - first_value).abs() <= f64::EPSILON {
                return Some(*second_time);
            }
            let ratio = (threshold - first_value) / (second_value - first_value);
            return Some(first_time + ratio * (second_time - first_time));
        }
    }
    None
}

fn source_value(source: &RcSourceWaveform, time: f64) -> f64 {
    if time <= source.times[0] {
        return source.normalized_voltage[0];
    }
    for index in 1..source.times.len() {
        if time <= source.times[index] {
            let ratio =
                (time - source.times[index - 1]) / (source.times[index] - source.times[index - 1]);
            return source.normalized_voltage[index - 1]
                + ratio
                    * (source.normalized_voltage[index] - source.normalized_voltage[index - 1]);
        }
    }
    *source
        .normalized_voltage
        .last()
        .expect("validated source has samples")
}

fn normalize(vector: &mut [f64]) {
    let norm = dot(vector, vector).sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn axpy(destination: &mut [f64], scale: f64, source: &[f64]) {
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination += scale * source;
    }
}
