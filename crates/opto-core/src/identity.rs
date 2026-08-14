// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Process-local ownership identities.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

struct OwnerMarker<Tag>(PhantomData<fn(Tag) -> Tag>);

/// Typed process-local identity shared by one owner and its derived handles.
///
/// Cloning a token preserves the identity. Constructing another token always
/// creates a distinct identity, even for the same `Tag`. The tag prevents
/// unrelated ownership domains from being compared accidentally.
pub struct OwnerToken<Tag> {
    marker: Arc<OwnerMarker<Tag>>,
}

impl<Tag> OwnerToken<Tag> {
    /// Creates a fresh identity for one ownership domain.
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            marker: Arc::new(OwnerMarker(PhantomData)),
        }
    }

    /// Returns whether both tokens were derived from the same owner.
    #[must_use]
    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.marker, &other.marker)
    }
}

impl<Tag> Clone for OwnerToken<Tag> {
    fn clone(&self) -> Self {
        Self {
            marker: Arc::clone(&self.marker),
        }
    }
}

impl<Tag> Default for OwnerToken<Tag> {
    fn default() -> Self {
        Self::fresh()
    }
}

impl<Tag> fmt::Debug for OwnerToken<Tag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("OwnerToken").finish_non_exhaustive()
    }
}

impl<Tag> PartialEq for OwnerToken<Tag> {
    fn eq(&self, other: &Self) -> bool {
        self.same_owner(other)
    }
}

impl<Tag> Eq for OwnerToken<Tag> {}

#[cfg(test)]
mod tests {
    use super::*;

    enum Registry {}

    #[test]
    fn clones_preserve_but_fresh_tokens_change_identity() {
        let owner = OwnerToken::<Registry>::fresh();
        let clone = owner.clone();
        let independent = OwnerToken::<Registry>::fresh();

        assert!(owner.same_owner(&clone));
        assert!(!owner.same_owner(&independent));
    }
}
