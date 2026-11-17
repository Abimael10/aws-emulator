// SPDX-License-Identifier: CC0-1.0

//! An API Gateway CONTROL-PLANE call made from inside a RUNNING handler, and the IAM decision
//! taken on it: Lambda, Amazon API Gateway, IAM and the token service on ONE engine, a
//! `python3.x` function whose code does `import boto3` and calls the control plane, and what
//! its calls did read back where a client reads it.
//!
//! RULE: a call an SDK client built inside a running handler makes travels to the service the
//! client names over that service's own protocol and is signed as the session of the
//! function's EXECUTION ROLE, so the serving service decides it against the policies in force
//! on that role at the instant of the call — and for this service the action is
//! `apigateway:<the HTTP method of the operation's `@http` binding>` on the ARN whose resource
//! portion is the request's own URI path. A decision that allows the call answers the
//! operation's own modelled output and its effect is held by the service; a decision that does
//! not is HTTP 403 `AccessDeniedException` naming the session, the action and that resource,
//! taken before any record is read — so a missing grant never reads as a missing resource, and
//! nothing lands. Either way the answer is a SERVICE answer the SDK raises as
//! `botocore.exceptions.ClientError`, never a client-side fault with no code at all, and the
//! invocation itself still answers 200 with no `FunctionError`.
//!
//! The rows differ along what the rule generalises over: WHICH HTTP method the operation binds
//! and therefore which action is decided (`GET`, `POST`, `PATCH`, `DELETE`), and how the
//! request addresses its resource (no path label at all, one label, one label and a sub-path,
//! two labels) — each row asserted in BOTH phases, refused while the role carries no statement
//! and served once the role carries the statement for that action and that resource, with one
//! method left ungranted in the second phase so the decision is shown to be per action rather
//! than per service.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The `x-www-form-urlencoded` encoder an awsQuery parameter travels under, and the one .zip
// writer this crate's suites package a handler with — each exported by the suite that first
// needed it rather than written again here.
use crate::event_source_mapping_role_access::form_encode;
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, and the one the invocation — and therefore
/// the client its handler builds — runs in.
const REGION: &str = "us-east-1";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long a wait for a condition the emulator owes may take before the suite gives up.
/// Generous on purpose: what is asserted is that the condition ARRIVES, never how fast the
/// host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long between two looks at a condition that has not arrived yet.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The access-key id standing for the account's own published credential, which is what every
/// request this suite makes on its own behalf presents: neither the token service nor IAM
/// holds a record of it, so it speaks for the account itself.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The name of the inline identity policy the execution role carries.
const INLINE_POLICY: &str = "inline";

/// The role the function runs as, and the function's own name — together they name the
/// session a handler's calls are made as.
const ROLE: &str = "apigateway-handler-role";
const FUNCTION: &str = "apigateway-sdk-caller";

/// The API the handler acts on, and the stage its deployment publishes.
const API_NAME: &str = "orders-api";
const STAGE: &str = "prod";

/// The error code and status AWS states an authorization failure with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The action namespace API Gateway publishes its control-plane actions under: a statement
/// spells one `apigateway:<HTTP method>`, which is what makes the action a fact about the
/// operation's own `@http` binding rather than about its name.
const ACTION_PREFIX: &str = "apigateway";

