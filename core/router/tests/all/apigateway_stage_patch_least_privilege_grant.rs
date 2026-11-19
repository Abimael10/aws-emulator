// SPDX-License-Identifier: CC0-1.0

//! The IAM decision API Gateway's REST control plane takes about a call that names ONE
//! resource of an api — a stage, a deployment, a method — under a least-privilege grant
//! written against that resource's own ARN.
//!
//! > **RULE: a request to the API Gateway REST control plane is authorized as the IAM action
//! > `apigateway:<the request's own HTTP METHOD>` on
//! > `arn:aws:apigateway:{region}::{the request's own URI path}`, so an identity-based
//! > statement naming EXACTLY that action and that ARN authorizes the call and the operation
//! > behind it runs — whichever resource of the api the path addresses and whichever verb the
//! > call uses. A grant naming another resource, or another verb, authorizes nothing, and a
//! > call no readable document allows is refused HTTP 403 `AccessDeniedException` with the
//! > sentence IAM words an authorization failure in.**
//!
//! The resource portion of an API Gateway ARN is the request's own path and the account field
//! is EMPTY (`arn:aws:apigateway:{region}::/restapis/{id}/stages/{stage}`), which is the form
//! every ARN this plane reports carries — so a statement scoped to exactly one stage is the
//! normal least-privilege shape, and what CDK's grant helpers emit.
//!
//! The rows span what the rule generalises over: the RESOURCE the call addresses (one stage,
//! one api, the api's deployments, one method of a resource — a leaf, a collection and a
//! record deep inside the api), the VERB it uses (`PATCH`, `GET`, `POST`, `PUT`), and the
//! SPELLING of the grant (the exact ARN of that very path, the same path under a trailing
//! wildcard, another resource's exact ARN, and the right resource under the wrong verb). Each
//! row is driven twice over the same documents: once by a client holding a session of the
//! role, and once by a RUNNING Lambda function whose own calls are signed as the session of
//! that role — the two principals a grant on a role has to decide alike.
//!
//! The written halves are read back where a client reads them: a served `UpdateStage` is
//! followed by an independent `GetStage`, so a grant that answered without writing, and a
//! refusal that wrote anyway, both fail here.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which is what the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";
const CFN_VERSION: &str = "2010-05-15";

/// The error shape and status an authorization refusal carries: IAM's own, in no operation's
/// modelled error set, because the decision is taken ahead of the service.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The AWS-managed policy a client creates a Lambda execution role with: it grants the log
/// writes a function makes and nothing else, so a role carrying only it has no grant on this
/// plane.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The stage the api below is deployed to.
const STAGE: &str = "prod";

/// Where Lambda binds the collection a function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State` a
/// function reports before it may be invoked. The timeout is well past what a run needs on any
/// host: what is under test is the authorization decision, not how fast an interpreter gets
/// through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;
const ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long an observation of work the CLOCK owes — a stack completing, a function going
/// `Active` — waits before calling it a hang, and how long between two looks. Generous, so a
/// loaded host delays the setup instead of deciding the outcome; each wait is on its own
/// CONDITION rather than on a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One answered request.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }

    /// The error SHAPE a restJson1 failure names, where a restJson1 client reads it.
    fn error_code(&self) -> String {
        self.headers
            .get("x-amzn-errortype")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_else(|| "<absent>".to_string())
    }

    /// The `message` member of an error body.
    fn error_message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }
}

/// A SigV4 `Authorization` whose credential names `access_key_id` and whose scope names
/// `signing_name` and [`REGION`], which is what routes the request and what Region the
/// decision is taken in. The signature itself is never checked here.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", &host)
                .header("authorization", authorization(access_key_id, signing_name))
                .header("content-type", content_type)
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One awsQuery call, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signing_name,
        ACCOUNT_KEY_ID,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        form.into_bytes(),
    )
    .await;
    (answered.status, answered.text)
}

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, signing_name, version, action, parameters).await;
    assert_eq!(
        status, 200,
        "setup: {signing_name} {action} must answer 200, got {status} with {body}"
    );
    body
}

