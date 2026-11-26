// SPDX-License-Identifier: CC0-1.0

//! What an ARMED Lambda notification configuration DOES when an object is written: the
//! configured function really RUNS on the event, and only for a key every rule of the
//! configuration's key-name `Filter` admits.
//!
//! RULE: a bucket whose notification configuration names a Lambda function for an event class
//! publishes each matching object change to that function as one asynchronous invocation, and a
//! key-name `Filter` rule is matched LITERALLY — "the Amazon S3 API supports filtering based on
//! the prefix and suffix of the object key name" (`NotificationConfigurationFilter`), with no
//! case folding — so a key that differs from a rule's value only in case is admitted by no rule
//! and reaches the function not at all.
//!
//! Both halves are asserted of ONE configuration and ONE function, with every suppressed key
//! written BEFORE the admitted ones: an admitted key's conversion appearing is the barrier that
//! proves the whole pipeline ran, so each suppressed key's absence is an absence rather than
//! lateness. What the handler does is observed the way a client observes it — it reads the
//! object the event named, converts the CSV it holds and writes the result back through the AWS
//! SDK its runtime supplies, and the converted object is read back through S3 — so nothing here
//! inspects a handler's internals, and a delivery that is merely QUEUED does not satisfy it.
//!
//! The rows differ along what the rule generalises over: which rule of the filter the key fails
//! (the prefix, the suffix, both), whether it fails only by the CASE of the characters the
//! rule's value spells, how deep under the prefix the key sits, and whether the write is the
//! first of the run or one made after earlier deliveries were already carried out.
//!
//! Amazon S3, Lambda, IAM and the token service are mounted on ONE engine, exactly as
//! `core/router/src/lib.rs` mounts them: S3's delivery is dispatched back into this same
//! emulator, and so is the handler's own SDK call, so the bucket this suite reads is the bucket
//! the handler wrote.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an S3 request payload.
pub(crate) const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// Where `CreateFunction` is bound, and the module and attribute the package carries its
/// handler as.
pub(crate) const FUNCTIONS_URI: &str = "/2015-03-31/functions";
pub(crate) const HANDLER: &str = "lambda_function.lambda_handler";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination.
pub(crate) const S3_PRINCIPAL: &str = "s3.amazonaws.com";
pub(crate) const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The bucket, the function and the execution role this suite drives.
const BUCKET: &str = "armed-notification-conversion";
const FUNCTION: &str = "convert-uploaded-csv";
const ROLE: &str = "convert-uploaded-csv-role";

/// The key-name filter the configuration carries: the two rule kinds `FilterRuleName` publishes
/// (`smithy_query(query="shape",shape="FilterRuleName")` reports `[prefix, suffix]`).
pub(crate) const FILTER_PREFIX: &str = "raw/";
pub(crate) const FILTER_SUFFIX: &str = ".csv";

/// Where the handler writes what it converted, which no rule of the filter admits — so the
/// handler's own write starts no second delivery.
pub(crate) const CONVERTED_PREFIX: &str = "converted/";

/// The CSV every object this suite writes holds, so a delivery that happened always converts:
/// an absence is then the delivery S3 did not make and never a handler that failed.
pub(crate) const CSV_BODY: &str = "id,total\n1,42\n2,7\n";

/// What that CSV converts to, which is what the handler must have written.
pub(crate) fn converted_document() -> Value {
    json!([
        {"id": "1", "total": "42"},
        {"id": "2", "total": "7"},
    ])
}

/// How long a wait for work the CLOCK owes lasts before giving up — the function's provisioning
/// window, and the asynchronous delivery of an object event — and how often it looks while
/// waiting. Generous: nothing under test depends on the span, and the suite waits for the
/// CONDITION rather than for a time to pass.
pub(crate) const BUDGET: Duration = Duration::from_secs(90);
pub(crate) const LOOK: Duration = Duration::from_millis(50);

/// The handler the function runs: for each record it is handed it reads the object the event
/// named, parses the CSV it holds and writes the rows back as JSON under
/// [`CONVERTED_PREFIX`] — through the AWS SDK the runtime supplies, so the write is a real
/// Amazon S3 call made with the function's own execution-role credentials.
///
/// It accepts a `.CSV` key as readily as a `.csv` one (`key.lower().endswith`), which is what
/// makes the suppression legs meaningful: a key the S3-side filter admits is converted, and a
/// key only the handler would have accepted is never handed to it.
pub(crate) const CONVERTING_HANDLER: &str = r#"
import csv
import io
import json

