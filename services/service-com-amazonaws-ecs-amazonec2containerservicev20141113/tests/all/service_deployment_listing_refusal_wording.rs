// SPDX-License-Identifier: CC0-1.0

//! What the SERVICE-DEPLOYMENT listing says when it refuses a filter or a page size, driven
//! end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: where a refusal states the values a member accepts, or the range a
//! page size has to fall in, the set and the sentence are the ones that OPERATION publishes —
//! so a caller reading the message is told what this operation takes, in the order it states
//! it. Two members of `ListServiceDeployments` publish such a statement:
//!
//! * `status` (`ServiceDeploymentStatusList`): the clause names the LIST the entries came in
//!   and prints the statuses the listing accepts, in the order it prints them —
//!   `IN_PROGRESS, CREATED, ROLLBACK_IN_PROGRESS, STOPPED, STOP_REQUESTED,
//!   ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, PENDING, ROLLBACK_FAILED, SUCCESSFUL`. That is
//!   RECORDED behaviour of the operation and is neither the order nor the membership of the
//!   modelled enum (`smithy_query(query="shape",shape="ServiceDeploymentStatus")`: PENDING,
//!   SUCCESSFUL, STOPPED, STOP_REQUESTED, IN_PROGRESS, ROLLBACK_REQUESTED,
//!   ROLLBACK_IN_PROGRESS, ROLLBACK_SUCCESSFUL, ROLLBACK_FAILED), which declares no
//!   `CREATED`: a filter naming a status the model does not declare is refused rather than
//!   served, with that same set printed.
//! * `maxResults` ("This value can be between 1 and 100."): the sentence states the whole
//!   span, naming both of its ends, at either end crossed and however far past it the value
//!   is — unlike the long-standing listings, which name the one bound the value crossed, and
//!   the daemon-plane listings, which state their range in words of their own
//!   ([`crate::page_size_range_refusals`] pins both).
//!
//! Nothing here changes WHICH requests are refused: each test also drives the requests the
//! same rule serves — every status the model declares as a filter, and each published page
//! bound — and every case goes through the same front door a client uses, so a refusal or a
//! page that is not computed from persisted state cannot pass.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, refusal, register_definition, service};

/// The cluster the listing below reports on.
const CLUSTER: &str = "deplistcluster";

/// The bridge-mode revision the fixture's service runs.
const FAMILY: &str = "deplistfamily";

/// The service whose deployment history every listing below asks for.
const SERVICE: &str = "deplistsvc";

/// The one error code this service models for a request whose parameters it will not serve.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The statuses `ListServiceDeployments` PUBLISHES for its `status` filter, in the order it
/// publishes them, as the clause prints them. RECORDED behaviour of the operation: `CREATED`
/// is published and is not a member of the modelled `ServiceDeploymentStatus`.
const PUBLISHED_STATUSES: &str = "IN_PROGRESS, CREATED, ROLLBACK_IN_PROGRESS, STOPPED, \
                                  STOP_REQUESTED, ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, \
                                  PENDING, ROLLBACK_FAILED, SUCCESSFUL";

/// The statuses the MODEL declares for `ServiceDeploymentStatus`
/// (`smithy_query(query="shape",shape="ServiceDeploymentStatus")`), each of which the filter
/// serves.
const MODELLED_STATUSES: [&str; 9] = [
    "PENDING",
    "SUCCESSFUL",
    "STOPPED",
    "STOP_REQUESTED",
    "IN_PROGRESS",
    "ROLLBACK_REQUESTED",
    "ROLLBACK_IN_PROGRESS",
    "ROLLBACK_SUCCESSFUL",
    "ROLLBACK_FAILED",
];

/// The page-size range `ListServiceDeployments$maxResults` documents: "This value can be
/// between 1 and 100."
const MIN_PAGE: i64 = 1;
const MAX_PAGE: i64 = 100;

/// The clause a `status` list holding an entry outside the set earns: the whole list as the
/// value, under the name the API prints for the member, with the element's own requirement
/// nested inside the list's and the published set inside that.
fn status_clause(carried: &[&str]) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at 'statusFilter' failed to satisfy \
         constraint: Member must satisfy constraint: [Member must satisfy enum value set: \
         [{PUBLISHED_STATUSES}]]",
        carried.join(", ")
    )
}

