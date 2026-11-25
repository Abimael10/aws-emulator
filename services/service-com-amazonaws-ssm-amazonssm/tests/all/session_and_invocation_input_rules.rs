// SPDX-License-Identifier: CC0-1.0

//! What this service decides about a REQUEST before it decides anything about its state: which
//! members of a Run Command and a Session Manager call narrow a report, which are held to the
//! form the service publishes for them, and which name a resource that has to be in a state.
//!
//! Four rules, each driven over a table whose rows differ along what the rule generalises over
//! rather than along the one request a recording happened to make:
//!
//! 1. `ListCommandInvocations` REPORTS: every member of its input is a criterion, so one no
//!    stored invocation meets — a `CommandId` the endpoint holds no command under among them —
//!    selects none and is answered 200 with `CommandInvocations` serialized as an empty list.
//!    Only a request the operation cannot read at all is refused.
//! 2. `StartSession` validates the FORM of its target before it reads anything about the
//!    target's connectivity, so a value outside the expression the service publishes for that
//!    parameter is `ValidationException` naming the member and the expression, and
//!    `TargetNotConnected` is left for a well-formed target nothing answers for.
//! 3. `GetConnectionStatus` is asked about "the managed node ID", so a value that is no
//!    managed-node id is refused rather than answered a connection status it cannot have.
//! 4. `TerminateSession` PERFORMS the end, so it is served only for a session this endpoint
//!    holds in a state an end still applies to, and the answer echoes the session it ended.
//!
//! Every call goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body through the multi-service
//! router — and the managed node the served rows need is one an Amazon EC2 launch registered,
//! through the helpers the sibling suites own ([`crate::session_manager_sessions`],
//! [`crate::ec2_managed_node_registration`]) rather than a second copy of them.
//!
//! Nothing here waits a fixed time or asserts on the wall clock: the registration is a stored
//! due instant, so the helpers LOOK AGAIN until it holds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{create_profile, emulator, image_id, AL2023_PARAMETER};
use crate::parameter_store::{call_in_region, error_code, error_message, REGION};
use crate::session_manager_sessions::{
    registered_node, registering_profile, session_in, start_session, OTHER_REGION,
};

/// The published command document the sends below run.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The expression the live service publishes for `StartSession`'s own `Target` parameter,
/// written out here so the suite pins the clause rather than echoing whatever the service
/// holds (`service::refusal::stated_constraints`'s session-target row).
const SESSION_TARGET_PATTERN: &str = "^[a-zA-Z0-9_:-]{1,400}$";

/// The published maximum of `SessionTarget`
/// (`smithy_query(query="shape",shape="SessionTarget")`: `length 1..400`).
const MAX_TARGET_CHARS: usize = 400;

/// How the connection report refuses a target that is no managed-node id — fixed text, with
/// nothing of the value echoed (`state::sessions::INVALID_TARGET_ID_FORMAT`).
const INVALID_TARGET_ID_FORMAT: &str = "Invalid target ID format";

/// How a session that is in no state for the change a request asks of it is refused
/// (`state::sessions::SESSION_NOT_IN_A_VALID_STATE`).
const NOT_IN_A_VALID_STATE: &str = "Session is not in a valid state";

/// The published maximum of `SessionId`
/// (`smithy_query(query="shape",shape="SessionId")`: `length 1..96`).
const MAX_SESSION_ID_CHARS: usize = 96;

/// The clause a refusal of `target` states for the published expression: the member and the
/// requirement alone, which is what this operation's recorded refusals state.
fn target_pattern_refusal() -> String {
    format!(
        "1 validation error detected: Value at 'target' failed to satisfy constraint: Member \
         must satisfy regular expression pattern: {SESSION_TARGET_PATTERN}"
    )
}

/// One `ListCommandInvocations` call at `region`'s endpoint.
async fn list(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "ListCommandInvocations", body).await
}

/// Send the shell document to `instance` and answer the command id the send minted.
async fn send(router: &MultiServiceRouter, case: &str, instance: &str) -> String {
    let (status, answered) = call_in_region(
        router,
        REGION,
        "SendCommand",
        &json!({
            "DocumentName": RUN_SHELL_SCRIPT,
            "InstanceIds": [instance],
            "Parameters": {"commands": ["echo input-rules"]},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status} with {answered}"
    );
    answered["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint a command id, got {answered}"))
        .to_string()
}

