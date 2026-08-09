// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CombinationalCellCatalog, LibraryCover, LibraryCoverBinding, LibraryCoverCell,
    LibraryCoverSource, MappingCost, inverter_truth,
};

impl LibraryCover {
    pub(crate) fn normalize(self, collapse_inverters: bool) -> Result<Self, crate::SynthError> {
        let mut cells = Vec::with_capacity(self.cells.len());
        let mut sources = Vec::<[Option<LibraryCoverSource>; 2]>::with_capacity(self.cells.len());
        for mut cell in self.cells {
            cell.sources = cell
                .sources
                .iter()
                .copied()
                .map(|source| remap_cover_source(source, &sources))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            if collapse_inverters
                && cell.second_truth.is_none()
                && cell.truth == inverter_truth()
                && cell.sources.len() == 1
                && let Some(source) = inverter_input(&cells, cell.sources[0])
            {
                sources.push([Some(source), None]);
                continue;
            }
            let index = cells.len();
            let second = cell
                .second_truth
                .map(|_| LibraryCoverSource::CellSecond(index));
            sources.push([Some(LibraryCoverSource::Cell(index)), second]);
            cells.push(cell);
        }
        let outputs = self
            .outputs
            .iter()
            .copied()
            .map(|source| remap_cover_source(source, &sources))
            .collect::<Result<Vec<_>, _>>()?;
        prune_cover(cells, outputs, self.total_area, self.output_costs)
    }

    /// Gives every published root a distinct physical driver artifact.
    pub(crate) fn isolate_outputs(
        &mut self,
        catalog: &CombinationalCellCatalog,
    ) -> Result<(), crate::SynthError> {
        if self.outputs.len() != self.output_costs.len() {
            return Err(crate::SynthError::invariant(
                "cover outputs and costs have inconsistent lengths",
            ));
        }
        let mut uses = hashbrown::HashMap::new();
        for &source in &self.outputs {
            *uses.entry(source).or_insert(0usize) += 1;
        }
        let requires_driver = |source: &LibraryCoverSource| {
            !matches!(
                *source,
                LibraryCoverSource::Cell(_) | LibraryCoverSource::CellSecond(_)
            ) || uses.get(source) != Some(&1)
        };
        if !self.outputs.iter().any(requires_driver) {
            return Ok(());
        }
        let identity = crate::boolean::logic::identity_truth();
        let inverter = crate::boolean::logic::inverter_truth();
        let (truth, binding, stages) = catalog
            .best_binding_for_truth(identity)
            .map(|binding| (identity, binding, 1))
            .or_else(|| {
                catalog
                    .best_binding_for_truth(inverter)
                    .map(|binding| (inverter, binding, 2))
            })
            .ok_or_else(|| {
                crate::SynthError::mapping(
                    "target library cannot isolate regional output obligations",
                )
            })?;
        let mut cells = std::mem::take(&mut self.cells).into_vec();
        let mut outputs = std::mem::take(&mut self.outputs).into_vec();
        let mut output_costs = std::mem::take(&mut self.output_costs).into_vec();
        let cost = catalog.cost_for_binding(binding);
        for (index, source) in outputs.iter_mut().enumerate() {
            if !requires_driver(source) {
                continue;
            }
            for _ in 0..stages {
                let cell = cells.len();
                cells.push(LibraryCoverCell {
                    second_node: None,
                    binding: LibraryCoverBinding::Single(binding),
                    binding_identity: catalog.binding_identity(binding).into_boxed_slice(),
                    truth,
                    second_truth: None,
                    sources: Box::new([*source]),
                });
                *source = LibraryCoverSource::Cell(cell);
                self.total_area += cost.area;
                output_costs[index] = output_costs[index].cell(cost);
            }
        }
        self.cells = cells.into_boxed_slice();
        self.outputs = outputs.into_boxed_slice();
        self.output_costs = output_costs.into_boxed_slice();
        Ok(())
    }
}

