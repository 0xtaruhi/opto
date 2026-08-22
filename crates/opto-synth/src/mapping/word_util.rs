// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

#[derive(Debug)]
pub(crate) struct GeneratedNames {
    next_instance: u32,
    next_wire: u32,
}

impl GeneratedNames {
    pub(crate) fn new(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        Ok(Self {
            next_instance: next_generated_index(
                module
                    .instances()
                    .iter()
                    .map(|instance| module.name_str(instance.name)),
                "U",
            )?,
            next_wire: next_generated_index(
                module
                    .signals()
                    .iter()
                    .filter_map(|signal| signal.name.map(|name| module.name_str(name))),
                "n",
            )?,
        })
    }

    pub(crate) fn instance(&mut self) -> Result<String, crate::SynthError> {
        let name = take_generated_name(&mut self.next_instance, "U")?;
        Ok(name)
    }

    pub(crate) fn preferred_instance(
        module: &word::WordModule,
        stem: &str,
        suffix: &str,
    ) -> Result<String, crate::SynthError> {
        let base = format!("{stem}{suffix}");
        if module.instance_id(&base).is_none() {
            return Ok(base);
        }
        for index in 1..=u32::MAX {
            let name = format!("{base}_{index}");
            if module.instance_id(&name).is_none() {
                return Ok(name);
            }
        }
        Err(crate::SynthError::invariant(format!(
            "exhausted generated instance names for '{base}'"
        )))
    }

    fn wire(&mut self) -> Result<String, crate::SynthError> {
        take_generated_name(&mut self.next_wire, "n")
    }
}

pub(crate) fn add_generated_wire_value(
    generated_names: &mut GeneratedNames,
    module: &mut word::WordModule,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let name = generated_names.wire()?;
    let signal = module
        .add_wire(
            name,
            word::WordType::bits(1).map_err(crate::SynthError::from)?,
            source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    module
        .read_signal(signal, source.clone())
        .map_err(crate::SynthError::from)
}

fn next_generated_index<'a>(
    names: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Result<u32, crate::SynthError> {
    let max_index = names
        .into_iter()
        .filter_map(|name| name.strip_prefix(prefix)?.parse::<u32>().ok())
        .max();
    match max_index {
        Some(index) => index.checked_add(1).ok_or_else(|| {
            crate::SynthError::invariant(format!("exhausted generated '{prefix}' names"))
        }),
        None => Ok(1),
    }
}

fn take_generated_name(next: &mut u32, prefix: &str) -> Result<String, crate::SynthError> {
    let index = *next;
    *next = index.checked_add(1).ok_or_else(|| {
        crate::SynthError::invariant(format!("exhausted generated '{prefix}' names"))
    })?;
    Ok(format!("{prefix}{index}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_are_allocated_monotonically_after_existing_names() {
        let mut module = word::WordModule::new("top");
        module
            .add_wire(
                "n7",
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .add_instance(
                "U9",
                "existing_cell",
                Vec::new(),
                word::SourceSpan::default(),
            )
            .unwrap();

        let mut names = GeneratedNames::new(&module).unwrap();

        assert_eq!(names.wire().unwrap(), "n8");
        assert_eq!(names.wire().unwrap(), "n9");
        assert_eq!(names.instance().unwrap(), "U10");
        assert_eq!(names.instance().unwrap(), "U11");
    }
}
