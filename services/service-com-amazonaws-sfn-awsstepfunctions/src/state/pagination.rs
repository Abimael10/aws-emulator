// SPDX-License-Identifier: CC0-1.0

//! How a paginated read of this service carries a position from one call to the next.
//!
//! "If `nextToken` is returned, there are more results available. The value of
//! `nextToken` is a unique pagination token for each page. Make the call again using
//! the returned token to retrieve the next page": the token is the caller's handle on
//! the rest of one listing, and it is OPAQUE — a caller reads no position out of it
//! and hands it back exactly as it arrived. So the position it names travels inside
//! it, authenticated under the secret the store that issued it holds
//! ([`::service_common::page_tokens::PageTokenKey`]) and rendered as base64: a value
//! no listing of this store minted names no position at all, which is what the
//! modelled `InvalidToken` reports, and nothing about the result's position is
//! legible on the wire.
//!
//! Every paginated read of this service — the listings and the execution history
//! alike — mints and resolves its tokens here, so one of them cannot carry a position
//! the others do not understand.

use ::service_common::page_tokens::PageTokenKey;

use super::*;

/// How many items one page of a list operation carries.
///
/// `maxResults` is modelled `0..1000`, so the walk has already refused anything
/// outside that; `0` means the default, per the member's own documentation.
pub fn page_size(max_results: i32) -> usize {
    if max_results <= 0 {
        DEFAULT_PAGE_SIZE
    } else {
        usize::try_from(max_results).unwrap_or(DEFAULT_PAGE_SIZE)
    }
}

/// The `nextToken` text that carries `cursor`: the cursor behind the authenticator the
/// issuing store's key computes over it, rendered as base64 — which is what makes the
/// token one string the caller hands back unread.
///
/// The modelled tokens are bounded in LENGTH (`PageToken` 1..1024,
/// `ListExecutionsPageToken` 1..3096) and this spelling is far inside both: a
/// fixed-width authenticator plus the decimal position, base64-encoded.
pub fn page_token(key: &PageTokenKey, cursor: &str) -> String {
    ::aws_smithy_types::base64::encode(key.mint(cursor))
}

/// The cursor `token` carries, or `None` for any value this store's listings did not
/// mint: one that is not base64, one whose authenticator does not match the cursor it
/// travels with, and one minted under another store's key.
fn page_cursor(key: &PageTokenKey, token: &str) -> Option<String> {
    let bytes = ::aws_smithy_types::base64::decode(token).ok()?;
    key.cursor_of(&bytes)
}

/// The offset a page token names, or `None` when the token is not one this service
/// issued — which the modelled `InvalidToken` reports.
///
/// A token carries the offset of the next item in the operation's own total order,
/// which is what makes it a token "for each page" that keeps meaning across calls; a
/// caller that sends no token at all asks for the first page.
pub fn page_offset(key: &PageTokenKey, next_token: Option<&str>) -> Option<usize> {
    match next_token {
        None => Some(0),
        Some(token) => page_cursor(key, token)?.parse::<usize>().ok(),
    }
}

/// The token that names the next page, or `None` when `items` were not truncated.
pub fn next_page_token(
    key: &PageTokenKey,
    offset: usize,
    page: usize,
    total: usize,
) -> Option<String> {
    let next = offset.saturating_add(page);
    (next < total).then(|| page_token(key, &next.to_string()))
}

/// What a HISTORY page token names: the position in this execution's OWN event order
/// — ascending, zero-based — at which the next page begins.
///
/// "The value of nextToken is a unique pagination token for each page. Make the call
/// again using the returned token to retrieve the next page. Keep all other
/// arguments unchanged": the token is a cursor into the HISTORY rather than a count
/// of pages, so the position a read continues from is the event it stopped at,
/// whichever end of the history that read walks from. That is also what binds a
/// token to the read that issued it: a backwards read continues from a position no
/// page of a forwards read ever begins at, so each order's tokens are the other's
/// `InvalidToken`.
///
/// The mapping is its own inverse, so one function turns a position in the READ's
/// order into the cursor the token carries and back.
fn history_cursor(position: usize, total: usize, reverse_order: bool) -> usize {
    if reverse_order {
        total.saturating_sub(1).saturating_sub(position)
    } else {
        position
    }
}