/// One API Gateway REST control-plane call, signed by `access_key_id`.
async fn control_plane(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answered {
    dispatch(
        router,
        "apigateway",
        access_key_id,
        method,
        path,
        "application/json",
        body.map(|value| value.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await
}

/// One API Gateway REST control-plane call the account's own credential must be served,
/// answered with its document.
async fn served_control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    expected_status: u16,
) -> Value {
    let answered = control_plane(router, ACCOUNT_KEY_ID, method, path, body).await;
    assert_eq!(
        answered.status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {} {}",
        answered.status, answered.text
    );
    answered.json()
}

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what}: expected a non-empty {member:?}, got {body}"))
}

/// The ARN of an API Gateway resource whose URI path is `path`: this plane's ARNs carry an
/// EMPTY account field, and their resource portion IS the path of the operation that reaches
/// them.
fn apigateway_arn(path: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::{path}")
}

/// The IAM action a call using `method` performs on this plane.
fn action_for(method: &str) -> String {
    format!("apigateway:{method}")
}

/// The sentence IAM refuses an unauthorized call with: who was refused, which action, on which
/// resource, and which of IAM's rules refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The clause closing a refusal no readable document allowed — IAM's default deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// The trust policy of a role the account's own credential assumes.
fn account_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The trust policy of a role one AWS service assumes.
fn service_trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The one name an inline document is put on a role under.
const INLINE_POLICY: &str = "grants";

/// Hold one role named `role_name` trusting `trust`, with the AWS-managed policies `attached`
/// attached to it, through IAM's own calls. Answers the role's ARN.
async fn given_role(
    router: &MultiServiceRouter,
    role_name: &str,
    trust: &str,
    attached: &[&str],
) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[("RoleName", role_name), ("AssumeRolePolicyDocument", trust)],
    )
    .await;
    let role_arn = element(&created, "Arn");
    for policy_arn in attached {
        served_query(
            router,
            "iam",
            IAM_VERSION,
            "AttachRolePolicy",
            &[("RoleName", role_name), ("PolicyArn", policy_arn)],
        )
        .await;
    }
    role_arn
}

/// Put `document` on `role_name` as its one inline policy — what a client grants an existing
/// role a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// Mint a session of the role `role_arn` through the token service's own `AssumeRole`.
/// Answers the access key id the session was minted under and the principal ARN it speaks for.
async fn assumed_session(
    router: &MultiServiceRouter,
    role_arn: &str,
    session_name: &str,
) -> (String, String) {
    let assumed = served_query(
        router,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", session_name)],
    )
    .await;
    (element(&assumed, "AccessKeyId"), element(&assumed, "Arn"))
}

/// One REST API carrying one `GET` method on its root resource, on a `MOCK` integration,
/// deployed to [`STAGE`]: the api, its root resource id and the method's path part.
struct Api {
    id: String,
    root: String,
}

