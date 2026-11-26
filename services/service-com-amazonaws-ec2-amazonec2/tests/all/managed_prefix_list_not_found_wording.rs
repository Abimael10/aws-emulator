// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the WORDING of EC2's `InvalidPrefixListID.NotFound` refusal across
//! the managed-prefix-list surface, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//! ec2Query is selected by the `ec2` signing name in the request's SigV4 credential scope,
//! and the Region in that scope is the Region the service answers for, so every call here
//! carries one.
//!
//! The rule under test: EC2 words the refusal of a prefix list id the caller can see no list
//! for BY THE ROLE the asking operation plays on that list — an operation that resolves one
//! to REPORT ON it (or to remove it) answers HTTP 400 `InvalidPrefixListID.NotFound` with
//! `The prefix list (<id>) does not exist.`, while the operation that REVISES one,
//! `ModifyManagedPrefixList`, answers the same code with `Prefix List <id> not found.`; in
//! both the only variable part is the offending id, copied from the member of the request
//! that named it.
//!
//! The rows of the table below differ along what that generalises over:
//!
//! * which operation resolves the id, and which of the two roles it plays — the narrowed
//!   describe, the entry report, the association report and the delete on the reporting side,
//!   and the revision on the writing side;
//! * the FORM of the id: both lengths EC2 mints a prefix list id in;
//! * WHY the id resolves to nothing: one no list of this account ever carried, and one that
//!   named a real list until the clock removed it after a delete;
//! * where in `PrefixListIds` the offending id sits: a request naming a live list beside an
//!   unknown one is refused for the unknown one rather than answered with the live list.
//!
//! `RestoreManagedPrefixListVersion` resolves a list the same way, so it is a row here too.
//! `CreateTransitGatewayPrefixListReference` is not: it resolves a transit gateway route
//! table before the prefix list, so its refusal of an unknown prefix list id is reached only
//! through another resource's fixture and is covered by that operation's own suite.
//!
//! Every wait here is a POLL of the condition with a deadline far past the service's own
//! transition span, so nothing asserted depends on how fast the host runs.

use std::time::Duration;

