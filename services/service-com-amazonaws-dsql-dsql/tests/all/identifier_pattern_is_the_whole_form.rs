// SPDX-License-Identifier: CC0-1.0

//! What an operation that states no form beyond the modelled `@pattern` answers
//! for a value inside that pattern which the base32 encoding this service MINTS
//! identifiers with does not spell.
//!
//! RULE under test: on such an operation the whole form an identifier member has
//! to be in is the `@pattern` its shape declares — `^[a-z0-9]{26}$` for
//! `ClusterId` and `StreamId` (`smithy_query(query="shape",
//! shape="ClusterId,StreamId")`) — so every 26-character lowercase-alphanumeric
//! value is a well-formed identifier, whatever letters it uses, and the operation
//! decides it AGAINST THE STORE: one the account does not hold is the modelled
//! `ResourceNotFoundException` (HTTP 404, the `ResourceNotFoundException` wire
//! code, `message` naming the ARN the call addressed, `resourceId` the identifier
//! the request carried and `resourceType` the type of the resource the lookup
//! stopped at), never the 400 a malformed request gets. The service's own
//! identifiers are rendered in a base32 alphabet that leaves out `i`, `l`, `o` and
//! `u`, but on these routes that alphabet is how this service WRITES an identifier
//! rather than a requirement on what a caller may send, so a value using one of
//! those letters is an identifier this account simply does not hold.
//!
//! The routes that DO state a narrower form refuse the same class of value as
//! input, which is the complement pinned in
//! `tests/all/identifier_outside_the_encoding.rs`: the two operations addressed at
//! a stream require the encoding of their `streamIdentifier` label, and the three
//! cluster-addressed operations that state the minted value RANGE refuse anything
//! outside it. The route table carries both answers as
//! `Route::outside_the_encoding` and `Route::above_the_minted_range`, and this
//! suite asserts its own half over that one table.
//!
//! The value table spans how a value inside the pattern can fall outside the
//! encoding: each letter the encoding leaves out, at the front, in the middle and
//! at the end; one such letter filling the whole value; every one of them in one
//! value; and the 26 lowercase letters in order. The symbol-by-symbol test then
//! walks the entire `[a-z0-9]` set in the leading position, so the rule is
//! asserted over the whole character class rather than over the rows named here.
//! The other complements are pinned beside this suite, over the same route table:
//! a value outside the modelled pattern
//! (`tests/all/malformed_resource_identifier.rs`), one of the form this service
//! mints (`tests/all/resource_identifier_absence.rs`) and one written in the
//! encoding alphabet above the minted range
//! (`tests/all/identifier_value_range.rs`).
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use crate::harness::Dsql;
use crate::resource_identifier_absence::{
    reported_absent, routes, the_store_is_untouched, value_in_the_alphabet, IdCase, Route, Verdict,
    IDENTIFIER_LEN, IDENTIFIER_SYMBOLS,
};

/// The letters `[a-z0-9]` carries that the encoding this service renders an
/// identifier with leaves out. Source: the alphabet `state::crockford_base32`
/// writes an identifier with (its `CROCKFORD_SYMBOLS`, mirrored by this suite's
/// [`IDENTIFIER_SYMBOLS`]) — `i` and `l` read as `1`, `o` as `0`, and `u` is left
/// out of the encoding altogether.
const OUTSIDE_THE_ENCODING: [char; 4] = ['i', 'l', 'o', 'u'];

