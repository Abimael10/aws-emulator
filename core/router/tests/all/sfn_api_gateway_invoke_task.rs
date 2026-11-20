// SPDX-License-Identifier: CC0-1.0

//! The optimized Amazon API Gateway invoke a Step Functions `Task` state names
//! (`arn:aws:states:::apigateway:invoke`), driven across the services the question spans —
//! Step Functions runs the state and makes the request, IAM holds the execution role, and
//! API Gateway's own `execute-api` data plane serves the deployed stage and decides the
//! call against the api's resource policy — mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a `Task` state whose `Resource` is `arn:aws:states:::apigateway:invoke` PERFORMS
//! the HTTP request its parameters describe on the deployed api stage they address —
//! `ApiEndpoint` the api's invoke hostname, `Stage` and `Path` the request's path,
//! `Method` its verb, `AuthType` the authorization it carries — and the stage's own answer
//! is what the task reports: a 2xx as the task's result, under the four members an
//! endpoint's answer is reported by (`StatusCode`, `StatusText`, `Headers`,
//! `ResponseBody`), and any other status as the task's FAILURE in the `ApiGateway` error
//! family, named by the exception CLASS the answer identified itself with and by its
//! STATUS where the answer named none, with the BODY that answer carried as the cause —
//! except for an
//! authorization refusal, whose cause is Step Functions' own sentence about the action an
//! invoke asks for rather than the endpoint's own body about the principal and the method.
//!
//! The rows differ along what the rule generalises over: the verb the parameters name, the
//! stage of the api they address, the authorization they ask for, whether the api's
//! resource policy allows the call, and what the answer is — a success, a non-2xx the
//! method's own backend produced, and a refusal the endpoint words itself.
//!
//! Every assertion reads the outcome back through Step Functions' own readers, and the
//! served rows read the method's answer, which is rendered from the request's OWN context
//! (`$context.httpMethod`, `$context.stage`) — so a task that did not really reach the
//! stage it addressed cannot pass.
//!
//! The Step Functions half of the harness — the role, the state machine and the execution
//! reader — is `crate::sfn_task_target_authorization`'s: `tests/all/main.rs` is one crate
//! root, so one wiring serves every suite that drives an execution.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, ran_with_input, role, sfn, string, task_failure, REGION,
};

/// The `Resource` every state machine below names.
const INVOKE_RESOURCE: &str = "arn:aws:states:::apigateway:invoke";

/// The two stages each api is deployed to.
const LIVE: &str = "live";
const BETA: &str = "beta";

/// The resource paths the apis carry: one method that answers a success, and one whose
/// `MOCK` integration answers a client error.
const SERVED_PATH: &str = "orders";
const NOT_FOUND_PATH: &str = "missing";

/// The document the method whose backend answers a client error renders.
const NOT_FOUND_BODY: &str = r#"{"served":"not-found"}"#;

/// The sentence Step Functions words a failed invoke the api's own authorization refused
/// with: the action an invoke asks for and no resource, so the principal and the method ARN
/// the endpoint's own body names reach no definition.
const NOT_AUTHORIZED_CAUSE: &str = "Not authorized to perform: execute-api:Invoke on the resource";

/// The clause the `execute-api` endpoint's own body refuses such a call with, which is the
/// sentence IAM words an authorization failure in — what the ENDPOINT answered, and so what
/// the task's cause must NOT carry.
const NOT_AUTHORIZED_CLAUSE: &str = "is not authorized to perform: execute-api:Invoke";

/// The three `AuthType` values this integration documents.
const NO_AUTH: &str = "NO_AUTH";
const IAM_ROLE: &str = "IAM_ROLE";
const RESOURCE_POLICY: &str = "RESOURCE_POLICY";

/// The four services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: the signature a signed invoke carries is
    // the emulator's own to verify, and an unsigned one carries none at all.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::from(payload))
        .expect("a well-formed control-plane request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A required string member of a control-plane answer.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: expected a {name} in {body}"))
        .to_string()
}

/// The host the api's own invoke endpoint answers on, which is what a task's `ApiEndpoint`
/// names (`{api-id}.execute-api.{region}.amazonaws.com`).
fn invoke_host(api_id: &str) -> String {
    format!("{api_id}.execute-api.{REGION}.amazonaws.com")
}

