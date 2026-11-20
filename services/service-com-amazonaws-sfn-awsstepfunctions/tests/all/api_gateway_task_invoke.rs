// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the API GATEWAY integration
//! (`arn:aws:states:::apigateway:invoke`): the `Task` state whose unit of work is one
//! HTTP request to a deployed stage of an api.
//!
//! The rule under test: a `Task` state whose `Resource` names an integration that makes an
//! HTTP REQUEST makes the request its parameters describe and reports that endpoint's own
//! answer — here the `Method` to the stage its `ApiEndpoint` host, `Stage` and `Path` name,
//! carrying its `Headers`, its `QueryParameters` and its `RequestBody`, signed as the
//! execution role where its `AuthType` asks for that — so a 2xx answer is the task's result
//! (`StatusCode`, `StatusText`, `Headers`, `ResponseBody`) and any other status fails the
//! task under the error name that status gives it in this integration's own family
//! (`ApiGateway.NNN`), with the body the api answered as the cause.
//!
//! Every row drives both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and API Gateway mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — against an api whose methods, `MOCK`
//! integrations and deployment are created through API Gateway's own control plane. So the
//! status, the headers and the body the task reports are the ones that api's own
//! `execute-api` data plane computed from those records: an answer this emulator made up
//! rather than read off the stage fails here, as does a request whose members never
//! reached it.
//!
//! The rows differ along what the rule generalises over: the method, which path of the api
//! is addressed, what the request carries (a query parameter, a header, a JSON body), which
//! status the stage answers (a 2xx, a client error, a server error), which authentication
//! method the task asks for, and the ways the parameters can describe no request at all.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

// The reading of a response body is this test binary's own, stated once in the suite that
// first needed it.
use crate::lambda_task_control_flow::parsed;
use crate::state_machine_lifecycle::{string, PAST_THE_EXECUTION_DELAY, REGION, ROLE_ARN};

/// The `Resource` that names the API Gateway integration.
const APIGATEWAY_INVOKE: &str = "arn:aws:states:::apigateway:invoke";

/// The stage the api below is deployed to.
const STAGE: &str = "prod";

/// How one poll of something the clock owes sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row waits for an execution to leave `RUNNING`, and for an event of a
/// suspended one to be recorded.
///
/// Generous on purpose: the spans this waits out are the service's own (one
/// [`PAST_THE_EXECUTION_DELAY`]), so the deadline is there to end a wait that is never
/// going to be satisfied rather than to bound how fast the host settles one.
const PATIENCE: Duration = PAST_THE_EXECUTION_DELAY.saturating_add(Duration::from_secs(180));

/// How one service's requests are addressed on the wire: `smithy_query(query=
/// "service_info")` of each model — Step Functions' signing name, `X-Amz-Target` prefix and
/// `awsJson` version, and API Gateway's signing name, whose control plane is `restJson1`.
const SFN_SIGNING_NAME: &str = "states";
/// See [`SFN_SIGNING_NAME`].
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";
/// See [`SFN_SIGNING_NAME`].
const APIGATEWAY_SIGNING_NAME: &str = "apigateway";

/// The header the api echoes back, and the value a row sends under it.
const TRACE_HEADER: &str = "x-trace";
/// See [`TRACE_HEADER`].
const TRACE_VALUE: &str = "t-1";

