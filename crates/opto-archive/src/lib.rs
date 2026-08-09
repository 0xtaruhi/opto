// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    clippy::missing_errors_doc,
    reason = "the public archive operations share one explicit ArchiveError model"
)]

//! Validated, deterministic binary archives for Opto's Serde domain models.
//!
//! Domain crates retain their existing Serde representations, including
//! canonical borrowed views used for fingerprints. This crate maps that data
//! model into a flat, post-order arena and archives the arena with rkyv. The
//! flat representation avoids recursive archived pointers, permits complete
//! structural validation before Serde sees a value, and keeps every allocation
//! bounded by bytes already validated by rkyv and bytecheck.

use serde::Serialize as SerdeSerialize;
use serde::de::{
    DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::ser::{
    SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use std::fmt::{self, Display};
use std::io::{Read, Write};

const FORMAT_VERSION: u32 = 1;
const MAX_NESTING_DEPTH: usize = 128;

/// Failure while constructing, validating, or reading an Opto archive.
#[derive(Debug)]
pub struct ArchiveError {
    message: String,
}

impl ArchiveError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArchiveError {}

impl serde::ser::Error for ArchiveError {
    fn custom<T: Display>(message: T) -> Self {
        Self::new(message.to_string())
    }
}

impl serde::de::Error for ArchiveError {
    fn custom<T: Display>(message: T) -> Self {
        Self::new(message.to_string())
    }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct Document {
    format_version: u32,
    root: u64,
    nodes: Vec<Node>,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
enum Node {
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    F32(f32),
    F64(f64),
    Char(char),
    String(String),
    Bytes(Vec<u8>),
    None,
    Some(u64),
    Unit,
    Sequence(Vec<u64>),
    Map(Vec<(u64, u64)>),
    UnitVariant(u32),
    NewtypeVariant { variant: u32, value: u64 },
    SequenceVariant { variant: u32, fields: Vec<u64> },
}

/// Serializes a value into the versioned, bytechecked rkyv archive format.
pub fn to_bytes<T>(value: &T) -> Result<Vec<u8>, ArchiveError>
where
    T: SerdeSerialize + ?Sized,
{
    let mut serializer = DocumentSerializer::default();
    let root = value.serialize(&mut serializer)?;
    let document = Document {
        format_version: FORMAT_VERSION,
        root,
        nodes: serializer.nodes,
    };
    validate_document(&document)?;
    rkyv::to_bytes::<rkyv::rancor::Error>(&document)
        .map(rkyv::util::AlignedVec::into_vec)
        .map_err(|error| ArchiveError::new(format!("rkyv serialization failed: {error}")))
}

/// Serializes a value and writes the complete archive to `writer`.
pub fn encode_into_std_write<T, W>(value: &T, writer: &mut W) -> Result<usize, ArchiveError>
where
    T: SerdeSerialize + ?Sized,
    W: Write + ?Sized,
{
    let bytes = to_bytes(value)?;
    writer
        .write_all(&bytes)
        .map_err(|error| ArchiveError::new(format!("archive write failed: {error}")))?;
    Ok(bytes.len())
}

/// Returns the exact encoded size of `value`.
pub fn serialized_size<T>(value: &T) -> Result<usize, ArchiveError>
where
    T: SerdeSerialize + ?Sized,
{
    to_bytes(value).map(|bytes| bytes.len())
}

/// Validates and deserializes one complete archive.
pub fn from_bytes<T>(bytes: &[u8]) -> Result<T, ArchiveError>
where
    T: DeserializeOwned,
{
    let archived = rkyv::access::<ArchivedDocument, rkyv::rancor::Error>(bytes)
        .map_err(|error| ArchiveError::new(format!("rkyv validation failed: {error}")))?;
    validate_archived_document(archived)?;
    let document = rkyv::deserialize::<Document, rkyv::rancor::Error>(archived)
        .map_err(|error| ArchiveError::new(format!("rkyv deserialization failed: {error}")))?;
    validate_document(&document)?;
    T::deserialize(ValueDeserializer::new(&document, document.root))
}

/// Reads exactly `payload_len` bytes, validates the archive, and deserializes it.
pub fn decode_from_std_read<T, R>(reader: &mut R, payload_len: usize) -> Result<T, ArchiveError>
where
    T: DeserializeOwned,
    R: Read + ?Sized,
{
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(payload_len)
        .map_err(|_| ArchiveError::new("could not allocate archive payload buffer"))?;
    bytes.resize(payload_len, 0);
    reader
        .read_exact(&mut bytes)
        .map_err(|error| ArchiveError::new(format!("archive read failed: {error}")))?;
    from_bytes(&bytes)
}

#[derive(Default)]
struct DocumentSerializer {
    nodes: Vec<Node>,
}

impl DocumentSerializer {
    fn push(&mut self, node: Node) -> Result<u64, ArchiveError> {
        let id = u64::try_from(self.nodes.len())
            .map_err(|_| ArchiveError::new("archive node count exceeds 64-bit capacity"))?;
        self.nodes
            .try_reserve(1)
            .map_err(|_| ArchiveError::new("could not allocate archive node"))?;
        self.nodes.push(node);
        Ok(id)
    }
}

impl<'a> serde::Serializer for &'a mut DocumentSerializer {
    type Ok = u64;
    type Error = ArchiveError;
    type SerializeSeq = SequenceSerializer<'a>;
    type SerializeTuple = SequenceSerializer<'a>;
    type SerializeTupleStruct = SequenceSerializer<'a>;
    type SerializeTupleVariant = VariantSequenceSerializer<'a>;
    type SerializeMap = MapSerializer<'a>;
    type SerializeStruct = SequenceSerializer<'a>;
    type SerializeStructVariant = VariantSequenceSerializer<'a>;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        self.push(Node::Bool(value))
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        self.push(Node::I8(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        self.push(Node::I16(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        self.push(Node::I32(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        self.push(Node::I64(value))
    }

    fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
        self.push(Node::I128(value))
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        self.push(Node::U8(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        self.push(Node::U16(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        self.push(Node::U32(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        self.push(Node::U64(value))
    }

    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        self.push(Node::U128(value))
    }

    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        self.push(Node::F32(value))
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        self.push(Node::F64(value))
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        self.push(Node::Char(value))
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        self.push(Node::String(value.to_owned()))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.push(Node::Bytes(value.to_vec()))
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.push(Node::None)
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        let value = value.serialize(&mut *self)?;
        self.push(Node::Some(value))
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.push(Node::Unit)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.push(Node::Unit)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.push(Node::UnitVariant(variant_index))
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        let value = value.serialize(&mut *self)?;
        self.push(Node::NewtypeVariant {
            variant: variant_index,
            value,
        })
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        SequenceSerializer::new(self, len)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        SequenceSerializer::new(self, Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        SequenceSerializer::new(self, Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        VariantSequenceSerializer::new(self, variant_index, len)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        MapSerializer::new(self, len)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        SequenceSerializer::new(self, Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        VariantSequenceSerializer::new(self, variant_index, len)
    }

    fn collect_str<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: Display + ?Sized,
    {
        self.serialize_str(&value.to_string())
    }
}

struct SequenceSerializer<'a> {
    serializer: &'a mut DocumentSerializer,
    fields: Vec<u64>,
}

impl<'a> SequenceSerializer<'a> {
    fn new(
        serializer: &'a mut DocumentSerializer,
        len: Option<usize>,
    ) -> Result<Self, ArchiveError> {
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(len.unwrap_or(0))
            .map_err(|_| ArchiveError::new("could not allocate archive sequence"))?;
        Ok(Self { serializer, fields })
    }

    fn push<T>(&mut self, value: &T) -> Result<(), ArchiveError>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.fields.push(value.serialize(&mut *self.serializer)?);
        Ok(())
    }

    fn finish(self) -> Result<u64, ArchiveError> {
        self.serializer.push(Node::Sequence(self.fields))
    }
}

impl SerializeSeq for SequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

impl SerializeTuple for SequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

impl SerializeTupleStruct for SequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

impl SerializeStruct for SequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

struct VariantSequenceSerializer<'a> {
    serializer: &'a mut DocumentSerializer,
    variant: u32,
    fields: Vec<u64>,
}

impl<'a> VariantSequenceSerializer<'a> {
    fn new(
        serializer: &'a mut DocumentSerializer,
        variant: u32,
        len: usize,
    ) -> Result<Self, ArchiveError> {
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(len)
            .map_err(|_| ArchiveError::new("could not allocate archive variant"))?;
        Ok(Self {
            serializer,
            variant,
            fields,
        })
    }

    fn push<T>(&mut self, value: &T) -> Result<(), ArchiveError>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.fields.push(value.serialize(&mut *self.serializer)?);
        Ok(())
    }

    fn finish(self) -> Result<u64, ArchiveError> {
        self.serializer.push(Node::SequenceVariant {
            variant: self.variant,
            fields: self.fields,
        })
    }
}

impl SerializeTupleVariant for VariantSequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

impl SerializeStructVariant for VariantSequenceSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

struct MapSerializer<'a> {
    serializer: &'a mut DocumentSerializer,
    entries: Vec<(u64, u64)>,
    pending_key: Option<u64>,
}

impl<'a> MapSerializer<'a> {
    fn new(
        serializer: &'a mut DocumentSerializer,
        len: Option<usize>,
    ) -> Result<Self, ArchiveError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(len.unwrap_or(0))
            .map_err(|_| ArchiveError::new("could not allocate archive map"))?;
        Ok(Self {
            serializer,
            entries,
            pending_key: None,
        })
    }
}

impl SerializeMap for MapSerializer<'_> {
    type Ok = u64;
    type Error = ArchiveError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        if self.pending_key.is_some() {
            return Err(ArchiveError::new("map serialized two keys without a value"));
        }
        self.pending_key = Some(key.serialize(&mut *self.serializer)?);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: SerdeSerialize + ?Sized,
    {
        let key = self
            .pending_key
            .take()
            .ok_or_else(|| ArchiveError::new("map value was serialized before its key"))?;
        let value = value.serialize(&mut *self.serializer)?;
        self.entries.push((key, value));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        if self.pending_key.is_some() {
            return Err(ArchiveError::new(
                "map ended without a value for its final key",
            ));
        }
        self.serializer.push(Node::Map(self.entries))
    }
}

fn validate_document(document: &Document) -> Result<(), ArchiveError> {
    if document.format_version != FORMAT_VERSION {
        return Err(ArchiveError::new(format!(
            "unsupported Opto archive format {}",
            document.format_version
        )));
    }
    let node_count = u64::try_from(document.nodes.len())
        .map_err(|_| ArchiveError::new("archive node count exceeds 64-bit capacity"))?;
    if node_count == 0 || document.root != node_count - 1 {
        return Err(ArchiveError::new(
            "archive root is not the final post-order node",
        ));
    }
    let mut parents = vec![0u8; document.nodes.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        let parent = u64::try_from(index).expect("usize fits in u64 on supported targets");
        for child in node.children() {
            if child >= parent {
                return Err(ArchiveError::new(
                    "archive child is not earlier than its post-order parent",
                ));
            }
            let slot = usize::try_from(child)
                .ok()
                .and_then(|child| parents.get_mut(child))
                .ok_or_else(|| ArchiveError::new("archive child index is out of bounds"))?;
            *slot = slot
                .checked_add(1)
                .ok_or_else(|| ArchiveError::new("archive node parent count overflow"))?;
            if *slot != 1 {
                return Err(ArchiveError::new(
                    "archive node is referenced by more than one parent",
                ));
            }
        }
    }
    let root = usize::try_from(document.root).expect("validated root fits usize");
    if parents[root] != 0 || parents[..root].iter().any(|&count| count != 1) {
        return Err(ArchiveError::new(
            "archive contains unreachable nodes or an invalid root parent",
        ));
    }
    validate_depth(document, document.root, 0)
}

fn validate_archived_document(document: &ArchivedDocument) -> Result<(), ArchiveError> {
    if document.format_version.to_native() != FORMAT_VERSION {
        return Err(ArchiveError::new(format!(
            "unsupported Opto archive format {}",
            document.format_version.to_native()
        )));
    }
    let node_count = u64::try_from(document.nodes.len())
        .map_err(|_| ArchiveError::new("archive node count exceeds 64-bit capacity"))?;
    let root = document.root.to_native();
    if node_count == 0 || root != node_count - 1 {
        return Err(ArchiveError::new(
            "archive root is not the final post-order node",
        ));
    }
    let mut parents = vec![0u8; document.nodes.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        let parent = u64::try_from(index).expect("usize fits in u64 on supported targets");
        for child in archived_children(node) {
            if child >= parent {
                return Err(ArchiveError::new(
                    "archive child is not earlier than its post-order parent",
                ));
            }
            let slot = usize::try_from(child)
                .ok()
                .and_then(|child| parents.get_mut(child))
                .ok_or_else(|| ArchiveError::new("archive child index is out of bounds"))?;
            *slot = slot
                .checked_add(1)
                .ok_or_else(|| ArchiveError::new("archive node parent count overflow"))?;
            if *slot != 1 {
                return Err(ArchiveError::new(
                    "archive node is referenced by more than one parent",
                ));
            }
        }
    }
    let root = usize::try_from(root).expect("validated root fits usize");
    if parents[root] != 0 || parents[..root].iter().any(|&count| count != 1) {
        return Err(ArchiveError::new(
            "archive contains unreachable nodes or an invalid root parent",
        ));
    }
    validate_archived_depth(document, document.root.to_native(), 0)
}

fn validate_archived_depth(
    document: &ArchivedDocument,
    node: u64,
    depth: usize,
) -> Result<(), ArchiveError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(ArchiveError::new(
            "archive nesting depth exceeds the configured limit",
        ));
    }
    let node = document
        .nodes
        .get(usize::try_from(node).map_err(|_| ArchiveError::new("invalid archive node"))?)
        .ok_or_else(|| ArchiveError::new("invalid archive node"))?;
    for child in archived_children(node) {
        validate_archived_depth(document, child, depth + 1)?;
    }
    Ok(())
}

fn archived_children(node: &ArchivedNode) -> Box<dyn Iterator<Item = u64> + '_> {
    match node {
        ArchivedNode::Some(value) | ArchivedNode::NewtypeVariant { value, .. } => {
            Box::new(std::iter::once(value.to_native()))
        }
        ArchivedNode::Sequence(fields) | ArchivedNode::SequenceVariant { fields, .. } => Box::new(
            fields
                .iter()
                .copied()
                .map(rkyv::rend::unaligned::u64_ule::to_native),
        ),
        ArchivedNode::Map(entries) => Box::new(
            entries
                .iter()
                .flat_map(|entry| [entry.0.to_native(), entry.1.to_native()]),
        ),
        _ => Box::new(std::iter::empty()),
    }
}

fn validate_depth(document: &Document, node: u64, depth: usize) -> Result<(), ArchiveError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(ArchiveError::new(
            "archive nesting depth exceeds the configured limit",
        ));
    }
    let node = document
        .nodes
        .get(usize::try_from(node).map_err(|_| ArchiveError::new("invalid archive node"))?)
        .ok_or_else(|| ArchiveError::new("invalid archive node"))?;
    for child in node.children() {
        validate_depth(document, child, depth + 1)?;
    }
    Ok(())
}

impl Node {
    fn children(&self) -> Box<dyn Iterator<Item = u64> + '_> {
        match self {
            Self::Some(value) | Self::NewtypeVariant { value, .. } => {
                Box::new(std::iter::once(*value))
            }
            Self::Sequence(fields) | Self::SequenceVariant { fields, .. } => {
                Box::new(fields.iter().copied())
            }
            Self::Map(entries) => Box::new(entries.iter().flat_map(|&(key, value)| [key, value])),
            _ => Box::new(std::iter::empty()),
        }
    }
}

#[derive(Clone, Copy)]
struct ValueDeserializer<'de> {
    document: &'de Document,
    node: u64,
    depth: usize,
}

impl<'de> ValueDeserializer<'de> {
    fn new(document: &'de Document, node: u64) -> Self {
        Self {
            document,
            node,
            depth: 0,
        }
    }

    fn value(self) -> Result<&'de Node, ArchiveError> {
        self.document
            .nodes
            .get(usize::try_from(self.node).map_err(|_| ArchiveError::new("invalid node"))?)
            .ok_or_else(|| ArchiveError::new("invalid node"))
    }

    fn child(self, node: u64) -> Result<Self, ArchiveError> {
        let depth = self
            .depth
            .checked_add(1)
            .filter(|&depth| depth <= MAX_NESTING_DEPTH)
            .ok_or_else(|| ArchiveError::new("archive nesting depth limit exceeded"))?;
        Ok(Self {
            document: self.document,
            node,
            depth,
        })
    }

    fn sequence(self, fields: &'de [u64]) -> SequenceAccess<'de> {
        SequenceAccess {
            value: self,
            fields,
            next: 0,
        }
    }
}

macro_rules! deserialize_number {
    ($method:ident, $variant:ident, $visit:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.value()? {
                Node::$variant(value) => visitor.$visit(*value),
                _ => Err(ArchiveError::new(concat!(
                    "archive type mismatch for ",
                    stringify!($method)
                ))),
            }
        }
    };
}

