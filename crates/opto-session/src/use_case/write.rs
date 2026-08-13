// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DesignRecord, Session, SessionError};
use std::io::Write;
use std::path::Path;

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
    /// Write the current source or mapped design as Verilog.
    ///
    /// `hierarchy` includes reachable definitions without flattening their
    /// ownership.
    pub fn write_hdl_file(&self, output: &Path, hierarchy: bool) -> Result<String, SessionError> {
        let current = self
            .state
            .current_design
            .as_ref()
            .ok_or_else(|| SessionError::state("write_hdl: no current design"))?;
        let roots = [current.clone()];
        let modules = self.collect_design_modules("write_hdl", &roots, hierarchy)?;
        super::atomic_file::write_atomically(output, "write_hdl", |file| {
            let mut writer = std::io::BufWriter::new(file);
            self.write_verilog_modules(&mut writer, &modules)?;
            writer.flush().map_err(|source| SessionError::Io {
                operation: "write_hdl",
                path: output.to_path_buf(),
                source,
            })
        })?;
        Ok(format!("Wrote HDL file '{}'", output.display()))
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
