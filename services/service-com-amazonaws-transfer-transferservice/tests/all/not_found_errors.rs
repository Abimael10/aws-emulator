// SPDX-License-Identifier: CC0-1.0

//! The not-found error every operation addressed at a missing server or user answers.
//!
//! RULE: an operation that names a Transfer resource the store does not hold answers
//! HTTP 400 with `__type` `ResourceNotFoundException`, `ResourceType` the resource name
//! capitalized, `Resource` the identifier the request named, and a fixed generic
//! `Message` that carries no identifier at all.
//!
//! The rows differ along the operation, the resource type, WHY the resource is missing
//! (never created, or deleted), and the shape of the identifier — a renamed probe is
//! not a different input, so each row changes one of those instead.

use serde_json::{json, Value};
use service_com_amazonaws_transfer_transferservice::state as st;

use crate::harness::{is_modelled_server_id, member_of, Answer, Emulator, ACCOUNT, REGION, ROLE};

/// Syntactically valid `ServerId`s (`len 19..19`, `^s-([0-9a-f]{17})$`, per the
/// `ServerId` shape) that no `CreateServer` ever minted: the low end of the id space,
/// a mid value, and the high end.
const NEVER_MINTED: [&str; 3] = [
    "s-00000000000000000",
    "s-0000000000000dead",
    "s-fffffffffffffffff",
];

/// Names satisfying `UserName`'s modelled `len 3..100` and pattern
/// `^[\w][\w@.-]{2,99}$`: the shortest legal name, each piece of punctuation the
/// pattern allows, and a name at the modelled length bound.
const ABSENT_USERS: [&str; 4] = [
    "bob",
    "no.such-user",
    "user@example",
    "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjj",
];

/// What the answer to one not-found call must carry.
struct Expect {
    /// `ResourceType`, as this service names the resource.
    resource_type: &'static str,
    /// `Resource`: the identifier the request named.
    resource: String,
    /// Every identifier the request carried. None of them may reach `Message`.
    identifiers: Vec<String>,
}

impl Expect {
    /// A missing server addressed by `server_id`.
    fn server(server_id: &str) -> Self {
        Self {
            resource_type: "Server",
            resource: server_id.to_string(),
            identifiers: vec![server_id.to_string()],
        }
    }

    /// A missing user: the name is the resource, and NEITHER it nor the server it was
    /// looked up on may appear in the message.
    fn user(server_id: &str, user_name: &str) -> Self {
        Self {
            resource_type: "User",
            resource: user_name.to_string(),
            identifiers: vec![user_name.to_string(), server_id.to_string()],
        }
    }
}

/// Drive one call that must not find its resource, assert the whole rule on the
/// answer, and hand back the `Message` so a caller can compare it across rows.
async fn assert_not_found(
    emulator: &Emulator,
    case: &str,
    op: &str,
    request: Value,
    expect: &Expect,
) -> String {
    let answer = emulator.call(op, request).await;

    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {}",
        answer.body
    );

    let resource_type = member_of(&answer.body, "ResourceType")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        resource_type, expect.resource_type,
        "case {case:?}: expected ResourceType {:?}, got {resource_type:?}",
        expect.resource_type
    );

    let resource = member_of(&answer.body, "Resource")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        resource, expect.resource,
        "case {case:?}: expected the requested identifier in Resource ({:?}), got {resource:?}",
        expect.resource
    );

    // `ResourceNotFoundException` models Message, Resource and ResourceType as
    // required, so all three must be on the wire.
    let message = member_of(&answer.body, "Message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !message.is_empty(),
        "case {case:?}: Message is modelled required, got body {}",
        answer.body
    );
    for identifier in &expect.identifiers {
        assert!(
            !message.contains(identifier.as_str()),
            "case {case:?}: expected no identifier in Message, got {message:?} carrying {identifier:?}"
        );
    }

    message
}

