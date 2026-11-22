// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what an ACCEPTED `DeleteCluster` leaves behind, driven over
//! the wire through the same `MultiServiceRouter` the production binary mounts.
//!
//! RULE: a delete this service ACCEPTS starts a deletion and does not complete it.
//! Until the clock finishes that deletion the resource is still there in its
//! `DELETING` status — an independent `DescribeCluster` answers 200 with that status
//! and with the identifiers the resource always had, the listing still names it, the
//! name is still taken, and a delete sent again is answered with the deletion that
//! is already running — and only once the deletion has finished does the name answer
//! 404. That is what every client waiting out a teardown polls for: a deletion whose
//! transient status no reader can observe gives a waiter the wrong answer (absent) at
//! an instant when the resource is still being torn down.
//!
//! The rows differ along what the rule generalises over: whether the cluster had
//! reached `ACTIVE` or was still `CREATING` when the delete was accepted, the Region
//! the request is signed for, whether the cluster carries tags, and which reader
//! observes the window (the describe, the listing, the delete sent again, a create
//! under the same name).
//!
//! The wire harness (`Answer`, `emulator`, `call`, `encode_label`) is the shared one
//! in `tests/all/wire.rs` and the `CreateCluster` body is `cluster_lifecycle`'s: no
//! suite grows its own client.

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    cluster_not_found_message, STATUS_DELETING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{minimal_request, REGION};
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// Another Region a request may be signed for, so a row can show the rule holding
/// wherever the cluster lives: the Region is read from the request's own credential
/// scope.
const OTHER_REGION: &str = "eu-west-1";

async fn create_cluster(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
) -> Answer {
    call(router, region, "POST", "/clusters", &body.to_string()).await
}

async fn describe_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn delete_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "DELETE",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn list_clusters(router: &MultiServiceRouter, region: &str) -> Answer {
    call(router, region, "GET", "/clusters", "").await
}

/// The names `ListClusters` answered with.
fn listed_names(answer: &Answer) -> Vec<String> {
    answer.body["clusters"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// One row: what the cluster is, and in which Region.
struct Row {
    case: &'static str,
    region: &'static str,
    name: &'static str,
    /// Whether the clock is given its `CREATING` -> `ACTIVE` move before the delete.
    settle: bool,
    /// Whether the create carries tags, so a row shows the window holding for a
    /// cluster that carries more than the required members.
    tagged: bool,
}

// ---------------------------------------------------------------------------
// RULE: an accepted `DeleteCluster` leaves the cluster readable, listed and in the
// way of its own name in `DELETING` until the clock has finished the deletion; only
// then is it absent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_accepted_delete_leaves_the_cluster_deleting_until_the_deletion_finishes() {
    let rows = [
        Row {
            case: "a cluster that reached ACTIVE",
            region: REGION,
            name: "window-active",
            settle: true,
            tagged: false,
        },
        Row {
            case: "a cluster still CREATING",
            region: REGION,
            name: "window-creating",
            settle: false,
            tagged: false,
        },
        Row {
            case: "a tagged cluster in another Region",
            region: OTHER_REGION,
            name: "window_tagged-2",
            settle: true,
            tagged: true,
        },
    ];

    for row in rows {
        let Row {
            case,
            region,
            name,
            settle,
            tagged,
        } = row;
        // ONE router per row, so the listing assertions name exactly this cluster.
        let router = emulator();
        let body = {
            let mut request = minimal_request(name);
            if tagged {
                request["tags"] = serde_json::json!({ "owner": "platform" });
            }
            request
        };
        let created = create_cluster(&router, region, &body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup: the cluster must be created, got {created:?}"
        );
        if settle {
            read_until(
                case,
                "the cluster reported ACTIVE",
                || describe_cluster(&router, region, name),
                |read| read.cluster()["status"] == serde_json::json!("ACTIVE"),
            )
            .await;
        }
        let before = describe_cluster(&router, region, name).await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: setup: the cluster must read back, got {before:?}"
        );

        // THE DELETE IS ACCEPTED and reports the status the cluster is moving into.
        let deleted = delete_cluster(&router, region, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected 200 from DeleteCluster, got {deleted:?}"
        );
        assert_eq!(
            deleted.cluster()["status"],
            serde_json::json!(STATUS_DELETING),
            "case {case:?}: the accepted delete reports {STATUS_DELETING}, got {:?}",
            deleted.cluster()["status"]
        );

        // AN INDEPENDENT READ SEES THE SAME THING: the cluster is still there, in
        // that status, under the identifiers it always had.
        let during = describe_cluster(&router, region, name).await;
        assert_eq!(
            during.status, 200,
            "case {case:?}: an independent read of a deleting cluster must be served, got \
             {during:?}"
        );
        assert_eq!(
            during.cluster()["status"],
            serde_json::json!(STATUS_DELETING),
            "case {case:?}: the independent read must report {STATUS_DELETING}, got {:?}",
            during.cluster()["status"]
        );
        for member in ["arn", "name", "createdAt"] {
            assert_eq!(
                during.cluster()[member],
                before.cluster()[member],
                "case {case:?}: the deleting cluster keeps its {member}, got {:?}",
                during.cluster()[member]
            );
        }
        // And the listing still names it: a waiter that lists until the name is gone
        // reads the same window the describe does.
        assert_eq!(
            listed_names(&list_clusters(&router, region).await),
            vec![name.to_owned()],
            "case {case:?}: a cluster being deleted is still listed"
        );

        // THE NAME IS STILL TAKEN, and the deletion is running: a delete sent again
        // is answered with that deletion — the same cluster, the same ARN, the status
        // the deletion put it in — and a create under that name is the conflict a
        // taken name earns; neither is an absent cluster.
        let again = delete_cluster(&router, region, name).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: a delete sent again inside the window is answered with the running \
             deletion, got {again:?}"
        );
        assert_eq!(
            (&again.cluster()["status"], &again.cluster()["arn"]),
            (&deleted.cluster()["status"], &deleted.cluster()["arn"]),
            "case {case:?}: the delete sent again answers what the accepted one did, got \
             {again:?}"
        );
        let retaken = create_cluster(&router, region, &body).await;
        assert_eq!(
            (retaken.status, error_code(&retaken).as_str()),
            (409, "ResourceInUseException"),
            "case {case:?}: the name is taken while the deletion runs, got {retaken:?}"
        );

        // ONCE THE DELETION HAS FINISHED the cluster is absent: the read is the 404
        // this API states for a cluster it does not hold, the listing drops it, a
        // delete is a not-found and the name is free again. Waited for by LOOKING
        // AGAIN rather than for a span ([`crate::wire::read_until`]).
        let gone = read_until(
            case,
            "the deleted cluster became absent",
            || describe_cluster(&router, region, name),
            |read| read.status == 404,
        )
        .await;
        assert_eq!(
            error_code(&gone),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException once the deletion finished, \
             got {gone:?}"
        );
        assert_eq!(
            gone.message(),
            cluster_not_found_message(name),
            "case {case:?}: expected the missing-cluster sentence, got {:?}",
            gone.message()
        );
        assert!(
            listed_names(&list_clusters(&router, region).await).is_empty(),
            "case {case:?}: a cluster whose deletion finished is no longer listed"
        );
        let after = delete_cluster(&router, region, name).await;
        assert_eq!(
            (after.status, error_code(&after).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a delete once the deletion finished is a not-found, got {after:?}"
        );
        let recreated = create_cluster(&router, region, &body).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: the name is free once the deletion finished, got {recreated:?}"
        );
        assert_eq!(
            recreated.cluster()["status"],
            serde_json::json!("CREATING"),
            "case {case:?}: the name created again is a new cluster, got {recreated:?}"
        );
    }
}