impl Api {
    /// Stand the api, its method, its integration and its deployment up under the account's
    /// own credential.
    async fn stand_up(router: &MultiServiceRouter, name: &str) -> Self {
        let created = served_control_plane(
            router,
            "CreateRestApi",
            "POST",
            "/restapis",
            Some(&json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
            201,
        )
        .await;
        let api = Self {
            id: as_str("setup: CreateRestApi", &created, "id"),
            root: as_str("setup: CreateRestApi", &created, "rootResourceId"),
        };
        served_control_plane(
            router,
            "PutMethod",
            "PUT",
            &format!("{}/methods/GET", api.root_methods()),
            Some(&json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        served_control_plane(
            router,
            "PutIntegration",
            "PUT",
            &format!("{}/methods/GET/integration", api.root_methods()),
            Some(&json!({ "type": "MOCK" })),
            201,
        )
        .await;
        served_control_plane(
            router,
            "CreateDeployment",
            "POST",
            &format!("/restapis/{}/deployments", api.id),
            Some(&json!({ "stageName": STAGE })),
            201,
        )
        .await;
        api
    }

    /// The path of the api's root resource, which the method paths hang off.
    fn root_methods(&self) -> String {
        format!("/restapis/{}/resources/{}", self.id, self.root)
    }

    /// The path of the stage the api is deployed to.
    fn stage_path(&self) -> String {
        format!("/restapis/{}/stages/{STAGE}", self.id)
    }

    /// The `variables` map the stage reports, read where a client reads it: this plane's own
    /// `GetStage`, under the account's own credential.
    async fn stage_variables(&self, router: &MultiServiceRouter, case: &str) -> Value {
        let answered = control_plane(router, ACCOUNT_KEY_ID, "GET", &self.stage_path(), None).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: GetStage must be served to the account, got {} {}",
            answered.status, answered.text
        );
        answered
            .json()
            .get("variables")
            .cloned()
            .unwrap_or(json!({}))
    }
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is the AUTHORIZATION of a credential, not its signature. A handler's own
    // calls are signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// One row: the call it makes, the grant in force, and whether that grant authorizes it.
struct Row {
    case: &'static str,
    /// The verb and the path of the call, built from the api.
    verb: &'static str,
    path: fn(&Api) -> String,
    /// The body the call carries.
    body: fn(&Api) -> Option<Value>,
    /// The status a served call answers.
    served_status: u16,
    /// The grant in force on the role while the call is made.
    grant: fn(&Api) -> Value,
    /// Whether that grant authorizes the call.
    served: bool,
}

/// The stage variable the patch rows set, and the value they set it to: the name is the
/// member `/variables/{name}` of the patch path, and the value is what `GetStage` must read
/// back.
const VARIABLE: &str = "envName";
const VARIABLE_VALUE: &str = "staging";

/// The patch body a stage-variable write carries.
fn variable_patch() -> Value {
    json!({
        "patchOperations": [
            { "op": "replace", "path": format!("/variables/{VARIABLE}"), "value": VARIABLE_VALUE }
        ]
    })
}

/// The rows, each driven under the grant it names.
const ROWS: &[Row] = &[
    Row {
        case: "the exact ARN of the stage the patch addresses",
        verb: "PATCH",
        path: |api| api.stage_path(),
        body: |_api| Some(variable_patch()),
        served_status: 200,
        grant: |api| allow("apigateway:PATCH", &apigateway_arn(&api.stage_path())),
        served: true,
    },
    Row {
        case: "the stage's own path under a trailing wildcard",
        verb: "PATCH",
        path: |api| api.stage_path(),
        body: |_api| Some(variable_patch()),
        served_status: 200,
        grant: |api| allow(
            "apigateway:PATCH",
            &apigateway_arn(&format!("/restapis/{}/stages/*", api.id)),
        ),
        served: true,
    },
    Row {
        case: "another stage's exact ARN",
        verb: "PATCH",
        path: |api| api.stage_path(),
        body: |_api| Some(variable_patch()),
        served_status: 200,
        grant: |api| allow(
            "apigateway:PATCH",
            &apigateway_arn(&format!("/restapis/{}/stages/other", api.id)),
        ),
        served: false,
    },
    Row {
        case: "the stage's exact ARN under another verb",
        verb: "PATCH",
        path: |api| api.stage_path(),
        body: |_api| Some(variable_patch()),
        served_status: 200,
        grant: |api| allow("apigateway:GET", &apigateway_arn(&api.stage_path())),
        served: false,
    },
    Row {
        case: "the exact ARN of the stage a read addresses",
        verb: "GET",
        path: |api| api.stage_path(),
        body: |_api| None,
        served_status: 200,
        grant: |api| allow("apigateway:GET", &apigateway_arn(&api.stage_path())),
        served: true,
    },
    Row {
        case: "the exact ARN of the api a patch addresses",
        verb: "PATCH",
        path: |api| format!("/restapis/{}", api.id),
        body: |_api| Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/description", "value": "granted" }
            ]
        })),
        served_status: 200,
        grant: |api| allow(
            "apigateway:PATCH",
            &apigateway_arn(&format!("/restapis/{}", api.id)),
        ),
        served: true,
    },
    Row {
        case: "the exact ARN of the deployments a write addresses",
        verb: "POST",
        path: |api| format!("/restapis/{}/deployments", api.id),
        body: |_api| Some(json!({ "stageName": STAGE })),
        served_status: 201,
        grant: |api| allow(
            "apigateway:POST",
            &apigateway_arn(&format!("/restapis/{}/deployments", api.id)),
        ),
        served: true,
    },
    Row {
        case: "the exact ARN of the method response a write addresses",
        verb: "PUT",
        path: |api| format!("{}/methods/GET/responses/200", api.root_methods()),
        body: |_api| Some(json!({})),
        served_status: 201,
        grant: |api| allow(
            "apigateway:PUT",
            &apigateway_arn(&format!("{}/methods/GET/responses/200", api.root_methods())),
        ),
        served: true,
    },
];

