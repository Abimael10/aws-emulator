// SPDX-License-Identifier: CC0-1.0

//! The Amazon RDS calls a RUNNING handler makes through the AWS SDK this runtime supplies,
//! end to end over the wire: Lambda and Amazon RDS on ONE engine, a `python3.x` function whose
//! code does `import boto3` and drives a database's Amazon S3 export, and the SAME calls made
//! directly against that service for comparison.
//!
//! RULE: a managed runtime ships the AWS SDK on the handler's import path, so a handler can
//! call Amazon RDS with no dependency of its own, and what the call answers is that SERVICE's
//! own answer — a success carrying the operation's output members, or its modelled refusal
//! raised as `botocore.exceptions.ClientError` with the service's own error code and message.
//! The service speaks `awsQuery`, whose requests no JSON path can build, so this is what binds
//! them.
//!
//! The rows differ along what the rule generalises over: whether the call is served or refused,
//! which operation of the service it names (an export started, the export tasks read, an
//! association made), and whether the operation is one this runtime's SDK binds at all. What
//! each refusal must carry is not spelled out here: the same call is made DIRECTLY against the
//! service and the code and sentence the handler read are held against the ones that call
//! answered, so a runtime that answered something of its own fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The one percent-encoder this crate's suites build an `awsQuery` body with, exported by the
// suite that first needed it rather than written again here.
use crate::event_source_mapping_role_access::form_encode;
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region the function runs in, which its clients sign and address for.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The snapshot, bucket, key and role an exporting call names. Nothing holds them: what each
/// row asserts is that the SERVICE's own answer about them is what the handler read.
const SNAPSHOT_ARN: &str = "arn:aws:rds:us-east-1:000000000000:snapshot:emu-no-such-snapshot";
const EXPORT_BUCKET: &str = "emu-export-bucket";
const EXPORT_ROLE: &str = "arn:aws:iam::000000000000:role/emu-export-role";
const EXPORT_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/emu-key";

/// Lambda and Amazon RDS over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// handler's SDK call is dispatched back into this same router, so the service it reaches is
/// the service this suite calls directly.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// One `awsQuery` call made DIRECTLY against Amazon RDS, as its status and body — the answer
/// each row's handler-side call is held against.
async fn rds(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/rds/aws4_request, \
                 SignedHeaders=host, Signature=unverified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8(response.body().to_vec()).expect("a UTF-8 response body");
    (status, body)
}

/// The text of one element of an `awsQuery` answer.
fn element(body: &str, tag: &str) -> String {
    body.split_once(&format!("<{tag}>"))
        .and_then(|(_, rest)| rest.split_once(&format!("</{tag}>")))
        .map(|(text, _)| text.to_string())
        .unwrap_or_default()
}

/// The handler every row runs: it reports what the ONE SDK call it makes answered, so the
/// invocation's payload IS what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("rds")
    method = getattr(client, event["method"], None)
    if method is None:
        return {"ok": False, "no_such_method": event["method"]}
    try:
        answered = method(**event["parameters"])
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "not_made": str(error)}
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(key for key in answered if key != "ResponseMetadata"),
        "answered": {
            key: value for key, value in answered.items() if key != "ResponseMetadata"
        },
    }
"#;

