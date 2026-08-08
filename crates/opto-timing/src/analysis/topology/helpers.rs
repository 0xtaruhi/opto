// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn btree_bytes<K, V>(len: usize) -> usize {
    // Model each entry as its payload plus four words of tree bookkeeping.
    opto_core::resident::slice_bytes::<(K, V, [usize; 4])>(len)
}

pub(super) fn validate_parasitics(
    parasitics: &crate::Parasitics,
    net_names: &SharedNetNames,
    instance_nets: &InstanceNetArena,
    design: &crate::model::SharedTimingDesign,
) -> Result<(), crate::TimingError> {
    if parasitics.is_empty() {
        return Ok(());
    }
    if let Some(name) = parasitics
        .net_names()
        .find(|name| net_names.net_id(name).is_none())
    {
        return Err(crate::TimingModelError::InvalidParasiticNet {
            net: name.to_string(),
            detail: "net is absent from the current timing design".to_string(),
        }
        .into());
    }

    // Instance names reuse the design's resident exact index. Only the small
    // top-level port order is temporary validation scratch.
    let ports = design.ports();
    let mut port_names = Vec::with_capacity(ports.len());
    for index in 0..ports.len() {
        port_names.push(
            u32::try_from(index).map_err(|_| crate::TimingModelError::Capacity {
                resource: "parasitic port-name index",
            })?,
        );
    }
    port_names.sort_unstable_by(|&left, &right| {
        ports[left as usize].name.cmp(&ports[right as usize].name)
    });

    for net_name in parasitics.net_names() {
        let net = parasitics
            .net(net_name)
            .expect("name originates from the parasitic net arena");
        if let Some(sink) = net.sink_names().find(|sink| {
            parasitic_sink_net(sink, design, instance_nets, net_names, &port_names)
                != Some(net_name)
        }) {
            return Err(crate::TimingModelError::InvalidParasiticNet {
                net: net_name.to_string(),
                detail: format!(
                    "sink '{sink}' is absent from this net in the current timing design"
                ),
            }
            .into());
        }
    }
    Ok(())
}

fn parasitic_sink_net<'a>(
    sink: &str,
    design: &'a crate::model::SharedTimingDesign,
    instance_nets: &'a InstanceNetArena,
    net_names: &'a SharedNetNames,
    port_names: &[u32],
) -> Option<&'a str> {
    if let Some((instance_name, pin_name)) = sink.rsplit_once('/')
        && let Some(position) = design.instance_position(instance_name)
        && let Some(instance) = design.instance(position)
        && let Some(pin) = instance
            .connections()
            .position(|connection| connection.pin == pin_name)
        && let Some(net) = instance_nets
            .get(instance.id)
            .and_then(|nets| nets.get(pin))
            .and_then(|net| net_names.get(net.index()))
    {
        return Some(net);
    }
    let ports = design.ports();
    port_names
        .binary_search_by(|&index| ports[index as usize].name.as_str().cmp(sink))
        .ok()
        .map(|position| ports[port_names[position] as usize].net.name())
}

pub(super) fn topological_order(graph: &TimingGraph) -> (Vec<usize>, Vec<usize>) {
    let mut indegree = vec![0usize; graph.outgoing.len()];
    for arcs in graph.outgoing.iter() {
        for &arc in arcs {
            indegree[graph.arc(arc).to.index()] += 1;
        }
    }
    let mut queue = indegree
        .iter()
        .enumerate()
        .filter_map(|(net, &degree)| (degree == 0).then_some(net))
        .collect::<VecDeque<_>>();
    let mut order = Vec::with_capacity(graph.outgoing.len());
    while let Some(net) = queue.pop_front() {
        order.push(net);
        for &arc in &graph.outgoing[net] {
            let to = graph.arc(arc).to.index();
            indegree[to] -= 1;
            if indegree[to] == 0 {
                queue.push_back(to);
            }
        }
    }
    (order, indegree)
}

