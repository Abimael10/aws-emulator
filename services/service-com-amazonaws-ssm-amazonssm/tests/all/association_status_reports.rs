// SPDX-License-Identifier: CC0-1.0

//! The status a managed node REPORTS for a State Manager association, end to end:
//! `UpdateAssociationStatus` persisting what the SSM Agent reports and every read of that
//! association reporting it back.
//!
//! RULE under test: the status a node reports for the association it was paired with through
//! the deprecated `InstanceId` member is persisted on that association exactly as the node
//! sent it, and from then on it is what the association's own `Status`, its progress
//! (`Overview`) and its per-node status report read, until that node reports another status.
//! A report of the status already reported changes nothing and is refused `StatusUnchanged`;
//! a document/node pair the endpoint holds no association for is refused
//! `AssociationDoesNotExist`, and both refusals carry the error code alone.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol") — over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention with the create, listing and update suites.
//!
//! The suites are tables. The rows differ along what the rule generalises over: which status
//! of the modelled `AssociationStatusName` enum is reported, whether the report carries a
//! user-defined string, which instant it says the status changed at, which half of the
//! document/node pair resolves nothing, how the association was targeted and which Region the
//! report is signed for — rather than the single report a recorded unit happened to make.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::ops::types::ASSOCIATION_STATUS_NAME_VALUES;
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, call_in_region, created, described, error_code, service, AGENT, NODE, OTHER_NODE,
    OTHER_REGION, REGION, SHELL,
};

/// Register `id` in `region` the way a node's own registration would, so the node reporting
/// below is a managed node of the account.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, id: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.ping_status = Some("Online".to_string());
    record.tags = BTreeMap::new();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// Report `status` for the association of `name` on `instance_id`, signed for `region`.
async fn report_in_region(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    instance_id: &str,
    status: &Value,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "UpdateAssociationStatus",
        &json!({
            "Name": name,
            "InstanceId": instance_id,
            "AssociationStatus": status,
        }),
    )
    .await
}

/// The same report signed for [`REGION`].
async fn report(
    router: &MultiServiceRouter,
    name: &str,
    instance_id: &str,
    status: &Value,
) -> (u16, Value) {
    report_in_region(router, REGION, name, instance_id, status).await
}

/// The refusal message of a failure body, read from either spelling, or `None` where the
/// response carries none at all.
fn refusal_message(body: &Value) -> Option<&str> {
    body["message"].as_str().or_else(|| body["Message"].as_str())
}

/// Assert that `response` is the refusal `code` carrying the error code ALONE: the modelled
/// `StatusUnchanged` declares no message member, and the `AssociationDoesNotExist` a report
/// addressed by a document and a node is answered with carries none either.
fn assert_refused(case: &str, status: u16, response: &Value, code: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {response}"
    );
    assert_eq!(
        error_code(response),
        code,
        "case {case:?}: expected {code}, got {response}"
    );
    let message = refusal_message(response);
    assert!(
        message.is_none_or(str::is_empty),
        "case {case:?}: expected the error code alone, got message {message:?} in {response}"
    );
}

/// Assert that `description` reports `reported` as the association's status, and that its
/// progress is that status over the one node that reported it.
fn assert_reports(case: &str, description: &Value, reported: &Value) {
    let name = reported["Name"].as_str().unwrap_or_default();
    let status = description["Status"].clone();
    assert_eq!(
        status["Name"].as_str(),
        Some(name),
        "case {case:?}: expected the reported status {name:?}, got {description}"
    );
    assert_eq!(
        status["Message"], reported["Message"],
        "case {case:?}: expected the reason the node gave, got {description}"
    );
    assert_eq!(
        status["AdditionalInfo"], reported["AdditionalInfo"],
        "case {case:?}: expected the user-defined string the node gave, got {description}"
    );
    assert_eq!(
        status["Date"].as_f64(),
        reported["Date"].as_f64(),
        "case {case:?}: expected the instant the node said the status changed at, got \
         {description}"
    );
    assert_eq!(
        description["Overview"]["Status"].as_str(),
        Some(name),
        "case {case:?}: a reported status is the association's progress, got {description}"
    );
    assert_eq!(
        description["Overview"]["DetailedStatus"].as_str(),
        Some(name),
        "case {case:?}: a reported status is all the detail there is, got {description}"
    );
    assert_eq!(
        description["Overview"]["AssociationStatusAggregatedCount"],
        json!({ name: 1 }),
        "case {case:?}: expected the one node that reported counted under {name:?}, got \
         {description}"
    );
}

