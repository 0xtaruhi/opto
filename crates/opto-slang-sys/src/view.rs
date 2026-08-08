// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Lifetime-bound Rust views over a frozen native Slang compilation.
//!
//! The owning [`SlangCompilation`] keeps the native snapshot alive. Modules are
//! materialized through explicit leases, and every nested view is bounded by
//! the lease lifetime.

use crate::bridge::{read, read_invariant};
use crate::ffi;
use crate::{SlangError, SlangPortDirection};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;

mod convert;
mod expression;
mod procedure;
mod type_layout;

pub use expression::{
    SlangConcat, SlangExpression, SlangExpressionKind, SlangLogicConstant, SlangSignalRef,
    SlangSourceSpan,
};
pub use procedure::{
    SlangBlock, SlangBlockId, SlangEdgeTarget, SlangEffect, SlangProcedure, SlangSensitivityEvent,
    SlangSwitchArm, SlangSwitchArms, SlangTerminator, SlangTerminatorKind,
};
pub use type_layout::{
    SlangArrayKind, SlangIndexRange, SlangTypeField, SlangTypeLayout, SlangTypeLayoutKind,
};

use convert::{has_flag, map_port_direction, optional_str, required_str};

/// Owning handle to a frozen native Slang compilation snapshot.
pub struct SlangCompilation {
    raw: NonNull<ffi::Snapshot>,
}

impl SlangCompilation {
    pub(crate) fn from_raw(raw: *mut ffi::Snapshot) -> Result<Self, SlangError> {
        let raw = NonNull::new(raw).ok_or_else(|| {
            SlangError::BridgeInvariant("native slang bridge returned a null design".to_string())
        })?;
        Ok(Self { raw })
    }

    fn view(&self) -> ffi::SnapshotView {
        // SAFETY: `raw` is a live snapshot and the bridge initializes the view on success.
        unsafe {
            read_invariant("snapshot", |view| {
                ffi::opto_slang_snapshot_view(self.raw.as_ptr(), view)
            })
        }
    }

    #[must_use]
    /// Returns the number of modules in the frozen snapshot.
    pub fn module_count(&self) -> usize {
        self.view().module_count
    }

    /// Iterates over module handles in stable source order.
    #[must_use]
    pub fn modules(&self) -> impl ExactSizeIterator<Item = SlangModule<'_>> + '_ {
        let design = SnapshotRef::new(self.raw);
        (0..self.module_count()).map(move |index| SlangModule { design, index })
    }

    /// Returns the selected top-module name, when Slang selected one.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for invalid native string data.
    pub fn top(&self) -> Result<Option<&str>, SlangError> {
        // SAFETY: snapshot-view strings remain owned by the live compilation.
        unsafe { optional_str(self.view().top, "top module name") }
    }

    pub(crate) fn materialize_all(&self) -> Result<(), SlangError> {
        for module in self.modules() {
            drop(module.materialize()?);
        }
        Ok(())
    }
}

impl fmt::Debug for SlangCompilation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlangCompilation")
            .field("module_count", &self.module_count())
            .finish_non_exhaustive()
    }
}

// SAFETY: frozen slang state is immutable and native module materialization is internally synchronized.
unsafe impl Send for SlangCompilation {}
// SAFETY: the native snapshot exposes immutable views and synchronizes materialization leases.
unsafe impl Sync for SlangCompilation {}

impl Drop for SlangCompilation {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the snapshot returned by the bridge and frees it exactly once.
        unsafe { ffi::opto_slang_snapshot_free(self.raw.as_ptr()) };
    }
}

#[derive(Debug, Clone, Copy)]
struct SnapshotRef<'a> {
    raw: NonNull<ffi::Snapshot>,
    _lifetime: PhantomData<&'a SlangCompilation>,
}

impl SnapshotRef<'_> {
    fn as_ptr(self) -> *const ffi::Snapshot {
        self.raw.as_ptr()
    }

    fn as_mut_ptr(self) -> *mut ffi::Snapshot {
        self.raw.as_ptr()
    }
}

impl SnapshotRef<'_> {
    fn new(raw: NonNull<ffi::Snapshot>) -> Self {
        Self {
            raw,
            _lifetime: PhantomData,
        }
    }
}

// SAFETY: the referenced frozen snapshot is safe to transfer; materialization is synchronized natively.
unsafe impl Send for SnapshotRef<'_> {}
// SAFETY: snapshot views are immutable and native materialization operations are synchronized.
unsafe impl Sync for SnapshotRef<'_> {}

#[derive(Debug, Clone, Copy)]
/// Lightweight handle to one module in a [`SlangCompilation`].
pub struct SlangModule<'a> {
    design: SnapshotRef<'a>,
    index: usize,
}

