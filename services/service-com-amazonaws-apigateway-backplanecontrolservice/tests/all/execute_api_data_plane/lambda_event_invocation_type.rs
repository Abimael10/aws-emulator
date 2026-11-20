// SPDX-License-Identifier: CC0-1.0

//! WHICH INVOCATION of its Lambda function a method's integration makes, and what the caller
//! is served for it.
//!
//! RULE: `Integration$requestParameters` maps values onto the INTEGRATION REQUEST, and AWS
//! Lambda's `Invoke` binds its `InvocationType` to the request header
//! `X-Amz-Invocation-Type` (`smithy_query(query="operation", operation="Invoke")` of the
//! Lambda model) — so the value an integration maps onto that header of the integration
//! request is the invocation type API Gateway makes the call under, and an integration that
//! maps none makes the synchronous call `Invoke` documents as its default. An invocation that
//! is not the synchronous one is answered before any run has produced a payload — `Event`
//! "invoke[s] the function asynchronously […] The API response only includes a status code",
//! `DryRun` "validate[s] parameter values and verif[ies] that the user or role has permission
//! to invoke the function" and runs nothing — so the caller is answered at once, with no body
//! from the run, and an `Event` invocation's function runs in the background all the same.
//!
//! The rows span what the rule generalises over: the MAPPING FORM (none at all, a static
//! value in single quotes, a method request header the caller supplies), the INVOCATION TYPE
//! named (`RequestResponse`, `Event`, `DryRun`) and the INTEGRATION TYPE that makes the call
//! (`AWS` and `AWS_PROXY`, the two types whose backend is the function-invoking action).
//!
//! What makes a run OBSERVABLE: the handler writes one marker item into a DynamoDB table,
//! keyed by the resource path it was reached through, before it answers. So a row asserts
//! what the CALLER was served and, separately, whether the function ran at all — the
//! asynchronous rows by waiting for the item to appear after the caller was already answered,
//! the dry-run row by the item never appearing, which the other rows' own markers show there
//! was time for.
//!
//! Only a cross-service suite can pin this: the function is AWS Lambda's, the item is Amazon
//! DynamoDB's, and the api, the stage and the integration are API Gateway's. The wire harness
//! is the sibling `proxy_binary_media_types` suite's, reused rather than stood up a second
//! way; only the engine this one mounts a THIRD service on is its own.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{
    apigw, authorization, dispatch, lambda, member, package, wait_until_active, Answer,
    FUNCTION_TIMEOUT, HANDLER, REGION, ROLE, STAGE,
};

/// The function every method below is integrated with.
const FUNCTION: &str = "apigw-invocation-type";

/// The table the handler writes its marker item into.
const TABLE: &str = "apigw-invocation-marks";

/// The header a mapped-from-the-request method takes its invocation type from, declared on
/// the method and mapped onto the integration request by the integration.
const CASE_HEADER: &str = "X-Invocation-Type";

/// How long this suite waits for another service's clock-owed work — a created function
/// becoming `Active`, a created table becoming `ACTIVE`, an accepted asynchronous invocation
/// being delivered — and how often it looks. The deadline is deliberately far longer than any
/// of those takes: what is waited for is a CONDITION, never a span of time, so the budget only
/// has to be generous enough that a loaded host cannot turn it into a verdict.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The ONE handler every method is integrated with: it marks its own run and answers what the
/// integration type that invoked it expects.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    # An AWS_PROXY integration hands over the whole method request; an AWS integration hands
    # over the integration request this api's own template rendered, which names the path.
    proxied = isinstance(event, dict) and "requestContext" in event
    if proxied:
        pk = event["requestContext"]["resourcePath"]
    else:
        pk = event["pk"]
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={"pk": {"S": pk}},
    )
    answered = {"stored": True, "pk": pk}
    if proxied:
        return {
            "statusCode": 200,
            "headers": {"Content-Type": "application/json"},
            "body": json.dumps(answered),
        }
    return answered
"#;

/// API Gateway, AWS Lambda and Amazon DynamoDB over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: the control-plane calls carry a credential
    // scope so the router can name the target service, and the invokes carry no credential at
    // all, as a client of a published invoke URL sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// The marker item the handler wrote for the resource path `pk`, or `None` when nothing is
