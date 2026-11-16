// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE an identity-based authorization decision of this service
//! is taken against.
//!
//! RULE: CloudWatch Logs has two resource types an action of its own authorizes against, and a
//! call is decided against the one it ADDRESSES — the log GROUP's own ARN
//! (`arn:aws:logs:<region>:<account>:log-group:<group>`) for a call that names a group and no
//! stream, the log STREAM's ARN (that ARN plus `:log-stream:<stream>`) for a call that names a
//! stream, and this service's stream ARN with both segments empty for a call that addresses no
//! log group at all. That ARN is what a grant has to cover and what the refusal names, and the
//! decision is taken before anything is resolved, so a caller the documents refuse is told it
//! is not authorized whether or not the group, or the stream, exists.
//!
//! The tables span what the rule generalises over: which operation is called (group-scoped,
//! stream-scoped and addressed by no log group, reads and writes alike, including operations
//! whose model declares no access error), whether the resource the call names exists, which
//! form the caller's grant is written in (the group's own ARN, the `:*` form that covers the
//! streams under it, a prefix wildcard over a path, every resource), and whether the refusal
//! comes from an explicit `Deny` or from no grant at all.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` — and every principal is minted with IAM's and
//! STS's OWN operations over the same engine, so the documents this service reads are the ones
//! those services wrote. Every log event is put at the instant the shared harness reads off
//! the clock, which is inside the window this service ingests an event in, so a seeded event
//! really is stored and a refused write is distinguishable from it; nothing here asserts on
//! the clock.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{assumed_role, statement, Emulator, Principal};
use crate::harness::{now_ms, REGION};

/// The wire error code of an authorization refusal, as the AWS "Common Errors" table names it.
/// Stated here rather than read off the crate under test.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The error code a read of a stream the account does not hold carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The log GROUP's own ARN — the resource a call naming a group and no stream is decided
/// against. Spelled here independently of the crate under test.
fn group_resource(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}",
        engine::emulator_account_id()
    )
}

/// The log STREAM's ARN — the resource a call naming a stream is decided against.
fn stream_resource(group: &str, stream: &str) -> String {
    format!("{}:log-stream:{stream}", group_resource(group))
}

/// The whole sentence IAM refuses a call with, stated here independently of the crate under
/// test.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause of a call no readable document allows — IAM's default deny.
fn nothing_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause of a call an explicit `Deny` refuses.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The IAM action one operation of this service is performed under.
fn action_of(operation: &str) -> String {
    format!("logs:{operation}")
}

/// One identity-based policy document carrying one statement over SEVERAL actions and
/// resources — the shape a least-privilege policy for a log-group path is written in.
///
/// `statement` (shared with the suites beside this one) carries one action and one resource,
/// which cannot express a grant over a set of actions.
fn statement_over(effect: &str, actions: &[String], resources: &[String]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": actions, "Resource": resources}],
    })
    .to_string()
}

/// The wire error code an answer reports, or `None` for an answer that is no error body.
fn reported_code(body: &Value) -> Option<String> {
    let raw = body.get("__type").and_then(Value::as_str)?;
    Some(raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string())
}

/// The message an answer carries.
fn message_of(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: an error carries a message, got {body}"))
        .to_string()
}

/// Assert the call `operation` with `body`, made as `principal`, is refused as unauthorized
/// with the whole sentence IAM states for `resource` and `reason`.
async fn refused(
    aws: &Emulator,
    principal: &Principal,
    case: &str,
    operation: &str,
    body: &Value,
    resource: &str,
    reason: &str,
) {
    let (status, parsed) = aws.call_as(&principal.access_key_id, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to be refused with HTTP 400, got {status} {parsed}"
    );
    assert_eq!(
        reported_code(&parsed).as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected the error code {ACCESS_DENIED}, got {parsed}"
    );
    let expected = not_authorized(&principal.arn, &action_of(operation), resource, reason);
    let stated = message_of(case, &parsed);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
    );
}

