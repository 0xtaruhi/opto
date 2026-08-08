// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! A strict, unit-normalizing subset of the SPEF parasitic format.
//!
//! Parsed capacitance and resistance are converted to SI units immediately.
//! Name-map references are resolved during parsing, so downstream timing code
//! receives self-contained net records.

use crate::FormatError;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
/// Parsed SPEF document.
pub struct Spef {
    /// Optional design name from `*DESIGN`.
    pub design: Option<String>,
    /// Hierarchy separator declared by `*DIVIDER`.
    pub divider: char,
    /// Pin delimiter declared by `*DELIMITER`.
    pub delimiter: char,
    /// Parasitic nets in input order.
    pub nets: Vec<SpefNet>,
}

#[derive(Debug, Clone, PartialEq)]
/// One distributed parasitic network.
pub struct SpefNet {
    /// Resolved net name.
    pub name: String,
    /// Declared total capacitance normalized to farads.
    pub total_capacitance_farads: f64,
    /// External ports and internal instance pins on the net.
    pub connections: Vec<SpefConnection>,
    /// Grounded and coupling capacitances.
    pub capacitors: Vec<SpefCapacitor>,
    /// Point-to-point resistances.
    pub resistors: Vec<SpefResistor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A port or internal pin attached to a parasitic net.
pub struct SpefConnection {
    /// Resolved node name.
    pub node: String,
    /// Whether the node is a design port or an internal pin.
    pub kind: SpefConnectionKind,
    /// Signal direction declared for the node.
    pub direction: SpefDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Namespace occupied by a SPEF connection.
pub enum SpefConnectionKind {
    /// Top-level design port.
    Port,
    /// Internal instance pin.
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Signal direction attached to a SPEF connection.
pub enum SpefDirection {
    /// Input connection.
    Input,
    /// Output connection.
    Output,
    /// Bidirectional connection.
    Inout,
}

#[derive(Debug, Clone, PartialEq)]
/// Grounded or coupling capacitance.
pub struct SpefCapacitor {
    /// Resolved first endpoint of the capacitance record.
    pub first: String,
    /// Second endpoint, or `None` for capacitance to ground.
    pub second: Option<String>,
    /// Capacitance normalized to farads.
    pub capacitance_farads: f64,
}

#[derive(Debug, Clone, PartialEq)]
/// Resistance between two parasitic nodes.
pub struct SpefResistor {
    /// Resolved first endpoint of the resistance record.
    pub first: String,
    /// Resolved second endpoint of the resistance record.
    pub second: String,
    /// Resistance normalized to ohms.
    pub resistance_ohms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    NameMap,
    Connections,
    Capacitances,
    Resistances,
}

/// Parse SPEF text and normalize supported physical units to SI.
///
/// The parser requires unit declarations before the first distributed net and
/// reports the first malformed record with a one-based line number.
///
/// # Errors
///
/// Returns [`FormatError::Spef`] for malformed syntax, unresolved name-map
/// references, invalid section records, or missing unit declarations.
#[allow(
    clippy::too_many_lines,
    reason = "the single-pass SPEF state machine keeps directive precedence and line diagnostics local"
)]
pub fn parse_spef(text: &str) -> Result<Spef, FormatError> {
    let mut design = None;
    let mut name_map = BTreeMap::new();
    let mut divider = '/';
    let mut delimiter = ':';
    let mut capacitance_scale = None;
    let mut resistance_scale = None;
    let mut section = Section::Header;
    let mut nets = Vec::new();
    let mut current = None;

    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let fields = split_fields(raw_line, line_number)?;
        if fields.is_empty() {
            continue;
        }
        // Section markers update the interpretation of subsequent numeric
        // records. Named directives remain unambiguous and are handled before
        // the section-dependent fallback later in the parser.
        match fields[0].as_str() {
            "*DESIGN" => {
                design = Some(required_field(&fields, 1, line_number, "design name")?.to_string());
            }
            "*DIVIDER" => {
                divider = parse_separator(&fields, line_number, "divider")?;
            }
            "*DELIMITER" => {
                delimiter = parse_separator(&fields, line_number, "delimiter")?;
            }
            "*C_UNIT" => {
                capacitance_scale = Some(parse_unit(
                    &fields,
                    line_number,
                    "capacitance",
                    &[
                        ("F", 1.0),
                        ("MF", 1e-3),
                        ("UF", 1e-6),
                        ("NF", 1e-9),
                        ("PF", 1e-12),
                        ("FF", 1e-15),
                    ],
                )?);
            }
            "*R_UNIT" => {
                resistance_scale = Some(parse_unit(
                    &fields,
                    line_number,
                    "resistance",
                    &[("OHM", 1.0), ("KOHM", 1e3), ("MOHM", 1e6)],
                )?);
            }
            "*NAME_MAP" => section = Section::NameMap,
            "*PORTS" => section = Section::Header,
            "*D_NET" => {
                finish_net(&mut current, &mut nets);
                let scale = capacitance_scale.ok_or_else(|| {
                    FormatError::spef(line_number, "*C_UNIT must precede the first *D_NET")
                })?;
                let name = resolve_name(
                    required_field(&fields, 1, line_number, "net name")?,
                    &name_map,
                    line_number,
                )?;
                let total = parse_scaled_positive_number(
                    required_field(&fields, 2, line_number, "total capacitance")?,
                    line_number,
                    "total capacitance",
                    true,
                    scale,
                )?;
                current = Some(SpefNet {
                    name,
                    total_capacitance_farads: total,
                    connections: Vec::new(),
                    capacitors: Vec::new(),
                    resistors: Vec::new(),
                });
                section = Section::Header;
            }
            "*CONN" => {
                require_net(current.as_ref(), line_number, "*CONN")?;
                section = Section::Connections;
            }
            "*CAP" => {
                require_net(current.as_ref(), line_number, "*CAP")?;
                section = Section::Capacitances;
            }
            "*RES" => {
                require_net(current.as_ref(), line_number, "*RES")?;
                section = Section::Resistances;
            }
            "*END" => {
                finish_net(&mut current, &mut nets);
                section = Section::Header;
            }
            token if section == Section::NameMap && is_name_map_key(token) => {
                let value = required_field(&fields, 1, line_number, "name-map value")?;
                if name_map
                    .insert(token.to_string(), value.to_string())
                    .is_some()
                {
                    return Err(FormatError::spef(
                        line_number,
                        format!("duplicate name-map key '{token}'"),
                    ));
                }
            }
            "*P" | "*I" if section == Section::Connections => {
                let net = current.as_mut().ok_or_else(|| {
                    FormatError::spef(line_number, "connection appears outside a *D_NET block")
                })?;
                let kind = if fields[0] == "*P" {
                    SpefConnectionKind::Port
                } else {
                    SpefConnectionKind::Internal
                };
                let node = resolve_name(
                    required_field(&fields, 1, line_number, "connection node")?,
                    &name_map,
                    line_number,
                )?;
                let direction =
                    match required_field(&fields, 2, line_number, "connection direction")? {
                        "I" => SpefDirection::Input,
                        "O" => SpefDirection::Output,
                        "B" => SpefDirection::Inout,
                        value => {
                            return Err(FormatError::spef(
                                line_number,
                                format!("unsupported connection direction '{value}'"),
                            ));
                        }
                    };
                net.connections.push(SpefConnection {
                    node,
                    kind,
                    direction,
                });
            }
            token if section == Section::Capacitances && !token.starts_with('*') => {
                let scale = capacitance_scale.ok_or_else(|| {
                    FormatError::spef(line_number, "*C_UNIT must precede the *CAP section")
                })?;
                let net = current.as_mut().ok_or_else(|| {
                    FormatError::spef(line_number, "capacitor appears outside a *D_NET block")
                })?;
                let first = resolve_name(
                    required_field(&fields, 1, line_number, "capacitor node")?,
                    &name_map,
                    line_number,
                )?;
                let (second, value_index) = match fields.len() {
                    3 => (None, 2),
                    4 => (Some(resolve_name(&fields[2], &name_map, line_number)?), 3),
                    _ => {
                        return Err(FormatError::spef(
                            line_number,
                            "capacitor must be '<id> <node> <value>' or '<id> <node> <node> <value>'",
                        ));
                    }
                };
                let capacitance_farads = parse_scaled_positive_number(
                    &fields[value_index],
                    line_number,
                    "capacitance",
                    true,
                    scale,
                )?;
                net.capacitors.push(SpefCapacitor {
                    first,
                    second,
                    capacitance_farads,
                });
            }
            token if section == Section::Resistances && !token.starts_with('*') => {
                let scale = resistance_scale.ok_or_else(|| {
                    FormatError::spef(line_number, "*R_UNIT must precede the *RES section")
                })?;
                if fields.len() != 4 {
                    return Err(FormatError::spef(
                        line_number,
                        "resistor must be '<id> <node> <node> <value>'",
                    ));
                }
                let resistance_ohms = parse_scaled_positive_number(
                    &fields[3],
                    line_number,
                    "resistance",
                    false,
                    scale,
                )?;
                current
                    .as_mut()
                    .ok_or_else(|| {
                        FormatError::spef(line_number, "resistor appears outside a *D_NET block")
                    })?
                    .resistors
                    .push(SpefResistor {
                        first: resolve_name(&fields[1], &name_map, line_number)?,
                        second: resolve_name(&fields[2], &name_map, line_number)?,
                        resistance_ohms,
                    });
            }
            token if token.starts_with('*') => {}
            token => {
                return Err(FormatError::spef(
                    line_number,
                    format!("unexpected token '{token}' in {section:?} section"),
                ));
            }
        }
    }
    finish_net(&mut current, &mut nets);
    if capacitance_scale.is_none() {
        return Err(FormatError::spef(1, "missing *C_UNIT declaration"));
    }
    if resistance_scale.is_none() && nets.iter().any(|net| !net.resistors.is_empty()) {
        return Err(FormatError::spef(1, "missing *R_UNIT declaration"));
    }
    Ok(Spef {
        design,
        divider,
        delimiter,
        nets,
    })
}

