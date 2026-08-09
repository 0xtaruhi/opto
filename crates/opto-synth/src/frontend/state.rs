// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::predicate::Predicate;
use opto_ir::{proc, word};
use smallvec::SmallVec;

pub(super) type ResetList = SmallVec<[word::Reset; 1]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TargetKey {
    pub(super) signal: word::SignalId,
    pub(super) lsb: u32,
    pub(super) width: u32,
}

impl TargetKey {
    pub(super) fn target(
        self,
        module: &word::WordModule,
    ) -> Result<word::LValue, crate::SynthError> {
        let width = module
            .signal(self.signal)
            .ok_or_else(|| crate::SynthError::invariant("procedural target signal disappeared"))?
            .ty
            .width();
        Ok(if self.lsb == 0 && self.width == width {
            word::LValue::signal(self.signal)
        } else {
            word::LValue::signal(self.signal).with_range(word::BitRange {
                msb: self.lsb + self.width - 1,
                lsb: self.lsb,
            })
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Coverage {
    Never,
    Always,
    When(word::ValueId),
}

#[derive(Debug, Clone)]
pub(super) struct Assignment {
    pub(super) target: word::LValue,
    pub(super) value: word::ValueId,
    pub(super) coverage: Coverage,
    pub(super) resets: ResetList,
    pub(super) held_events: SmallVec<[proc::SensitivityEvent; 2]>,
    pub(super) source: word::SourceSpan,
}

impl Assignment {
    pub(super) fn is_definite(&self) -> bool {
        self.coverage == Coverage::Always
    }

    pub(super) fn enable(&self) -> Option<word::ValueId> {
        match self.coverage {
            Coverage::Never | Coverage::Always => None,
            Coverage::When(value) => Some(value),
        }
    }

    pub(super) fn target_name<'a>(&self, module: &'a word::WordModule) -> &'a str {
        module
            .signal(self.target.signal)
            .and_then(|signal| signal.name)
            .map_or("<unnamed>", |name| module.name_str(name))
    }

    pub(super) fn holds_on(&self, event: proc::SensitivityEvent) -> bool {
        self.held_events.contains(&event)
    }
}

#[derive(Debug, Clone)]
pub(super) struct Slot {
    /// Value observed by later blocking reads on this control-flow path.
    pub(super) current: word::ValueId,
    /// Value presented to the inferred storage element when `coverage` holds.
    pub(super) update: word::ValueId,
    pub(super) coverage: Predicate,
    pub(super) resets: ResetList,
    pub(super) source: word::SourceSpan,
}

impl Slot {
    pub(super) fn unassigned(value: word::ValueId, source: word::SourceSpan) -> Self {
        Self {
            current: value,
            update: value,
            coverage: Predicate::Never,
            resets: ResetList::new(),
            source,
        }
    }

    pub(super) fn assigned(value: word::ValueId, source: word::SourceSpan) -> Self {
        Self {
            current: value,
            update: value,
            coverage: Predicate::Always,
            resets: ResetList::new(),
            source,
        }
    }

    pub(super) fn semantically_eq(&self, other: &Self) -> bool {
        self.current == other.current
            && self.update == other.update
            && self.coverage == other.coverage
            && self.resets == other.resets
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameId(usize);

#[derive(Debug, Default)]
struct Frame {
    parent: Option<FrameId>,
    head: Option<usize>,
    depth: usize,
}

#[derive(Debug)]
struct Write {
    previous: Option<usize>,
    key: TargetKey,
    slot: Slot,
}

/// Persistent sparse state. Branches share a parent frame and record only
/// their writes; a join materializes one phi frame in deterministic key order.
#[derive(Debug, Default)]
pub(super) struct StateArena {
    frames: Vec<Frame>,
    writes: Vec<Write>,
}

impl StateArena {
    pub(super) fn root(&mut self) -> FrameId {
        self.push(None)
    }

    pub(super) fn child(&mut self, parent: FrameId) -> FrameId {
        self.push(Some(parent))
    }

    fn push(&mut self, parent: Option<FrameId>) -> FrameId {
        let id = FrameId(self.frames.len());
        let depth = parent.map_or(0, |parent| self.frames[parent.0].depth + 1);
        self.frames.push(Frame {
            parent,
            head: None,
            depth,
        });
        id
    }

    pub(super) fn set(&mut self, frame: FrameId, key: TargetKey, slot: Slot) {
        let previous = self.frames[frame.0].head;
        self.frames[frame.0].head = Some(self.writes.len());
        self.writes.push(Write {
            previous,
            key,
            slot,
        });
    }

    pub(super) fn get(&self, mut frame: FrameId, key: TargetKey) -> Option<&Slot> {
        loop {
            let current = &self.frames[frame.0];
            let mut write = current.head;
            while let Some(index) = write {
                let record = &self.writes[index];
                if record.key == key {
                    return Some(&record.slot);
                }
                write = record.previous;
            }
            frame = current.parent?;
        }
    }

    pub(super) fn common_ancestor(
        &self,
        frames: impl IntoIterator<Item = FrameId>,
    ) -> Result<Option<FrameId>, crate::SynthError> {
        let mut frames = frames.into_iter();
        let Some(mut ancestor) = frames.next() else {
            return Ok(None);
        };
        for frame in frames {
            ancestor = self.pairwise_common_ancestor(ancestor, frame)?;
        }
        Ok(Some(ancestor))
    }

    pub(super) fn collect_changed_keys(
        &self,
        mut frame: FrameId,
        ancestor: FrameId,
        keys: &mut Vec<TargetKey>,
    ) -> Result<(), crate::SynthError> {
        while frame != ancestor {
            let current = &self.frames[frame.0];
            let mut write = current.head;
            while let Some(index) = write {
                let record = &self.writes[index];
                keys.push(record.key);
                write = record.previous;
            }
            frame = current.parent.ok_or_else(|| {
                crate::SynthError::invariant(
                    "procedural merge input does not descend from its common ancestor",
                )
            })?;
        }
        Ok(())
    }

    fn pairwise_common_ancestor(
        &self,
        mut left: FrameId,
        mut right: FrameId,
    ) -> Result<FrameId, crate::SynthError> {
        while self.frames[left.0].depth > self.frames[right.0].depth {
            left = self.frames[left.0].parent.ok_or_else(|| {
                crate::SynthError::invariant("non-root state frame has no parent")
            })?;
        }
        while self.frames[right.0].depth > self.frames[left.0].depth {
            right = self.frames[right.0].parent.ok_or_else(|| {
                crate::SynthError::invariant("non-root state frame has no parent")
            })?;
        }
        while left != right {
            left = self.frames[left.0].parent.ok_or_else(|| {
                crate::SynthError::invariant("procedural state frames do not share a root")
            })?;
            right = self.frames[right.0].parent.ok_or_else(|| {
                crate::SynthError::invariant("procedural state frames do not share a root")
            })?;
        }
        Ok(left)
    }
}
