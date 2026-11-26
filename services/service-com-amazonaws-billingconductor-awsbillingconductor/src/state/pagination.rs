// SPDX-License-Identifier: CC0-1.0

//! Paging one listing: the page size, the opaque `NextToken` this service mints and reads
//! back, and the cut over a sequence both halves agree on.

use super::*;

/// The page size the list operations apply when the request names none.
///
/// `ListAccountAssociations` is `@paginated` with a `NextToken` but models no
/// `MaxResults` at all, and `ListBillingGroups`'s `MaxResults` is optional, so in
/// both cases the SERVER chooses. This is the upper bound the service itself models
/// for that member (`MaxBillingGroupResults`, range 1..100), so one response is
/// bounded however much state exists, and a caller that wants more pages the
/// `NextToken` it already gets.
pub const DEFAULT_PAGE_SIZE: i32 = 100;

/// The input member every paginated read of this service carries its cursor in, as the
/// wire spells it — what a refusal about that cursor names in `Fields`.
pub const NEXT_TOKEN_MEMBER: &str = "NextToken";

/// The separator between the SCOPE a pagination token was minted for and the cursor it
/// carries, inside the bytes the token encodes.
///
/// A unit separator occurs in no scope (every scope of this service is built by
/// [`page_scope_of`], which joins with [`PAGE_SCOPE_SEPARATOR`] instead) and in none of the
/// keys this service pages over (ARNs, account ids, fixed-width ordinals), and only the FIRST
/// one is read as the separator, so a cursor is handed back exactly as it was minted.
pub(super) const PAGE_TOKEN_SEPARATOR: char = '\u{1f}';

/// The separator between the parts of a SCOPE that names more than the operation, distinct
/// from [`PAGE_TOKEN_SEPARATOR`] so the scope of a token is read back whole however many
/// parts it has.
pub(super) const PAGE_SCOPE_SEPARATOR: char = '\u{1e}';

/// The secret ONE store's pagination tokens are authenticated under, re-exported here so
/// the store that holds a key and every read that mints under it name the same type.
pub use ::service_common::page_tokens::PageTokenKey;

/// The `NextToken` a truncated page hands back: the hex encoding of an authenticator over
/// the `scope` that minted it and the `cursor` the next page resumes from, computed under
/// the issuing store's `key`.
///
/// A pagination token is this service's OWN value — the only thing a caller may do with one
/// is hand it back, unchanged, to the listing that returned it — and the scope travels
/// inside it: a token decodes to a cursor only under the scope it was minted for, which is
/// what stops one listing's token from being a cursor into another's sequence. See
/// [`page_scope_within`] for the scope of a listing read within one resource, and
/// [`page_scope_in_period`] for the scope of one answered for a single billing period.
///
/// The authenticator is what makes a token that was not minted here DECIDABLE rather than
/// read as some other cut point. It is computed under a secret the store drew from the
/// system's randomness and never reveals ([`PageTokenKey`]), so a caller cannot produce one
/// for a cursor of its own however much of the payload it already knows: a value that does
/// not carry this store's authenticator over the cursor it travels with was not produced
/// here, and is refused instead of resuming the listing somewhere the caller never asked
/// for. The cursor itself is readable to whoever decodes the bytes, and nothing about it has
/// to be hidden — what a caller cannot do is name a cut point of its own.
pub fn encode_next_token(key: &PageTokenKey, scope: &str, cursor: &str) -> String {
    ::hex::encode(key.mint(&format!("{scope}{PAGE_TOKEN_SEPARATOR}{cursor}")))
}

/// A CHOSEN ceiling on the characters a `NextToken` this service will even decode may
/// have. The model constrains `Token` not at all, so the value is caller-sized: the
/// longest token this service mints is an authenticator, an operation name, a resource ARN,
/// the request values that shape the sequence being read ([`page_scope_shaped`]) and a store
/// key, all far inside this, and anything longer was not minted here, so it is refused
/// before its length decides any work.
pub(super) const MAX_PAGE_TOKEN_LENGTH: usize = 1024;