fn parse_separator(fields: &[String], line: usize, description: &str) -> Result<char, FormatError> {
    let raw = required_field(fields, 1, line, description)?;
    let mut characters = raw.chars();
    let separator = characters
        .next()
        .ok_or_else(|| FormatError::spef(line, format!("empty {description}")))?;
    if characters.next().is_some() {
        return Err(FormatError::spef(
            line,
            format!("{description} must be one character"),
        ));
    }
    Ok(separator)
}

fn finish_net(current: &mut Option<SpefNet>, nets: &mut Vec<SpefNet>) {
    if let Some(net) = current.take() {
        nets.push(net);
    }
}

fn require_net(current: Option<&SpefNet>, line: usize, section: &str) -> Result<(), FormatError> {
    if current.is_some() {
        Ok(())
    } else {
        Err(FormatError::spef(
            line,
            format!("{section} appears outside a *D_NET block"),
        ))
    }
}

fn is_name_map_key(token: &str) -> bool {
    token.strip_prefix('*').is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn resolve_name(
    token: &str,
    names: &BTreeMap<String, String>,
    line: usize,
) -> Result<String, FormatError> {
    let Some(suffix) = token.strip_prefix('*') else {
        return Ok(token.to_string());
    };
    let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Ok(token.to_string());
    }
    let key = format!("*{}", &suffix[..digits]);
    let base = names
        .get(&key)
        .ok_or_else(|| FormatError::spef(line, format!("unresolved name-map reference '{key}'")))?;
    Ok(format!("{base}{}", &suffix[digits..]))
}

