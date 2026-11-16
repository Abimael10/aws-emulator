// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE an authorization refusal names when the request carries
//! nothing to name one with.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it, signed with a
//! role session IAM's and STS's own operations minted over the same engine — so the decision
//! and the sentence it carries are computed from the documents those services wrote.
//!
//! The rule under test: a call is judged against the resource it ADDRESSES, built from the
//! members of its request. An operation whose request carries NO member at all has nothing in
//! it to name a resource with, so the call is about every log group of the account and the
//! refusal names this service's log-group ARN carrying IAM's wildcard — while a request that
//! does carry log-group members is judged against the ARN those members build: the group's
//! own ARN where it names a group and no stream, the stream's where it names both, and this
//! service's stream ARN with both segments empty where it names neither.
//!
//! The table spans what the rule generalises over: whether the operation's request carries
//! members at all, whether the caller names a log group in them, whether it names a log
//! stream as well, and whether the grant the caller holds covers the account's log groups or
//! one group alone.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{assumed_role, statement, Emulator, Principal};
use crate::harness::REGION;

/// The error code an authorization refusal carries, as the AWS "Common Errors" table names
/// it. Stated here rather than read off the crate under test.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The operation whose modelled request carries no member at all. Source:
/// `smithy_query(query="operation", operation="GetStorageTierPolicy")` — its input shape
/// declares no members.
const ADDRESSED_BY_NO_MEMBER: &str = "GetStorageTierPolicy";

/// The resource ARN a call about every log group of the account is judged against: this
/// service's log-group ARN carrying IAM's wildcard. Spelled here independently of the crate
/// under test.
fn every_log_group() -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:*",
        engine::emulator_account_id()
    )
}

/// The resource ARN a call naming the log group `group` and the log stream `stream` is judged
/// against, with a segment left empty where the request names none.
fn addressed(group: &str, stream: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}:log-stream:{stream}",
        engine::emulator_account_id()
    )
}

/// The resource ARN a call naming the log group `group` and NO log stream is judged against:
/// the group's own ARN, which carries no `log-stream:` segment.
fn addressed_group(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}",
        engine::emulator_account_id()
    )
}

/// The whole sentence IAM refuses a call with, stated here independently of the crate under
/// test.
fn not_authorized(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The wire error code of an answer, or `None` for a body that is no error.
fn reported_code(body: &Value) -> Option<String> {
    let raw = body.get("__type").and_then(Value::as_str)?;
    Some(raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string())
}

/// Assert `operation` called as `principal` is refused naming `resource`.
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
        "case {case:?}: expected {ACCESS_DENIED}, got {parsed}"
    );
    let expected = not_authorized(&principal.arn, &format!("logs:{operation}"), resource);
    let stated = parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {parsed}"));
    assert_eq!(
        stated, expected,
        "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
    );
}

/// RULE: a request carrying no member at all is judged against every log group of the
/// account, and a request carrying log-group members against the ARN they build.
#[tokio::test]
async fn a_request_with_no_member_is_refused_for_every_log_group_of_the_account() {
    let aws = Emulator::new();
    let group = "/storage-tier/group";
    let denied = assumed_role(
        &aws,
        "logs-storage-tier",
        &[statement("Allow", "sts:GetCallerIdentity", "*")],
    )
    .await;

    // (case, the operation, its body, the resource the refusal must name)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a read whose request carries no member at all",
            ADDRESSED_BY_NO_MEMBER,
            json!({}),
            every_log_group(),
        ),
        (
            "a write whose request carries a member naming no log group",
            "PutStorageTierPolicy",
            json!({"storageTier": "STANDARD"}),
            addressed("", ""),
        ),
        (
            "a listing whose request names no log group",
            "DescribeLogGroups",
            json!({"logGroupNamePrefix": "/storage-tier/"}),
            addressed("", ""),
        ),
        (
            "a read addressed by a log group",
            "DescribeLogStreams",
            json!({"logGroupName": group}),
            addressed_group(group),
        ),
        (
            "a read addressed by a log group and a log stream",
            "GetLogEvents",
            json!({"logGroupName": group, "logStreamName": "one"}),
            addressed(group, "one"),
        ),
    ];
    for (case, operation, body, resource) in rows {
        refused_naming(&aws, &denied, case, operation, &body, &resource).await;
    }
}

/// RULE: the resource a call is judged against is the resource a GRANT has to cover, so a
/// statement scoped to every log group of the account admits the call carrying no member
/// while one scoped to a single log group does not.
#[tokio::test]
async fn a_grant_over_every_log_group_admits_the_call_that_names_none() {
    let aws = Emulator::new();
    let action = format!("logs:{ADDRESSED_BY_NO_MEMBER}");
    let one_group = format!(
        "arn:aws:logs:{REGION}:{}:log-group:/storage-tier/one:*",
        engine::emulator_account_id()
    );
    // (case, the resource the grant is scoped to, whether the call is served)
    let rows: Vec<(&str, String, bool)> = vec![
        ("a grant over every log group", every_log_group(), true),
        ("a grant over every resource", "*".to_string(), true),
        ("a grant scoped to one log group", one_group, false),
    ];
    for (index, (case, resource, served)) in rows.into_iter().enumerate() {
        let principal = assumed_role(
            &aws,
            &format!("logs-storage-tier-grant-{index}"),
            &[statement("Allow", &action, &resource)],
        )
        .await;
        let (status, parsed) = aws
            .call_as(&principal.access_key_id, ADDRESSED_BY_NO_MEMBER, &json!({}))
            .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} {parsed}"
            );
            assert!(
                parsed.get("storageTier").is_some(),
                "case {case:?}: expected the account's storage tier, got {parsed}"
            );
            continue;
        }
        assert_eq!(
            reported_code(&parsed).as_deref(),
            Some(ACCESS_DENIED),
            "case {case:?}: expected the call to be refused, got {status} {parsed}"
        );
    }
}
