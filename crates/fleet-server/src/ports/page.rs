//! Keyset pages for the lists of users, invites and sessions (Plan.md P7.6,
//! P7.9, ADR-0016): newest first by version 7 ID, which sorts by creation
//! time, like the audit log's pages by entry ID.
//!
//! A repository fetches one row more than the page holds
//! ([`PageLimit::fetch`]), so [`Paged::split`] knows whether another page
//! follows without a second query.

/// How many items one page holds: 1 to [`PageLimit::MAX`], 50 by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimit(u8);

/// Why a page size isn't valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a page holds 1 to {max} items")]
pub struct PageLimitError {
    /// [`PageLimit::MAX`].
    pub max: u8,
}

impl PageLimit {
    /// The largest page.
    pub const MAX: u8 = 100;
    /// The page size when the client names none.
    pub const DEFAULT: Self = Self(50);

    /// Checks a page size.
    ///
    /// # Errors
    /// [`PageLimitError`] for 0 or more than [`PageLimit::MAX`].
    pub fn new(limit: u32) -> Result<Self, PageLimitError> {
        u8::try_from(limit)
            .ok()
            .filter(|limit| (1..=Self::MAX).contains(limit))
            .map(Self)
            .ok_or(PageLimitError { max: Self::MAX })
    }

    /// Returns the page size.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// How many rows a repository fetches: one more than the page holds, so
    /// the caller knows whether another page follows.
    #[must_use]
    pub const fn fetch(self) -> u32 {
        self.0 as u32 + 1
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Which page to list: the newest items older than `before` (all of them
/// without a cursor), newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page<I> {
    /// Only items with a smaller ID; `None` starts at the newest.
    pub before: Option<I>,
    /// How many items at most.
    pub limit: PageLimit,
}

/// One page of items, and the cursor of the next page when one follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paged<T, I> {
    /// The items, newest first.
    pub items: Vec<T>,
    /// The `before` of the next page: the last item's ID, when more follow.
    pub next: Option<I>,
}

impl<T, I> Paged<T, I> {
    /// Splits the rows a repository fetched with `limit.fetch()` into the
    /// page's items and the next page's cursor, which `id` reads from the
    /// page's last item.
    #[must_use]
    pub fn split(mut rows: Vec<T>, limit: PageLimit, id: impl Fn(&T) -> I) -> Self {
        let more = rows.len() > usize::from(limit.get());
        rows.truncate(usize::from(limit.get()));
        let next = if more { rows.last().map(id) } else { None };
        Self { items: rows, next }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::the_smallest(1)]
    #[case::the_default(50)]
    #[case::the_largest(100)]
    fn a_size_in_range_is_kept(#[case] limit: u32) {
        assert_eq!(u32::from(PageLimit::new(limit).unwrap().get()), limit);
    }

    #[rstest]
    #[case::zero(0)]
    #[case::one_too_many(101)]
    #[case::past_a_byte(256)]
    #[case::the_largest(u32::MAX)]
    fn a_size_out_of_range_is_refused(#[case] limit: u32) {
        assert_eq!(PageLimit::new(limit), Err(PageLimitError { max: 100 }));
    }

    #[test]
    fn the_default_is_fifty_and_fetches_one_more() {
        assert_eq!(PageLimit::default().get(), 50);
        assert_eq!(PageLimit::default().fetch(), 51);
        assert_eq!(PageLimit::new(100).unwrap().fetch(), 101);
    }

    #[test]
    fn a_short_fetch_is_the_last_page() {
        let paged = Paged::split(vec![9, 8, 7], PageLimit::new(3).unwrap(), |n| *n);

        assert_eq!(paged.items, [9, 8, 7]);
        assert_eq!(paged.next, None);
    }

    #[test]
    fn one_row_more_than_the_page_names_the_next_page() {
        let paged = Paged::split(vec![9, 8, 7, 6], PageLimit::new(3).unwrap(), |n| *n);

        assert_eq!(paged.items, [9, 8, 7]);
        assert_eq!(paged.next, Some(7));
    }

    #[test]
    fn nothing_fetched_is_an_empty_last_page() {
        let paged = Paged::split(Vec::<u8>::new(), PageLimit::default(), |n| *n);

        assert_eq!(paged.items, [0_u8; 0]);
        assert_eq!(paged.next, None);
    }
}
