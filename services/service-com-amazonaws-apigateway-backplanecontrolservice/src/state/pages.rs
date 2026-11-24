// SPDX-License-Identifier: CC0-1.0

//! One `position`/`limit` page of a collection of this control plane, and the CURSOR a
//! page hands back for the walk to carry on at.
//!
//! A collection is walked in the order of whatever NAMES its elements, and the cursor a page
//! hands back names where the next one resumes. Which value that is follows from where the
//! name came from:
//!
//! * a collection walked by an identifier THIS SERVICE minted for each record — the account's
//!   api keys, rest apis, usage plans, vpc links and client certificates, an api's resources,
//!   deployments, authorizers and request validators, a plan's metered keys — hands back a
//!   continuation TOKEN of this store's ([`minted_page_of`]): the authenticator over the
//!   position it names, computed under a secret the store draws from the system's randomness
//!   when it is created ([`::service_common::page_tokens::PageTokenKey`], the workspace's one
//!   reading of a page token). An identifier this service assigned to a record is not a value
//!   a caller reads out of a cursor, and a value no store minted names no position at all, so
//!   it is refused ([`INVALID_POSITION`]) rather than read as some insertion point in the
//!   sequence — whether a caller assembled it, another store minted it (another emulator
//!   process, a restarted one), or another listing of this service did, including the same
//!   operation's listing of ANOTHER parent's collection ([`listing_scope`]);
//! * a collection walked by an ADDRESS the caller composed — a model's name, a base path, a
//!   domain name, a documentation version, the location a documentation part documents —
//!   hands that address back ([`page_of`], and [`Page::cut`] applied to the locations by
//!   `crate::ops::get_documentation_parts`), which is the cursor AWS's own answer carries for
//!   them: the caller already holds the address, and it is what every other operation of that
//!   collection addresses the element by.

use super::*;

pub use ::service_common::page_tokens::{PageTokenKey, AUTHENTICATOR_BYTES};

/// What separates the SCOPE a token was minted for from the cursor inside it: a control
/// character, which no identifier, name, base path or domain name a cursor of this service
/// is spelled with can carry.
const SCOPE_SEPARATOR: char = '\u{1e}';

/// What separates the parts of a scope from each other, for the same reason.
const SCOPE_PART_SEPARATOR: char = '\u{1f}';

/// The SCOPE a listing's cursors are bound to: the operation that minted them, then the
/// parts of the request that decide WHICH collection is being walked.
///
/// A cursor is honoured only against the scope it was minted for, so the continuation of one
/// listing can never resume another: `within` names the parent whose collection this is (the
/// rest api of a resource, model or deployment listing, the usage plan of a metered-key or
/// usage report), and is empty for a collection of the ACCOUNT. Two collections that hold
/// the same identifiers — the api keys of the account and the api keys one usage plan meters
/// — are therefore scoped apart rather than resuming each other's walks.
pub fn listing_scope(operation: &str, within: &[&str]) -> String {
    let mut scope = String::from(operation);
    for part in within {
        scope.push(SCOPE_PART_SEPARATOR);
        scope.push_str(part);
    }
    scope
}

/// One page of an ordered cursor sequence, as a `position`/`limit` paginated collection
/// reports it.
#[derive(Debug, Clone)]
pub struct Page {
    /// The cursors on this page, in the sequence's own order.
    pub ids: Vec<String>,
    /// The `position` this answer reports: the cursor of the first element AFTER this
    /// page, which is where the next page RESUMES. Absent once nothing follows the page,
    /// and that absence is what ends a walk of the collection.
    pub next_position: Option<String>,
    /// The page size the service APPLIED to cut this page, for a request that asked for
    /// one: the size it asked for, held inside the range the member publishes. Absent
    /// when the request asked for no size at all. See [`Page::echoed_limit`].
    applied_limit: Option<i32>,
}

