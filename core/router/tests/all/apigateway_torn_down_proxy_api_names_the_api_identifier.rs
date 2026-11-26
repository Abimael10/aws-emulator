// SPDX-License-Identifier: CC0-1.0

//! WHICH identifier API Gateway names when a mutation addresses a REST API that was torn
//! down after it had been wired to a Lambda proxy backend, driven end to end through the
//! whole assembled router.
//!
//! Three services and one engine, so this is a router suite: API Gateway serves the control
//! plane and then the deployed stage, Lambda runs the function behind the `AWS_PROXY`
//! integration, CloudFormation archives the inline handler into the deployment package
//! Lambda runs, and IAM answers for the role Lambda runs it as.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > An API Gateway control-plane call whose own target IS the REST API — including a create
//! > that resolves `restApiId` as a precondition of its own, ahead of the parent record it
//! > would hang its new record off — refuses a `restApiId` that resolves to no stored REST
//! > API with HTTP 404 `NotFoundException` whose message is the account-qualified identifier
//! > sentence `Invalid API identifier specified <accountId>:<restApiId>`: the fixed text, the
//! > account the lookup ran in and the identifier the request sent. The fixed resource
//! > sentence belongs to a record resolved INSIDE an API that DOES exist, so it never answers
//! > an API that is absent, however that API came to be absent and whatever else the request
//! > names.
//!
//! The rows differ along what the rule generalises over: how the API went away (one deleted
//! after it had been built out, wired to a Lambda `AWS_PROXY` integration, deployed to a
//! stage and invoked through it — the teardown the recorded check drives — one deleted while
//! still flat, and identifiers no API of the account ever held), the identifier's own length,
//! alphabet and case, the parent named beside it (the torn-down API's own stale root, a stale
//! nested child of it, the live root of a surviving API, a parent no API has), the `pathPart`
//! submitted with it (a plain segment, one carrying the punctuation a segment may hold, a
//! path variable, a greedy one) and the call that resolved the identifier (the create the
//! mutation runs through, and the read of the API record beside it).
//!
//! What the teardown must NOT have changed is asserted too: Lambda still reports the function
//! the gone API's integration named, and the surviving API still serves the same create — so
//! the refusals are about the identifier the request sent and not about the requests.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling `apigateway_proxy_event_caller_identity` suite does.

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for, and the Region the integration ARN names.
const REGION: &str = "us-east-1";

/// The stage the proxy API is deployed to, and the path part its method hangs off.
const STAGE: &str = "prod";
const BACKED_PATH: &str = "events";

/// The function the integration proxies to, the stack that declares it and the role Lambda
/// assumes to run it. The handler makes no call of its own, so the role carries the trust
/// policy alone.
const FUNCTION: &str = "gone-api-proxy-backend";
const STACK: &str = "gone-api-proxy-backend-stack";
const ROLE_NAME: &str = "gone-api-proxy-backend-exec";

/// How long one invocation is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long to wait between polls of a clock-owed transition, and how many polls to make.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// The API version each awsQuery call carries.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// An access key id whose scope routes each call; the signature is never checked here.
const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The class a refused control-plane call names, and the header a `restJson1` client reads it
/// off.
const NOT_FOUND: &str = "NotFoundException";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The sentence API Gateway states for a resource address that resolves to no resource: fixed
/// text in full, with neither the identifier nor the account in it. It is the sentence the
/// recorded teardown was answered with where the API identifier's own was owed.
const NO_SUCH_RESOURCE: &str = "Invalid Resource identifier specified";

/// Identifiers no API of the account ever held, differing in length (one character, the ten
/// characters API Gateway mints, longer), alphabet (letters, digits, a mix, a hyphen) and
/// case. `restApiId` is a plain `String` label with no `@pattern`
/// (`smithy_query(query="operation", operation="CreateResource")`), so every row reaches the
/// lookup.
const NEVER_HELD: &[&str] = &["g", "zzzzzzzzzz", "0123456789", "Ab3Xy9Zq1W", "no-such-api-07"];

/// Parents no stored API has a resource under, in the same spread: a resource id is a plain
/// `String` label too.
const UNKNOWN_PARENTS: &[&str] = &["p", "yyyyyyyyyy", "9876543210", "Zq1WAb3Xy9", "no-such-res-07"];

