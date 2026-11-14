// SPDX-License-Identifier: CC0-1.0

//! What the Studio/notebook plane says when it rejects a value outside a modelled enum,
//! end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends.
//!
//! THE RULE: the EMR Studio and EMR Notebooks operations are fronted by their own
//! backend, and EVERY refusal that backend raises is its own sentence for what it
//! rejected followed by the parenthesised service detail naming the backend, the status,
//! the error code and that response's own request id. So a value outside a modelled enum
//! of one of those operations' own input shapes is refused `InvalidRequestException`
//! (HTTP 400) with the MEMBER's sentence inside that wrapper — the rejected execution
//! status for a notebook-execution listing, the member named as the model spells it for
//! every other enum member — and never with the generic constraint-validation diagnostic,
//! which states a member path and enumerates the values the enum declares.
//!
//! The rows span what the rule generalises over: every Studio/notebook operation carrying
//! an enum-valued member of its own input shape, every such member (the two the recorded
//! instances cover and the three they do not), and several forms of rejected value —
//! including a value the enum declares written in another case, which is outside the set.
//! The controls are the other half: a value the enum DOES declare reaches the handler, and
//! the cluster plane keeps wording its own enum refusals.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, at, Emr};
use crate::refusal_messages::{message, studio_request, wrapped_request_id};

/// The modelled client-input error every operation of this plane declares
/// (`smithy_query(query="operation", operation="ListNotebookExecutions")` and the other
/// rows below list `InvalidRequestException`, HTTP 400).
const REFUSED_CODE: &str = "InvalidRequestException";

/// Every value `com.amazonaws.emr#NotebookExecutionStatus` declares
/// (`smithy_query(query="shape", shape="NotebookExecutionStatus")`), which the refusals
/// below must NOT disclose and whose members must be accepted.
const NOTEBOOK_EXECUTION_STATUSES: [&str; 10] = [
    "START_PENDING",
    "STARTING",
    "RUNNING",
    "FINISHING",
    "FINISHED",
    "FAILING",
    "FAILED",
    "STOP_PENDING",
    "STOPPING",
    "STOPPED",
];

/// Every value `com.amazonaws.emr#IdentityType` declares
/// (`smithy_query(query="shape", shape="IdentityType")`).
const IDENTITY_TYPES: [&str; 2] = ["USER", "GROUP"];

/// The session-policy ARN the session-mapping writes below carry, which they require.
const SESSION_POLICY_ARN: &str = "arn:aws:iam::aws:policy/EMRStudioBasic";

/// One row: the operation, the body it is called with, and the sentence the backend must
/// state in front of its service detail.
struct Case {
    case: String,
    operation: &'static str,
    body: Value,
    sentence: String,
}

fn case(case: &str, operation: &'static str, body: Value, sentence: &str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        sentence: sentence.to_string(),
    }
}

/// A session-mapping body naming `studio_id` and the identity type `identity_type`.
fn mapping(studio_id: &str, identity_type: &str) -> Value {
    json!({
        "StudioId": studio_id,
        "IdentityName": "analyst",
        "IdentityType": identity_type,
    })
}

/// The same, plus the `SessionPolicyArn` a create and an update require.
fn mapping_with_policy(studio_id: &str, identity_type: &str) -> Value {
    let mut body = mapping(studio_id, identity_type);
    body["SessionPolicyArn"] = json!(SESSION_POLICY_ARN);
    body
}

/// The smallest `StartNotebookExecution` body that satisfies the operation's required
/// members, with `OutputNotebookFormat` set to `format`.
fn start_notebook(format: &str) -> Value {
    json!({
        "NotebookExecutionName": "format-row",
        "RelativePath": "notebooks/demo.ipynb",
        "ExecutionEngine": { "Id": "j-ABCDEFGHIJKLM", "Type": "EMR" },
        "ServiceRole": "EMR_Notebooks_DefaultRole",
        "OutputNotebookFormat": format,
    })
}

