// SPDX-License-Identifier: CC0-1.0

//! The Amazon RDS CATALOGUE a RUNNING handler reads through the AWS SDK this runtime supplies,
//! end to end over the wire: Lambda and Amazon RDS on ONE engine, a `python3.x` function whose
//! code does `import boto3` and asks the service what it offers, and the SAME call made
//! directly against that service for comparison.
//!
//! RULE: an operation a handler calls through the SDK this runtime supplies is DISPATCHED to the
//! service that serves it, and what the handler reads is that service's own answer — the
//! operation's output members for a served call, or its modelled code, sentence and status for a
//! refused one. The reference data a handler reads before it acts on a database — the engine
//! versions an account can launch and the configurations each of them can be launched in — is
//! part of that surface, so a scheduled or periodic handler whose whole job is to report what
//! the service offers reads it exactly as it reads a database of its own.
//!
//! The rows differ along what the rule generalises over: which catalogue read the call names,
//! how it is narrowed (not at all, by engine, by page size, to the default version only),
//! whether the call is served or refused, and whether the operation is one this runtime's SDK
//! binds at all. What the refusal must carry is not spelled out here: the same call is made
//! DIRECTLY against the service and the code and sentence the handler read are held against the
//! ones that call answered, so a runtime that answered something of its own fails here.
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

/// An engine the service publishes versions for, and one it publishes none for.
///
/// `postgres` is one entry of the engine vocabulary this model's own members publish
/// (`smithy_query(query="doc",shape="DescribeDBEngineVersionsMessage",member="Engine")`), and is
/// not the engine the issue behind this suite named. The second value names no engine at all,
/// which is what the service refuses.
const PUBLISHED_ENGINE: &str = "postgres";
const UNPUBLISHED_ENGINE: &str = "emu-no-such-engine";

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
///
/// `pub(crate)` so a sibling suite of this one test target calls the service the same way rather
/// than growing another copy of this request.
pub(crate) async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
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
///
/// `pub(crate)` for the same reason as [`rds`]: one reader of these answers, not one per suite.
pub(crate) fn element(body: &str, tag: &str) -> String {
    body.split_once(&format!("<{tag}>"))
        .and_then(|(_, rest)| rest.split_once(&format!("</{tag}>")))
        .map(|(text, _)| text.to_string())
        .unwrap_or_default()
}

/// The handler every row runs: it reports what the ONE SDK call it makes answered, summarised
/// rather than copied out, so one invocation's payload stays small whatever the catalogue holds.
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
    rows = answered.get(event["items"], [])
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(key for key in answered if key != "ResponseMetadata"),
        "count": len(rows),
        "engines": sorted({row.get("Engine") for row in rows if row.get("Engine")}),
        "first": {name: rows[0].get(name) for name in event["read"]} if rows else {},
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

/// One row's served answer, as the count of entries it reported.
fn served_count(case: &str, payload: &Value) -> i64 {
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
    payload
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a count of entries, got {payload}"))
}