/// `pathPart` values the grammar admits, so a row is refused for the API it names and not for
/// its body: a plain segment, one carrying the punctuation a segment may hold, a path variable
/// and a greedy one (`Resource$pathPart`).
const PATH_PARTS: &[&str] = &["gone", "post-delete", "{petId}", "{proxy+}"];

/// The handler behind the proxy integration: it reports back what the v1 proxy event says
/// about the request it was handed, so the invoke that precedes the teardown is answered by
/// the function and not by API Gateway on its own.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    context_of = event.get("requestContext") or {}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "path": event.get("path"),
                "httpMethod": event.get("httpMethod"),
                "stage": context_of.get("stage"),
            }
        ),
    }
"#;

/// One answered request: its status, the error class it named, and its body verbatim.
struct Answered {
    status: u16,
    error: Option<String>,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
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
    let error = parts
        .headers
        .get(ERROR_TYPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(':').next().unwrap_or(value).to_string());
    Answered {
        status: parts.status.as_u16(),
        error,
        body,
    }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One API Gateway control-plane call over its `restJson1` wire, whatever it answers.
async fn apigw_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let credential = authorization("apigateway");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &credential),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`; answers its body.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let answered = apigw_call(router, method, path, body).await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let credential = authorization("lambda");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &credential),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One awsQuery call to `signing_name`, which must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let credential = authorization(signing_name);
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &credential),
        ],
        Bytes::from(form),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup {signing_name} {action}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One IAM call over its awsQuery wire.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> String {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        IAM_VERSION,
        action,
        parameters,
    )
    .await
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        CLOUDFORMATION_VERSION,
        action,
        parameters,
    )
    .await
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// The sentence API Gateway states for an API identifier that resolves to no API. The account
/// segment is the emulator's own account (`engine::emulator_account_id`), never a literal, so
/// this expectation stays true of whatever account the runtime presents.
fn no_such_api(rest_api_id: &str) -> String {
    format!(
        "Invalid API identifier specified {}:{rest_api_id}",
        engine::emulator_account_id()
    )
}

/// Assert `answered` is a 404 `NotFoundException` whose message is exactly `message`.
fn assert_reports(answered: &Answered, case: &str, message: &str) {
    assert_eq!(
        answered.status, 404,
        "case {case:?}: expected HTTP 404, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.error.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected {NOT_FOUND} in {ERROR_TYPE_HEADER}, got {:?} with {:?}",
        answered.error,
        answered.text()
    );
    assert_eq!(
        answered.json()["message"].as_str(),
        Some(message),
        "case {case:?}: expected the message {message:?}, got {:?}",
        answered.text()
    );
}

/// Deploy the stack that declares the function: the inline source is archived into the
/// deployment package by CloudFormation, which is how a handler written here comes to be one
/// Lambda can run. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Backend": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    if reached != CREATE_COMPLETE {
        let events = cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        panic!(
            "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described} \
             with the events {events}"
        );
    }

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let mut configuration = Value::Null;
    for _ in 0..POLLS {
        configuration = function_configuration(router).await.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    member("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// What Lambda reports for the function right now.
async fn function_configuration(router: &MultiServiceRouter) -> Answered {
    lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        None,
    )
    .await
}

/// Create a REST API, answering the id it was minted under and the id of the root resource it
/// was created with.
async fn create_api(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let record = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    (
        member("CreateRestApi", &record, "id"),
        member("CreateRestApi", &record, "rootResourceId"),
    )
}

/// Add one child resource under `parent`, answering the id it was minted under.
async fn create_resource(
    router: &MultiServiceRouter,
    api: &str,
    parent: &str,
    path_part: &str,
) -> String {
    let record = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    member("CreateResource", &record, "id")
}

/// `CreateResource` for `path_part` under `parent` of `api`, whatever it answers — every row
/// below drives a create that is expected to be REFUSED.
async fn attempt_create_resource(
    router: &MultiServiceRouter,
    api: &str,
    parent: &str,
    path_part: &str,
) -> Answered {
    apigw_call(
        router,
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": path_part })),
    )
    .await
}

/// `DeleteRestApi` for `api`, asserting the removal was accepted.
async fn delete_api(router: &MultiServiceRouter, api: &str) {
    let answered = apigw_call(router, "DELETE", &format!("/restapis/{api}"), None).await;
    assert_eq!(
        answered.status, 202,
        "setup DeleteRestApi({api}): expected HTTP 202, got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// An API that is NOT in the store, with whatever identifiers were valid parents of it while
/// it was.
struct Absent {
    /// How this API came to be absent, named in every assertion of the row.
    how: String,
    /// The `restApiId` the rows send.
    api: String,
    /// Parents the service itself minted for this API, valid until it went away.
    stale_parents: Vec<String>,
}

#[test]
fn a_create_on_a_torn_down_proxy_api_names_the_api_identifier() {
    std::thread::Builder::new()
        .name("apigateway-torn-down-proxy-api".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                // 1. The backend the proxy integration names: the role Lambda runs the
                //    function as, the function itself, and the grant that admits an
                //    invocation from API Gateway.
                let role = iam(
                    &router,
                    "CreateRole",
                    &[
                        ("RoleName", ROLE_NAME),
                        (
                            "AssumeRolePolicyDocument",
                            &json!({
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Principal": { "Service": "lambda.amazonaws.com" },
                                    "Action": "sts:AssumeRole",
                                }],
                            })
                            .to_string(),
                        ),
                    ],
                )
                .await;
                let role_arn = element(&role, "Arn");
                let function_arn = deployed_function(&router, &role_arn).await;
                let granted = lambda(
                    &router,
                    "POST",
                    &format!("/2015-03-31/functions/{FUNCTION}/policy"),
                    Some(json!({
                        "StatementId": "apigw-invoke",
                        "Action": "lambda:InvokeFunction",
                        "Principal": "apigateway.amazonaws.com",
                    })),
                )
                .await;
                assert_eq!(
                    granted.status, 201,
                    "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
                    granted.status,
                    granted.text()
                );

                // 2. The API the recorded teardown drives: a resource behind an AWS_PROXY
                //    integration on that function, deployed to a stage.
                let (wired, wired_root) = create_api(&router, "torn-down-proxy-api").await;
                let backed = create_resource(&router, &wired, &wired_root, BACKED_PATH).await;
                let nested = create_resource(&router, &wired, &backed, "nested").await;
                let method = format!("/restapis/{wired}/resources/{backed}/methods/GET");
                apigw(
                    &router,
                    "PutMethod",
                    "PUT",
                    &method,
                    Some(json!({ "authorizationType": "NONE" })),
                    201,
                )
                .await;
                apigw(
                    &router,
                    "PutIntegration",
                    "PUT",
                    &format!("{method}/integration"),
                    Some(json!({
                        "type": "AWS_PROXY",
                        // `integrationHttpMethod` travels under its `jsonName`, which is what
                        // `smithy_query(query="operation",operation="PutIntegration")` reports.
                        "httpMethod": "POST",
                        "uri": format!(
                            "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                             {function_arn}/invocations"
                        ),
                    })),
                    201,
                )
                .await;
                apigw(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{wired}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // The stage SERVES that integration before the teardown, so what the rows
                // below address really was a live API with a Lambda backend behind it.
                let served = dispatch(
                    &router,
                    "GET",
                    &format!("{wired}.execute-api.{REGION}.amazonaws.com"),
                    &format!("/{STAGE}/{BACKED_PATH}"),
                    &[],
                    Bytes::new(),
                )
                .await;
                assert_eq!(
                    served.status, 200,
                    "setup the live stage: expected the handler's 200, got {} with {:?}",
                    served.status,
                    served.text()
                );
                let reported = served.json();
                assert_eq!(
                    reported["stage"].as_str(),
                    Some(STAGE),
                    "setup the live stage: expected the event's requestContext.stage {STAGE:?}, \
                     got {reported}"
                );
                assert_eq!(
                    reported["path"].as_str(),
                    Some(format!("/{BACKED_PATH}").as_str()),
                    "setup the live stage: expected the event's path /{BACKED_PATH}, got \
                     {reported}"
                );

                // 3. A second API, deleted while still flat, and a third that SURVIVES every
                //    teardown and supplies a parent that resolves somewhere in the account.
                let (flat, flat_root) = create_api(&router, "torn-down-flat-api").await;
                let (surviving, surviving_root) = create_api(&router, "surviving-api").await;

                delete_api(&router, &wired).await;
                delete_api(&router, &flat).await;

                let mut absent = vec![
                    Absent {
                        how: "an api deleted after it was wired to a lambda proxy backend and \
                              deployed"
                            .to_string(),
                        api: wired.clone(),
                        stale_parents: vec![wired_root.clone(), backed.clone(), nested.clone()],
                    },
                    Absent {
                        how: "an api deleted while it was still flat".to_string(),
                        api: flat.clone(),
                        stale_parents: vec![flat_root.clone()],
                    },
                ];
                for never in NEVER_HELD {
                    absent.push(Absent {
                        how: format!("an api identifier {never:?} the account never held"),
                        api: (*never).to_string(),
                        stale_parents: Vec::new(),
                    });
                }

                // 4. The rule: whatever parent and `pathPart` the create carries, the API
                //    identifier is what the refusal names.
                for row in &absent {
                    let mut parents: Vec<(String, String)> = row
                        .stale_parents
                        .iter()
                        .map(|parent| (format!("its own stale parent {parent}"), parent.clone()))
                        .collect();
                    parents.push((
                        "the live root of the surviving api".to_string(),
                        surviving_root.clone(),
                    ));
                    for unknown in UNKNOWN_PARENTS {
                        parents.push((
                            format!("the unknown parent {unknown:?}"),
                            (*unknown).to_string(),
                        ));
                    }

                    for (position, parent) in &parents {
                        for path_part in PATH_PARTS {
                            let case = format!(
                                "CreateResource(pathPart={path_part:?}) on {} with {position}",
                                row.how
                            );
                            let answered =
                                attempt_create_resource(&router, &row.api, parent, path_part)
                                    .await;
                            assert_reports(&answered, &case, &no_such_api(&row.api));
                        }
                    }

                    // The read of the API record answers the same sentence, so the mutation and
                    // the read of one absent API agree.
                    let read =
                        apigw_call(&router, "GET", &format!("/restapis/{}", row.api), None).await;
                    assert_reports(
                        &read,
                        &format!("GetRestApi on {}", row.how),
                        &no_such_api(&row.api),
                    );
                }

                // 5. The other half of the rule: a parent is resolved INSIDE the API the
                //    request names, so the fixed resource sentence answers a parent that names
                //    no resource of an API that DOES exist — including one that was a resource
                //    of the torn-down API.
                let mut unheld: Vec<(String, String)> = vec![
                    (
                        "the stale root of the torn-down proxy api".to_string(),
                        wired_root.clone(),
                    ),
                    (
                        "a stale nested resource of the torn-down proxy api".to_string(),
                        nested.clone(),
                    ),
                ];
                for unknown in UNKNOWN_PARENTS {
                    unheld.push((
                        format!("the unknown parent {unknown:?}"),
                        (*unknown).to_string(),
                    ));
                }
                for (position, parent) in &unheld {
                    for path_part in PATH_PARTS {
                        let case = format!(
                            "CreateResource(pathPart={path_part:?}) on the surviving api with \
                             {position}"
                        );
                        let answered =
                            attempt_create_resource(&router, &surviving, parent, path_part).await;
                        assert_reports(&answered, &case, NO_SUCH_RESOURCE);
                    }
                }

                // 6. The same create against the SURVIVING api and its own root is served, and
                //    reads back under the parent it named — so the refusals above are about the
                //    identifiers the requests sent and not about the requests.
                let created = create_resource(&router, &surviving, &surviving_root, "gone").await;
                let read = apigw(
                    &router,
                    "GetResource",
                    "GET",
                    &format!("/restapis/{surviving}/resources/{created}"),
                    None,
                    200,
                )
                .await;
                assert_eq!(
                    read["parentId"].as_str(),
                    Some(surviving_root.as_str()),
                    "case GetResource(the surviving api): expected the parent \
                     {surviving_root:?}, got {read}"
                );
                assert_eq!(
                    read["path"].as_str(),
                    Some("/gone"),
                    "case GetResource(the surviving api): expected the path /gone, got {read}"
                );

                // 7. Tearing the API down changed nothing in the service that OWNS the
                //    backend: Lambda still reports the function the gone integration named.
                let still = function_configuration(&router).await;
                assert_eq!(
                    still.status, 200,
                    "case GetFunctionConfiguration after the teardown: expected HTTP 200, got {} \
                     with {:?}",
                    still.status,
                    still.text()
                );
                let configuration = still.json();
                assert_eq!(
                    configuration["FunctionArn"].as_str(),
                    Some(function_arn.as_str()),
                    "case GetFunctionConfiguration after the teardown: expected the arn \
                     {function_arn:?}, got {configuration}"
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
