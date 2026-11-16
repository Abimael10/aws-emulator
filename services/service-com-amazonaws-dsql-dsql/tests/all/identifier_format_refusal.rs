// SPDX-License-Identifier: CC0-1.0

//! WHICH operations refuse an identifier's form at all, as against resolving it.
//!
//! RULE under test: the outer form an identifier member has to be in is the
//! `@pattern` its shape declares (`ClusterId` and `StreamId`: `^[a-z0-9]{26}$`,
//! `smithy_query(query="shape", shape="ClusterId,StreamId")`), and beyond it an
//! operation states only the form it needs of the label it reads: the two
//! operations addressed at a STREAM require the base32 encoding of their
//! `streamIdentifier` label, and `GetCluster`, `DeleteCluster` and
//! `GetClusterPolicy` require the value RANGE a cluster identifier is minted from.
//! So for a value inside the pattern that this service could not have minted, the
//! set of operations answering a 400 is exactly the ones whose stated form that
//! value breaks — every other identifier-addressed operation resolves the value
//! against the store and reports the modelled absent resource instead. A value no
//! longer refused by one of them, or newly refused by any other, breaks this suite
//! rather than silently turning an absent resource into a malformed request.
//!
//! The value table spans both ways a value inside the pattern can be one no mint
//! could have rendered — a letter the encoding leaves out, and symbols that spell a
//! value above the range a minted identifier is drawn from — and each row carries
//! which of the two it is, so the expected set is read off the route table's answer
//! for that class rather than written out here. What each class answers, member by
//! member, is pinned beside this suite —
//! `tests/all/identifier_pattern_is_the_whole_form.rs` and
//! `tests/all/identifier_outside_the_encoding.rs` for the letters the encoding
//! leaves out, `tests/all/identifier_value_range.rs` for the range — over the same
//! route table.
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use crate::harness::Dsql;
use crate::resource_identifier_absence::{
    routes, the_store_is_untouched, value_in_the_alphabet, IdCase, Route, Verdict, IDENTIFIER_LEN,
    LEADING_SYMBOL_MAX,
};

/// Which way a value inside the modelled pattern is one no mint could have
/// rendered, and therefore which of a route's two stated forms it breaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// The value uses a symbol the base32 encoding leaves out, so it is no
    /// encoded identifier at all.
    OutsideTheEncoding,
    /// The value is written in the encoding, but its symbols spell a value above
    /// the range a minted identifier is drawn from.
    AboveTheMintedRange,
}

impl Class {
    /// What `route` answers a value of this class, as the route table states it.
    fn verdict_of(self, route: &Route) -> Verdict {
        match self {
            Class::OutsideTheEncoding => route.outside_the_encoding,
            Class::AboveTheMintedRange => route.above_the_minted_range,
        }
    }
}

/// Values inside `^[a-z0-9]{26}$` that this service could not have minted, each
/// with the class it belongs to: one using a letter the encoding leaves out, the 26
/// lowercase letters in order, and the two ways symbols of the encoding spell a
/// value above the minted range.
fn values_no_mint_could_have_rendered() -> Vec<(Class, IdCase)> {
    let base = value_in_the_alphabet();
    let mut with_a_letter_outside: Vec<char> = base.chars().collect();
    with_a_letter_outside[IDENTIFIER_LEN / 2] = 'u';
    vec![
        (
            Class::OutsideTheEncoding,
            IdCase {
                name: "a letter the encoding leaves out".to_string(),
                value: with_a_letter_outside.into_iter().collect(),
            },
        ),
        (
            Class::OutsideTheEncoding,
            IdCase {
                name: "the 26 lowercase letters in order".to_string(),
                value: ('a'..='z').collect(),
            },
        ),
        (
            Class::AboveTheMintedRange,
            IdCase {
                name: "the smallest value above the minted range".to_string(),
                value: format!("8{}", "0".repeat(IDENTIFIER_LEN - 1)),
            },
        ),
        (
            Class::AboveTheMintedRange,
            IdCase {
                name: "the largest value the encoding spells".to_string(),
                value: "z".repeat(IDENTIFIER_LEN),
            },
        ),
    ]
}

/// RULE: for a value inside the modelled pattern that no mint could have rendered,
/// the operations that answer a 400 are exactly the ones stating the form that
/// value breaks; every other identifier-addressed operation answers the modelled
/// 404 for an absent resource. Nothing is read or written either way.
#[tokio::test]
async fn only_the_operations_stating_the_form_a_value_breaks_refuse_it() {
    let dsql = Dsql::new();
    // One live cluster, so a row that puts the value under test in the stream
    // label is answered for that label rather than for a missing cluster.
    let live = dsql.create_settled().await;

    let routes = routes();
    for (class, case) in values_no_mint_could_have_rendered() {
        let expected: Vec<&str> = routes
            .iter()
            .filter(|route| class.verdict_of(route) == Verdict::Refused)
            .map(|route| route.operation)
            .collect();
        assert!(
            !expected.is_empty() && expected.len() < routes.len(),
            "case {:?}: the route table must carry both answers for {class:?}, got \
             {expected:?} of {} routes",
            case.name,
            routes.len()
        );

        let mut refused: Vec<&str> = Vec::new();
        for route in &routes {
            let (method, path, body) = (route.request)(&case.value, &live);
            let answer = dsql.call(method, &path, body).await;
            assert!(
                answer.status == 400 || answer.status == 404,
                "case {} on {}: a value inside the pattern is either refused as input \
                 or reported absent, got {} {:?}",
                case.name,
                route.operation,
                answer.status,
                answer.body
            );
            if answer.status == 400 {
                refused.push(route.operation);
            }
        }
        assert_eq!(
            refused, expected,
            "case {:?}: expected only {expected:?} to refuse the value's form, got {refused:?}",
            case.name
        );
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE at the bound: the forms stated beyond the pattern are the encoding and the
/// minted range, so a value the mint COULD have rendered breaks neither — every
/// route, the ones stating a form among them, resolves it against the store.
#[tokio::test]
async fn a_value_the_mint_could_have_rendered_is_refused_by_nothing() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let routes = routes();
    let base = value_in_the_alphabet();
    assert!(
        base.chars()
            .next()
            .is_some_and(|leading| leading <= LEADING_SYMBOL_MAX),
        "the base value must sit inside the minted range, got {base:?}"
    );

    for route in &routes {
        let (method, path, body) = (route.request)(&base, &live);
        let answer = dsql.call(method, &path, body).await;
        assert_eq!(
            answer.status,
            404,
            "case {base:?} on {}: a value of the minted form that the account does \
             not hold must be reported absent, got {} {:?}",
            route.operation,
            answer.status,
            answer.body
        );
    }

    the_store_is_untouched(&dsql, &live).await;
}