/// RULE: a control-plane call is authorized as `apigateway:<its own verb>` on the ARN of its
/// own path, so a statement naming exactly that action and that ARN serves it — whichever
/// resource of the api the path addresses — and one naming another resource or another verb
/// serves nothing.
#[tokio::test]
async fn a_grant_on_one_resources_own_arn_authorizes_the_call_that_addresses_it() {
    let router = emulator();
    let api = Api::stand_up(&router, "stage-patch-grant-rows").await;
    let role = "apigw-stage-grant-row";
    let role_arn = given_role(&router, role, &account_trust_policy(), &[BASIC_EXECUTION]).await;
    let (access_key_id, principal) = assumed_session(&router, &role_arn, role).await;

    for row in ROWS {
        put_role_policy(&router, role, &(row.grant)(&api)).await;
        let path = (row.path)(&api);
        let answered = control_plane(
            &router,
            &access_key_id,
            row.verb,
            &path,
            (row.body)(&api).as_ref(),
        )
        .await;
        let case = row.case;
        if row.served {
            assert_eq!(
                answered.status,
                row.served_status,
                "case {case:?}: expected the grant on {path:?} to serve the {} call, got {} {}",
                row.verb,
                answered.status,
                answered.text
            );
            continue;
        }
        let action = action_for(row.verb);
        let code = answered.error_code();
        assert_eq!(
            (answered.status, code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {}",
            answered.status,
            answered.text
        );
        assert_eq!(
            answered.error_message(),
            not_authorized(
                &principal,
                &action,
                &apigateway_arn(&path),
                &no_grant(&action)
            ),
            "case {case:?}: expected IAM's sentence naming {action} on the path's own ARN, got {}",
            answered.text
        );
    }

    // The stage variable the served patches set is there, read back through an independent
    // `GetStage`: a grant that answered without writing fails here.
    let variables = api.stage_variables(&router, "after the served patches").await;
    assert_eq!(
        variables[VARIABLE],
        json!(VARIABLE_VALUE),
        "case \"after the served patches\": expected the stage to carry the patched variable, \
         got {variables}"
    );
}

/// The handler the probe function runs: `import boto3` and the control-plane client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than this call — and then
/// the ONE management call the event names, reporting either its result or the refusal API
/// Gateway answered it with.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

APIGATEWAY = boto3.client("apigateway")


def handler(event, context):
    try:
        if event["call"] == "update_stage":
            updated = APIGATEWAY.update_stage(
                restApiId=event["api"],
                stageName=event["stage"],
                patchOperations=[
                    {
                        "op": "replace",
                        "path": "/variables/" + event["name"],
                        "value": event["value"],
                    }
                ],
            )
            return {"ok": True, "variables": updated.get("variables", {})}
        read = APIGATEWAY.get_stage(restApiId=event["api"], stageName=event["stage"])
        return {"ok": True, "variables": read.get("variables", {})}
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// The stack the probe function is deployed by, the role it runs as and its own name: the
/// inline source is archived into the deployment package by CloudFormation, which is how a
/// function whose handler is written here comes to exist at all.
const STACK: &str = "apigw-stage-grant-stack";
const EXECUTION_ROLE: &str = "apigw-stage-grant-role";
const FUNCTION: &str = "apigw-stage-grant-probe";

/// The session a call the running function makes is signed as: "the execution role grants the
/// function permission to use Amazon Web Services services"
/// (`FunctionConfiguration$Role`), and the session a managed runtime opens for a run is named
/// after the function.
fn execution_session() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{EXECUTION_ROLE}/{FUNCTION}",
        ::engine::emulator_account_id()
    )
}

