// SPDX-License-Identifier: CC0-1.0

//! The Lambda side of an S3 Batch Operations `LambdaInvoke` job: the payload one
//! task is handed to the function, and the per-task verdict the function's answer
//! carries back.
//!
//! `JobOperation$LambdaInvoke` "directs the specified job to invoke an Lambda
//! function on every object in the manifest", so one task IS one invocation of
//! the function: the job hands it the object the task acts on, and the function
//! answers whether it carried that task out. Both documents are the ones the
//! invocation schema the job named declares —
//! `LambdaInvokeOperation$InvocationSchemaVersion` ("version `1.0` is the
//! default", version `2.0` "required when you use Batch Operations to invoke
//! Lambda functions that act on directory buckets, or if you need to specify
//! `UserArguments`", whose function code "uses bucket name rather than bucket
//! ARN").
//!
//! Held apart from the worker that runs the job because everything here is a
//! pure function of one manifest row, the job's operation and the bytes the
//! function answered: nothing here invokes anything or touches the store.

use super::*;

use crate::ops::types::LambdaInvokeOperation;

use super::batch_job_plan::ManifestEntry;

/// The `InvocationSchemaVersion` a job that names none is invoked under:
/// "version `1.0` is the default".
pub const INVOCATION_SCHEMA_DEFAULT: &str = "1.0";

/// The `InvocationSchemaVersion` whose payload carries the job's
/// `UserArguments` and names each task's bucket by NAME rather than by ARN.
pub const INVOCATION_SCHEMA_WITH_ARGUMENTS: &str = "2.0";

/// The `resultCode` of a task the function carried out.
pub const RESULT_SUCCEEDED: &str = "Succeeded";

/// The `resultCode` of a task the function could not carry out this time.
pub const RESULT_TEMPORARY_FAILURE: &str = "TemporaryFailure";

/// The `resultCode` of a task the function will never carry out.
pub const RESULT_PERMANENT_FAILURE: &str = "PermanentFailure";

/// Every `resultCode` a function's answer may report for one task.
pub const RESULT_CODES: [&str; 3] = [
    RESULT_SUCCEEDED,
    RESULT_TEMPORARY_FAILURE,
    RESULT_PERMANENT_FAILURE,
];

/// How many bytes of digest one opaque identifier is minted from — enough that
/// two tasks of one job never share an identifier.
const TOKEN_BYTES: usize = 24;

/// The characters of an object key that do NOT travel literally in the payload's
/// `s3Key`.
///
/// The payload carries the key URL-ENCODED: the handler AWS publishes for this
/// action reads it as `urllib.parse.unquote_plus(task['s3Key'])`, so the value
/// is one such a decode returns the key from. Encoded here over everything
/// outside RFC 3986's unreserved set, with the key's own `/` separator left
/// alone — so a key that needs no encoding travels as itself, and a handler that
/// decodes and one that uses the value as it stands both address the object the
/// manifest named.
const KEY_RESERVED: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

/// One opaque identifier of an invocation (`invocationId`) or of a task
/// (`taskId`) — a value the function echoes back rather than parses.
///
/// The bytes come from the crate's one minting recipe
/// ([`super::minted_digest`]), keyed by `sequence` — the position of the task in
/// the job's object list — so two identifiers minted in the same nanosecond
/// still differ. This names no resource and is no credential.
#[must_use]
pub fn opaque_token(sequence: u64) -> String {
    let digest = minted_digest(TOKEN_LABEL, sequence, minting_nanos());
    ::aws_smithy_types::base64::encode(&digest[..TOKEN_BYTES])
}

/// What [`opaque_token`] labels its digest with, so an identifier of a batch
/// job's invocation is independent of every other identifier this crate mints.
const TOKEN_LABEL: &str = "batch-operations-invocation";

/// The `InvocationSchemaVersion` `invoke` is carried out under.
#[must_use]
pub fn schema_version(invoke: &LambdaInvokeOperation) -> &str {
    invoke
        .invocation_schema_version
        .as_deref()
        .filter(|version| !version.is_empty())
        .unwrap_or(INVOCATION_SCHEMA_DEFAULT)
}