/// One `MOCK` method on its own resource under the api's root, answering `status`.
///
/// A `MOCK` needs no backend — "for testing without actually invoking the backend"
/// (`IntegrationType`) — and its request template IS what the integration answered, so the
/// `statusCode` that template names is the result the integration responses are selected
/// by: the patterned branch for a result in its class, and the unpatterned branch for every
/// result no pattern covers. The success branch renders the request's OWN context, so an
/// answer proves which verb and which stage served it.
async fn mock_method(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    verbs: &[&str],
    status: u16,
) {
    let case = format!("the {path_part} resource");
    let created = apigw(
        router,
        &case,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    let resource = member(&case, &created, "id");
    for verb in verbs {
        let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
        apigw(
            router,
            &case,
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            &case,
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "MOCK",
                "requestTemplates": {
                    "application/json": format!("{{\"statusCode\": {status}}}"),
                },
            })),
            201,
        )
        .await;
        apigw(
            router,
            &case,
            "PUT",
            &format!("{method}/responses/200"),
            Some(json!({})),
            201,
        )
        .await;
        apigw(
            router,
            &case,
            "PUT",
            &format!("{method}/integration/responses/200"),
            Some(json!({
                "responseTemplates": {
                    "application/json":
                        "{\"method\":\"$context.httpMethod\",\"stage\":\"$context.stage\"}",
                },
            })),
            201,
        )
        .await;
        if status != 200 {
            apigw(
                router,
                &case,
                "PUT",
                &format!("{method}/responses/{status}"),
                Some(json!({})),
                201,
            )
            .await;
            apigw(
                router,
                &case,
                "PUT",
                &format!("{method}/integration/responses/{status}"),
                Some(json!({
                    "selectionPattern": format!(r"{}\d{{2}}", status / 100),
                    "responseTemplates": { "application/json": NOT_FOUND_BODY },
                })),
                201,
            )
            .await;
        }
    }
}

/// An api called `name` carrying `policy` as its resource policy, with a `GET` and a `POST`
/// method that answer a success and a `GET` whose backend answers a client error, deployed
/// to both stages. Answers the api's id.
async fn deployed_api(router: &MultiServiceRouter, name: &str, policy: Option<&str>) -> String {
    let mut request = json!({ "name": name });
    if let Some(document) = policy {
        request["policy"] = json!(document);
    }
    let created = apigw(router, name, "POST", "/restapis", Some(request), 201).await;
    let api = member(name, &created, "id");
    let root = member(name, &created, "rootResourceId");
    mock_method(router, &api, &root, SERVED_PATH, &["GET", "POST"], 200).await;
    mock_method(router, &api, &root, NOT_FOUND_PATH, &["GET"], 404).await;
    for stage in [LIVE, BETA] {
        apigw(
            router,
            name,
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
            201,
        )
        .await;
    }
    api
}

/// A resource policy that DENIES every caller the invoke, written in the
/// `execute-api:/{stage}/{verb}/{path}` shorthand API Gateway's own policy examples use.
fn denying_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": "*",
            "Action": "execute-api:Invoke",
            "Resource": "execute-api:/*",
        }],
    })
    .to_string()
}

/// What the stage owes one row.
enum Expected {
    /// The stage SERVED the request: the status it answered, and the document it rendered.
    Served { status: u64, body: Value },
    /// The stage refused it, or its backend answered a non-2xx: the error name the task
    /// reports, and the cause it carries.
    Failed {
        error: &'static str,
        cause: Cause,
    },
}