#[tokio::test]
async fn a_handler_reads_the_rds_catalogue_through_the_sdk_this_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let name = "rds-catalogue-caller";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
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
    await_function_active(&router, "the catalogue-reading function", name).await;

    // 1. The engine versions an account can launch, narrowed by nothing: the handler reads the
    //    operation's own output member, and every entry of it names an engine and a version.
    let case = "the engine versions of an account, unnarrowed";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_engine_versions",
            "parameters": {},
            "items": "DBEngineVersions",
            "read": ["Engine", "EngineVersion", "DBParameterGroupFamily"],
        }),
    )
    .await;
    let unnarrowed = served_count(case, &payload);
    assert!(
        unnarrowed > 0,
        "case {case:?}: expected the service's own engine versions, got {payload}"
    );
    assert!(
        payload
            .pointer("/first/Engine")
            .and_then(Value::as_str)
            .is_some_and(|engine| !engine.is_empty())
            && payload
                .pointer("/first/EngineVersion")
                .and_then(Value::as_str)
                .is_some_and(|version| !version.is_empty()),
        "case {case:?}: expected each entry to name an engine and a version, got {payload}"
    );

    // 2. The same read narrowed to ONE engine: every entry the handler read is that engine's.
    let case = "the engine versions of one engine";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_engine_versions",
            "parameters": { "Engine": PUBLISHED_ENGINE },
            "items": "DBEngineVersions",
            "read": ["Engine", "EngineVersion"],
        }),
    )
    .await;
    let of_one_engine = served_count(case, &payload);
    assert!(
        of_one_engine > 0,
        "case {case:?}: expected the versions of {PUBLISHED_ENGINE}, got {payload}"
    );
    assert_eq!(
        payload.get("engines"),
        Some(&json!([PUBLISHED_ENGINE])),
        "case {case:?}: expected every entry to be {PUBLISHED_ENGINE}'s, got {payload}"
    );

    // 3. The same read cut to a PAGE: the page size the call asked for bounds what came back.
    //    20 is the smallest `MaxRecords` the member publishes
    //    (`smithy_query(query="doc",shape="DescribeDBEngineVersionsMessage",member="MaxRecords")`:
    //    "Minimum 20, maximum 100").
    let case = "a page of at most the records asked for";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_engine_versions",
            "parameters": { "Engine": PUBLISHED_ENGINE, "MaxRecords": 20 },
            "items": "DBEngineVersions",
            "read": ["Engine", "EngineVersion"],
        }),
    )
    .await;
    let paged = served_count(case, &payload);
    assert!(
        paged > 0 && paged <= 20,
        "case {case:?}: expected at most the 20 records asked for, got {paged} in {payload}"
    );

    // 4. The same read narrowed to the DEFAULT version: no more entries than the whole engine
    //    has, and at least one.
    let case = "the default version of one engine";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_engine_versions",
            "parameters": { "Engine": PUBLISHED_ENGINE, "DefaultOnly": true },
            "items": "DBEngineVersions",
            "read": ["Engine", "EngineVersion"],
        }),
    )
    .await;
    let default_only = served_count(case, &payload);
    assert!(
        default_only > 0 && default_only <= of_one_engine,
        "case {case:?}: expected at least one and at most the {of_one_engine} versions of the \
         engine, got {default_only} in {payload}"
    );

    // 5. A REFUSED read: an engine the service publishes nothing for. What it must be refused
    //    with is the service's own answer to the same call, read by making it directly.
    let case = "an engine the service publishes no versions for";
    let (status, body) = rds(
        &router,
        "DescribeDBEngineVersions",
        &[("Engine", UNPUBLISHED_ENGINE)],
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
            "method": "describe_db_engine_versions",
            "parameters": { "Engine": UNPUBLISHED_ENGINE },
            "items": "DBEngineVersions",
            "read": ["Engine"],
        }),
    )
    .await;
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

    // 6. The OTHER catalogue read, which the same rule covers: the configurations an engine can
    //    be launched in, each naming the instance class it is for.
    let case = "the configurations an engine can be launched in";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_orderable_db_instance_options",
            "parameters": { "Engine": PUBLISHED_ENGINE },
            "items": "OrderableDBInstanceOptions",
            "read": ["Engine", "DBInstanceClass"],
        }),
    )
    .await;
    let orderable = served_count(case, &payload);
    assert!(
        orderable > 0,
        "case {case:?}: expected the orderable options of {PUBLISHED_ENGINE}, got {payload}"
    );
    assert_eq!(
        payload.get("engines"),
        Some(&json!([PUBLISHED_ENGINE])),
        "case {case:?}: expected every option to be {PUBLISHED_ENGINE}'s, got {payload}"
    );
    assert!(
        payload
            .pointer("/first/DBInstanceClass")
            .and_then(Value::as_str)
            .is_some_and(|class| !class.is_empty()),
        "case {case:?}: expected each option to name an instance class, got {payload}"
    );

    // 7. An operation of the service this runtime's SDK does not bind: the handler is told the
    //    call was NOT MADE, naming the operations it does bind, rather than sent a request this
    //    seam would be guessing at.
    let case = "an operation this runtime's SDK does not bind";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_snapshots",
            "parameters": {},
            "items": "DBSnapshots",
            "read": ["DBSnapshotIdentifier"],
        }),
    )
    .await;
    let reported = payload
        .get("not_made")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The name the refusal reports is the one Amazon RDS's own model spells the operation
    // with (`DescribeDBSnapshots`), not the recasing of the method name the SDK derived it
    // from (`describe_db_snapshots`), because which operation a client's name names is that
    // model's to say.
    assert!(
        reported.contains("DescribeDBSnapshots") && reported.contains("DescribeDBEngineVersions"),
        "case {case:?}: expected the refusal to name the call and what is bound, got {payload}"
    );
}
