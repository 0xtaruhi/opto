// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Portable target-cover payloads.

use super::{LibraryCoverBinding, LibraryCoverSource};
use crate::boolean::logic::MAX_MATCH_INPUTS;

#[derive(Debug)]
pub(crate) struct PortableCoverCell {
    binding_identity: Box<[u8]>,
    truth: crate::boolean::logic::TruthTable,
    second_truth: Option<crate::boolean::logic::TruthTable>,
    joint: bool,
    sources: Box<[LibraryCoverSource]>,
}

#[derive(Debug)]
pub(crate) struct PortableCover {
    cells: Box<[PortableCoverCell]>,
    outputs: Box<[LibraryCoverSource]>,
}

impl PortableCover {
    pub(crate) fn cells(&self) -> &[PortableCoverCell] {
        &self.cells
    }

    pub(crate) fn outputs(&self) -> &[LibraryCoverSource] {
        &self.outputs
    }
}

impl PortableCoverCell {
    pub(crate) fn sources(&self) -> &[LibraryCoverSource] {
        &self.sources
    }

    pub(crate) const fn truth(&self) -> crate::boolean::logic::TruthTable {
        self.truth
    }

    pub(crate) const fn second_truth(&self) -> Option<crate::boolean::logic::TruthTable> {
        self.second_truth
    }

    pub(crate) fn binding(
        &self,
        catalog: &super::super::library::CombinationalCellCatalog,
    ) -> Result<LibraryCoverBinding, crate::SynthError> {
        if self.joint {
            let second = self.second_truth.ok_or_else(|| {
                crate::SynthError::invariant("joint regional cover binding has no secondary truth")
            })?;
            return catalog
                .joint_binding_for_identity((self.truth, second), &self.binding_identity)
                .map(LibraryCoverBinding::Joint)
                .ok_or_else(|| {
                    crate::SynthError::mapping(
                        "regional cover-plan joint binding is absent from the active library",
                    )
                });
        }
        if self.second_truth.is_some() {
            return Err(crate::SynthError::invariant(
                "single-output regional cover binding has a secondary truth",
            ));
        }
        catalog
            .binding_for_identity(self.truth, &self.binding_identity)
            .map(LibraryCoverBinding::Single)
            .ok_or_else(|| {
                crate::SynthError::mapping(
                    "regional cover-plan binding is absent from the active library",
                )
            })
    }
}

pub(crate) fn decode(payload: &[u8]) -> Result<PortableCover, crate::SynthError> {
    let mut reader = PlanPayloadReader::new(payload, "regional cover-plan");
    if reader.read_array::<5>()? != *b"ORCP\x03" {
        return Err(crate::SynthError::invariant(
            "regional cover-plan payload has an unknown ABI",
        ));
    }
    let cell_count = reader.read_count("regional cover-plan cell count", 31)?;
    let mut cells = Vec::with_capacity(cell_count);
    for index in 0..cell_count {
        let local = reader.read_u32()?;
        if usize::try_from(local).ok() != Some(index) {
            return Err(crate::SynthError::invariant(
                "regional cover-plan cell IDs are not dense and ordered",
            ));
        }
        let identity_len = reader.read_count("regional cover binding identity", 1)?;
        let binding_identity = reader.read_bytes(identity_len)?.into();
        let truth = reader.read_truth()?;
        let joint = match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => {
                return Err(crate::SynthError::invariant(
                    "regional cover-plan has an unknown binding kind",
                ));
            }
        };
        let second_truth = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_truth()?),
            _ => {
                return Err(crate::SynthError::invariant(
                    "regional cover-plan has an invalid secondary-output marker",
                ));
            }
        };
        let source_count = reader.read_count("regional cover-plan source count", 9)?;
        let sources = (0..source_count)
            .map(|_| reader.read_source())
            .collect::<Result<_, _>>()?;
        cells.push(PortableCoverCell {
            binding_identity,
            truth,
            second_truth,
            joint,
            sources,
        });
    }
    let output_count = reader.read_count("regional cover-plan output count", 9)?;
    let outputs = (0..output_count)
        .map(|_| reader.read_source())
        .collect::<Result<_, _>>()?;
    if !reader.is_empty() {
        return Err(crate::SynthError::invariant(
            "regional cover-plan payload has trailing bytes",
        ));
    }
    Ok(PortableCover {
        cells: cells.into_boxed_slice(),
        outputs,
    })
}

