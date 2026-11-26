// SPDX-License-Identifier: CC0-1.0

//! The per-resource TAG QUOTA of this control plane, and the one sentence a request past
//! it is refused with.
//!
//! The quota itself is [`super::MAX_TAGS`], AWS's limit of 50 tags per resource. What lives
//! here is the REFUSAL: API Gateway states the limit as a fixed sentence that names the
//! limit and the effect of breaking it — none of the request's tags is applied — and quotes
//! back neither the count the request would have reached nor any key it carried.
//!
//! One copy, reached by every path that charges the quota: `TagResource`, which charges it
//! on the request and on the merged collection, and every create that carries a `tags` map.
//! A tag write refused on one of them is therefore refused the same way on all of them.

use super::MAX_TAGS;

/// The `BadRequestException` message for a tag write that would take a resource past the
/// per-resource quota.
///
/// ENTIRELY FIXED TEXT but for the published limit it names ([`MAX_TAGS`]): no count, no key
/// and no resource identifier appears in it. The second sentence is the all-or-nothing
/// effect callers rely on — a refused write applies NONE of the tags it carried, which is
/// what every caller of [`bound_tags`] does by checking before anything is stored.
pub fn tag_quota_reached() -> String {
    format!("Up to {MAX_TAGS} tags can be set per resource. No tags were set on this request.")
}

/// Refuse a tag collection of `count` entries: `Err` carries [`tag_quota_reached`] when the
/// collection would hold more than the quota admits.
///
/// `count` is the number of entries the RESOURCE would carry — the merged collection for a
/// write onto a stored one, the request's own map for a create, which starts from none.
pub fn bound_tags(count: usize) -> Result<(), String> {
    if count > MAX_TAGS {
        return Err(tag_quota_reached());
    }
    Ok(())
}
