// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the permission a read needs to see what a DATA PROTECTION POLICY
//! masked.
//!
//! RULE under test, as `GetLogEventsRequest$unmask` and `FilterLogEventsRequest$unmask` both
//! state it: "Specify true to display the log event fields with all sensitive data unmasked
//! and visible. The default is false. To use this operation with this parameter, you must be
//! signed into an account with the logs:Unmask permission." So asking to unmask performs the
//! `logs:Unmask` action BESIDE the read's own action, and it is decided against the calling
//! principal's identity-based IAM policies before anything is read: a caller whose documents
//! do not allow that action on the resource the read addresses is refused with
//! `AccessDeniedException` (HTTP 400) and is served no event at all, while the SAME read
//! without `unmask` keeps being served the de-identified events. The resource is the log group
//! the read addresses — stream-qualified for a read of one stream, the group's own ARN for a
//! read scoped to the group — and granting `logs:Unmask` over it makes the identical call
//! succeed and report the ingested message verbatim.
//!
//! The rows span what the rule generalises over: which of the two reads is called, whether the
//! caller holds no `logs:Unmask` statement / one over the group addressed / one over every
//! resource / one over ANOTHER group / a service-wide grant with an explicit deny, and the
//! account's own credential, which holds no IAM policies.
//!
//! The principals are made with IAM's and STS's OWN operations over the same engine, so the
//! documents this service reads are the ones those services wrote, and every case drives the
//! service the way a client does — `POST /` with `X-Amz-Target: Logs_20140328.<Operation>`.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{assumed_role, statement, Emulator, Principal};
use crate::harness::REGION;

/// The wire error code of an authorization refusal, as the AWS "Common Errors" table names it.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The IAM action a read performs by asking for masked data to be revealed, as the member's
/// own documentation names it. Stated here rather than read off the crate under test.
const UNMASK_ACTION: &str = "logs:Unmask";

/// The two reads the rule names, and the only two whose model carries an `unmask` member
/// together with the masked rendering it lifts.
const STREAM_READ: &str = "GetLogEvents";
const GROUP_READ: &str = "FilterLogEvents";

/// The data identifier the protected group's policy names.
const EMAIL_IDENTIFIER: &str = "arn:aws:dataprotection::aws:data-identifier/EmailAddress";

/// The sensitive term the ingested message carries, and the message carrying it.
const ADDRESS: &str = "jane.doe@example.com";
const INGESTED: &str = "login attempt from user jane.doe@example.com failed";

/// The same message as a reader without the permission sees it: the matched span replaced by
/// asterisks, the rest verbatim.
const MASKED: &str = "login attempt from user ******************** failed";

/// The reason clause of a call no readable document allows — IAM's default deny.
fn nothing_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause of a call an explicit `Deny` refuses.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The whole sentence IAM refuses a call with, stated here independently of the crate under
/// test.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The ARN of the log group `name` in this account and Region.
fn group_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{name}",
        engine::emulator_account_id()
    )
}

/// The ARN form a policy statement covers a log group and everything under it with.
fn group_wildcard_arn(name: &str) -> String {
    format!("{}:*", group_arn(name))
}

/// The resource the `logs:Unmask` decision of one read is taken against: the log group it
/// addresses, carrying the empty `log-stream:` qualifier for a read of ONE stream and bare for
/// a read scoped to the group. Spelled here independently of the crate under test.
fn unmask_resource(operation: &str, group: &str) -> String {
    if operation == STREAM_READ {
        format!("{}:log-stream:", group_arn(group))
    } else {
        group_arn(group)
    }
}

/// The body one read is called with: the group (and, for the stream read, the stream) it
/// addresses, and whether it asks to unmask.
fn read_body(operation: &str, group: &str, stream: &str, unmask: bool) -> Value {
    if operation == STREAM_READ {
        json!({
            "logGroupName": group,
            "logStreamName": stream,
            "startFromHead": true,
            "unmask": unmask,
        })
    } else {
        json!({"logGroupName": group, "unmask": unmask})
    }
}

/// The `message` values of a page either read answers with, in the order returned.
fn messages(case: &str, page: &Value) -> Vec<String> {
    page.get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a read reports events, got {page}"))
        .iter()
        .map(|event| {
            event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: every event carries a message, got {event}"))
                .to_string()
        })
        .collect()
}

/// The wire error code an answer reports, or `None` for an answer that is no error body.
fn reported_code(body: &Value) -> Option<String> {
    let raw = body.get("__type").and_then(Value::as_str)?;
    Some(raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string())
}

/// The message an error body reports.
fn reported_message(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {body}"))
        .to_string()
}

/// A policy document carrying one statement over SEVERAL resource ARNs — the two ARN forms a
/// grant for one log group's event reads is written over.
fn statement_over(effect: &str, action: &str, resources: &[String]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": action, "Resource": resources}],
    })
    .to_string()
}