impl<'a> SlangModule<'a> {
    fn info(self) -> Result<ffi::ModuleInfoView, SlangError> {
        // SAFETY: the module index originates from this live snapshot's bounded iterator.
        unsafe {
            read("module info", |view| {
                ffi::opto_slang_module_info(self.design.as_ptr(), self.index, view)
            })
        }
    }

    /// Returns the source module name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the module index or native name view is invalid.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: module-info strings remain owned by the live snapshot for `'a`.
        unsafe { required_str(self.info()?.name, "module name") }
    }

    /// Returns the deterministic source-order key assigned by the bridge.
    ///
    /// # Panics
    ///
    /// Panics only if this module index no longer belongs to its live native snapshot.
    #[must_use]
    pub fn source_order(self) -> u64 {
        self.info()
            .expect("module index originates from the native snapshot")
            .source_order
    }

    /// Acquires a native materialization lease for this module.
    ///
    /// # Errors
    ///
    /// Returns a compile or bridge error if Slang cannot elaborate the module
    /// or expose a valid immutable view.
    pub fn materialize(self) -> Result<SlangMaterializedModule<'a>, SlangError> {
        // SAFETY: the module index is valid for this live snapshot; the bridge synchronizes leases.
        let status =
            unsafe { ffi::opto_slang_module_materialize(self.design.as_mut_ptr(), self.index) };
        if status != ffi::OK {
            // SAFETY: a failed materialization retains an error string in this live snapshot.
            let error = unsafe {
                required_str(
                    ffi::opto_slang_module_materialize_error(self.design.as_ptr(), self.index),
                    "module materialization error",
                )
            }?;
            return Err(SlangError::CompileFailed(error.to_string()));
        }
        // SAFETY: successful materialization acquired a lease and initializes the view on success.
        let view = unsafe {
            read("materialized module", |view| {
                ffi::opto_slang_module_view(self.design.as_ptr(), self.index, view)
            })
        };
        let view = match view {
            Ok(view) => view,
            Err(error) => {
                // Materialization acquired one native lease. If the view
                // invariant fails, release it before propagating the error.
                // SAFETY: the matching materialization above acquired exactly one live lease.
                unsafe { ffi::opto_slang_module_release(self.design.as_mut_ptr(), self.index) };
                return Err(error);
            }
        };
        Ok(SlangMaterializedModule { module: self, view })
    }
}

/// RAII lease exposing the fully materialized contents of one module.
///
/// Dropping this value releases the matching native lease.
#[derive(Debug)]
pub struct SlangMaterializedModule<'a> {
    module: SlangModule<'a>,
    view: ffi::ModuleView,
}

impl SlangMaterializedModule<'_> {
    /// Returns the elaborated module name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the leased module's native name is malformed.
    pub fn name(&self) -> Result<&str, SlangError> {
        self.module.name()
    }

    /// Returns the deterministic source-order key.
    #[must_use]
    pub fn source_order(&self) -> u64 {
        self.module.source_order()
    }

    /// Iterates over evaluated attributes attached to the source definition.
    #[must_use]
    pub fn attributes(&self) -> impl ExactSizeIterator<Item = SlangAttribute<'_>> {
        (0..self.view.attribute_count).map(|index| SlangAttribute::from_module(self.module, index))
    }

    /// Iterates over ports in declaration order.
    #[must_use]
    pub fn ports(&self) -> impl ExactSizeIterator<Item = SlangPort<'_>> {
        (0..self.view.port_count).map(|index| SlangPort::from_module(self.module, index))
    }

    /// Iterates over nets in declaration order.
    #[must_use]
    pub fn nets(&self) -> impl ExactSizeIterator<Item = SlangNet<'_>> {
        (0..self.view.net_count).map(|index| SlangNet::from_module(self.module, index))
    }

    /// Iterates over child instances in declaration order.
    #[must_use]
    pub fn instances(&self) -> impl ExactSizeIterator<Item = SlangInstance<'_>> {
        (0..self.view.instance_count).map(|index| SlangInstance::from_module(self.module, index))
    }

    /// Iterates over continuous assignments in source order.
    #[must_use]
    pub fn assigns(&self) -> impl ExactSizeIterator<Item = SlangContinuousAssign<'_>> {
        (0..self.view.assign_count)
            .map(|index| SlangContinuousAssign::from_module(self.module, index))
    }

    /// Iterates over procedural blocks in source order.
    #[must_use]
    pub fn procedures(&self) -> impl ExactSizeIterator<Item = SlangProcedure<'_>> {
        (0..self.view.procedure_count).map(|index| SlangProcedure::from_module(self.module, index))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed evaluated `SystemVerilog` attribute attached to a module definition.