/// Step Functions and API Gateway over ONE engine, so the request a task makes is served
/// by the very api stage the test deployed.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: std::sync::Arc<engine::InMemoryEngine> =
            std::sync::Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(std::sync::Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` header whose credential scope names `service`, which is
    /// what routes the request. The signature is never checked: this router runs with
    /// request authentication off.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host, Signature=0123456789abcdef"
        )
    }

    /// One Step Functions call, requiring a 200.
    async fn sfn(&self, case: &str, operation: &str, body: &Value) -> Value {
        let host = format!("{SFN_SIGNING_NAME}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{SFN_TARGET_PREFIX}.{operation}"))
            .header("authorization", Self::authorization(SFN_SIGNING_NAME))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = parsed(response.into_body());
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One call to API Gateway's own control plane, as its status and parsed body.
    async fn api_gateway(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map(|body| body.to_string()).unwrap_or_default();
        let host = format!("{APIGATEWAY_SIGNING_NAME}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", &host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                Self::authorization(APIGATEWAY_SIGNING_NAME),
            )
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed(response.into_body()))
    }

    /// One accepted control-plane call, as its parsed body.
    async fn accepted(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let (status, answered) = self.api_gateway(method, uri, body).await;
        assert!(
            (200..300).contains(&status),
            "setup {method} {uri}: expected the call to be accepted, got {status} {answered}"
        );
        answered
    }

    /// Create a resource under `parent` and answer its id.
    async fn resource(&self, api: &str, parent: &str, part: &str) -> String {
        let created = self
            .accepted(
                "POST",
                &format!("/restapis/{api}/resources/{parent}"),
                Some(json!({ "pathPart": part })),
            )
            .await;
        string(&created, "id")
    }

    /// Put one open `MOCK` method on `resource`: its request template IS what the
    /// integration answers, so the `statusCode` that template names is the integration
    /// response the call maps to, and that response's own template renders the body the
    /// stage serves.
    ///
    /// `answers` is one `(status, response template)` pair per branch the method declares.
    async fn mock_method(
        &self,
        api: &str,
        resource: &str,
        verb: &str,
        request_template: &str,
        answers: &[(&str, &str)],
    ) {
        let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
        self.accepted("PUT", &method, Some(json!({ "authorizationType": "NONE" })))
            .await;
        self.accepted(
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": request_template },
            })),
        )
        .await;
        for (status, template) in answers {
            self.accepted(
                "PUT",
                &format!("{method}/integration/responses/{status}"),
                Some(json!({ "responseTemplates": { "application/json": template } })),
            )
            .await;
        }
    }

    /// Stand the api up: the four paths the rows address, deployed to one stage. Answers
    /// the api's id.
    ///
    /// Each path is served by a `MOCK` integration, so what the stage answers is computed
    /// from the api's own records rather than from a backend this emulator would have to
    /// reach: the `echo` path renders the path parameter, the query parameter and the
    /// header the request carried, the `created` path renders the body it was sent, and the
    /// two refusing paths answer a client and a server status.
    async fn given_api(&self) -> String {
        let created = self
            .accepted("POST", "/restapis", Some(json!({ "name": "task-target" })))
            .await;
        let api = string(&created, "id");
        let root = string(&created, "rootResourceId");

        let echo = self.resource(&api, &root, "echo").await;
        let one = self.resource(&api, &echo, "{id}").await;
        self.mock_method(
            &api,
            &one,
            "GET",
            r#"{"statusCode": 200}"#,
            &[(
                "200",
                &format!(
                    r#"{{"at":"echo","id":"$input.params('id')","q":"$input.params('q')","trace":"$input.params('{TRACE_HEADER}')"}}"#
                ),
            )],
        )
        .await;
        self.mock_method(
            &api,
            &one,
            "POST",
            r#"{"statusCode": 201, "sent": "$util.escapeJavaScript($input.body)"}"#,
            &[("201", r#"{"at":"created","sent":$input.json('$.sent')}"#)],
        )
        .await;

        let refusing = self.resource(&api, &root, "refusing").await;
        self.mock_method(
            &api,
            &refusing,
            "GET",
            r#"{"statusCode": 403}"#,
            &[("403", r#"{"message":"this path refuses the caller"}"#)],
        )
        .await;
        let faulty = self.resource(&api, &root, "faulty").await;
        self.mock_method(
            &api,
            &faulty,
            "GET",
            r#"{"statusCode": 500}"#,
            &[("500", r#"{"message":"this path is broken"}"#)],
        )
        .await;

        self.accepted(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
        )
        .await;
        api
    }

    /// Create `definition` as a state machine and start one execution of it, answering that
    /// execution's ARN.
    async fn started(&self, case: &str, name: &str, definition: &Value) -> String {
        let created = self
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        let machine = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                case,
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "only", "input": "{}" }),
            )
            .await;
        string(&started, "executionArn")
    }

    /// The execution `execution`, once it has left `RUNNING`.
    async fn awaited(&self, case: &str, execution: &str) -> Value {
        let began = std::time::Instant::now();
        loop {
            let described = self
                .sfn(case, "DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, last saw \
                 {described}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Run `definition` and report the execution it came to.
    async fn ran(&self, case: &str, name: &str, definition: &Value) -> Value {
        let execution = self.started(case, name, definition).await;
        self.awaited(case, &execution).await
    }
}

/// One `Task` state named `Call` whose work is the API Gateway request `parameters`
/// describes, under the `Resource` `resource`.
fn one_task(resource: &str, parameters: Value) -> Value {
    json!({
        "StartAt": "Call",
        "States": {
            "Call": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "End": true,
            },
        },
    })
}

/// What an execution must come to for one row.
enum Outcome {
    /// It SUCCEEDED: the task's result must carry this status and reason phrase, and each
    /// of these members of the body the stage served.
    Succeeded {
        status: u64,
        status_text: &'static str,
        answered: &'static [(&'static str, &'static str)],
    },
    /// It FAILED under this error name, with a cause carrying each of these fragments.
    Failed {
        error: &'static str,
        cause_has: &'static [&'static str],
    },
}

/// One row: the `Resource`, the parameters the task is given — built from the api's own
/// hostname, which only exists once the api does — and what the execution must come to.
struct Row {
    case: &'static str,
    resource: &'static str,
    parameters: fn(&str) -> Value,
    outcome: Outcome,
}

/// The hostname an api of API Gateway is reached on, which is what `ApiEndpoint` names.
fn api_endpoint(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

const ROWS: &[Row] = &[
    Row {
        case: "a GET on a path of the stage, carrying a query parameter and a header",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": STAGE,
                "Path": "echo/42",
                "QueryParameters": { "q": "ada" },
                "Headers": { TRACE_HEADER: TRACE_VALUE },
            })
        },
        outcome: Outcome::Succeeded {
            status: 200,
            status_text: "OK",
            answered: &[("at", "echo"), ("id", "42"), ("q", "ada"), ("trace", TRACE_VALUE)],
        },
    },
    Row {
        case: "a POST carrying a JSON body, which the stage answers with its own status",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "POST",
                "Stage": STAGE,
                "Path": "echo/7",
                "RequestBody": { "name": "ada" },
            })
        },
        outcome: Outcome::Succeeded {
            status: 201,
            status_text: "Created",
            answered: &[("at", "created"), ("sent", r#"{"name":"ada"}"#)],
        },
    },
    Row {
        case: "the path and the stage written with the separators round them",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": "/prod/",
                "Path": "/echo/13/",
            })
        },
        outcome: Outcome::Succeeded {
            status: 200,
            status_text: "OK",
            answered: &[("at", "echo"), ("id", "13")],
        },
    },
    Row {
        case: "the request signed as the execution role, which the open method still serves",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": STAGE,
                "Path": "echo/99",
                "AuthType": "IAM_ROLE",
            })
        },
        outcome: Outcome::Succeeded {
            status: 200,
            status_text: "OK",
            answered: &[("at", "echo"), ("id", "99")],
        },
    },
    Row {
        case: "a path the stage answers with a client error",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": STAGE,
                "Path": "refusing",
            })
        },
        outcome: Outcome::Failed {
            error: "ApiGateway.403",
            cause_has: &[r#"{"message":"this path refuses the caller"}"#],
        },
    },
    Row {
        case: "a path the stage answers with a server error",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": STAGE,
                "Path": "faulty",
            })
        },
        outcome: Outcome::Failed {
            error: "ApiGateway.500",
            cause_has: &[r#"{"message":"this path is broken"}"#],
        },
    },
    Row {
        case: "a hostname no api of this emulator answers on",
        resource: APIGATEWAY_INVOKE,
        parameters: |_api| {
            json!({
                "ApiEndpoint": "nothing-serves-this.example.test",
                "Method": "GET",
                "Stage": STAGE,
                "Path": "echo/42",
            })
        },
        outcome: Outcome::Failed {
            error: "States.Runtime",
            cause_has: &["nothing in this emulator serves"],
        },
    },
    Row {
        case: "an AuthType that is none of the integration's authentication methods",
        resource: APIGATEWAY_INVOKE,
        parameters: |api| {
            json!({
                "ApiEndpoint": api_endpoint(api),
                "Method": "GET",
                "Stage": STAGE,
                "Path": "echo/42",
                "AuthType": "SIGV4",
            })
        },
        outcome: Outcome::Failed {
            error: "States.Runtime",
            cause_has: &["which is none of 'NO_AUTH', 'IAM_ROLE' and 'RESOURCE_POLICY'"],
        },
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_api_gateway_task_calls_the_stage_and_reports_its_own_answer() {
    let chain = Chain::new();
    let api = chain.given_api().await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let definition = one_task(row.resource, (row.parameters)(&api));
        let described = chain
            .ran(case, &format!("machine-{index}"), &definition)
            .await;
        let status = string(&described, "status");
        match &row.outcome {
            Outcome::Succeeded {
                status: answered,
                status_text,
                answered: members,
            } => {
                assert_eq!(
                    status, "SUCCEEDED",
                    "case {case:?}: expected the task to report the stage's answer, got \
                     {described}"
                );
                let result: Value = serde_json::from_str(&string(&described, "output"))
                    .expect("the execution's output is a JSON document");
                assert_eq!(
                    result.get("StatusCode").and_then(Value::as_u64),
                    Some(*answered),
                    "case {case:?}: expected the status the stage answered, got {result}"
                );
                assert_eq!(
                    result.get("StatusText").and_then(Value::as_str),
                    Some(*status_text),
                    "case {case:?}: expected the reason phrase of that status, got {result}"
                );
                let body = result
                    .get("ResponseBody")
                    .cloned()
                    .unwrap_or(Value::Null);
                for (member, expected) in *members {
                    assert_eq!(
                        body.get(*member).and_then(Value::as_str),
                        Some(*expected),
                        "case {case:?}: expected the stage's own body to carry {member}={expected:?}, \
                         got {body}"
                    );
                }
                assert!(
                    result
                        .get("Headers")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| !headers.is_empty()),
                    "case {case:?}: expected the headers the stage answered, got {result}"
                );
            }
            Outcome::Failed { error, cause_has } => {
                assert_eq!(
                    status, "FAILED",
                    "case {case:?}: expected the task to fail, got {described}"
                );
                let reported = string(&described, "error");
                assert_eq!(
                    reported, *error,
                    "case {case:?}: expected the error name {error:?}, got {described}"
                );
                let cause = string(&described, "cause");
                for fragment in *cause_has {
                    assert!(
                        cause.contains(fragment),
                        "case {case:?}: expected a cause carrying {fragment:?}, got {cause:?}"
                    );
                }
            }
        }
    }
}

/// The `.waitForTaskToken` form of the integration: the request is made exactly as the
/// request-response form makes it, and the task is finished by a `SendTask*` quoting the
/// token the state's parameters carried rather than by what the stage answered — so the
/// execution is still running once the task has started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_callback_form_waits_for_its_token_once_the_request_is_made() {
    let chain = Chain::new();
    let api = chain.given_api().await;
    let definition = one_task(
        "arn:aws:states:::apigateway:invoke.waitForTaskToken",
        json!({
            "ApiEndpoint": api_endpoint(&api),
            "Method": "GET",
            "Stage": STAGE,
            "Path": "echo/42",
            "Headers": { "x-task-token.$": "$$.Task.Token" },
        }),
    );
    let case = "the callback form";
    let execution = chain.started(case, "callback-machine", &definition).await;

    // The task has STARTED: the walk recorded the submission and made the request. Waited
    // for by looking again rather than by a span, so the row observes the event itself.
    let began = std::time::Instant::now();
    let types = loop {
        let history = chain
            .sfn(case, "GetExecutionHistory", &json!({ "executionArn": execution }))
            .await;
        let types: Vec<String> = history
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.get("type").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if types.iter().any(|named| named == "TaskStarted") {
            break types;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the task to start within {PATIENCE:?}, last saw {types:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    };
    assert!(
        !types.iter().any(|named| named == "TaskSucceeded"),
        "case {case:?}: expected no task outcome while the token is unanswered, got {types:?}"
    );
    let described = chain
        .sfn(case, "DescribeExecution", &json!({ "executionArn": execution }))
        .await;
    assert_eq!(
        string(&described, "status"),
        "RUNNING",
        "case {case:?}: expected the execution to wait for its token, got {described}"
    );
}
