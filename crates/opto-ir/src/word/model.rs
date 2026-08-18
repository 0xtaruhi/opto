// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Owned data records for word-level structural and operation arenas.
//!
//! Records in [`super::WordModule`] contain typed IDs rather than references so
//! modules remain movable, serializable, and safe to inspect concurrently after
//! publication. Source metadata is shared separately to keep repeated spans
//! inexpensive.

use super::{MemoryId, OpId, PortId, SignalId, TypeLayoutId, ValueId, WordError};
use crate::NameId;
use crate::value::ConstBits;
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Number of logic states represented by a word-level value.
pub enum LogicStateKind {
    /// Boolean `0` and `1` only.
    TwoState,
    /// `SystemVerilog` `0`, `1`, unknown, and high-impedance states.
    FourState,
}

impl LogicStateKind {
    pub(super) fn merge(self, other: Self) -> Self {
        if matches!(self, LogicStateKind::FourState) || matches!(other, LogicStateKind::FourState) {
            LogicStateKind::FourState
        } else {
            LogicStateKind::TwoState
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Nonzero bit width, signedness, and state domain of a word-level value.
pub struct WordType {
    width: NonZeroU32,
    signed: bool,
    state: LogicStateKind,
}

impl WordType {
    /// Creates an unsigned four-state bit-vector type.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `width` is zero.
    pub fn bits(width: u32) -> Result<Self, WordError> {
        Self::new(width, false, LogicStateKind::FourState)
    }

    /// Creates a word type from its complete representation.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `width` is zero.
    pub fn new(width: u32, signed: bool, state: LogicStateKind) -> Result<Self, WordError> {
        let width = NonZeroU32::new(width)
            .ok_or_else(|| WordError::new("RTL bit type width must be non-zero"))?;
        Ok(Self {
            width,
            signed,
            state,
        })
    }

    /// Returns the bit width.
    #[must_use]
    pub fn width(self) -> u32 {
        self.width.get()
    }

    /// Returns whether arithmetic and comparisons interpret the value as signed.
    #[must_use]
    pub fn is_signed(self) -> bool {
        self.signed
    }

    /// Returns the two-state or four-state value domain.
    #[must_use]
    pub fn state(self) -> LogicStateKind {
        self.state
    }

    pub(super) fn with_width(self, width: u32) -> Result<Self, WordError> {
        Self::new(width, self.signed, self.state)
    }

    pub(super) fn merged_state(self, other: Self) -> LogicStateKind {
        self.state.merge(other.state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct SourceMetadata {
    file: Option<Arc<str>>,
    construct: Option<Arc<str>>,
    identity: Option<SourceIdentity>,
}

#[derive(Default)]
enum SourceOriginSerdeState {
    #[default]
    Inactive,
    Serializing {
        origins: BTreeMap<Arc<SourceMetadata>, u32>,
    },
    Deserializing {
        origins: Vec<SourceOrigin>,
        identities: BTreeMap<Arc<SourceMetadata>, u32>,
    },
}

std::thread_local! {
    static SOURCE_ORIGIN_SERDE_STATE: RefCell<SourceOriginSerdeState> =
        const { RefCell::new(SourceOriginSerdeState::Inactive) };
}

struct SourceOriginSerdeGuard;

impl Drop for SourceOriginSerdeGuard {
    fn drop(&mut self) {
        SOURCE_ORIGIN_SERDE_STATE.with(|state| {
            *state.borrow_mut() = SourceOriginSerdeState::Inactive;
        });
    }
}

fn begin_source_origin_serde(
    next: SourceOriginSerdeState,
) -> Result<SourceOriginSerdeGuard, &'static str> {
    SOURCE_ORIGIN_SERDE_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !matches!(*state, SourceOriginSerdeState::Inactive) {
            return Err("a source-origin serde scope is already active on this thread");
        }
        *state = next;
        Ok(SourceOriginSerdeGuard)
    })
}

pub(crate) fn with_source_origin_serialization<T, E>(
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    E: serde::ser::Error,
{
    let _guard = begin_source_origin_serde(SourceOriginSerdeState::Serializing {
        origins: BTreeMap::new(),
    })
    .map_err(E::custom)?;
    operation()
}

pub(crate) fn with_source_origin_deserialization<T, E>(
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    E: serde::de::Error,
{
    let _guard = begin_source_origin_serde(SourceOriginSerdeState::Deserializing {
        origins: Vec::new(),
        identities: BTreeMap::new(),
    })
    .map_err(E::custom)?;
    operation()
}

#[derive(Serialize)]
enum SourceOriginRef<'a> {
    Define {
        id: u32,
        metadata: &'a SourceMetadata,
    },
    Reference(u32),
}

#[derive(Deserialize)]
enum SourceOriginWire {
    Define { id: u32, metadata: SourceMetadata },
    Reference(u32),
}

#[derive(Serialize)]
#[serde(rename = "SourceOrigin")]
struct PlainSourceOriginRef<'a>(&'a Arc<SourceMetadata>);

#[derive(Deserialize)]
#[serde(rename = "SourceOrigin")]
struct PlainSourceOrigin(Arc<SourceMetadata>);

#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq)]
/// Shared source file and construct metadata.
///
/// Cloning an origin is constant time. Spans from the same frontend construct
/// can share one allocation without coupling their line and column.
pub struct SourceOrigin(Arc<SourceMetadata>);

impl Serialize for SourceOrigin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let reference = SOURCE_ORIGIN_SERDE_STATE.with(|state| {
            let mut state = state.borrow_mut();
            match &mut *state {
                SourceOriginSerdeState::Inactive => Ok(None),
                SourceOriginSerdeState::Serializing { origins, .. } => {
                    if let Some(id) = origins.get(&self.0).copied() {
                        return Ok(Some(SourceOriginRef::Reference(id)));
                    }
                    let id = u32::try_from(origins.len()).map_err(|_| {
                        S::Error::custom("source-origin table exceeds 32-bit capacity")
                    })?;
                    origins.insert(Arc::clone(&self.0), id);
                    Ok(Some(SourceOriginRef::Define {
                        id,
                        metadata: self.0.as_ref(),
                    }))
                }
                SourceOriginSerdeState::Deserializing { .. } => Err(S::Error::custom(
                    "cannot serialize a source origin while deserialization is active",
                )),
            }
        })?;
        match reference {
            Some(reference) => reference.serialize(serializer),
            None => PlainSourceOriginRef(&self.0).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SourceOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let scoped = SOURCE_ORIGIN_SERDE_STATE.with(|state| match *state.borrow() {
            SourceOriginSerdeState::Inactive => Ok(false),
            SourceOriginSerdeState::Deserializing { .. } => Ok(true),
            SourceOriginSerdeState::Serializing { .. } => Err(D::Error::custom(
                "cannot deserialize a source origin while serialization is active",
            )),
        })?;
        if !scoped {
            return PlainSourceOrigin::deserialize(deserializer).map(|plain| Self(plain.0));
        }
        let wire = SourceOriginWire::deserialize(deserializer)?;
        SOURCE_ORIGIN_SERDE_STATE.with(|state| {
            let mut state = state.borrow_mut();
            match (&mut *state, wire) {
                (
                    SourceOriginSerdeState::Deserializing {
                        origins,
                        identities,
                        ..
                    },
                    SourceOriginWire::Define { id, metadata },
                ) => {
                    let expected = u32::try_from(origins.len()).map_err(|_| {
                        D::Error::custom("source-origin table exceeds 32-bit capacity")
                    })?;
                    if id != expected {
                        return Err(D::Error::custom(
                            "source-origin definitions are not in dense ID order",
                        ));
                    }
                    let metadata = Arc::new(metadata);
                    if identities.contains_key(&metadata) {
                        return Err(D::Error::custom(
                            "source-origin table contains duplicate metadata",
                        ));
                    }
                    let origin = Self(Arc::clone(&metadata));
                    identities.insert(metadata, id);
                    origins.push(origin.clone());
                    Ok(origin)
                }
                (
                    SourceOriginSerdeState::Deserializing { origins, .. },
                    SourceOriginWire::Reference(id),
                ) => origins.get(id as usize).cloned().ok_or_else(|| {
                    D::Error::custom("source-origin reference precedes its definition")
                }),
                _ => Err(D::Error::custom(
                    "source-origin deserialization scope ended before its value",
                )),
            }
        })
    }
}

impl SourceOrigin {
    /// Creates shared source metadata.
    #[must_use]
    pub fn new(file: Option<String>, construct: Option<String>) -> Self {
        Self(Arc::new(SourceMetadata {
            file: file.map(Into::into),
            construct: construct.map(Into::into),
            identity: None,
        }))
    }

    /// Returns the source file path, if known.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.0.file.as_deref()
    }

    /// Returns the frontend construct name, if known.
    #[must_use]
    pub fn construct(&self) -> Option<&str> {
        self.0.construct.as_deref()
    }

    fn with_identity(&self, identity: SourceIdentity) -> Self {
        Self(Arc::new(SourceMetadata {
            file: self.0.file.clone(),
            construct: self.0.construct.clone(),
            identity: Some(identity),
        }))
    }

    fn identity(&self) -> Option<SourceIdentity> {
        self.0.identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Stable identity of one frontend syntax node within a module definition.
pub struct SourceIdentity([u8; 32]);

impl SourceIdentity {
    /// Creates a stable source identity from a frontend-owned digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical serialized digest.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    fn derived(self, construct: &str, role: &[u8]) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/generated-source/v1\0");
        digest.update(&self.0);
        digest.update(&(construct.len() as u64).to_le_bytes());
        digest.update(construct.as_bytes());
        digest.update(&(role.len() as u64).to_le_bytes());
        digest.update(role);
        Self(*digest.finalize().as_bytes())
    }

    pub(crate) fn in_occurrence(self, occurrence: &str) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/source-occurrence/v1\0");
        digest.update(&self.0);
        digest.update(&(occurrence.len() as u64).to_le_bytes());
        digest.update(occurrence.as_bytes());
        Self(*digest.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
/// Optional source origin and one-based line and column.
///
/// An empty span is valid for generated IR. Zero passed to a constructor is
/// treated as an unknown line or column.
pub struct SourceSpan {
    origin: Option<SourceOrigin>,
    line: Option<NonZeroU32>,
    column: Option<NonZeroU32>,
}

impl SourceSpan {
    /// Creates an unlocated span with an explicit stable programmatic identity.
    ///
    /// Callers constructing IR without a source-language syntax tree must keep
    /// `stable_key` unchanged for the same logical construction site.
    #[must_use]
    pub fn stable(stable_key: impl AsRef<[u8]>) -> Self {
        let key = stable_key.as_ref();
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/programmatic-source/v1\0");
        digest.update(&(key.len() as u64).to_le_bytes());
        digest.update(key);
        Self::default().with_identity(SourceIdentity::from_bytes(*digest.finalize().as_bytes()))
    }

    /// Creates an unlocated span for a well-known frontend construct.
    #[must_use]
    pub fn construct(construct: &'static str) -> Self {
        Self {
            origin: Some(shared_construct(construct)),
            ..Self::default()
        }
    }

    /// Creates a located span with owned file and construct names.
    pub fn located(
        file: impl Into<String>,
        line: Option<u32>,
        column: Option<u32>,
        construct: impl Into<String>,
    ) -> Self {
        Self::with_origin(
            SourceOrigin::new(Some(file.into()), Some(construct.into())),
            line,
            column,
        )
    }

    /// Creates a span that reuses shared origin metadata.
    pub fn with_origin(origin: SourceOrigin, line: Option<u32>, column: Option<u32>) -> Self {
        Self {
            origin: Some(origin),
            line: line.and_then(NonZeroU32::new),
            column: column.and_then(NonZeroU32::new),
        }
    }

    /// Attaches the stable syntax identity carried independently of diagnostics.
    #[must_use]
    pub fn with_identity(mut self, identity: SourceIdentity) -> Self {
        let origin = self
            .origin
            .take()
            .unwrap_or_else(|| SourceOrigin::new(None, None));
        self.origin = Some(origin.with_identity(identity));
        self
    }

    /// Returns the stable frontend syntax identity, if one was supplied.
    #[must_use]
    pub fn identity(&self) -> Option<SourceIdentity> {
        self.origin.as_ref().and_then(SourceOrigin::identity)
    }

    /// Derives the identity of generated IR from an anchored source construct.
    ///
    /// `role` identifies the generated operation's structural role within the
    /// parent construct. It must be stable across unrelated source edits and
    /// must not contain arena positions or semantic content. The diagnostic
    /// location is inherited from the parent while the construct name records
    /// the transformation that emitted the generated IR.
    #[must_use]
    pub fn derived(&self, construct: &'static str, role: impl AsRef<[u8]>) -> Option<Self> {
        let identity = self.identity()?.derived(construct, role.as_ref());
        Some(Self {
            origin: Some(
                SourceOrigin::new(self.file().map(str::to_owned), Some(construct.to_string()))
                    .with_identity(identity),
            ),
            line: self.line,
            column: self.column,
        })
    }

    pub(crate) fn in_occurrence(&self, occurrence: &str) -> Self {
        let mut source = self.clone();
        if let Some(identity) = source.identity() {
            source = source.with_identity(identity.in_occurrence(occurrence));
        }
        source
    }

    /// Returns the source file path, if known.
    pub fn file(&self) -> Option<&str> {
        self.origin.as_ref().and_then(SourceOrigin::file)
    }

    /// Returns the one-based source line, if known.
    pub fn line(&self) -> Option<u32> {
        self.line.map(NonZeroU32::get)
    }

    /// Returns the one-based source column, if known.
    pub fn column(&self) -> Option<u32> {
        self.column.map(NonZeroU32::get)
    }

    /// Returns the frontend construct name, if known.
    pub fn construct_name(&self) -> Option<&str> {
        self.origin.as_ref().and_then(SourceOrigin::construct)
    }
}

fn shared_construct(construct: &'static str) -> SourceOrigin {
    static CONSTRUCTS: OnceLock<BTreeMap<&'static str, SourceOrigin>> = OnceLock::new();
    let constructs = CONSTRUCTS.get_or_init(|| {
        [
            "always",
            "always_comb",
            "always_ff",
            "always_latch",
            "assertion guard",
            "blocking assignment",
            "case item",
            "case statement",
            "child output",
            "concat",
            "data assignment",
            "else assignment",
            "enable condition",
            "if statement",
            "instance connection",
            "module instance",
            "net",
            "nonblocking assignment",
            "parent output",
            "port",
            "procedural block",
            "procedural state entry",
            "procedural partial read",
            "process local",
            "reset assignment",
            "reset condition",
            "signal read",
            "then assignment",
        ]
        .into_iter()
        .map(|name| (name, SourceOrigin::new(None, Some(name.to_string()))))
        .collect()
    });
    constructs
        .get(construct)
        .cloned()
        .unwrap_or_else(|| SourceOrigin::new(None, Some(construct.to_string())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
/// Direction of a module port relative to its definition.
pub enum PortDirection {
    /// Driven by the module's environment.
    Input,
    /// Driven by the module.
    Output,
    /// May be driven from either side using resolved-net semantics.
    Inout,
    /// Exact variable alias bound to an enclosing signal during linked elaboration.
    Ref,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Named module interface port backed by one signal.
pub struct Port {
    /// Interned port name.
    pub name: NameId,
    /// Direction relative to the containing module.
    pub direction: PortDirection,
    /// Signal carrying the port value.
    pub signal: SignalId,
    /// Declared word type.
    pub ty: WordType,
    /// Declaration source span.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Storage role of a structural signal.
pub enum SignalKind {
    /// Continuously driven net.
    Wire,
    /// Procedurally assigned state-bearing signal.
    Register,
    /// Temporary signal valid only during procedural normalization.
    ProcessLocal,
    /// Signal that implements the identified module port.
    Port(PortId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Resolution function applied when a signal has several structural drivers.
pub enum SignalResolution {
    /// Exactly one driver is permitted.
    SingleDriver,
    /// Drivers remain explicit physical tri-state contributions.
    TriState,
    /// Driver values are combined by bitwise AND.
    WiredAnd,
    /// Driver values are combined by bitwise OR.
    WiredOr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Typed signal stored in a [`super::WordModule`].
pub struct Signal {
    /// Optional interned name; generated temporaries may be unnamed.
    pub name: Option<NameId>,
    /// Structural storage role.
    pub kind: SignalKind,
    /// Bit width, signedness, and state domain.
    pub ty: WordType,
    /// Multiple-driver resolution policy.
    pub resolution: SignalResolution,
    /// Optional source-facing aggregate layout.
    pub type_layout: Option<TypeLayoutId>,
    /// Declaration or generation source span.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Contiguous signal slice referenced by a word-level value.
pub struct SignalRef {
    /// Referenced signal.
    pub signal: SignalId,
    /// Zero-based least-significant bit offset.
    pub lsb: u32,
    pub(super) width: NonZeroU32,
}

impl SignalRef {
    /// Returns the selected width.
    #[must_use]
    pub fn width(self) -> u32 {
        self.width.get()
    }
}

/// A signal-backed fragment of a structural connection, ordered from least to
/// most significant when returned by [`crate::word::WordModule::signal_fragments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalFragment {
    /// Signal slice supplying this fragment.
    pub reference: SignalRef,
    /// Type of the fragment after structural slicing.
    pub ty: WordType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Storage backing a word-level SSA value.
pub enum ValueKind {
    /// Read of a structural signal slice.
    Signal(SignalRef),
    /// Literal constant.
    Constant(ConstBits),
    /// Result of an operation in the operation arena.
    Operation(OpId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Typed word-level value.
pub struct Value {
    /// Signal, constant, or operation backing the value.
    pub kind: ValueKind,
    /// Complete word type.
    pub ty: WordType,
    /// Source span that produced the value.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Unary word-level operation.
pub enum UnaryOp {
    /// One-bit logical negation after truth conversion.
    LogicalNot,
    /// Per-bit complement.
    BitNot,
    /// AND reduction to one bit.
    ReductionAnd,
    /// OR reduction to one bit.
    ReductionOr,
    /// XOR parity reduction to one bit.
    ReductionXor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Binary word-level operation.
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Division.
    Div,
    /// Remainder.
    Mod,
    /// Per-bit AND.
    BitAnd,
    /// Per-bit OR.
    BitOr,
    /// Per-bit exclusive OR.
    BitXor,
    /// One-bit logical conjunction.
    LogicalAnd,
    /// One-bit logical disjunction.
    LogicalOr,
    /// Equality comparison.
    Eq,
    /// Inequality comparison.
    Ne,
    /// Less-than comparison using operand signedness.
    Lt,
    /// Less-than-or-equal comparison using operand signedness.
    Le,
    /// Greater-than comparison using operand signedness.
    Gt,
    /// Greater-than-or-equal comparison using operand signedness.
    Ge,
    /// Logical left shift.
    Shl,
    /// Logical right shift.
    Shr,
    /// Arithmetic right shift.
    Ashr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Explicit width-changing conversion.
pub enum CastKind {
    /// Extends with zero bits.
    ZeroExtend,
    /// Extends by replicating the sign bit.
    SignExtend,
    /// Discards most-significant bits.
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Active clock or sensitivity edge.
pub enum Edge {
    /// Rising edge.
    Pos,
    /// Falling edge.
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Clock value and active edge for a memory port.
pub struct MemoryClock {
    /// Scalar clock value.
    pub value: ValueId,
    /// Active edge.
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Output behavior of a disabled synchronous memory read.
pub enum DisabledRead {
    /// Preserve the previously read value.
    Hold,
    /// Drive an unspecified value.
    Undefined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Value observed when a read and write address collide.
pub enum ReadDuringWrite {
    /// Observe memory contents before the write.
    OldData,
    /// Observe newly written data.
    NewData,
    /// Preserve the previous read output.
    NoChange,
    /// Collision behavior is unspecified.
    Undefined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Combinational or registered timing of a memory read port.
pub enum MemoryReadTiming {
    /// Address changes propagate without a clock edge.
    Asynchronous,
    /// Data is captured on an active clock edge.
    Synchronous {
        /// Sampling clock and edge.
        clock: MemoryClock,
        /// Optional qualified enable.
        enable: Option<Enable>,
        /// Output behavior while disabled.
        disabled: DisabledRead,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Technology-independent memory resource.
pub struct Memory {
    /// Interned resource name.
    pub name: NameId,
    /// Type of one addressed element.
    pub element_type: WordType,
    /// Number of addressable elements.
    pub depth: NonZeroU32,
    /// Declaration source span.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One logical read port of a memory resource.
pub struct MemoryReadPort {
    /// Memory read by this port.
    pub memory: MemoryId,
    /// Unsigned address value.
    pub address: ValueId,
    /// The unique signal driven by this port.
    pub data: SignalId,
    /// Asynchronous or synchronous read timing.
    pub timing: MemoryReadTiming,
    /// Same-address read/write collision policy.
    pub read_during_write: ReadDuringWrite,
    /// Source span of the read construct.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Optional lane mask applied to a memory write port.
pub struct MemoryWriteMask {
    /// Mask value, with one bit per lane.
    pub value: ValueId,
    /// Number of adjacent data bits controlled by one mask bit.
    pub granularity: NonZeroU32,
    /// Whether a set mask bit enables the corresponding lane.
    pub active_high: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One synchronous write port of a memory resource.
pub struct MemoryWritePort {
    /// Memory written by this port.
    pub memory: MemoryId,
    /// Unsigned address value.
    pub address: ValueId,
    /// Element-width write data.
    pub data: ValueId,
    /// Sampling clock and edge.
    pub clock: MemoryClock,
    /// Optional qualified port enable.
    pub enable: Option<Enable>,
    /// Optional lane mask.
    pub mask: Option<MemoryWriteMask>,
    /// Higher values win when enabled ports write the same address.
    pub priority: u32,
    /// Source span of the write construct.
    pub source: SourceSpan,
}

/// Memory resources awaiting technology-independent register-bank lowering.
///
/// Extraction is deliberately all-or-nothing: once owned by the resource
/// inference stage, no memory port remains in the structural module for a
/// later pass to overlook.
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryResources {
    /// Extracted memory declarations.
    pub memories: Vec<Memory>,
    /// Extracted read ports.
    pub reads: Vec<MemoryReadPort>,
    /// Extracted write ports.
    pub writes: Vec<MemoryWritePort>,
}

impl MemoryResources {
    #[must_use]
    /// Returns `true` when no declarations or ports were extracted.
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty() && self.reads.is_empty() && self.writes.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Boolean enable value and its active polarity.
pub struct Enable {
    /// Scalar enable value.
    pub value: ValueId,
    /// Whether logical one enables the operation.
    pub active_high: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Scheduling relationship between reset and clock.
pub enum ResetKind {
    /// Reset is sampled only on an active clock edge.
    Sync,
    /// Reset may update state independently of the clock.
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// One reset control and reset value.
pub struct Reset {
    /// Synchronous or asynchronous scheduling.
    pub kind: ResetKind,
    /// Scalar reset condition.
    pub value: ValueId,
    /// Whether logical one asserts reset.
    pub active_high: bool,
    /// Value loaded while reset is asserted.
    pub reset_value: ValueId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Technology-independent edge-triggered register operation.
pub struct RegisterOp {
    /// Optional interned state name.
    pub name: Option<NameId>,
    /// Next-state data value.
    pub d: ValueId,
    /// Scalar clock.
    pub clock: ValueId,
    /// Active clock edge.
    pub edge: Edge,
    /// Optional clock enable.
    pub enable: Option<Enable>,
    /// Reset controls in descending priority order.
    pub resets: Vec<Reset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Technology-independent level-sensitive latch operation.
pub struct LatchOp {
    /// Optional interned state name.
    pub name: Option<NameId>,
    /// Next-state data value.
    pub d: ValueId,
    /// Level-sensitive gate.
    pub enable: Enable,
    /// Reset controls in descending priority order.
    pub resets: Vec<Reset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Operation payload stored in the word-level operation arena.
pub enum OpKind {
    /// Unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand value.
        arg: ValueId,
    },
    /// Binary operation.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
    /// Conditional selection.
    Mux {
        /// One-bit condition.
        cond: ValueId,
        /// Value selected when `cond` is true.
        then_value: ValueId,
        /// Value selected when `cond` is false.
        else_value: ValueId,
    },
    /// Conditionally enabled high-impedance driver.
    ///
    /// The result is `data` while `enable` is active and high impedance while
    /// it is inactive. Resolved-net normalization or physical tri-state
    /// mapping must consume this operation before ordinary Boolean lowering.
    TriState {
        /// Value driven while enabled.
        data: ValueId,
        /// Driver enable and its active polarity.
        enable: Enable,
    },
    /// Most-significant-first concatenation of values.
    Concat {
        /// Ordered concatenation operands.
        parts: Vec<ValueId>,
    },
    /// Static contiguous bit extraction.
    Extract {
        /// Source value.
        value: ValueId,
        /// Zero-based least-significant bit offset.
        lsb: u32,
        /// Nonzero result width.
        width: NonZeroU32,
    },
    /// Runtime-indexed contiguous bit extraction.
    DynamicExtract {
        /// Source value.
        value: ValueId,
        /// Unsigned runtime bit offset.
        offset: ValueId,
        /// Nonzero result width.
        width: NonZeroU32,
    },
    /// Runtime-indexed replacement of a contiguous bit range.
    DynamicInsert {
        /// Original complete value.
        value: ValueId,
        /// Unsigned runtime bit offset.
        offset: ValueId,
        /// Replacement bits; their width defines the written range.
        replacement: ValueId,
    },
    /// Explicit width conversion.
    Cast {
        /// Extension or truncation mode.
        kind: CastKind,
        /// Value being converted.
        value: ValueId,
        /// Complete result type.
        target: WordType,
    },
    /// Edge-triggered storage.
    Register(RegisterOp),
    /// Level-sensitive storage.
    Latch(LatchOp),
}

impl OpKind {
    /// Visits every value consumed directly by this operation in semantic
    /// operand order.
    pub fn for_each_input(&self, mut visit: impl FnMut(ValueId)) {
        let result = self.try_for_each_input::<std::convert::Infallible>(|value| {
            visit(value);
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(error) => match error {},
        }
    }

    /// Fallible form of [`Self::for_each_input`].
    ///
    /// # Errors
    ///
    /// Stops at the first operand for which `visit` returns an error and
    /// propagates that error unchanged.
    pub fn try_for_each_input<E>(
        &self,
        mut visit: impl FnMut(ValueId) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Unary { arg, .. } => visit(*arg)?,
            Self::Binary { left, right, .. } => {
                visit(*left)?;
                visit(*right)?;
            }
            Self::Mux {
                cond,
                then_value,
                else_value,
            } => {
                visit(*cond)?;
                visit(*then_value)?;
                visit(*else_value)?;
            }
            Self::TriState { data, enable } => {
                visit(*data)?;
                visit(enable.value)?;
            }
            Self::Concat { parts } => {
                for &part in parts {
                    visit(part)?;
                }
            }
            Self::Extract { value, .. } | Self::Cast { value, .. } => visit(*value)?,
            Self::DynamicExtract { value, offset, .. } => {
                visit(*value)?;
                visit(*offset)?;
            }
            Self::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                visit(*value)?;
                visit(*offset)?;
                visit(*replacement)?;
            }
            Self::Register(register) => {
                visit(register.d)?;
                visit(register.clock)?;
                if let Some(enable) = register.enable {
                    visit(enable.value)?;
                }
                for reset in &register.resets {
                    visit(reset.value)?;
                    visit(reset.reset_value)?;
                }
            }
            Self::Latch(latch) => {
                visit(latch.d)?;
                visit(latch.enable.value)?;
                for reset in &latch.resets {
                    visit(reset.value)?;
                    visit(reset.reset_value)?;
                }
            }
        }
        Ok(())
    }

    /// Mutably visits every value consumed directly by this operation in
    /// semantic operand order.
    pub fn for_each_input_mut(&mut self, mut visit: impl FnMut(&mut ValueId)) {
        let result = self.try_for_each_input_mut::<std::convert::Infallible>(|value| {
            visit(value);
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(error) => match error {},
        }
    }

    /// Fallible form of [`Self::for_each_input_mut`].
    ///
    /// # Errors
    ///
    /// Stops at the first operand for which `visit` returns an error and
    /// propagates that error unchanged. Mutations made before that operand are
    /// retained.
    pub fn try_for_each_input_mut<E>(
        &mut self,
        mut visit: impl FnMut(&mut ValueId) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Unary { arg, .. } => visit(arg)?,
            Self::Binary { left, right, .. } => {
                visit(left)?;
                visit(right)?;
            }
            Self::Mux {
                cond,
                then_value,
                else_value,
            } => {
                visit(cond)?;
                visit(then_value)?;
                visit(else_value)?;
            }
            Self::TriState { data, enable } => {
                visit(data)?;
                visit(&mut enable.value)?;
            }
            Self::Concat { parts } => {
                for part in parts {
                    visit(part)?;
                }
            }
            Self::Extract { value, .. } | Self::Cast { value, .. } => visit(value)?,
            Self::DynamicExtract { value, offset, .. } => {
                visit(value)?;
                visit(offset)?;
            }
            Self::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                visit(value)?;
                visit(offset)?;
                visit(replacement)?;
            }
            Self::Register(register) => {
                visit(&mut register.d)?;
                visit(&mut register.clock)?;
                if let Some(enable) = &mut register.enable {
                    visit(&mut enable.value)?;
                }
                for reset in &mut register.resets {
                    visit(&mut reset.value)?;
                    visit(&mut reset.reset_value)?;
                }
            }
            Self::Latch(latch) => {
                visit(&mut latch.d)?;
                visit(&mut latch.enable.value)?;
                for reset in &mut latch.resets {
                    visit(&mut reset.value)?;
                    visit(&mut reset.reset_value)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Operation record and the value that names its result.
pub struct Operation {
    /// Operation payload.
    pub kind: OpKind,
    /// Unique result value.
    pub result: ValueId,
    /// Source span of the operation.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Operand reference used by a deterministic generated-operation batch.
pub enum BatchValue {
    /// Value already present in the destination module.
    Existing(ValueId),
    /// Earlier result in the same batch, addressed by zero-based ordinal.
    Generated(u32),
}

#[derive(Debug, Clone)]
/// One scalar mux in a deterministic generated-operation batch.
pub struct MuxBatchOperation {
    /// One-bit selection value.
    pub cond: BatchValue,
    /// Value selected when `cond` is true.
    pub then_value: BatchValue,
    /// Value selected when `cond` is false.
    pub else_value: BatchValue,
    /// Source span inherited by the generated mux.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Inclusive static bit range preserving source orientation.
pub struct BitRange {
    /// Source-facing most-significant endpoint.
    pub msb: u32,
    /// Source-facing least-significant endpoint.
    pub lsb: u32,
}

impl BitRange {
    /// Returns the nonzero number of selected bits.
    #[must_use]
    pub fn width(self) -> u32 {
        self.msb.abs_diff(self.lsb) + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Structural assignment target.
///
/// At most one of `range` and `dynamic` may be present.
pub struct LValue {
    /// Destination signal.
    pub signal: SignalId,
    /// Optional statically selected bit range.
    pub range: Option<BitRange>,
    /// Optional runtime-indexed bit range.
    pub dynamic: Option<DynamicRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Runtime-indexed contiguous bit range.
pub struct DynamicRange {
    /// Unsigned runtime least-significant bit offset.
    pub offset: ValueId,
    /// Nonzero selected width.
    pub width: NonZeroU32,
}

impl LValue {
    /// Creates a whole-signal assignment target.
    #[must_use]
    pub fn signal(signal: SignalId) -> Self {
        Self {
            signal,
            range: None,
            dynamic: None,
        }
    }

    /// Adds a static bit selection.
    ///
    /// Calling this after [`Self::with_dynamic_range`] creates an invalid
    /// target that [`super::WordModule::validate`] will reject.
    #[must_use]
    pub fn with_range(mut self, range: BitRange) -> Self {
        self.range = Some(range);
        self
    }

    /// Adds a runtime-indexed bit selection.
    ///
    /// Calling this after [`Self::with_range`] creates an invalid target that
    /// [`super::WordModule::validate`] will reject.
    #[must_use]
    pub fn with_dynamic_range(mut self, offset: ValueId, width: NonZeroU32) -> Self {
        self.dynamic = Some(DynamicRange { offset, width });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Continuous structural assignment.
pub struct Connect {
    /// Destination signal or slice.
    pub target: LValue,
    /// Assigned value.
    pub value: ValueId,
    /// Source span of the assignment.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Named port binding on a structural instance.
pub struct InstanceConnection {
    /// Interned child port name.
    pub port: NameId,
    /// Parent-module value connected to the port.
    pub value: ValueId,
    /// Source span of the connection.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Structural child-definition or library-cell instance.
pub struct Instance {
    /// Interned instance name.
    pub name: NameId,
    /// Interned referenced definition name.
    pub module: NameId,
    /// Port bindings in deterministic source order.
    pub connections: Vec<InstanceConnection>,
    /// Source span of the instantiation.
    pub source: SourceSpan,
}

#[derive(Debug, Clone)]
/// One instance row prepared for deterministic packed Word construction.
pub struct PackedInstanceSpec {
    /// Unique instance name.
    pub name: String,
    /// Referenced definition name.
    pub module: String,
    /// Port name, parent value, and source span in deterministic order.
    pub connections: Vec<(String, ValueId, SourceSpan)>,
    /// Source span of the instantiation.
    pub source: SourceSpan,
}