/// The document one task of the job `job_id` is invoked with: the invocation's
/// own id, the job it belongs to and the single object the task acts on.
///
/// One invocation carries ONE task, because the function is invoked "on every
/// object in the manifest" and each object's outcome is the function's own
/// verdict on that object.
#[must_use]
pub fn invocation_payload(
    invoke: &LambdaInvokeOperation,
    job_id: &str,
    invocation_id: &str,
    task_id: &str,
    entry: &ManifestEntry,
) -> Vec<u8> {
    let schema = schema_version(invoke);
    let names_bucket = schema == INVOCATION_SCHEMA_WITH_ARGUMENTS;

    let mut job = ::serde_json::Map::new();
    job.insert("id".to_string(), ::serde_json::Value::String(job_id.to_string()));
    // "Key-value pairs that are passed in the payload that Batch Operations
    // sends when invoking an Lambda function. You must specify
    // `InvocationSchemaVersion` 2.0 for `LambdaInvoke` operations that include
    // `UserArguments`" — so they travel only under that schema, and a job that
    // named none adds nothing to the document.
    if names_bucket {
        if let Some(arguments) = invoke.user_arguments.as_ref().filter(|a| !a.is_empty()) {
            let mut written: Vec<(&String, &String)> = arguments.iter().collect();
            written.sort_by(|left, right| left.0.cmp(right.0));
            job.insert(
                "userArguments".to_string(),
                ::serde_json::Value::Object(
                    written
                        .into_iter()
                        .map(|(name, value)| {
                            (name.clone(), ::serde_json::Value::String(value.clone()))
                        })
                        .collect(),
                ),
            );
        }
    }

    let mut task = ::serde_json::Map::new();
    task.insert(
        "taskId".to_string(),
        ::serde_json::Value::String(task_id.to_string()),
    );
    task.insert(
        "s3Key".to_string(),
        ::serde_json::Value::String(encoded_key(&entry.key)),
    );
    task.insert(
        "s3VersionId".to_string(),
        entry
            .version_id
            .clone()
            .map_or(::serde_json::Value::Null, ::serde_json::Value::String),
    );
    if names_bucket {
        task.insert(
            "s3Bucket".to_string(),
            ::serde_json::Value::String(entry.bucket.clone()),
        );
    } else {
        task.insert(
            "s3BucketArn".to_string(),
            ::serde_json::Value::String(format!("arn:aws:{S3_ARN_NAMESPACE}:::{}", entry.bucket)),
        );
    }

    ::serde_json::json!({
        "invocationSchemaVersion": schema,
        "invocationId": invocation_id,
        "job": job,
        "tasks": [task],
    })
    .to_string()
    .into_bytes()
}

/// The payload's `s3Key` for the object key `key`: percent-encoded over
/// [`KEY_RESERVED`], so a key carrying a space, a `+` or a `#` names the object
/// it is rather than a different one.
#[must_use]
pub fn encoded_key(key: &str) -> String {
    ::percent_encoding::utf8_percent_encode(key, KEY_RESERVED).to_string()
}

/// What a function's answer says about one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResult {
    /// One of [`RESULT_CODES`].
    pub code: String,
    /// The `resultString` the function wrote beside that code, empty when it
    /// wrote none.
    pub message: String,
}

impl TaskResult {
    /// Whether the function carried the task out.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.code == RESULT_SUCCEEDED
    }
}

