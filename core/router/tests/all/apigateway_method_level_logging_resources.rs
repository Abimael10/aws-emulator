// SPDX-License-Identifier: CC0-1.0

//! WHICH `methodSettings` entry of a deployed REST stage gates the execution log of a request
//! it served, over the RESOURCES of the stage: the entry keyed by the method the request
//! reached, and only then the stage-wide one.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys (designated as /{method_setting_key) are method paths defined as
//! > `{resource_path}/{http_method}` for an individual method override, or `/*/*` for
//! > overriding all methods in the stage", and `MethodSetting$loggingLevel` "specifies the
//! > logging level for this method, which affects the log entries pushed to Amazon CloudWatch
//! > Logs" with "valid values are `OFF`, `ERROR`, and `INFO`". So the level in force for a
//! > served request is the entry keyed by the METHOD it reached — that method's own resource
//! > path, at whatever depth and with whatever parameters in it, and its HTTP method — where
//! > the stage carries one, and the stage-wide entry for every method carrying none; and that
//! > one level decides what the group API Gateway publishes for the stage
//! > (`API-Gateway-Execution-Logs_{api-id}/{stage}`) comes to hold about the request: at `INFO`
//! > the whole trace of it, the informational step reporting the status the method completed
//! > with included, at `ERROR` only the entries that report a failure and only for a request
//! > the stage answered WITH one, and at `OFF` nothing.
//!
//! Only a cross-service suite can pin this: the api, its stage's method settings and the
//! emission of the trace are API Gateway's, and whether a log group holds an event at all is
//! Amazon CloudWatch Logs'. Every assertion reads the outcome back through CloudWatch Logs' own
//! reader (`FilterLogEvents`), so a record that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the RESOURCE the overridden method
//! sits on — the api's root resource, a child of it, a child of that child, and a parameterised
//! one, which are the shapes the resource half of a method-path key takes — which verb of each
//! resource carries the override, and the
//! STATUS the stage answered with, which is what separates what `ERROR` writes from what `INFO`
//! writes. The recorded case is one row of it: a stage at `ERROR` with one method of one
//! resource raised to `INFO`.
//!
//! Every method here is a `MOCK` integration — "for testing without actually invoking the
//! backend" (`IntegrationType`) — so what a leg shows is the stage's own LOGGING and never a
//! backend's doing.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for, every invoke host carries and every ARN names.
const REGION: &str = "us-east-1";

/// The long-term key the setup calls are signed with. The signature itself is never checked:
/// this router runs with request authentication off, as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The stage the api is deployed to.
const STAGE: &str = "s";

/// The `methodSettings` key that names every method of the stage, and — behind the pointer's
/// own leading separator — the `UpdateStage` patch path that addresses it.
const EVERY_METHOD_KEY: &str = "*/*";

/// The level the stage-wide entry carries: a method left under it traces a request the stage
/// answered with an error and nothing about one it answered successfully.
const STAGE_WIDE_LEVEL: &str = "ERROR";

/// The level every per-method entry below raises its own method to.
const OVERRIDE_LEVEL: &str = "INFO";

/// The header API Gateway names the request a response answers in, lowercased as the wire
/// carries it; every record of a stage's execution log is prefixed with that id.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The role the account writes its logs under. `Account$cloudwatchRoleArn` is "the ARN of an
/// Amazon CloudWatch role for the current Account": without one, API Gateway has nothing to
/// write the account's logs as and no trace is delivered at all.
fn cloudwatch_role_arn() -> String {
    format!(
        "arn:aws:iam::{account}:role/apigw-method-level-logging",
        account = engine::emulator_account_id()
    )
}

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

