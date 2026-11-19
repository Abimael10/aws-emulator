// SPDX-License-Identifier: CC0-1.0

//! How a resource-based policy DOCUMENT this service cannot attach is refused: one sentence
//! per defect class, worded as AWS words that class.
//!
//! RULE pinned here: **a resource-policy write refuses a document it cannot attach with
//! AWS's sentence for the CLASS of defect the document has, interpolating only what that
//! class's template takes from the request — an `Action` naming another vendor's service is
//! the fixed vendor-inconsistency sentence, which names no action; an `Action` naming no
//! action of the resource the document is attached to lists the offending names, each quoted
//! as the document wrote it; and an `Allow` that grants a public principal with nothing to
//! bound it is the unbounded-access sentence, a class of its own that is NOT stated behind
//! the parameter-value clause the document defects are. Every refusal is HTTP 400
//! `ValidationException` and attaches nothing, and a document of none of those classes is
//! attached and reads back.**
//!
//! The rows differ along what the rule generalises over: which vendor an action names, the
//! form the `Action` element is written in (one string, a list, a wildcard, a second
//! statement), which resource the document is attached to (a table and its stream), how many
//! names of one document are invalid, the case the action is spelled in, and the spelling of
//! a public `Principal` (the bare wildcard, the wildcard under its principal type, a
//! one-element list) with and without a `Condition` to bound it.
//!
//! Driven over the wire (`POST /` with `X-Amz-Target: DynamoDB_20120810.<Operation>`)
//! through the same `MultiServiceRouter` the emulator's front doors use.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, POLL_INTERVAL};

/// AWS's sentence for an `Action` that names a service other than the one the resource
/// belongs to: fixed text in its entirety, naming neither the action nor the vendor.
const VENDOR_SENTENCE: &str = "One or more parameter values were invalid: Invalid policy \
                               document: Action field includes AWS services that are \
                               inconsistent with specified vendor";

/// AWS's sentence for an `Allow` that grants unbounded access: its own class, stated without
/// the parameter-value clause the document defects above are stated behind.
const UNBOUNDED_SENTENCE: &str = "Resource-based policy grants unbounded access in one or more \
                                  elements. Please revise the policy to ensure least-privilege \
                                  form of access";

/// How long a row waits for an attached policy to become readable: "`PutResourcePolicy` is an
/// asynchronous operation", so the wait is on the REVISION the write reported, under a
/// deadline far longer than the lag so a loaded host cannot turn the wait into a failure.
const POLICY_READ_DEADLINE: Duration = Duration::from_secs(60);

/// The policy-language version every document here states.
const POLICY_VERSION: &str = "2012-10-17";

/// An identity of ANOTHER account, named where a row needs a principal that is neither
/// public nor this caller.
const ELSEWHERE: &str = "arn:aws:iam::210987654321:role/elsewhere";

/// AWS's sentence for action names the resource does not have: the names from the request,
/// each quoted, in the order the document lists them.
fn invalid_action_names(named: &[&str]) -> String {
    let listed: Vec<String> = named.iter().map(|value| format!("\"{value}\"")).collect();
    format!(
        "One or more parameter values were invalid: Invalid policy document: The following \
         action names are invalid: {}",
        listed.join(", ")
    )
}

/// A one-statement document of `effect` over `action`, about `principal`, on `resource_arn`.
fn statement(effect: &str, principal: Value, action: Value, resource_arn: &str) -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": effect,
            "Principal": principal,
            "Action": action,
            "Resource": resource_arn,
        }],
    })
}

/// A `Deny` of `action` about everyone, which is the effect no admission rule here refuses.
fn deny(action: Value, resource_arn: &str) -> Value {
    statement("Deny", json!({"AWS": "*"}), action, resource_arn)
}

