// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Session, design_view::CellView};
use opto_db::{Direction, ResolvedObject};
use opto_ir::word::{AnnotationTarget, InstId, SignalId, SynthesisDirectiveKind};

impl Session {
    pub(in crate::objects) fn object_attribute(
        &self,
        object: ResolvedObject<'_>,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        match object {
            ResolvedObject::Design { name } => self.design_attribute(name, attribute),
            ResolvedObject::Port { design, name } => self.port_attribute(design, name, attribute),
            ResolvedObject::Cell { design, name } => self.cell_attribute(design, name, attribute),
            ResolvedObject::Pin {
                design,
                cell,
                name,
                full_name,
            } => self.pin_attribute(design, cell, name, full_name, attribute),
            ResolvedObject::Net { design, name } => self.net_attribute(design, name, attribute),
            ResolvedObject::Clock { name } => self.clock_attribute(name, attribute),
        }
    }

    pub(in crate::objects) fn design_attribute(
        &self,
        name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let record = self.state.designs.get(name).ok_or_else(|| {
            crate::SessionError::state(format!("design '{name}' is missing from design store"))
        })?;
        Ok(match attribute {
            "name" | "full_name" => name.to_string(),
            "object_class" => "design".to_string(),
            "dont_touch" => {
                directive_value(record.source.word().synthesis_directive(
                    AnnotationTarget::Module,
                    SynthesisDirectiveKind::DontTouch,
                ))
            }
            "ungroup" => directive_value(
                record
                    .source
                    .word()
                    .synthesis_directive(AnnotationTarget::Module, SynthesisDirectiveKind::Ungroup),
            ),
            _ => String::new(),
        })
    }

    pub(in crate::objects) fn port_attribute(
        &self,
        design_name: &str,
        port_name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let design = self.design_by_name(design_name)?;
        let port = design.port_by_name(port_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "port '{port_name}' is missing from design '{design_name}'"
            ))
        })?;
        Ok(match attribute {
            "name" | "full_name" => port.name.to_string(),
            "object_class" => "port".to_string(),
            "direction" => direction_value(port.direction).to_string(),
            "bit_width" => port.width.to_string(),
            _ => String::new(),
        })
    }

    pub(in crate::objects) fn cell_attribute(
        &self,
        design_name: &str,
        cell_name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let design = self.design_by_name(design_name)?;
        let cell = design.cell_by_name(cell_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "cell '{cell_name}' is missing from design '{design_name}'"
            ))
        })?;
        Ok(match attribute {
            "name" | "full_name" => cell.name.to_string(),
            "object_class" => "cell".to_string(),
            "ref_name" => cell.reference.to_string(),
            "dont_touch" => self.source_instance_directive(
                design_name,
                cell_name,
                SynthesisDirectiveKind::DontTouch,
            )?,
            "ungroup" => self.source_instance_directive(
                design_name,
                cell_name,
                SynthesisDirectiveKind::Ungroup,
            )?,
            _ => String::new(),
        })
    }

    pub(in crate::objects) fn pin_attribute(
        &self,
        design_name: &str,
        cell_name: &str,
        pin_name: &str,
        full_name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let design = self.design_by_name(design_name)?;
        let cell = design.cell_by_name(cell_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "cell '{cell_name}' is missing from design '{design_name}'"
            ))
        })?;
        let declared = self
            .state
            .designs
            .get(cell.reference)
            .map(crate::DesignView::from_record)
            .is_some_and(|reference| reference.port_by_name(pin_name).is_some());
        if cell.connection_by_name(pin_name).is_none() && !declared {
            return Err(crate::SessionError::state(format!(
                "pin '{full_name}' is missing from design '{design_name}'"
            )));
        }
        Ok(match attribute {
            "name" => pin_name.to_string(),
            "full_name" => full_name.to_string(),
            "object_class" => "pin".to_string(),
            "direction" => self.pin_direction(cell, pin_name),
            _ => String::new(),
        })
    }

    pub(in crate::objects) fn pin_direction(&self, cell: CellView<'_>, pin_name: &str) -> String {
        self.state
            .designs
            .get(cell.reference)
            .map(crate::DesignView::from_record)
            .and_then(|reference| {
                reference
                    .port_by_name(pin_name)
                    .map(|port| direction_value(port.direction).to_string())
            })
            .unwrap_or_default()
    }

    pub(in crate::objects) fn net_attribute(
        &self,
        design_name: &str,
        net_name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let design = self.design_by_name(design_name)?;
        let width = Self::net_width(design, net_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "net '{net_name}' is missing from design '{design_name}'"
            ))
        })?;
        Ok(match attribute {
            "name" | "full_name" => net_name.to_string(),
            "object_class" => "net".to_string(),
            "bit_width" => width.to_string(),
            "dont_touch" => self.source_signal_directive(
                design_name,
                net_name,
                SynthesisDirectiveKind::DontTouch,
            )?,
            _ => String::new(),
        })
    }

    pub(in crate::objects) fn clock_attribute(
        &self,
        clock_name: &str,
        attribute: &str,
    ) -> Result<String, crate::SessionError> {
        let clock = self
            .state
            .timing
            .clocks()
            .iter()
            .find(|clock| clock.name == clock_name)
            .ok_or_else(|| {
                crate::SessionError::state(format!(
                    "clock '{clock_name}' is missing from timing context"
                ))
            })?;
        Ok(match attribute {
            "name" | "full_name" => clock.name.clone(),
            "object_class" => "clock".to_string(),
            "period" => format_float(clock.period),
            "sources" => clock
                .sources
                .iter()
                .filter_map(|id| self.state.objects.resolve(id.erase()))
                .map(ResolvedObject::object_name)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        })
    }

    fn source_instance_directive(
        &self,
        design_name: &str,
        cell_name: &str,
        kind: SynthesisDirectiveKind,
    ) -> Result<String, crate::SessionError> {
        let record = self.state.designs.get(design_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "design '{design_name}' is missing from design store"
            ))
        })?;
        if record.mapped_object_index.is_some() {
            return Ok(String::new());
        }
        let word = record.source.word();
        let target = word
            .instances()
            .iter()
            .enumerate()
            .find(|(_, instance)| word.name_str(instance.name) == cell_name)
            .map(|(index, _)| InstId::from_index(index))
            .transpose()?
            .map(AnnotationTarget::Instance);
        Ok(target.map_or_else(String::new, |target| {
            directive_value(word.synthesis_directive(target, kind))
        }))
    }

    fn source_signal_directive(
        &self,
        design_name: &str,
        net_name: &str,
        kind: SynthesisDirectiveKind,
    ) -> Result<String, crate::SessionError> {
        let record = self.state.designs.get(design_name).ok_or_else(|| {
            crate::SessionError::state(format!(
                "design '{design_name}' is missing from design store"
            ))
        })?;
        if record.mapped_object_index.is_some() {
            return Ok(String::new());
        }
        let word = record.source.word();
        let target = word
            .signals()
            .iter()
            .enumerate()
            .find(|(_, signal)| {
                signal
                    .name
                    .is_some_and(|name| word.name_str(name) == net_name)
            })
            .map(|(index, _)| SignalId::from_index(index))
            .transpose()?
            .map(AnnotationTarget::Signal);
        Ok(target.map_or_else(String::new, |target| {
            directive_value(word.synthesis_directive(target, kind))
        }))
    }
}

fn directive_value(value: Option<bool>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn direction_value(direction: Direction) -> &'static str {
    match direction {
        Direction::Input => "in",
        Direction::Output => "out",
        Direction::Inout => "inout",
        Direction::Ref => "ref",
    }
}

fn format_float(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}