/// The policy document a data protection policy over the email identifier is put with, in the
/// form `PutDataProtectionPolicy$policyDocument` documents.
fn data_protection_document() -> String {
    json!({
        "Name": "unmask-suite-policy",
        "Version": "2021-06-01",
        "Statement": [
            {
                "Sid": "audit-email",
                "DataIdentifier": [EMAIL_IDENTIFIER],
                "Operation": {"Audit": {"FindingsDestination": {}}},
            },
            {
                "Sid": "mask-email",
                "DataIdentifier": [EMAIL_IDENTIFIER],
                "Operation": {"Deidentify": {"MaskConfig": {}}},
            },
        ],
    })
    .to_string()
}

/// An in-window timestamp: `PutLogEvents` admits events against a clock-relative window, so
/// every event here is stamped an hour before the ingestion clock.
fn base_ms() -> i64 {
    crate::harness::now_ms() - 60 * 60 * 1_000
}

/// A protected log group holding one stream and one event whose message carries the address,
/// with a data protection policy over the email identifier in force when it was ingested.
async fn protected_group(aws: &Emulator, group: &str, stream: &str) {
    aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    aws.ok(
        "PutDataProtectionPolicy",
        &json!({
            "logGroupIdentifier": group,
            "policyDocument": data_protection_document(),
        }),
    )
    .await;
    aws.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": [{"timestamp": base_ms(), "message": INGESTED}],
        }),
    )
    .await;
    // The policy is in force on the ingest path: the privileged read reports the masked
    // message, which is what the unmask decision below is about.
    let page = aws
        .ok(STREAM_READ, &read_body(STREAM_READ, group, stream, false))
        .await;
    let read = messages("the privileged masked read", &page);
    assert_eq!(
        read,
        vec![MASKED.to_string()],
        "case \"the privileged masked read\": expected the masked message, got {read:?}"
    );
}

/// The statements a role needs to READ the protected group at all — this service decides every
/// operation against the caller's policies, so the gate under test is only reached by a caller
/// the read's own action is allowed for.
fn read_grants(group: &str) -> Vec<String> {
    vec![statement_over(
        "Allow",
        "logs:GetLogEvents",
        &[group_arn(group), group_wildcard_arn(group)],
    ),
    statement_over(
        "Allow",
        "logs:FilterLogEvents",
        &[group_arn(group), group_wildcard_arn(group)],
    )]
}

/// Assert the read `operation` as `principal` was refused as unauthorized to unmask, with the
/// whole sentence IAM states for `reason` — and that it was served no event.
async fn refused_to_unmask(
    aws: &Emulator,
    principal: &Principal,
    operation: &str,
    group: &str,
    stream: &str,
    case: &str,
    reason: &str,
) {
    let (status, parsed) = aws
        .call_as(
            &principal.access_key_id,
            operation,
            &read_body(operation, group, stream, true),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to be refused with HTTP 400, got {status} {parsed}"
    );
    assert_eq!(
        reported_code(&parsed).as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected the error code {ACCESS_DENIED}, got {parsed}"
    );
    let expected = not_authorized(
        &principal.arn,
        UNMASK_ACTION,
        &unmask_resource(operation, group),
        reason,
    );
    let stated = reported_message(case, &parsed);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
    );
    assert!(
        !stated.contains(ADDRESS) && parsed.get("events").is_none(),
        "case {case:?}: a refused read is served no event, got {parsed}"
    );
}

/// Assert the read `operation` as `principal` was served the INGESTED message: the permission
/// was granted, so the masking is lifted.
async fn served_unmasked(
    aws: &Emulator,
    principal: &Principal,
    operation: &str,
    group: &str,
    stream: &str,
    case: &str,
) {
    let (status, parsed) = aws
        .call_as(
            &principal.access_key_id,
            operation,
            &read_body(operation, group, stream, true),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be served, got {status} {parsed}"
    );
    let read = messages(case, &parsed);
    assert_eq!(
        read,
        vec![INGESTED.to_string()],
        "case {case:?}: expected the ingested message, got {read:?}"
    );
}

