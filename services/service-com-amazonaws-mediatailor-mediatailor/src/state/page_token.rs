// SPDX-License-Identifier: CC0-1.0

//! The continuation token every paginated read of this service issues, in ONE
//! statement.
//!
//! Such a read walks a store ordered by the identifier its items are held under,
//! so the only thing a page boundary has to carry is the identifier of the LAST
//! item the page served: the next page resumes at the first stored identifier
//! after it. `NextToken` is documented as a "[p]agination token returned by the
//! list request when results exceed the maximum allowed", with no form a caller
//! may read or construct, so the token is kept OPAQUE: hex of the boundary's
//! bytes, which is total over every identifier an item can be held under and
//! gives a caller nothing to build a token of its own from.
//!
//! A token this service did not issue decodes to nothing, so the read that was
//! handed one refuses the request rather than serving a page from a boundary it
//! invented.

/// The continuation token that resumes a listing AFTER `boundary`.
pub fn encode_page_token(boundary: &str) -> String {
    hex::encode(boundary.as_bytes())
}

/// The boundary `token` resumes after, or `None` when `token` is not one this
/// service issued.
pub fn decode_page_token(token: &str) -> Option<String> {
    String::from_utf8(hex::decode(token).ok()?).ok()
}