/// `Invoke` the function with `event` and answer the payload the run produced.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    serde_json::from_str(&answered.text()).unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_handler_drives_an_rds_export_through_the_sdk_this_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let name = "rds-export-caller";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 20,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, "the exporting function", name).await;

    // 1. A SERVED read: the export tasks of an account that has none. The handler reads the
    //    operation's own output member, which is the empty list the service answered with.
    let case = "the export tasks of an account with none";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({ "method": "describe_export_tasks", "parameters": {} }),
    )
    .await;
    assert_eq!(
        payload.get("ok"),
        Some(&Value::Bool(true)),
        "case {case:?}: expected the call to be served, got {payload}"
    );
    assert_eq!(
        payload.get("status").and_then(Value::as_i64),
        Some(200),
        "case {case:?}: expected the service's own 200, got {payload}"
    );
    assert_eq!(
        payload
            .pointer("/answered/ExportTasks")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "case {case:?}: expected the empty list of export tasks, got {payload}"
    );

    // 2. A REFUSED export: the snapshot the task would export is not one this account holds.
    //    What it must be refused with is the service's own answer to the same call, read by
    //    making it directly.
    let case = "an export of a snapshot the account does not hold";
    let (status, body) = rds(
        &router,
        "StartExportTask",
        &[
            ("ExportTaskIdentifier", "emu-direct-export"),
            ("SourceArn", SNAPSHOT_ARN),
            ("S3BucketName", EXPORT_BUCKET),
            ("IamRoleArn", EXPORT_ROLE),
            ("KmsKeyId", EXPORT_KEY),
        ],
    )
    .await;
    let expected_code = element(&body, "Code");
    let expected_message = element(&body, "Message");
    assert_ne!(
        status, 200,
        "case {case:?}: the direct call must be refused, got {status} {body}"
    );
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "start_export_task",
            "parameters": {
                "ExportTaskIdentifier": "emu-handler-export",
                "SourceArn": SNAPSHOT_ARN,
                "S3BucketName": EXPORT_BUCKET,
                "IamRoleArn": EXPORT_ROLE,
                "KmsKeyId": EXPORT_KEY,
            },
        }),
    )
    .await;
    assert_eq!(
        payload.get("ok"),
        Some(&Value::Bool(false)),
        "case {case:?}: expected the service's refusal, got {payload}"
    );
    assert_eq!(
        (
            payload.get("code").and_then(Value::as_str),
            payload.get("message").and_then(Value::as_str),
            payload.get("status").and_then(Value::as_i64),
        ),
        (
            Some(expected_code.as_str()),
            Some(expected_message.as_str()),
            Some(i64::from(status)),
        ),
        "case {case:?}: expected the code, sentence and status the service answered \
         ({expected_code}, {expected_message:?}, {status}), got {payload}"
    );

    // 3. A REFUSED association, which is the other half of the integration a handler drives:
    //    the same comparison against the service's own answer.
    let case = "an association on a DB instance that is not there";
    let (status, body) = rds(
        &router,
        "AddRoleToDBInstance",
        &[
            ("DBInstanceIdentifier", "emu-no-such-instance"),
            ("RoleArn", EXPORT_ROLE),
            ("FeatureName", "s3Export"),
        ],
    )
    .await;
    let expected_code = element(&body, "Code");
    assert_ne!(
        status, 200,
        "case {case:?}: the direct call must be refused, got {status} {body}"
    );
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "add_role_to_db_instance",
            "parameters": {
                "DBInstanceIdentifier": "emu-no-such-instance",
                "RoleArn": EXPORT_ROLE,
                "FeatureName": "s3Export",
            },
        }),
    )
    .await;
    assert_eq!(
        payload.get("code").and_then(Value::as_str),
        Some(expected_code.as_str()),
        "case {case:?}: expected the code the service answered ({expected_code}), got {payload}"
    );

    // 4. An operation of the service this runtime's SDK does not bind: the handler is told the
    //    call was NOT MADE, naming the operations it does bind, rather than sent a request this
    //    seam would be guessing at.
    let case = "an operation this runtime's SDK does not bind";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "create_db_snapshot",
            "parameters": {
                "DBInstanceIdentifier": "emu-no-such-instance",
                "DBSnapshotIdentifier": "emu-no-such-snapshot",
            },
        }),
    )
    .await;
    let reported = payload
        .get("not_made")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The name the refusal reports is Amazon RDS's own model's spelling of the operation
    // (`CreateDBSnapshot`), not the recasing of the method name the SDK derived it from
    // (`create_db_snapshot`).
    assert!(
        reported.contains("CreateDBSnapshot") && reported.contains("StartExportTask"),
        "case {case:?}: expected the refusal to name the call and what is bound, got {payload}"
    );
}