/// RULE: a read that asks to unmask without the `logs:Unmask` permission is refused, naming
/// the principal, the action and the resource that read addresses — while the identical read
/// WITHOUT `unmask` keeps serving the de-identified event, and granting the permission to the
/// same role makes the refused call succeed and report the ingested message.
#[tokio::test]
async fn unmask_is_refused_without_the_permission_and_the_masked_read_is_still_served() {
    let aws = Emulator::new();
    let group = "/unmask/limited";
    let stream = "ingest";
    protected_group(&aws, group, stream).await;
    let role = "logs-unmask-limited";
    let principal = assumed_role(&aws, role, &read_grants(group)).await;

    for operation in [STREAM_READ, GROUP_READ] {
        // The positive control: the same principal, the same read, no `unmask`.
        let case = format!("{operation} without unmask");
        let (status, parsed) = aws
            .call_as(
                &principal.access_key_id,
                operation,
                &read_body(operation, group, stream, false),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the masked read to be served, got {status} {parsed}"
        );
        let read = messages(&case, &parsed);
        assert_eq!(
            read,
            vec![MASKED.to_string()],
            "case {case:?}: expected the de-identified event, got {read:?}"
        );

        // ...and the same read asking to unmask is refused.
        refused_to_unmask(
            &aws,
            &principal,
            operation,
            group,
            stream,
            &format!("{operation} with unmask and no logs:Unmask grant"),
            &nothing_allows(UNMASK_ACTION),
        )
        .await;
    }

    // Granting `logs:Unmask` to the SAME role, over the two ARN forms its reads are addressed
    // by, makes the identical calls succeed: the decision is taken per request, so a statement
    // IAM wrote decides the next read.
    aws.query(
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role.to_string()),
            ("PolicyName", "unmask-grant".to_string()),
            (
                "PolicyDocument",
                statement_over(
                    "Allow",
                    UNMASK_ACTION,
                    &[group_arn(group), group_wildcard_arn(group)],
                ),
            ),
        ],
    )
    .await;
    for operation in [STREAM_READ, GROUP_READ] {
        served_unmasked(
            &aws,
            &principal,
            operation,
            group,
            stream,
            &format!("{operation} with unmask once logs:Unmask is granted"),
        )
        .await;
    }
}

/// RULE: the decision is IAM's own evaluation of the documents in force, so WHICH statements
/// the caller carries decides whether the masking is lifted — a grant over the group addressed
/// permits it, one over another group does not, and an explicit deny wins over a service-wide
/// grant.
#[tokio::test]
async fn which_statements_decide_an_unmask_read() {
    let aws = Emulator::new();
    let group = "/unmask/documents";
    let stream = "ingest";
    protected_group(&aws, group, stream).await;

    // (case, the statements the role carries beside the read grants, the expected refusal)
    let rows: Vec<(&str, Vec<String>, Option<String>)> = vec![
        (
            "a grant of logs:Unmask over the group addressed",
            vec![statement_over(
                "Allow",
                UNMASK_ACTION,
                &[group_arn(group), group_wildcard_arn(group)],
            )],
            None,
        ),
        (
            "a grant of logs:Unmask over every resource",
            vec![statement("Allow", UNMASK_ACTION, "*")],
            None,
        ),
        (
            "a service-wide grant of this service's actions",
            vec![statement("Allow", "logs:*", "*")],
            None,
        ),
        (
            "a grant of logs:Unmask over another log group",
            vec![statement_over(
                "Allow",
                UNMASK_ACTION,
                &[
                    group_arn("/unmask/elsewhere"),
                    group_wildcard_arn("/unmask/elsewhere"),
                ],
            )],
            Some(nothing_allows(UNMASK_ACTION)),
        ),
        (
            "no logs:Unmask statement at all",
            Vec::new(),
            Some(nothing_allows(UNMASK_ACTION)),
        ),
        (
            "a grant for another action of this service on the same group",
            vec![statement(
                "Allow",
                "logs:DescribeLogStreams",
                &group_wildcard_arn(group),
            )],
            Some(nothing_allows(UNMASK_ACTION)),
        ),
        (
            "a service-wide grant and an explicit deny of logs:Unmask",
            vec![
                statement("Allow", "logs:*", "*"),
                statement("Deny", UNMASK_ACTION, "*"),
            ],
            Some(EXPLICIT_DENY.to_string()),
        ),
    ];

    for (index, (case, extra, refusal)) in rows.into_iter().enumerate() {
        let mut documents = read_grants(group);
        documents.extend(extra);
        let principal = assumed_role(&aws, &format!("logs-unmask-{index}"), &documents).await;
        for operation in [STREAM_READ, GROUP_READ] {
            let case = format!("{case} calling {operation}");
            match &refusal {
                Some(reason) => {
                    refused_to_unmask(&aws, &principal, operation, group, stream, &case, reason)
                        .await
                }
                None => served_unmasked(&aws, &principal, operation, group, stream, &case).await,
            }
        }
    }
}

/// RULE: the account's own published credential holds no IAM policies — the position AWS
/// account root credentials are in — so it keeps being served an unmasked read, which is also
/// what keeps this emulator's own in-process reads working.
#[tokio::test]
async fn the_accounts_own_credential_keeps_being_served_an_unmask_read() {
    let aws = Emulator::new();
    let group = "/unmask/account-credential";
    let stream = "ingest";
    protected_group(&aws, group, stream).await;
    for operation in [STREAM_READ, GROUP_READ] {
        let case = format!("{operation} with unmask under the account's own credential");
        let page = aws
            .ok(operation, &read_body(operation, group, stream, true))
            .await;
        let read = messages(&case, &page);
        assert_eq!(
            read,
            vec![INGESTED.to_string()],
            "case {case:?}: expected the ingested message, got {read:?}"
        );
    }
}