/// The node ids one listing reports an invocation for, in the order it reports them.
fn reported_nodes(answered: &Value) -> Vec<String> {
    answered["CommandInvocations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|invocation| invocation["InstanceId"].as_str().map(str::to_string))
        .collect()
}

/// One `GetConnectionStatus` call.
async fn connection_status(router: &MultiServiceRouter, target: &str) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "GetConnectionStatus",
        &json!({ "Target": target }),
    )
    .await
}

/// One `TerminateSession` call at `region`'s endpoint.
async fn terminate(router: &MultiServiceRouter, region: &str, session_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await
}

/// RULE 1: every member of an invocation listing is a CRITERION, so a request whose criteria
/// no stored invocation meets is answered 200 with an empty `CommandInvocations` list — and the
/// same listing reports the invocation the send created when the criteria do select it.
#[tokio::test]
async fn an_invocation_listing_narrows_by_every_criterion_instead_of_refusing_one() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "the node a command is sent to", &image, &registering_profile()).await;
    let command_id = send(&router, "the command whose invocation is listed", &node).await;

    // The criteria select the one invocation the send created, so the empty answers below are
    // about the criteria rather than about an empty store.
    let (status, answered) = list(&router, REGION, &json!({ "CommandId": command_id })).await;
    assert_eq!(
        status, 200,
        "case \"the command's own id\": the listing must be served, got {status} with {answered}"
    );
    assert_eq!(
        reported_nodes(&answered),
        vec![node.clone()],
        "case \"the command's own id\": expected the invocation of {node}, got {answered}"
    );

    // (case, the endpoint asked, the criteria)
    let selects_none: Vec<(&str, &str, Value)> = vec![
        (
            "a well-formed command id nothing minted",
            REGION,
            json!({ "CommandId": "00000000-0000-0000-0000-000000000000" }),
        ),
        (
            "another well-formed command id nothing minted",
            REGION,
            json!({ "CommandId": "DEADBEEF-1234-4321-abcd-0123456789ab" }),
        ),
        (
            "the command's own id at another Region's endpoint",
            OTHER_REGION,
            json!({ "CommandId": command_id.clone() }),
        ),
        (
            "a node id nothing was sent to",
            REGION,
            json!({ "InstanceId": "i-00000000000000000" }),
        ),
        (
            "a hybrid node id nothing was sent to",
            REGION,
            json!({ "InstanceId": "mi-01234567890123456" }),
        ),
        (
            "an unminted command id beside the node that does hold an invocation",
            REGION,
            json!({
                "CommandId": "00000000-0000-0000-0000-000000000000",
                "InstanceId": node.clone(),
            }),
        ),
        (
            "the command's own id beside a node nothing was sent to",
            REGION,
            json!({ "CommandId": command_id.clone(), "InstanceId": "i-0abcdef0" }),
        ),
        (
            "a status filter nothing matches",
            REGION,
            json!({
                "CommandId": command_id.clone(),
                "Filters": [{ "key": "Status", "value": "Cancelled" }],
            }),
        ),
        (
            "a document filter naming another document",
            REGION,
            json!({ "Filters": [{ "key": "DocumentName", "value": "AWS-RunPowerShellScript" }] }),
        ),
        (
            "an invoked-before filter older than anything sent",
            REGION,
            json!({ "Filters": [{ "key": "InvokedBefore", "value": "1970-01-02T00:00:00Z" }] }),
        ),
        (
            "an unminted command id with the details asked for and a page size",
            REGION,
            json!({
                "CommandId": "00000000-0000-0000-0000-000000000000",
                "Details": true,
                "MaxResults": 50,
            }),
        ),
        (
            "no criterion at all, at an endpoint nothing was sent through",
            OTHER_REGION,
            json!({}),
        ),
    ];

    for (case, region, body) in selects_none {
        let (status, answered) = list(&router, region, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must be served, got {status} with {answered}"
        );
        assert!(
            answered["CommandInvocations"].is_array(),
            "case {case:?}: expected the modelled list on the answer, got {answered}"
        );
        assert!(
            reported_nodes(&answered).is_empty(),
            "case {case:?}: expected no invocation, got {answered}"
        );
        assert!(
            answered["__type"].is_null(),
            "case {case:?}: expected no error on the answer, got {answered}"
        );
    }
}