/// The sentence a page size outside the documented range earns from this listing: the member
/// as the refusal names it and both ends of the range, with nothing of the value the request
/// carried.
fn page_span_sentence() -> String {
    format!("Max results must be between {MIN_PAGE} and {MAX_PAGE}")
}

/// A router holding one cluster with capacity, one revision and one service — so the listing
/// has a history to report and every refusal below is earned for the value under test rather
/// than for a resource the request could not resolve.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    register_definition(&router, FAMILY, "fixture").await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        "fixture",
    )
    .await;
    router
}

#[tokio::test]
async fn a_status_filter_outside_the_set_prints_the_statuses_the_listing_publishes() {
    let router = fixture().await;

    // The rows differ in the FORM of the entry outside the set (a member spelled in the wrong
    // case, a value that names no status at all, a near-miss, an empty string, a status the
    // service publishes but the model does not declare), in how many entries are at fault and
    // in where they sit — because one list is ONE violation however many of its entries are
    // outside the set, and nothing in the printed set comes from the request.
    let rows: [(&str, Vec<&str>); 7] = [
        ("one entry in the wrong case", vec!["successful"]),
        ("one entry that names no status", vec!["ROLLED_BACK"]),
        ("a near-miss of a member", vec!["ROLLBACK"]),
        ("an empty entry", vec![""]),
        // `CREATED` is in the published set and absent from the modelled enum, so a filter
        // naming it is refused — with the set that lists it.
        ("a published status the model does not declare", vec!["CREATED"]),
        ("a bad entry after a good one", vec!["IN_PROGRESS", "created"]),
        ("a bad entry before a good one", vec!["pending", "STOPPED"]),
    ];

    for (case, statuses) in rows {
        let (code, message) = refusal(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "status": statuses }),
            case,
        )
        .await;
        let expected = status_clause(&statuses);
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected.as_str()),
            "case {case:?}: expected the clause the list earns, got {code:?} with {message:?}"
        );
    }

    // Every status the MODEL declares is served as a filter, each on its own and all of them
    // together: the printed set decides nothing about which requests are refused.
    for status in MODELLED_STATUSES {
        let case = format!("filtering on {status}");
        let listed = ok(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "status": [status] }),
            &case,
        )
        .await;
        let reported = list(&listed, "serviceDeployments", &case);
        assert!(
            reported.len() <= 1,
            "case {case:?}: expected at most the service's own deployment, got {listed}"
        );
    }
    let case = "filtering on every modelled status";
    let listed = ok(
        &router,
        "ListServiceDeployments",
        json!({ "cluster": CLUSTER, "service": SERVICE, "status": MODELLED_STATUSES }),
        case,
    )
    .await;
    assert_eq!(
        list(&listed, "serviceDeployments", case).len(),
        1,
        "case {case:?}: expected the create's own deployment, got {listed}"
    );
}

#[tokio::test]
async fn a_service_deployment_page_size_outside_its_range_states_the_whole_span() {
    let router = fixture().await;

    // The rows differ in the END crossed and the DISTANCE past it — one step, far, and the
    // floor and the ceiling of the member's own type — because the sentence states the range
    // and nothing of the value.
    for (case, max_results) in [
        ("one below the range", 0_i64),
        ("far below the range", -37),
        ("at the floor of the member's type", i64::from(i32::MIN)),
        ("one above the range", MAX_PAGE + 1),
        ("far above the range", 5_000),
        ("at the ceiling of the member's type", i64::from(i32::MAX)),
    ] {
        let (code, message) = refusal(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "maxResults": max_results }),
            case,
        )
        .await;
        let expected = page_span_sentence();
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected.as_str()),
            "case {case:?}: expected the span this listing states, got {code:?} with \
             {message:?}"
        );
    }

    // Each published bound is INSIDE the range, so it is paged with rather than refused.
    for (case, max_results) in [
        ("the smallest page", MIN_PAGE),
        ("the largest page", MAX_PAGE),
    ] {
        let listed = ok(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "maxResults": max_results }),
            case,
        )
        .await;
        let reported = list(&listed, "serviceDeployments", case);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected a page inside {MIN_PAGE}..={MAX_PAGE} to hold the \
             service's own deployment, got {listed}"
        );
    }
}
