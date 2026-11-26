// SPDX-License-Identifier: CC0-1.0

//! How `DescribeAssociation` words `AssociationDoesNotExist` for the LEGACY lookup form —
//! the document name and the managed-node id a create paired.
//!
//! One rule spans every such lookup that resolves nothing: the refusal names the PAIR it was
//! asked about and states what an association created through `Targets` must be looked up by
//! instead — "Association not found for documentName <Name> and instanceId <InstanceId>. Try
//! using the association-id parameter if you are looking for an association created using
//! targets." The two variable parts are the `Name` and the `InstanceId` the request carried,
//! copied verbatim; the rest of the sentence is fixed. That is a DISTINCT sentence from the
//! one the id form states ("Association <AssociationId> does not exist."), so a caller can
//! tell WHICH lookup failed.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.DescribeAssociation` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The suites are tables whose rows differ along what the rule generalises over: which half
//! of the pair resolves to nothing, the form of the document named (one the account created,
//! a published command document, a published automation document, a name no document has),
//! the form of the node id (`i-` with eight or seventeen characters, `mi-` with seventeen,
//! from the modelled `InstanceId` pattern `^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`), and the
//! route the pair has no association by (never associated, or associated and then deleted).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// A command document Systems Manager publishes whose every parameter carries a default, so
/// a create that supplies none satisfies it.
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A second published command document, to vary the document half of the pair.
const RUN_SHELL: &str = "AWS-RunShellScript";

/// A published automation document, so the rows span document types too.
const AUTOMATION: &str = "AWS-EnableS3BucketEncryption";

/// A node id of the `i-` + seventeen characters form the modelled `InstanceId` pattern
/// admits.
const NODE: &str = "i-0123456789abcdef0";

/// The sentence the legacy lookup form states when the pair names no association. Its fixed
/// text is the requirement; `document_name` and `instance_id` are the request's own.
fn not_found_for_pair(document_name: &str, instance_id: &str) -> String {
    format!(
        "Association not found for documentName {document_name} and instanceId \
         {instance_id}. Try using the association-id parameter if you are looking for an \
         association created using targets."
    )
}

/// The sentence the id lookup form states, which the pair form must NOT be confused with.
fn does_not_exist(association_id: &str) -> String {
    format!("Association {association_id} does not exist.")
}

/// Create an association of `name` on `instance_id`, answering its id.
async fn create_association(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> String {
    let (status, response) = call(
        router,
        "CreateAssociation",
        &json!({ "Name": name, "InstanceId": instance_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association must be created, got {status} {response}"
    );
    response["AssociationDescription"]["AssociationId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a create reports an AssociationId, got {response}")
        })
        .to_string()
}

/// Create a command document of the account's own named `name`.
async fn create_document(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, response) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Command",
            "Content": r#"{"schemaVersion":"2.2","description":"no-op command document","mainSteps":[{"action":"aws:runShellScript","name":"noop","inputs":{"runCommand":["echo hi"]}}]}"#,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the document must be created, got {status} {response}"
    );
}