/// Create the log group `group` with the stream `stream` under it, and put one event into it,
/// all with the account's own credential — the state the rows below read and write.
async fn seeded(aws: &Emulator, group: &str, stream: &str) {
    aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    aws.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": [{"timestamp": now_ms(), "message": "seeded"}],
        }),
    )
    .await;
}

/// The events the account's own credential reads back from one stream.
async fn stored_events(aws: &Emulator, group: &str, stream: &str) -> Vec<Value> {
    let page = aws
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("GetLogEvents reports an events list, got {page}"))
        .clone()
}

/// RULE: the refusal names the resource the call ADDRESSES — the group's own ARN for a
/// group-scoped call, the stream's ARN for a stream-scoped one, and the stream ARN with both
/// segments empty for a call that names no log group.
#[tokio::test]
async fn the_refusal_names_the_resource_the_call_addresses() {
    let aws = Emulator::new();
    let group = "/scope/addressed";
    let stream = "stream-1";
    let absent = "/scope/never-created";
    seeded(&aws, group, stream).await;
    let principal = assumed_role(
        &aws,
        "logs-scope-no-grant",
        &[statement("Allow", "sts:GetCallerIdentity", "*")],
    )
    .await;
    // (case, operation, body, the resource the refusal must name)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a group-scoped read of the events across a group's streams",
            "FilterLogEvents",
            json!({"logGroupName": group}),
            group_resource(group),
        ),
        (
            "a group-scoped read that selects streams by a list",
            "FilterLogEvents",
            json!({"logGroupIdentifier": group, "logStreamNames": [stream]}),
            group_resource(group),
        ),
        (
            "a group-scoped listing of a group's streams",
            "DescribeLogStreams",
            json!({"logGroupName": group}),
            group_resource(group),
        ),
        (
            "a group-scoped write of a group property",
            "PutRetentionPolicy",
            json!({"logGroupName": group, "retentionInDays": 7}),
            group_resource(group),
        ),
        (
            "a group-scoped create of a group the account does not hold yet",
            "CreateLogGroup",
            json!({"logGroupName": absent}),
            group_resource(absent),
        ),
        (
            "a stream-scoped read of one stream's events",
            "GetLogEvents",
            json!({"logGroupName": group, "logStreamName": stream}),
            stream_resource(group, stream),
        ),
        (
            "a stream-scoped write into one stream",
            "PutLogEvents",
            json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": [{"timestamp": now_ms(), "message": "one"}],
            }),
            stream_resource(group, stream),
        ),
        (
            "a stream-scoped create of a stream the group does not hold yet",
            "CreateLogStream",
            json!({"logGroupName": group, "logStreamName": "stream-2"}),
            stream_resource(group, "stream-2"),
        ),
        (
            "a stream-scoped read of a stream the account does not hold",
            "GetLogEvents",
            json!({"logGroupName": absent, "logStreamName": "nope"}),
            stream_resource(absent, "nope"),
        ),
        (
            "a listing that names no log group",
            "DescribeLogGroups",
            json!({"logGroupNamePrefix": "/scope/"}),
            stream_resource("", ""),
        ),
        (
            "a read of the account's queries, which names no log group",
            "DescribeQueries",
            json!({}),
            stream_resource("", ""),
        ),
    ];
    for (case, operation, body, resource) in rows {
        let reason = nothing_allows(&action_of(operation));
        refused(&aws, &principal, case, operation, &body, &resource, &reason).await;
    }
    // Nothing the refusals asked for happened: the group the create named was not minted, and
    // the stream the write named holds only the seeded event.
    let page = aws
        .ok("DescribeLogGroups", &json!({"logGroupNamePrefix": absent}))
        .await;
    let groups = page["logGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("DescribeLogGroups reports a logGroups list, got {page}"))
        .clone();
    assert!(
        groups.is_empty(),
        "case \"a refused create\": expected the account to hold no such group, got {groups:?}"
    );
    let events = stored_events(&aws, group, stream).await;
    assert_eq!(
        events.len(),
        1,
        "case \"a refused write\": expected only the seeded event, got {events:?}"
    );
}

