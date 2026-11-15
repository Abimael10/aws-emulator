// SPDX-License-Identifier: CC0-1.0

//! What an identifier-addressed operation answers for a value inside `ClusterId`'s
//! and `StreamId`'s pattern that the encoding this service MINTS identifiers with
//! does not spell.
//!
//! RULE under test: the whole form an identifier member has to be in is the
//! `@pattern` its shape declares — `^[a-z0-9]{26}$` for `ClusterId` and `StreamId`
//! (`smithy_query(query="shape", shape="ClusterId,StreamId")`) — so every
//! 26-character lowercase-alphanumeric value is a well-formed identifier, whatever
//! letters it uses, and an operation that resolves such an identifier decides it
//! AGAINST THE STORE: one the account does not hold is the modelled
//! `ResourceNotFoundException` (HTTP 404, the `ResourceNotFoundException` wire
//! code, `message` naming the ARN the call addressed, `resourceId` the identifier
//! the request carried and `resourceType` the type of the resource the lookup
//! stopped at), never the 400 a malformed request gets. The service's own
//! identifiers are rendered in a base32 alphabet that leaves out `i`, `l`, `o` and
//! `u`, but that alphabet is how this service WRITES an identifier, not a
//! requirement on what a caller may send, so a value using one of those letters is
//! an identifier this account simply does not hold.
//!
//! The one precondition beyond the pattern is the minted value RANGE that
//! `GetCluster`, `DeleteCluster` and `GetClusterPolicy` state as their own, which
//! `tests/all/identifier_value_range.rs` pins: those three refuse a value they
//! could not have minted, including one the encoding does not spell, with the
//! modelled 400. The route table carries that per-operation answer as
//! `Route::above_the_minted_range`, and this suite asserts BOTH answers over that
//! one table.
//!
//! The value table spans how a value inside the pattern can fall outside the
//! encoding: each letter the encoding leaves out, at the front, in the middle and
//! at the end; one such letter filling the whole value; every one of them in one
//! value; and the 26 lowercase letters in order. The symbol-by-symbol test then
//! walks the entire `[a-z0-9]` set in the leading position, so the rule is
//! asserted over the whole character class rather than over the rows named here.
//! The complements are pinned beside this suite, over this suite's route table: a
//! value outside the modelled pattern
//! (`tests/all/malformed_resource_identifier.rs`), one of the form this service
//! mints (`tests/all/resource_identifier_absence.rs`) and one written in the
//! encoding alphabet above the minted range
//! (`tests/all/identifier_value_range.rs`).
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use crate::harness::{expected_stream_arn, Dsql};
use crate::resource_identifier_absence::{
    refused_as_input, reported_absent, routes, stream_request, the_store_is_untouched,
    value_in_the_alphabet, AboveTheMintedRange, IdCase, IDENTIFIER_LEN, IDENTIFIER_SYMBOLS,
    LEADING_SYMBOL_MAX,
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
fn values_outside_the_encoding() -> Vec<IdCase> {
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
    // pattern rather than about how many such letters a value happens to carry.
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

/// RULE: a value inside the modelled pattern that the encoding does not spell is
/// an identifier, so every operation that resolves one reports it as the modelled
/// absent resource — 404 `ResourceNotFoundException` with the ARN-naming sentence,
/// `resourceId` and `resourceType` — while the three that state the minted range
/// as a precondition refuse it as input, and nothing is read or written.
#[tokio::test]
async fn an_identifier_the_encoding_does_not_spell_is_absent_rather_than_invalid() {
    let dsql = Dsql::new();
    // One live cluster, so a row that puts the identifier under test in the stream
    // label is answered for the stream and not for a missing cluster.
    let live = dsql.create_settled().await;

    let routes = routes();
    for case in values_outside_the_encoding() {
        for route in &routes {
            match route.above_the_minted_range {
                AboveTheMintedRange::Absent => reported_absent(&dsql, route, &live, &case).await,
                AboveTheMintedRange::Refused => refused_as_input(&dsql, route, &live, &case).await,
            }
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE at the bound, symbol by symbol: the modelled pattern admits every symbol
/// of `[a-z0-9]` in every position, so no symbol in the leading position turns a
/// resolving route's answer into a refusal — each one answers 404 for an
/// identifier the account does not hold. The three routes that state the minted
/// range answer that same value per the range alone: a refusal exactly when the
/// value is one they could not have minted.
#[tokio::test]
async fn no_symbol_of_the_modeled_pattern_turns_a_lookup_into_a_refusal() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let routes = routes();
    let base = value_in_the_alphabet();
    for leading in ('0'..='9').chain('a'..='z') {
        let mut value: Vec<char> = base.chars().collect();
        value[0] = leading;
        let case = IdCase {
            name: format!("a leading {leading:?}"),
            value: value.into_iter().collect(),
        };
        // The minted range is the encoding AND the leading symbol: a value the
        // encoding does not spell is outside it just as one above its ceiling is.
        let could_have_been_minted =
            IDENTIFIER_SYMBOLS.contains(leading) && leading <= LEADING_SYMBOL_MAX;

        for route in &routes {
            let refused = route.above_the_minted_range == AboveTheMintedRange::Refused
                && !could_have_been_minted;
            if refused {
                refused_as_input(&dsql, route, &live, &case).await;
            } else {
                reported_absent(&dsql, route, &live, &case).await;
            }
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE, the other way round: an identifier the encoding does not spell is absent
/// rather than malformed, so it neither disturbs nor hides the resources that DO
/// exist — a stream minted on the live cluster still reads back while every such
/// value keeps answering 404 on that same nested route, naming the stream ARN it
/// addressed under the cluster that is really held.
#[tokio::test]
async fn an_absent_stream_identifier_does_not_disturb_the_stream_that_exists() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let created = dsql.create_stream(&live, stream_request()).await;
    assert_eq!(
        created.status, 200,
        "CreateStream must be served, got {} {:?}",
        created.status, created.body
    );
    let stream = created.str("streamIdentifier");

    for case in values_outside_the_encoding() {
        for (operation, answer) in [
            ("GetStream", dsql.get_stream(&live, &case.value).await),
            (
                "DeleteStream",
                dsql.delete_stream(&live, &case.value, "").await,
            ),
        ] {
            let name = format!("{} on {operation}", case.name);
            assert_eq!(
                answer.status, 404,
                "case {name}: a well-formed stream identifier the cluster does not \
                 hold must answer 404, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {name}: expected ResourceNotFoundException, got {:?}",
                answer.error
            );
            let arn = expected_stream_arn(dsql.region(), &live, &case.value);
            assert_eq!(
                answer.message(),
                format!("The resource with ARN {arn} doesn't exist. Verify the ARN and try again."),
                "case {name}: expected the sentence naming {arn:?}, got {:?}",
                answer.message()
            );
            assert_eq!(
                answer.str("resourceId"),
                case.value,
                "case {name}: resourceId is @required and is the identifier the \
                 request carried, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.str("resourceType"),
                "stream",
                "case {name}: resourceType is @required and is the addressed \
                 resource's own type, got {:?}",
                answer.body
            );
        }

        let read = dsql.get_stream(&live, &stream).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: the minted stream must still be read back, got {} {:?}",
            case.name,
            read.status,
            read.body
        );
    }
}