/// RULE 2: a `StartSession` target is held to the expression the service publishes for it
/// BEFORE anything about the target's connectivity is read, so a value outside that expression
/// is a `ValidationException` about the input and a well-formed value nothing answers for is
/// `TargetNotConnected` — the two answers mean opposite things to a client.
#[tokio::test]
async fn a_session_start_validates_the_target_before_it_reads_connectivity() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "the node a session is opened to", &image, &registering_profile()).await;

    // (case, a target carrying a character the published expression admits none of)
    let malformed: Vec<(&str, String)> = vec![
        ("a host name, whose dots the expression admits none of", "node.example.invalid".to_string()),
        ("a node id with a space after it", format!("{node} ")),
        ("a slash-separated address", "ecs:cluster/task".to_string()),
        ("a tag selector, whose equals sign is outside it", "tag:Name=web".to_string()),
        ("a node id with punctuation appended", format!("{node}!")),
        ("a target of one character outside it", ".".to_string()),
    ];
    for (case, target) in &malformed {
        let (status, body) = start_session(&router, REGION, &json!({ "Target": target })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a malformed target is refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            target_pattern_refusal(),
            "case {case:?}: expected the clause naming the published expression, got {body}"
        );
    }

    // A target the expression admits, that no node of this endpoint answers for: the refusal
    // is about the NODE, and it names the target it could not reach.
    let unreachable: Vec<(&str, String)> = vec![
        ("an EC2 node id nothing registered", "i-00000000000000000".to_string()),
        ("a hybrid node id nothing registered", "mi-01234567890123456".to_string()),
        (
            "a container target, whose colons and underscores the expression admits",
            "ecs:cluster_task_container".to_string(),
        ),
        ("a value that is no node id but is well formed", "not-a-managed-node".to_string()),
        ("a well-formed target at the published maximum length", "t".repeat(MAX_TARGET_CHARS)),
    ];
    for (case, target) in &unreachable {
        let (status, body) = start_session(&router, REGION, &json!({ "Target": target })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a target nothing answers for is refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "TargetNotConnected",
            "case {case:?}: expected TargetNotConnected, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("{target} is not connected."),
            "case {case:?}: expected the refusal to name the target, got {body}"
        );
    }

    // One character past the published length: the clause names the BOUND, which is the
    // modelled constraint rather than the expression.
    let too_long = "t".repeat(MAX_TARGET_CHARS + 1);
    let (status, body) = start_session(&router, REGION, &json!({ "Target": too_long })).await;
    assert_eq!(
        status, 400,
        "case \"one character past the published maximum\": expected a refusal, got {status} \
         with {body}"
    );
    let message = error_message(&body);
    assert!(
        message.contains("'target'") && message.contains("length less than or equal to 400"),
        "case \"one character past the published maximum\": expected the clause naming the \
         bound, got {message:?}"
    );

    // And the node this endpoint does hold is still served, so none of the above is a refusal
    // of every target.
    let (status, started) = start_session(&router, REGION, &json!({ "Target": node })).await;
    assert_eq!(
        status, 200,
        "case \"the registered node\": the start must be served, got {status} with {started}"
    );
    assert!(
        started["SessionId"].as_str().is_some_and(|id| !id.is_empty()),
        "case \"the registered node\": expected the session's id, got {started}"
    );
}