/// Every row, against a Studio the service HOLDS — so what is refused is the enum value
/// and not the Studio the request names.
fn refused_cases(studio_id: &str) -> Vec<Case> {
    let mut cases = vec![
        // The notebook-execution status filter: the one member of this plane whose
        // sentence states the rejected value.
        case(
            "a Status outside NotebookExecutionStatus",
            "ListNotebookExecutions",
            json!({ "Status": "NOT_A_REAL_STATUS" }),
            "Invalid Execution Status: NOT_A_REAL_STATUS",
        ),
        case(
            "a declared NotebookExecutionStatus in lower case",
            "ListNotebookExecutions",
            json!({ "Status": "finished" }),
            "Invalid Execution Status: finished",
        ),
        case(
            "a Status of another service's vocabulary",
            "ListNotebookExecutions",
            json!({ "Status": "SUCCEEDED" }),
            "Invalid Execution Status: SUCCEEDED",
        ),
        case(
            "a Status beside a filter the listing admits",
            "ListNotebookExecutions",
            json!({ "Status": "NOT_A_REAL_STATUS", "EditorId": "e-ABCDEFGHIJKLM" }),
            "Invalid Execution Status: NOT_A_REAL_STATUS",
        ),
        // The identity type, on every operation of the session-mapping surface: the
        // sentence names the member and nothing of the request.
        case(
            "an IdentityType outside IdentityType, filtering mappings",
            "ListStudioSessionMappings",
            json!({ "StudioId": studio_id, "IdentityType": "ADMIN" }),
            "IdentityType is not valid.",
        ),
        case(
            "another IdentityType outside IdentityType, filtering mappings",
            "ListStudioSessionMappings",
            json!({ "StudioId": studio_id, "IdentityType": "SUPERUSER" }),
            "IdentityType is not valid.",
        ),
        case(
            "a declared IdentityType in lower case, filtering mappings",
            "ListStudioSessionMappings",
            json!({ "StudioId": studio_id, "IdentityType": "user" }),
            "IdentityType is not valid.",
        ),
        case(
            "an IdentityType outside IdentityType, reading one mapping",
            "GetStudioSessionMapping",
            mapping(studio_id, "SUPERUSER"),
            "IdentityType is not valid.",
        ),
        case(
            "an IdentityType outside IdentityType, creating a mapping",
            "CreateStudioSessionMapping",
            mapping_with_policy(studio_id, "ADMIN"),
            "IdentityType is not valid.",
        ),
        case(
            "an IdentityType outside IdentityType, updating a mapping",
            "UpdateStudioSessionMapping",
            mapping_with_policy(studio_id, "ADMIN"),
            "IdentityType is not valid.",
        ),
        case(
            "an IdentityType outside IdentityType, deleting a mapping",
            "DeleteStudioSessionMapping",
            mapping(studio_id, "ADMIN"),
            "IdentityType is not valid.",
        ),
        // The members no recording covers: the same template, built from the member.
        case(
            "an AuthMode outside AuthMode",
            "CreateStudio",
            {
                let mut body = studio_request("auth-mode-row");
                body["AuthMode"] = json!("LDAP");
                body
            },
            "AuthMode is not valid.",
        ),
        case(
            "an IdcUserAssignment outside IdcUserAssignment",
            "CreateStudio",
            {
                let mut body = studio_request("idc-assignment-row");
                body["IdcUserAssignment"] = json!("MANDATORY");
                body
            },
            "IdcUserAssignment is not valid.",
        ),
        case(
            "an OutputNotebookFormat outside OutputNotebookFormat",
            "StartNotebookExecution",
            start_notebook("PDF"),
            "OutputNotebookFormat is not valid.",
        ),
    ];
    // Every declared status written in lower case is outside the set, and each is stated
    // with the value the request sent.
    for status in NOTEBOOK_EXECUTION_STATUSES {
        let sent = status.to_ascii_lowercase();
        cases.push(case(
            &format!("the declared status {status} lower-cased"),
            "ListNotebookExecutions",
            json!({ "Status": &sent }),
            &format!("Invalid Execution Status: {sent}"),
        ));
    }
    cases
}

/// THE RULE: every rejected enum value of this plane is answered 400
/// `InvalidRequestException` with the member's sentence inside the backend's service
/// detail — its own request id each time — and with nothing of the generic constraint
/// diagnostic: no member path and no list of the values the enum declares.
#[tokio::test]
async fn a_rejected_enum_value_is_worded_by_the_backend_fronting_this_plane() {
    let emr = Emr::new();
    let studio_id = emr.ok("CreateStudio", studio_request("live")).await["StudioId"]
        .as_str()
        .expect("a StudioId")
        .to_string();

    let mut seen_request_ids: Vec<String> = Vec::new();
    for row in refused_cases(&studio_id) {
        let label = &row.case;
        let (status, payload) = emr.call(row.operation, row.body.clone()).await;
        assert_refused(label, status, &payload, REFUSED_CODE);
        let said = message(label, &payload);

        let request_id = wrapped_request_id(label, &said);
        assert!(
            !seen_request_ids.contains(&request_id),
            "case {label:?}: each refusal carries its own request id, got the \
             already-reported {request_id:?}"
        );
        seen_request_ids.push(request_id.clone());

        let expected = format!(
            "{} (Service: AWSEditors; Status Code: 400; Error Code: \
             InvalidRequestException; Request ID: {request_id}; Proxy: null)",
            row.sentence
        );
        assert_eq!(
            said, expected,
            "case {label:?}: expected {expected:?}, got {said:?}"
        );
        for declared in NOTEBOOK_EXECUTION_STATUSES.iter().chain(IDENTITY_TYPES.iter()) {
            // A declared value the request itself sent (lower-cased) is not a disclosure
            // of the set, so only the set's own spelling is forbidden.
            assert!(
                !said.contains(declared),
                "case {label:?}: the refusal must not disclose the value set, got \
                 {declared:?} in {said:?}"
            );
        }
        assert!(
            !said.contains("validation error detected"),
            "case {label:?}: the generic constraint diagnostic must not reach the wire, \
             got {said:?}"
        );
    }
}

