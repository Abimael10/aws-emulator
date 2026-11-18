// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST stage's EXECUTION LOG carries about the requests its `AWS` integration
//! served, and what the DATA TRACE adds to it, on an emulator where every service the rule
//! spans is mounted on one engine.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > `MethodSetting$loggingLevel` "specifies the logging level for this method, which affects
//! > the log entries pushed to Amazon CloudWatch Logs", with "valid values are `OFF`, `ERROR`,
//! > and `INFO`. Choose `ERROR` to write only error-level entries to CloudWatch Logs, or choose
//! > `INFO` to include all `ERROR` events as well as extra informational events", and
//! > `MethodSetting$dataTrace` "specifies whether data trace logging is enabled for this
//! > method, which affects the log entries pushed to Amazon CloudWatch Logs". So, once the
//! > account names a CloudWatch role API Gateway can assume, the setting in force for the
//! > method a request reached decides what the group API Gateway publishes for that stage
//! > (`API-Gateway-Execution-Logs_{api-id}/{stage}`) holds about it: nothing at `OFF`; only the
//! > entries that report the FAILURE of a request the stage answered with an error at `ERROR`;
//! > and the whole trace of every request at `INFO`. The data trace decides, independently,
//! > whether that trace carries the PAYLOADS the request exchanged — with it on, one line per
//! > transformation point of serving the request (the method request before transformations,
//! > the endpoint request after them, the endpoint response before them and the method response
//! > after them), each carrying the bytes that really crossed that point; with it off, no
//! > payload line at all.
//!
//! Only a cross-service suite can pin this: the api, the stage, its method settings and the
//! emission of the trace are API Gateway's, the table action the integration performs is Amazon
//! DynamoDB's, the two roles are AWS IAM's, and whether a log group holds an event at all is
//! Amazon CloudWatch Logs'. Every assertion reads the outcome back through CloudWatch Logs' own
//! `FilterLogEvents`, so a line that never reached that store cannot pass, and the payload lines
//! are asserted against what the call really exchanged: the document the table answered and the
//! body the client was served.
//!
//! The rows differ along what the rule generalises over: the level in force (`OFF`, `ERROR`,
//! `INFO`), the data trace beside it (off, on), and the OUTCOME of the request the trace is of —
//! one the integration performed and the stage answered `200`, and a sibling whose integration
//! names an execution role that does not exist, which the stage answers `500` through its own
//! configuration-error gateway response without the call being made.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call is signed for, every invoke host carries and every ARN names.
const REGION: &str = "us-east-1";

/// The long-term key the setup calls are signed with. The signature itself is never checked:
/// this router runs with request authentication off, as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The stage the api is deployed to.
const STAGE: &str = "logs";

/// The table the integration's action addresses, and the key attribute of its items.
const TABLE: &str = "execution-log-markers";
const KEY_ATTRIBUTE: &str = "id";

/// The resource whose integration is performed under a role IAM holds, and the sibling whose
/// integration names a role it holds nothing under.
const SERVED_PART: &str = "count";
const BROKEN_PART: &str = "broken";

/// The role the served resource's integration is performed under, and the role the account
/// writes its logs under, each created through IAM's own `CreateRole`.
const TABLE_ROLE: &str = "apigw-execution-log-table";
const CLOUDWATCH_ROLE: &str = "apigw-execution-log-push";

/// A role ARN this account's IAM holds nothing under, which is what makes the sibling
/// resource's integration one that cannot be performed at all.
const ABSENT_ROLE: &str = "arn:aws:iam::000000000000:role/apigw-execution-log-absent";

/// A trust policy that lets API Gateway assume the role it is attached to, which is what makes
/// a role one this service may act under at all.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The `methodSettings` key that names every method of the stage, which is the scope every
/// patch below addresses (`Stage$methodSettings`: `{resource-path}/{HTTP-METHOD}` or `*/*`).
const EVERY_METHOD_KEY: &str = "*/*";

/// The header API Gateway names the request a response answers in, lowercased as the wire
/// carries it; every record of a stage's execution log is prefixed with that id.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long to wait between polls of a clock-owed transition, and the generous budget one is
/// given before the suite calls it a failure. Nothing here asserts on how long it took: the
/// condition is polled for until it holds.
const POLL: Duration = Duration::from_millis(50);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(120);