impl Page {
    /// Cut the `limit`-sized page that begins at index `start` out of `ordered`.
    ///
    /// `limit` defaults to [`DEFAULT_PAGE_LIMIT`] and is clamped into the documented
    /// `1..=`[`MAX_PAGE_LIMIT`] range. `start` beyond the last element is the empty final
    /// page, not a failure: a caller that walks a collection to its end is served the
    /// page that holds nothing rather than a client error.
    pub fn cut(ordered: &[String], start: usize, limit: Option<i32>) -> Self {
        let size = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let window = usize::try_from(size).unwrap_or(1);
        let start = start.min(ordered.len());
        let end = start.saturating_add(window).min(ordered.len());
        let ids = ordered[start..end].to_vec();
        Self {
            // The cursor a client sends back names where to CARRY ON, so it is the first
            // element the page did not report and the page that follows begins AT it. A
            // page the sequence holds nothing after reports no cursor at all, which is
            // what tells a caller the collection has ended.
            next_position: ordered.get(end).cloned(),
            ids,
            // The page was cut to `size`, so that — and not the number the request
            // carried — is the size this page HAS, whichever end of the published range
            // the requested one lay outside of.
            applied_limit: limit.map(|_| size),
        }
    }

    /// The page size this answer echoes: the size the page was CUT to, for a page the
    /// collection carries on after — the member states the window the caller is being
    /// paged through, which is the requested size held inside the range the member
    /// publishes, so a request that asked for a size outside that range is told the size
    /// it was served and a request that asked for no size is told none. The page that
    /// ends the collection paged nothing further, and reports neither a size nor a
    /// cursor.
    pub fn echoed_limit(&self) -> Option<i32> {
        self.applied_limit.filter(|_| self.next_position.is_some())
    }

    /// The page of elements as the answer's collection member REPORTS it: PRESENT, and the
    /// empty list where the page holds no element. "The current page of elements from this
    /// collection" is what the member carries, and a page of nothing is a page of nothing
    /// rather than an answer that withholds the member — so a client reads "nothing is
    /// listed" off a list it can count, and a listing of the same collection reports it the
    /// same way whatever it holds. That holds however the page came to be empty: a collection
    /// that holds no element at all, one a caller emptied, a filter that selects none, and the
    /// window a caller that walked to the end asks for.
    ///
    /// One copy for every `position`/`limit` collection of this service, so no listing of this
    /// service reports an empty page a second way. The base path mappings of one domain name
    /// are the one collection this service answers otherwise
    /// ([`Page::items_reported_only_when_held`]).
    pub fn reported_items<Element>(&self, items: Vec<Element>) -> Option<Vec<Element>> {
        Some(items)
    }

    /// The page as the ONE collection of this service that leaves its member OUT of an empty
    /// answer reports it: present where the page holds an element, absent altogether where it
    /// holds none, so the whole answer is the empty object.
    ///
    /// The base path mappings of a domain name are answered this way — a domain name that
    /// carries no mapping is answered no `item` member at all — against
    /// [`Page::reported_items`] for every other listing of this service.
    pub fn items_reported_only_when_held<Element>(
        &self,
        items: Vec<Element>,
    ) -> Option<Vec<Element>> {
        (!items.is_empty()).then_some(items)
    }
}

/// The `position` a listing in `scope` hands back to carry on AT `cursor`.
///
/// The token is the AUTHENTICATOR over the scoped cursor and nothing else — the position it
/// names is recovered by [`minted_page_of`] matching it against the sequence being walked,
/// so the cursor travels in no part of the token and a caller holding one reads no record of
/// this store out of it. Hexadecimal, the spelling the sibling API Gateway v2 control plane
/// renders its own continuation tokens in, so the token is one word of text a caller hands
/// back in a query string with nothing to escape.
pub fn position_token(key: &PageTokenKey, scope: &str, cursor: &str) -> String {
    let mut minted = key.mint(&format!("{scope}{SCOPE_SEPARATOR}{cursor}"));
    // The authenticator alone: `mint` puts it ahead of the cursor, so dropping everything
    // after it leaves the token carrying no part of the position it names.
    minted.truncate(AUTHENTICATOR_BYTES);
    ::hex::encode(&minted)
}