fn required_field<'a>(
    fields: &'a [String],
    index: usize,
    line: usize,
    description: &str,
) -> Result<&'a str, FormatError> {
    fields
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| FormatError::spef(line, format!("missing {description}")))
}

fn parse_unit(
    fields: &[String],
    line: usize,
    dimension: &str,
    units: &[(&str, f64)],
) -> Result<f64, FormatError> {
    let factor = parse_positive_number(
        required_field(fields, 1, line, "unit multiplier")?,
        line,
        "unit multiplier",
        false,
    )?;
    let unit = required_field(fields, 2, line, "unit name")?.to_ascii_uppercase();
    let scale = units
        .iter()
        .find_map(|(name, scale)| (*name == unit).then_some(*scale))
        .ok_or_else(|| FormatError::spef(line, format!("unsupported {dimension} unit '{unit}'")))?;
    scale_finite(factor, scale, line, &format!("{dimension} unit"))
}

fn parse_scaled_positive_number(
    raw: &str,
    line: usize,
    description: &str,
    allow_zero: bool,
    scale: f64,
) -> Result<f64, FormatError> {
    let value = parse_positive_number(raw, line, description, allow_zero)?;
    scale_finite(value, scale, line, description)
}

fn scale_finite(
    value: f64,
    scale: f64,
    line: usize,
    description: &str,
) -> Result<f64, FormatError> {
    let scaled = value * scale;
    if scaled.is_finite() {
        Ok(scaled)
    } else {
        Err(FormatError::spef(
            line,
            format!("scaled {description} exceeds the finite numeric range"),
        ))
    }
}