/// RULE (never created): every operation that looks a server or user up answers the
/// 400 `ResourceNotFoundException` with the capitalized `ResourceType`, the identifier
/// in `Resource` only, and one fixed `Message` per resource type.
#[tokio::test]
async fn operations_on_resources_that_were_never_created_answer_the_generic_400_not_found() {
    let emulator = Emulator::new();
    let server_id = emulator.create_server(json!({})).await;
    emulator.create_user(&server_id, "alice").await;

    for absent in NEVER_MINTED {
        assert!(
            is_modelled_server_id(absent),
            "the probe id {absent:?} must satisfy ServerId's modelled pattern"
        );
    }

    let mut server_messages: Vec<(String, String)> = Vec::new();
    let mut user_messages: Vec<(String, String)> = Vec::new();

    // Every operation that resolves a ServerId, against each unminted id.
    for absent in NEVER_MINTED {
        let by_server_id: Vec<(&str, Value)> = vec![
            ("DescribeServer", json!({"ServerId": absent})),
            ("DeleteServer", json!({"ServerId": absent})),
            ("ListUsers", json!({"ServerId": absent})),
            ("ListHostKeys", json!({"ServerId": absent})),
            // A malformed `NextToken` does not change WHICH refusal the caller gets:
            // the named server is resolved first, by both listings of that server.
            (
                "ListUsers",
                json!({"ServerId": absent, "NextToken": "not-a-token"}),
            ),
            (
                "ListHostKeys",
                json!({"ServerId": absent, "NextToken": "not-a-token"}),
            ),
            (
                "DescribeUser",
                json!({"ServerId": absent, "UserName": "alice"}),
            ),
            (
                "DeleteUser",
                json!({"ServerId": absent, "UserName": "alice"}),
            ),
            (
                "CreateUser",
                json!({"ServerId": absent, "UserName": "alice", "Role": ROLE}),
            ),
        ];
        for (op, request) in by_server_id {
            let case = format!("{op} on the unminted server {absent}");
            let message =
                assert_not_found(&emulator, &case, op, request, &Expect::server(absent)).await;
            server_messages.push((case, message));
        }
    }

    // Every operation that resolves a UserName on a server that DOES exist.
    for absent_user in ABSENT_USERS {
        let by_user_name: Vec<(&str, Value)> = vec![
            (
                "DescribeUser",
                json!({"ServerId": &server_id, "UserName": absent_user}),
            ),
            (
                "DeleteUser",
                json!({"ServerId": &server_id, "UserName": absent_user}),
            ),
        ];
        for (op, request) in by_user_name {
            let case = format!("{op} for the absent user {absent_user}");
            let message = assert_not_found(
                &emulator,
                &case,
                op,
                request,
                &Expect::user(&server_id, absent_user),
            )
            .await;
            user_messages.push((case, message));
        }
    }

    // The message is FIXED per resource type: it is built from the resource type alone,
    // so every row of a type answers the same string.
    assert_same_message("Server", &server_messages);
    assert_same_message("User", &user_messages);
}

/// RULE (deleted): a resource that existed and was deleted is missing in exactly the
/// same way as one that never existed — the teardown path a client retries.
#[tokio::test]
async fn operations_on_deleted_resources_answer_the_same_generic_400_not_found() {
    let emulator = Emulator::new();

    // Rows differ in how much the server held when it went away: nothing, one user,
    // several users.
    let cases: [(&str, &[&str]); 3] = [
        ("a server with no users", &[]),
        ("a server with one user", &["alice"]),
        ("a server with three users", &["alice", "bob", "carol"]),
    ];

    let mut server_messages: Vec<(String, String)> = Vec::new();
    let mut user_messages: Vec<(String, String)> = Vec::new();

    for (name, users) in cases {
        let server_id = emulator.create_server(json!({})).await;
        for user in users {
            emulator.create_user(&server_id, user).await;
        }

        let deleted = emulator
            .call("DeleteServer", json!({"ServerId": &server_id}))
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {name:?}: expected DeleteServer 200, got {} with body {}",
            deleted.status, deleted.body
        );

        for (op, request) in [
            ("DescribeServer", json!({"ServerId": &server_id})),
            ("ListUsers", json!({"ServerId": &server_id})),
            ("ListHostKeys", json!({"ServerId": &server_id})),
            // A second delete: the idempotent-teardown path.
            ("DeleteServer", json!({"ServerId": &server_id})),
        ] {
            let case = format!("{op} after deleting {name}");
            let message =
                assert_not_found(&emulator, &case, op, request, &Expect::server(&server_id)).await;
            server_messages.push((case, message));
        }

        // The users went with the server, so the server lookup is what fails first.
        for user in users {
            let case = format!("DescribeUser for {user} after deleting {name}");
            let message = assert_not_found(
                &emulator,
                &case,
                "DescribeUser",
                json!({"ServerId": &server_id, "UserName": user}),
                &Expect::server(&server_id),
            )
            .await;
            server_messages.push((case, message));
        }

        // A user deleted from a server that still exists: the USER is what is missing.
        let live_server = emulator.create_server(json!({})).await;
        emulator.create_user(&live_server, "dave").await;
        let dropped = emulator
            .call(
                "DeleteUser",
                json!({"ServerId": &live_server, "UserName": "dave"}),
            )
            .await;
        assert_eq!(
            dropped.status, 200,
            "case {name:?}: expected DeleteUser 200, got {} with body {}",
            dropped.status, dropped.body
        );
        for op in ["DescribeUser", "DeleteUser"] {
            let case = format!("{op} after deleting the user dave ({name})");
            let message = assert_not_found(
                &emulator,
                &case,
                op,
                json!({"ServerId": &live_server, "UserName": "dave"}),
                &Expect::user(&live_server, "dave"),
            )
            .await;
            user_messages.push((case, message));
        }
    }

    assert_same_message("Server", &server_messages);
    assert_same_message("User", &user_messages);
}