/// Assert that `DescribeAssociation` refuses `body` with `AssociationDoesNotExist` carrying
/// `expected`.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    body: &serde_json::Value,
    expected: &str,
) {
    let (status, response) = call(router, "DescribeAssociation", body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: DescribeAssociation must be refused, got {status} {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "AssociationDoesNotExist",
        "case {case:?}: expected AssociationDoesNotExist, got {code:?} in {response}"
    );
    let message = error_message(&response);
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}

/// RULE: a lookup by the document name and the node id that resolves no association is
/// refused naming that pair, whichever half of it resolves to nothing and whatever form the
/// two halves take.
#[tokio::test]
async fn a_pair_that_names_no_association_is_refused_naming_the_pair() {
    let router = router();
    // A document of the account's own, whose name carries the punctuation the modelled
    // document-name pattern admits, so the rows span created and published documents alike.
    let own = "Echo.Message-1_noop";
    create_document(&router, "the account's own document", own).await;
    // One association DOES exist, on a published document and another node, so each row below
    // is a pair the store resolves nothing for while the store is not empty.
    let existing = create_association(&router, "a pair that exists", AGENT, NODE).await;
    assert!(
        !existing.is_empty(),
        "the control association must report an id"
    );

    // (case, the document named, the node named)
    let rows: [(&str, &str, &str); 8] = [
        (
            "the account's own document, never associated with the node",
            own,
            "i-00112233445566778",
        ),
        (
            "a published command document, never associated with the node",
            RUN_SHELL,
            NODE,
        ),
        (
            "a published automation document, never associated with the node",
            AUTOMATION,
            NODE,
        ),
        (
            "a name no document of the Region has",
            "Nothing-Has-This-Name",
            NODE,
        ),
        (
            "the associated document, on a node nothing paired it with",
            AGENT,
            "i-0fedcba987654321f",
        ),
        (
            "a short node id, the eight-character i- form",
            AGENT,
            "i-0abc1234",
        ),
        (
            "a hybrid node id, the seventeen-character mi- form",
            AGENT,
            "mi-0123456789abcdef0",
        ),
        (
            "neither half pairing anything",
            own,
            "mi-fedcba9876543210f",
        ),
    ];

    for (case, name, instance_id) in rows {
        refused(
            &router,
            case,
            &json!({ "Name": name, "InstanceId": instance_id }),
            &not_found_for_pair(name, instance_id),
        )
        .await;
    }
}

/// The same rule on the route where the pair DID have an association: the lookup is served
/// while the association is there and refused naming the pair once the delete has removed it,
/// so the sentence tracks what the store holds rather than what the request looks like.
#[tokio::test]
async fn a_pair_whose_association_was_deleted_is_refused_naming_the_pair() {
    let router = router();
    // A document of the account's own, so the rows span created and published documents.
    let own = "Echo.Message-2_noop";
    create_document(&router, "the account's own document", own).await;

    // (case, the document, the node)
    let rows: [(&str, &str, &str); 3] = [
        ("a published command document", AGENT, NODE),
        ("the account's own document", own, "i-0abc1234"),
        (
            "a hybrid node",
            AGENT,
            "mi-0123456789abcdef0",
        ),
    ];

    for (case, name, instance_id) in rows {
        let association_id = create_association(&router, case, name, instance_id).await;

        // While it is held, the very same pair resolves it: the refusal below is the store's
        // answer, not this lookup form's.
        let (status, served) = call(
            &router,
            "DescribeAssociation",
            &json!({ "Name": name, "InstanceId": instance_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the pair must resolve its association, got {status} {served}"
        );
        let reported = served["AssociationDescription"]["AssociationId"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            reported, association_id,
            "case {case:?}: expected the created association {association_id:?}, got \
             {reported:?} in {served}"
        );

        let (status, deleted) = call(
            &router,
            "DeleteAssociation",
            &json!({ "AssociationId": association_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must succeed, got {status} {deleted}"
        );

        refused(
            &router,
            case,
            &json!({ "Name": name, "InstanceId": instance_id }),
            &not_found_for_pair(name, instance_id),
        )
        .await;
    }
}

/// The two lookup forms state DIFFERENT sentences, so a caller can tell which one failed:
/// the id form names the id it could not resolve, the pair form names the pair. An id named
/// beside a pair is the lookup, so that request states the id form's sentence.
#[tokio::test]
async fn each_lookup_form_states_its_own_sentence() {
    let router = router();
    let unknown_id = "00000000-0000-4000-8000-000000000000";

    refused(
        &router,
        "the id form alone",
        &json!({ "AssociationId": unknown_id }),
        &does_not_exist(unknown_id),
    )
    .await;

    refused(
        &router,
        "an id named beside a pair",
        &json!({ "AssociationId": unknown_id, "Name": AGENT, "InstanceId": NODE }),
        &does_not_exist(unknown_id),
    )
    .await;

    refused(
        &router,
        "the pair form alone",
        &json!({ "Name": AGENT, "InstanceId": NODE }),
        &not_found_for_pair(AGENT, NODE),
    )
    .await;
}