/// What a failed row's cause must be: the body the stage answered or the service's own
/// sentence for an authorization refusal, each read exactly, or a phrase a cause holds.
enum Cause {
    /// The cause carries this phrase of the answer the stage gave.
    #[allow(dead_code)]
    Holds(&'static str),
    /// The cause IS this text.
    Is(&'static str),
}

/// One row: the case, the task's parameters, and what the stage owes it.
struct Row {
    case: &'static str,
    parameters: Value,
    expected: Expected,
}

/// RULE: the optimized `apigateway:invoke` integration performs the request its parameters
/// describe on the stage they address, reports the stage's own 2xx answer as the task's
/// result, and fails the task in the `ApiGateway` error family — by the class the answer
/// named, else by its status — for every other answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_api_gateway_task_invokes_the_stage_its_parameters_address() {
    let router = emulator();
    let served = deployed_api(&router, "sfn-apigw-served", None).await;
    let denied = deployed_api(&router, "sfn-apigw-denied", Some(&denying_policy())).await;
    let served_host = invoke_host(&served);
    let denied_host = invoke_host(&denied);
    // The role every state machine below runs as: the log writes a state machine makes, and
    // the invoke grant an `IAM_ROLE` task's own documentation tells a caller to attach.
    let role_arn = role(
        &router,
        "ApiGatewayInvokeRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("execute-api:Invoke", "*")],
        }),
    )
    .await;

    let rows = [
        Row {
            case: "a GET the stage serves, with no authorization asked for",
            parameters: json!({
                "ApiEndpoint": served_host,
                "Method": "GET",
                "Stage": LIVE,
                "Path": SERVED_PATH,
            }),
            expected: Expected::Served {
                status: 200,
                body: json!({ "method": "GET", "stage": LIVE }),
            },
        },
        Row {
            case: "a POST carrying a body, signed as the execution role",
            parameters: json!({
                "ApiEndpoint": served_host,
                "Method": "POST",
                "Stage": LIVE,
                "Path": SERVED_PATH,
                "AuthType": IAM_ROLE,
                "RequestBody": { "item": "42", "note": Value::Null },
            }),
            expected: Expected::Served {
                status: 200,
                body: json!({ "method": "POST", "stage": LIVE }),
            },
        },
        Row {
            case: "another stage of the same api, with the query it reads",
            parameters: json!({
                "ApiEndpoint": served_host,
                "Method": "GET",
                "Stage": BETA,
                "Path": SERVED_PATH,
                "QueryParameters": { "dryRun": "true" },
                "AuthType": NO_AUTH,
            }),
            expected: Expected::Served {
                status: 200,
                body: json!({ "method": "GET", "stage": BETA }),
            },
        },
        Row {
            case: "a method whose own backend answers a client error",
            parameters: json!({
                "ApiEndpoint": served_host,
                "Method": "GET",
                "Stage": LIVE,
                "Path": NOT_FOUND_PATH,
            }),
            expected: Expected::Failed {
                error: "ApiGateway.404",
                // The cause IS the body the method's own backend answered: the error name
                // already carries the status, and what a `Catch` path parses is that
                // document.
                cause: Cause::Is(NOT_FOUND_BODY),
            },
        },
        Row {
            case: "an api whose resource policy denies the caller",
            parameters: json!({
                "ApiEndpoint": denied_host,
                "Method": "GET",
                "Stage": LIVE,
                "Path": SERVED_PATH,
            }),
            expected: Expected::Failed {
                error: "ApiGateway.AccessDeniedException",
                cause: Cause::Is(NOT_AUTHORIZED_CAUSE),
            },
        },
        Row {
            case: "the same denial reached by a task asking for the api's resource policy",
            parameters: json!({
                "ApiEndpoint": denied_host,
                "Method": "GET",
                "Stage": LIVE,
                "Path": SERVED_PATH,
                "AuthType": RESOURCE_POLICY,
            }),
            expected: Expected::Failed {
                error: "ApiGateway.AccessDeniedException",
                cause: Cause::Is(NOT_AUTHORIZED_CAUSE),
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let machine_arn = machine(
            &router,
            &format!("sfn-apigw-task-{index}"),
            INVOKE_RESOURCE,
            &row.parameters,
            &role_arn,
        )
        .await;
        let (execution_arn, described) = ran_with_input(&router, &machine_arn, None, case).await;
        match &row.expected {
            Expected::Served { status, body } => {
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the task to report the stage's answer, got \
                     {described:?}"
                );
                let output = string(&described, "output");
                let result: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
                    panic!("case {case:?}: expected the invoke's result, got {output:?} ({error})")
                });
                assert_eq!(
                    result.get("StatusCode").and_then(Value::as_u64),
                    Some(*status),
                    "case {case:?}: expected the status the stage answered, got {result:?}"
                );
                assert_eq!(
                    result.get("StatusText").and_then(Value::as_str),
                    Some("OK"),
                    "case {case:?}: expected the reason phrase of that status, got {result:?}"
                );
                assert_eq!(
                    result.get("ResponseBody"),
                    Some(body),
                    "case {case:?}: expected the document the method rendered from the \
                     request's own context, got {result:?}"
                );
                assert!(
                    result
                        .get("Headers")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| !headers.is_empty()),
                    "case {case:?}: expected the headers the stage answered, got {result:?}"
                );
            }
            Expected::Failed { error, cause } => {
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("FAILED"),
                    "case {case:?}: expected the task's failure to end the execution, got \
                     {described:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(*error),
                    "case {case:?}: expected the error name the answer gives this task, got \
                     {described:?}"
                );
                let carried = string(&described, "cause");
                match cause {
                    Cause::Holds(phrase) => assert!(
                        carried.contains(phrase),
                        "case {case:?}: expected the cause to carry {phrase:?}, got \
                         {carried:?}"
                    ),
                    Cause::Is(sentence) => {
                        assert_eq!(
                            carried, *sentence,
                            "case {case:?}: expected the cause {sentence:?}, got {carried:?}"
                        );
                        // The endpoint's own body named the principal it refused and the
                        // method ARN it refused them on; neither reaches the definition.
                        assert!(
                            !carried.contains(NOT_AUTHORIZED_CLAUSE),
                            "case {case:?}: expected the endpoint's own sentence to stay out \
                             of the cause, got {carried:?}"
                        );
                    }
                }
                // The same name and cause on the task's own event, which is what a `Catch`
                // is matched against.
                let details = task_failure(&router, &execution_arn, case).await;
                assert_eq!(
                    details.get("error").and_then(Value::as_str),
                    Some(*error),
                    "case {case:?}: expected the task event to name the same error, got \
                     {details:?}"
                );
            }
        }
    }
}