/// RULE: the HTTP status a Transfer refusal of a reference it cannot resolve goes out
/// under belongs to the SERVICE, not to the operation, the plane or the resource kind —
/// every operation answers `ResourceNotFoundException` at the one status this service
/// answers an unresolvable reference with (`state::RESOURCE_NOT_FOUND_HTTP_STATUS`),
/// with the body the model's own.
///
/// The rows differ along the operation, the resource kind, the plane the call belongs to
/// (a reader, the tag plane, a workflow callback) and WHY the reference resolves to
/// nothing — never minted, or, for a callback token, already consumed.
#[tokio::test]
async fn every_unresolvable_reference_is_refused_at_the_services_one_not_found_status() {
    let emulator = Emulator::new();
    let server_id = emulator.create_server(json!({})).await;
    emulator.create_user(&server_id, "alice").await;
    let workflow_id = create_callback_workflow(&emulator).await;

    // One run of that workflow, with one pending custom step, whose token is then
    // CONSUMED: the replay of it is the second reference the callback cannot resolve.
    let live_execution = "a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d";
    let live_token = "live_token";
    seed_execution(&emulator, &workflow_id, live_execution, live_token);
    let reported = callback(&emulator, &workflow_id, live_execution, live_token, "SUCCESS").await;
    assert_eq!(
        (reported.status, &reported.body),
        (200, &json!({})),
        "the first callback of a pending step must be accepted, got {} with body {}",
        reported.status,
        reported.body
    );

    // `ExecutionId`s satisfying the modelled `len 36..36` UUID pattern that this
    // workflow is not running: the low end of the id space and an upper-case one.
    let absent_executions = [
        "00000000-0000-0000-0000-000000000000",
        "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
    ];
    let transfer_arn =
        |resource: &str| format!("arn:aws:transfer:{REGION}:{ACCOUNT}:{resource}");

    // (case, operation, request, ResourceType, Resource)
    let mut rows: Vec<(String, &str, Value, &str, String)> = vec![
        (
            "a reader of a server that was never minted".to_string(),
            "DescribeServer",
            json!({"ServerId": NEVER_MINTED[0]}),
            "Server",
            NEVER_MINTED[0].to_string(),
        ),
        (
            "a reader of a user the stored server does not hold".to_string(),
            "DescribeUser",
            json!({"ServerId": &server_id, "UserName": ABSENT_USERS[0]}),
            "User",
            ABSENT_USERS[0].to_string(),
        ),
        (
            "a reader of a workflow that was never minted".to_string(),
            "DescribeWorkflow",
            json!({"WorkflowId": "w-00000000000000000"}),
            "Workflow",
            "w-00000000000000000".to_string(),
        ),
        (
            "a reader of a connector that was never minted".to_string(),
            "DescribeConnector",
            json!({"ConnectorId": "c-00000000000000000"}),
            "Connector",
            "c-00000000000000000".to_string(),
        ),
        (
            "a reader of an agreement the stored server does not hold".to_string(),
            "DescribeAgreement",
            json!({"ServerId": &server_id, "AgreementId": "a-00000000000000000"}),
            "Agreement",
            "a-00000000000000000".to_string(),
        ),
        (
            "the tag plane, for a server ARN that was never minted".to_string(),
            "TagResource",
            json!({
                "Arn": transfer_arn(&format!("server/{}", NEVER_MINTED[1])),
                "Tags": [{"Key": "env", "Value": "prod"}],
            }),
            "Server",
            NEVER_MINTED[1].to_string(),
        ),
        (
            "the callback, for a token whose step has already reported".to_string(),
            "SendWorkflowStepState",
            json!({
                "WorkflowId": &workflow_id,
                "ExecutionId": live_execution,
                "Token": live_token,
                "Status": "FAILURE",
            }),
            "Execution",
            live_execution.to_string(),
        ),
    ];

    // The run a reader and the callback are both asked about, for each unrun id: the
    // same unresolvable reference reached through two different operations.
    for absent in absent_executions {
        rows.push((
            format!("a reader of the execution {absent}, which is not running"),
            "DescribeExecution",
            json!({"WorkflowId": &workflow_id, "ExecutionId": absent}),
            "Execution",
            absent.to_string(),
        ));
        rows.push((
            format!("the callback, for the execution {absent}, which is not running"),
            "SendWorkflowStepState",
            json!({
                "WorkflowId": &workflow_id,
                "ExecutionId": absent,
                "Token": "deadbeef",
                "Status": "SUCCESS",
            }),
            "Execution",
            absent.to_string(),
        ));
    }

    let mut statuses: Vec<(String, u16)> = Vec::new();
    for (case, op, request, resource_type, resource) in rows {
        let answer = emulator.call(op, request).await;
        assert_eq!(
            answer.status,
            st::RESOURCE_NOT_FOUND_HTTP_STATUS,
            "case {case:?}: expected HTTP {}, got {} with body {}",
            st::RESOURCE_NOT_FOUND_HTTP_STATUS,
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {}",
            answer.body
        );
        assert_eq!(
            member_of(&answer.body, "ResourceType").as_str(),
            Some(resource_type),
            "case {case:?}: expected ResourceType {resource_type:?}, got {}",
            answer.body
        );
        assert_eq!(
            member_of(&answer.body, "Resource").as_str(),
            Some(resource.as_str()),
            "case {case:?}: expected Resource {resource:?}, got {}",
            answer.body
        );
        statuses.push((case, answer.status));
    }

    // ONE status for the whole service: no row answers a different one.
    let (first_case, first) = statuses[0].clone();
    for (case, status) in &statuses {
        assert_eq!(
            *status, first,
            "case {case:?}: expected the same status as case {first_case:?} ({first}), \
             got {status}"
        );
    }

    // The callback's refusal carries this service's fixed text for an execution
    // reference, with no variable part — the body the override leaves untouched.
    let refused = callback(
        &emulator,
        &workflow_id,
        absent_executions[0],
        "deadbeef",
        "SUCCESS",
    )
    .await;
    assert_eq!(
        member_of(&refused.body, "Message").as_str(),
        Some(st::NOT_FOUND_EXECUTION.message),
        "expected the fixed execution not-found message, got {}",
        refused.body
    );
}

