// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// One ordered entry from a mapping or resolution library selection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LibrarySelector {
    /// Search designs already present in memory at this position in the link order.
    DesignMemory,
    /// Select a loaded Liberty library by its declared name or source-name alias.
    Library(String),
}

impl LibrarySelector {
    #[must_use]
    /// Returns the selector's command-language token.
    pub fn token(&self) -> &str {
        match self {
            Self::DesignMemory => "*",
            Self::Library(name) => name,
        }
    }
}

/// A semantic, ordered mapping or resolution library selection.
///
/// Repeated selectors are retained so ordered resolution can apply stable
/// first-match semantics rather than changing meaning through set conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibrarySelection {
    selectors: Vec<LibrarySelector>,
}

impl LibrarySelection {
    #[must_use]
    /// Parses a whitespace-separated selection while preserving order and duplicates.
    pub fn parse(value: &str) -> Self {
        Self {
            selectors: value
                .split_whitespace()
                .map(|token| {
                    if token == "*" {
                        LibrarySelector::DesignMemory
                    } else {
                        LibrarySelector::Library(token.to_string())
                    }
                })
                .collect(),
        }
    }

    pub(crate) fn from_library_names<'a>(
        names: impl IntoIterator<Item = &'a str>,
        include_design_memory: bool,
    ) -> Self {
        let mut selectors = Vec::new();
        if include_design_memory {
            selectors.push(LibrarySelector::DesignMemory);
        }
        selectors.extend(
            names
                .into_iter()
                .map(|name| LibrarySelector::Library(name.to_string())),
        );
        Self { selectors }
    }

    #[must_use]
    /// Borrows selectors in search order.
    pub fn selectors(&self) -> &[LibrarySelector] {
        &self.selectors
    }

    #[must_use]
    /// Returns `true` when no selectors were provided.
    pub fn is_empty(&self) -> bool {
        self.selectors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_preserves_order_wildcards_and_duplicates() {
        let selection = LibrarySelection::parse("slow.lib * fast slow.lib");

        assert_eq!(
            selection.selectors(),
            [
                LibrarySelector::Library("slow.lib".to_string()),
                LibrarySelector::DesignMemory,
                LibrarySelector::Library("fast".to_string()),
                LibrarySelector::Library("slow.lib".to_string()),
            ]
        );
    }
}
