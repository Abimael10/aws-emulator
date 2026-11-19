// SPDX-License-Identifier: CC0-1.0

//! How a write of a State Manager association words `AssociationDoesNotExist`.
//!
//! One rule spans the operations that resolve an association: a refusal NAMES the
//! association the request asked about — "Association <AssociationId> does not exist.",
//! fixed text around the id the request carried — wherever the request carries an id to
//! name, and answers the error's own documented sentence, which names none, only where it
//! does not: a call addressed by the document and the node a create paired. A caller tearing
//! down several associations can then tell WHICH one is gone.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The suites are tables: the rows differ along the id form asked about (the case of its
//! hexadecimal digits, an id of another Region's association, the id of an association that
//! was deleted), the way the request addresses the association and which write is called —
//! rather than the single id a recorded unit happened to delete.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router};

/// A command document Systems Manager publishes whose every parameter carries a default, so
/// a create that supplies none satisfies it (`state::PUBLISHED_DOCUMENTS`).
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A node id of the form the modelled `InstanceId` pattern admits.
const NODE: &str = "i-0123456789abcdef0";

/// A second Region, to pin that an association is reached through the endpoint it was
/// created at.
const OTHER_REGION: &str = "us-east-1";

/// How the refusal is worded where the request carries the id it could not resolve.
fn does_not_exist(association_id: &str) -> String {
    format!("Association {association_id} does not exist.")
}

/// The sentence the refusal carries where the request named no id at all:
/// `AssociationDoesNotExist`'s own documented sentence.
const DOES_NOT_EXIST_WITHOUT_AN_ID: &str = "The specified association doesn't exist.";

/// Create an association of [`AGENT`] on `instance_id` at `region`, answering its id.
async fn create_association(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    instance_id: &str,
) -> String {
    let (status, response) = call_in_region(
        router,
        region,
        "CreateAssociation",
        &json!({ "Name": AGENT, "InstanceId": instance_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association must be created, got {status} {response}"
    );
    response["AssociationDescription"]["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId, got {response}"))
        .to_string()
}

/// Assert that `body` is refused `AssociationDoesNotExist` carrying `expected`.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    expected: &str,
) {
    let (status, response) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must be refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "AssociationDoesNotExist",
        "case {case:?}: {operation} must answer the modelled refusal, got {response}"
    );
    let message = error_message(&response);
    assert_eq!(
        message, expected,
        "case {case:?}: {operation} expected {expected:?}, got {message:?}"
    );
}

/// RULE: a write addressed by an `AssociationId` that resolves to nothing names that id in
/// its refusal.
#[tokio::test]
async fn a_write_addressed_by_an_id_names_it() {
    let router = router();
    // An association of another Region exists, so a row can ask about an id that names one
    // somewhere else — which is still an id this endpoint resolves nothing for.
    let elsewhere = create_association(&router, "another Region's association", OTHER_REGION, NODE)
        .await;

    // (case, the id asked about)
    let rows: [(&str, &str); 5] = [
        (
            "an id nothing was minted under",
            "00000000-0000-4000-8000-000000000000",
        ),
        (
            "another id nothing was minted under",
            "ffffffff-ffff-4fff-bfff-ffffffffffff",
        ),
        (
            "an id in upper case, which the modelled pattern admits",
            "AABBCCDD-1122-4334-8455-66778899AABB",
        ),
        (
            "an id in mixed case",
            "aAbBcCdD-1122-4334-8455-66778899aAbB",
        ),
        ("an association of another Region", elsewhere.as_str()),
    ];

    for (case, association_id) in rows {
        let expected = does_not_exist(association_id);
        for (operation, body) in [
            (
                "DeleteAssociation",
                json!({ "AssociationId": association_id }),
            ),
            (
                "UpdateAssociation",
                json!({ "AssociationId": association_id }),
            ),
            (
                "StartAssociationsOnce",
                json!({ "AssociationIds": [association_id] }),
            ),
            (
                "DescribeAssociation",
                json!({ "AssociationId": association_id }),
            ),
        ] {
            refused(&router, case, operation, &body, &expected).await;
        }
    }
}

/// The same rule on the id of an association that DID exist: once the delete has removed it,
/// a second delete of the very same id is refused naming that id — so the sentence tracks
/// what the store holds rather than what the request looks like.
#[tokio::test]
async fn a_second_delete_of_one_association_names_its_id() {
    let router = router();
    let case = "an association deleted twice";
    let association_id = create_association(&router, case, "us-west-2", NODE).await;

    let (status, deleted) = call(
        &router,
        "DeleteAssociation",
        &json!({ "AssociationId": association_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first delete must succeed, got {status} {deleted}"
    );

    refused(
        &router,
        case,
        "DeleteAssociation",
        &json!({ "AssociationId": association_id }),
        &does_not_exist(&association_id),
    )
    .await;
}

/// A write addressed by the document and the node a create paired carries no id to name, so
/// it answers the error's own documented sentence. The rows differ along which half of the
/// pair resolves to nothing.
#[tokio::test]
async fn a_write_addressed_by_a_pairing_names_no_id() {
    let router = router();
    let case = "the pairing form";
    let association_id = create_association(&router, case, "us-west-2", NODE).await;
    assert!(
        !association_id.is_empty(),
        "case {case:?}: the create reports an id"
    );

    // (case, the document named, the node named)
    let rows: [(&str, &str, &str); 3] = [
        ("a document nothing paired with the node", "AWS-RunShellScript", NODE),
        ("a node nothing paired with the document", AGENT, "i-0abc1234"),
        (
            "neither half pairing anything",
            "AWS-RunShellScript",
            "i-0abc1234",
        ),
    ];

    for (row, name, instance_id) in rows {
        refused(
            &router,
            row,
            "DeleteAssociation",
            &json!({ "Name": name, "InstanceId": instance_id }),
            DOES_NOT_EXIST_WITHOUT_AN_ID,
        )
        .await;
    }
}