/// The two services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request to that service.
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

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
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
    .await;
    assert_eq!(
        answer.status,
        expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
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

/// The messages `FilterLogEvents` reports for `group`, or `None` when the account holds no such
/// group at all — which is what "nothing was delivered and no group was brought into existence"
/// looks like to a client.
async fn delivered(router: &MultiServiceRouter, group: &str) -> Option<Vec<String>> {
    let answer = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answer.status != 200 {
        return None;
    }
    Some(
        answer
            .json()
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        event
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com`, which is the endpoint
/// `RestApi$disableExecuteApiEndpoint` documents — carrying no credential at all, as a client of
/// a public method sends none.
async fn invoke(router: &MultiServiceRouter, api: &str, verb: &str, path: &str) -> Answer {
    dispatch(
        router,
        verb,
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

/// One record of a stage's execution log as the group holds it: API Gateway prefixes each line of
/// a request's trace with that request's own id in parentheses, so one group holds the traces of
/// every request the stage served.
fn traced_line(request_id: &str, line: &str) -> String {
    format!("({request_id}) {line}")
}

/// The `methodSettings` key the method `verb` of `resource_path` is held under: the method path
/// the patch POINTER spelled ([`method_patch_path`]), which is that pointer without the leading
/// separator a JSON Pointer opens with — so the resource half keeps the `~1` escape
/// `PatchOperation$path` spells a path name's separators with.
fn method_key(resource_path: &str, verb: &str) -> String {
    method_patch_path(resource_path, verb)
}

/// The patch path that addresses a setting of that same method, behind the pointer's own leading
/// separator: inside a pointer the resource path is ONE reference token, so every separator in it
/// — the leading one included — is written as the escape `PatchOperation$path` spells a separator
/// inside a path name with ("Any slash ("/") character appearing in path names must be escaped
/// with "~1"").
fn method_patch_path(resource_path: &str, verb: &str) -> String {
    format!("{escaped}/{verb}", escaped = resource_path.replace('/', "~1"))
}

/// One deployed method of the api: the resource path it sits on, the verb it answers, and the
/// status its one `MOCK` branch answers with.
///
/// The resource paths span the shapes the resource half of a method-path key takes: the api's
/// root resource, a child of it, a child of that child, and a parameterised one.
const METHODS: [(&str, &str, u16); 9] = [
    ("/", "GET", 200),
    ("/", "POST", 200),
    ("/echo", "GET", 200),
    ("/echo", "POST", 200),
    ("/echo/deep", "GET", 200),
    ("/echo/deep", "POST", 200),
    ("/items/{id}", "GET", 200),
    ("/items/{id}", "POST", 200),
    ("/fails", "GET", 503),
];

/// The methods raised to [`OVERRIDE_LEVEL`]: one per resource that carries two verbs, and a
/// different verb on each, so no leg's outcome can follow from the verb alone.
const OVERRIDDEN: [(&str, &str); 4] = [
    ("/", "GET"),
    ("/echo", "POST"),
    ("/echo/deep", "POST"),
    ("/items/{id}", "GET"),
];

/// The body the method of `verb` on `resource_path` answers, which names the method that served
/// the request: a leg that reached another method of the api answers another body.
fn served_body(resource_path: &str, verb: &str) -> String {
    format!(r#"{{"served":"{verb} {resource_path}"}}"#)
}

/// The resources every method of [`METHODS`] sits on, by resource path: the api's own root and
/// the tree created under it, parents before children.
async fn given_resources(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
) -> BTreeMap<String, String> {
    let mut resources: BTreeMap<String, String> = BTreeMap::new();
    resources.insert("/".to_string(), root.to_string());
    for (parent, part) in [
        ("/", "echo"),
        ("/echo", "deep"),
        ("/", "items"),
        ("/items", "{id}"),
        ("/", "fails"),
    ] {
        let parent_id = resources
            .get(parent)
            .unwrap_or_else(|| panic!("setup CreateResource({part}): no resource at {parent:?}"))
            .clone();
        let created = apigw_ok(
            router,
            &format!("CreateResource({part})"),
            "POST",
            &format!("/restapis/{api}/resources/{parent_id}"),
            Some(json!({ "pathPart": part })),
            201,
        )
        .await;
        let path = member("CreateResource", &created, "path");
        resources.insert(path, member("CreateResource", &created, "id"));
    }
    resources
}

/// One `MOCK` method: the method itself, its integration, the method response the status is
/// declared by and the integration response that renders the body.
async fn given_method(
    router: &MultiServiceRouter,
    api: &str,
    resource: &str,
    resource_path: &str,
    verb: &str,
    status: u16,
) {
    let case = format!("the {verb} method of {resource_path}");
    let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
    apigw_ok(
        router,
        &case,
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("{method}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("{method}/responses/{status}"),
        Some(json!({})),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("{method}/integration/responses/{status}"),
        Some(json!({
            "responseTemplates": { "application/json": served_body(resource_path, verb) },
        })),
        201,
    )
    .await;
}

/// The whole api: the resource tree, every method of [`METHODS`] on it, and a deployment released
/// to [`STAGE`]. Answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let created = apigw_ok(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "method-level-logging-resources" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resources = given_resources(router, &api, &root).await;
    for (resource_path, verb, status) in METHODS {
        let resource = resources
            .get(resource_path)
            .unwrap_or_else(|| panic!("setup: no resource at {resource_path:?}"));
        given_method(router, &api, resource, resource_path, verb, status).await;
    }
    apigw_ok(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// Install the account's CloudWatch role, which is what API Gateway writes the account's logs as.
async fn install_account_role(router: &MultiServiceRouter) {
    apigw_ok(
        router,
        "UpdateAccount",
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/cloudwatchRoleArn",
                    "value": cloudwatch_role_arn(),
                },
            ],
        })),
        200,
    )
    .await;
}

/// Gate the stage: the stage-wide entry at [`STAGE_WIDE_LEVEL`], and each method of
/// [`OVERRIDDEN`] raised to [`OVERRIDE_LEVEL`] through the patch path that addresses exactly that
/// method.
///
/// Also asserts WHICH entries the stage then reports, because the key a patch stored is the key
/// the gating is resolved by: a stage reporting other keys is gating other methods than the ones
/// named here.
async fn gate(router: &MultiServiceRouter, api: &str) {
    let mut operations = vec![json!({
        "op": "replace",
        "path": format!("/{EVERY_METHOD_KEY}/logging/loglevel"),
        "value": STAGE_WIDE_LEVEL,
    })];
    let mut expected = vec![EVERY_METHOD_KEY.to_string()];
    for (resource_path, verb) in OVERRIDDEN {
        let key = method_key(resource_path, verb);
        operations.push(json!({
            "op": "replace",
            "path": format!("/{}/logging/loglevel", method_patch_path(resource_path, verb)),
            "value": OVERRIDE_LEVEL,
        }));
        expected.push(key);
    }
    apigw_ok(
        router,
        "UpdateStage methodSettings",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({ "patchOperations": operations })),
        200,
    )
    .await;
    expected.sort();

    let read = apigw_ok(
        router,
        "GetStage",
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    let mut reported: Vec<String> = read
        .get("methodSettings")
        .and_then(Value::as_object)
        .map(|settings| settings.keys().cloned().collect())
        .unwrap_or_default();
    reported.sort();
    assert_eq!(
        reported, expected,
        "expected GetStage to report the methodSettings keys {expected:?}, got {reported:?} in \
         {read}"
    );
    for (resource_path, verb) in OVERRIDDEN {
        let key = method_key(resource_path, verb);
        let level = &read["methodSettings"][&key]["loggingLevel"];
        assert_eq!(
            level,
            &json!(OVERRIDE_LEVEL),
            "expected the entry {key:?} to carry loggingLevel {OVERRIDE_LEVEL:?}, got {level} in \
             {read}"
        );
    }
}

/// One leg: a request the stage serves, and what its execution log then has to hold.
struct Leg {
    /// The resource path of the method that serves it, which is the template a trace of it names
    /// and the resource half of the key its level is resolved by.
    resource_path: &'static str,
    /// The path the client addresses within the stage, empty for the api's root resource.
    request_path: &'static str,
    /// The verb the client sends.
    verb: &'static str,
    /// The status the stage answers it with.
    status: u16,
    /// Whether the level in force for that method leaves a trace of this request at all.
    traced: bool,
    /// Whether that level is `INFO`, which is what decides whether the trace carries the
    /// informational steps of serving the request beside the entries that report a failure.
    at_info: bool,
}

/// The legs, the traced ones first so a later untraced one is asserted against a group the stage
/// has already delivered into rather than against one that does not exist yet.
///
/// Each resource contributes the verb carrying its own `INFO` entry and the verb left to the
/// stage-wide `ERROR`, and the last leg is answered WITH an error through a method left to that
/// stage-wide entry: that entry is in force rather than absent, so it traces the failure and
/// nothing beside it.
const LEGS: [Leg; 9] = [
    Leg {
        resource_path: "/",
        request_path: "",
        verb: "GET",
        status: 200,
        traced: true,
        at_info: true,
    },
    Leg {
        resource_path: "/echo",
        request_path: "/echo",
        verb: "POST",
        status: 200,
        traced: true,
        at_info: true,
    },
    Leg {
        resource_path: "/echo/deep",
        request_path: "/echo/deep",
        verb: "POST",
        status: 200,
        traced: true,
        at_info: true,
    },
    Leg {
        resource_path: "/items/{id}",
        request_path: "/items/42",
        verb: "GET",
        status: 200,
        traced: true,
        at_info: true,
    },
    Leg {
        resource_path: "/",
        request_path: "",
        verb: "POST",
        status: 200,
        traced: false,
        at_info: false,
    },
    Leg {
        resource_path: "/echo",
        request_path: "/echo",
        verb: "GET",
        status: 200,
        traced: false,
        at_info: false,
    },
    Leg {
        resource_path: "/echo/deep",
        request_path: "/echo/deep",
        verb: "GET",
        status: 200,
        traced: false,
        at_info: false,
    },
    Leg {
        resource_path: "/items/{id}",
        request_path: "/items/42",
        verb: "POST",
        status: 200,
        traced: false,
        at_info: false,
    },
    Leg {
        resource_path: "/fails",
        request_path: "/fails",
        verb: "GET",
        status: 503,
        traced: true,
        at_info: false,
    },
];

/// Drive one leg through the stage and assert what the stage's execution log holds about it.
async fn one_leg(router: &MultiServiceRouter, api: &str, leg: &Leg) {
    let case = &format!(
        "{verb} {path:?} served by {resource:?}, answered {status}",
        verb = leg.verb,
        path = leg.request_path,
        resource = leg.resource_path,
        status = leg.status,
    );
    let answered = invoke(router, api, leg.verb, leg.request_path).await;
    assert_eq!(
        (answered.status, answered.text()),
        (leg.status, served_body(leg.resource_path, leg.verb)),
        "case {case:?}: expected the answer of that method, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let request_id = answered
        .header(REQUEST_ID_HEADER)
        .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));

    // The records of THIS request, read back through CloudWatch Logs' own reader: every record of
    // a stage's execution log names the request it is about, which is what separates this
    // request's trace from the traces of the requests before it in the group. A stage that has
    // traced nothing yet has no group of its own either, which is as much "nothing was written"
    // as an empty trace in a group is.
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
    let records = delivered(router, &group).await.unwrap_or_default();
    let prefix = traced_line(&request_id, "");
    let trace: Vec<String> = records
        .iter()
        .filter(|record| record.starts_with(&prefix))
        .cloned()
        .collect();
    if !leg.traced {
        assert!(
            trace.is_empty(),
            "case {case:?}: expected no trace of this request in {group:?}, got {trace:?}"
        );
        return;
    }
    // The line that OPENS a request's log is in every trace, whichever level wrote it: it is what
    // names the request the records following it are about.
    let opens = traced_line(&request_id, &format!("Execution log for request {request_id}"));
    assert!(
        trace.contains(&opens),
        "case {case:?}: expected the record {opens:?} in this request's trace, got {trace:?}"
    );
    // The INFORMATIONAL steps of serving the request, which only a trace written at `INFO`
    // carries: a trace written at `ERROR` reports the failure and nothing beside it.
    for line in [
        traced_line(
            &request_id,
            &format!(
                "HTTP Method: {verb}, Resource Path: {resource}",
                verb = leg.verb,
                resource = leg.resource_path,
            ),
        ),
        traced_line(
            &request_id,
            &format!("Method completed with status: {}", leg.status),
        ),
    ] {
        assert_eq!(
            trace.contains(&line),
            leg.at_info,
            "case {case:?}: expected the record {line:?} in this request's trace={expected}, got \
             {trace:?}",
            expected = leg.at_info,
        );
    }
}

/// RULE: the entry keyed by the method a request reached gates its execution log, on whatever
/// resource of the stage that method sits on, and every method carrying no entry of its own stays
/// under the stage-wide entry.
#[tokio::test]
async fn the_entry_of_the_matched_method_gates_its_execution_log_on_every_resource() {
    let router = emulator();
    install_account_role(&router).await;
    let api = deployed(&router).await;
    gate(&router, &api).await;

    for leg in &LEGS {
        one_leg(&router, &api, leg).await;
    }
}
