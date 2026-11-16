// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the gate the ACCOUNT's query-result customer managed key puts on
//! CloudWatch Logs Insights queries.
//!
//! CloudWatch Logs and KMS are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so the key policy this gate reads is one KMS itself holds, written
//! by KMS's own operations, and a query that runs or is refused here is decided by a document
//! no part of this suite handed to CloudWatch Logs.
//!
//! The rule under test: while a KMS key is associated with the account's query results
//! (`AssociateKmsKey` with a `query-result` resource identifier), that key is in the path of
//! every Insights query of the account — so a query is served only while KMS reports a key
//! CloudWatch Logs can use AND that key's own key policy still lets it use the key for the
//! query-result ARN. Once either answer withholds the key, `StartQuery` and `GetQueryResults`
//! are refused with `ResourceNotFoundException` and `The KMS Key associated with the Account
//! does not have access on query results ARN`, nothing of the store changes, and making the key
//! usable again — or disassociating it — serves queries again.
//!
//! The table spans what the rule generalises over: how the key stops being usable (a policy that
//! grants CloudWatch Logs nothing, one that denies it, one scoped to another resource's ARN, and
//! the key's own state), how it becomes usable again (the policy restored, the key enabled, the
//! key disassociated), and which of the two operations on the query path asks the question.

use std::time::Duration;

use serde_json::{json, Value};

use crate::cmk_key_policy_scope::{account_statement, key_policy, Emulator, REGION};

/// The modelled error code a query the key can no longer serve is refused under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence such a refusal states, as CloudWatch Logs words it — written out here rather
/// than read from the crate, so a change of wording fails this suite.
const NO_ACCESS: &str =
    "The KMS Key associated with the Account does not have access on query results ARN";

/// The log group and stream every case queries.
const GROUP: &str = "/insights/query-result-key";
const STREAM: &str = "stream";

/// The messages the group holds, which a query that runs answers with.
const MESSAGES: [&str; 2] = ["qr-line one", "qr-line two"];

/// How long a case waits for a started query to finish before giving up — generous room for a
/// loaded host, since what is waited for is the CONDITION below and never this bound.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that condition is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The ARN of the account's stored query results, the target an association names.
fn query_results_arn() -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:query-result:*",
        engine::emulator_account_id()
    )
}

/// A key policy granting CloudWatch Logs the encryption operations unconditionally.
fn usable_policy() -> String {
    key_policy(&format!("logs.{REGION}.amazonaws.com"), None)
}

/// One KMS call that must succeed — KMS's own operation, over the same engine CloudWatch Logs
/// reads the key through.
async fn kms_ok(aws: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = aws.call("kms", "TrentService", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} {body} must succeed, got {status} {parsed}"
    );
    parsed
}

/// Put `policy` on the key `key` holds, which is how a grant is revoked and restored.
async fn put_key_policy(aws: &Emulator, case: &str, key: &str, policy: &str) {
    kms_ok(
        aws,
        case,
        "PutKeyPolicy",
        &json!({"KeyId": key, "PolicyName": "default", "Policy": policy}),
    )
    .await;
}

/// A log group holding [`MESSAGES`], and the query window in the epoch SECONDS
/// `StartQuery`'s range is specified in.
async fn group_with_events(aws: &Emulator, case: &str) -> (i64, i64) {
    aws.logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    aws.logs_ok(
        case,
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    let base = crate::harness::now_ms() - 60 * 1_000;
    let events: Vec<Value> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": base + index as i64 * 1_000, "message": message})
        })
        .collect();
    aws.logs_ok(
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": STREAM,
            "logEvents": events,
        }),
    )
    .await;
    let now_secs = crate::harness::now_ms() / 1_000;
    (now_secs - 3_600, now_secs + 60)
}

/// The request that starts the one query every case runs.
fn start_request(window: (i64, i64)) -> Value {
    json!({
        "logGroupName": GROUP,
        "startTime": window.0,
        "endTime": window.1,
        "queryString": "fields @message | sort @timestamp asc",
    })
}