/// THE RULE's other half: a value the enum DECLARES is not refused at the boundary at
/// all — it reaches the handler, which answers for itself. The Studio these rows name is
/// one the service holds no Studio for, so the handler's own refusal (a different
/// sentence under the same wrapper) is the evidence the value got past the boundary,
/// while the filters that resolve no Studio answer a listing.
#[tokio::test]
async fn a_declared_enum_value_reaches_the_handler_on_this_plane() {
    let emr = Emr::new();
    let unknown_studio = "es-ZZZZZZZZZZZZZ";

    // Every declared notebook-execution status is a filter the listing serves.
    for status in NOTEBOOK_EXECUTION_STATUSES {
        let label = format!("the declared status {status}");
        let page = emr
            .ok("ListNotebookExecutions", json!({ "Status": status }))
            .await;
        assert!(
            at(&page, &["NotebookExecutions"])
                .as_array()
                .is_none_or(|rows| rows.is_empty()),
            "case {label:?}: a declared status must be served, got {page}"
        );
    }

    // Every declared identity type is a filter the mapping listing serves.
    for identity_type in IDENTITY_TYPES {
        let label = format!("the declared identity type {identity_type}");
        let page = emr
            .ok(
                "ListStudioSessionMappings",
                json!({ "IdentityType": identity_type }),
            )
            .await;
        assert!(
            at(&page, &["SessionMappings"])
                .as_array()
                .is_none_or(|rows| rows.is_empty()),
            "case {label:?}: a declared identity type must be served, got {page}"
        );
    }

    // And on the operations that resolve a Studio, the declared value reaches the
    // handler, whose refusal is the absent STUDIO rather than the identity type.
    for identity_type in IDENTITY_TYPES {
        for (operation, body) in [
            (
                "GetStudioSessionMapping",
                mapping(unknown_studio, identity_type),
            ),
            (
                "CreateStudioSessionMapping",
                mapping_with_policy(unknown_studio, identity_type),
            ),
            (
                "DeleteStudioSessionMapping",
                mapping(unknown_studio, identity_type),
            ),
        ] {
            let label = format!("{operation} with the declared identity type {identity_type}");
            let (status, payload) = emr.call(operation, body).await;
            assert_refused(&label, status, &payload, REFUSED_CODE);
            let said = message(&label, &payload);
            let request_id = wrapped_request_id(&label, &said);
            assert_eq!(
                said,
                format!(
                    "Studio does not exist. (Service: AWSEditors; Status Code: 400; \
                     Error Code: InvalidRequestException; Request ID: {request_id}; \
                     Proxy: null)"
                ),
                "case {label:?}: a declared value must reach the handler, got {said:?}"
            );
        }
    }

    // A Studio created with a declared `AuthMode` and `IdcUserAssignment` is stored and
    // reads back, so the boundary refuses neither.
    let mut body = studio_request("declared-enums");
    body["IdcUserAssignment"] = json!("OPTIONAL");
    let created = emr.ok("CreateStudio", body).await;
    let studio_id = created["StudioId"].as_str().expect("a StudioId").to_string();
    let read = emr
        .ok("DescribeStudio", json!({ "StudioId": &studio_id }))
        .await;
    assert_eq!(
        at(&read, &["Studio", "IdcUserAssignment"]).as_str(),
        Some("OPTIONAL"),
        "the declared assignment must be stored and read back, got {read}"
    );
}

/// THE RULE's boundary: the cluster plane is NOT fronted by that backend, so its own
/// enum refusals keep their own wording — the rejected values of one filter member, with
/// no service detail wrapped around them.
#[tokio::test]
async fn the_cluster_plane_keeps_its_own_enum_wording() {
    let emr = Emr::new();
    for (label, operation, body, expected) in [
        (
            "a ClusterStates entry outside ClusterState",
            "ListClusters",
            json!({ "ClusterStates": ["NOPE"] }),
            "Cluster states [NOPE] are not valid.",
        ),
        (
            "two ClusterStates entries outside ClusterState",
            "ListClusters",
            json!({ "ClusterStates": ["NOPE", "running"] }),
            "Cluster states [NOPE, running] are not valid.",
        ),
    ] {
        let (status, payload) = emr.call(operation, body).await;
        assert_refused(label, status, &payload, REFUSED_CODE);
        let said = message(label, &payload);
        assert_eq!(
            said, expected,
            "case {label:?}: expected {expected:?}, got {said:?}"
        );
        assert!(
            !said.contains("AWSEditors"),
            "case {label:?}: the cluster plane carries no Studio service detail, got \
             {said:?}"
        );
    }
}