/// The cursor `token` carries for the listing `scope`, or `None` when `token` is not one
/// `key` minted for that listing — a value longer than any token it mints, one that is not
/// hex, is too short to carry an authenticator, does not carry this store's authenticator
/// over the cursor it travels with (a value a caller assembled, one another store minted,
/// one changed on the way back), does not decode to UTF-8, carries no scope at all, carries
/// a different sequence's scope, or is spelled any way other than the ONE spelling
/// [`encode_next_token`] writes.
///
/// Verifying the authenticator is only the first half of reading a token: `::hex::decode`
/// reads more spellings than `::hex::encode` writes — it accepts `A-F` as well as the `a-f`
/// the minter emits — and the authenticator is computed over the DECODED payload, so it is
/// identical for every spelling of one cursor. A token is therefore kept only when it is,
/// byte for byte, the token this store would mint for the cursor it decodes to: re-minting is
/// what compares the two, so the accepted spelling is the ENCODER's rather than a second
/// grammar maintained beside it, and a re-cased copy of an issued token — a value no response
/// of this service ever carried, so not one the caller handed back unchanged — is refused
/// with [`invalid_next_token`] like every other value this service did not mint.
pub fn decode_next_token(key: &PageTokenKey, scope: &str, token: &str) -> Option<String> {
    if token.len() > MAX_PAGE_TOKEN_LENGTH {
        return None;
    }
    let encoded = ::hex::decode(token).ok()?;
    let payload = key.cursor_of(&encoded)?;
    let (minted_for, cursor) = payload.split_once(PAGE_TOKEN_SEPARATOR)?;
    if minted_for != scope {
        return None;
    }
    (encode_next_token(key, scope, cursor) == token).then(|| cursor.to_string())
}

/// The scope of the tokens a listing read WITHIN one resource mints: the operation and
/// that resource.
///
/// The scope identifies the SEQUENCE a cursor points into, and for these reads that
/// sequence belongs to one parent resource (a plan's rules, an item's versions, a group's
/// cost rows), so a cursor minted while paging one parent names no position in another's
/// sequence. The filters a request narrows a sequence with are not part of it: they select
/// which elements of the one sequence are reported, not which sequence is being read. A read
/// ANSWERED FOR one billing period is scoped by [`page_scope_in_period`] instead, because the
/// month it is answered for decides what each reported element is and so which sequence the
/// read walks.
pub fn page_scope_within(operation: &str, resource_arn: &str) -> String {
    page_scope_of(operation, &[resource_arn])
}

/// A scope, in the ONE spelling every scope of this service has: the operation, then each
/// value that identifies the sequence being read, joined with [`PAGE_SCOPE_SEPARATOR`].
///
/// Built in one place so a scope is spelled the same however many parts it has, and so the
/// scope of a read is read back whole.
fn page_scope_of(operation: &str, parts: &[&str]) -> String {
    let mut scope = operation.to_string();
    for part in parts {
        scope.push(PAGE_SCOPE_SEPARATOR);
        scope.push_str(part);
    }
    scope
}

/// The scope of the tokens a read ANSWERED FOR ONE BILLING PERIOD mints: the operation, the
/// resource the read is within when its sequence belongs to one, and the canonical month the
/// read is answered for.
///
/// A period-scoped read of this service answers for ONE month — the month the request names,
/// or the current one when it names none — and that month decides what each reported element
/// IS: the generation of a resource that was in force then, the associations and memberships
/// it held then, and the amounts that month was charged. So it is exactly the kind of request
/// value [`page_scope_shaped`] is the scope of: it names a different sequence for every month
/// it takes, and a cursor into one month's sequence names no position in another's, so a
/// token handed back under a different month is refused with [`invalid_next_token`] rather
/// than read as a cut point into a keyspace it names nothing in.
///
/// `resource_arn` is the parent the read is within for the reads whose sequence belongs to
/// one, and absent for a read of the account's own collection, exactly as
/// [`page_scope_within`] separates the two. The month travels CANONICAL
/// ([`canonical_billing_period`]), so the two spellings `BillingPeriod`'s pattern admits of
/// one month name the one sequence and resume it.
pub fn page_scope_in_period(
    operation: &str,
    resource_arn: Option<&str>,
    billing_period: &str,
) -> String {
    let billing_period = canonical_billing_period(billing_period);
    let mut parts: Vec<&str> = resource_arn.into_iter().collect();
    parts.push(billing_period.as_str());
    page_scope_of(operation, &parts)
}

/// The scope of the tokens a read WITHIN one resource mints when the REQUEST, and not that
/// resource alone, decides which sequence is being read: the operation, the resource, and
/// each request value that shapes the sequence.
///
/// [`page_scope_within`] is the whole scope of a read whose request members only SELECT
/// elements of one fixed sequence. A member that decides what ONE element IS — which
/// attributes a reported group aggregates over, and so what the cursor key of an element
/// even spells, or which periods an element's amounts cover — names a DIFFERENT sequence
/// for every value it takes, and a cursor into one of those sequences names no position in
/// another, so it travels in the scope: a token minted under one shape is then refused with
/// [`invalid_next_token`], the answer every token this service did not mint for the
/// sequence being read gets, instead of being read as a cut point into a keyspace it names
/// nothing in.
///
/// Each shaping value is passed ALREADY CANONICAL — one spelling per sequence, and the
/// values that merely order or repeat within it normalised away — so two requests that name
/// the same sequence resume the same one however they spelled it.
pub fn page_scope_shaped(operation: &str, resource_arn: &str, shaping: &[&str]) -> String {
    let mut parts: Vec<&str> = vec![resource_arn];
    parts.extend_from_slice(shaping);
    page_scope_of(operation, &parts)
}