/// RULE: an `Action` naming another vendor's service is refused with AWS's fixed
/// vendor-inconsistency sentence, whichever vendor it names, whichever form the element takes
/// and whichever of the two resources the document is offered for.
#[tokio::test]
async fn an_action_of_another_vendor_is_refused_with_the_vendor_sentence() {
    let router = emulator();
    let (table_arn, stream_arn) = streamed_table(&router, "PolicyVendorActions").await;

    let rows: Vec<(&str, &String, Value)> = vec![
        (
            "one foreign action on the table",
            &table_arn,
            deny(json!("s3:GetObject"), &table_arn),
        ),
        (
            "another vendor entirely",
            &table_arn,
            deny(json!("sqs:SendMessage"), &table_arn),
        ),
        (
            "a vendor spelled in another case",
            &table_arn,
            deny(json!("KMS:Decrypt"), &table_arn),
        ),
        (
            "a foreign vendor's wildcard",
            &table_arn,
            deny(json!("lambda:*"), &table_arn),
        ),
        (
            "a foreign action listed after this service's own",
            &table_arn,
            deny(json!(["dynamodb:GetItem", "s3:PutObject"]), &table_arn),
        ),
        (
            "a foreign action in the second statement",
            &table_arn,
            json!({
                "Version": POLICY_VERSION,
                "Statement": [
                    {"Effect": "Deny", "Principal": {"AWS": "*"},
                     "Action": "dynamodb:GetItem", "Resource": &table_arn},
                    {"Effect": "Deny", "Principal": {"AWS": "*"},
                     "Action": "sns:Publish", "Resource": &table_arn},
                ],
            }),
        ),
        (
            // The vendor rule is about the ACTION's service, so it decides a stream's
            // document too — and takes precedence over which actions that resource has.
            "a foreign action on the stream",
            &stream_arn,
            deny(json!("s3:GetObject"), &stream_arn),
        ),
    ];

    for (case, arn, document) in &rows {
        refused(&router, case, arn, document, VENDOR_SENTENCE).await;
    }
}

/// RULE: an `Action` whose vendor is right but which names no action the resource HAS is a
/// different class — the names the document lists, quoted, in document order.
#[tokio::test]
async fn an_action_the_stream_does_not_have_is_refused_by_name() {
    let router = emulator();
    let (table_arn, stream_arn) = streamed_table(&router, "PolicyStreamActionNames").await;

    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a table read action in a stream's policy",
            deny(json!("dynamodb:GetItem"), &stream_arn),
            invalid_action_names(&["dynamodb:GetItem"]),
        ),
        (
            "a table write action in a stream's policy",
            deny(json!("dynamodb:PutItem"), &stream_arn),
            invalid_action_names(&["dynamodb:PutItem"]),
        ),
        (
            "a control-plane action in a stream's policy",
            deny(json!("dynamodb:DescribeTable"), &stream_arn),
            invalid_action_names(&["dynamodb:DescribeTable"]),
        ),
        (
            // The name is echoed as the document wrote it: the quoted part of the sentence
            // is the request's value, not a normalised one.
            "an action spelled in another case",
            deny(json!("dynamodb:getitem"), &stream_arn),
            invalid_action_names(&["dynamodb:getitem"]),
        ),
        (
            "two names the stream does not have, listed in one element",
            deny(
                json!(["dynamodb:GetItem", "dynamodb:GetRecords", "dynamodb:Query"]),
                &stream_arn,
            ),
            invalid_action_names(&["dynamodb:GetItem", "dynamodb:Query"]),
        ),
        (
            "a name the stream does not have in the second statement",
            json!({
                "Version": POLICY_VERSION,
                "Statement": [
                    {"Effect": "Deny", "Principal": {"AWS": "*"},
                     "Action": "dynamodb:GetRecords", "Resource": &stream_arn},
                    {"Effect": "Deny", "Principal": {"AWS": "*"},
                     "Action": "dynamodb:Scan", "Resource": &stream_arn},
                ],
            }),
            invalid_action_names(&["dynamodb:Scan"]),
        ),
    ];

    for (case, document, expected) in &rows {
        refused(&router, case, &stream_arn, document, expected).await;
    }

    // The same action names are what a TABLE's policy is attached with, so the refusal above
    // is about the resource the document is attached to and not about the names themselves.
    let attached = ok(
        &router,
        "PutResourcePolicy",
        json!({"ResourceArn": &table_arn, "Policy": deny(json!("dynamodb:GetItem"), &table_arn)
            .to_string()}),
    )
    .await;
    assert!(
        attached["RevisionId"].as_str().is_some_and(|id| !id.is_empty()),
        "case a table action in the TABLE's policy: expected a revision id, got {attached}"
    );
}