/// stored under it — which is what a function that never ran leaves behind.
async fn marker(router: &MultiServiceRouter, case: &str, pk: &str) -> Option<Value> {
    let read = dynamodb(
        router,
        "GetItem",
        json!({ "TableName": TABLE, "Key": { "pk": { "S": pk } }, "ConsistentRead": true }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetItem for {pk:?}, got {} with {:?}",
        read.status,
        read.text()
    );
    read.json().get("Item").cloned()
}

/// Wait until the marker item for `pk` is there, and answer whether it arrived: the run an
/// accepted asynchronous invocation makes happens after the caller was answered, so the
/// CONDITION is polled rather than a span waited out.
async fn marked(router: &MultiServiceRouter, case: &str, pk: &str) -> bool {
    let looks = WAIT.as_millis() / LOOK.as_millis().max(1);
    for _ in 0..looks {
        if marker(router, case, pk).await.is_some() {
            return true;
        }
        tokio::time::sleep(LOOK).await;
    }
    false
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE`, which is when it
/// serves the writes the handler makes ("you can perform read and write operations only on an
/// ACTIVE table").
async fn wait_until_table_active(router: &MultiServiceRouter) {
    let looks = WAIT.as_millis() / LOOK.as_millis().max(1);
    let mut last = Value::Null;
    for _ in 0..looks {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": TABLE })).await;
        let status = described.json();
        if status.pointer("/Table/TableStatus").and_then(Value::as_str) == Some("ACTIVE") {
            return;
        }
        last = status;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup CreateTable: the table never became ACTIVE, last read {last}");
}

/// One method of the api: the integration type in front of the one function, and what its
/// `requestParameters` map onto the invocation type header.
struct Method {
    /// The path part, which is also the resource path the handler marks its run by.
    part: &'static str,
    /// `Integration$type`.
    kind: &'static str,
    /// The `requestParameters` value mapped onto
    /// `integration.request.header.X-Amz-Invocation-Type`, or `None` for an integration that
    /// maps nothing onto it.
    mapped: Option<&'static str>,
}

/// The methods the api carries, each on its own path so one deployment serves every row and
/// each run has a marker key of its own.
const METHODS: &[Method] = &[
    Method {
        part: "sync-default",
        kind: "AWS",
        mapped: None,
    },
    Method {
        part: "sync-stated",
        kind: "AWS",
        mapped: Some("'RequestResponse'"),
    },
    Method {
        part: "async",
        kind: "AWS",
        mapped: Some("'Event'"),
    },
    Method {
        part: "dry-run",
        kind: "AWS",
        mapped: Some("'DryRun'"),
    },
    // Two methods mapping the invocation type from a HEADER THE CALLER SENDS, so the rows can
    // drive both values through one mapping form.
    Method {
        part: "mapped-async",
        kind: "AWS",
        mapped: Some("method.request.header.X-Invocation-Type"),
    },
    Method {
        part: "mapped-sync",
        kind: "AWS",
        mapped: Some("method.request.header.X-Invocation-Type"),
    },
    Method {
        part: "proxy-sync",
        kind: "AWS_PROXY",
        mapped: None,
    },
    Method {
        part: "proxy-async",
        kind: "AWS_PROXY",
        mapped: Some("'Event'"),
    },
    Method {
        part: "proxy-dry-run",
        kind: "AWS_PROXY",
        mapped: Some("'DryRun'"),
    },
];

/// Stand the table, the function and the api up, and deploy the api to [`STAGE`]. Answers the
/// api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let created = dynamodb(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup CreateTable: expected HTTP 200, got {} with {:?}",
        created.status,
        created.text()
    );

    let function = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "TABLE": TABLE } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        function.status,
        201,
        "setup CreateFunction: expected HTTP 201, got {} with {:?}",
        function.status,
        function.text()
    );
    let function_arn = member("CreateFunction", &function.json(), "FunctionArn");

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits every invocation below. It names no `SourceArn`, so one statement admits the
    // call from every method of the api.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-invocation-type",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201, got {} with {:?}",
        granted.status,
        granted.text()
    );

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "lambda-invocation-type" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");

    for method in METHODS {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": method.part })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        let mut put_method = json!({ "authorizationType": "NONE" });
        if method
            .mapped
            .is_some_and(|value| value.starts_with("method.request."))
        {
            // The method declares the request parameter its integration maps onward, which is
            // how a caller-supplied value reaches the integration request.
            let mut declared = serde_json::Map::new();
            declared.insert(
                format!("method.request.header.{CASE_HEADER}"),
                Value::Bool(false),
            );
            put_method["requestParameters"] = Value::Object(declared);
        }
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/POST"),
            Some(put_method),
            201,
        )
        .await;
        let mut integration = json!({
            "type": method.kind,
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        });
        if let Some(mapped) = method.mapped {
            integration["requestParameters"] =
                json!({ "integration.request.header.X-Amz-Invocation-Type": mapped });
        }
        if method.kind == "AWS" {
            // An `AWS` integration's event is the integration request its own template
            // renders; the proxy type maps nothing and passes the method request through.
            integration["requestTemplates"] = json!({
                "application/json": "{\"pk\":\"$context.resourcePath\"}",
            });
        }
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/POST/integration"),
            Some(integration),
            201,
        )
        .await;
        if method.kind == "AWS" {
            // The method response and the DEFAULT integration response — the one with no
            // `selectionPattern` — which is what maps whatever Lambda answered the invocation
            // with onto the method response.
            apigw(
                router,
                "PutMethodResponse",
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/POST/responses/200"),
                Some(json!({})),
                201,
            )
            .await;
            apigw(
                router,
                "PutIntegrationResponse",
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/POST/integration/\
                     responses/200"
                ),
                Some(json!({})),
                201,
            )
            .await;
        }
    }

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    wait_until_active(router, FUNCTION).await;
    wait_until_table_active(router).await;
    api
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying no credential at all, as a
/// client of a public method sends none.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    part: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let mut sent = vec![("content-type", "application/json")];
    sent.extend_from_slice(headers);
    dispatch(
        router,
        "POST",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{part}"),
        &sent,
        Bytes::from_static(b"{}"),
    )
    .await
}

/// What the caller must be served for one row.
#[derive(Debug, PartialEq, Eq)]
enum Served {
    /// The document the function's own run produced, which only an invocation made FOR that
    /// answer can carry.
    TheRun,
    /// No body at all: the invocation was answered before any run produced one.
    Nothing,
}

/// One row: the method it invokes, the invocation-type header the caller sends, what the
/// caller must be served and whether the function must have run.
struct Row {
    case: &'static str,
    part: &'static str,
    /// The value the caller sends under [`CASE_HEADER`], for a method that maps the header.
    sent: Option<&'static str>,
    served: Served,
    /// Whether the function's own run must have happened.
    ran: bool,
}

/// RULE: the invocation type the integration maps onto the integration request's
/// `X-Amz-Invocation-Type` header decides the call API Gateway makes — so a mapping that names
/// the asynchronous invocation answers the caller 200 with no body while the function runs in
/// the background, one that names the validating invocation answers the same with nothing run
/// at all, and an integration that maps none (or names the synchronous invocation) serves what
/// the run produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mapped_invocation_type_decides_the_call_the_integration_makes() {
    let router = emulator();
    let api = deployed(&router).await;

    let rows: &[Row] = &[
        Row {
            case: "an AWS integration mapping nothing: the documented default invocation",
            part: "sync-default",
            sent: None,
            served: Served::TheRun,
            ran: true,
        },
        Row {
            case: "an AWS integration mapping the synchronous invocation",
            part: "sync-stated",
            sent: None,
            served: Served::TheRun,
            ran: true,
        },
        Row {
            case: "an AWS integration mapping the asynchronous invocation",
            part: "async",
            sent: None,
            served: Served::Nothing,
            ran: true,
        },
        Row {
            case: "an AWS integration mapping the validating invocation, which runs nothing",
            part: "dry-run",
            sent: None,
            served: Served::Nothing,
            ran: false,
        },
        Row {
            case: "a caller-sent header mapped onto the asynchronous invocation",
            part: "mapped-async",
            sent: Some("Event"),
            served: Served::Nothing,
            ran: true,
        },
        Row {
            case: "the same mapping with the caller naming the synchronous invocation",
            part: "mapped-sync",
            sent: Some("RequestResponse"),
            served: Served::TheRun,
            ran: true,
        },
        Row {
            case: "an AWS_PROXY integration mapping nothing",
            part: "proxy-sync",
            sent: None,
            served: Served::TheRun,
            ran: true,
        },
        Row {
            case: "an AWS_PROXY integration mapping the asynchronous invocation",
            part: "proxy-async",
            sent: None,
            served: Served::Nothing,
            ran: true,
        },
        Row {
            case: "an AWS_PROXY integration mapping the validating invocation, which runs \
                   nothing",
            part: "proxy-dry-run",
            sent: None,
            served: Served::Nothing,
            ran: false,
        },
    ];

    // Every row's CALLER is answered first, so the asynchronous rows are answered before any
    // marker is looked for: what the caller holds cannot depend on a run that has not
    // happened.
    for row in rows {
        let headers: Vec<(&str, &str)> = row
            .sent
            .map(|value| vec![(CASE_HEADER, value)])
            .unwrap_or_default();
        let served = invoke(&router, &api, row.part, &headers).await;
        assert_eq!(
            served.status,
            200,
            "case {:?}: expected HTTP 200 from the stage, got {} with {:?}",
            row.case,
            served.status,
            served.text()
        );
        match row.served {
            Served::Nothing => assert_eq!(
                served.text(),
                "",
                "case {:?}: an invocation made for no answer leaves the method response with \
                 no body, got {:?}",
                row.case,
                served.text()
            ),
            Served::TheRun => assert_eq!(
                served.json(),
                json!({ "stored": true, "pk": format!("/{}", row.part) }),
                "case {:?}: expected the document the run produced, got {:?}",
                row.case,
                served.text()
            ),
        }
    }

    // ... and only then whether each function RAN, which is the other half of the rule: the
    // asynchronous rows' runs happened after their callers were answered, so the condition is
    // polled for the rows that must have run.
    for row in rows {
        let pk = format!("/{}", row.part);
        if row.ran {
            assert!(
                marked(&router, row.case, &pk).await,
                "case {:?}: the function's own run must have written its marker item under \
                 {pk:?}",
                row.case
            );
        }
    }
    // The validating invocation ran nothing: its marker is still absent now that every other
    // row's run has been seen, which is what shows there was time for one.
    for row in rows.iter().filter(|row| !row.ran) {
        let held = marker(&router, row.case, &format!("/{}", row.part)).await;
        assert!(
            held.is_none(),
            "case {:?}: an invocation that runs nothing must leave no marker item, got \
             {held:?}",
            row.case
        );
    }
}