/// What one call answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answer {
    /// One response header's value.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The four services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One API Gateway control-plane call.
async fn apigw(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let answer = apigw(router, method, path, body).await;
    assert_eq!(
        answer.status,
        expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, which has to answer 200.
async fn ddb_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let answer = dispatch(
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
    .await;
    assert_eq!(
        answer.status,
        200,
        "{case:?}: expected HTTP 200 from DynamoDB {operation}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Amazon CloudWatch Logs call over its own `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One IAM awsQuery call that has to succeed, answering its body.
async fn iam_ok(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let answer = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from IAM, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com`, which is the endpoint
/// `RestApi$disableExecuteApiEndpoint` documents — carrying no credential at all, as a client
/// of a public method sends it.
async fn invoke(router: &MultiServiceRouter, api: &str, path: &str) -> Answer {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// One IAM role created with the API Gateway trust policy and `document` as its one inline
/// policy, answering its ARN.
async fn role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let created = iam_ok(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={}",
            form_encode(APIGATEWAY_TRUST)
        ),
    )
    .await;
    let arn = element(&created, "Arn");
    iam_ok(
        router,
        &format!("PutRolePolicy({name})"),
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}&PolicyName=execution-log\
             &PolicyDocument={}",
            form_encode(document)
        ),
    )
    .await;
    arn
}

/// One identity-based policy allowing `action` on `resource`.
fn policy(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// The table the integration writes into, created through DynamoDB's own API and waited for
/// until that service first reports it `ACTIVE` — a table that is not is one no call lands on.
async fn table(router: &MultiServiceRouter) {
    ddb_ok(
        router,
        "setup CreateTable",
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = ddb_ok(
            router,
            "setup DescribeTable",
            "DescribeTable",
            json!({ "TableName": TABLE }),
        )
        .await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the table never reported ACTIVE, last {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The request template that renders the table action's input document: the marker the request
/// carries in its query string is both the item's key and the attribute the action writes, so
/// one request's document is distinguishable from another's.
fn request_template() -> String {
    format!(
        concat!(
            r#"{{"TableName":"{table}","Key":{{"{key}":{{"S":"$input.params('m')"}}}},"#,
            r#""UpdateExpression":"SET marker = :m","#,
            r#""ExpressionAttributeValues":{{":m":{{"S":"$input.params('m')"}}}},"#,
            r#""ReturnValues":"UPDATED_NEW"}}"#
        ),
        table = TABLE,
        key = KEY_ATTRIBUTE
    )
}

/// The response template of the success branch, which renders the document this api publishes
/// out of what the table answered.
const RESPONSE_TEMPLATE: &str = r#"{"marker":"$input.path('$.Attributes.marker.S')"}"#;

/// Stand one resource of the api up: the method, its `AWS` integration to the table action
/// under `credentials`, and the `200` branch that maps what the table answered.
async fn given_resource(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    part: &str,
    credentials: &str,
) {
    let case = format!("the {part} resource");
    let created = apigw_ok(
        router,
        &case,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    let resource = member(&case, &created, "id");
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
        Some(json!({
            "type": "AWS",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports for it.
            "httpMethod": "POST",
            "uri": format!("arn:aws:apigateway:{REGION}:dynamodb:action/UpdateItem"),
            "credentials": credentials,
            "passthroughBehavior": "NEVER",
            "requestTemplates": { "application/json": request_template() },
        })),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
        Some(json!({
            "responseTemplates": { "application/json": RESPONSE_TEMPLATE },
        })),
        201,
    )
    .await;
}

/// Gate the stage's execution logging: the level in force for every method of it, and the data
/// trace beside it. Also asserts what `GetStage` then reports, because the setting the gating is
/// resolved by is the one the patch stored.
async fn gate(
    router: &MultiServiceRouter,
    api: &str,
    case: &str,
    level: &str,
    data_trace: bool,
) {
    apigw_ok(
        router,
        case,
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": format!("/{EVERY_METHOD_KEY}/logging/loglevel"),
                    "value": level,
                },
                {
                    "op": "replace",
                    "path": format!("/{EVERY_METHOD_KEY}/logging/dataTrace"),
                    "value": data_trace.to_string(),
                },
            ],
        })),
        200,
    )
    .await;
    let read = apigw_ok(
        router,
        case,
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    let reported = &read["methodSettings"][EVERY_METHOD_KEY];
    assert_eq!(
        (&reported["loggingLevel"], &reported["dataTraceEnabled"]),
        (&json!(level), &json!(data_trace)),
        "case {case:?}: expected GetStage to report the gating the patch stored, got {reported}"
    );
}

/// One request the stage answered: its status, its body and the id API Gateway reported it
/// under.
struct Served {
    status: u16,
    body: String,
    request_id: String,
}

/// Drive one request through `part` of the stage, carrying `marker` as the value its template
/// reads.
async fn served(
    router: &MultiServiceRouter,
    api: &str,
    case: &str,
    part: &str,
    marker: &str,
) -> Served {
    let answered = invoke(router, api, &format!("/{part}?m={marker}")).await;
    let request_id = answered.header(REQUEST_ID_HEADER).unwrap_or_else(|| {
        panic!("case {case:?}: the answer to /{part} named no {REQUEST_ID_HEADER}")
    });
    Served {
        status: answered.status,
        body: answered.text(),
        request_id,
    }
}

/// One record of a stage's execution log as the group holds it: API Gateway prefixes each line
/// of a request's trace with that request's own id in parentheses, so one group can hold the
/// traces of every request the stage served.
fn traced_line(request_id: &str, line: &str) -> String {
    format!("({request_id}) {line}")
}

/// The records of ONE request in the stage's execution log group, in the order they were
/// delivered, read back through CloudWatch Logs' own `FilterLogEvents`; empty for a request
/// nothing was written for, and for a stage that has no such group at all.
async fn trace_of(router: &MultiServiceRouter, api: &str, request_id: &str) -> Vec<String> {
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
    let answer = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answer.status != 200 {
        return Vec::new();
    }
    let prefix = traced_line(request_id, "");
    answer.json()["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str())
                .filter(|message| message.starts_with(&prefix))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// How many records of `trace` are exactly `line`.
fn carrying(trace: &[String], line: &str) -> usize {
    trace.iter().filter(|record| *record == line).count()
}

/// How many records of `trace` open with `prefix`.
fn opening_with(trace: &[String], prefix: &str) -> usize {
    trace
        .iter()
        .filter(|record| record.starts_with(prefix))
        .count()
}

/// The text the one record of `trace` opening with `prefix` carries after it.
fn traced_payload(trace: &[String], prefix: &str) -> String {
    trace
        .iter()
        .find_map(|record| record.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no record opening with {prefix:?} in {trace:?}"))
        .to_string()
}

/// One row of the table: the gating in force, and what each of the stage's two requests then
/// leaves in the group.
struct Row {
    case: &'static str,
    /// The `loggingLevel` every method of the stage is gated by.
    level: &'static str,
    /// The `dataTrace` beside it.
    data_trace: bool,
    /// Whether the request the integration SERVED leaves a trace.
    traces_the_served_request: bool,
    /// Whether the request whose integration could not be performed leaves one.
    traces_the_failed_request: bool,
}

/// The rows: logging off, errors only, the whole trace, and the whole trace with the data.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "logging OFF",
            level: "OFF",
            data_trace: false,
            traces_the_served_request: false,
            traces_the_failed_request: false,
        },
        Row {
            case: "errors only",
            level: "ERROR",
            data_trace: false,
            traces_the_served_request: false,
            traces_the_failed_request: true,
        },
        Row {
            case: "the whole trace, without the data",
            level: "INFO",
            data_trace: false,
            traces_the_served_request: true,
            traces_the_failed_request: true,
        },
        Row {
            case: "the whole trace, with the data",
            level: "INFO",
            data_trace: true,
            traces_the_served_request: true,
            traces_the_failed_request: true,
        },
    ]
}

/// The lines the data trace puts in a trace: one per transformation point of serving the
/// request, which is what `dataTrace` exists to gate.
const PAYLOAD_PREFIXES: [&str; 4] = [
    "Method request body before transformations: ",
    "Endpoint request body after transformations: ",
    "Endpoint response body before transformations: ",
    "Method response body after transformations: ",
];

/// The sentence API Gateway opens the entry that reports a configuration error of the method's
/// integration with, which is the one entry `ERROR` writes for such a request.
const CONFIGURATION_ERROR: &str = "Execution failed due to configuration error: ";

/// RULE: the level in force decides which of a method's requests are traced and which entries
/// each trace carries, and the data trace beside it decides whether the trace carries the
/// payloads the request exchanged with the integration's backend.
#[tokio::test]
async fn the_level_in_force_and_the_data_trace_gate_the_stages_execution_log() {
    let router = emulator();
    table(&router).await;
    let table_role = role(
        &router,
        TABLE_ROLE,
        &policy(
            "dynamodb:UpdateItem",
            &format!("arn:aws:dynamodb:{REGION}:000000000000:table/{TABLE}"),
        ),
    )
    .await;
    // API Gateway writes a stage's execution log under the ACCOUNT's CloudWatch role, so the
    // account names one it really holds before any of the rows runs.
    let push_role = role(
        &router,
        CLOUDWATCH_ROLE,
        &policy("logs:PutLogEvents", "*"),
    )
    .await;
    apigw_ok(
        &router,
        "UpdateAccount",
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/cloudwatchRoleArn", "value": push_role },
            ],
        })),
        200,
    )
    .await;

    let created = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "execution-log-data-trace" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let resources = apigw_ok(
        &router,
        "GetResources",
        "GET",
        &format!("/restapis/{api}/resources"),
        None,
        200,
    )
    .await;
    let root = resources["item"]
        .as_array()
        .and_then(|items| items.first())
        .map(|item| member("GetResources", item, "id"))
        .expect("the api's root resource");
    given_resource(&router, &api, &root, SERVED_PART, &table_role).await;
    given_resource(&router, &api, &root, BROKEN_PART, ABSENT_ROLE).await;
    apigw_ok(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        gate(&router, &api, case, row.level, row.data_trace).await;

        // The request the integration PERFORMS: the table action is made, and the method
        // response is what the table answered, mapped by the integration response's template.
        let marker = format!("m-{index}");
        let served_request = served(&router, &api, case, SERVED_PART, &marker).await;
        let expected_body = format!(r#"{{"marker":"{marker}"}}"#);
        assert_eq!(
            (served_request.status, served_request.body.as_str()),
            (200, expected_body.as_str()),
            "case {case:?}: expected the table's answer mapped by the integration response, \
             got {} with {:?}",
            served_request.status,
            served_request.body
        );
        // The write reached the TABLE, which is what makes the trace below the trace of a call
        // that was really made.
        let read = ddb_ok(
            &router,
            &format!("case {case:?}: GetItem"),
            "GetItem",
            json!({ "TableName": TABLE, "Key": { KEY_ATTRIBUTE: { "S": marker } } }),
        )
        .await;
        assert_eq!(
            read["Item"]["marker"]["S"],
            json!(marker),
            "case {case:?}: expected the table to hold what the integration wrote, got {read}"
        );

        // And the sibling request, whose integration names an execution role IAM holds nothing
        // under: no call is made, and the stage answers it through its own configuration-error
        // gateway response.
        let failed_request = served(&router, &api, case, BROKEN_PART, &marker).await;
        assert_eq!(
            (failed_request.status, failed_request.body.as_str()),
            (500, r#"{"message":"Internal server error"}"#),
            "case {case:?}: expected the configuration-error refusal, got {} with {:?}",
            failed_request.status,
            failed_request.body
        );

        // 1. WHICH of the two requests left a trace at all.
        let served_trace = trace_of(&router, &api, &served_request.request_id).await;
        let failed_trace = trace_of(&router, &api, &failed_request.request_id).await;
        assert_eq!(
            !served_trace.is_empty(),
            row.traces_the_served_request,
            "case {case:?}: expected the served request to be traced={}, got {served_trace:?}",
            row.traces_the_served_request
        );
        assert_eq!(
            !failed_trace.is_empty(),
            row.traces_the_failed_request,
            "case {case:?}: expected the failed request to be traced={}, got {failed_trace:?}",
            row.traces_the_failed_request
        );

        // 2. The trace of the FAILED request names the configuration error that stopped it,
        //    which is the entry that reports the failure — and at `ERROR` the informational
        //    steps beside it are not written, which is what writing only error-level entries
        //    means.
        if row.traces_the_failed_request {
            let failures = opening_with(
                &failed_trace,
                &traced_line(&failed_request.request_id, CONFIGURATION_ERROR),
            );
            assert_eq!(
                failures, 1,
                "case {case:?}: expected exactly one configuration-error entry in the failed \
                 request's trace, got {failed_trace:?}"
            );
            let informational = carrying(
                &failed_trace,
                &traced_line(
                    &failed_request.request_id,
                    &format!(
                        "Starting execution for request: {}",
                        failed_request.request_id
                    ),
                ),
            );
            assert_eq!(
                informational,
                usize::from(row.level == "INFO"),
                "case {case:?}: expected the informational opening entry only at INFO, got \
                 {failed_trace:?}"
            );
        }

        // 3. The trace of the SERVED request carries the steps of serving it.
        if row.traces_the_served_request {
            for line in [
                format!(
                    "Starting execution for request: {}",
                    served_request.request_id
                ),
                format!("HTTP Method: GET, Resource Path: /{SERVED_PART}"),
                "Integration type: AWS".to_string(),
                "Method completed with status: 200".to_string(),
            ] {
                let line = traced_line(&served_request.request_id, &line);
                assert_eq!(
                    carrying(&served_trace, &line),
                    1,
                    "case {case:?}: expected exactly one record {line:?} in the served \
                     request's trace, got {served_trace:?}"
                );
            }
        }

        // 4. And the DATA TRACE decides whether the payloads of the call are in it: one line
        //    per transformation point with it on, and no payload line at all with it off.
        let payload_lines = usize::from(row.data_trace && row.traces_the_served_request);
        for prefix in PAYLOAD_PREFIXES {
            let prefix = traced_line(&served_request.request_id, prefix);
            let found = opening_with(&served_trace, &prefix);
            assert_eq!(
                found, payload_lines,
                "case {case:?}: expected the payload line {prefix:?} exactly \
                 {payload_lines} time(s) in the served request's trace, got {served_trace:?}"
            );
        }
        if payload_lines == 0 {
            continue;
        }
        // Each payload line carries what the request REALLY exchanged: the integration request
        // document the template rendered, the document the table answered, and the body the
        // client was served.
        let endpoint_request = traced_payload(
            &served_trace,
            &traced_line(
                &served_request.request_id,
                "Endpoint request body after transformations: ",
            ),
        );
        let rendered: Value = serde_json::from_str(&endpoint_request).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected the endpoint request line to carry the document the \
                 template rendered, got {endpoint_request:?}: {error}"
            )
        });
        assert_eq!(
            (
                &rendered["TableName"],
                &rendered["ExpressionAttributeValues"][":m"]["S"]
            ),
            (&json!(TABLE), &json!(marker)),
            "case {case:?}: expected the integration request document in the endpoint request \
             line, got {rendered}"
        );
        let endpoint_response = traced_payload(
            &served_trace,
            &traced_line(
                &served_request.request_id,
                "Endpoint response body before transformations: ",
            ),
        );
        let answered: Value = serde_json::from_str(&endpoint_response).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected the endpoint response line to carry the document the \
                 table answered, got {endpoint_response:?}: {error}"
            )
        });
        assert_eq!(
            answered["Attributes"]["marker"]["S"],
            json!(marker),
            "case {case:?}: expected the table's own answer in the endpoint response line, got \
             {answered}"
        );
        let method_response = traced_payload(
            &served_trace,
            &traced_line(
                &served_request.request_id,
                "Method response body after transformations: ",
            ),
        );
        assert_eq!(
            method_response, served_request.body,
            "case {case:?}: expected the method response line to carry the body the client was \
             served ({:?}), got {method_response:?}",
            served_request.body
        );
    }
}
