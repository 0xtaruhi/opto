// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::timing_model;
use crate::{Session, SessionError};
use opto_formats::{AreaCellKind, AreaLibrary, AreaReportContext};
use opto_synth::target_cell_is_buffer_or_inverter;
use opto_timing::ReportTimingOptions;

fn source_is_timing_model_compatible(
    module: &opto_ir::word::WordModule,
    library: &opto_library::TimingLibrary,
) -> bool {
    module.operations().is_empty()
        && module.instances().iter().all(|instance| {
            let cell_name = module.name_str(instance.module);
            let mut matches = library.cells.iter().filter(|cell| cell.name() == cell_name);
            let Some(cell) = matches.next() else {
                return false;
            };
            matches.next().is_none()
                && instance.connections.iter().all(|connection| {
                    let port = module.name_str(connection.port);
                    cell.pins().any(|pin| pin.name() == port)
                })
        })
}

fn area_report_context(session: &Session) -> Result<AreaReportContext, SessionError> {
    let selection = session.resolution_library_selection();
    if selection.is_empty() {
        return Ok(AreaReportContext::default());
    }

    let mut context = AreaReportContext::default();
    let revision = session.process.libraries.current();
    context.libraries = revision
        .selected_libraries(&selection)?
        .into_iter()
        .map(|library| AreaLibrary {
            name: library.name,
            source: library.source,
        })
        .collect();
    let cells = revision.target_cells(&selection)?;
    for cell in cells.iter() {
        // Only characterized cells enter the map. An absent entry then means
        // "no area characterization" for both unknown and uncharacterized
        // references, which the report surfaces instead of scoring as zero.
        if let Some(area) = cell.area() {
            context
                .library_cell_area
                .insert(cell.name().to_string(), area);
        }
        context.library_cell_kind.insert(
            cell.name().to_string(),
            if cell.sequential().next().is_some() {
                AreaCellKind::Sequential
            } else if target_cell_is_buffer_or_inverter(cell) {
                AreaCellKind::BufferInverter
            } else {
                AreaCellKind::Combinational
            },
        );
    }
    Ok(context)
}

fn hierarchical_mapped_artifacts<'a>(
    session: &'a Session,
    command: &str,
) -> Result<Option<Vec<(&'a opto_ir::mapped::MappedNetlist, u64)>>, SessionError> {
    if session
        .current_record()?
        .synthesized
        .as_ref()
        .is_some_and(|synthesis| synthesis.mapped().design_instance_count() == 0)
    {
        return Ok(None);
    }
    let graph = session.definition_graph(command)?;
    if graph.postorder().len() == 1 {
        return Ok(None);
    }
    let mut modules = Vec::with_capacity(graph.postorder().len());
    for &definition in graph.postorder() {
        let name = graph.definition_name(definition);
        let Some(synthesis) = session
            .state
            .designs
            .get(name)
            .and_then(|record| record.synthesized.as_ref())
        else {
            return Ok(None);
        };
        modules.push((synthesis.mapped(), graph.occurrence_count(definition)));
    }
    Ok(Some(modules))
}
impl Session {
    /// Render area for the current source or mapped artifact.
    pub fn report_area(&self) -> Result<String, SessionError> {
        let record = self.current_record()?;
        let context = area_report_context(self)?;
        let report = match record.synthesized.as_ref() {
            Some(synthesis) => match hierarchical_mapped_artifacts(self, "report_area")? {
                Some(modules) => opto_formats::report_hierarchical_mapped_area(
                    synthesis.mapped(),
                    &modules,
                    &context,
                ),
                None => opto_formats::report_mapped_area(synthesis.mapped(), &context),
            },
            None => opto_formats::report_area(record.source.word(), &context),
        };
        Ok(report.render_plain())
    }