/// Lambda, Amazon API Gateway, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router and signed as the session of its own execution role, so the API it acts on is
/// the API this suite created and the policies that decide its calls are the ones written here
/// with IAM's own operations.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        // The handler's OWN call is signed by the runtime, with the credential it was given.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
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

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
    /// the request and where the Region comes from. The signature is never checked here.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One `restJson1` call to the service answering at the endpoint host `signing_name`: its
    /// status, its headers and its parsed body.
    async fn rest(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", Self::authorization(signing_name))
            .header("content-type", "application/json")
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        let parsed: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (parts.status.as_u16(), parts.headers, parsed)
    }

    /// One API Gateway call made as a CLIENT does, answering its status and its body: the
    /// suite reads the service's own records through its own API, so a call the handler
    /// reported without doing the work fails here.
    async fn apigateway(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body
            .map(|value| serde_json::to_vec(value).expect("the request body serializes"))
            .unwrap_or_default();
        let (status, _headers, parsed) = self.rest("apigateway", method, path, &payload).await;
        (status, parsed)
    }

    /// The same, asserting the call was SERVED — what every step of this suite's own setup
    /// expects, since the account's own credential is not refused by IAM.
    async fn served(&self, what: &str, method: &str, path: &str, body: Option<&Value>) -> Value {
        let (status, parsed) = self.apigateway(method, path, body).await;
        assert!(
            (200..300).contains(&status),
            "setup {what}: expected a 2xx from {method} {path}, got {status} ({parsed})"
        );
        parsed
    }

    /// One awsQuery call to IAM that must be served.
    async fn iam(&self, what: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        assert_eq!(
            status,
            200,
            "setup {what}: IAM {action} must succeed, got {status} {}",
            String::from_utf8_lossy(response.body())
        );
    }

    /// Create the execution role, trusted by Lambda and carrying NO identity policy at all —
    /// the position a role provisioned for a function whose grants are written later is in.
    async fn execution_role(&self) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "lambda.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            "the execution role",
            "CreateRole",
            &[
                ("RoleName", ROLE.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        role_arn()
    }

    /// Put `policy` on the role as its inline identity policy, which is the write that puts a
    /// grant in force on every session of that role.
    async fn grant(&self, policy: &str) {
        self.iam(
            "the role's inline policy",
            "PutRolePolicy",
            &[
                ("RoleName", ROLE.to_string()),
                ("PolicyName", INLINE_POLICY.to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }

    /// Create the function, running as `role_arn`, and wait until it reports itself `Active`
    /// — the state the clock owes a create, waited for as a CONDITION.
    async fn function(&self, role_arn: &str, source: &str) {
        let (status, _headers, created) = self
            .rest(
                "lambda",
                "POST",
                FUNCTIONS_URI,
                &serde_json::to_vec(&json!({
                    "FunctionName": FUNCTION,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": 30,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD
                            .encode(package(&[("lambda_function.py", source)])),
                    },
                }))
                .expect("the create body serializes"),
            )
            .await;
        assert_eq!(
            status, 201,
            "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
        );
        let started = Instant::now();
        loop {
            let (_status, _headers, read) = self
                .rest(
                    "lambda",
                    "GET",
                    &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
                    &[],
                )
                .await;
            if read["State"] == json!("Active") {
                return;
            }
            assert!(
                started.elapsed() < SETTLE_DEADLINE,
                "setup: expected the function to become Active, got {read}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `Invoke` the function once with `event`, asserting the invocation was served and the
    /// handler ran to completion — which is what a refusal the handler CAUGHT looks like from
    /// outside — and answering what the handler reported.
    async fn invoke(&self, case: &str, event: &Value) -> Value {
        let (status, headers, payload) = self
            .rest(
                "lambda",
                "POST",
                &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
                event.to_string().as_bytes(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        payload
    }

    /// Invoke the function with ONE reporting step until that step is served, which is how a
    /// suite waits for a grant it has just written to be the one a call is decided against:
    /// the decision is taken at the instant of the call, so what is waited for is the
    /// CONDITION and not a span. The step reads and writes nothing, so looking again costs
    /// the API nothing.
    async fn await_grant(&self, case: &str, step: &Value) {
        let started = Instant::now();
        loop {
            let payload = self
                .invoke(case, &json!({ "steps": [step.clone()] }))
                .await;
            let reported = payload["steps"][PROBE].clone();
            if reported["ok"] == json!(true) {
                return;
            }
            assert!(
                started.elapsed() < SETTLE_DEADLINE,
                "case {case:?}: expected the written grant to admit the call, got {reported}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The case name the grant probe reports under.
const PROBE: &str = "the grant is in force";

/// The ARN the account holds the execution role under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", engine::emulator_account_id())
}

/// The assumed-role session a function's own calls are made as:
/// `arn:aws:sts::<account>:assumed-role/<role name>/<function name>`, the identity AWS reports
/// for a function's calls.
fn session_principal() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{FUNCTION}",
        engine::emulator_account_id()
    )
}

/// The ARN a control-plane request addressing `path` names: API Gateway's own ARN form, whose
/// account field is EMPTY and whose resource portion is the request's own URI path — spelled
/// out here rather than read off the crate under test.
fn control_plane_arn(path: &str) -> String {
    format!("arn:aws:{ACTION_PREFIX}:{REGION}::{path}")
}

/// The sentence IAM refuses a request with when no identity-based document allows it, built
/// here from the principal, the action and the resource rather than read off the crates under
/// test.
fn denial(method: &str, path: &str) -> String {
    let action = format!("{ACTION_PREFIX}:{method}");
    format!(
        "User: {} is not authorized to perform: {action} on resource: {} because no \
         identity-based policy allows the {action} action",
        session_principal(),
        control_plane_arn(path)
    )
}

/// The handler the suite runs: one step per operation, each reported under its own case name,
/// and every refusal caught and reported as the code, message and status the SERVICE answered
/// — the shape a function that redeploys an API it owns has.
///
/// `createdDate` is reported as whether the answer carried an instant at all: the member is a
/// timestamp, and what this suite asserts about the answer is that the modelled output reached
/// the guest, not which instant one run minted.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

GATEWAY = boto3.client("apigateway")


def _call(step):
    name = step["call"]
    if name == "get_rest_apis":
        answered = GATEWAY.get_rest_apis()
        return {"names": sorted(api.get("name") for api in answered.get("items", []))}
    if name == "get_resources":
        answered = GATEWAY.get_resources(restApiId=step["api"])
        return {"paths": sorted(one.get("path") for one in answered.get("items", []))}
    if name == "create_deployment":
        answered = GATEWAY.create_deployment(
            restApiId=step["api"], stageName=step["stage"], description=step["description"]
        )
        return {
            "id": answered.get("id"),
            "description": answered.get("description"),
            "dated": answered.get("createdDate") is not None,
        }
    if name == "update_stage":
        answered = GATEWAY.update_stage(
            restApiId=step["api"], stageName=step["stage"], patchOperations=step["patch"]
        )
        return {
            "stage": answered.get("stageName"),
            "variables": answered.get("variables"),
            "deployment": answered.get("deploymentId"),
        }
    if name == "delete_stage":
        GATEWAY.delete_stage(restApiId=step["api"], stageName=step["stage"])
        return {}
    raise ValueError("no such step: " + name)


def lambda_handler(event, context):
    steps = {}
    for step in event["steps"]:
        try:
            answered = _call(step)
        except botocore.exceptions.ClientError as error:
            steps[step["case"]] = {
                "ok": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            }
        else:
            answered["ok"] = True
            steps[step["case"]] = answered
    return {"steps": steps, "region": GATEWAY.meta.region_name}
"#;

/// The variables the handler's `UpdateStage` writes, one through each op that names a single
/// entry of a stage's variable map (`add` puts an entry there that was not, `replace`
/// assigns one).
const VARIABLES: [(&str, &str, &str); 2] = [
    ("add", "table", "orders"),
    ("replace", "endpoint", "https://orders.example.com"),
];

/// Every binding class of this service's control plane, driven from inside a handler in two
/// phases — the role carrying no statement, then the role carrying the statement for each
/// action but one — with the effects read back through API Gateway's own API.
#[tokio::test]
async fn an_api_gateway_control_plane_call_from_a_handler_is_decided_against_its_execution_role() {
    let _stream = one_invocation_stream_at_a_time().await;
    let case = "a handler calling the API Gateway control plane through the runtime's SDK";
    let emulator = Emulator::new();
    let role = emulator.execution_role().await;

    // The API the handler acts on, created and given one method by the suite's own client: a
    // deployment is the snapshot of an API's methods, so an API with none has nothing to
    // deploy.
    let created = emulator
        .served(
            "the API",
            "POST",
            "/restapis",
            Some(&json!({
                "name": API_NAME,
                "endpointConfiguration": {"types": ["REGIONAL"]},
            })),
        )
        .await;
    let api_id = created["id"]
        .as_str()
        .expect("CreateRestApi answers the api's id")
        .to_string();
    let resources = emulator
        .served(
            "the root resource",
            "GET",
            &format!("/restapis/{api_id}/resources"),
            None,
        )
        .await;
    let root = resources["item"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|root| root["id"].as_str())
        .expect("the created API carries a root resource")
        .to_string();
    emulator
        .served(
            "the root method",
            "PUT",
            &format!("/restapis/{api_id}/resources/{root}/methods/GET"),
            Some(&json!({ "authorizationType": "NONE" })),
        )
        .await;
    emulator
        .served(
            "the root integration",
            "PUT",
            &format!("/restapis/{api_id}/resources/{root}/methods/GET/integration"),
            Some(&json!({ "type": "MOCK" })),
        )
        .await;

    emulator.function(&role, HANDLER_SOURCE).await;

    // The paths each row's request addresses, which is the resource portion of the ARN its
    // decision is taken over.
    let restapis_path = "/restapis".to_string();
    let resources_path = format!("/restapis/{api_id}/resources");
    let deployments_path = format!("/restapis/{api_id}/deployments");
    let stage_path = format!("/restapis/{api_id}/stages/{STAGE}");

    let patch: Vec<Value> = VARIABLES
        .iter()
        .map(|(op, name, value)| json!({"op": op, "path": format!("/variables/{name}"), "value": value}))
        .collect();
    let steps = json!([
        // A GET addressed by no path label at all.
        {"case": "a listing addressed by no label", "call": "get_rest_apis"},
        // A GET addressed by one path label.
        {"case": "a listing addressed by one label", "call": "get_resources", "api": api_id},
        // A POST addressed by one label and a sub-path, carrying its input in the body.
        {"case": "a create addressed by a label and a sub-path", "call": "create_deployment",
         "api": api_id, "stage": STAGE, "description": "deployed by a handler"},
        // A PATCH addressed by TWO labels, carrying a list of patch operations.
        {"case": "a patch addressed by two labels", "call": "update_stage",
         "api": api_id, "stage": STAGE, "patch": patch},
        // A DELETE addressed by those same two labels, which the second phase does NOT grant.
        {"case": "a delete addressed by two labels", "call": "delete_stage",
         "api": api_id, "stage": STAGE},
    ]);

    // Each row as the decision sees it: the case it is reported under, the HTTP METHOD its
    // operation's own `@http` trait binds — which is the action IAM decides — and the path its
    // request addresses, which is the resource portion of the ARN the decision is taken over.
    let rows: [(&str, &str, &str); 5] = [
        ("a listing addressed by no label", "GET", &restapis_path),
        ("a listing addressed by one label", "GET", &resources_path),
        (
            "a create addressed by a label and a sub-path",
            "POST",
            &deployments_path,
        ),
        ("a patch addressed by two labels", "PATCH", &stage_path),
        ("a delete addressed by two labels", "DELETE", &stage_path),
    ];

    // PHASE ONE — the role carries no statement, so every row is IAM's own refusal: the
    // session, the action its own HTTP method names, the ARN of the path it addressed, and the
    // 403 every AWS SDK unmarshals as an authorization failure. The refusal is taken before a
    // record is read, which is why the PATCH and the DELETE of a stage that does not exist yet
    // are refused as unauthorized rather than as not found.
    let denied = emulator
        .invoke(case, &json!({ "steps": steps.clone() }))
        .await;
    assert_eq!(
        denied["region"],
        json!(REGION),
        "case {case:?}: expected the client to report the run's Region, got {denied}"
    );
    for (row, method, path) in rows {
        let reported = denied["steps"][row].clone();
        assert_eq!(
            (
                reported["ok"].clone(),
                reported["code"].clone(),
                reported["status"].clone(),
                reported["message"].clone(),
            ),
            (
                json!(false),
                json!(ACCESS_DENIED),
                json!(ACCESS_DENIED_STATUS),
                json!(denial(method, path)),
            ),
            "case {case:?}/{row}: expected IAM's own refusal of {ACTION_PREFIX}:{method} on \
             {path}, got {reported}"
        );
    }

    // No effect landed: the API holds no deployment and no stage, so a refused call is
    // distinguishable from a served one by what the service holds and not only by its answer.
    let held = emulator
        .served(
            "the deployments after the refusals",
            "GET",
            &deployments_path,
            None,
        )
        .await;
    assert_eq!(
        held["item"].as_array().map(Vec::len).unwrap_or_default(),
        0,
        "case {case:?}: expected the refused create to deploy nothing, got {held}"
    );
    let (missing, body) = emulator
        .apigateway("GET", &stage_path, None)
        .await;
    assert_eq!(
        missing, 404,
        "case {case:?}: expected no stage to exist after the refusals, got {missing} ({body})"
    );

    // PHASE TWO — the role carries one statement per action, each naming the resources that
    // action is taken on, and nothing for the DELETE. The resource ARNs are the ones a
    // developer writes against this service: its own ARN form, whose resource portion is the
    // request's path.
    emulator
        .grant(
            &json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": format!("{ACTION_PREFIX}:GET"),
                        "Resource": [
                            control_plane_arn("/restapis"),
                            control_plane_arn("/restapis/*"),
                        ],
                    },
                    {
                        "Effect": "Allow",
                        "Action": format!("{ACTION_PREFIX}:POST"),
                        "Resource": control_plane_arn("/restapis/*/deployments"),
                    },
                    {
                        "Effect": "Allow",
                        "Action": format!("{ACTION_PREFIX}:PATCH"),
                        "Resource": control_plane_arn("/restapis/*/stages/*"),
                    },
                ],
            })
            .to_string(),
        )
        .await;
    emulator
        .await_grant(
            case,
            &json!({ "case": PROBE, "call": "get_rest_apis" }),
        )
        .await;

    let served = emulator
        .invoke(case, &json!({ "steps": steps.clone() }))
        .await;
    let reported = served["steps"].clone();

    // The GET with no label: the API this suite created, read back under the output member's
    // OWN name.
    assert_eq!(
        reported["a listing addressed by no label"]["names"],
        json!([API_NAME]),
        "case {case:?}: expected the created API listed, got {}",
        reported["a listing addressed by no label"]
    );
    // The GET with one label.
    assert_eq!(
        reported["a listing addressed by one label"]["paths"],
        json!(["/"]),
        "case {case:?}: expected the API's root resource, got {}",
        reported["a listing addressed by one label"]
    );
    // The POST: the modelled output of a create, whose id the stage now serves.
    let deployment = reported["a create addressed by a label and a sub-path"].clone();
    assert_eq!(
        (
            deployment["ok"].clone(),
            deployment["description"].clone(),
            deployment["dated"].clone()
        ),
        (json!(true), json!("deployed by a handler"), json!(true)),
        "case {case:?}: expected the deployment's own modelled members, got {deployment}"
    );
    let deployment_id = deployment["id"]
        .as_str()
        .expect("the served create answers the deployment's id")
        .to_string();
    // The PATCH: the stage's variables as the patch operations left them, and the deployment
    // the stage serves.
    let expected_variables: Value = VARIABLES
        .iter()
        .map(|(_op, name, value)| ((*name).to_string(), json!(value)))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let patched = reported["a patch addressed by two labels"].clone();
    assert_eq!(
        (
            patched["ok"].clone(),
            patched["stage"].clone(),
            patched["variables"].clone(),
            patched["deployment"].clone()
        ),
        (
            json!(true),
            json!(STAGE),
            expected_variables.clone(),
            json!(deployment_id.clone())
        ),
        "case {case:?}: expected the patched stage's own members, got {patched}"
    );
    // The DELETE, which this phase grants nothing for: the decision is per ACTION, so one
    // statement short of it is still IAM's refusal while every other row is served.
    let refused = reported["a delete addressed by two labels"].clone();
    assert_eq!(
        (
            refused["ok"].clone(),
            refused["code"].clone(),
            refused["status"].clone(),
            refused["message"].clone()
        ),
        (
            json!(false),
            json!(ACCESS_DENIED),
            json!(ACCESS_DENIED_STATUS),
            json!(denial("DELETE", &stage_path))
        ),
        "case {case:?}: expected IAM's own refusal of the ungranted action, got {refused}"
    );

    // What API GATEWAY itself holds: the deployment the handler was answered is the one a
    // client lists, the stage carries the variables the handler's patch wrote and points at
    // that deployment, and the refused delete took nothing away.
    let listed = emulator
        .served("the deployments a client lists", "GET", &deployments_path, None)
        .await;
    let ids: Vec<&str> = listed["item"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|entry| entry["id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec![deployment_id.as_str()],
        "case {case:?}: expected the handler's own deployment where a client lists them, got \
         {listed}"
    );
    let stage = emulator
        .served("the stage a client reads", "GET", &stage_path, None)
        .await;
    assert_eq!(
        (stage["variables"].clone(), stage["deploymentId"].clone()),
        (expected_variables, json!(deployment_id)),
        "case {case:?}: expected the stage the handler's calls left behind, got {stage}"
    );
}
