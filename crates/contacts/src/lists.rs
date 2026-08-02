//! Named lists and the algebra that combines them (deliverable L-009-06).
//!
//! Spec §11.7 asks for lists that can be filtered and combined by union and
//! intersection. A [`ListSelection`] is that combination, expressed once and
//! handed to the store, which turns it into a single query — rather than
//! resolved here by reading three lists into memory and intersecting them,
//! which is exactly the "load everything" that guide §11.3 forbids.
//!
//! # The empty selection is the trap this module exists to spell out
//!
//! `ListSelection::union([])` selects **nothing**, and
//! [`ListSelection::everything`] selects everything. They are different
//! constructors because they are different intents, and conflating them is how
//! a campaign aimed at one empty list goes out to the whole address book. The
//! type makes the two unrepresentable as one another.

use crate::model::ListId;

/// How the lists of a selection combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Combination {
    /// No list restriction at all: every contact of the store.
    Everything,
    /// A contact of **at least one** of the lists (union).
    Any,
    /// A contact of **every one** of the lists (intersection).
    All,
}

/// Which contacts a caller wants.
///
/// Built through one of the three constructors; a selection with no
/// combination is not representable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSelection {
    combination: Combination,
    lists: Vec<ListId>,
    excluded: Vec<ListId>,
}

impl ListSelection {
    /// Every contact in the store.
    #[must_use]
    pub const fn everything() -> Self {
        Self {
            combination: Combination::Everything,
            lists: Vec::new(),
            excluded: Vec::new(),
        }
    }

    /// Contacts belonging to at least one of `lists` (union).
    ///
    /// An empty `lists` selects **no** contact. See the module note.
    #[must_use]
    pub fn union<I>(lists: I) -> Self
    where
        I: IntoIterator<Item = ListId>,
    {
        Self {
            combination: Combination::Any,
            lists: deduplicated(lists),
            excluded: Vec::new(),
        }
    }

    /// Contacts belonging to every one of `lists` (intersection).
    ///
    /// An empty `lists` selects **no** contact — not everything. An
    /// intersection over nothing is mathematically the universe, and that is
    /// precisely the reading that would turn "the operator picked no list" into
    /// "send to all".
    #[must_use]
    pub fn intersection<I>(lists: I) -> Self
    where
        I: IntoIterator<Item = ListId>,
    {
        Self {
            combination: Combination::All,
            lists: deduplicated(lists),
            excluded: Vec::new(),
        }
    }

    /// The same selection, minus the contacts of `lists`.
    ///
    /// Applied **after** the combination, and always: an exclusion that a
    /// union could override would not be an exclusion.
    #[must_use]
    pub fn excluding<I>(mut self, lists: I) -> Self
    where
        I: IntoIterator<Item = ListId>,
    {
        self.excluded = deduplicated(lists);
        self
    }

    /// How the lists combine.
    #[must_use]
    pub const fn combination(&self) -> Combination {
        self.combination
    }

    /// The lists being combined, without repeats.
    #[must_use]
    pub fn lists(&self) -> &[ListId] {
        &self.lists
    }

    /// The lists being subtracted, without repeats.
    #[must_use]
    pub fn excluded(&self) -> &[ListId] {
        &self.excluded
    }

    /// Whether this selection can match nothing whatever the store holds.
    ///
    /// True for a union or an intersection over no list. An implementor checks
    /// this **before** querying: `COUNT(…) = 0` over an empty intersection is
    /// trivially true for every contact, so an implementation that let the
    /// empty case reach the SQL would return the whole table for a selection
    /// that means the opposite.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !matches!(self.combination, Combination::Everything) && self.lists.is_empty()
    }
}

/// Keeps the first occurrence of each identifier, preserving order.
///
/// A repeated list in an intersection would make the "belongs to as many
/// distinct lists as were asked for" count disagree with the number of
/// identifiers passed, and the intersection would match nothing.
fn deduplicated<I>(lists: I) -> Vec<ListId>
where
    I: IntoIterator<Item = ListId>,
{
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();

    for list in lists {
        if seen.insert(list) {
            kept.push(list);
        }
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::{Combination, ListSelection};
    use crate::model::ListId;

    #[test]
    fn everything_restricts_nothing() {
        let selection = ListSelection::everything();

        assert_eq!(selection.combination(), Combination::Everything);
        assert!(selection.lists().is_empty());
        assert!(!selection.is_empty(), "everything is not the empty set");
    }

    /// The trap this module exists for: a union over no list must select no
    /// contact, and an intersection over no list must not become "all".
    #[test]
    fn a_combination_over_no_list_selects_no_contact() {
        assert!(ListSelection::union(Vec::new()).is_empty());
        assert!(ListSelection::intersection(Vec::new()).is_empty());
    }

    #[test]
    fn a_combination_over_one_list_is_not_empty() {
        assert!(!ListSelection::union([ListId::new()]).is_empty());
        assert!(!ListSelection::intersection([ListId::new()]).is_empty());
    }

    /// A repeated identifier would make an intersection's distinct-count
    /// disagree with the number of identifiers asked for, and match nothing.
    #[test]
    fn a_repeated_list_is_counted_once() {
        let list = ListId::new();

        let selection = ListSelection::intersection([list, list]);

        assert_eq!(selection.lists(), &[list]);
    }

    #[test]
    fn an_exclusion_survives_the_combination_it_is_applied_to() {
        let kept = ListId::new();
        let dropped = ListId::new();

        let selection = ListSelection::union([kept]).excluding([dropped, dropped]);

        assert_eq!(selection.combination(), Combination::Any);
        assert_eq!(selection.lists(), &[kept]);
        assert_eq!(selection.excluded(), &[dropped]);
    }
}