pub(super) fn compute_design_topological_order(
    graph: &TimingGraph,
) -> Result<Vec<usize>, crate::TimingError> {
    let (order, indegree) = topological_order(graph);
    if order.len() == graph.outgoing.len() {
        return Ok(order);
    }
    let loop_net = indegree
        .iter()
        .position(|&degree| degree != 0)
        .ok_or(crate::TimingAnalysisError::InconsistentTopology)?;
    let cycle = residual_cycle(graph, &indegree, loop_net);
    let contains_latch_arc = graph.outgoing.iter().enumerate().any(|(from, arcs)| {
        indegree[from] != 0
            && arcs.iter().any(|&arc| {
                let arc = graph.arc(arc);
                indegree[arc.to.index()] != 0 && matches!(arc.kind, GraphArcKind::LatchData { .. })
            })
    });
    Err(if contains_latch_arc {
        crate::TimingAnalysisError::LatchTransparencyLoop {
            net: graph.net_names[loop_net].to_string(),
        }
    } else {
        crate::TimingAnalysisError::CombinationalLoop {
            net: graph.net_names[loop_net].to_string(),
            path: cycle,
        }
    }
    .into())
}

/// Names one cycle in the subgraph the topological sweep could not order.
///
/// Every net the sweep left behind has an unsatisfied predecessor that was also
/// left behind, so walking predecessors inside the residue can never dead-end
/// and must revisit a net; the walk between the two visits is a cycle. Walking
/// successors instead would leave the cycle through its fanout.
fn residual_cycle(graph: &TimingGraph, indegree: &[usize], start: usize) -> String {
    let mut predecessor = std::collections::HashMap::<usize, usize>::new();
    for (from, arcs) in graph.outgoing.iter().enumerate() {
        if indegree[from] == 0 {
            continue;
        }
        for &arc in arcs {
            let to = graph.arc(arc).to.index();
            if indegree[to] != 0 {
                predecessor.entry(to).or_insert(from);
            }
        }
    }
    let mut seen = std::collections::HashMap::new();
    let mut walk = Vec::new();
    let mut net = start;
    loop {
        if let Some(&first) = seen.get(&net) {
            let mut text = walk[first..]
                .iter()
                .rev()
                .map(|&net: &usize| graph.net_names[net].to_string())
                .collect::<Vec<_>>();
            text.push(graph.net_names[net].to_string());
            return text.join(" -> ");
        }
        seen.insert(net, walk.len());
        walk.push(net);
        let Some(&previous) = predecessor.get(&net) else {
            return graph.net_names[net].to_string();
        };
        net = previous;
    }
}

pub(super) fn compute_topological_order(
    graph: &TimingGraph,
) -> Result<Vec<usize>, crate::TimingError> {
    let (order, _) = topological_order(graph);
    if order.len() == graph.outgoing.len() {
        Ok(order)
    } else {
        Err(crate::TimingAnalysisError::BufferInsertionLoop.into())
    }
}

pub(crate) fn connection_map_ref(
    instance: TimingInstanceRef<'_>,
) -> BTreeMap<&str, crate::TimingNetId> {
    instance
        .connections()
        .map(|connection| (connection.pin, connection.net))
        .collect()
}

impl SealedTopology {
    pub(crate) fn flat(design: &TimingDesign) -> Result<Self, crate::TimingError> {
        let mut names = TimingNetNamesBuilder::new();
        let port_nets = design
            .ports
            .iter()
            .map(|port| names.intern(port.net.name()))
            .collect::<Result<Vec<_>, _>>()?;
        let row_count = design
            .instances
            .iter()
            .map(|instance| instance.id.raw() as usize + 1)
            .max()
            .unwrap_or(0);
        let mut positions = vec![None; row_count];
        for (position, instance) in design.instances.iter().enumerate() {
            if positions[instance.id.raw() as usize]
                .replace(position)
                .is_some()
            {
                return Err(crate::TimingModelError::DuplicateInstanceId {
                    id: instance.id.raw(),
                }
                .into());
            }
        }
        let mut instance_nets = InstanceNetArena::builder(row_count)?;
        let mut row_scratch_high_water_bytes = 0usize;
        for position in positions.into_iter().flatten() {
            let instance = &design.instances[position];
            row_scratch_high_water_bytes =
                row_scratch_high_water_bytes.max(opto_core::resident::slice_bytes::<
                    crate::TimingNetId,
                >(instance.connections.len()));
            let nets = instance
                .connections
                .iter()
                .map(|connection| names.intern(&connection.net))
                .collect::<Result<Vec<_>, _>>()?;
            instance_nets.push(instance.id, nets.into_iter())?;
        }
        Ok(Self {
            net_names: names.finish(),
            port_nets: port_nets.into_boxed_slice(),
            instance_nets: instance_nets.finish()?,
            construction_scratch_high_water_bytes:
                opto_core::resident::slice_bytes::<Option<usize>>(row_count)
                    .saturating_add(row_scratch_high_water_bytes),
        })
    }
}