import boto3


def lambda_handler(event, context):
    client = boto3.client("s3")
    converted = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = record["s3"]["object"]["key"]
        if not key.lower().endswith(".csv"):
            continue
        body = client.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        rows = list(csv.DictReader(io.StringIO(body)))
        target = "converted/" + key.rsplit("/", 1)[-1].rsplit(".", 1)[0] + ".json"
        client.put_object(
            Bucket=bucket,
            Key=target,
            Body=json.dumps(rows).encode("utf-8"),
        )
        converted.append(target)
    return {"converted": converted}
"#;

/// Amazon S3, Lambda, IAM and the token service over ONE shared engine.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

/// What a dispatched request answered.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: String,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id. The
        // handler's OWN call is signed by the runtime, with the credential it was given.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
    /// request. The signature is never checked: request authentication is off.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        )
    }

    /// Dispatch one request, signed for `signing_name` so the router resolves the service the
    /// way it does in production.
    pub(crate) async fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Answer {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("authorization", Self::authorization(signing_name))
            .header("content-type", content_type)
            .body(bytes::Bytes::from(body))
            .expect("the test request must build");
        // Boxed: this router carries four services, and the dispatch of a handler's own call
        // nests inside the dispatch that started it, so the futures go on the heap rather than
        // on the test thread's stack.
        let response = Box::pin(self.router.dispatch(request)).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    /// One Amazon S3 request, path style.
    pub(crate) async fn s3(&self, method: &str, uri: &str, body: &[u8]) -> Answer {
        self.send("s3", method, uri, "application/octet-stream", body.to_vec())
            .await
    }

    /// One Amazon S3 request that must be served.
    pub(crate) async fn s3_ok(&self, case: &str, method: &str, uri: &str, body: &[u8]) {
        let answer = self.s3(method, uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {case:?}: S3 {method} {uri} must succeed, got {} with {}",
            answer.status,
            answer.body
        );
    }

    /// One Lambda `restJson1` call.
    pub(crate) async fn lambda(&self, method: &str, uri: &str, body: Vec<u8>) -> Answer {
        self.send("lambda", method, uri, "application/json", body)
            .await
    }

    /// One awsQuery call of `signing_name` that must be served.
    pub(crate) async fn query(
        &self,
        case: &str,
        signing_name: &str,
        action: &str,
        params: &[(&str, String)],
    ) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answer = self
            .send(
                signing_name,
                "POST",
                "/",
                "application/x-www-form-urlencoded",
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: {signing_name} {action} must succeed, got {} with {}",
            answer.status, answer.body
        );
    }
}