/// Start the query and read it back once it has finished, answering the id it was minted and
/// the messages it served.
///
/// The wait is on the CONDITION the query reaches — the status it reports — looked at again
/// until it holds or [`COMPLETION_DEADLINE`] passes, so nothing here depends on how fast the
/// host runs.
async fn queried_messages(
    aws: &Emulator,
    case: &str,
    window: (i64, i64),
) -> (String, Vec<String>) {
    let started = aws.logs_ok(case, "StartQuery", &start_request(window)).await;
    let id = started["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a start answers a queryId, got {started}"))
        .to_string();
    let deadline = std::time::Instant::now() + COMPLETION_DEADLINE;
    loop {
        let answer = aws
            .logs_ok(case, "GetQueryResults", &json!({"queryId": id}))
            .await;
        if answer["status"].as_str() == Some("Complete") {
            let served = answer["results"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: results are a list, got {answer}"))
                .iter()
                .filter_map(|row| {
                    row.as_array()?
                        .iter()
                        .find(|field| field["field"].as_str() == Some("@message"))?
                        .get("value")?
                        .as_str()
                        .map(str::to_string)
                })
                .collect();
            return (id, served);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the query never completed, last answer {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that `operation` is refused by the query-result key gate, in AWS's sentence.
async fn refused_by_the_gate(aws: &Emulator, case: &str, operation: &str, body: &Value) {
    let (status, parsed) = aws.logs(operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must be refused once the grant is gone, got {status} \
         {parsed}"
    );
    assert_eq!(
        crate::harness::error_code(&parsed),
        NOT_FOUND,
        "case {case:?}: expected {NOT_FOUND} from {operation}, got {parsed}"
    );
    let said = parsed["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {parsed}"))
        .to_string();
    let sentence = crate::harness::without_sdk_envelope(case, &said, 400, NOT_FOUND);
    assert_eq!(
        sentence, NO_ACCESS,
        "case {case:?}: expected the query-result key sentence from {operation}, got {said:?}"
    );
}

/// One KMS call a row makes about the key, built from that key's ARN: the operation and its
/// body.
type KeyCall = fn(&str) -> (&'static str, Value);

/// One row: how the key stops being one CloudWatch Logs can use, and how it becomes one again.
struct Row {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The KMS call that leaves the key unable to serve the account's query results.
    withdraw: KeyCall,
    /// The KMS call that makes it usable again.
    restore: KeyCall,
}

/// The rows, in the order they are driven: each is a different way the key stops being usable.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the policy grants CloudWatch Logs nothing",
            withdraw: |key| {
                (
                    "PutKeyPolicy",
                    json!({
                        "KeyId": key,
                        "PolicyName": "default",
                        "Policy": json!({
                            "Version": "2012-10-17",
                            "Statement": [account_statement()],
                        })
                        .to_string(),
                    }),
                )
            },
            restore: |key| {
                (
                    "PutKeyPolicy",
                    json!({"KeyId": key, "PolicyName": "default", "Policy": usable_policy()}),
                )
            },
        },
        Row {
            case: "the policy denies CloudWatch Logs explicitly",
            withdraw: |key| {
                (
                    "PutKeyPolicy",
                    json!({
                        "KeyId": key,
                        "PolicyName": "default",
                        "Policy": json!({
                            "Version": "2012-10-17",
                            "Statement": [
                                account_statement(),
                                {
                                    "Effect": "Deny",
                                    "Principal": {
                                        "Service": format!("logs.{REGION}.amazonaws.com")
                                    },
                                    "Action": "kms:*",
                                    "Resource": "*",
                                },
                            ],
                        })
                        .to_string(),
                    }),
                )
            },
            restore: |key| {
                (
                    "PutKeyPolicy",
                    json!({"KeyId": key, "PolicyName": "default", "Policy": usable_policy()}),
                )
            },
        },
        Row {
            case: "the policy's grant is scoped to another resource's ARN",
            withdraw: |key| {
                (
                    "PutKeyPolicy",
                    json!({
                        "KeyId": key,
                        "PolicyName": "default",
                        "Policy": key_policy(
                            &format!("logs.{REGION}.amazonaws.com"),
                            Some(json!({
                                "ArnEquals": {
                                    "kms:EncryptionContext:aws:logs:arn": format!(
                                        "arn:aws:logs:{REGION}:{}:log-group:/somewhere/else",
                                        engine::emulator_account_id()
                                    )
                                }
                            })),
                        ),
                    }),
                )
            },
            restore: |key| {
                (
                    "PutKeyPolicy",
                    json!({"KeyId": key, "PolicyName": "default", "Policy": usable_policy()}),
                )
            },
        },
        Row {
            // The key's STATE is the other half of the question: a key KMS reports in any state
            // but enabled is one CloudWatch Logs cannot decrypt the stored results with,
            // whatever its policy grants.
            case: "the key is disabled",
            withdraw: |key| ("DisableKey", json!({"KeyId": key})),
            restore: |key| ("EnableKey", json!({"KeyId": key})),
        },
    ]
}

/// RULE: a query-result key CloudWatch Logs can no longer use stops the account's queries —
/// whichever way it stopped being usable — and making it usable again serves them.
#[tokio::test]
async fn an_unusable_query_result_key_stops_queries() {
    for row in rows() {
        let aws = Emulator::new();
        let case = row.case;
        let window = group_with_events(&aws, case).await;
        let key = aws.created_key(case, &usable_policy()).await;
        aws.logs_ok(
            case,
            "AssociateKmsKey",
            &json!({"resourceIdentifier": query_results_arn(), "kmsKeyId": key}),
        )
        .await;

        // The association is in place and the key grants CloudWatch Logs its use: queries run
        // and the results report the key they are stored under.
        let (first_id, served) = queried_messages(&aws, case, window).await;
        assert_eq!(
            served,
            MESSAGES.map(str::to_string).to_vec(),
            "case {case:?}: a usable query-result key serves the account's queries, got \
             {served:?}"
        );

        // Take the key away: both operations on the query path refuse — a new query cannot be
        // started, and the results the earlier one stored cannot be read.
        let (operation, body) = (row.withdraw)(&key);
        kms_ok(&aws, case, operation, &body).await;
        refused_by_the_gate(&aws, case, "StartQuery", &start_request(window)).await;
        refused_by_the_gate(
            &aws,
            case,
            "GetQueryResults",
            &json!({"queryId": first_id}),
        )
        .await;

        // Make it usable again: the same query runs, over the same events.
        let (operation, body) = (row.restore)(&key);
        kms_ok(&aws, case, operation, &body).await;
        let (_, again) = queried_messages(&aws, case, window).await;
        assert_eq!(
            again,
            MESSAGES.map(str::to_string).to_vec(),
            "case {case:?}: a usable key serves the account's queries again, got {again:?}"
        );
    }
}

/// RULE: the gate asks about the key the account ASSOCIATED, so disassociating it serves
/// queries again even while the key's own grant stays revoked — and an account that associated
/// no key is never gated.
#[tokio::test]
async fn only_an_associated_key_gates_the_account_queries() {
    let case = "the association is what puts the key in the path";
    let aws = Emulator::new();
    let window = group_with_events(&aws, case).await;

    // No association yet: a key whose grant is revoked decides nothing.
    let key = aws.created_key(case, &usable_policy()).await;
    let (_, served) = queried_messages(&aws, case, window).await;
    assert_eq!(
        served,
        MESSAGES.map(str::to_string).to_vec(),
        "case {case:?}: an account with no query-result key is not gated, got {served:?}"
    );

    aws.logs_ok(
        case,
        "AssociateKmsKey",
        &json!({"resourceIdentifier": query_results_arn(), "kmsKeyId": key}),
    )
    .await;
    put_key_policy(
        &aws,
        case,
        &key,
        &json!({"Version": "2012-10-17", "Statement": [account_statement()]}).to_string(),
    )
    .await;
    refused_by_the_gate(&aws, case, "StartQuery", &start_request(window)).await;

    // Disassociating takes the key out of the path, so the account's queries run again.
    aws.logs_ok(
        case,
        "DisassociateKmsKey",
        &json!({"resourceIdentifier": query_results_arn()}),
    )
    .await;
    let (_, again) = queried_messages(&aws, case, window).await;
    assert_eq!(
        again,
        MESSAGES.map(str::to_string).to_vec(),
        "case {case:?}: a disassociated key gates nothing, got {again:?}"
    );
}

/// RULE: the grant is read against the QUERY-RESULT ARN, so a key scoped to exactly that ARN
/// serves the account's queries — the gate is about the resource the key was associated with
/// and not about CloudWatch Logs holding the key at all.
#[tokio::test]
async fn a_key_scoped_to_the_query_result_arn_serves_the_queries() {
    let case = "a key policy scoped to the query-result ARN";
    let aws = Emulator::new();
    let window = group_with_events(&aws, case).await;
    let scoped = key_policy(
        &format!("logs.{REGION}.amazonaws.com"),
        Some(json!({
            "ArnEquals": {"kms:EncryptionContext:aws:logs:arn": query_results_arn()}
        })),
    );
    let key = aws.created_key(case, &scoped).await;
    aws.logs_ok(
        case,
        "AssociateKmsKey",
        &json!({"resourceIdentifier": query_results_arn(), "kmsKeyId": key}),
    )
    .await;

    let (_, served) = queried_messages(&aws, case, window).await;
    assert_eq!(
        served,
        MESSAGES.map(str::to_string).to_vec(),
        "case {case:?}: a key scoped to the query results serves them, got {served:?}"
    );
}