/// RULE: the stage decides the call, so a `Catch` written for the `ApiGateway` error family
/// routes a refusal the api's own document produced — the handler runs and the execution
/// ends at it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_on_the_api_gateway_error_routes_a_resource_policy_denial() {
    let router = emulator();
    let denied = deployed_api(&router, "sfn-apigw-caught", Some(&denying_policy())).await;
    let role_arn = role(
        &router,
        "ApiGatewayCaughtRole",
        &json!({ "Version": "2012-10-17", "Statement": [logs_only()] }),
    )
    .await;
    let definition = json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": INVOKE_RESOURCE,
                "Parameters": {
                    "ApiEndpoint": invoke_host(&denied),
                    "Method": "GET",
                    "Stage": LIVE,
                    "Path": SERVED_PATH,
                },
                "Catch": [{
                    "ErrorEquals": ["ApiGateway.AccessDeniedException"],
                    "Next": "Denied",
                    "ResultPath": "$.denial",
                }],
                "End": true,
            },
            "Denied": { "Type": "Pass", "End": true },
        },
    })
    .to_string();
    let created = sfn(
        &router,
        "CreateStateMachine",
        &json!({
            "name": "sfn-apigw-caught",
            "definition": definition,
            "roleArn": role_arn,
        }),
    )
    .await;
    let machine_arn = string(&created, "stateMachineArn");
    let case = "a Catch on the ApiGateway error family";
    let (_arn, described) = ran_with_input(&router, &machine_arn, None, case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the Catch to route the denial, got {described:?}"
    );
    let output = string(&described, "output");
    let caught: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the caught failure, got {output:?} ({error})")
    });
    assert_eq!(
        caught["denial"]["Error"].as_str(),
        Some("ApiGateway.AccessDeniedException"),
        "case {case:?}: expected the error the Catch matched, got {caught:?}"
    );
    let cause = caught["denial"]["Cause"].as_str().unwrap_or_default();
    assert_eq!(
        cause, NOT_AUTHORIZED_CAUSE,
        "case {case:?}: expected the refusal's own cause, got {cause:?}"
    );
}
