// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the two not-found refusals of the custom DB cluster
//! endpoint API: `CreateDBClusterEndpoint`, `DescribeDBClusterEndpoints`,
//! `ModifyDBClusterEndpoint` and `DeleteDBClusterEndpoint`.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! 1. A call of this API that resolves a `DBClusterIdentifier` — the report that narrows to
//!    one cluster and the create that hangs an endpoint off one — refuses an identifier
//!    naming no DB cluster of the region it was signed for with the modelled
//!    `DBClusterNotFoundFault` whose message is `The source cluster could not be found or
//!    cannot be accessed: ` followed by that identifier exactly as the request sent it.
//! 2. A call of this API that ADDRESSES a custom endpoint — the modification and the delete
//!    — refuses a `DBClusterEndpointIdentifier` naming no custom endpoint of that region
//!    with the modelled `DBClusterEndpointNotFoundFault` whose message is
//!    `DBClusterEndpoint ` + that identifier exactly as the request sent it + ` not found`.
//!
//! Both sentences are formatted from the REQUEST, so neither is the error shape's
//! documentation string and neither loses the identifier the caller wrote.
//!
//! The tables below span what those rules generalise over: an identifier of one character
//! and one at the longest length this service admits, a lower-case spelling, a mixed-case
//! one (which the store folds, so the refusal has to carry the caller's own spelling back)
//! and one carrying digits and hyphens; both operations of each error class; a resource that
//! EXISTS in another region, so the absence under test is a regional one rather than a
//! spelling; a resource that exists in the region, which is served rather than refused; and
//! an endpoint that existed and was deleted, whose refusal appears only once the delete the
//! clock owed has settled.

use service_com_amazonaws_rds_amazonrdsv19::state::MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// A second region, so a resource that exists is absent from the scope of a call signed for
/// another region.
const OTHER_REGION: &str = "eu-west-1";

/// The engine the clusters below run: the custom endpoint API "applies only to Aurora DB
/// clusters".
const AURORA: &str = "aurora-postgresql";

/// The DB cluster the served rows create their endpoints on.
const CLUSTER: &str = "emu-cew-cluster";

/// The status a custom endpoint reports once the transition an accepted create owed it has
/// settled.
const AVAILABLE: &str = "available";

/// The status an accepted modification reports.
const MODIFYING: &str = "modifying";

/// The fixed half of the sentence Amazon RDS answers a `DBClusterNotFoundFault` with where
/// the cluster is the SOURCE the call's own resource stands on; the request's
/// `DBClusterIdentifier` is the only variable part.
const SOURCE_CLUSTER_PREFIX: &str = "The source cluster could not be found or cannot be accessed: ";

/// The sentence Amazon RDS answers a `DBClusterEndpointNotFoundFault` with: the resource
/// type, the identifier the request sent, and `not found` with no terminal period.
fn endpoint_not_found(named: &str) -> String {
    format!("DBClusterEndpoint {named} not found")
}

/// The documentation string `DBClusterEndpointNotFoundFault` carries in the model. It
/// describes the error CLASS, so it must not be what a refusal puts on the wire.
const ENDPOINT_FAULT_DOCUMENTATION: &str = "The specified custom endpoint doesn't exist.";

/// The identifiers the tables send for a resource nothing created: one character, a plain
/// lower-case name, a mixed-case spelling, one carrying digits and hyphens, and one at the
/// longest length this service admits.
fn absent_identifiers(prefix: &str) -> Vec<String> {
    vec![
        "a".to_string(),
        format!("{prefix}-absent"),
        format!("{prefix}-Mixed-Case"),
        format!("{prefix}-7-42"),
        at_the_longest(&format!("{prefix}-long-")),
    ]
}

/// `prefix` grown to the longest identifier this API admits
/// (`MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN`), so a row sends the published bound without a
/// hand-counted literal.
fn at_the_longest(prefix: &str) -> String {
    let mut named = prefix.to_string();
    while named.len() < MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN {
        named.push('x');
    }
    assert_eq!(
        named.len(),
        MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN,
        "the padded identifier must sit at the published bound, got {named:?}"
    );
    named
}