/// The modelled refusal every paginated read of this service answers a `NextToken` it did
/// not mint with.
///
/// A token the service did not issue (or one a client corrupted) carries no page boundary
/// at all, so it is a fault in the INPUT rather than a listing to serve from an arbitrary
/// cut point: `ValidationExceptionReason` enumerates `CANNOT_PARSE` for exactly a value
/// the service cannot read
/// (`smithy_query(query="shape",shape="ValidationExceptionReason")`), so the answer is the
/// 400 `ValidationException` with that reason. ONE wording, built here, so a token refused
/// by a read whose cursor is a resource ARN reads exactly like one refused by a read whose
/// cursor is a minted ordinal.
///
/// The message states the member and the requirement and NOTHING of the rejected token:
/// the token is caller input, and a refusal that echoed it would copy it into every client
/// log that records the failure.
pub fn invalid_next_token() -> crate::ops::types::ValidationException {
    validation_exception(
        "CANNOT_PARSE",
        NEXT_TOKEN_MEMBER,
        format!(
            "Value at '{NEXT_TOKEN_MEMBER}' failed to satisfy constraint: {NEXT_TOKEN_MEMBER} \
             must be a pagination token a previous page of this operation returned"
        ),
    )
}

/// Cut one page out of an ordered, already-filtered sequence of `(key, element)`.
///
/// The `NextToken` carries the key of the first element left out, so it is a cursor rather
/// than an offset: resuming skips every key below it. It is minted and read by
/// [`encode_next_token`] / [`decode_next_token`] under `scope` — the listing it belongs to
/// — and under `key`, the store's own secret, so a token this store did not mint for THIS
/// listing is refused with
/// [`invalid_next_token`] instead of being interpreted as a cut point. The page size is the
/// request's `max_results` CLAMPED to 1..=[`DEFAULT_PAGE_SIZE`] — a request that
/// names none, or names a value the modelled range does not allow, is served the
/// default — so a page is bounded whatever reaches this helper. An operation whose own
/// `MaxResults` range reaches past that default pages through
/// [`paginate_bounded`] instead.
pub fn paginate<T>(
    key: &PageTokenKey,
    scope: &str,
    items: Vec<(String, T)>,
    next_token: Option<&str>,
    max_results: Option<i32>,
) -> Result<(Vec<T>, Option<String>), crate::ops::types::ValidationException> {
    paginate_bounded(key, scope, items, next_token, max_results, DEFAULT_PAGE_SIZE)
}

/// [`paginate`], for an operation whose own `MaxResults` member models a range the
/// default page size does not fit.
///
/// `page_size_bound` is the largest page that operation can be asked for — the maximum
/// of its modelled `MaxResults` range — and is also the size the server picks when the
/// request names none, so a caller that asks for a page the model allows is served it
/// whole and one that asks for nothing still gets a bounded page.
pub fn paginate_bounded<T>(
    key: &PageTokenKey,
    scope: &str,
    items: Vec<(String, T)>,
    next_token: Option<&str>,
    max_results: Option<i32>,
    page_size_bound: i32,
) -> Result<(Vec<T>, Option<String>), crate::ops::types::ValidationException> {
    let start = match next_token {
        Some(token) => Some(decode_next_token(key, scope, token).ok_or_else(invalid_next_token)?),
        None => None,
    };
    let mut page: Vec<(String, T)> = match start.as_deref() {
        Some(start) => items
            .into_iter()
            .filter(|(key, _)| key.as_str() >= start)
            .collect(),
        None => items,
    };
    let requested = max_results
        .filter(|max| *max > 0)
        .unwrap_or(page_size_bound)
        .min(page_size_bound);
    let limit = usize::try_from(requested).unwrap_or(1);
    let token = page
        .get(limit)
        .map(|(cursor, _)| encode_next_token(key, scope, cursor));
    page.truncate(limit);
    Ok((page.into_iter().map(|(_, element)| element).collect(), token))
}

/// The first value that appears more than once in `values`, if any.
pub fn first_duplicate(values: &[String]) -> Option<&String> {
    let mut seen = BTreeSet::new();
    values.iter().find(|value| !seen.insert(value.as_str()))
}
