// SPDX-License-Identifier: CC0-1.0

//! What `CreateAssociationBatch` does about the DOCUMENT its entries name.
//!
//! Two rules, both about the request as a whole rather than about one entry:
//!
//! 1. An entry naming a document this endpoint holds none of refuses the WHOLE call with
//!    `InvalidDocument` ("the specified SSM document doesn't exist",
//!    `smithy_query(query="operation", operation="CreateAssociationBatch")`), and no entry
//!    of the batch is created — so a caller reading a 200 never has to clean up half a
//!    batch.
//! 2. A name the expression a document name must match does not admit is refused with State
//!    Manager's own name validation — `ValidationException` carrying
//!    "Invalid document name: <the name the entry sent>" — rather than with the shape-level
//!    constraint clause the schema would otherwise report.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and every refusal is
//! checked against what `ListAssociations` reports, so a refusal that created an
//! association anyway cannot pass.
//!
//! The rows differ along what the rules generalise over: WHERE in the batch the offending
//! entry sits, how many entries travel with it, which way the name is unusable (a name
//! nothing holds, an ARN of another account or Region, a name too short, too long, or
//! carrying a character the expression forbids) — rather than the single batch a recorded
//! unit happened to send.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// A command document Systems Manager publishes whose every parameter carries a default, so
/// an entry naming it is an entry this service can create (`state::PUBLISHED_DOCUMENTS`).
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A node id of the form the modelled `InstanceId` pattern admits.
const NODE: &str = "i-0123456789abcdef0";

/// A second one, so two sound entries of one batch name two nodes.
const OTHER_NODE: &str = "i-0fedcba9876543210";

/// How the association plane states a document name it cannot resolve: fixed text followed
/// by the name the request carried.
fn document_does_not_exist(name: &str) -> String {
    format!("Document does not exist: {name}")
}

/// How State Manager states a name the document-name expression does not admit: fixed text
/// followed by the name the entry sent.
fn invalid_document_name(name: &str) -> String {
    format!("Invalid document name: {name}")
}

/// How many associations this Region holds, read through `ListAssociations`.
async fn held(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, response) = call(router, "ListAssociations", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListAssociations must be served, got {status} with {response}"
    );
    response["Associations"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// One sound entry, naming `node`.
fn sound(node: &str) -> Value {
    json!({ "Name": AGENT, "InstanceId": node })
}

/// RULE 1: an entry naming a document this endpoint holds none of refuses the whole call,
/// and creates nothing.
#[tokio::test]
async fn an_entry_naming_no_document_refuses_the_whole_batch() {
    // (case, the entries the batch carries, the name the refusal states)
    let rows: Vec<(&str, Vec<Value>, String)> = vec![
        (
            "the only entry names no document",
            vec![json!({ "Name": "no-such-document" })],
            "no-such-document".to_string(),
        ),
        (
            "the FIRST of two entries names no document",
            vec![json!({ "Name": "absent-one" }), sound(NODE)],
            "absent-one".to_string(),
        ),
        (
            "the LAST of three entries names no document",
            vec![
                sound(NODE),
                sound(OTHER_NODE),
                json!({ "Name": "absent-last" }),
            ],
            "absent-last".to_string(),
        ),
        (
            "an ARN naming a document of another account",
            vec![
                sound(NODE),
                json!({ "Name": format!("arn:aws:ssm:{REGION}:111122223333:document/{AGENT}") }),
            ],
            format!("arn:aws:ssm:{REGION}:111122223333:document/{AGENT}"),
        ),
        (
            "an ARN naming a document of another Region",
            vec![json!({
                "Name": format!("arn:aws:ssm:eu-west-1:{ACCOUNT}:document/{AGENT}"),
            })],
            format!("arn:aws:ssm:eu-west-1:{ACCOUNT}:document/{AGENT}"),
        ),
    ];

    for (case, entries, named) in rows {
        let router = router();
        let (status, response) = call(
            &router,
            "CreateAssociationBatch",
            &json!({ "Entries": entries }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidDocument"),
            "case {case:?}: expected the whole call refused, got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            document_does_not_exist(&named),
            "case {case:?}: expected the refusal to name the document, got {response}"
        );
        assert!(
            response["Successful"].is_null() && response["Failed"].is_null(),
            "case {case:?}: a refused batch reports no entry at all, got {response}"
        );
        assert_eq!(
            held(&router, case).await,
            0,
            "case {case:?}: a refused batch creates nothing"
        );
    }
}

/// RULE 1, the other side: a batch whose every entry names a document this endpoint holds
/// is served, and an entry that is unsound for any OTHER reason is still reported per entry.
#[tokio::test]
async fn a_batch_whose_documents_all_resolve_is_served() {
    let router = router();
    let case = "two sound entries beside one unsound for another reason";
    let (status, response) = call(
        &router,
        "CreateAssociationBatch",
        &json!({
            "Entries": [
                sound(NODE),
                sound(OTHER_NODE),
                // The document resolves; the SCHEDULE is in neither form an expression
                // takes, which is a property of the entry and is reported as one.
                json!({ "Name": AGENT, "ScheduleExpression": "every 5 minutes" }),
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a batch whose documents all resolve is served, got {status} with \
         {response}"
    );
    assert_eq!(
        response["Successful"].as_array().map(Vec::len),
        Some(2),
        "case {case:?}: the two sound entries are created, got {response}"
    );
    assert_eq!(
        response["Failed"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: the unsound entry is reported per entry, got {response}"
    );
    assert_eq!(
        held(&router, case).await,
        2,
        "case {case:?}: exactly the created entries are held"
    );
}

/// RULE 2: a name the document-name expression does not admit is refused in State Manager's
/// own words, naming the name the entry sent.
#[tokio::test]
async fn a_name_outside_the_document_name_expression_is_refused_by_name() {
    // (case, the name the offending entry carries)
    let rows: Vec<(&str, String)> = vec![
        ("a name one character short of the floor", "ab".to_string()),
        ("a one-character name", "a".to_string()),
        (
            "a name one character past the ceiling",
            "d".repeat(129),
        ),
        (
            "a name carrying a character the expression forbids",
            "my document".to_string(),
        ),
        ("an empty name", String::new()),
    ];

    for (case, name) in rows {
        let router = router();
        // The offending entry travels beside a sound one, so the refusal is the request's
        // rather than the batch's only entry being bad.
        let (status, response) = call(
            &router,
            "CreateAssociationBatch",
            &json!({ "Entries": [sound(NODE), json!({ "Name": name })] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the name refusal, got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            invalid_document_name(&name),
            "case {case:?}: expected State Manager's own name validation, got {response}"
        );
        assert_eq!(
            held(&router, case).await,
            0,
            "case {case:?}: a refused batch creates nothing"
        );
    }
}