/// Whether `token` is the one this store minted for `cursor` in `scope`.
///
/// The only way a position is resolved: a token names a position exactly when the sequence
/// being walked holds the element it was minted for, so a value a caller assembled, one
/// another store minted (another emulator process, a restarted one) and one minted for
/// another listing of this one all name no position at all.
///
/// The authenticator the caller sent back is checked by the shared reading's own verify
/// ([`PageTokenKey::cursor_of`], which compares in constant time), so a refusal tells the
/// caller nothing about how close its value was; what is compared here afterwards is the
/// scoped cursor the verify handed back, which is this store's own text and no secret.
pub fn position_names(key: &PageTokenKey, scope: &str, cursor: &str, token: &str) -> bool {
    let Ok(presented) = ::hex::decode(token) else {
        return false;
    };
    // The verify takes the authenticator FOLLOWED BY the text it was computed over, which
    // is how `mint` lays a token out: the caller sends the authenticator alone, and the text
    // is the position this listing is asking about.
    let scoped = format!("{scope}{SCOPE_SEPARATOR}{cursor}");
    let mut authenticated = presented;
    authenticated.extend_from_slice(scoped.as_bytes());
    key.cursor_of(&authenticated)
        .is_some_and(|named| named == scoped)
}

/// Cut the page a caller asked for out of `ordered`, a stable sequence of cursors, and mint
/// the `position` the next page resumes at.
///
/// For the collections of this service walked in the order of the identifiers it MINTED for
/// their records: what the answer reports is a token of this store's
/// ([`position_token`]), because an identifier this service assigned to a record is not a
/// value a caller reads out of a continuation token. A collection walked in the order of an
/// address the CALLER composed — a model's name, a mapping's base path, a domain name, a
/// documentation version, the location a documentation part documents — is paged by
/// [`page_of`] and its cursor is that address.
///
/// `position` is the token the previous page of this listing handed back — it names the
/// first element that page did not report, so this one begins AT it; `None` begins at the
/// first element.
///
/// `Err` carries [`INVALID_POSITION`] for a value that names no position: one this store did
/// not mint for `scope` ([`position_names`]), and one it minted whose element the sequence no
/// longer holds. Either must be told to the caller rather than served an arbitrary page.
///
/// Resolving a cursor costs one pass over `ordered`, which every collection of this service
/// bounds server-side (a store's growth bound, e.g. [`MAX_API_KEYS`]), so the work a request
/// can ask for here does not grow with anything the request carries.
pub fn minted_page_of(
    key: &PageTokenKey,
    scope: &str,
    ordered: &[String],
    position: Option<&str>,
    limit: Option<i32>,
) -> Result<Page, String> {
    let start = match position {
        None => 0,
        Some(token) => ordered
            .iter()
            .position(|cursor| position_names(key, scope, cursor, token))
            .ok_or_else(|| INVALID_POSITION.to_string())?,
    };
    let mut page = Page::cut(ordered, start, limit);
    // What the answer reports is the TOKEN for the element the next page begins at, never
    // that element's own identifier: the sequence this listing walks is the service's, not
    // something a caller reads off the page it was served.
    page.next_position = page
        .next_position
        .as_deref()
        .map(|cursor| position_token(key, scope, cursor));
    Ok(page)
}

/// Cut the page a caller asked for out of `ordered`, a stable sequence of cursors each of
/// which is the ADDRESS its element is named by.
///
/// `position` is the cursor the collection carries on AT — the first element the previous
/// page did not report — so the page begins at it; `None` begins at the first element. For
/// the collections of this service a caller addresses an element of by a name or a path it
/// composed itself, which is the cursor AWS's own answer carries for them; a collection
/// walked by a minted identifier is paged by [`minted_page_of`].
///
/// `Err` carries [`INVALID_POSITION`] for an unresolvable cursor: a `position` naming no
/// element of the sequence, which the caller must be told about rather than served an
/// arbitrary page.
pub fn page_of(
    ordered: &[String],
    position: Option<&str>,
    limit: Option<i32>,
) -> Result<Page, String> {
    let start = match position {
        None => 0,
        Some(cursor) => ordered
            .iter()
            .position(|id| id == cursor)
            .ok_or_else(|| INVALID_POSITION.to_string())?,
    };
    Ok(Page::cut(ordered, start, limit))
}

