// SPDX-License-Identifier: CC0-1.0

//! Paging, as every List operation of this service does it: the page size a
//! request asks for and the opaque `NextToken` a page boundary is carried in.
//!
//! One reading of both, so two enumerations cannot drift on what an
//! out-of-range `MaxResults` means or on what a token is: each List operation
//! decides only the ORDER it enumerates in and the fields its own cursor
//! carries.

/// The largest page a List operation of this service returns, from the
/// `MaxResults` these operations share: `@range(min: 1, max: 100)`.
pub const MAX_PAGE_SIZE: i32 = 100;

/// The page size a request that names no `MaxResults` gets: that member's own
/// modelled default, which is also the range's maximum.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// Separates the fields inside a page token. A unit separator cannot occur in
/// any of them: the names this service mints are `[A-Za-z0-9][\.\-_A-Za-z0-9]*`
/// and its ARNs are printable ASCII.
const PAGE_TOKEN_SEPARATOR: &str = "\u{1f}";

/// The page size a request asking for `requested` gets.
///
/// `MaxResults` is modelled `@range(min: 1, max: 100)` with a default of 100,
/// and the boundary validator refuses anything outside that window before a
/// handler runs. An in-process caller can still hand over a value the wire never
/// carries, so a value at or below zero takes the MODELLED DEFAULT rather than
/// the range's minimum — a page of one is not what an absent member asks for —
/// and anything above the maximum is capped at it.
pub fn page_size(requested: i32) -> usize {
    match requested {
        requested if requested > 0 => {
            usize::try_from(requested.min(MAX_PAGE_SIZE)).unwrap_or(DEFAULT_PAGE_SIZE)
        }
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// Render the fields of a page cursor as the opaque `NextToken` the next request
/// replays.
///
/// `NextToken` is modelled as an opaque string of 1..4096 characters, so the
/// cursor is rendered in hex: an alphabet that survives any transport and that
/// no client can mistake for a value it may construct itself.
pub fn encode_page_token(fields: &[&str]) -> String {
    ::hex::encode(fields.join(PAGE_TOKEN_SEPARATOR))
}

/// Read a cursor of exactly `fields` fields back out of a `NextToken`. `None`
/// when the token is not one this service issued for a cursor of that shape.
pub fn decode_page_token(token: &str, fields: usize) -> Option<Vec<String>> {
    let decoded = String::from_utf8(::hex::decode(token).ok()?).ok()?;
    let parts: Vec<String> = decoded
        .split(PAGE_TOKEN_SEPARATOR)
        .map(str::to_string)
        .collect();
    (parts.len() == fields).then_some(parts)
}