use ec2::state::{
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_STATE_CREATE_COMPLETE, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// EC2's code for a request naming an id no prefix list the caller can see carries — the one
/// code every row below answers, whichever sentence it carries.
const NOT_FOUND: &str = "InvalidPrefixListID.NotFound";

/// The first entry of `DescribeManagedPrefixLists`'s `PrefixListIds`, as ec2Query flattens
/// that list onto the wire: the member's `xmlName` is `PrefixListId`
/// (`smithy_query(query="shape",shape="DescribeManagedPrefixListsRequest")`), indexed from 1.
const PREFIX_LIST_IDS_FIELD: &str = "PrefixListId.1";

/// The second entry of the same list, for the rows that name two ids.
const SECOND_PREFIX_LIST_IDS_FIELD: &str = "PrefixListId.2";

/// The deadline the polls below give up at: a large multiple of the service's own transition
/// span, so an assertion is about what the clock owes and never about how fast the host runs.
fn move_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of that span, so a poll inside
/// the deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `DescribeManagedPrefixLists` narrowed to `prefix_list_id`.
async fn describe_one(router: &MultiServiceRouter, prefix_list_id: &str) -> (u16, String) {
    call(
        router,
        "DescribeManagedPrefixLists",
        &[param(PREFIX_LIST_IDS_FIELD, prefix_list_id)],
    )
    .await
}

/// One prefix list of the account carrying one IPv4 entry, as the id the create minted.
async fn create_one(router: &MultiServiceRouter, prefix_list_name: &str) -> String {
    let (status, body) = call(
        router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", prefix_list_name),
            param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
            param("MaxEntries", "5"),
            param("Entry.1.Cidr", "10.10.0.0/32"),
            param("Entry.1.Description", "seed"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {prefix_list_name:?}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("creating {prefix_list_name:?}: no <prefixListId> in {body}"))
}

/// Look at the list AGAIN until the narrowed describe reports it in `wanted`, or a deadline
/// far past the service's own transition span passes.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    wanted: &str,
) {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let (status, body) = describe_one(router, prefix_list_id).await;
        if status == 200 && element(&body, "state").as_deref() == Some(wanted) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {prefix_list_id} to reach state {wanted:?}, last read \
             {status} with body {body}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Look at the list AGAIN until no read reports it at all — the removal the clock owed the
/// delete — or a deadline far past the service's own transition span passes.
async fn poll_until_gone(router: &MultiServiceRouter, case: &str, prefix_list_id: &str) {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let (status, body) = describe_one(router, prefix_list_id).await;
        if status == 400 && error_code(&body) == NOT_FOUND {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {prefix_list_id} to be removed, last read {status} with \
             body {body}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One list of the account, created, settled, deleted and left until the clock has removed it:
/// the id of a list that WAS there, which is the case a deletion-confirmation poll asks about.
async fn removed_list_id(router: &MultiServiceRouter, case: &str) -> String {
    let prefix_list_id = create_one(router, "the-deleted-list").await;
    poll_until_state(router, case, &prefix_list_id, PREFIX_LIST_STATE_CREATE_COMPLETE).await;
    let (status, body) = call(
        router,
        "DeleteManagedPrefixList",
        &[param("PrefixListId", &prefix_list_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete of {prefix_list_id} to succeed, got {status} with \
         body {body}"
    );
    poll_until_gone(router, case, &prefix_list_id).await;
    prefix_list_id
}

/// Whether an operation RESOLVES a prefix list to report on (or remove) it, or REVISES it —
/// which is what decides the sentence EC2 words its not-found refusal with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The operations that report on, or remove, the list the caller named.
    Reports,
    /// `ModifyManagedPrefixList`, the operation that revises one.
    Revises,
}

/// EC2's sentence for an unresolvable prefix list id, as the operation in `role` words it,
/// naming `prefix_list_id` and nothing else.
fn expected_message(role: Role, prefix_list_id: &str) -> String {
    match role {
        Role::Reports => format!("The prefix list ({prefix_list_id}) does not exist."),
        Role::Revises => format!("Prefix List {prefix_list_id} not found."),
    }
}

/// One operation that resolves a caller-named prefix list.
struct Resolver {
    /// The ec2Query `Action` to send.
    action: &'static str,
    /// The form fields naming `prefix_list_id`, beside whatever else the operation requires to
    /// get as far as resolving it.
    params: fn(&str) -> Vec<Param>,
    /// Which sentence this operation's role earns.
    role: Role,
}

/// Every operation of this crate that refuses an unresolvable prefix list id under
/// [`NOT_FOUND`], with the role each plays on the list it resolves.
fn resolvers() -> Vec<Resolver> {
    vec![
        Resolver {
            action: "DescribeManagedPrefixLists",
            params: |id| vec![param(PREFIX_LIST_IDS_FIELD, id)],
            role: Role::Reports,
        },
        Resolver {
            action: "GetManagedPrefixListEntries",
            params: |id| vec![param("PrefixListId", id)],
            role: Role::Reports,
        },
        Resolver {
            action: "GetManagedPrefixListAssociations",
            params: |id| vec![param("PrefixListId", id)],
            role: Role::Reports,
        },
        Resolver {
            action: "DeleteManagedPrefixList",
            params: |id| vec![param("PrefixListId", id)],
            role: Role::Reports,
        },
        Resolver {
            action: "RestoreManagedPrefixListVersion",
            params: |id| {
                vec![
                    param("PrefixListId", id),
                    param("CurrentVersion", "2"),
                    param("PreviousVersion", "1"),
                ]
            },
            role: Role::Reports,
        },
        Resolver {
            action: "ModifyManagedPrefixList",
            params: |id| {
                vec![
                    param("PrefixListId", id),
                    param("CurrentVersion", "1"),
                    param("AddEntry.1.Cidr", "10.10.3.0/32"),
                ]
            },
            role: Role::Revises,
        },
    ]
}

#[tokio::test]
async fn an_unresolvable_prefix_list_id_is_refused_in_the_wording_of_the_asking_operation() {
    let (router, _engine) = super::ec2_query::emulator();

    // The ids that resolve to nothing: both lengths EC2 mints a prefix list id in, neither of
    // which this account holds a list under, and one that named a list of this account until
    // the clock removed it after a delete.
    let removed = removed_list_id(&router, "a list the clock removed").await;
    let ids: Vec<(String, String)> = vec![
        (
            "a short id no list ever carried".to_string(),
            "pl-0a1b2c3d".to_string(),
        ),
        (
            "a long id no list ever carried".to_string(),
            "pl-00000000000000000".to_string(),
        ),
        ("the id of a deleted list".to_string(), removed),
    ];

    for (why, prefix_list_id) in &ids {
        for resolver in resolvers() {
            let case = format!("{why} asked of {action}", action = resolver.action);
            let (status, body) =
                call(&router, resolver.action, &(resolver.params)(prefix_list_id)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                NOT_FOUND,
                "case {case:?}: expected code {NOT_FOUND}, got {actual:?} in body {body}",
                actual = error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                expected_message(resolver.role, prefix_list_id),
                "case {case:?}: expected the wording of this operation's role, got {actual:?}",
                actual = error_message(&body)
            );
        }
    }

    // The two roles word ONE code differently, so a client keying off the text can still tell a
    // report of a list that is gone from a refused revision of it.
    assert_ne!(
        expected_message(Role::Reports, "pl-0a1b2c3d"),
        expected_message(Role::Revises, "pl-0a1b2c3d"),
        "the reporting and the revising wordings must stay distinct"
    );
}

#[tokio::test]
async fn a_describe_naming_a_live_list_beside_an_unknown_id_is_refused_for_the_unknown_id() {
    let (router, _engine) = super::ec2_query::emulator();

    let case = "a live list beside an unknown id";
    let live = create_one(&router, "the-live-list").await;
    poll_until_state(&router, case, &live, PREFIX_LIST_STATE_CREATE_COMPLETE).await;

    // The offending id is the one the request named, wherever in `PrefixListIds` it sits: the
    // live list is reported by neither ordering.
    for (position, params) in [
        (
            "the unknown id last",
            vec![
                param(PREFIX_LIST_IDS_FIELD, &live),
                param(SECOND_PREFIX_LIST_IDS_FIELD, "pl-00000000000000bad"),
            ],
        ),
        (
            "the unknown id first",
            vec![
                param(PREFIX_LIST_IDS_FIELD, "pl-00000000000000bad"),
                param(SECOND_PREFIX_LIST_IDS_FIELD, &live),
            ],
        ),
    ] {
        let (status, body) = call(&router, "DescribeManagedPrefixLists", &params).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, NOT_FOUND),
            "case {position:?}: expected 400 {NOT_FOUND}, got {status} with body {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message(Role::Reports, "pl-00000000000000bad"),
            "case {position:?}: expected the unknown id to be the one named, got {actual:?}",
            actual = error_message(&body)
        );
    }

    // A refused read changed nothing: the live list is still reported on its own.
    let (status, body) = describe_one(&router, &live).await;
    assert_eq!(
        (status, element(&body, "prefixListId").as_deref()),
        (200, Some(live.as_str())),
        "case {case:?}: the live list must still be reported, got {status} with body {body}"
    );
}
