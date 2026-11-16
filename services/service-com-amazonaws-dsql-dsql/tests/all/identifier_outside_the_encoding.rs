// SPDX-License-Identifier: CC0-1.0

//! What the operations that DO state a form beyond the modelled `@pattern` answer
//! for a value inside that pattern which the base32 encoding this service writes
//! an identifier in does not spell.
//!
//! RULE under test: `^[a-z0-9]{26}$` (`smithy_query(query="shape",
//! shape="ClusterId,StreamId")`) is wider than that encoding, which leaves out `i`
//! and `l` (they read as `1`), `o` (it reads as `0`) and `u` altogether — so a
//! 26-character lowercase-alphanumeric value can be no base32-encoded identifier
//! at all, and an operation that states the encoding, or a range inside it, as a
//! precondition of its own refuses such a value as INPUT before any lookup: HTTP
//! 400 with the service's own modelled `ValidationException`, its `@required`
//! `reason` filled, its `fieldList` naming the member the value arrived in, and
//! the sentence that precondition is worded in. Two preconditions are stated:
//!
//! * the two operations addressed at a STREAM require the encoding of their
//!   `streamIdentifier` label, because a value outside it is no stream id at all —
//!   it could not appear in a `StreamArn` either — and refuse it in Aurora DSQL's
//!   sentence for a member whose value it cannot read, which names the member and
//!   the format the value had to be in (the whole rule for that label, with the
//!   order it is decided in, is pinned in `tests/all/stream_identifier_form.rs`);
//! * the three cluster-addressed operations that state the minted value RANGE
//!   refuse it in the cluster-identifier sentence, since a value the encoding does
//!   not spell is outside that range as surely as one above its ceiling
//!   (`tests/all/identifier_value_range.rs`).
//!
//! The complement — every other identifier-addressed operation, which states no
//! narrower form and resolves such a value against the store — is pinned in
//! `tests/all/identifier_pattern_is_the_whole_form.rs`, which owns the value
//! table both halves run over. The per-route answer lives on the route table as
//! `Route::outside_the_encoding`, so neither half picks its own routes.
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use crate::harness::Dsql;
use crate::identifier_pattern_is_the_whole_form::values_outside_the_encoding;
use crate::resource_identifier_absence::{
    refused_as_input, routes, the_store_is_untouched, value_in_the_alphabet, IdCase, Route, Verdict,
    IDENTIFIER_SYMBOLS,
};

/// Every route that states a form the encoding is part of, so a value outside the
/// encoding is refused as input rather than resolved.
fn routes_that_require_the_encoding() -> Vec<Route> {
    let gated: Vec<Route> = routes()
        .into_iter()
        .filter(|route| route.outside_the_encoding == Verdict::Refused)
        .collect();
    assert!(
        !gated.is_empty(),
        "the route table must carry routes that require the encoding"
    );
    gated
}

/// RULE: a value the encoding does not spell is refused as input by every route
/// that states a form the encoding is part of — 400 `ValidationException` with the
/// member named and the sentence that route's precondition is worded in — and
/// nothing is read or written.
#[tokio::test]
async fn a_value_outside_the_encoding_is_refused_where_the_encoding_is_required() {
    let dsql = Dsql::new();
    // One live cluster, so a row that puts the value under test in the stream
    // label is answered for that label and not for a missing cluster.
    let live = dsql.create_settled().await;

    let gated = routes_that_require_the_encoding();
    for case in values_outside_the_encoding() {
        for route in &gated {
            refused_as_input(&dsql, route, &live, &case).await;
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE at the bound, symbol by symbol: what these routes refuse is exactly the
/// symbols the encoding leaves out, so walking the whole `[a-z0-9]` set in the
/// leading position refuses a value on one of them if and only if that symbol is
/// outside the encoding — a symbol the encoding carries leaves the value to be
/// decided by the route's other precondition, which is the minted range
/// (`tests/all/identifier_value_range.rs`), not by this one.
#[tokio::test]
async fn only_the_symbols_the_encoding_leaves_out_are_refused_for_the_encoding() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let gated = routes_that_require_the_encoding();
    let base = value_in_the_alphabet();
    for leading in ('0'..='9').chain('a'..='z') {
        if IDENTIFIER_SYMBOLS.contains(leading) {
            continue;
        }
        let mut value: Vec<char> = base.chars().collect();
        value[0] = leading;
        let case = IdCase {
            name: format!("a leading {leading:?}, outside the encoding"),
            value: value.into_iter().collect(),
        };
        for route in &gated {
            refused_as_input(&dsql, route, &live, &case).await;
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}