impl<'de> serde::Deserializer<'de> for ValueDeserializer<'de> {
    type Error = ArchiveError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Bool(value) => visitor.visit_bool(*value),
            Node::I8(value) => visitor.visit_i8(*value),
            Node::I16(value) => visitor.visit_i16(*value),
            Node::I32(value) => visitor.visit_i32(*value),
            Node::I64(value) => visitor.visit_i64(*value),
            Node::I128(value) => visitor.visit_i128(*value),
            Node::U8(value) => visitor.visit_u8(*value),
            Node::U16(value) => visitor.visit_u16(*value),
            Node::U32(value) => visitor.visit_u32(*value),
            Node::U64(value) => visitor.visit_u64(*value),
            Node::U128(value) => visitor.visit_u128(*value),
            Node::F32(value) => visitor.visit_f32(*value),
            Node::F64(value) => visitor.visit_f64(*value),
            Node::Char(value) => visitor.visit_char(*value),
            Node::String(value) => visitor.visit_borrowed_str(value),
            Node::Bytes(value) => visitor.visit_borrowed_bytes(value),
            Node::None => visitor.visit_none(),
            Node::Some(value) => visitor.visit_some(self.child(*value)?),
            Node::Unit => visitor.visit_unit(),
            Node::Sequence(fields) => visitor.visit_seq(self.sequence(fields)),
            Node::Map(entries) => visitor.visit_map(MapValueAccess::new(self, entries)),
            Node::UnitVariant(_) | Node::NewtypeVariant { .. } | Node::SequenceVariant { .. } => {
                self.deserialize_enum("", &[], visitor)
            }
        }
    }

    deserialize_number!(deserialize_i8, I8, visit_i8);
    deserialize_number!(deserialize_i16, I16, visit_i16);
    deserialize_number!(deserialize_i32, I32, visit_i32);
    deserialize_number!(deserialize_i64, I64, visit_i64);
    deserialize_number!(deserialize_i128, I128, visit_i128);
    deserialize_number!(deserialize_u8, U8, visit_u8);
    deserialize_number!(deserialize_u16, U16, visit_u16);
    deserialize_number!(deserialize_u32, U32, visit_u32);
    deserialize_number!(deserialize_u64, U64, visit_u64);
    deserialize_number!(deserialize_u128, U128, visit_u128);
    deserialize_number!(deserialize_f32, F32, visit_f32);
    deserialize_number!(deserialize_f64, F64, visit_f64);
    deserialize_number!(deserialize_char, Char, visit_char);

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Bool(value) => visitor.visit_bool(*value),
            _ => Err(ArchiveError::new("archive type mismatch for bool")),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::String(value) => visitor.visit_borrowed_str(value),
            _ => Err(ArchiveError::new("archive type mismatch for string")),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::String(value) => visitor.visit_string(value.clone()),
            _ => Err(ArchiveError::new("archive type mismatch for string")),
        }
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Bytes(value) => visitor.visit_borrowed_bytes(value),
            _ => Err(ArchiveError::new("archive type mismatch for bytes")),
        }
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Bytes(value) => visitor.visit_byte_buf(value.clone()),
            _ => Err(ArchiveError::new("archive type mismatch for bytes")),
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::None => visitor.visit_none(),
            Node::Some(value) => visitor.visit_some(self.child(*value)?),
            _ => Err(ArchiveError::new("archive type mismatch for option")),
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Unit => visitor.visit_unit(),
            _ => Err(ArchiveError::new("archive type mismatch for unit")),
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Sequence(fields) => visitor.visit_seq(self.sequence(fields)),
            _ => Err(ArchiveError::new("archive type mismatch for sequence")),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::Map(entries) => visitor.visit_map(MapValueAccess::new(self, entries)),
            _ => Err(ArchiveError::new("archive type mismatch for map")),
        }
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value()? {
            Node::UnitVariant(variant) => visitor.visit_enum(ArchiveEnumAccess {
                value: self,
                variant: *variant,
                shape: VariantShape::Unit,
            }),
            Node::NewtypeVariant { variant, value } => visitor.visit_enum(ArchiveEnumAccess {
                value: self,
                variant: *variant,
                shape: VariantShape::Newtype(*value),
            }),
            Node::SequenceVariant { variant, fields } => visitor.visit_enum(ArchiveEnumAccess {
                value: self,
                variant: *variant,
                shape: VariantShape::Sequence(fields),
            }),
            _ => Err(ArchiveError::new("archive type mismatch for enum")),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_any(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }
}

