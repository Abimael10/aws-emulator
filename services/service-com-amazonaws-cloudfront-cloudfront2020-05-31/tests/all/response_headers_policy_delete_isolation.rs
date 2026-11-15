// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteResponseHeadersPolicy` leaves ALONE, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! [`crate::response_headers_policy_delete`] asserts what the delete does to the policy it
//! names. The rule this suite adds is the other half of "Deletes a response headers policy":
//! a landed delete removes EXACTLY the one stored record its `Id` names, so afterwards every
//! other policy the account holds is still readable through both readers at the same version
//! token it carried before, the listing still reports each of them (and reports one fewer
//! item), and the catalogue AWS publishes is untouched; a REFUSED delete removes nothing at
//! all.
//!
//! The rows span what that rule generalises over — how many policies the account holds (one,
//! two, several) and in WHICH ORDER they are deleted (creation order, its reverse, and from
//! the middle outwards), since the record a delete reaches is addressed by id and not by
//! where it sits in the store's order — and each step of each row interleaves a refused
//! delete, so "nothing else moved" is asserted after a refusal as well as after a success.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the writer and readers are the
//! response headers policy suites', and the delete and its gone-check are
//! [`crate::response_headers_policy_delete`]'s — reused rather than copied.

use std::collections::BTreeSet;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MANAGED_RESPONSE_HEADERS_POLICIES;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer};
use crate::response_headers_policy_config_read::get_policy_config;
use crate::response_headers_policy_delete::{assert_gone, delete_policy};
use crate::response_headers_policy_lifecycle::get_policy;
use crate::response_headers_policy_refusal_message::{create_one, list_policies};

/// The two values `ResponseHeadersPolicyType` admits, which the listing's `Type` filter
/// draws from (from `smithy_query` on `ResponseHeadersPolicyType`): the account's own
/// records, and the ones AWS publishes.
const TYPE_CUSTOM: &str = "custom";
const TYPE_MANAGED: &str = "managed";

/// A version token in the shape this service mints (`E` followed by 13 characters of the id
/// alphabet) that is no policy's CURRENT one, so a delete under it is refused as an unmet
/// precondition rather than as an unusable token.
const STALE_VERSION: &str = "E1STALE1VERSIO";

/// The policy family's precondition sentence, spelled out here rather than taken from the
/// crate, so a reworded refusal fails this suite.
const PRECONDITION_FAILED_MESSAGE: &str =
    "The request failed because it didn't meet the preconditions for this operation.";

// ---------------------------------------------------------------------------
// Reading the account's policies back
// ---------------------------------------------------------------------------

/// The ids `ListResponseHeadersPolicies` reports for `kind`, in the order the listing serves
/// them, with the wrapper's own `Quantity` checked against the items it carries.
async fn listed_ids(router: &MultiServiceRouter, kind: &str, case: &str) -> Vec<String> {
    let listed = list_policies(router, &format!("?Type={kind}")).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected HTTP 200 from ListResponseHeadersPolicies for {kind}, got {} \
         with {}",
        listed.status, listed.raw
    );
    let ids = summary_ids(&listed);
    let quantity = listed
        .root()
        .text_at("Quantity")
        .unwrap_or_default()
        .parse::<usize>()
        .unwrap_or_else(|_| {
            panic!(
                "case {case:?}: expected the {kind} listing to state its own length, got {}",
                listed.raw
            )
        });
    assert_eq!(
        quantity,
        ids.len(),
        "case {case:?}: expected the {kind} listing's Quantity to be the number of summaries \
         it carries ({}), got {quantity}",
        ids.len()
    );
    ids
}