/// `application/x-www-form-urlencoded` for one awsQuery parameter value, leaving only RFC
/// 3986's unreserved characters literal.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The container that makes the pipeline LIVE: one Lambda configuration naming `function_arn`
/// for `events`, filtered to [`FILTER_PREFIX`] and [`FILTER_SUFFIX`]. The Lambda list is bound
/// as `CloudFunctionConfiguration` and its ARN member as `CloudFunction`
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`).
///
/// `pub(crate)`: the ONE spelling of this container in the crate, so a suite that varies the
/// event class ([`super::notification_pipeline_dismantling`]) varies that and nothing else.
pub(crate) fn lambda_configuration(function_arn: &str, events: &str) -> String {
    format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\"><CloudFunctionConfiguration>\
         <Id>convert</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{events}</Event><Filter><S3Key>\
         <FilterRule><Name>prefix</Name><Value>{FILTER_PREFIX}</Value></FilterRule>\
         <FilterRule><Name>suffix</Name><Value>{FILTER_SUFFIX}</Value></FilterRule>\
         </S3Key></Filter></CloudFunctionConfiguration></NotificationConfiguration>"
    )
}

/// The EMPTY container, which is what turns notifications off for the bucket: "you can disable
/// notifications by adding the empty NotificationConfiguration element"
/// (`PutBucketNotificationConfiguration`'s own documentation).
///
/// `pub(crate)`: beside [`lambda_configuration`], so the suites of this family spell the
/// container one way.
pub(crate) fn cleared_configuration() -> String {
    format!("<NotificationConfiguration xmlns=\"{XMLNS}\"></NotificationConfiguration>")
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
pub(crate) fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// One row of the filter table: the key written, and whether the configuration's filter admits
/// it.
struct Row {
    case: &'static str,
    key: &'static str,
    admitted: bool,
}

/// The table. The rows the filter admits — among them a deeply nested key and keys written
/// after earlier deliveries have already been made — and one per way a key can fail the filter,
/// among them the ones that differ from a rule's value only in CASE.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key both rules admit",
            key: "raw/report.csv",
            admitted: true,
        },
        Row {
            case: "a key deep under the prefix",
            key: "raw/2026/11/15/deep.csv",
            admitted: true,
        },
        Row {
            case: "a key whose stem is upper case but whose suffix is not",
            key: "raw/LEDGER.csv",
            admitted: true,
        },
        Row {
            case: "a key written after earlier deliveries were made",
            key: "raw/fresh.csv",
            admitted: true,
        },
        Row {
            case: "the suffix spelled in another case",
            key: "raw/upper.CSV",
            admitted: false,
        },
        Row {
            case: "the suffix spelled in mixed case",
            key: "raw/mixed.Csv",
            admitted: false,
        },
        Row {
            case: "a suffix the rule does not name at all",
            key: "raw/notes.txt",
            admitted: false,
        },
        Row {
            case: "the prefix spelled in another case",
            key: "RAW/cased.csv",
            admitted: false,
        },
        Row {
            case: "a key under another prefix",
            key: "staging/plain.csv",
            admitted: false,
        },
        Row {
            case: "a key failing both rules",
            key: "staging/other.CSV",
            admitted: false,
        },
    ]
}

/// Where the handler writes the conversion of `key`: the last element of the key, with its
/// extension replaced, under [`CONVERTED_PREFIX`]. One target per row, so no row's absence can
/// be satisfied by another row's delivery.
///
/// `pub(crate)`: the ONE reading of the key the converting handler derives, so a suite driving
/// that handler ([`super::notification_pipeline_dismantling`]) cannot drift from what it writes.
pub(crate) fn converted_key(key: &str) -> String {
    let name = key.rsplit('/').next().unwrap_or(key);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    format!("{CONVERTED_PREFIX}{stem}.json")
}

#[tokio::test]
async fn an_armed_lambda_notification_runs_its_handler_only_for_a_key_its_filter_admits() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();
    let function_arn = format!("arn:aws:lambda:{REGION}:{account}:function:{FUNCTION}");

    // The execution role the handler's own Amazon S3 calls are made as, written with IAM's own
    // operations: the read of the object the event named and the write of the conversion are
    // judged against the policies really in force on it.
    emulator
        .query(
            "setup",
            "iam",
            "CreateRole",
            &[
                ("RoleName", ROLE.to_string()),
                (
                    "AssumeRolePolicyDocument",
                    json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    })
                    .to_string(),
                ),
            ],
        )
        .await;
    emulator
        .query(
            "setup",
            "iam",
            "PutRolePolicy",
            &[
                ("RoleName", ROLE.to_string()),
                ("PolicyName", "convert".to_string()),
                (
                    "PolicyDocument",
                    json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["s3:GetObject", "s3:PutObject"],
                            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                        }],
                    })
                    .to_string(),
                ),
            ],
        )
        .await;

    // The function, whose deployment package really holds the handler above.
    let created = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            serde_json::to_vec(&json!({
                "FunctionName": FUNCTION,
                "Role": format!("arn:aws:iam::{account}:role/{ROLE}"),
                "Runtime": "python3.12",
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(CONVERTING_HANDLER)),
                },
            }))
            .expect("the create body serializes"),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case \"setup\": CreateFunction must succeed, got {} with {}",
        created.status, created.body
    );

    // An invocation needs a function in `Active`, which the clock owes: waited for by LOOKING
    // rather than by sleeping a span of this suite's own choosing.
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = emulator
            .lambda("GET", &format!("{FUNCTIONS_URI}/{FUNCTION}"), Vec::new())
            .await;
        assert_eq!(
            read.status, 200,
            "case \"setup\": GetFunction must succeed, got {} with {}",
            read.status, read.body
        );
        let reported: Value = serde_json::from_str(&read.body).unwrap_or(Value::Null);
        let state = reported["Configuration"]["State"].as_str().unwrap_or("");
        if state == "Active" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"setup\": the function never reached Active, last reported {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }

    // The grant S3 reads before it records the configuration: "in the case of Lambda
    // destinations, Amazon S3 verifies that the Lambda function permissions grant Amazon S3
    // permission to invoke the function from the Amazon S3 bucket"
    // (`PutBucketNotificationConfiguration`'s own documentation).
    let granted = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
            serde_json::to_vec(&json!({
                "StatementId": "s3-invoke",
                "Action": INVOKE_ACTION,
                "Principal": S3_PRINCIPAL,
                "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
            }))
            .expect("the permission body serializes"),
        )
        .await;
    assert_eq!(
        granted.status, 201,
        "case \"setup\": AddPermission must succeed, got {} with {}",
        granted.status, granted.body
    );

    emulator
        .s3_ok("setup", "PUT", &format!("/{BUCKET}"), &[])
        .await;

    // The configuration: the Lambda list is bound as `CloudFunctionConfiguration`, whose ARN
    // member is bound as `CloudFunction`
    // (`smithy_query(query="shape",shape="NotificationConfiguration")`).
    let configuration = lambda_configuration(&function_arn, "s3:ObjectCreated:*");
    emulator
        .s3_ok(
            "setup",
            "PUT",
            &format!("/{BUCKET}?notification"),
            configuration.as_bytes(),
        )
        .await;

    // Every suppressed key FIRST, then the admitted ones in table order: the conversions of the
    // admitted keys are the barrier that proves the pipeline ran, so each absence below is an
    // absence. The admitted keys are written one after another, so what is pinned is also that
    // the pipeline stays live: a later write is delivered as readily as the first.
    let rows = rows();
    for row in rows.iter().filter(|row| !row.admitted) {
        emulator
            .s3_ok(
                row.case,
                "PUT",
                &format!("/{BUCKET}/{}", row.key),
                CSV_BODY.as_bytes(),
            )
            .await;
    }
    let admitted: Vec<&Row> = rows.iter().filter(|row| row.admitted).collect();
    assert!(
        !admitted.is_empty(),
        "case \"the table\": it must carry the barrier rows"
    );
    for row in &admitted {
        emulator
            .s3_ok(
                row.case,
                "PUT",
                &format!("/{BUCKET}/{}", row.key),
                CSV_BODY.as_bytes(),
            )
            .await;
    }

    // The barrier: the handler ran on each admitted key's event and wrote what it converted.
    for row in &admitted {
        let target = converted_key(row.key);
        let deadline = Instant::now() + BUDGET;
        let served = loop {
            let answer = emulator.s3("GET", &format!("/{BUCKET}/{target}"), &[]).await;
            if answer.status == 200 {
                break answer.body;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: the armed notification never converted {:?}: nothing was written \
                 to {target:?} (last read answered {} with {})",
                row.case,
                row.key,
                answer.status,
                answer.body
            );
            tokio::time::sleep(LOOK).await;
        };
        let document: Value = serde_json::from_str(&served).unwrap_or_else(|error| {
            panic!(
                "case {:?}: expected the handler's converted document, got {error} for \
                 {served:?}",
                row.case
            )
        });
        assert_eq!(
            document,
            converted_document(),
            "case {:?}: expected the CSV converted row for row, got {document}",
            row.case
        );
    }

    // And every key the filter excludes reached the function not at all.
    for row in rows.iter().filter(|row| !row.admitted) {
        let target = converted_key(row.key);
        let answer = emulator.s3("GET", &format!("/{BUCKET}/{target}"), &[]).await;
        assert_eq!(
            answer.status, 404,
            "case {:?}: {:?} passes no rule of the filter, so nothing may be written to \
             {target:?}; the read answered {} with {}",
            row.case, row.key, answer.status, answer.body
        );
    }
}