/// The status entry `DescribeInstanceAssociationsStatus` reports for `association_id` on
/// `instance_id`.
async fn status_on_node(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    association_id: &str,
) -> Value {
    let (status, listed) = call(
        router,
        "DescribeInstanceAssociationsStatus",
        &json!({ "InstanceId": instance_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the node's association status must be served, got {status} with \
         {listed}"
    );
    listed["InstanceAssociationStatusInfos"]
        .as_array()
        .and_then(|infos| {
            infos
                .iter()
                .find(|info| info["AssociationId"].as_str() == Some(association_id))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// RULE: the status a node reports is persisted as sent and is what the association's own
/// read, a read addressed by the pair, and the node's own status report all answer with.
///
/// The rows walk one association through the whole modelled `AssociationStatusName` enum
/// (`smithy_query(query="shape", shape="AssociationStatusName")` — Pending, Success, Failed)
/// and back, so each row is a transition from the status the row before it reported.
#[tokio::test]
async fn a_reported_status_is_what_every_read_of_the_association_reports() {
    let (router, engine) = service();
    register(&engine, REGION, NODE);

    let case = "the legacy pairing";
    let description = created(
        &router,
        case,
        &json!({ "Name": AGENT, "InstanceId": NODE }),
    )
    .await;
    assert!(
        description["Status"].is_null(),
        "case {case:?}: a create reports no status, because no node has reported one: \
         {description}"
    );
    let association_id = description["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId"))
        .to_string();

    // (case, the status the node reports)
    let rows: &[(&str, Value)] = &[
        (
            "the first report, pending with a reason",
            json!({
                "Name": "Pending",
                "Message": "Waiting for the next poll",
                "Date": 1_700_000_000,
            }),
        ),
        (
            "a transition to success carrying a user-defined string",
            json!({
                "Name": "Success",
                "Message": "The document was applied",
                "AdditionalInfo": "agent 3.2.1377.0",
                "Date": 1_700_000_060,
            }),
        ),
        (
            "a transition to failure",
            json!({
                "Name": "Failed",
                "Message": "aws:runShellScript failed with exit code 1",
                "Date": 1_700_000_120,
            }),
        ),
        (
            "a transition back to success",
            json!({
                "Name": "Success",
                "Message": "Applied on the following run",
                "Date": 1_700_000_180,
            }),
        ),
    ];
    // Every status the modelled enum admits is reported by some row, so the table spans the
    // catalogue rather than the one status a recorded unit happened to report.
    for admitted in ASSOCIATION_STATUS_NAME_VALUES {
        assert!(
            rows.iter()
                .any(|(_, status)| status["Name"].as_str() == Some(*admitted)),
            "the table must report {admitted:?}, which the AssociationStatusName enum admits"
        );
    }

    for (row, reported) in rows {
        let (status, response) = report(&router, AGENT, NODE, reported).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200, got {status} with {response}"
        );
        assert_reports(row, &response["AssociationDescription"], reported);

        // The same report read back through the association's own id, and through the
        // document and node the create paired: one store, so neither read can disagree.
        let by_id = described(&router, row, &json!({ "AssociationId": association_id })).await;
        assert_reports(row, &by_id, reported);
        let by_pair = described(&router, row, &json!({ "Name": AGENT, "InstanceId": NODE })).await;
        assert_reports(row, &by_pair, reported);

        // And the status the node itself is reported to be in for this association.
        let on_node = status_on_node(&router, row, NODE, &association_id).await;
        assert_eq!(
            on_node["Status"].as_str(),
            reported["Name"].as_str(),
            "case {row:?}: expected the node's own report, got {on_node}"
        );
        assert_eq!(
            on_node["DetailedStatus"].as_str(),
            reported["Name"].as_str(),
            "case {row:?}: expected the detail of the node's own report, got {on_node}"
        );
    }
}

/// RULE: a report of the status the node has already reported is refused `StatusUnchanged`
/// and leaves the report that is held in place, whatever reason or user-defined string the
/// second report carries.
#[tokio::test]
async fn reporting_the_status_already_reported_changes_nothing() {
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    let created = created(
        &router,
        "a repeated report",
        &json!({ "Name": AGENT, "InstanceId": NODE }),
    )
    .await;
    let association_id = created["AssociationId"]
        .as_str()
        .expect("a create reports an AssociationId")
        .to_string();

    let held = json!({
        "Name": "Success",
        "Message": "The document was applied",
        "AdditionalInfo": "the first report",
        "Date": 1_700_000_000,
    });
    let (status, response) = report(&router, AGENT, NODE, &held).await;
    assert_eq!(
        status, 200,
        "the first report must land, got {status} with {response}"
    );

    // (case, the status reported again)
    let rows: &[(&str, Value)] = &[
        (
            "the identical report",
            json!({
                "Name": "Success",
                "Message": "The document was applied",
                "AdditionalInfo": "the first report",
                "Date": 1_700_000_000,
            }),
        ),
        (
            "the same status with another reason",
            json!({
                "Name": "Success",
                "Message": "Applied again",
                "Date": 1_700_009_999,
            }),
        ),
        (
            "the same status with no user-defined string",
            json!({ "Name": "Success", "Message": "Applied", "Date": 1_700_000_001 }),
        ),
    ];

    for (row, again) in rows {
        let (status, response) = report(&router, AGENT, NODE, again).await;
        assert_refused(row, status, &response, "StatusUnchanged");
        // The held report is untouched: a refused report persists nothing.
        let read = described(&router, row, &json!({ "AssociationId": association_id })).await;
        assert_reports(row, &read, &held);
    }

    // A status the node has NOT reported is a change, so it lands even after the refusals.
    let moved = json!({ "Name": "Failed", "Message": "Exit code 1", "Date": 1_700_010_000 });
    let (status, response) = report(&router, AGENT, NODE, &moved).await;
    assert_eq!(
        status, 200,
        "a different status is a change, got {status} with {response}"
    );
    assert_reports(
        "a status the node has not reported",
        &response["AssociationDescription"],
        &moved,
    );
}

/// RULE: a document and node pair this endpoint holds no association for is refused
/// `AssociationDoesNotExist` and nothing is persisted for it.
///
/// The rows differ along which half of the pair resolves nothing, and cover an association
/// that exists but was created for TARGETS rather than for a node — the node ids targets
/// select are criteria resolved per run, not the pairing a report is addressed by.
#[tokio::test]
async fn a_pair_no_association_pairs_is_refused() {
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    register(&engine, REGION, OTHER_NODE);
    // One association of the pairing form, so a row that names only one wrong half is a miss
    // rather than an empty store, and one created for targets.
    let paired = created(
        &router,
        "the association that does pair",
        &json!({ "Name": AGENT, "InstanceId": NODE }),
    )
    .await;
    let association_id = paired["AssociationId"]
        .as_str()
        .expect("a create reports an AssociationId")
        .to_string();
    created(
        &router,
        "an association created for targets",
        &json!({
            "Name": SHELL,
            "Targets": [{ "Key": "InstanceIds", "Values": [OTHER_NODE] }],
            "Parameters": { "commands": ["echo one"] },
        }),
    )
    .await;

    // (case, the document named, the node named)
    let rows: &[(&str, &str, &str)] = &[
        ("a document nothing paired with the node", SHELL, NODE),
        ("a node nothing paired with the document", AGENT, OTHER_NODE),
        ("neither half pairing anything", SHELL, "i-0dead999"),
        (
            "a node a well-formed id names no node of",
            AGENT,
            "mi-01234567890123456",
        ),
    ];

    let reported = json!({ "Name": "Success", "Message": "Applied", "Date": 1_700_000_000 });
    for (row, name, instance_id) in rows {
        let (status, response) = report(&router, name, instance_id, &reported).await;
        assert_refused(row, status, &response, "AssociationDoesNotExist");
    }

    // The association that DOES pair is untouched by every refusal above.
    let read = described(
        &router,
        "the association that does pair",
        &json!({ "AssociationId": association_id }),
    )
    .await;
    assert!(
        read["Status"].is_null(),
        "a refused report persists nothing: {read}"
    );
}

/// RULE: an association is reported on through the endpoint it was created at — the Region
/// comes from the request's own credential scope — so the same pair is a miss through any
/// other Region's endpoint.
#[tokio::test]
async fn a_report_reaches_only_the_region_the_association_was_created_in() {
    let (router, engine) = service();
    register(&engine, OTHER_REGION, NODE);
    let case = "an association of another Region";
    let (status, response) = call_in_region(
        &router,
        OTHER_REGION,
        "CreateAssociation",
        &json!({ "Name": AGENT, "InstanceId": NODE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association must be created, got {status} with {response}"
    );

    let reported = json!({ "Name": "Success", "Message": "Applied", "Date": 1_700_000_000 });
    let (status, refused) = report(&router, AGENT, NODE, &reported).await;
    assert_refused(case, status, &refused, "AssociationDoesNotExist");

    let (status, served) = report_in_region(&router, OTHER_REGION, AGENT, NODE, &reported).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the Region it was created in serves the report, got {status} with \
         {served}"
    );
    assert_reports(case, &served["AssociationDescription"], &reported);
}