struct SequenceAccess<'de> {
    value: ValueDeserializer<'de>,
    fields: &'de [u64],
    next: usize,
}

impl<'de> SeqAccess<'de> for SequenceAccess<'de> {
    type Error = ArchiveError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(&node) = self.fields.get(self.next) else {
            return Ok(None);
        };
        self.next += 1;
        seed.deserialize(self.value.child(node)?).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.fields.len() - self.next)
    }
}

struct MapValueAccess<'de> {
    value: ValueDeserializer<'de>,
    entries: &'de [(u64, u64)],
    next: usize,
    pending_value: Option<u64>,
}

impl<'de> MapValueAccess<'de> {
    fn new(value: ValueDeserializer<'de>, entries: &'de [(u64, u64)]) -> Self {
        Self {
            value,
            entries,
            next: 0,
            pending_value: None,
        }
    }
}

impl<'de> MapAccess<'de> for MapValueAccess<'de> {
    type Error = ArchiveError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        if self.pending_value.is_some() {
            return Err(ArchiveError::new("map key requested before pending value"));
        }
        let Some(&(key, value)) = self.entries.get(self.next) else {
            return Ok(None);
        };
        self.pending_value = Some(value);
        seed.deserialize(self.value.child(key)?).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let value = self
            .pending_value
            .take()
            .ok_or_else(|| ArchiveError::new("map value requested before key"))?;
        self.next += 1;
        seed.deserialize(self.value.child(value)?)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len() - self.next)
    }
}

