// SPDX-License-Identifier: CC0-1.0

//! WHICH operations refuse an identifier's form at all, as against resolving it.
//!
//! RULE under test: the form an identifier member has to be in is the `@pattern`
//! its shape declares (`ClusterId` and `StreamId`: `^[a-z0-9]{26}$`,
//! `smithy_query(query="shape", shape="ClusterId,StreamId")`), and the ONE
//! precondition any operation of this service states beyond it is the minted value
//! range `GetCluster`, `DeleteCluster` and `GetClusterPolicy` state as their own.
//! So for a value inside the pattern that this service could not have minted, the
//! set of operations answering a 400 is exactly those three: every other
//! identifier-addressed operation resolves the value against the store and reports
//! the modelled absent resource instead. A value no longer refused by one of them,
//! or newly refused by any other, breaks this suite rather than silently turning an
//! absent resource into a malformed request.
//!
//! The value table spans both ways a value inside the pattern can be one no mint
//! could have rendered: a letter the encoding leaves out, and symbols that spell a
//! value above the range a minted identifier is drawn from. What each class
//! answers, member by member, is pinned beside this suite —
//! `tests/all/identifier_pattern_is_the_whole_form.rs` for the letters the encoding
//! leaves out and `tests/all/identifier_value_range.rs` for the range — over the
//! same route table.
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use crate::harness::Dsql;
use crate::resource_identifier_absence::{
    routes, the_store_is_untouched, value_in_the_alphabet, AboveTheMintedRange, IdCase,
    IDENTIFIER_LEN, LEADING_SYMBOL_MAX,
};

/// Values inside `^[a-z0-9]{26}$` that this service could not have minted: one
/// using a letter the encoding leaves out, the 26 lowercase letters in order, and
/// the two ways symbols of the encoding spell a value above the minted range.
fn values_no_mint_could_have_rendered() -> Vec<IdCase> {
    let base = value_in_the_alphabet();
    let mut with_a_letter_outside: Vec<char> = base.chars().collect();
    with_a_letter_outside[IDENTIFIER_LEN / 2] = 'u';
    vec![
        IdCase {
            name: "a letter the encoding leaves out".to_string(),
            value: with_a_letter_outside.into_iter().collect(),
        },
        IdCase {
            name: "the 26 lowercase letters in order".to_string(),
            value: ('a'..='z').collect(),
        },
        IdCase {
            name: "the smallest value above the minted range".to_string(),
            value: format!("8{}", "0".repeat(IDENTIFIER_LEN - 1)),
        },
        IdCase {
            name: "the largest value the encoding spells".to_string(),
            value: "z".repeat(IDENTIFIER_LEN),
        },
    ]
}

/// RULE: for a value inside the modelled pattern that no mint could have rendered,
/// the operations that answer a 400 are exactly the ones stating the minted range
/// as a precondition; every other identifier-addressed operation answers the
/// modelled 404 for an absent resource. Nothing is read or written either way.
#[tokio::test]
async fn only_the_operations_stating_the_minted_range_refuse_an_identifier_form() {
    let dsql = Dsql::new();
    // One live cluster, so a row that puts the value under test in the stream
    // label is answered for that label rather than for a missing cluster.
    let live = dsql.create_settled().await;

    let routes = routes();
    let expected: Vec<&str> = routes
        .iter()
        .filter(|route| route.above_the_minted_range == AboveTheMintedRange::Refused)
        .map(|route| route.operation)
        .collect();
    assert!(
        !expected.is_empty() && expected.len() < routes.len(),
        "the route table must carry both answers, got {expected:?} of {} routes",
        routes.len()
    );

    for case in values_no_mint_could_have_rendered() {
        let mut refused: Vec<&str> = Vec::new();
        for route in &routes {
            let (method, path, body) = (route.request)(&case.value, &live);
            let answer = dsql.call(method, &path, body).await;
            assert!(
                answer.status == 400 || answer.status == 404,
                "case {} on {}: a well-formed identifier is either refused as input \
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

/// RULE at the bound: the minted range is the only precondition, so a value the
/// mint COULD have rendered is refused by nothing — every route, the three that
/// state the range among them, resolves it against the store.
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
