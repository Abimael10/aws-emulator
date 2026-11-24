// SPDX-License-Identifier: CC0-1.0

//! What `DescribeAssociationExecutionTargets` answers for an execution id it holds no run
//! under.
//!
//! RULE: the not-found refusal of this read is scoped to the ASSOCIATION — an
//! `AssociationId` naming no association of the Region the request was signed for is
//! `AssociationDoesNotExist`, naming that id — while an `ExecutionId` naming no run of a
//! KNOWN association selects no target, which is an empty page (HTTP 200 with an empty
//! `AssociationExecutionTargets`) rather than a failure, the way every selecting read of
//! this service answers one that matches nothing. A client polling for the targets of a run
//! it has not observed yet is told there are none.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and the association
//! every row reads was created by a `CreateAssociation` of this suite, with the execution
//! id it reports read back through `DescribeAssociationExecutions`, so nothing is asserted
//! about state no call persisted.
//!
//! The rows differ along what the rule generalises over: the spelling of the unknown
//! execution id, whether the read narrows with a filter or a page size, and which half of
//! the pair (association, execution) is the unknown one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router, REGION};

/// A command document Systems Manager publishes whose every parameter carries a default, so
/// a create that supplies none is served (`state::PUBLISHED_DOCUMENTS`).
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A second Region, to pin that an association is reached through the endpoint it was
/// created at.
const OTHER_REGION: &str = "eu-west-1";

/// Well-formed `AssociationExecutionId` values (the modelled UUID pattern) that no dispatch
/// of this suite ever minted: lower case, upper case and mixed, since the pattern admits
/// each.
const UNKNOWN_EXECUTIONS: &[&str] = &[
    "00000000-0000-4000-8000-000000000000",
    "FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF",
    "aAbBcCdD-1122-4334-8455-66778899aAbB",
];

/// A well-formed `AssociationId` nothing was ever minted under.
const UNKNOWN_ASSOCIATION: &str = "11111111-2222-4333-8444-555555555555";

/// Create an association of [`AGENT`] in [`REGION`], answering the id the create minted.
async fn association(router: &MultiServiceRouter) -> String {
    let (status, created) = call(router, "CreateAssociation", &json!({ "Name": AGENT })).await;
    assert_eq!(
        status, 200,
        "the association every row reads must be created first, got {status} with {created}"
    );
    created["AssociationDescription"]["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create answers with the id it minted, got {created}"))
        .to_string()
}

/// The execution ids `DescribeAssociationExecutions` reports for `association_id`.
async fn executions(router: &MultiServiceRouter, association_id: &str) -> Vec<String> {
    let (status, response) = call(
        router,
        "DescribeAssociationExecutions",
        &json!({ "AssociationId": association_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAssociationExecutions must be served, got {status} with {response}"
    );
    response["AssociationExecutions"]
        .as_array()
        .map(|executions| {
            executions
                .iter()
                .filter_map(|execution| execution["ExecutionId"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `DescribeAssociationExecutionTargets` with `body`, signed for `region`.
async fn targets(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    call_in_region(router, region, "DescribeAssociationExecutionTargets", body).await
}

/// RULE: an unknown execution id on a known association is an empty page.
#[tokio::test]
async fn an_execution_the_association_never_had_reports_no_target() {
    let router = router();
    let association_id = association(&router).await;

    // One row per shape the read can take: the bare pair, the pair narrowed by each of the
    // filter keys this read accepts, and the pair under a page size.
    let shapes: Vec<(&str, Value)> = vec![
        ("the bare pair", json!({})),
        (
            "narrowed by a status filter",
            json!({ "Filters": [{ "Key": "Status", "Value": "Success" }] }),
        ),
        (
            "narrowed by a resource-id filter",
            json!({ "Filters": [{ "Key": "ResourceId", "Value": "i-0123456789abcdef0" }] }),
        ),
        ("under the smallest page size", json!({ "MaxResults": 1 })),
        ("under the largest page size", json!({ "MaxResults": 50 })),
    ];

    for execution_id in UNKNOWN_EXECUTIONS {
        for (shape, extra) in &shapes {
            let case = format!("{execution_id} {shape}");
            let mut body = json!({
                "AssociationId": association_id,
                "ExecutionId": execution_id,
            });
            if let Some(extra) = extra.as_object() {
                for (member, value) in extra {
                    body[member.as_str()] = value.clone();
                }
            }
            let (status, response) = targets(&router, REGION, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: an execution the association never had selects no target, got \
                 {status} with {response}"
            );
            assert_eq!(
                response["AssociationExecutionTargets"],
                json!([]),
                "case {case:?}: expected an empty target list, got {response}"
            );
            assert!(
                response["NextToken"].is_null(),
                "case {case:?}: an empty page resumes at nothing, got {response}"
            );
        }
    }
}

/// RULE: the refusal that remains is the ASSOCIATION's — an id naming no association of the
/// endpoint the read was asked through, whether the execution id names a real run of
/// another association or nothing at all.
#[tokio::test]
async fn an_association_that_resolves_to_nothing_is_still_refused() {
    let router = router();
    let association_id = association(&router).await;
    let minted = executions(&router, &association_id).await;
    let execution_id = minted
        .first()
        .cloned()
        .expect("creating an association dispatches a run");

    // (case, the Region the read is asked through, the association id it asks about)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "an association id nothing was minted under",
            REGION,
            UNKNOWN_ASSOCIATION.to_string(),
        ),
        (
            "another Region's view of a real association id",
            OTHER_REGION,
            association_id.clone(),
        ),
    ];
    for (case, region, asked_about) in rows {
        for (which, execution) in [
            ("a real execution id", execution_id.as_str()),
            ("an execution id nothing minted", UNKNOWN_EXECUTIONS[0]),
        ] {
            let (status, response) = targets(
                &router,
                region,
                &json!({ "AssociationId": asked_about, "ExecutionId": execution }),
            )
            .await;
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, "AssociationDoesNotExist"),
                "case {case:?} with {which}: expected the association refusal, got {status} \
                 with {response}"
            );
            assert_eq!(
                error_message(&response),
                format!("Association {asked_about} does not exist."),
                "case {case:?} with {which}: expected the refusal to name the association, got \
                 {response}"
            );
        }
    }

    // And the execution the dispatch DID mint is served through the Region it ran in.
    let (status, response) = targets(
        &router,
        REGION,
        &json!({ "AssociationId": association_id, "ExecutionId": execution_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the run the create dispatched is readable, got {status} with {response}"
    );
}
