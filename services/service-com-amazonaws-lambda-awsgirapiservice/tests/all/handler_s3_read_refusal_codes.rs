// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING handler's own Amazon S3 read ANSWERS WITH, class by class, end to end over
//! the wire: Lambda, Amazon S3, IAM and the token service on ONE engine, and a `python3.x`
//! function whose code does `import boto3` and reports whatever each `GetObject` came to.
//!
//! The rule pinned here: an in-function SDK call is answered by the SERVICE that serves it, so
//! what the handler reads back is that service's own answer — a call the policies in force
//! permit is served with the object's bytes under HTTP 200, and a call that is refused carries
//! the serving service's own error code, its message and its status, never a placeholder code
//! and never an absent message. A handler branching on `e.response["Error"]["Code"]` therefore
//! sees a code Amazon S3 itself publishes, and the sentence the SDK renders for the refusal
//! names it.
//!
//! The rows differ along what the rule generalises over: WHICH error class the read falls into
//! — the read that is served, the key no bucket holds, the bucket nothing holds, the read no
//! grant covers, and the absent key a caller may not even list — so each one exercises a
//! different answer of the same call. The codes and statuses are Amazon S3's own:
//! `smithy_query(query="operation", operation="GetObject")` lists `NoSuchKey` (HTTP 404) and
//! `smithy_query(query="shape", shape="NoSuchBucket")` reports HTTP 404, while Amazon S3's
//! "Error responses" list carries `AccessDenied` with HTTP 403.

use serde_json::{json, Value};