/// `CreateDBCluster` for an Aurora cluster in `region`.
async fn create_cluster(router: &MultiServiceRouter, region: &str, identifier: &str) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("Engine", AURORA),
            ("MasterUsername", "emuadmin"),
            ("MasterUserPassword", "emu-Secret-99"),
        ],
    )
    .await
}

/// `CreateDBClusterEndpoint` with its three required members.
async fn create_endpoint(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
    cluster: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBClusterEndpoint",
        &[
            ("DBClusterEndpointIdentifier", identifier),
            ("DBClusterIdentifier", cluster),
            ("EndpointType", "ANY"),
        ],
    )
    .await
}

/// `DescribeDBClusterEndpoints` narrowed to one endpoint identifier, in `region`.
async fn describe_endpoint(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBClusterEndpoints",
        &[("DBClusterEndpointIdentifier", identifier)],
    )
    .await
}

/// Poll the report until the named endpoint reports `status`.
///
/// The read is `DescribeDBClusterEndpoints` and nothing else — a REPORTING call — so only a
/// transition this service's clock owes can end the loop.
async fn poll_endpoint_until(router: &MultiServiceRouter, case: &str, identifier: &str, status: &str) {
    let read = || describe_endpoint(router, REGION, identifier);
    poll_until(
        case,
        &format!("{identifier} reaching {status}"),
        read,
        |http_status, body| {
            http_status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|endpoint| {
                        optional_text(endpoint, "Status").as_deref() == Some(status)
                    })
        },
    )
    .await;
}

/// Poll the report until it carries no endpoint under `identifier` at all.
async fn poll_endpoint_until_gone(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let read = || describe_endpoint(router, REGION, identifier);
    poll_until(
        case,
        &format!("{identifier} leaving the store"),
        read,
        |http_status, body| {
            http_status == 200 && subtrees(body, "DBClusterEndpointList").is_empty()
        },
    )
    .await;
}

/// Assert the modelled cluster-not-found refusal and its sentence.
fn expect_source_cluster_refusal(case: &str, response: (u16, String), named: &str) {
    let body = expect_error(case, response, 404, "DBClusterNotFoundFault");
    let message = text(&body, "Message");
    assert_eq!(
        message,
        format!("{SOURCE_CLUSTER_PREFIX}{named}"),
        "case {case:?}: expected the source-cluster sentence naming {named:?}, got {message:?} \
         in {body}"
    );
}

/// Assert the modelled endpoint-not-found refusal and its sentence.
fn expect_endpoint_refusal(case: &str, response: (u16, String), named: &str) {
    let body = expect_error(case, response, 400, "DBClusterEndpointNotFoundFault");
    let message = text(&body, "Message");
    assert_eq!(
        message,
        endpoint_not_found(named),
        "case {case:?}: expected the endpoint-not-found sentence naming {named:?}, got \
         {message:?} in {body}"
    );
    assert_ne!(
        message, ENDPOINT_FAULT_DOCUMENTATION,
        "case {case:?}: the refusal must carry RDS's message, not the error shape's \
         documentation, got {body}"
    );
}

#[tokio::test]
async fn every_cluster_endpoint_call_names_the_source_cluster_it_could_not_resolve() {
    let router = emulator();

    for (index, named) in absent_identifiers("emu-cew-cluster").iter().enumerate() {
        expect_source_cluster_refusal(
            &format!("DescribeDBClusterEndpoints for {named:?}"),
            call(
                &router,
                "DescribeDBClusterEndpoints",
                &[("DBClusterIdentifier", named.clone())],
            )
            .await,
            named,
        );
        expect_source_cluster_refusal(
            &format!("CreateDBClusterEndpoint on {named:?}"),
            create_endpoint(&router, REGION, &format!("emu-cew-ep-{index}"), named).await,
            named,
        );
    }

    // A cluster that EXISTS in another region is absent from the scope of a call signed for
    // this one, and is reported absent in the same words.
    expect_ok(
        "the cluster of the other region",
        create_cluster(&router, OTHER_REGION, CLUSTER).await,
    );
    expect_source_cluster_refusal(
        "DescribeDBClusterEndpoints for a cluster of another region",
        call(
            &router,
            "DescribeDBClusterEndpoints",
            &[("DBClusterIdentifier", CLUSTER.to_string())],
        )
        .await,
        CLUSTER,
    );
    expect_source_cluster_refusal(
        "CreateDBClusterEndpoint on a cluster of another region",
        create_endpoint(&router, REGION, "emu-cew-cross-region", CLUSTER).await,
        CLUSTER,
    );

    // The same identifier is SERVED once a cluster of this region carries it, so the refusal
    // reports what the store holds rather than a shape of the request.
    expect_ok("the cluster of this region", create_cluster(&router, REGION, CLUSTER).await);
    let report = expect_ok(
        "the report of a cluster that exists",
        call(
            &router,
            "DescribeDBClusterEndpoints",
            &[("DBClusterIdentifier", CLUSTER.to_string())],
        )
        .await,
    );
    assert!(
        !subtrees(&report, "DBClusterEndpointList").is_empty(),
        "case {CLUSTER:?}: a cluster that exists reports its own endpoints, got {report}"
    );
    expect_ok(
        "the create on a cluster that exists",
        create_endpoint(&router, REGION, "emu-cew-served", CLUSTER).await,
    );
}

