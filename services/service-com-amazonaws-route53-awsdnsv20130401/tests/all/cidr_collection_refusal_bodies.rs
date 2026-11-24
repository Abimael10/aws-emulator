// SPDX-License-Identifier: CC0-1.0

//! The error BODY of the three Route 53 CIDR-collection refusals the service answers by
//! their error class alone, end to end over the router.
//!
//! The rule under test: the `Message` a refusal carries is what the service puts on the
//! wire for that error class, and an error shape's model documentation is documentation OF
//! the class rather than a sentence the service sends — so a class the service reports by
//! its identity alone leaves the shape's optional `message` member unset and the response
//! carries no `Message` element at all. Three of `ChangeCidrCollection`'s classes are
//! reported that way: a change held to a collection version the collection is not at
//! (`CidrCollectionVersionMismatchException`, HTTP 409), a change addressed at a
//! collection the account does not hold (`NoSuchCidrCollectionException`, HTTP 404), and a
//! `PUT` of a block the collection already holds in another location
//! (`CidrBlockInUseException`, HTTP 400). Each error shape carries one optional `message`
//! member and nothing else
//! (smithy_query(query="shape",shape="CidrCollectionVersionMismatchException,NoSuchCidrCollectionException,CidrBlockInUseException")),
//! so which of them puts an element on the wire is the service's own behaviour, not the
//! model's.
//!
//! The rule is about the SHAPE of the body, so it is asserted over a table of requests
//! rather than over the one that exposed it: the rows differ along the version the request
//! states (the published minimum, one past the collection's current one, one far past it,
//! the largest value the modelled `long` admits), the version the COLLECTION is at when it
//! arrives, the changes the refused request carries (one `PUT`, a `DELETE_IF_EXISTS`,
//! several changes at once) and — for the in-use refusal — the block and the two locations
//! it is held in and put in, and they are asked over two collections of one account, so a
//! body keyed to one collection could not pass.
//!
//! The other half of the rule is asserted too: a refusal of the SAME operation that is
//! neither of those two classes still states its message, so the body shape belongs to
//! those classes rather than to the operation or to the service's error envelope.
//!
//! The harness (`router`, `call`, `Reply`), the request builders and the shared body check
//! come from the suites that own them, so these tests drive the service through the same
//! router a socket-served request goes through.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::MAX_CIDR_BLOCKS_PER_COLLECTION;

use crate::cidr_collection_blocks::{
    change, change_collection, create_collection, listed_version, DELETE_IF_EXISTS, PUT,
};
use crate::hosted_zone_records::{call, router};
use crate::refusal_message_rules::{assert_identity_only_at, message_of};

/// The wire code of a change held to a version the collection is not at.
const VERSION_MISMATCH: &str = "CidrCollectionVersionMismatchException";

/// The wire code of a change addressed at a collection the account does not hold.
const NO_SUCH_COLLECTION: &str = "NoSuchCidrCollectionException";

/// The wire code of a `PUT` of a block the collection already holds elsewhere.
const BLOCK_IN_USE: &str = "CidrBlockInUseException";

/// The status the model declares for `CidrBlockInUseException`.
const BLOCK_IN_USE_STATUS: u16 = 400;

/// The status the model declares for `CidrCollectionVersionMismatchException`
/// (smithy_query(query="operation",operation="ChangeCidrCollection")).
const VERSION_MISMATCH_STATUS: u16 = 409;

/// The status the model declares for `NoSuchCidrCollectionException`.
const NO_SUCH_COLLECTION_STATUS: u16 = 404;

/// A `UUID` of the minted form that names no collection of this account.
const UNMINTED_ID: &str = "00000000-0000-4000-8000-000000000000";