/// The stack declaring the probe function.
fn function_stack(role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// Look again until the stack `stack` reports [`CREATE_COMPLETE`], and answer the last status
/// it reported. Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = served_query(
            router,
            "cloudformation",
            CFN_VERSION,
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until `function` reports [`ACTIVE`], which is the state it cannot be invoked
/// before. Waits on that CONDITION rather than on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let answered = dispatch(
            router,
            "lambda",
            ACCOUNT_KEY_ID,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            "application/json",
            Vec::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "setup: GetFunctionConfiguration({function}) must be served, got {} {}",
            answered.status, answered.text
        );
        let configuration = answered.json();
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// One invocation of the probe function with `event`, as it came back.
async fn invoke_once(router: &MultiServiceRouter, event: &Value) -> Answered {
    dispatch(
        router,
        "lambda",
        ACCOUNT_KEY_ID,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        "application/json",
        event.to_string().into_bytes(),
    )
    .await
}

/// The status Lambda answers an invocation that found no run capacity with: "Lambda returns
/// `TooManyRequestsException` if running the function would cause you to exceed a concurrency
/// limit" (`smithy_query(query="operation", operation="Invoke")`).
const THROTTLED: u16 = 429;

/// Invoke the probe function with `event` and answer what the handler reported.
///
/// A host running many functions at once has only so many run slots, and an invocation that
/// finds none is answered the service's own capacity refusal ([`THROTTLED`]) — which is not the
/// decision under test, so this looks again until one is not throttled, as a client retries a
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`].
async fn invoke(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut answered = invoke_once(router, event).await;
    while answered.status == THROTTLED && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        answered = invoke_once(router, event).await;
    }
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({FUNCTION}), got {} {}",
        answered.status, answered.text
    );
    let signal = answered
        .headers
        .get("x-amz-function-error")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    assert_eq!(
        signal, None,
        "case {case:?}: expected no function error, got {signal:?} with {}",
        answered.text
    );
    answered.json()
}

/// RULE, for the principal a RUNNING function's own calls speak for: the same decision, taken
/// against the documents in force on its execution role — so a handler whose role grants
/// `apigateway:PATCH` on exactly the stage it patches is served, the stage carries the variable
/// afterwards, and the same handler under a grant naming another stage is refused.
#[tokio::test]
async fn a_functions_patch_of_the_stage_its_role_names_is_served() {
    let router = emulator();
    let api = Api::stand_up(&router, "stage-patch-grant-in-function").await;
    let principal = execution_session();
    let role_arn = given_role(
        &router,
        EXECUTION_ROLE,
        &service_trust_policy(LAMBDA_PRINCIPAL),
        &[BASIC_EXECUTION],
    )
    .await;
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&role_arn)),
        ],
    )
    .await;
    let reached = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached}"
    );
    wait_for_active(&router, FUNCTION).await;

    let patch_event = json!({
        "call": "update_stage",
        "api": api.id,
        "stage": STAGE,
        "name": VARIABLE,
        "value": VARIABLE_VALUE,
    });
    let read_event = json!({ "call": "get_stage", "api": api.id, "stage": STAGE });

    // The stage starts with no variable at all, which is what the served patch below changes.
    let before = api.stage_variables(&router, "before the patch").await;
    assert_eq!(
        before.get(VARIABLE),
        None,
        "case \"before the patch\": expected the stage to carry no variable, got {before}"
    );

    // A grant naming ANOTHER stage's ARN authorizes nothing: the refusal names the action the
    // call's own verb publishes, on the ARN of the path it addressed.
    put_role_policy(
        &router,
        EXECUTION_ROLE,
        &allow(
            "apigateway:PATCH",
            &apigateway_arn(&format!("/restapis/{}/stages/other", api.id)),
        ),
    )
    .await;
    let case = "a function granted another stage's ARN";
    let refused = invoke(&router, case, &patch_event).await;
    let action = action_for("PATCH");
    assert_eq!(
        (
            refused["ok"].clone(),
            refused["error_code"].clone(),
            refused["status"].clone(),
        ),
        (
            json!(false),
            json!(ACCESS_DENIED),
            json!(ACCESS_DENIED_STATUS),
        ),
        "case {case:?}: expected the patch to be refused {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED}, got {refused}"
    );
    assert_eq!(
        refused["message"],
        json!(not_authorized(
            &principal,
            &action,
            &apigateway_arn(&api.stage_path()),
            &no_grant(&action)
        )),
        "case {case:?}: expected IAM's sentence naming the function's session and the stage's \
         own ARN, got {refused}"
    );
    let unchanged = api.stage_variables(&router, case).await;
    assert_eq!(
        unchanged.get(VARIABLE),
        None,
        "case {case:?}: expected the refused patch to have written nothing, got {unchanged}"
    );

    // The least-privilege grant a client writes for exactly the stage the handler patches: the
    // identical call is served from the next invocation on.
    put_role_policy(
        &router,
        EXECUTION_ROLE,
        &allow(
            "apigateway:PATCH",
            &apigateway_arn(&api.stage_path()),
        ),
    )
    .await;
    let case = "a function granted apigateway:PATCH on the stage it patches";
    let patched = invoke(&router, case, &patch_event).await;
    assert_eq!(
        patched["ok"],
        json!(true),
        "case {case:?}: expected the granted patch to be served, got {patched}"
    );
    assert_eq!(
        patched["variables"][VARIABLE],
        json!(VARIABLE_VALUE),
        "case {case:?}: expected the answered Stage to carry the replaced variable, got {patched}"
    );
    let variables = api.stage_variables(&router, case).await;
    assert_eq!(
        variables[VARIABLE],
        json!(VARIABLE_VALUE),
        "case {case:?}: expected an independent GetStage to read the same variable, got \
         {variables}"
    );

    // The grant covers the one verb it names and nothing else: the handler's READ of the same
    // stage is refused for `apigateway:GET` on the very ARN the patch was served on.
    let case = "a function granted only the patch verb, reading the stage";
    let refused = invoke(&router, case, &read_event).await;
    let read_action = action_for("GET");
    assert_eq!(
        (refused["ok"].clone(), refused["error_code"].clone()),
        (json!(false), json!(ACCESS_DENIED)),
        "case {case:?}: expected the read to be refused {ACCESS_DENIED}, got {refused}"
    );
    assert_eq!(
        refused["message"],
        json!(not_authorized(
            &principal,
            &read_action,
            &apigateway_arn(&api.stage_path()),
            &no_grant(&read_action)
        )),
        "case {case:?}: expected IAM's sentence naming {read_action}, got {refused}"
    );
}