/// The offset into this read's own order that the page token `next_token` names, or
/// `None` when it is not a token this read handed out — which the modelled
/// `InvalidToken` reports.
///
/// A token this read issued names one of ITS page boundaries, inside this
/// execution's history, never the first page (which is asked for by sending no
/// token at all), and no further than this read has got — `handed_out` is how far
/// each read of this history has handed cursors out
/// ([`Execution::issued_page_cursors`]). So a value this store never minted, a
/// position no page of this read begins at, and a cursor minted for another execution,
/// the other sort order or another page size all resolve to nothing.
pub fn history_page_offset(
    key: &PageTokenKey,
    next_token: Option<&str>,
    read: HistoryRead,
    total: usize,
    handed_out: &[HistoryPageCursor],
) -> Option<usize> {
    let Some(token) = next_token else {
        return Some(0);
    };
    let cursor = page_cursor(key, token)?.parse::<usize>().ok()?;
    if cursor >= total {
        return None;
    }
    let offset = history_cursor(cursor, total, read.reverse_order);
    let boundary = read.page > 0 && offset.is_multiple_of(read.page);
    if offset == 0 || offset >= total || !boundary {
        return None;
    }
    let reached = handed_out.iter().find(|remembered| remembered.of(read))?;
    (offset <= reached.offset).then_some(offset)
}

/// The cursor that names the next page of a history read, or `None` when the page
/// was not truncated — the one [`history_page_offset`] resolves back, and the one
/// the execution remembers handing out.
pub fn next_history_page_cursor(
    offset: usize,
    page: usize,
    total: usize,
    read: HistoryRead,
) -> Option<HistoryPageCursor> {
    let next = offset.saturating_add(page);
    (next < total).then(|| read.at(next))
}

/// The `nextToken` text a cursor into a history of `total` events is carried as: the
/// position in the EXECUTION's own event order ([`history_cursor`]), carried in the
/// one opaque spelling every token of this service is minted in ([`page_token`]).
pub fn history_page_token(key: &PageTokenKey, cursor: HistoryPageCursor, total: usize) -> String {
    page_token(
        key,
        &history_cursor(cursor.offset, total, cursor.reverse_order).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token round-trips to the position it was minted for, under the key that
    /// minted it and no other, and nothing about that position is legible in it.
    ///
    /// The rows span the positions a listing of this service hands out: the first page
    /// boundary, a middle one, a far one, and the widest offset a store could count to.
    #[test]
    fn a_page_token_is_opaque_and_names_a_position_only_under_its_own_key() {
        let key = PageTokenKey::new();
        let other = PageTokenKey::new();
        for offset in [1_usize, 2, 7, 100, 4096, usize::MAX] {
            let token = page_token(&key, &offset.to_string());
            let decoded = page_offset(&key, Some(&token));
            assert_eq!(
                decoded,
                Some(offset),
                "case {offset}: expected the token to name its own offset, got {decoded:?}"
            );
            assert_eq!(
                page_offset(&other, Some(&token)),
                None,
                "case {offset}: a token minted under another store's key must name no \
                 position, got {:?}",
                page_offset(&other, Some(&token))
            );
            assert_ne!(
                token,
                offset.to_string(),
                "case {offset}: the token must not be the position itself"
            );
            // Within the narrowest modelled token length (`PageToken` 1..1024).
            assert!(
                (1..=1024).contains(&token.len()),
                "case {offset}: expected a token of a modelled length, got {} characters",
                token.len()
            );
        }
    }

    /// A value no listing of this store minted names no position: the bare decimal
    /// index a caller might assume, text that is not base64, an empty token, and a
    /// token with the shape of a minted one.
    #[test]
    fn a_token_this_store_did_not_mint_names_no_position() {
        let key = PageTokenKey::new();
        let forged = ::aws_smithy_types::base64::encode(
            [
                vec![0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES].as_slice(),
                b"1".as_slice(),
            ]
            .concat(),
        );
        for (case, token) in [
            ("a bare decimal index", "1".to_string()),
            ("a bare decimal index past the end", "9".to_string()),
            ("text that is not base64", "not-a-token".to_string()),
            ("an empty token", String::new()),
            ("a minted token's shape with a forged authenticator", forged),
        ] {
            let decoded = page_offset(&key, Some(&token));
            assert_eq!(
                decoded, None,
                "case {case:?}: expected no position, got {decoded:?}"
            );
        }
        // No token at all is the first page, which is how a listing starts.
        assert_eq!(
            page_offset(&key, None),
            Some(0),
            "expected no token to name the first page, got {:?}",
            page_offset(&key, None)
        );
    }
}