pub struct SlangAttribute<'a> {
    view: ffi::AttributeView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangAttribute<'a> {
    fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the attribute count from this materialized module.
        let view = unsafe {
            read_invariant("module attribute", |view| {
                ffi::opto_slang_module_attribute_view(
                    module.design.as_ptr(),
                    module.index,
                    index,
                    view,
                )
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    fn from_port(port: *const ffi::Port, index: usize) -> Self {
        // SAFETY: `port` belongs to the leased snapshot and `index` is bounded by its attribute count.
        let view = unsafe {
            read_invariant("port attribute", |view| {
                ffi::opto_slang_port_attribute_view(port, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    fn from_net(net: *const ffi::Net, index: usize) -> Self {
        // SAFETY: `net` belongs to the leased snapshot and `index` is bounded by its attribute count.
        let view = unsafe {
            read_invariant("net attribute", |view| {
                ffi::opto_slang_net_attribute_view(net, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    fn from_instance(instance: *const ffi::Instance, index: usize) -> Self {
        // SAFETY: `instance` belongs to the leased snapshot and `index` is bounded by its attribute count.
        let view = unsafe {
            read_invariant("instance attribute", |view| {
                ffi::opto_slang_instance_attribute_view(instance, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the attribute name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native name is null or invalid UTF-8.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: attribute strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.name, "attribute name") }
    }

    /// Returns the evaluated attribute value.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for a missing value string or unknown value tag.
    pub fn value(self) -> Result<SlangAttributeValue<'a>, SlangError> {
        // SAFETY: attribute strings remain owned by the leased snapshot for `'a`.
        let value = unsafe { required_str(self.view.value, "attribute value")? };
        match self.view.kind {
            ffi::ATTRIBUTE_INTEGER => Ok(SlangAttributeValue::Integer {
                bits: value,
                width: self.view.integer_width,
                signed: has_flag(self.view.integer_signed),
            }),
            ffi::ATTRIBUTE_STRING => Ok(SlangAttributeValue::String(value)),
            ffi::ATTRIBUTE_OTHER => Ok(SlangAttributeValue::Other(value)),
            raw => Err(convert::unknown_enum("attribute value kind", raw)),
        }
    }

    /// Returns Slang's constant truth value for directive-style attributes.
    #[must_use]
    pub fn is_true(self) -> bool {
        has_flag(self.view.is_true)
    }

    /// Returns the source location of the attribute name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Evaluated constant payload retained for one `SystemVerilog` attribute.
pub enum SlangAttributeValue<'a> {
    /// Exact most-significant-first four-state integer bits.
    Integer {
        /// Most-significant-first four-state bit text.
        bits: &'a str,
        /// Evaluated integer width.
        width: u32,
        /// Whether the integer uses signed interpretation.
        signed: bool,
    },
    /// Evaluated string contents.
    String(&'a str),
    /// Canonical text for constant forms not yet represented structurally.
    Other(&'a str),
}

impl Drop for SlangMaterializedModule<'_> {
    fn drop(&mut self) {
        // SAFETY: this value owns the single lease acquired by `materialize` and releases it once.
        unsafe {
            ffi::opto_slang_module_release(self.module.design.as_mut_ptr(), self.module.index);
        };
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one elaborated module port.
pub struct SlangPort<'a> {
    view: ffi::PortView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangPort<'a> {
    fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the port count from this materialized module.
        let view = unsafe {
            read_invariant("port", |view| {
                ffi::opto_slang_port_view(module.design.as_ptr(), module.index, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the elaborated port name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native port name is null or invalid UTF-8.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: port-view strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.name, "port name") }
    }

    /// Returns the port direction.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native direction tag.
    pub fn direction(self) -> Result<SlangPortDirection, SlangError> {
        map_port_direction(self.view.direction)
    }

    #[must_use]
    /// Returns the flattened bit width.
    pub fn width(self) -> u32 {
        self.view.width
    }

    /// Returns whether arithmetic on the port uses signed interpretation.
    #[must_use]
    pub fn is_signed(self) -> bool {
        has_flag(self.view.is_signed)
    }

    /// Returns the net-resolution policy applied to the port.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native resolution tag.
    pub fn resolution(self) -> Result<crate::SlangNetResolution, SlangError> {
        convert::map_net_resolution(self.view.resolution)
    }

    /// Returns the complete source type layout.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native layout pointer is null.
    pub fn type_layout(self) -> Result<SlangTypeLayout<'a>, SlangError> {
        SlangTypeLayout::from_raw(self.view.type_layout, "port type layout")
    }

    /// Iterates over evaluated attributes attached to this port declaration.
    #[must_use]
    pub fn attributes(self) -> impl ExactSizeIterator<Item = SlangAttribute<'a>> {
        (0..self.view.attribute_count)
            .map(move |index| SlangAttribute::from_port(self.view.identity, index))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one elaborated module net or variable.
pub struct SlangNet<'a> {
    view: ffi::NetView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangNet<'a> {
    fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the net count from this materialized module.
        let view = unsafe {
            read_invariant("net", |view| {
                ffi::opto_slang_net_view(module.design.as_ptr(), module.index, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the elaborated net or variable name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native net name is null or invalid UTF-8.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: net-view strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.name, "net name") }
    }

    #[must_use]
    /// Returns the flattened bit width.
    pub fn width(self) -> u32 {
        self.view.width
    }

    /// Returns whether the complete value is signed.
    #[must_use]
    pub fn is_signed(self) -> bool {
        has_flag(self.view.is_signed)
    }

    /// Returns whether array elements are signed.
    #[must_use]
    pub fn element_is_signed(self) -> bool {
        has_flag(self.view.element_is_signed)
    }

    /// Returns whether the declaration is local to a procedure.
    #[must_use]
    pub fn is_process_local(self) -> bool {
        has_flag(self.view.is_process_local)
    }

    /// Returns the net-resolution policy.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native resolution tag.
    pub fn resolution(self) -> Result<crate::SlangNetResolution, SlangError> {
        convert::map_net_resolution(self.view.resolution)
    }

    /// Returns the source type layout, when retained by the bridge.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if a present native layout pointer is invalid.
    pub fn type_layout(self) -> Result<Option<SlangTypeLayout<'a>>, SlangError> {
        SlangTypeLayout::from_optional_raw(self.view.type_layout)
    }

    /// Iterates over evaluated attributes attached to this net or variable declaration.
    #[must_use]
    pub fn attributes(self) -> impl ExactSizeIterator<Item = SlangAttribute<'a>> {
        (0..self.view.attribute_count)
            .map(move |index| SlangAttribute::from_net(self.view.identity, index))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one elaborated child instance.
pub struct SlangInstance<'a> {
    view: ffi::InstanceView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangInstance<'a> {
    fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the instance count from this materialized module.
        let view = unsafe {
            read_invariant("instance", |view| {
                ffi::opto_slang_instance_view(module.design.as_ptr(), module.index, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the elaborated instance name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native instance name is null or invalid UTF-8.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: instance-view strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.name, "instance name") }
    }

    /// Returns the instantiated module definition name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native definition name is null or invalid UTF-8.
    pub fn module_name(self) -> Result<&'a str, SlangError> {
        // SAFETY: instance-view strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.module_name, "instance module name") }
    }

    /// Iterates over port connections in elaborated order.
    #[must_use]
    pub fn connections(self) -> impl ExactSizeIterator<Item = SlangInstanceConnection<'a>> {
        (0..self.view.connection_count)
            .map(move |index| SlangInstanceConnection::from_instance(self.view.identity, index))
    }

    /// Iterates over evaluated attributes attached to this instance occurrence.
    #[must_use]
    pub fn attributes(self) -> impl ExactSizeIterator<Item = SlangAttribute<'a>> {
        (0..self.view.attribute_count)
            .map(move |index| SlangAttribute::from_instance(self.view.identity, index))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one instance port connection.
pub struct SlangInstanceConnection<'a> {
    view: ffi::ConnectionView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangInstanceConnection<'a> {
    fn from_instance(raw: *const ffi::Instance, index: usize) -> Self {
        // SAFETY: `raw` belongs to the leased snapshot and `index` is bounded by its connection count.
        let view = unsafe {
            read_invariant("instance connection", |view| {
                ffi::opto_slang_connection_view(raw, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the connected port name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native port name is null or invalid UTF-8.
    pub fn port(self) -> Result<&'a str, SlangError> {
        // SAFETY: connection-view strings remain owned by the leased snapshot for `'a`.
        unsafe { required_str(self.view.port, "instance connection port name") }
    }

    /// Returns the elaborated connection expression.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native expression pointer is null.
    pub fn expression(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.expression, "instance connection expression")
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one continuous assignment.
pub struct SlangContinuousAssign<'a> {
    view: ffi::AssignView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangContinuousAssign<'a> {
    fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the assignment count from this materialized module.
        let view = unsafe {
            read_invariant("continuous assignment", |view| {
                ffi::opto_slang_assign_view(module.design.as_ptr(), module.index, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the assignment left-hand side.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native left-hand expression is null.
    pub fn lhs(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.lhs, "continuous assignment lhs")
    }

    /// Returns the assignment right-hand side.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native right-hand expression is null.
    pub fn rhs(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.rhs, "continuous assignment rhs")
    }
}