/// RULE: every action a stream HAS is attached, so the refusal above turns on the resource's
/// own actions — and the attached document reads back through `GetResourcePolicy`.
#[tokio::test]
async fn the_actions_a_stream_has_are_attached_and_read_back() {
    let router = emulator();
    let (_table_arn, stream_arn) = streamed_table(&router, "PolicyStreamActionsHeld").await;

    // The actions a stream's policy may carry, from the Amazon DynamoDB Developer Guide's
    // "Resource-based policies" entry for streams, plus the wildcard forms that select
    // whatever actions the resource has. A wildcard that would cover the policy-management
    // actions is written about ANOTHER account's identity, so no row asks this caller to
    // confirm the removal of its own access (`ConfirmRemoveSelfResourceAccess`), which is a
    // rule of its own.
    let accepted: Vec<(Value, Value)> = vec![
        (json!({"AWS": "*"}), json!("dynamodb:GetRecords")),
        (json!({"AWS": "*"}), json!("dynamodb:GetShardIterator")),
        (json!({"AWS": "*"}), json!("dynamodb:DescribeStream")),
        (json!({"AWS": "*"}), json!("dynamodb:ListStreams")),
        (
            json!({"AWS": "*"}),
            json!(["dynamodb:GetRecords", "dynamodb:ListStreams"]),
        ),
        (json!({"AWS": ELSEWHERE}), json!("dynamodb:Get*")),
        (json!({"AWS": ELSEWHERE}), json!("*")),
    ];
    for (principal, action) in accepted {
        let case = format!("a stream policy over {action} about {principal}");
        let document = statement("Deny", principal, action, &stream_arn);
        let answered = ok(
            &router,
            "PutResourcePolicy",
            json!({"ResourceArn": &stream_arn, "Policy": document.to_string()}),
        )
        .await;
        let revision = answered["RevisionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a revision id, got {answered}"))
            .to_string();
        let read = read_when_revised(&router, &case, &stream_arn, &revision).await;
        assert_eq!(
            reported_document(&case, &read),
            document,
            "case {case:?}: the attached document must read back as it was attached, got {read}"
        );
    }
}

/// RULE: an `Allow` naming a public principal with no `Condition` to bound it grants
/// unbounded access and earns that class's own sentence; one a condition bounds, and a `Deny`
/// of everyone, are attached.
#[tokio::test]
async fn a_public_allow_is_refused_unless_a_condition_bounds_it() {
    let router = emulator();
    let (table_arn, _stream_arn) = streamed_table(&router, "PolicyUnboundedAccess").await;
    let action = json!("dynamodb:GetItem");

    let refusals: Vec<(&str, Value)> = vec![
        (
            "the bare wildcard as the whole Principal element",
            statement("Allow", json!("*"), action.clone(), &table_arn),
        ),
        (
            "the wildcard under its principal type",
            statement("Allow", json!({"AWS": "*"}), action.clone(), &table_arn),
        ),
        (
            "the wildcard as a one-element list",
            statement("Allow", json!({"AWS": ["*"]}), action.clone(), &table_arn),
        ),
        (
            "the wildcard listed beside a named identity",
            statement(
                "Allow",
                json!({"AWS": ["*", ELSEWHERE]}),
                action.clone(),
                &table_arn,
            ),
        ),
        (
            "a public Allow in the second statement",
            json!({
                "Version": POLICY_VERSION,
                "Statement": [
                    {"Effect": "Deny", "Principal": {"AWS": "*"},
                     "Action": "dynamodb:DeleteItem", "Resource": &table_arn},
                    {"Effect": "Allow", "Principal": {"AWS": "*"},
                     "Action": "dynamodb:GetItem", "Resource": &table_arn},
                ],
            }),
        ),
    ];
    for (case, document) in &refusals {
        refused(&router, case, &table_arn, document, UNBOUNDED_SENTENCE).await;
    }

    // Bounded, so attached: a condition on the statement decides which requests the grant
    // reaches, and a Deny of everyone grants nothing to anyone.
    let admitted: Vec<(&str, Value)> = vec![
        (
            "a public Allow bounded by the calling account",
            conditioned(
                statement("Allow", json!({"AWS": "*"}), action.clone(), &table_arn),
                json!({"StringEquals": {"aws:PrincipalAccount": "000000000000"}}),
            ),
        ),
        (
            "a public Allow bounded by the source address",
            conditioned(
                statement("Allow", json!("*"), action.clone(), &table_arn),
                json!({"IpAddress": {"aws:SourceIp": "203.0.113.0/24"}}),
            ),
        ),
        (
            "a Deny of everyone, which takes nothing from anyone in particular",
            statement("Deny", json!({"AWS": "*"}), action.clone(), &table_arn),
        ),
        (
            "an Allow of a named account rather than of everyone",
            statement(
                "Allow",
                json!({"AWS": "arn:aws:iam::000000000000:root"}),
                action.clone(),
                &table_arn,
            ),
        ),
    ];
    for (case, document) in &admitted {
        let answered = ok(
            &router,
            "PutResourcePolicy",
            json!({"ResourceArn": &table_arn, "Policy": document.to_string()}),
        )
        .await;
        let revision = answered["RevisionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a revision id, got {answered}"))
            .to_string();
        let read = read_when_revised(&router, case, &table_arn, &revision).await;
        assert_eq!(
            reported_document(case, &read),
            *document,
            "case {case:?}: the attached document must read back as it was attached, got {read}"
        );
    }
}