/// Create a workflow whose one step is the `CUSTOM` step `SendWorkflowStepState` is the
/// callback of, over the wire, and return its `WorkflowId`.
///
/// `CustomStepDetails.Target` satisfies `CustomStepTarget`'s modelled `len 0..170`,
/// pattern `^arn:[a-z-]+:lambda:.*$`.
async fn create_callback_workflow(emulator: &Emulator) -> String {
    let answer = emulator
        .call(
            "CreateWorkflow",
            json!({
                "Steps": [{
                    "Type": "CUSTOM",
                    "CustomStepDetails": {
                        "Name": "callback-step",
                        "Target": format!(
                            "arn:aws:lambda:{REGION}:{ACCOUNT}:function:workflow-callback"
                        ),
                        "TimeoutSeconds": 120,
                    },
                }],
            }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "CreateWorkflow: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    answer
        .get("WorkflowId")
        .as_str()
        .unwrap_or_else(|| panic!("CreateWorkflow returned no WorkflowId: {}", answer.body))
        .to_string()
}

/// Seed one run of `workflow_id` with a single pending custom step addressed by `token`:
/// no emulated data plane uploads a file here, so nothing else starts an execution.
fn seed_execution(emulator: &Emulator, workflow_id: &str, execution_id: &str, token: &str) {
    let state = emulator.store();
    let mut guard = state.lock();
    guard.workflow_executions.insert(
        (workflow_id.to_string(), execution_id.to_string()),
        st::WorkflowExecution {
            workflow_id: workflow_id.to_string(),
            execution_id: execution_id.to_string(),
            step_states: [(token.to_string(), None)].into_iter().collect(),
            ..st::WorkflowExecution::default()
        },
    );
}

/// One `SendWorkflowStepState` callback, over the wire.
async fn callback(
    emulator: &Emulator,
    workflow_id: &str,
    execution_id: &str,
    token: &str,
    status: &str,
) -> Answer {
    emulator
        .call(
            "SendWorkflowStepState",
            json!({
                "WorkflowId": workflow_id,
                "ExecutionId": execution_id,
                "Token": token,
                "Status": status,
            }),
        )
        .await
}

/// Every row of one resource type must answer the SAME `Message`: it is derived from
/// the resource type, never from the request.
fn assert_same_message(resource_type: &str, messages: &[(String, String)]) {
    let Some((first_case, first)) = messages.first() else {
        panic!("resource type {resource_type}: no case produced a message");
    };
    for (case, message) in messages {
        assert_eq!(
            message, first,
            "case {case:?}: expected the same fixed {resource_type} message as case \
             {first_case:?} ({first:?}), got {message:?}"
        );
    }
}