enum VariantShape<'de> {
    Unit,
    Newtype(u64),
    Sequence(&'de [u64]),
}

struct ArchiveEnumAccess<'de> {
    value: ValueDeserializer<'de>,
    variant: u32,
    shape: VariantShape<'de>,
}

impl<'de> EnumAccess<'de> for ArchiveEnumAccess<'de> {
    type Error = ArchiveError;
    type Variant = ArchiveVariantAccess<'de>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(serde::de::value::U32Deserializer::<ArchiveError>::new(
            self.variant,
        ))?;
        Ok((
            variant,
            ArchiveVariantAccess {
                value: self.value,
                shape: self.shape,
            },
        ))
    }
}

struct ArchiveVariantAccess<'de> {
    value: ValueDeserializer<'de>,
    shape: VariantShape<'de>,
}

impl<'de> VariantAccess<'de> for ArchiveVariantAccess<'de> {
    type Error = ArchiveError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        match self.shape {
            VariantShape::Unit => Ok(()),
            _ => Err(ArchiveError::new("enum variant is not a unit variant")),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.shape {
            VariantShape::Newtype(node) => seed.deserialize(self.value.child(node)?),
            _ => Err(ArchiveError::new("enum variant is not a newtype variant")),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.shape {
            VariantShape::Sequence(fields) => visitor.visit_seq(self.value.sequence(fields)),
            _ => Err(ArchiveError::new("enum variant is not a tuple variant")),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.shape {
            VariantShape::Sequence(fields) => visitor.visit_seq(self.value.sequence(fields)),
            _ => Err(ArchiveError::new("enum variant is not a struct variant")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum ExampleEnum {
        Unit,
        Newtype(String),
        Tuple(u32, bool),
        Struct {
            bytes: Vec<u8>,
            optional: Option<i64>,
        },
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Example {
        name: String,
        values: Vec<ExampleEnum>,
        map: BTreeMap<String, u128>,
    }

    #[test]
    fn round_trips_the_complete_serde_data_model_used_by_opto() {
        let value = Example {
            name: "archive".to_string(),
            values: vec![
                ExampleEnum::Unit,
                ExampleEnum::Newtype("value".to_string()),
                ExampleEnum::Tuple(42, true),
                ExampleEnum::Struct {
                    bytes: vec![1, 2, 3],
                    optional: Some(-7),
                },
            ],
            map: BTreeMap::from([("wide".to_string(), u128::MAX)]),
        };
        let encoded = to_bytes(&value).unwrap();
        let decoded: Example = from_bytes(&encoded).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn rejects_corrupted_archives() {
        let mut encoded = to_bytes(&vec![1u64, 2, 3]).unwrap();
        encoded[0] ^= 0xff;
        assert!(from_bytes::<Vec<u64>>(&encoded).is_err());
    }

    #[test]
    fn encoding_is_deterministic() {
        let value = ExampleEnum::Tuple(12, false);
        assert_eq!(to_bytes(&value).unwrap(), to_bytes(&value).unwrap());
    }
}
