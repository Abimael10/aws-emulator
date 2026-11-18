// SPDX-License-Identifier: CC0-1.0

//! The SHARE of a sequence of occurrences a stated proportion names, counted deterministically.
//!
//! Several things this service does to a share of what it serves — the viewer requests a
//! real-time log configuration represents, the requests a continuous deployment policy routes
//! to its staging distribution, the responses a response headers policy adds its
//! `Server-Timing` header to — are each stated as a proportion and have to come out as that
//! proportion over the sequence they cover.
//!
//! ONE counting answers all of them ([`takes_next_share`]): each occurrence adds its share to
//! a cursor the record carries, and the occurrence takes the thing — spending a whole
//! occurrence's worth — as soon as the cursor holds one, with what is left over CARRIED to the
//! next. That makes the long-run share the stated proportion at every value of it, with no
//! randomness, and keeps the cursor below one whole occurrence plus that share. Each caller
//! names its own UNIT: the proportion and the whole are in whatever units hold the values that
//! caller's member may state exactly.

/// Whether the next occurrence takes the share `share` of them — in `whole`ths of one
/// occurrence — advancing `cursor` by that share and spending a whole occurrence's worth when
/// it holds one.
///
/// A share of none takes nothing and a share of a whole occurrence takes every one, which is
/// what makes the two ends of a proportion's range exact.
pub fn takes_next_share(cursor: &mut i64, share: i64, whole: i64) -> bool {
    *cursor = cursor.saturating_add(share);
    if *cursor < whole {
        return false;
    }
    *cursor -= whole;
    true
}