/// The policy ids a listing's summaries carry.
fn summary_ids(listed: &Answer) -> Vec<String> {
    listed
        .root()
        .at("Items")
        .map(|items| items.children_named("ResponseHeadersPolicySummary"))
        .unwrap_or_default()
        .into_iter()
        .map(|summary| {
            summary
                .text_at("ResponseHeadersPolicy/Id")
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Assert that every policy in `expected` — `(id, version token)` pairs — is still exactly
/// where it was: readable through both readers at that token, and reported by the listing of
/// the account's own policies, which reports nothing else.
async fn assert_intact(
    router: &MultiServiceRouter,
    expected: &[(String, String)],
    case: &str,
    context: &str,
) {
    for (id, etag) in expected {
        for (reader, answer) in [
            ("GetResponseHeadersPolicy", get_policy(router, id).await),
            (
                "GetResponseHeadersPolicyConfig",
                get_policy_config(router, id).await,
            ),
        ] {
            assert_eq!(
                answer.status, 200,
                "case {case:?} ({context}): expected {reader} to still serve {id:?}, got {} \
                 with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.header("etag"),
                etag.as_str(),
                "case {case:?} ({context}): expected {reader} to still report {id:?} at its own \
                 version token {etag:?}, got {:?}",
                answer.header("etag")
            );
        }
    }

    let listed = listed_ids(router, TYPE_CUSTOM, case).await;
    let reported: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let surviving: BTreeSet<&str> = expected.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        reported, surviving,
        "case {case:?} ({context}): expected the listing to report exactly the policies the \
         account still holds {surviving:?}, got {reported:?}"
    );
}

/// The ids of the catalogue AWS publishes, as the crate's own catalogue states them.
fn published_ids() -> Vec<&'static str> {
    MANAGED_RESPONSE_HEADERS_POLICIES
        .iter()
        .map(|managed| managed.id)
        .collect()
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// How many policies the account holds for a row, and the order their deletes are issued in
/// (indices into the creation order).
struct Row {
    case: &'static str,
    count: usize,
    order: &'static [usize],
}

const ROWS: &[Row] = &[
    Row {
        case: "the only policy the account holds",
        count: 1,
        order: &[0],
    },
    Row {
        case: "two policies, deleted in the order they were created",
        count: 2,
        order: &[0, 1],
    },
    Row {
        case: "two policies, deleted in the reverse of the order they were created",
        count: 2,
        order: &[1, 0],
    },
    Row {
        case: "four policies, the last created deleted first",
        count: 4,
        order: &[3, 2, 1, 0],
    },
    Row {
        case: "five policies, deleted from the middle outwards",
        count: 5,
        order: &[2, 1, 3, 0, 4],
    },
];

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Each row's `order` really is a deletion order over that row's policies: every policy
/// deleted exactly once. A mistyped row would otherwise weaken the suite silently.
#[test]
fn every_row_deletes_each_of_its_policies_exactly_once() {
    for row in ROWS {
        let visited: BTreeSet<usize> = row.order.iter().copied().collect();
        assert_eq!(
            (visited.len(), row.order.len(), visited.iter().next_back()),
            (row.count, row.count, Some(&(row.count - 1))),
            "case {:?}: expected the deletion order to visit each of the {} policies once, got \
             {:?}",
            row.case,
            row.count,
            row.order
        );
    }
}

/// A delete removes exactly the record its id names: every other policy the account holds
/// survives it unchanged, the listing reports one fewer item, and a refused delete removes
/// nothing at all.
#[tokio::test]
async fn a_delete_removes_only_the_policy_it_names() {
    let published = published_ids();

    for (index, row) in ROWS.iter().enumerate() {
        let router = emulator();

        // The account's own policies, in creation order.
        let mut held: Vec<(String, String)> = Vec::new();
        for slot in 0..row.count {
            held.push(create_one(&router, &format!("delete-isolation-{index}-{slot}")).await);
        }
        assert_intact(&router, &held, row.case, "before any delete").await;

        let mut surviving = held.clone();
        for target in row.order {
            let (id, etag) = held[*target].clone();

            // A delete under a version that is not this policy's current one is refused, and
            // the store is exactly as it was: the policy it named included.
            let refused = delete_policy(&router, &id, Some(STALE_VERSION)).await;
            assert_eq!(
                refused.status, 412,
                "case {:?}: expected HTTP 412 deleting {id:?} under a stale version, got {} \
                 with {}",
                row.case, refused.status, refused.raw
            );
            assert_eq!(
                refused.error_code(),
                "PreconditionFailed",
                "case {:?}: expected PreconditionFailed deleting {id:?} under a stale version, \
                 got {}",
                row.case,
                refused.raw
            );
            let served = refused.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                served, PRECONDITION_FAILED_MESSAGE,
                "case {:?}: expected the policy family's precondition sentence, got {served:?}",
                row.case
            );
            assert_intact(&router, &surviving, row.case, "after a refused delete").await;

            let before = surviving.len();
            let deleted = delete_policy(&router, &id, Some(&etag)).await;
            assert_eq!(
                deleted.status, 204,
                "case {:?}: expected HTTP 204 deleting {id:?} under its own version, got {} \
                 with {}",
                row.case, deleted.status, deleted.raw
            );

            surviving.retain(|(held_id, _)| held_id != &id);
            assert_gone(&router, &id, row.case).await;
            assert_intact(&router, &surviving, row.case, "after the delete").await;
            assert_eq!(
                surviving.len(),
                before - 1,
                "case {:?}: expected the delete of {id:?} to take exactly one policy out of the \
                 account's {before}, got {} left",
                row.case,
                surviving.len()
            );

            // Nothing a delete of this account's record does reaches the catalogue AWS
            // publishes: no call of this service creates, changes or removes one of those.
            let listed = listed_ids(&router, TYPE_MANAGED, row.case).await;
            assert_eq!(
                listed, published,
                "case {:?}: expected the published catalogue to be whole after the delete of \
                 {id:?}, got {listed:?}",
                row.case
            );
        }

        // The account holds nothing of its own now, and a page that lists nothing carries no
        // items at all.
        let empty = list_policies(&router, &format!("?Type={TYPE_CUSTOM}")).await;
        assert_eq!(
            empty.status, 200,
            "case {:?}: expected HTTP 200 from the listing once every policy is deleted, got {} \
             with {}",
            row.case, empty.status, empty.raw
        );
        assert_eq!(
            empty.root().text_at("Quantity"),
            Some("0"),
            "case {:?}: expected the listing to report no policies of this account, got {}",
            row.case,
            empty.raw
        );
        assert!(
            empty.root().at("Items").is_none(),
            "case {:?}: expected no Items element at all once every policy is deleted, got {}",
            row.case,
            empty.raw
        );
    }
}
