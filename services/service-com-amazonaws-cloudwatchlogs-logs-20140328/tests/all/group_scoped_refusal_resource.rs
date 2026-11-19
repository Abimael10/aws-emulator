// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE ARN an authorization refusal of a LOG-GROUP-SCOPED action
//! renders.
//!
//! RULE: an action CloudWatch Logs authorizes against its `log-group` resource type names, in
//! the `AccessDeniedException` it is refused with, the log group's own ARN and nothing more —
//! `arn:aws:logs:<region>:<account>:log-group:<name>`, carrying no `log-stream:` segment at
//! all — whichever member of the request names the group, whichever stream SELECTOR the
//! request carries beside it, and whatever shape the group's name has; the ARN a request
//! carries in that member is named as carried. Only a call that names ONE log stream is a call
//! on a stream, and that is the only call whose resource carries the stream segment, which is
//! why a statement written over the group's own ARN decides the group-scoped call and not the
//! stream-scoped one.
//!
//! That ARN is what a client parses out of a refusal and copies into the `Resource` element of
//! the statement it repairs, so a stream segment the request never asked about would send an
//! operator — or an automated policy-repair tool — at the wrong resource.
//!
//! The rows span what the rule generalises over: the shape of the log group's name (a vended
//! path of several segments, a flat name, a name carrying the punctuation `LogGroupName`
//! admits, a deep path in upper case), which member names the group (`logGroupName`,
//! `logGroupIdentifier` by name and `logGroupIdentifier` carrying the group's own ARN), which
//! stream selector the request carries beside it (none, a list of stream names, a stream-name
//! prefix), and which group-scoped operation is called — reads, a write, a delete and a
//! tagging read, including operations the issue this rule was written for did not cite and
//! ones whose own model declares no access error.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` — and the principal is minted with IAM's and STS's
//! OWN operations over the same engine, so the documents this service reads are the ones those
//! services wrote. Nothing here asserts on the clock.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{assumed_role, statement, Emulator, Principal};
use crate::harness::{now_ms, REGION};

/// The wire error code of an authorization refusal, as the AWS "Common Errors" table names it.
/// Stated here rather than read off the crate under test.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The log stream seeded under every group below — what the stream-scoped control reads, and
/// what the stream selectors of the group-scoped rows name.
const STREAM: &str = "2026/01/01/[$LATEST]one";

/// The segment an ARN of this service's log-STREAM resource type carries and one of its
/// log-GROUP resource type does not.
const STREAM_SEGMENT: &str = ":log-stream:";

/// The log GROUP's own ARN — the resource a group-scoped call is decided against. Spelled here
/// independently of the crate under test.
fn group_resource(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}",
        engine::emulator_account_id()
    )
}

/// The log STREAM's own ARN — the resource a call naming one stream is decided against.
fn stream_resource(group: &str, stream: &str) -> String {
    format!("{}{STREAM_SEGMENT}{stream}", group_resource(group))
}

/// The whole sentence IAM refuses a call with, stated here independently of the crate under
/// test.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause of a call an explicit `Deny` refuses.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The IAM action one operation of this service is performed under.
fn action_of(operation: &str) -> String {
    format!("logs:{operation}")
}

/// One identity-based policy document carrying one statement over several resource ARNs.
///
/// `statement` (shared with the suites beside this one) carries a single resource, which cannot
/// express a deny over the groups of a whole table.
fn statement_over(effect: &str, action: &str, resources: &[String]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": action, "Resource": resources}],
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

/// Create the log group `group` with one stream under it and one event in that stream, all with
/// the account's own credential.
async fn seeded(aws: &Emulator, group: &str) {
    aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": STREAM}),
    )
    .await;
    aws.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": STREAM,
            "logEvents": [{"timestamp": now_ms(), "message": "seeded"}],
        }),
    )
    .await;
}