/// The same statement, gated on `condition`.
fn conditioned(mut document: Value, condition: Value) -> Value {
    document["Statement"][0]["Condition"] = condition;
    document
}

/// Attach `document` to `resource_arn`, expecting the refusal `expected` — and that the
/// refused write left the resource's policy exactly as it was.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
    document: &Value,
    expected: &str,
) {
    let before = call(
        router,
        "GetResourcePolicy",
        json!({"ResourceArn": resource_arn}),
    )
    .await;
    let (status, answered) = call(
        router,
        "PutResourcePolicy",
        json!({"ResourceArn": resource_arn, "Policy": document.to_string()}),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} with {answered}"
    );
    assert_eq!(
        message(&answered),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        message(&answered)
    );
    let after = call(
        router,
        "GetResourcePolicy",
        json!({"ResourceArn": resource_arn}),
    )
    .await;
    assert_eq!(
        after, before,
        "case {case:?}: a refused attach must change nothing, got {after:?} after {before:?}"
    );
}

/// Create a stream-enabled table and answer its ARN and the ARN of its stream.
async fn streamed_table(router: &MultiServiceRouter, name: &str) -> (String, String) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {"StreamEnabled": true, "StreamViewType": "NEW_IMAGE"},
        }),
    )
    .await;
    let described = describe_when_active(router, name, name).await;
    let table_arn = described["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: the description reports the table ARN"))
        .to_string();
    let stream_arn = described["LatestStreamArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: a stream-enabled table reports its stream ARN"))
        .to_string();
    (table_arn, stream_arn)
}

/// The `Policy` of a `GetResourcePolicy` answer, parsed — the document is compared as JSON,
/// so nothing here depends on how one object's members are ordered.
fn reported_document(case: &str, read: &Value) -> Value {
    let document = read["Policy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the read reports a policy document, got {read}"));
    serde_json::from_str(document).unwrap_or_else(|error| {
        panic!("case {case:?}: the reported document must be JSON, got {error} for {document}")
    })
}

/// Poll `GetResourcePolicy` until `revision` is the one in force, and answer that read — the
/// reader is eventually consistent, so the wait is for the CONDITION under a generous
/// deadline ([`POLICY_READ_DEADLINE`]) rather than for a span.
async fn read_when_revised(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
    revision: &str,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < POLICY_READ_DEADLINE {
        let (status, answered) = call(
            router,
            "GetResourcePolicy",
            json!({"ResourceArn": resource_arn}),
        )
        .await;
        if status == 200 && answered["RevisionId"] == json!(revision) {
            return answered;
        }
        last = answered;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the revision {revision} never came into force within \
         {POLICY_READ_DEADLINE:?}, last {last}"
    )
}