#[tokio::test]
async fn the_endpoint_not_found_refusal_names_the_identifier_the_request_sent() {
    let router = emulator();
    expect_ok("the cluster", create_cluster(&router, REGION, CLUSTER).await);
    expect_ok(
        "the endpoint the served rows address",
        create_endpoint(&router, REGION, "emu-cew-live", CLUSTER).await,
    );
    poll_endpoint_until(&router, "the created endpoint", "emu-cew-live", AVAILABLE).await;

    for named in absent_identifiers("emu-cew-endpoint") {
        expect_endpoint_refusal(
            &format!("ModifyDBClusterEndpoint for {named:?}"),
            call(
                &router,
                "ModifyDBClusterEndpoint",
                &[
                    ("DBClusterEndpointIdentifier", named.clone()),
                    ("EndpointType", "ANY".to_string()),
                ],
            )
            .await,
            &named,
        );
        expect_endpoint_refusal(
            &format!("DeleteDBClusterEndpoint for {named:?}"),
            call(
                &router,
                "DeleteDBClusterEndpoint",
                &[("DBClusterEndpointIdentifier", named.clone())],
            )
            .await,
            &named,
        );
    }

    // An endpoint that EXISTS in this region is absent from the scope of a call signed for
    // another one, and is named there exactly as that call sent it.
    expect_endpoint_refusal(
        "ModifyDBClusterEndpoint in another region",
        call_in(
            &router,
            OTHER_REGION,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-cew-live"),
                ("EndpointType", "ANY"),
            ],
        )
        .await,
        "emu-cew-live",
    );
    expect_endpoint_refusal(
        "DeleteDBClusterEndpoint in another region",
        call_in(
            &router,
            OTHER_REGION,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-cew-live")],
        )
        .await,
        "emu-cew-live",
    );

    // The identifier is "stored as a lowercase string", so the endpoint IS resolved from a
    // spelling in another case — the refusal above is about the record, not the spelling.
    let modified = expect_ok(
        "a modification addressed in another case",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "EMU-CEW-Live"),
                ("EndpointType", "READER"),
            ],
        )
        .await,
    );
    assert_eq!(
        text(&modified, "Status"),
        MODIFYING,
        "case \"EMU-CEW-Live\": an accepted modification reports {MODIFYING:?}, got {modified}"
    );
    poll_endpoint_until(&router, "the modified endpoint", "emu-cew-live", AVAILABLE).await;

    // Once the delete the clock owed has settled, the very identifier that was served is
    // refused with the same sentence — carrying the caller's own spelling of it.
    expect_ok(
        "the delete of the live endpoint",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-cew-live")],
        )
        .await,
    );
    poll_endpoint_until_gone(&router, "the deleted endpoint", "emu-cew-live").await;
    expect_endpoint_refusal(
        "ModifyDBClusterEndpoint after the delete settled",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "EMU-CEW-Live"),
                ("EndpointType", "ANY"),
            ],
        )
        .await,
        "EMU-CEW-Live",
    );
    expect_endpoint_refusal(
        "DeleteDBClusterEndpoint after the delete settled",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-cew-live")],
        )
        .await,
        "emu-cew-live",
    );
}