/// Assert the call `operation` with `body`, made as `principal`, is refused as unauthorized with
/// the whole sentence IAM states for `resource`, and that the sentence names no log stream.
async fn refused_naming(
    aws: &Emulator,
    principal: &Principal,
    case: &str,
    operation: &str,
    body: &Value,
    resource: &str,
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
    let stated = message_of(case, &parsed);
    let expected = not_authorized(
        &principal.arn,
        &action_of(operation),
        resource,
        EXPLICIT_DENY,
    );
    assert_eq!(
        stated, expected,
        "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
    );
    assert!(
        !stated.contains(STREAM_SEGMENT),
        "case {case:?}: a group-scoped refusal names no log stream, got {stated:?}"
    );
}

/// The group-scoped calls one log group is asked for below: the case, the operation, and the
/// body built over that group.
///
/// SOURCE of each member named here, and of the operation's scope: the operation's own modelled
/// input, `smithy_query(query="operation",operation="<Operation>")` for each of
/// `FilterLogEvents`, `GetLogGroupFields`, `DescribeSubscriptionFilters`, `ListTagsLogGroup`,
/// `GetDataProtectionPolicy`, `PutRetentionPolicy` and `DeleteLogGroup` — each takes a log
/// group and no log stream, so each is a call on the group.
fn group_scoped_calls(group: &str) -> Vec<(String, &'static str, Value)> {
    vec![
        (
            "a read across the group's streams naming no selector".to_string(),
            "FilterLogEvents",
            json!({"logGroupName": group}),
        ),
        (
            "a read selecting the group's streams by a list".to_string(),
            "FilterLogEvents",
            json!({"logGroupName": group, "logStreamNames": [STREAM]}),
        ),
        (
            "a read selecting the group's streams by a prefix".to_string(),
            "FilterLogEvents",
            json!({"logGroupName": group, "logStreamNamePrefix": "2026/"}),
        ),
        (
            "a read addressed by the group's identifier".to_string(),
            "FilterLogEvents",
            json!({"logGroupIdentifier": group}),
        ),
        (
            "a read addressed by the group's own ARN".to_string(),
            "FilterLogEvents",
            json!({"logGroupIdentifier": group_resource(group)}),
        ),
        (
            "a read of the fields across the group".to_string(),
            "GetLogGroupFields",
            json!({"logGroupName": group}),
        ),
        (
            "a listing of the group's subscription filters".to_string(),
            "DescribeSubscriptionFilters",
            json!({"logGroupName": group}),
        ),
        (
            "a read of the group's tags".to_string(),
            "ListTagsLogGroup",
            json!({"logGroupName": group}),
        ),
        (
            "a read of the group's data protection policy".to_string(),
            "GetDataProtectionPolicy",
            json!({"logGroupIdentifier": group}),
        ),
        (
            "a write of a group property".to_string(),
            "PutRetentionPolicy",
            json!({"logGroupName": group, "retentionInDays": 7}),
        ),
        (
            "a delete of the group itself".to_string(),
            "DeleteLogGroup",
            json!({"logGroupName": group}),
        ),
    ]
}