fn remap_cover_source(
    source: LibraryCoverSource,
    cells: &[[Option<LibraryCoverSource>; 2]],
) -> Result<LibraryCoverSource, crate::SynthError> {
    match source {
        LibraryCoverSource::Constant(_) | LibraryCoverSource::Input(_) => Ok(source),
        LibraryCoverSource::Cell(index) => cells
            .get(index)
            .and_then(|outputs| outputs[0])
            .ok_or_else(|| crate::SynthError::invariant("cover source has no primary output")),
        LibraryCoverSource::CellSecond(index) => cells
            .get(index)
            .and_then(|outputs| outputs[1])
            .ok_or_else(|| crate::SynthError::invariant("cover source has no secondary output")),
    }
}

fn inverter_input(
    cells: &[LibraryCoverCell],
    source: LibraryCoverSource,
) -> Option<LibraryCoverSource> {
    let (index, second) = match source {
        LibraryCoverSource::Cell(index) => (index, false),
        LibraryCoverSource::CellSecond(index) => (index, true),
        LibraryCoverSource::Constant(_) | LibraryCoverSource::Input(_) => return None,
    };
    let cell = cells.get(index)?;
    let truth = if second {
        cell.second_truth?
    } else {
        cell.truth
    };
    (truth == inverter_truth() && cell.sources.len() == 1).then_some(cell.sources[0])
}

fn prune_cover(
    cells: Vec<LibraryCoverCell>,
    outputs: Vec<LibraryCoverSource>,
    total_area: f64,
    output_costs: Box<[MappingCost]>,
) -> Result<LibraryCover, crate::SynthError> {
    let mut live = vec![false; cells.len()];
    let mut pending = outputs
        .iter()
        .filter_map(|source| cover_source_cell(*source))
        .collect::<Vec<_>>();
    while let Some(index) = pending.pop() {
        let present = live.get_mut(index).ok_or_else(|| {
            crate::SynthError::invariant("cover output references an unknown cell")
        })?;
        if *present {
            continue;
        }
        *present = true;
        pending.extend(
            cells
                .get(index)
                .ok_or_else(|| {
                    crate::SynthError::invariant("live cover references an unknown cell")
                })?
                .sources
                .iter()
                .filter_map(|source| cover_source_cell(*source)),
        );
    }
    let mut remap = vec![None; cells.len()];
    let mut next = 0usize;
    for (index, &is_live) in live.iter().enumerate() {
        if is_live {
            remap[index] = Some(next);
            next += 1;
        }
    }
    let mut retained = Vec::with_capacity(next);
    for (index, mut cell) in cells.into_iter().enumerate() {
        if !live[index] {
            continue;
        }
        cell.sources = cell
            .sources
            .iter()
            .copied()
            .map(|source| remap_live_source(source, &remap))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        retained.push(cell);
    }
    let outputs = outputs
        .into_iter()
        .map(|source| remap_live_source(source, &remap))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LibraryCover {
        cells: retained.into_boxed_slice(),
        outputs: outputs.into_boxed_slice(),
        total_area,
        output_costs,
    })
}

fn cover_source_cell(source: LibraryCoverSource) -> Option<usize> {
    match source {
        LibraryCoverSource::Cell(index) | LibraryCoverSource::CellSecond(index) => Some(index),
        LibraryCoverSource::Constant(_) | LibraryCoverSource::Input(_) => None,
    }
}

fn remap_live_source(
    source: LibraryCoverSource,
    cells: &[Option<usize>],
) -> Result<LibraryCoverSource, crate::SynthError> {
    match source {
        LibraryCoverSource::Cell(index) => Ok(LibraryCoverSource::Cell(
            cells.get(index).copied().flatten().ok_or_else(|| {
                crate::SynthError::invariant("live cover references a removed cell")
            })?,
        )),
        LibraryCoverSource::CellSecond(index) => Ok(LibraryCoverSource::CellSecond(
            cells.get(index).copied().flatten().ok_or_else(|| {
                crate::SynthError::invariant("live cover references a removed cell")
            })?,
        )),
        LibraryCoverSource::Constant(_) | LibraryCoverSource::Input(_) => Ok(source),
    }
}
