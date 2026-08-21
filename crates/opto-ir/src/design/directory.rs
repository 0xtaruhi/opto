// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Persistent stable-ID directory used by immutable design revisions.

use std::sync::Arc;

const LEAF_CAPACITY: usize = 4;
const RADIX_BITS: usize = 4;
const MAX_DEPTH: usize = 256 / RADIX_BITS;

pub(super) trait StableKey: Copy + Ord {
    fn bytes(self) -> [u8; 32];
}

#[derive(Debug, Clone)]
pub(super) struct PersistentDirectory<K, V> {
    root: Option<Arc<Node<K, V>>>,
    len: usize,
}

#[derive(Debug)]
enum Node<K, V> {
    Leaf(Box<[(K, V)]>),
    Branch {
        bitmap: u16,
        children: Box<[Arc<Self>]>,
    },
}

impl<K, V> Default for PersistentDirectory<K, V> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<K, V> PersistentDirectory<K, V>
where
    K: StableKey,
    V: Copy,
{
    pub(super) fn get(&self, key: K) -> Option<V> {
        let bytes = key.bytes();
        let mut node = self.root.as_deref()?;
        let mut depth = 0usize;
        loop {
            match node {
                Node::Leaf(entries) => {
                    return entries
                        .binary_search_by_key(&key, |(stored, _)| *stored)
                        .ok()
                        .map(|index| entries[index].1);
                }
                Node::Branch { bitmap, children } => {
                    let slot = key_slot(&bytes, depth);
                    let mask = 1u16 << slot;
                    if bitmap & mask == 0 {
                        return None;
                    }
                    let child = (bitmap & (mask - 1)).count_ones() as usize;
                    node = &children[child];
                    depth += 1;
                }
            }
        }
    }

    pub(super) fn insert(&self, key: K, value: V) -> (Self, Option<V>) {
        let bytes = key.bytes();
        let (root, previous) = insert_node(self.root.as_ref(), key, value, &bytes, 0);
        (
            Self {
                root: Some(root),
                len: self.len + usize::from(previous.is_none()),
            },
            previous,
        )
    }

    #[cfg(test)]
    pub(super) const fn len(&self) -> usize {
        self.len
    }
}

fn insert_node<K, V>(
    node: Option<&Arc<Node<K, V>>>,
    key: K,
    value: V,
    bytes: &[u8; 32],
    depth: usize,
) -> (Arc<Node<K, V>>, Option<V>)
where
    K: StableKey,
    V: Copy,
{
    let Some(node) = node else {
        return (Arc::new(Node::Leaf(Box::new([(key, value)]))), None);
    };
    match node.as_ref() {
        Node::Leaf(entries) => match entries.binary_search_by_key(&key, |(stored, _)| *stored) {
            Ok(index) => {
                let mut replacement = entries.to_vec();
                let previous = replacement[index].1;
                replacement[index].1 = value;
                (
                    Arc::new(Node::Leaf(replacement.into_boxed_slice())),
                    Some(previous),
                )
            }
            Err(index) if entries.len() < LEAF_CAPACITY || depth == MAX_DEPTH => {
                let mut replacement = entries.to_vec();
                replacement.insert(index, (key, value));
                (Arc::new(Node::Leaf(replacement.into_boxed_slice())), None)
            }
            Err(_) => {
                let mut branch = Arc::new(Node::Branch {
                    bitmap: 0,
                    children: Box::new([]),
                });
                for &(stored_key, stored_value) in entries {
                    let stored_bytes = stored_key.bytes();
                    branch = insert_node(
                        Some(&branch),
                        stored_key,
                        stored_value,
                        &stored_bytes,
                        depth,
                    )
                    .0;
                }
                insert_node(Some(&branch), key, value, bytes, depth)
            }
        },
        Node::Branch { bitmap, children } => {
            let slot = key_slot(bytes, depth);
            let mask = 1u16 << slot;
            let child = (bitmap & (mask - 1)).count_ones() as usize;
            let existing = (bitmap & mask != 0).then(|| &children[child]);
            let (replacement, previous) =
                insert_node(existing, key, value, bytes, depth.saturating_add(1));
            let mut next_children = children.to_vec();
            let next_bitmap = if existing.is_some() {
                next_children[child] = replacement;
                *bitmap
            } else {
                next_children.insert(child, replacement);
                *bitmap | mask
            };
            (
                Arc::new(Node::Branch {
                    bitmap: next_bitmap,
                    children: next_children.into_boxed_slice(),
                }),
                previous,
            )
        }
    }
}

fn key_slot(bytes: &[u8; 32], depth: usize) -> usize {
    let byte = bytes[depth / 2];
    if depth.is_multiple_of(2) {
        usize::from(byte >> RADIX_BITS)
    } else {
        usize::from(byte & 0x0f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct Key([u8; 32]);

    impl StableKey for Key {
        fn bytes(self) -> [u8; 32] {
            self.0
        }
    }

    #[test]
    fn forks_share_untouched_paths_and_keep_prior_values() {
        let mut original = PersistentDirectory::default();
        for value in 0..64u8 {
            let mut bytes = [0; 32];
            bytes[0] = value;
            original = original.insert(Key(bytes), u32::from(value)).0;
        }
        let mut added = [0; 32];
        added[31] = 1;
        let fork = original.insert(Key(added), 1000).0;

        assert_eq!(original.len(), 64);
        assert_eq!(fork.len(), 65);
        assert_eq!(original.get(Key(added)), None);
        assert_eq!(fork.get(Key(added)), Some(1000));
        for value in 0..64u8 {
            let mut bytes = [0; 32];
            bytes[0] = value;
            assert_eq!(fork.get(Key(bytes)), Some(u32::from(value)));
        }
    }

    #[test]
    fn replacement_does_not_change_directory_cardinality() {
        let key = Key([7; 32]);
        let original = PersistentDirectory::default().insert(key, 1).0;
        let (replacement, previous) = original.insert(key, 2);

        assert_eq!(previous, Some(1));
        assert_eq!(replacement.len(), 1);
        assert_eq!(original.get(key), Some(1));
        assert_eq!(replacement.get(key), Some(2));
    }
}