pub(crate) fn empty_plan_key(region: crate::RegionAnchorId, decision_key: [u8; 32]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/regional/empty-cover-plan/v1\0");
    digest.update(&region.bytes());
    digest.update(&decision_key);
    *digest.finalize().as_bytes()
}

pub(crate) fn stable_plan_key(
    region: crate::RegionAnchorId,
    decision_key: [u8; 32],
    payload: &[u8],
    cells: &[crate::regional::RegionImplementationCell],
) -> [u8; 32] {
    let mut payload_digest = blake3::Hasher::new();
    payload_digest.update(b"opto/regional/cover-plan/v1\0");
    payload_digest.update(&region.bytes());
    payload_digest.update(&decision_key);
    payload_digest.update(payload);
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/regional/implementation/v1\0");
    digest.update(payload_digest.finalize().as_bytes());
    for cell in cells {
        digest.update(&(cell.cell_name.len() as u64).to_le_bytes());
        digest.update(cell.cell_name.as_bytes());
        digest.update(&cell.pin_count.to_le_bytes());
    }
    *digest.finalize().as_bytes()
}

pub(crate) struct PlanPayloadReader<'a> {
    remaining: &'a [u8],
    label: &'static str,
}

impl<'a> PlanPayloadReader<'a> {
    pub(crate) const fn new(payload: &'a [u8], label: &'static str) -> Self {
        Self {
            remaining: payload,
            label,
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8, crate::SynthError> {
        Ok(self.read_array::<1>()?[0])
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, crate::SynthError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64, crate::SynthError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    pub(crate) fn read_len(&mut self, resource: &'static str) -> Result<usize, crate::SynthError> {
        usize::try_from(self.read_u64()?).map_err(|_| crate::SynthError::capacity(resource))
    }

    fn read_count(
        &mut self,
        resource: &'static str,
        minimum_item_bytes: usize,
    ) -> Result<usize, crate::SynthError> {
        let count = self.read_len(resource)?;
        if count > self.remaining.len() / minimum_item_bytes {
            return Err(crate::SynthError::invariant(format!(
                "{resource} exceeds the remaining {} payload",
                self.label,
            )));
        }
        Ok(count)
    }

    fn read_truth(&mut self) -> Result<crate::boolean::logic::TruthTable, crate::SynthError> {
        let bits = self.read_u64()?;
        let input_count = usize::from(self.read_u8()?);
        if input_count > MAX_MATCH_INPUTS {
            return Err(crate::SynthError::invariant(
                "regional cover-plan truth exceeds the supported cut width",
            ));
        }
        Ok(crate::boolean::logic::TruthTable { input_count, bits })
    }

    fn read_source(&mut self) -> Result<LibraryCoverSource, crate::SynthError> {
        let kind = self.read_u8()?;
        let index = self.read_len("regional cover-plan source index")?;
        match kind {
            0 if index == 0 => Ok(LibraryCoverSource::Constant(false)),
            1 if index == 0 => Ok(LibraryCoverSource::Constant(true)),
            2 => Ok(LibraryCoverSource::Input(index)),
            3 => Ok(LibraryCoverSource::Cell(index)),
            4 => Ok(LibraryCoverSource::CellSecond(index)),
            _ => Err(crate::SynthError::invariant(
                "regional cover-plan has an invalid local source",
            )),
        }
    }

    pub(crate) fn read_array<const N: usize>(&mut self) -> Result<[u8; N], crate::SynthError> {
        let (bytes, remaining) = self.remaining.split_at_checked(N).ok_or_else(|| {
            crate::SynthError::invariant(format!("{} payload is truncated", self.label))
        })?;
        self.remaining = remaining;
        bytes.try_into().map_err(|_| {
            crate::SynthError::invariant(format!("{} payload width is invalid", self.label))
        })
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], crate::SynthError> {
        let (bytes, remaining) = self.remaining.split_at_checked(len).ok_or_else(|| {
            crate::SynthError::invariant(format!("{} payload is truncated", self.label))
        })?;
        self.remaining = remaining;
        Ok(bytes)
    }
}