/// RULE: the resource a call addresses is the resource a GRANT has to cover, so which ARN form
/// a statement is written in decides which calls it admits: the group's own ARN admits the
/// group-scoped call and not the stream-scoped one, the `:*` form that covers the names UNDER
/// the group admits the stream-scoped call and not the group-scoped one, and a prefix wildcard
/// over the path, like every resource, admits both.
#[tokio::test]
async fn the_grant_has_to_cover_the_resource_the_call_addresses() {
    let aws = Emulator::new();
    let group = "/scope/granted";
    let stream = "stream-1";
    seeded(&aws, group, stream).await;
    let group_scoped = (
        "FilterLogEvents",
        json!({"logGroupName": group}),
        group_resource(group),
    );
    let stream_scoped = (
        "GetLogEvents",
        json!({"logGroupName": group, "logStreamName": stream}),
        stream_resource(group, stream),
    );
    // (case, the resource the grant is scoped to, admits the group-scoped call, admits the
    // stream-scoped call)
    let rows: Vec<(&str, String, bool, bool)> = vec![
        (
            "a grant on the group's own ARN",
            group_resource(group),
            true,
            false,
        ),
        (
            "a grant on the names under the group",
            format!("{}:*", group_resource(group)),
            false,
            true,
        ),
        (
            "a grant on a prefix wildcard over the path",
            format!("{}*", group_resource("/scope/")),
            true,
            true,
        ),
        ("a grant on every resource", "*".to_string(), true, true),
        (
            "a grant on another group's path",
            format!("{}*", group_resource("/elsewhere/")),
            false,
            false,
        ),
    ];
    for (index, (case, resource, admits_group, admits_stream)) in rows.into_iter().enumerate() {
        let actions: Vec<String> = ["FilterLogEvents", "GetLogEvents"]
            .iter()
            .map(|operation| action_of(operation))
            .collect();
        let principal = assumed_role(
            &aws,
            &format!("logs-scope-grant-{index}"),
            &[statement_over("Allow", &actions, std::slice::from_ref(&resource))],
        )
        .await;
        for (scope, admitted, (operation, body, addressed)) in [
            ("the group-scoped call", admits_group, &group_scoped),
            ("the stream-scoped call", admits_stream, &stream_scoped),
        ] {
            let case = format!("{case} on {scope}");
            if admitted {
                let (status, parsed) = aws
                    .call_as(&principal.access_key_id, operation, body)
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the grant to admit {operation}, got {status} {parsed}"
                );
                continue;
            }
            let reason = nothing_allows(&action_of(operation));
            refused(
                &aws,
                &principal,
                &case,
                operation,
                body,
                addressed,
                &reason,
            )
            .await;
        }
    }
}