fn parse_positive_number(
    raw: &str,
    line: usize,
    description: &str,
    allow_zero: bool,
) -> Result<f64, FormatError> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| FormatError::spef(line, format!("invalid {description} value '{raw}'")))?;
    if value.is_finite() && (value > 0.0 || allow_zero && value == 0.0) {
        Ok(value)
    } else {
        Err(FormatError::spef(
            line,
            format!(
                "{description} must be {} and finite",
                if allow_zero {
                    "nonnegative"
                } else {
                    "positive"
                }
            ),
        ))
    }
}

fn split_fields(line: &str, line_number: usize) -> Result<Vec<String>, FormatError> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            field.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '/' if !quoted && field == "/" => {
                field.clear();
                break;
            }
            character if character.is_whitespace() && !quoted => {
                if !field.is_empty() {
                    fields.push(std::mem::take(&mut field));
                }
            }
            _ => field.push(character),
        }
    }
    if quoted {
        return Err(FormatError::spef(line_number, "unterminated quoted field"));
    }
    if !field.is_empty() {
        fields.push(field);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_mapped_rc_networks_in_si_units() {
        let spef = parse_spef(
            r#"
*SPEF "IEEE 1481-1998"
*DESIGN "top"
*C_UNIT 1 PF
*R_UNIT 1 KOHM
*NAME_MAP
*1 n
*2 U1/Z
*3 U2/A
*D_NET *1 0.3
*CONN
*I *2 O
*I *3 I
*CAP
1 *2 0.1
2 *3 0.2
*RES
1 *2 *3 2
*END
"#,
        )
        .unwrap();

        assert_eq!(spef.design.as_deref(), Some("top"));
        assert_eq!(spef.divider, '/');
        assert_eq!(spef.delimiter, ':');
        assert_eq!(spef.nets[0].name, "n");
        assert_eq!(
            spef.nets[0].connections[1].kind,
            SpefConnectionKind::Internal
        );
        assert_eq!(spef.nets[0].connections[1].node, "U2/A");
        assert!((spef.nets[0].capacitors[1].capacitance_farads - 2e-13).abs() < 1e-27);
        assert!((spef.nets[0].resistors[0].resistance_ohms - 2e3).abs() < 1e-10);
    }

    #[test]
    fn rejects_resistance_sections_without_units() {
        let error = parse_spef("*C_UNIT 1 PF\n*D_NET n 0.1\n*RES\n1 a b 1\n*END\n").unwrap_err();
        assert!(error.to_string().contains("*R_UNIT"));
    }

    #[test]
    fn rejects_unresolved_name_map_references() {
        let error = parse_spef("*C_UNIT 1 PF\n*D_NET *7 0.1\n*END\n").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unresolved name-map reference '*7'")
        );
    }

    #[test]
    fn rejects_scaled_values_that_overflow() {
        let error = parse_spef("*C_UNIT 1e308 F\n*D_NET n 10\n*END\n").unwrap_err();
        assert!(error.to_string().contains("finite numeric range"));
    }
}