/// RULE 3: the connection report is asked about "the managed node ID", so it answers a status
/// for every managed-node id form and refuses every value that is none.
#[tokio::test]
async fn a_connection_report_answers_only_about_a_managed_node_id() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "the node reported on", &image, &registering_profile()).await;

    // (case, a value that IS a managed-node id, whether this endpoint holds it as a node)
    let node_ids: Vec<(&str, String, bool)> = vec![
        ("the registered node", node.clone(), true),
        (
            "an EC2 id of the 17-character width nothing registered",
            "i-00000000000000000".to_string(),
            false,
        ),
        (
            "an EC2 id of the 8-character width nothing registered",
            "i-0abcdef0".to_string(),
            false,
        ),
        (
            "a hybrid id of its one width nothing registered",
            "mi-01234567890123456".to_string(),
            false,
        ),
        (
            "an id whose word characters include an underscore",
            "i-0abc_def".to_string(),
            false,
        ),
    ];
    for (case, target, held) in &node_ids {
        let (status, body) = connection_status(&router, target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a node id is reported on, got {status} with {body}"
        );
        assert_eq!(
            body["Target"].as_str(),
            Some(target.as_str()),
            "case {case:?}: expected the report to echo the target, got {body}"
        );
        let expected = if *held { "connected" } else { "notconnected" };
        assert_eq!(
            body["Status"].as_str(),
            Some(expected),
            "case {case:?}: expected {expected}, got {body}"
        );
    }

    // (case, a value that is no managed-node id at all)
    let not_node_ids: Vec<(&str, String)> = vec![
        ("a host name", "node.example.invalid".to_string()),
        ("an EC2 id one character short of either width", "i-0abcdef".to_string()),
        ("an EC2 id one character past the shorter width", "i-0abcdef01".to_string()),
        ("a hybrid id one character short", "mi-0123456789012345".to_string()),
        ("an id with no managed-node prefix", "0abcdef0123456789".to_string()),
        ("a word the expression admits nothing of", "not-a-managed-node".to_string()),
        ("a target at the published maximum length", "t".repeat(MAX_TARGET_CHARS)),
        ("a single character", "t".to_string()),
    ];
    for (case, target) in &not_node_ids {
        let (status, body) = connection_status(&router, target).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a target that is no node id is refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            INVALID_TARGET_ID_FORMAT,
            "case {case:?}: expected {INVALID_TARGET_ID_FORMAT:?}, got {body}"
        );
        assert!(
            body["Status"].is_null(),
            "case {case:?}: a refused report answers no status, got {body}"
        );
    }
}

/// RULE 4: an end is PERFORMED, so it is served only for a session this endpoint holds in a
/// state an end applies to — and the served answer echoes the session it ended.
#[tokio::test]
async fn an_end_is_served_only_for_a_session_this_endpoint_can_still_end() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "the node the sessions connect to", &image, &registering_profile()).await;

    let mut started: Vec<String> = Vec::new();
    for case in ["the session that is ended", "the session ended twice"] {
        let (status, body) = start_session(&router, REGION, &json!({ "Target": node })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session must be started first, got {status} with {body}"
        );
        started.push(
            body["SessionId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a start reports a SessionId, got {body}"))
                .to_string(),
        );
    }
    let live = started[0].clone();
    let ended_twice = started[1].clone();

    // The end of a live session: served, echoing the session it ended, which the history then
    // reports.
    for (case, session_id) in [
        ("the session that is ended", &live),
        ("the session ended twice", &ended_twice),
    ] {
        let (status, body) = terminate(&router, REGION, session_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first end must be served, got {status} with {body}"
        );
        assert_eq!(
            body["SessionId"].as_str(),
            Some(session_id.as_str()),
            "case {case:?}: the end reports the session it ended, got {body}"
        );
        assert!(
            session_in(&router, REGION, "History", session_id)
                .await
                .is_some(),
            "case {case:?}: an ended session is reported in the history"
        );
    }

    // (case, the endpoint asked, the session id it names)
    let refused: Vec<(&str, &str, String)> = vec![
        (
            "a session id this endpoint holds none under",
            REGION,
            "never-started-here".to_string(),
        ),
        (
            "a session of this endpoint, asked of another Region's",
            OTHER_REGION,
            ended_twice.clone(),
        ),
        (
            "a session that has already ended",
            REGION,
            ended_twice.clone(),
        ),
        (
            "the same session a third time",
            REGION,
            ended_twice.clone(),
        ),
        (
            "an id at the published maximum length",
            REGION,
            "s".repeat(MAX_SESSION_ID_CHARS),
        ),
        ("an id of one character", REGION, "s".to_string()),
    ];
    for (case, region, session_id) in &refused {
        let (status, body) = terminate(&router, region, session_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the end must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_IN_A_VALID_STATE,
            "case {case:?}: expected {NOT_IN_A_VALID_STATE:?}, got {body}"
        );
        assert!(
            body["SessionId"].is_null(),
            "case {case:?}: a refused end names no session, got {body}"
        );
    }

    // A refused end changed nothing: the one history record each ended session has is still
    // the one its own end wrote, and nothing is active.
    for session_id in [&live, &ended_twice] {
        assert!(
            session_in(&router, REGION, "Active", session_id)
                .await
                .is_none(),
            "case \"after the refusals\": an ended session is no longer active"
        );
        let ended = session_in(&router, REGION, "History", session_id)
            .await
            .unwrap_or_else(|| panic!("case \"after the refusals\": {session_id} is in history"));
        assert_eq!(
            ended["Status"],
            json!("Terminated"),
            "case \"after the refusals\": expected a terminated session, got {ended}"
        );
    }
}
