// SPDX-License-Identifier: CC0-1.0

//! The page size a paginated read of this service walks with.
//!
//! Every paginated read of this service models `maxResults` with a `@range`,
//! and the page it answers is held inside that range: the request boundary
//! refuses a size outside it before a handler runs, and the handler holds the
//! size inside the same bounds whatever reaches the member. Two things follow
//! from that, and both are why the size is normalised here rather than in each
//! handler: no answer carries more entries than the surface states, and no size
//! can leave a stored entry unreachable — a page size of zero would answer an
//! empty page, and an empty page is how a read says "nothing is stored", so the
//! walk could neither report the entries nor be resumed past them.
//!
//! Every read's own `maxResults` default and bounds stay beside that read, next
//! to the modelled member they are read off; what they mean for the walk is this
//! one computation.

/// The page size a paginated read walks with.
///
/// `requested` is `maxResults` as the request carried it, `default` the size the
/// member documents for a request that names none, and `minimum`/`maximum` the
/// bounds of that member's modelled `@range` (so `minimum` never exceeds
/// `maximum`). A value that is no page size at all — a negative one — names no
/// size, so the documented default answers it.
pub fn page_limit(
    requested: Option<i32>,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> usize {
    requested
        .and_then(|max_results| usize::try_from(max_results).ok())
        .unwrap_or(default)
        .clamp(minimum, maximum)
}