/// The verdict `payload` reports for the task `task_id`, or the reason that
/// answer reports none.
///
/// The answer's `results` carries one entry per task, keyed by the `taskId` the
/// invocation handed over; a function that reported no entry for the task
/// answered `treatMissingKeysAs` for it when it named one, and otherwise
/// reported nothing this job can account the task by.
pub fn task_result(task_id: &str, payload: &[u8]) -> Result<TaskResult, String> {
    let answer: ::serde_json::Value = ::serde_json::from_slice(payload).map_err(|error| {
        format!("The function's answer is not the JSON document a LambdaInvoke task answers: {error}")
    })?;
    let results = answer
        .get("results")
        .and_then(::serde_json::Value::as_array)
        .ok_or_else(|| "The function's answer carries no results for the task.".to_string())?;
    let named = results.iter().find(|result| {
        result.get("taskId").and_then(::serde_json::Value::as_str) == Some(task_id)
    });
    let (code, message) = match named {
        Some(result) => (
            result
                .get("resultCode")
                .and_then(::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            result
                .get("resultString")
                .and_then(::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        None => (
            answer
                .get("treatMissingKeysAs")
                .and_then(::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            String::new(),
        ),
    };
    if !RESULT_CODES.contains(&code.as_str()) {
        return Err(format!(
            "The function's answer reports no result code for the task; the codes a task is \
             answered with are {}.",
            RESULT_CODES.join(", ")
        ));
    }
    Ok(TaskResult { code, message })
}

/// What a function that RAISED says about it: the `errorMessage` of the error
/// document every runtime answers with, or the bytes themselves when they are
/// not that document.
#[must_use]
pub fn error_document_message(payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload).into_owned();
    ::serde_json::from_str::<::serde_json::Value>(&text)
        .ok()
        .as_ref()
        .and_then(|document| document.get("errorMessage"))
        .and_then(::serde_json::Value::as_str)
        .map_or(text, str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One manifest row the payload is rendered for.
    fn entry(bucket: &str, key: &str, version_id: Option<&str>) -> ManifestEntry {
        ManifestEntry {
            bucket: bucket.to_string(),
            key: key.to_string(),
            version_id: version_id.map(str::to_string),
        }
    }

    /// The payload one task is handed names the job, the invocation, the task
    /// and the object — addressing the bucket by ARN under the default schema
    /// and by NAME under the schema that carries the job's user arguments.
    #[test]
    fn the_invocation_payload_follows_the_schema_version_the_job_named() {
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("the schema a job naming none is invoked under", None, false),
            ("the default schema, named", Some("1.0"), false),
            ("the schema that carries user arguments", Some("2.0"), true),
        ];
        for (case, version, names_bucket) in cases {
            let mut invoke = LambdaInvokeOperation {
                function_arn: Some("arn:aws:lambda:us-east-1:123456789012:function:tagger".into()),
                invocation_schema_version: version.map(str::to_string),
                user_arguments: None,
            };
            invoke.user_arguments = Some(
                [("stage".to_string(), "archived".to_string())]
                    .into_iter()
                    .collect(),
            );
            let rendered = invocation_payload(
                &invoke,
                "job-7",
                "invocation-7",
                "task-7",
                &entry("reports", "finance/q1 report.csv", Some("v2")),
            );
            let document: ::serde_json::Value =
                ::serde_json::from_slice(&rendered).unwrap_or_else(|error| {
                    panic!("case {case:?}: the payload is JSON: {error}")
                });
            assert_eq!(
                document["invocationSchemaVersion"].as_str(),
                Some(version.unwrap_or(INVOCATION_SCHEMA_DEFAULT)),
                "case {case:?}: expected the schema the job named, got {document}"
            );
            assert_eq!(
                document["job"]["id"].as_str(),
                Some("job-7"),
                "case {case:?}: expected the payload to name the job, got {document}"
            );
            let task = &document["tasks"][0];
            assert_eq!(
                task["taskId"].as_str(),
                Some("task-7"),
                "case {case:?}: expected the payload to name the task, got {document}"
            );
            assert_eq!(
                task["s3Key"].as_str(),
                Some("finance/q1%20report.csv"),
                "case {case:?}: expected the key encoded over the reserved characters, got \
                 {document}"
            );
            assert_eq!(
                task["s3VersionId"].as_str(),
                Some("v2"),
                "case {case:?}: expected the version the row named, got {document}"
            );
            if *names_bucket {
                assert_eq!(
                    task["s3Bucket"].as_str(),
                    Some("reports"),
                    "case {case:?}: expected the bucket named by name, got {document}"
                );
                assert_eq!(
                    document["job"]["userArguments"]["stage"].as_str(),
                    Some("archived"),
                    "case {case:?}: expected the job's user arguments, got {document}"
                );
            } else {
                assert_eq!(
                    task["s3BucketArn"].as_str(),
                    Some("arn:aws:s3:::reports"),
                    "case {case:?}: expected the bucket named by ARN, got {document}"
                );
                assert!(
                    document["job"].get("userArguments").is_none(),
                    "case {case:?}: user arguments travel only under the schema that carries \
                     them, got {document}"
                );
            }
        }
    }

    /// A key travels in `s3Key` as a value `unquote_plus` returns the key from:
    /// the unreserved characters and the `/` separator as themselves, everything
    /// else percent-encoded.
    #[test]
    fn a_keys_reserved_characters_are_encoded_and_the_rest_travel_as_themselves() {
        let cases: &[(&str, &str, &str)] = &[
            ("a key needing no encoding", "obj1.txt", "obj1.txt"),
            (
                "the unreserved set and the separator",
                "a-b_c.d~e/f",
                "a-b_c.d~e/f",
            ),
            ("a space", "q1 report.csv", "q1%20report.csv"),
            ("a plus", "a+b.csv", "a%2Bb.csv"),
            ("a fragment marker", "a#b.csv", "a%23b.csv"),
            ("a percent", "a%b.csv", "a%25b.csv"),
        ];
        for (case, key, expected) in cases {
            let encoded = encoded_key(key);
            assert_eq!(
                encoded, *expected,
                "case {case:?}: expected {key:?} to travel as {expected:?}, got {encoded:?}"
            );
        }
    }

    /// One row of the verdict table: the case name, the document the function
    /// answered, and the `(resultCode, resultString)` the task must be
    /// accounted by — `None` for an answer that reports no verdict at all.
    type VerdictCase = (&'static str, &'static str, Option<(&'static str, &'static str)>);

    /// A task's verdict is the entry of `results` keyed by its own `taskId`;
    /// `treatMissingKeysAs` answers a task no entry names, and an answer that
    /// reports neither is one no task can be accounted by.
    #[test]
    fn a_tasks_verdict_is_read_from_the_result_its_task_id_keys() {
        let cases: &[VerdictCase] = &[
            (
                "the task's own result",
                r#"{"results":[{"taskId":"mine","resultCode":"Succeeded","resultString":"ok"}]}"#,
                Some(("Succeeded", "ok")),
            ),
            (
                "the task's result among several",
                r#"{"results":[{"taskId":"other","resultCode":"Succeeded"},
                   {"taskId":"mine","resultCode":"PermanentFailure","resultString":"no"}]}"#,
                Some(("PermanentFailure", "no")),
            ),
            (
                "a result carrying no message",
                r#"{"results":[{"taskId":"mine","resultCode":"TemporaryFailure"}]}"#,
                Some(("TemporaryFailure", "")),
            ),
            (
                "a missing result the answer says how to treat",
                r#"{"treatMissingKeysAs":"PermanentFailure","results":[]}"#,
                Some(("PermanentFailure", "")),
            ),
            (
                "a missing result the answer says nothing about",
                r#"{"results":[{"taskId":"other","resultCode":"Succeeded"}]}"#,
                None,
            ),
            (
                "a result code no task is answered with",
                r#"{"results":[{"taskId":"mine","resultCode":"Maybe"}]}"#,
                None,
            ),
            ("an answer that is no document at all", "not json", None),
            (
                "an answer carrying no results",
                r#"{"invocationSchemaVersion":"1.0"}"#,
                None,
            ),
        ];
        for (case, answered, expected) in cases {
            let read = task_result("mine", answered.as_bytes());
            match expected {
                Some((code, message)) => assert_eq!(
                    read.as_ref()
                        .map(|result| (result.code.as_str(), result.message.as_str()))
                        .ok(),
                    Some((*code, *message)),
                    "case {case:?}: expected {code}/{message:?}, got {read:?}"
                ),
                None => assert!(
                    read.is_err(),
                    "case {case:?}: expected no verdict to be read, got {read:?}"
                ),
            }
        }
    }

    /// What a raising function says is the `errorMessage` of its error document,
    /// and the bytes themselves for an answer that is not one.
    #[test]
    fn a_raising_functions_message_is_read_off_its_error_document() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "an error document",
                r#"{"errorType":"KeyError","errorMessage":"'tasks'"}"#,
                "'tasks'",
            ),
            ("bytes that are no document", "boom", "boom"),
            (
                "a document carrying no message",
                r#"{"errorType":"KeyError"}"#,
                r#"{"errorType":"KeyError"}"#,
            ),
        ];
        for (case, answered, expected) in cases {
            let read = error_document_message(answered.as_bytes());
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// Two identifiers minted for one job's tasks differ, and each is the base64
    /// a function echoes back.
    #[test]
    fn minted_identifiers_are_opaque_and_distinct() {
        let minted: Vec<String> = (0..8).map(opaque_token).collect();
        for (at, token) in minted.iter().enumerate() {
            assert!(
                ::aws_smithy_types::base64::decode(token).is_ok(),
                "task {at}: expected an opaque base64 identifier, got {token:?}"
            );
            assert!(
                minted.iter().filter(|other| *other == token).count() == 1,
                "task {at}: expected the identifier {token:?} to be minted once, got {minted:?}"
            );
        }
    }
}