use crate::handler_s3_object_access::{
    session_arn, Emulator, ACCESS_DENIED, ACCESS_DENIED_STATUS,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The bucket this suite reads, the key it holds and the bytes that key holds.
const BUCKET: &str = "handler-refusal-codes";
const KEY: &str = "reports/held.csv";
const BODY: &str = "id,total\n1,42\n";

/// A key the bucket does not hold, and a bucket nothing holds: the two addresses that resolve
/// to nothing.
const ABSENT_KEY: &str = "reports/absent.csv";
const ABSENT_BUCKET: &str = "handler-refusal-codes-absent";

/// The code and status Amazon S3 answers a read of a key a bucket does not hold
/// (`smithy_query(query="operation", operation="GetObject")` lists `NoSuchKey` with
/// `httpError` 404), and the sentence its error document carries.
const NO_SUCH_KEY: &str = "NoSuchKey";
const NO_SUCH_KEY_STATUS: i64 = 404;
const NO_SUCH_KEY_MESSAGE: &str = "The specified key does not exist.";

/// The same for a read addressed to a bucket nothing holds
/// (`smithy_query(query="shape", shape="NoSuchBucket")`: `httpError` 404).
const NO_SUCH_BUCKET: &str = "NoSuchBucket";
const NO_SUCH_BUCKET_STATUS: i64 = 404;
const NO_SUCH_BUCKET_MESSAGE: &str = "The specified bucket does not exist";

/// The stand-in an SDK falls back to when an answer named no code at all — not a code Amazon
/// S3 publishes, so no answer of a served call may ever report it.
const PLACEHOLDER_CODE: &str = "Unknown";

/// The handler every row runs: ONE `GetObject` per address the event names, reporting what the
/// SDK gave it — the bytes for a read that was served, and for a refusal the error member as it
/// stands, read with `.get(...)` so a code or a message the SDK did not fill in is reported as
/// absent rather than raising inside the handler.
const READER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("s3")
    results = []
    for address in event["reads"]:
        try:
            answered = client.get_object(Bucket=address["bucket"], Key=address["key"])
        except botocore.exceptions.ClientError as error:
            refusal = error.response.get("Error") or {}
            metadata = error.response.get("ResponseMetadata") or {}
            results.append({
                "read": False,
                "code": refusal.get("Code"),
                "message": refusal.get("Message"),
                "status": metadata.get("HTTPStatusCode"),
                "sentence": str(error),
            })
            continue
        results.append({
            "read": True,
            "body": answered["Body"].read().decode("utf-8"),
            "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        })
    return {"results": results}
"#;

/// What one read must come to: the bytes, or the refusal Amazon S3 words for it.
enum Answer {
    Served,
    Refused {
        code: &'static str,
        status: i64,
        message: String,
    },
}

/// One row: the address it reads and what that read must answer with.
struct Row {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    answer: Answer,
}

/// The rows that share ONE identity policy on the execution role, read in one invocation: the
/// role's policies are the FUNCTION's, so what varies within a group is the address read and
/// what varies between groups is the grant in force.
struct Group {
    /// What the grant in force over this group's reads is, named so a failure in writing it
    /// or in the invocation that reads under it says which grant it was.
    case: &'static str,
    /// The identity policy the execution role carries for this group's reads, if any.
    identity: Option<String>,
    rows: Vec<Row>,
}

/// An identity policy allowing `actions` on `resources`, the form a role's own policy takes.
fn identity_policy(actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// The sentence Amazon S3 words a refusal of `action` on `resource` to the function's own role
/// session in, when no document in force allows it.
fn no_grant(role_name: &str, function_name: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: \"{resource}\" because \
         no identity-based policy allows the {action} action",
        session_arn(role_name, function_name)
    )
}

/// The groups: the grant in force, and the addresses read under it.
fn groups(role_name: &str, function_name: &str) -> Vec<Group> {
    let object = format!("arn:aws:s3:::{BUCKET}/{KEY}");
    let bucket_arn = format!("arn:aws:s3:::{BUCKET}");
    vec![
        // Every read of every bucket of this account is granted, listing included, so each row
        // of this group is about what the ADDRESS resolves to and nothing else.
        Group {
            case: "every read and every listing granted",
            identity: Some(identity_policy(
                &["s3:GetObject", "s3:ListBucket"],
                &["arn:aws:s3:::*", "arn:aws:s3:::*/*"],
            )),
            rows: vec![
                Row {
                    case: "the grant covers the read and the bucket holds the key",
                    bucket: BUCKET,
                    key: KEY,
                    answer: Answer::Served,
                },
                Row {
                    case: "the grant covers the read and the bucket holds no such key",
                    bucket: BUCKET,
                    key: ABSENT_KEY,
                    answer: Answer::Refused {
                        code: NO_SUCH_KEY,
                        status: NO_SUCH_KEY_STATUS,
                        message: NO_SUCH_KEY_MESSAGE.to_string(),
                    },
                },
                Row {
                    case: "the read is addressed to a bucket nothing holds",
                    bucket: ABSENT_BUCKET,
                    key: KEY,
                    answer: Answer::Refused {
                        code: NO_SUCH_BUCKET,
                        status: NO_SUCH_BUCKET_STATUS,
                        message: NO_SUCH_BUCKET_MESSAGE.to_string(),
                    },
                },
            ],
        },
        // The object read is granted and the bucket LISTING is not: "if you don't have the
        // s3:ListBucket permission, Amazon S3 returns an HTTP status code 403 Access Denied
        // error" (`GetObject`'s own documentation), so the absent key is refused instead of
        // being reported absent — and the refusal names the action that was withheld.
        Group {
            case: "the object grant alone, without s3:ListBucket",
            identity: Some(identity_policy(
                &["s3:GetObject"],
                &["arn:aws:s3:::*/*"],
            )),
            rows: vec![
                Row {
                    case: "the object grant alone covers the key the bucket holds",
                    bucket: BUCKET,
                    key: KEY,
                    answer: Answer::Served,
                },
                Row {
                    case: "the object grant alone, for a key the bucket does not hold",
                    bucket: BUCKET,
                    key: ABSENT_KEY,
                    answer: Answer::Refused {
                        code: ACCESS_DENIED,
                        status: ACCESS_DENIED_STATUS,
                        message: no_grant(
                            role_name,
                            function_name,
                            "s3:ListBucket",
                            &bucket_arn,
                        ),
                    },
                },
            ],
        },
        // No document in force grants anything, which is the issue's negative half.
        Group {
            case: "no grant at all",
            identity: None,
            rows: vec![Row {
                case: "no policy in force grants the read",
                bucket: BUCKET,
                key: KEY,
                answer: Answer::Refused {
                    code: ACCESS_DENIED,
                    status: ACCESS_DENIED_STATUS,
                    message: no_grant(role_name, function_name, "s3:GetObject", &object),
                },
            }],
        },
    ]
}

/// One invocation per grant, each reading every address of its group: what the handler read
/// back is the answer Amazon S3 itself gave the call, down to the code, the message and the
/// status — and never the SDK's placeholder for an answer that named none.
#[tokio::test]
async fn a_handlers_own_s3_read_answers_with_amazon_s3s_own_code_and_message() {
    let _stream = one_invocation_stream_at_a_time().await;
    let role_name = "lambda-s3-refusal-reader";
    let function_name = "s3-refusal-reader";
    let case = "setup";
    let emulator = Emulator::new();
    let role = emulator.execution_role(case, role_name, None).await;
    emulator
        .function(case, function_name, &role, READER_SOURCE)
        .await;
    emulator
        .s3_ok(case, "PUT", &format!("/{BUCKET}"), &[])
        .await;
    emulator
        .s3_ok(case, "PUT", &format!("/{BUCKET}/{KEY}"), BODY.as_bytes())
        .await;

    for group in groups(role_name, function_name) {
        emulator
            .role_policy(group.case, role_name, group.identity.as_deref())
            .await;
        let reads: Vec<Value> = group
            .rows
            .iter()
            .map(|row| json!({ "bucket": row.bucket, "key": row.key }))
            .collect();
        let (signal, payload) = emulator
            .invoke(group.case, function_name, &json!({ "reads": reads }))
            .await;
        let group_case = group.case;
        assert_eq!(
            signal, None,
            "case {group_case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        let answered = payload["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            answered.len(),
            group.rows.len(),
            "case {group_case:?}: expected one answer per read of the group, got {payload}"
        );
        for (row, answer) in group.rows.iter().zip(&answered) {
            let case = row.case;
            match &row.answer {
                Answer::Served => {
                    assert_eq!(
                        answer["read"],
                        json!(true),
                        "case {case:?}: expected the read to be served, got {answer}"
                    );
                    assert_eq!(
                        answer["body"],
                        json!(BODY),
                        "case {case:?}: expected the bytes the bucket holds, got {answer}"
                    );
                    assert_eq!(
                        answer["status"],
                        json!(200),
                        "case {case:?}: expected Amazon S3's own 200, got {answer}"
                    );
                }
                Answer::Refused {
                    code,
                    status,
                    message,
                } => {
                    assert_eq!(
                        answer["read"],
                        json!(false),
                        "case {case:?}: expected the read to be refused, got {answer}"
                    );
                    // The invariant the issue was filed on, asserted ahead of the exact
                    // values: a refusal reports a code the service publishes and a message
                    // it worded, so neither the placeholder nor an absent member reaches a
                    // handler that branches on them.
                    let reported = answer["code"].as_str();
                    assert!(
                        reported.is_some_and(|code| !code.is_empty() && code != PLACEHOLDER_CODE),
                        "case {case:?}: expected Amazon S3's own error code, got {answer}"
                    );
                    assert!(
                        answer["message"]
                            .as_str()
                            .is_some_and(|message| !message.is_empty()),
                        "case {case:?}: expected the refusal's own message, got {answer}"
                    );
                    assert_eq!(
                        answer["code"],
                        json!(code),
                        "case {case:?}: expected the code {code:?}, got {answer}"
                    );
                    assert_eq!(
                        answer["message"],
                        json!(message),
                        "case {case:?}: expected the message {message:?}, got {answer}"
                    );
                    assert_eq!(
                        answer["status"],
                        json!(status),
                        "case {case:?}: expected the status {status}, got {answer}"
                    );
                    // The sentence the SDK renders for the refusal, which is what a handler
                    // logs: it carries the same code and message, not the placeholder.
                    let sentence = format!(
                        "An error occurred ({code}) when calling the GetObject operation: \
                         {message}"
                    );
                    assert_eq!(
                        answer["sentence"],
                        json!(sentence),
                        "case {case:?}: expected the sentence {sentence:?}, got {answer}"
                    );
                }
            }
        }
    }
}