/// The `BadRequestException` message for a `position` this service did not issue for the
/// collection being listed.
///
/// A fixed sentence naming the member that is wrong, which quotes the rejected cursor back
/// nowhere. One copy for every `position`/`limit` collection, so an unusable cursor is
/// refused the same way whichever listing took it.
pub const INVALID_POSITION: &str = "Invalid position parameter";

impl ComAmazonawsApigatewayBackplanecontrolserviceState {
    /// The secret THIS store's page cursors are authenticated under.
    ///
    /// Handed back by value (the key is `Copy`), so a paginated read takes it off the store
    /// and goes on to borrow the records it is about to report in the same call.
    pub fn page_token_key(&self) -> PageTokenKey {
        self.page_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token names the position it was minted for, and names none under another key,
    /// another listing or another element; and it carries no part of the cursor.
    #[test]
    fn a_position_is_named_only_by_the_token_its_own_listing_minted() {
        let key = PageTokenKey::new();
        let another_store = PageTokenKey::new();
        let scope = listing_scope("GetApiKeys", &[]);
        // The scopes a cursor of this service is bound to besides its own: another
        // operation, and the SAME operation over another parent's collection.
        let other_scopes = [
            listing_scope("GetUsagePlanKeys", &[]),
            listing_scope("GetApiKeys", &["m54nh4kei0"]),
            listing_scope("GetUsagePlanKeys", &["plan-a"]),
            listing_scope("GetUsagePlanKeys", &["plan-b"]),
        ];
        // The forms a cursor of this service takes: a minted record id, a sub-resource id,
        // a caller-supplied domain name, a base path and the empty position.
        for cursor in ["m54nh4kei0", "abc123", "api.example.com", "(none)", ""] {
            let token = position_token(&key, &scope, cursor);
            assert!(
                position_names(&key, &scope, cursor, &token),
                "case {cursor:?}: a minted token must name its own position"
            );
            for other in &other_scopes {
                assert!(
                    !position_names(&key, other, cursor, &token),
                    "case {cursor:?}: a token minted for {scope:?} must name no position in \
                     {other:?}"
                );
            }
            assert!(
                !position_names(&another_store, &scope, cursor, &token),
                "case {cursor:?}: another store's key must not name a position for this token"
            );
            assert!(
                !position_names(&key, &scope, "another-element", &token),
                "case {cursor:?}: the token must name no other element"
            );
            assert!(
                cursor.is_empty() || !token.contains(cursor),
                "case {cursor:?}: the token must not carry the cursor in the clear, got \
                 {token:?}"
            );
        }

        // Two parents' listings of the same operation hold the same cursor and still do not
        // resume each other: the parent is part of the scope a token is bound to.
        let metered = "m54nh4kei0";
        let plan_a = position_token(&key, &listing_scope("GetUsagePlanKeys", &["plan-a"]), metered);
        assert!(
            !position_names(
                &key,
                &listing_scope("GetUsagePlanKeys", &["plan-b"]),
                metered,
                &plan_a
            ),
            "case a key metered by two plans: one plan's cursor must name no position in the \
             other's listing"
        );

        // Values a caller could assemble, none of which this store minted.
        let minted = position_token(&key, &scope, "m54nh4kei0");
        for (case, forged) in [
            ("the record id itself", "m54nh4kei0".to_string()),
            ("not a token's spelling", "!!!".to_string()),
            ("empty", String::new()),
            (
                "a minted token with a character dropped",
                minted[..minted.len() - 2].to_string(),
            ),
        ] {
            assert!(
                !position_names(&key, &scope, "m54nh4kei0", &forged),
                "case {case:?}: a value this store did not mint must name no position"
            );
        }
    }
}