/// One refused change: how far the collection has been moved before it arrives, the
/// `CollectionVersion` it states, and the changes it carries.
struct MismatchRow {
    case: &'static str,
    /// How many changes are applied BEFORE the refused one. A created collection is at
    /// `Version` 1 and "Route 53 … increments it by 1 each time you update the
    /// collection", so this is what the collection's current version is set by.
    applied_first: u32,
    /// The `CollectionVersion` the refused request states, as the request writes it.
    stated: &'static str,
    /// The changes the refused request carries, as `(location, action, blocks)`.
    changes: &'static [(&'static str, &'static str, &'static [&'static str])],
}

/// The largest value the modelled `CollectionVersion` admits: it is a `long` with
/// `range 1..` (smithy_query(query="operation",operation="ChangeCidrCollection")), so the
/// upper end is the one the type itself sets.
const LARGEST_VERSION: &str = "9223372036854775807";

const MISMATCH_ROWS: &[MismatchRow] = &[
    MismatchRow {
        case: "the published minimum version, on a collection that has moved past it",
        applied_first: 1,
        stated: "1",
        changes: &[("office", PUT, &["10.0.0.0/24"])],
    },
    MismatchRow {
        case: "the version one past the one a created collection carries",
        applied_first: 0,
        stated: "2",
        changes: &[("office", PUT, &["10.0.1.0/24"])],
    },
    MismatchRow {
        case: "a version far past the collection's current one",
        applied_first: 0,
        stated: "9999",
        changes: &[("branch", PUT, &["198.51.100.0/24"])],
    },
    MismatchRow {
        case: "the largest value the modelled long admits",
        applied_first: 0,
        stated: LARGEST_VERSION,
        changes: &[("branch", PUT, &["2001:db8::/32"])],
    },
    MismatchRow {
        case: "a version two moves back, on a request that takes a block out",
        applied_first: 2,
        stated: "1",
        changes: &[("seed-0", DELETE_IF_EXISTS, &["10.128.0.0/24"])],
    },
    MismatchRow {
        case: "a request carrying several changes at a version the collection is past",
        applied_first: 3,
        stated: "2",
        changes: &[
            ("east", PUT, &["203.0.113.0/24"]),
            ("west", DELETE_IF_EXISTS, &["10.129.0.0/24"]),
        ],
    },
];

/// Move `id` forward by `moves` applied changes, each putting a block of its own, and
/// answer the `Version` the collection is then at.
///
/// Each change carries a block no other change in this suite names, so what moves the
/// version is the change being APPLIED rather than anything about the blocks it carries.
async fn move_version(router: &MultiServiceRouter, id: &str, moves: u32) -> String {
    for step in 0..moves {
        let block = format!("10.{}.0.0/24", 128 + step);
        let applied = change_collection(
            router,
            id,
            None,
            &change(&format!("seed-{step}"), PUT, &[block.as_str()]),
        )
        .await;
        assert_eq!(
            applied.status, 200,
            "the seeding change {step} must apply, got {} with body {}",
            applied.status, applied.body
        );
    }
    listed_version(router, id).await
}

#[tokio::test]
async fn a_version_the_collection_is_not_at_is_refused_by_its_code_alone() {
    for row in MISMATCH_ROWS {
        let case = row.case;
        let router = router();
        // Two collections of one account: the refusal is asked about the first one while
        // the second one sits beside it at a version of its own, so a body built from
        // stored state could not be read as the rule holding.
        let id = create_collection(&router, "refusal-bodies").await;
        let beside = create_collection(&router, "refusal-bodies-beside").await;
        let at = move_version(&router, &id, row.applied_first).await;
        move_version(&router, &beside, 1).await;
        assert_ne!(
            at, row.stated,
            "case {case:?}: the row must state a version the collection is NOT at, got {at}"
        );

        let changes: String = row
            .changes
            .iter()
            .map(|(location, action, blocks)| change(location, action, blocks))
            .collect();
        let refused = change_collection(&router, &id, Some(row.stated), &changes).await;
        assert_identity_only_at(&refused, case, VERSION_MISMATCH_STATUS, VERSION_MISMATCH);

        // The refusal changed nothing: the collection is still at the version it was at.
        assert_eq!(
            listed_version(&router, &id).await,
            at,
            "case {case:?}: the refused change must leave the collection at version {at}"
        );
    }
}

#[tokio::test]
async fn a_collection_the_account_does_not_hold_is_refused_by_its_code_alone() {
    // The OTHER class of this operation the service reports by its identity alone, so the
    // rule is asserted over both classes rather than over the one that exposed it. The
    // rows differ along why the id names no collection: it was never minted, and it named
    // a collection until that collection was deleted.
    let router = router();
    let deleted = create_collection(&router, "refusal-bodies-deleted").await;
    let removed = call(
        &router,
        "DELETE",
        &format!("/2013-04-01/cidrcollection/{deleted}"),
        "",
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "the collection must delete so its id names none, got {} with body {}",
        removed.status, removed.body
    );

    let rows = [
        ("an unminted id of the minted form", UNMINTED_ID.to_string()),
        ("the id of a deleted collection", deleted.clone()),
    ];
    // Crossed with the versions a request can state, including none at all: whether the
    // request is held to a version does not change which body a missing collection gets.
    for (case, id) in &rows {
        for stated in [None, Some("1"), Some("9999")] {
            let case = format!("{case} at CollectionVersion {stated:?}");
            let refused = change_collection(
                &router,
                id,
                stated,
                &change("office", PUT, &["10.0.0.0/24"]),
            )
            .await;
            assert_identity_only_at(
                &refused,
                &case,
                NO_SUCH_COLLECTION_STATUS,
                NO_SUCH_COLLECTION,
            );
        }
    }
}

/// The blocks the collection `id` holds in `location`, read back through
/// `ListCidrBlocks`.
async fn blocks_in(router: &MultiServiceRouter, id: &str, location: &str) -> Vec<String> {
    let reply = call(
        router,
        "GET",
        &format!("/2013-04-01/cidrcollection/{id}/cidrblocks?location={location}"),
        "",
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "expected 200 from ListCidrBlocks, got {} with body {}",
        reply.status, reply.body
    );
    reply
        .all("CidrBlock")
        .iter()
        .map(|block| (*block).to_string())
        .collect()
}

/// One refused `PUT` of a block the collection already holds: the block, the location
/// holding it, the location the refused change names, and one further change that
/// request carries beside it.
struct InUseRow {
    case: &'static str,
    block: &'static str,
    held_in: &'static str,
    put_in: &'static str,
    /// A further change the refused request carries, as `(location, action, block)`.
    extra: Option<(&'static str, &'static str, &'static str)>,
}

#[tokio::test]
async fn a_block_the_collection_holds_elsewhere_is_refused_by_its_code_alone() {
    // The THIRD class of this operation the service reports by its identity alone. The
    // rows differ along the block's address family and prefix length, the locations it is
    // held in and put in, and whether the refused request carries the `PUT` alone or
    // beside another change — so a body keyed to one block, one location or one request
    // shape could not pass. A second collection of the account holds the same block in a
    // location of its own, so the refusal is about the collection the request named.
    let router = router();
    let id = create_collection(&router, "refusal-bodies-in-use").await;
    let beside = create_collection(&router, "refusal-bodies-in-use-beside").await;

    let rows = [
        InUseRow {
            case: "an IPv4 /24 put in a second location",
            block: "192.0.2.0/24",
            held_in: "east",
            put_in: "west",
            extra: None,
        },
        InUseRow {
            case: "an IPv4 /16 put in a second location",
            block: "198.51.0.0/16",
            held_in: "north",
            put_in: "south",
            extra: None,
        },
        InUseRow {
            case: "an IPv6 /32 put in a second location",
            block: "2001:db8::/32",
            held_in: "office",
            put_in: "branch",
            extra: None,
        },
        InUseRow {
            case: "a single-address /32 put in a second location",
            block: "203.0.113.7/32",
            held_in: "desk",
            put_in: "lab",
            extra: None,
        },
        InUseRow {
            case: "a block put in a second location beside another change",
            block: "203.0.113.128/25",
            held_in: "campus",
            put_in: "annex",
            extra: Some(("extra", DELETE_IF_EXISTS, "10.64.0.0/24")),
        },
    ];

    for row in &rows {
        let InUseRow {
            case,
            block,
            held_in,
            put_in,
            extra,
        } = *row;
        // The block is held by BOTH collections, each in a location of its own.
        for (collection, location) in [(&id, held_in), (&beside, "beside")] {
            let applied = change_collection(
                &router,
                collection,
                None,
                &change(location, PUT, &[block]),
            )
            .await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: the holding put must apply, got {} with body {}",
                applied.status, applied.body
            );
        }

        let mut changes = change(put_in, PUT, &[block]);
        if let Some((location, action, other)) = extra {
            changes.push_str(&change(location, action, &[other]));
        }
        let refused = change_collection(&router, &id, None, &changes).await;
        assert_identity_only_at(&refused, case, BLOCK_IN_USE_STATUS, BLOCK_IN_USE);

        // The refusal changed nothing: the block is still held where it was, and the
        // location the refused PUT named holds nothing.
        let listed = blocks_in(&router, &id, held_in).await;
        assert!(
            listed.contains(&block.to_string()),
            "case {case:?}: the refused change must leave {block} in {held_in}, got {listed:?}"
        );
        let elsewhere = blocks_in(&router, &id, put_in).await;
        assert!(
            !elsewhere.contains(&block.to_string()),
            "case {case:?}: the refused change must put {block} nowhere, got {elsewhere:?}"
        );
    }
}

#[tokio::test]
async fn another_refusal_of_the_same_operation_still_states_its_message() {
    // The other half of the rule: the body shape belongs to the three classes above, not
    // to this operation or to the service's envelope, so a refusal this operation answers
    // from any other class still says why. The row is the remaining class a change can be
    // refused with from inside the handler: a request past the collection's own quota.
    let router = router();
    let id = create_collection(&router, "refusal-bodies-controls").await;
    let held = "192.0.2.0/24";
    let applied = change_collection(&router, &id, None, &change("east", PUT, &[held])).await;
    assert_eq!(
        applied.status, 200,
        "the first put must apply, got {} with body {}",
        applied.status, applied.body
    );

    // One block past the quota, spread over several changes: the collection holds at most
    // `MAX_CIDR_BLOCKS_PER_COLLECTION` blocks, while one `CidrList` is bounded at 1000
    // entries by the model itself, so the request past the quota carries its blocks in
    // more than one change.
    let blocks: Vec<String> = (0..=MAX_CIDR_BLOCKS_PER_COLLECTION)
        .map(|index| format!("10.{}.{}.0/24", index / 256, index % 256))
        .collect();
    let over_quota: String = blocks
        .chunks(500)
        .enumerate()
        .map(|(group, chunk)| {
            let chunk: Vec<&str> = chunk.iter().map(String::as_str).collect();
            change(&format!("bulk-{group}"), PUT, &chunk)
        })
        .collect();

    let rows = [(
        "a request past the collection's quota",
        "LimitsExceeded",
        400u16,
        over_quota,
    )];

    for (case, code, status, changes) in &rows {
        let refused = change_collection(&router, &id, None, changes).await;
        assert_eq!(
            refused.status, *status,
            "case {case:?}: expected HTTP {status}, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            *code,
            "case {case:?}: expected the code {code:?}, got {:?}; body {}",
            refused.error_code(),
            refused.body
        );
        let message = message_of(&refused, case);
        assert!(
            !message.is_empty(),
            "case {case:?}: the refusal must state why; body {}",
            refused.body
        );
    }
}