/// RULE: an explicit `Deny` over a log group refuses every call this service decides against
/// that group's resources — and the decision precedes the resource lookup, so a stream the
/// account does not hold inside a denied group is still an authorization refusal, while the
/// same read of an allowed group is the missing-resource error.
#[tokio::test]
async fn an_explicit_deny_over_a_group_is_decided_before_existence() {
    let aws = Emulator::new();
    let allowed = "/scope/allowed/one";
    let denied = "/scope/denied/one";
    let stream = "stream-1";
    let missing = "stream-nope";
    seeded(&aws, allowed, stream).await;
    seeded(&aws, denied, stream).await;
    let reads: Vec<String> = ["FilterLogEvents", "GetLogEvents"]
        .iter()
        .map(|operation| action_of(operation))
        .collect();
    let principal = assumed_role(
        &aws,
        "logs-scope-explicit-deny",
        &[
            statement_over(
                "Allow",
                &reads,
                &[
                    format!("{}*", group_resource("/scope/allowed/")),
                    format!("{}*", group_resource("/scope/denied/")),
                ],
            ),
            statement_over(
                "Deny",
                &reads,
                &[format!("{}*", group_resource("/scope/denied/"))],
            ),
        ],
    )
    .await;

    // (case, operation, body, the resource the refusal names)
    let denied_rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a group-scoped read of the denied group",
            "FilterLogEvents",
            json!({"logGroupName": denied}),
            group_resource(denied),
        ),
        (
            "a stream-scoped read of the denied group's stream",
            "GetLogEvents",
            json!({"logGroupName": denied, "logStreamName": stream}),
            stream_resource(denied, stream),
        ),
        (
            "a stream-scoped read of a stream the denied group does not hold",
            "GetLogEvents",
            json!({"logGroupName": denied, "logStreamName": missing}),
            stream_resource(denied, missing),
        ),
    ];
    for (case, operation, body, resource) in denied_rows {
        refused(
            &aws,
            &principal,
            case,
            operation,
            &body,
            &resource,
            EXPLICIT_DENY,
        )
        .await;
    }

    // The same reads of the ALLOWED group reach their handlers, which is the control: the
    // session is live and the grant covers both ARN forms of that path.
    for (case, operation, body) in [
        (
            "a group-scoped read of the allowed group",
            "FilterLogEvents",
            json!({"logGroupName": allowed}),
        ),
        (
            "a stream-scoped read of the allowed group's stream",
            "GetLogEvents",
            json!({"logGroupName": allowed, "logStreamName": stream}),
        ),
    ] {
        let (status, parsed) = aws
            .call_as(&principal.access_key_id, operation, &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to be served, got {status} {parsed}"
        );
    }
    // And a stream the ALLOWED group does not hold is the missing-resource error, which is
    // what the denied group's refusal masks.
    let (status, parsed) = aws
        .call_as(
            &principal.access_key_id,
            "GetLogEvents",
            &json!({"logGroupName": allowed, "logStreamName": missing}),
        )
        .await;
    assert_eq!(
        reported_code(&parsed).as_deref(),
        Some(NOT_FOUND),
        "case \"a missing stream in the allowed group\": expected {NOT_FOUND}, got {status} \
         {parsed}"
    );
}

/// RULE: an action no statement names at all is refused with IAM's default deny — on the
/// stream-scoped ARN a write addresses — and the refused write stores nothing.
#[tokio::test]
async fn a_write_no_statement_names_is_refused_and_stores_nothing() {
    let aws = Emulator::new();
    let group = "/scope/implicit/one";
    let stream = "stream-1";
    seeded(&aws, group, stream).await;
    // The reads of the path are granted; the write is named by nothing, which is the implicit
    // deny.
    let principal = assumed_role(
        &aws,
        "logs-scope-implicit-deny",
        &[statement_over(
            "Allow",
            &[action_of("GetLogEvents")],
            &[format!("{}*", group_resource("/scope/implicit/"))],
        )],
    )
    .await;
    let write = json!({
        "logGroupName": group,
        "logStreamName": stream,
        "logEvents": [{"timestamp": now_ms(), "message": "refused"}],
    });
    refused(
        &aws,
        &principal,
        "a write into a stream no statement names",
        "PutLogEvents",
        &write,
        &stream_resource(group, stream),
        &nothing_allows(&action_of("PutLogEvents")),
    )
    .await;
    let events = stored_events(&aws, group, stream).await;
    assert_eq!(
        events.len(),
        1,
        "case \"a refused write\": expected only the seeded event, got {events:?}"
    );
    // The granted read of the same stream is served, so the refusal is about the action, not
    // about the session.
    let (status, parsed) = aws
        .call_as(
            &principal.access_key_id,
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the granted read of the same stream\": expected HTTP 200, got {status} {parsed}"
    );
}
