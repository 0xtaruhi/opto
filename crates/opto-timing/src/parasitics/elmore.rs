// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{TimingEdge, arnoldi::RcResponse, invalid_net};
use std::collections::VecDeque;

pub(super) fn analyze(
    net: &str,
    node_capacitances: &[Vec<f64>; 2],
    adjacency: &[Vec<(usize, f64)>],
    root: usize,
    time_unit: f64,
) -> Result<Vec<Option<RcResponse>>, crate::TimingError> {
    let tree = RcTree::build(net, adjacency, root)?;
    let mut delays = vec![[0.0; 2]; adjacency.len()];
    for edge in TimingEdge::ALL {
        let mut subtree = node_capacitances[edge.index()].clone();
        for &node in tree.order.iter().rev() {
            if let Some(parent) = tree.parents[node] {
                subtree[parent] += subtree[node];
            }
        }
        for &node in tree.order.iter().skip(1) {
            let parent = tree.parents[node].expect("every non-root tree node has a parent");
            delays[node][edge.index()] = delays[parent][edge.index()]
                + tree.parent_resistance[node] * subtree[node] / time_unit;
        }
    }
    Ok(delays
        .into_iter()
        .map(|delay| {
            Some(RcResponse {
                delay,
                transition: None,
            })
        })
        .collect())
}

struct RcTree {
    parents: Vec<Option<usize>>,
    parent_resistance: Vec<f64>,
    order: Vec<usize>,
}

impl RcTree {
    fn build(
        net: &str,
        adjacency: &[Vec<(usize, f64)>],
        root: usize,
    ) -> Result<Self, crate::TimingError> {
        let mut parents = vec![None; adjacency.len()];
        let mut parent_resistance = vec![0.0; adjacency.len()];
        let mut visited = vec![false; adjacency.len()];
        let mut order = Vec::with_capacity(adjacency.len());
        let mut pending = VecDeque::from([root]);
        visited[root] = true;
        while let Some(node) = pending.pop_front() {
            order.push(node);
            for &(neighbor, edge_resistance) in &adjacency[node] {
                if !visited[neighbor] {
                    visited[neighbor] = true;
                    parents[neighbor] = Some(node);
                    parent_resistance[neighbor] = edge_resistance;
                    pending.push_back(neighbor);
                } else if parents[node] != Some(neighbor) {
                    return Err(invalid_net(
                        net,
                        "resistor network is cyclic; use Arnoldi analysis",
                    ));
                }
            }
        }
        if let Some(node) = visited.iter().position(|visited| !visited) {
            return Err(invalid_net(
                net,
                format!("RC node {node} is disconnected from the driver"),
            ));
        }
        Ok(Self {
            parents,
            parent_resistance,
            order,
        })
    }
}