/// Values INSIDE `^[a-z0-9]{26}$` that the encoding does not spell: each letter it
/// leaves out, at the front, in the middle and at the end; each one filling the
/// whole value; all four in one value; and the 26 lowercase letters in order.
///
/// The one table of this value class, shared with the suites that pin what the
/// routes stating a narrower form answer it with
/// (`tests/all/identifier_outside_the_encoding.rs`,
/// `tests/all/stream_identifier_form.rs`), so every half of the rule runs over the
/// same rows.
pub fn values_outside_the_encoding() -> Vec<IdCase> {
    let base = value_in_the_alphabet();
    let mut cases = Vec::new();

    for symbol in OUTSIDE_THE_ENCODING {
        for (position, where_it_is) in [
            (0, "first"),
            (IDENTIFIER_LEN / 2, "middle"),
            (IDENTIFIER_LEN - 1, "last"),
        ] {
            let mut value: Vec<char> = base.chars().collect();
            value[position] = symbol;
            cases.push(IdCase {
                name: format!("{symbol:?}, outside the encoding, as the {where_it_is} symbol"),
                value: value.into_iter().collect(),
            });
        }
        cases.push(IdCase {
            name: format!("{symbol:?} filling the whole value"),
            value: symbol.to_string().repeat(IDENTIFIER_LEN),
        });
    }

    // Every letter the encoding leaves out, in one value: the rule is about the
    // encoding rather than about how many such letters a value happens to carry.
    let mut all_four: Vec<char> = base.chars().collect();
    for (offset, symbol) in OUTSIDE_THE_ENCODING.into_iter().enumerate() {
        all_four[offset * 2] = symbol;
    }
    cases.push(IdCase {
        name: "every letter outside the encoding in one value".to_string(),
        value: all_four.into_iter().collect(),
    });

    // The whole lowercase alphabet in order, which carries every letter the
    // encoding leaves out at once.
    cases.push(IdCase {
        name: "the 26 lowercase letters in order".to_string(),
        value: ('a'..='z').collect(),
    });

    for case in &cases {
        assert_eq!(
            case.value.len(),
            IDENTIFIER_LEN,
            "case {:?}: every row must be {IDENTIFIER_LEN} symbols, got {:?}",
            case.name,
            case.value
        );
        assert!(
            case.value
                .chars()
                .all(|symbol| symbol.is_ascii_lowercase() || symbol.is_ascii_digit()),
            "case {:?}: every row must be inside the modelled pattern, got {:?}",
            case.name,
            case.value
        );
        assert!(
            case.value
                .chars()
                .any(|symbol| !IDENTIFIER_SYMBOLS.contains(symbol)),
            "case {:?}: every row must use a letter the encoding leaves out, got {:?}",
            case.name,
            case.value
        );
    }
    cases
}

/// Every route that states no form beyond the modelled pattern: neither the
/// encoding of its label nor the minted value range, so every value of the pattern
/// reaches its store lookup.
fn routes_that_state_no_narrower_form() -> Vec<Route> {
    let resolving: Vec<Route> = routes()
        .into_iter()
        .filter(|route| {
            route.outside_the_encoding == Verdict::Absent
                && route.above_the_minted_range == Verdict::Absent
        })
        .collect();
    assert!(
        !resolving.is_empty(),
        "the route table must carry routes that state no narrower form"
    );
    resolving
}

/// RULE: on a route that states no form beyond the modelled pattern, a value the
/// encoding does not spell is an identifier, so it is reported as the modelled
/// absent resource — 404 `ResourceNotFoundException` with the ARN-naming sentence,
/// `resourceId` and `resourceType` — and nothing is read or written.
#[tokio::test]
async fn an_identifier_the_encoding_does_not_spell_is_absent_rather_than_invalid() {
    let dsql = Dsql::new();
    // One live cluster, so a row that puts the identifier under test in a nested
    // route's cluster label is answered for that label and not for a stream.
    let live = dsql.create_settled().await;

    let resolving = routes_that_state_no_narrower_form();
    for case in values_outside_the_encoding() {
        for route in &resolving {
            reported_absent(&dsql, route, &live, &case).await;
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE at the bound, symbol by symbol: the modelled pattern admits every symbol
/// of `[a-z0-9]` in every position, so on a route that states no narrower form no
/// symbol in the leading position turns a lookup into a refusal — each one answers
/// 404 for an identifier the account does not hold.
#[tokio::test]
async fn no_symbol_of_the_modeled_pattern_turns_a_lookup_into_a_refusal() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let resolving = routes_that_state_no_narrower_form();
    let base = value_in_the_alphabet();
    for leading in ('0'..='9').chain('a'..='z') {
        let mut value: Vec<char> = base.chars().collect();
        value[0] = leading;
        let case = IdCase {
            name: format!("a leading {leading:?}"),
            value: value.into_iter().collect(),
        };
        for route in &resolving {
            reported_absent(&dsql, route, &live, &case).await;
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}