/// RULE: every group-scoped call is refused naming the log group's own ARN — never that ARN
/// with a stream segment appended — whatever the group's name looks like, whichever member
/// names it and whichever stream selector the request carries beside it.
#[tokio::test]
async fn a_group_scoped_refusal_names_the_groups_own_arn() {
    let aws = Emulator::new();
    // The rows differ along the shape of the name, which `LogGroupName` admits as
    // `^[\.\-_/#A-Za-z0-9]+$` (`smithy_query(query="shape",shape="LogGroupName")`): a vended
    // path of several segments, a flat name, a name carrying that punctuation, and a deep path
    // in upper case.
    let groups = [
        (
            "a vended path of several segments",
            "/aws/vendedlogs/emulator/runtime/APPLICATION_LOGS/agent-7",
        ),
        ("a flat name", "orders"),
        ("a name carrying punctuation", "team.one_logs-9#a"),
        ("a deep path in upper case", "/AWS/Deep/PATH/Segment/NINE"),
    ];
    for (_, group) in groups {
        seeded(&aws, group).await;
    }
    // The session may do everything except what the table asks of these groups: an explicit
    // deny written over each group's OWN ARN, which is the resource every group-scoped call
    // addresses and no stream-scoped one does.
    let denied: Vec<String> = groups
        .iter()
        .map(|(_, group)| group_resource(group))
        .collect();
    let principal = assumed_role(
        &aws,
        "logs-group-scoped-refusal",
        &[
            statement("Allow", "logs:*", "*"),
            statement_over("Deny", "logs:*", &denied),
        ],
    )
    .await;
    for (shape, group) in groups {
        for (call, operation, body) in group_scoped_calls(group) {
            let case = format!("{call} of a group with {shape}");
            refused_naming(
                &aws,
                &principal,
                &case,
                operation,
                &body,
                &group_resource(group),
            )
            .await;
        }
    }
    // Nothing the refusals asked for happened: every group the deletes named is still held, and
    // with the retention the refused writes never set.
    for (shape, group) in groups {
        let page = aws
            .ok("DescribeLogGroups", &json!({"logGroupNamePrefix": group}))
            .await;
        let held = page["logGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("DescribeLogGroups reports a logGroups list, got {page}"))
            .clone();
        assert_eq!(
            held.len(),
            1,
            "case \"a refused delete of a group with {shape}\": expected the group to be held \
             still, got {held:?}"
        );
        assert!(
            held[0].get("retentionInDays").is_none(),
            "case \"a refused retention write on a group with {shape}\": expected no retention, \
             got {:?}",
            held[0]
        );
    }
}

/// RULE: the stream segment belongs to a call that names ONE log stream, so a statement written
/// over the group's own ARN decides the group-scoped call and leaves the stream-scoped one to
/// the ARN that does carry the segment.
#[tokio::test]
async fn a_deny_on_the_groups_own_arn_leaves_the_stream_scoped_call_alone() {
    let aws = Emulator::new();
    let group = "/aws/vendedlogs/emulator/runtime/APPLICATION_LOGS/agent-1";
    seeded(&aws, group).await;
    let principal = assumed_role(
        &aws,
        "logs-group-scoped-only",
        &[
            statement("Allow", "logs:*", "*"),
            statement("Deny", "logs:*", &group_resource(group)),
        ],
    )
    .await;
    refused_naming(
        &aws,
        &principal,
        "the group-scoped read the deny reaches",
        "FilterLogEvents",
        &json!({"logGroupName": group}),
        &group_resource(group),
    )
    .await;
    // The same deny does not reach a call on ONE stream of that group: those are decided
    // against the stream's own ARN, which the deny's resource does not cover.
    for (case, operation, body) in [
        (
            "a read of one stream's events",
            "GetLogEvents",
            json!({"logGroupName": group, "logStreamName": STREAM}),
        ),
        (
            "a write into one stream",
            "PutLogEvents",
            json!({
                "logGroupName": group,
                "logStreamName": STREAM,
                "logEvents": [{"timestamp": now_ms(), "message": "served"}],
            }),
        ),
    ] {
        let (status, parsed) = aws
            .call_as(&principal.access_key_id, operation, &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} on {} to be served, got {status} {parsed}",
            stream_resource(group, STREAM)
        );
    }
    // And a deny written over the STREAM's own ARN is the mirror image: the stream-scoped read
    // is refused naming that ARN, while the group-scoped read is served.
    let stream_denied = assumed_role(
        &aws,
        "logs-stream-scoped-only",
        &[
            statement("Allow", "logs:*", "*"),
            statement("Deny", "logs:*", &stream_resource(group, STREAM)),
        ],
    )
    .await;
    let case = "a read of one stream's events the stream deny reaches";
    let (status, parsed) = aws
        .call_as(
            &stream_denied.access_key_id,
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": STREAM}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the stream read to be refused, got {status} {parsed}"
    );
    let stated = message_of(case, &parsed);
    let expected = not_authorized(
        &stream_denied.arn,
        &action_of("GetLogEvents"),
        &stream_resource(group, STREAM),
        EXPLICIT_DENY,
    );
    assert_eq!(
        stated, expected,
        "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
    );
    let (status, parsed) = aws
        .call_as(
            &stream_denied.access_key_id,
            "FilterLogEvents",
            &json!({"logGroupName": group}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the group-scoped read a stream deny does not reach\": expected HTTP 200, got \
         {status} {parsed}"
    );
}