    /// Render the compact `QoR` summary for the current design.
    pub fn report_qor(&self) -> Result<String, SessionError> {
        let record = self.current_record()?;
        let context = area_report_context(self)?;
        let timing_library = self.timing_library()?;
        let timing = if !self.state.timing.has_path_constraints()
            || timing_library
                .cells
                .iter()
                .all(|cell| cell.pins().all(|pin| pin.timing_arcs().next().is_none()))
            || (record.synthesized.is_none()
                && !source_is_timing_model_compatible(record.source.word(), &timing_library))
        {
            None
        } else {
            let model = timing_model::current_timing_model(self)?;
            let generation = model.generation();
            let options = ReportTimingOptions {
                checks: opto_timing::ScenarioCheckSet::SETUP,
                ..ReportTimingOptions::default()
            };
            match self
                .process
                .timing_engine
                .quality(&self.state.timing, &model, &options)
            {
                Ok(timing) => {
                    if timing.generation() != generation {
                        return Err(SessionError::state(
                            "report_qor: timing result belongs to a different sealed generation",
                        ));
                    }
                    Some(timing)
                }
                Err(opto_timing::TimingError::Analysis(
                    opto_timing::TimingAnalysisError::NoTimingPaths,
                )) => None,
                Err(error) => return Err(error.into()),
            }
        };
        let report = match record.synthesized.as_ref() {
            Some(synthesis) => match hierarchical_mapped_artifacts(self, "report_qor")? {
                Some(modules) => opto_formats::report_hierarchical_mapped_qor(
                    synthesis.mapped(),
                    &modules,
                    &context,
                    timing.as_ref(),
                ),
                None => {
                    opto_formats::report_mapped_qor(synthesis.mapped(), &context, timing.as_ref())
                }
            },
            None => opto_formats::report_qor(record.source.word(), &context, timing.as_ref()),
        };
        Ok(report.render_plain())
    }

    /// Render implementation and sharing provenance for selected designs.
    pub fn report_resources(
        &self,
        designs: &[String],
        hierarchy: bool,
    ) -> Result<String, SessionError> {
        let roots = if designs.is_empty() {
            vec![self.current_design_name()?.to_string()]
        } else {
            for design in designs {
                if !self.state.designs.contains_key(design) {
                    return Err(SessionError::state(format!(
                        "report_resources: design '{design}' not found"
                    )));
                }
            }
            designs.to_vec()
        };
        let modules = self.collect_source_design_modules("report_resources", &roots, hierarchy)?;
        let reports = modules
        .iter()
        .map(|name| -> Result<_, SessionError> {
            let design = self.state.designs.get(name).ok_or_else(|| {
                SessionError::state(format!(
                    "report_resources: design '{name}' is missing from store"
                ))
            })?;
            let implementations = design
                .synthesized
                .as_ref()
                .map(|synthesis| {
                    synthesis
                        .implementation_db()
                        .regions()
                    .iter()
                    .map(|region| {
                        let source_file = region.source_file().ok_or_else(|| {
                            SessionError::state(format!(
                                "report_resources: source operation {} has no source file",
                                region.operator().raw()
                            ))
                        })?;
                        let source_line = region.source_line().ok_or_else(|| {
                            SessionError::state(format!(
                                "report_resources: source operation {} has no source line",
                                region.operator().raw()
                            ))
                        })?;
                        let source_lines = region
                            .source_lines()
                            .iter()
                            .map(|line| {
                                line.ok_or_else(|| {
                                    SessionError::state(
                                        "report_resources: shared source operation has no source line",
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(opto_formats::ResourceImplementationEntry {
                            number: region.id().raw() + 1,
                            module: region.module_name().to_string(),
                            width: region.width(),
                            operation_mnemonic: region.operation_mnemonic().to_string(),
                            source_file: source_file.to_string(),
                            source_line,
                            source_lines,
                            implementation: region.implementation_name().to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, SessionError>>()
                })
                .transpose()?;
            Ok(opto_formats::ResourceReportEntry {
                design: design.source.word().name().to_string(),
                implementations,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
        Ok(opto_formats::report_resources(&reports).render_plain())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_rtl_operations_are_not_timing_model_compatible() {
        let mut module = opto_ir::word::WordModule::new("leaf_rtl");
        let port = module
            .add_port(
                "a",
                opto_ir::word::PortDirection::Input,
                opto_ir::word::WordType::bits(1).unwrap(),
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .read_signal(
                module.port(port).unwrap().signal,
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();
        module
            .unary(
                opto_ir::word::UnaryOp::BitNot,
                input,
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();

        assert!(!source_is_timing_model_compatible(
            &module,
            &opto_library::TimingLibrary::default(),
        ));
    }
}
