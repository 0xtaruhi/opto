// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DesignRecord, Session, SessionError};
use std::io::Write;
use std::path::PathBuf;

fn resolve_write_designs(
    session: &Session,
    designs: &[String],
) -> Result<Vec<String>, SessionError> {
    if designs.is_empty() {
        let current =
            session.state.current_design.as_ref().ok_or_else(|| {
                SessionError::state("write_hdl: no files or designs were specified")
            })?;
        return Ok(vec![current.clone()]);
    }

    for design in designs {
        if !session.state.designs.contains_key(design) {
            return Err(SessionError::state(format!(
                "write_hdl: design '{design}' not found"
            )));
        }
    }
    Ok(designs.to_vec())
}

fn write_verilog_module<W: Write>(
    output: &mut W,
    module: &DesignRecord,
) -> Result<(), SessionError> {
    match module.synthesized.as_ref() {
        Some(synthesis) => opto_formats::write_mapped_verilog(output, synthesis.mapped())?,
        None => opto_formats::write_verilog(output, module.source.word())?,
    }
    Ok(())
}
impl Session {
    /// Write selected source or mapped designs as Verilog.
    ///
    /// `hierarchy` includes reachable definitions without flattening their
    /// ownership; an empty `designs` slice selects the current design.
    pub fn write_hdl_file(
        &self,
        output: Option<PathBuf>,
        designs: &[String],
        hierarchy: bool,
    ) -> Result<String, SessionError> {
        let roots = resolve_write_designs(self, designs)?;
        let modules = self.collect_design_modules("write_hdl", &roots, hierarchy)?;

        if let Some(path) = output {
            super::atomic_file::write_atomically(&path, "write_hdl", |file| {
                let mut writer = std::io::BufWriter::new(file);
                self.write_verilog_modules(&mut writer, &modules)?;
                writer.flush().map_err(|source| SessionError::Io {
                    operation: "write_hdl",
                    path: path.clone(),
                    source,
                })
            })?;
            return Ok(format!("Wrote HDL file '{}'", path.display()));
        }

        let mut rendered = Vec::with_capacity(modules.len());
        for module_name in modules {
            let path = PathBuf::from(format!("{module_name}.v"));
            let module = self.state.designs.get(&module_name).ok_or_else(|| {
                SessionError::state(format!(
                    "write_hdl: design '{module_name}' is missing from store"
                ))
            })?;
            let mut contents = Vec::new();
            write_verilog_module(&mut contents, module)?;
            rendered.push((path, contents));
        }

        let mut messages = Vec::with_capacity(rendered.len());
        for (path, contents) in rendered {
            super::atomic_file::write_atomically(&path, "write_hdl", |file| {
                file.write_all(&contents)
                    .map_err(|source| SessionError::Io {
                        operation: "write_hdl",
                        path: path.clone(),
                        source,
                    })
            })?;
            messages.push(format!("Wrote HDL file '{}'", path.display()));
        }
        Ok(messages.join("\n"))
    }

    pub(crate) fn write_verilog_modules<W: Write>(
        &self,
        output: &mut W,
        modules: &[String],
    ) -> Result<(), SessionError> {
        for (index, name) in modules.iter().enumerate() {
            let module = self.state.designs.get(name).ok_or_else(|| {
                SessionError::state(format!("write_hdl: design '{name}' is missing from store"))
            })?;
            if index != 0 {
                output
                    .write_all(b"\n")
                    .map_err(opto_formats::FormatError::from)?;
            }
            write_verilog_module(output, module)?;
        }
        Ok(())
    }
}
