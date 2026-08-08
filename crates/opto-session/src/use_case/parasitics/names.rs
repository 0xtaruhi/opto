// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SessionError;
use opto_formats::Spef;
use opto_timing::TimingModel;

pub(super) fn validate_design(
    spef: &Spef,
    model: &TimingModel,
    transform: &NameTransform,
) -> Result<(), SessionError> {
    if transform.path.is_some() || transform.strip_path.is_some() {
        return Ok(());
    }
    if let Some(spef_design) = spef.design.as_deref()
        && canonical_hierarchy(spef_design, spef.divider) != model.design().name()
    {
        return Err(SessionError::state(format!(
            "read_parasitics: SPEF design '{spef_design}' does not match current design '{}'",
            model.design().name()
        )));
    }
    Ok(())
}

pub(super) struct NameTransform {
    path: Option<String>,
    strip_path: Option<String>,
}

impl NameTransform {
    pub(super) fn new(path: Option<&str>, strip_path: Option<&str>) -> Self {
        Self {
            path: path.map(normalize_path),
            strip_path: strip_path.map(normalize_path),
        }
    }

    pub(super) fn apply(&self, name: &str, divider: char) -> Result<String, SessionError> {
        let mut name = canonical_hierarchy(name, divider);
        if let Some(prefix) = self.strip_path.as_deref() {
            name = name
                .strip_prefix(prefix)
                .and_then(|suffix| suffix.strip_prefix('/').or(Some(suffix)))
                .filter(|suffix| !suffix.is_empty())
                .ok_or_else(|| {
                    SessionError::state(format!(
                        "read_parasitics: object '{name}' is outside -strip_path '{prefix}'"
                    ))
                })?
                .to_string();
        }
        if let Some(path) = self.path.as_deref() {
            name = format!("{path}/{name}");
        }
        Ok(name)
    }

    pub(super) fn apply_pin(
        &self,
        node: &str,
        divider: char,
        delimiter: char,
    ) -> Result<String, SessionError> {
        let (instance, pin) = node.rsplit_once(delimiter).ok_or_else(|| {
            SessionError::state(format!(
                "read_parasitics: internal connection '{node}' has no '{delimiter}' pin delimiter"
            ))
        })?;
        Ok(format!("{}/{}", self.apply(instance, divider)?, pin))
    }
}

fn canonical_hierarchy(name: &str, divider: char) -> String {
    if divider == '/' {
        name.to_string()
    } else {
        name.replace(divider, "/")
    }
}

fn normalize_path(path: &str) -> String {
    path.trim_matches('/').replace('.', "/")
}

#[cfg(test)]
mod tests {
    use super::NameTransform;

    #[test]
    fn path_transform_strips_then_adds_dc_hierarchy_prefixes() {
        let transform = NameTransform::new(Some("chip.block"), Some("spef/top"));
        assert_eq!(
            transform.apply("spef|top|u1|n", '|').unwrap(),
            "chip/block/u1/n"
        );
        assert_eq!(
            transform.apply_pin("spef|top|u1:Y", '|', ':').unwrap(),
            "chip/block/u1/Y"
        );
        assert!(transform.apply("another|top|n", '|').is_err());
    }
}