/// The LISTING follows the deletion on its own: a client that only ever lists sees
/// the cluster while it is being deleted and stops seeing it once the deletion has
/// finished, with no read of that cluster having been made in between.
///
/// Worth its own row because the removal is a move the CLOCK owes: an operation that
/// reports the store has to materialise what is due before it answers, or a listing
/// names a cluster a read of the same name answers 404 for.
#[tokio::test]
async fn the_listing_follows_the_deletion_with_no_read_in_between() {
    let router = emulator();
    let name = "window-listed";
    let created = create_cluster(&router, REGION, &minimal_request(name)).await;
    assert_eq!(created.status, 200, "setup: {created:?}");

    let deleted = delete_cluster(&router, REGION, name).await;
    assert_eq!(
        deleted.status, 200,
        "the delete must be accepted, got {deleted:?}"
    );
    assert_eq!(
        listed_names(&list_clusters(&router, REGION).await),
        vec![name.to_owned()],
        "a cluster being deleted is still listed"
    );

    read_until(
        "the listing follows the deletion",
        "the listing dropped the deleted cluster",
        || list_clusters(&router, REGION),
        |listed| listed_names(listed).is_empty(),
    )
    .await;

    // And the name answers the not-found a client's waiter is looking for.
    let gone = describe_cluster(&router, REGION, name).await;
    assert_eq!(
        (gone.status, error_code(&gone).as_str()),
        (404, "ResourceNotFoundException"),
        "once the listing has dropped it the name holds nothing, got {gone:?}"
    );
}

/// The window is never ZERO: a reader acting straight after the accepted delete
/// still observes the transient status, for a cluster whose own creation window has
/// not even closed yet.
///
/// The two windows are independent, which is what the second half reads: every read
/// until the name becomes absent reports the deletion, never the `ACTIVE` the clock
/// once owed the creation. It is driven by LOOKING AGAIN rather than by waiting out
/// either span ([`crate::wire::read_until`]).
#[tokio::test]
async fn a_reader_straight_after_the_delete_still_observes_deleting() {
    let router = emulator();
    let name = "window-immediate";
    let created = create_cluster(&router, REGION, &minimal_request(name)).await;
    assert_eq!(created.status, 200, "setup: {created:?}");

    let deleted = delete_cluster(&router, REGION, name).await;
    assert_eq!(
        deleted.status, 200,
        "the delete must be accepted, got {deleted:?}"
    );

    let read = describe_cluster(&router, REGION, name).await;
    assert_eq!(
        read.cluster()["status"],
        serde_json::json!(STATUS_DELETING),
        "a read straight after the delete still sees {STATUS_DELETING}, got {read:?}"
    );

    // Every status read between here and the name becoming absent is collected, so
    // the `CREATING` -> `ACTIVE` move the clock owed the create — whose span closes
    // inside this window — can be shown not to have been applied to a cluster that
    // is being deleted.
    let observed: std::cell::RefCell<Vec<serde_json::Value>> = std::cell::RefCell::new(Vec::new());
    read_until(
        "the deletion finishes",
        "the deleted cluster became absent",
        || describe_cluster(&router, REGION, name),
        |read| {
            if read.status == 200 {
                observed.borrow_mut().push(read.cluster()["status"].clone());
            }
            read.status == 404
        },
    )
    .await;
    let observed = observed.into_inner();
    assert!(
        !observed.is_empty(),
        "the deleting cluster must have been read at least once before it was absent"
    );
    assert!(
        observed
            .iter()
            .all(|status| *status == serde_json::json!(STATUS_DELETING)),
        "a deleting cluster is never handed back to ACTIVE, saw {observed:?}"
    );
}
