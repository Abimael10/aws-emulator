// SPDX-License-Identifier: CC0-1.0

//! The IAM authorization of API Gateway's REST CONTROL PLANE, through the whole assembled
//! router: which documents decide a management call, and what a caller they do not cover is
//! told.
//!
//! > **RULE: a request to the API Gateway REST control plane is authorized BEFORE the
//! > operation reads or writes a record, as the IAM action `apigateway:<the request's own HTTP
//! > METHOD>` on `arn:aws:apigateway:{region}::{the request's own URI path}`, against every
//! > identity-based document in force on the principal its credential speaks for — the
//! > documents embedded in that principal's role and the AWS-managed policies attached to it
//! > alike. A call a readable document DENIES, and a call no readable document allows, is
//! > refused HTTP 403 with the error code `AccessDeniedException` and the sentence IAM words
//! > an authorization failure in, and performs nothing; a call one of those documents allows
//! > is served.**
//!
//! API Gateway publishes no action per operation: its control-plane actions are the HTTP
//! methods of the REST API that serves the plane (`apigateway:GET`, `apigateway:POST`,
//! `apigateway:PUT`, `apigateway:PATCH`, `apigateway:DELETE`) taken on an ARN whose resource
//! portion is the request's own path, which is the form every ARN this plane reports carries
//! (`arn:aws:apigateway:{region}::/restapis/{id}/…`). So a statement a developer writes
//! against `apigateway:POST` on `arn:aws:apigateway:{region}::/restapis/{id}/deployments*`
//! decides exactly the calls AWS would decide with it, and a grant of one verb never covers
//! another.
//!
//! Four crates have to agree for that to hold, which is why this is a router suite: IAM holds
//! each role, the document embedded in it and the AWS-managed policies attached to it, the
//! token service mints the session a request signs with, AWS Lambda runs the handler whose own
//! calls are signed as its execution role's session, and API Gateway takes the decision and
//! words the refusal. Everything is driven the way a client reaches it — `iam:CreateRole`,
//! `iam:AttachRolePolicy`, `iam:PutRolePolicy`, `sts:AssumeRole`, then the signed management
//! call — so the documents under test are the ones IAM stored and the principal a refusal
//! blames is the one the credential was minted for.
//!
//! The rows span what the rule generalises over: WHERE the document in force comes from (an
//! AWS-managed policy attached to the role, a document embedded in it, both at once, and
//! nothing at all), WHICH action it names (the verb the call uses, another verb of the same
//! plane, every action of the service), WHICH resource it names (every resource, the
//! collection the read addresses, the api's own deployments, another api's deployments),
//! which EFFECT it states (an allow, an allow with an explicit deny beside it), the VERB of
//! the call (a collection read and a deployment write, so one document is asked about two
//! actions), and the PRINCIPAL (an assumed-role session, a function's execution-role session,
//! and the account's own published credential, which holds no IAM policies). Every row
//! asserts both halves — what the caller was answered, and which deployments the api holds
//! afterwards, read where a client reads them — so a refusal that still deployed, and a grant
//! that answered without deploying, both fail here.
//!
//! The function-principal rows close on the half a role granted BOTH verbs cannot show: a
//! document granting ONE verb of the plane, asked about both calls, so a grant of the read verb
//! is seen to serve the listing and refuse the deployment and a grant of the write verb to do
//! the reverse — the two halves the `apigateway__cloudformation__lambda` scenario's own checks
//! are made of.

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

/// The AWS-managed policies the rows attach, each by the ARN AWS publishes it under. The
/// first is the policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on this plane.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";
/// AWS's own read-only policy, which grants the read verbs of every service
/// (`"Action": ["*:Get*", …]`) and no write of any.
const READ_ONLY: &str = "arn:aws:iam::aws:policy/ReadOnlyAccess";
/// AWS's own administrator policy, which grants every action on every resource.
const ADMINISTRATOR: &str = "arn:aws:iam::aws:policy/AdministratorAccess";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The stage a deployment the function makes names, as the recorded scenario names one.
const STAGE: &str = "prod";

/// Where Lambda binds the collection a function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State`
/// a function reports before it may be invoked. The timeout is well past what a run needs on
/// any host: what is under test is the authorization decision, not how fast an interpreter
/// gets through the handler.
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

/// The clause closing a refusal a readable document DENIED.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// An identity-based policy allowing EVERYTHING and denying `action` on every resource: the
/// explicit deny no grant lifts, which a single-statement document cannot express.
fn allow_everything_but(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "*", "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": "*" },
        ],
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
/// attached to it and `inline` embedded in it, through IAM's own calls. Answers the role's ARN.
async fn given_role(
    router: &MultiServiceRouter,
    role_name: &str,
    trust: &str,
    attached: &[&str],
    inline: Option<&Value>,
) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", trust),
        ],
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
    if let Some(document) = inline {
        put_role_policy(router, role_name, document).await;
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
    (
        element(&assumed, "AccessKeyId"),
        element(&assumed, "Arn"),
    )
}

/// One REST API carrying one `GET` method on its root resource, on a `MOCK` integration: an
/// api with a method on some resource is what a deployment can snapshot at all. Answers its id.
async fn api_with_a_method(router: &MultiServiceRouter, name: &str) -> String {
    let created = served_control_plane(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(&json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
        201,
    )
    .await;
    let api = as_str("setup: CreateRestApi", &created, "id");
    let root = as_str("setup: CreateRestApi", &created, "rootResourceId");
    served_control_plane(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{root}/methods/GET"),
        Some(&json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    served_control_plane(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{root}/methods/GET/integration"),
        Some(&json!({ "type": "MOCK" })),
        201,
    )
    .await;
    api
}

/// The ids of the deployments the api `api` holds, read where a client reads them: this
/// plane's own `GetDeployments`, under the account's own credential.
async fn deployment_ids(router: &MultiServiceRouter, case: &str, api: &str) -> Vec<String> {
    let answered = control_plane(
        router,
        ACCOUNT_KEY_ID,
        "GET",
        &format!("/restapis/{api}/deployments"),
        None,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: GetDeployments must be served to the account, got {} {}",
        answered.status, answered.text
    );
    answered.json()[WIRE_PAGE_MEMBER]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The path of the collection a REST-API discovery read addresses.
const COLLECTION_PATH: &str = "/restapis";

/// The member one page of a collection of this plane reports its elements at, ON THE WIRE:
/// the page member's `jsonName` is `item` (`smithy_query(query="shape", shape="RestApis")`),
/// which an SDK surfaces to its caller as `items`. A suite reading the raw body reads the
/// wire name, and the handler running inside the function below reads the SDK's.
const WIRE_PAGE_MEMBER: &str = "item";

/// One row: the documents in force on the caller's own role, and what each of the two calls
/// must be answered.
struct Row {
    case: &'static str,
    /// The AWS-managed policies attached to the row's role.
    attached: &'static [&'static str],
    /// The document embedded in it, built from the id of the api the write addresses.
    inline: fn(&str) -> Option<Value>,
    /// Whether the collection READ (`GET /restapis`) is served.
    read_served: bool,
    /// Whether the deployment WRITE (`POST /restapis/{api}/deployments`) is served.
    write_served: bool,
    /// Set where a readable document DENIES the row's calls, which is the other clause a
    /// refusal can close with.
    denied: bool,
}

/// The rows, each driven under a role and a session of its own.
const ROWS: &[Row] = &[
    Row {
        case: "no document at all",
        attached: &[],
        inline: |_api| None,
        read_served: false,
        write_served: false,
        denied: false,
    },
    Row {
        // The role a client creates for a function and nothing more, which is the shape a
        // least-privilege scenario starts from.
        case: "the AWS-managed execution-role policy alone",
        attached: &[BASIC_EXECUTION],
        inline: |_api| None,
        read_served: false,
        write_served: false,
        denied: false,
    },
    Row {
        case: "the AWS-managed read-only policy beside it",
        attached: &[BASIC_EXECUTION, READ_ONLY],
        inline: |_api| None,
        read_served: true,
        write_served: false,
        denied: false,
    },
    Row {
        case: "an embedded grant of the read verb on the collection",
        attached: &[BASIC_EXECUTION],
        inline: |_api| Some(allow("apigateway:GET", &apigateway_arn("/restapis*"))),
        read_served: true,
        write_served: false,
        denied: false,
    },
    Row {
        case: "an embedded grant of the write verb on the api's deployments",
        attached: &[BASIC_EXECUTION],
        inline: |api| {
            Some(allow(
                "apigateway:POST",
                &apigateway_arn(&format!("/restapis/{api}/deployments*")),
            ))
        },
        read_served: false,
        write_served: true,
        denied: false,
    },
    Row {
        case: "an embedded grant naming another api's deployments",
        attached: &[BASIC_EXECUTION],
        inline: |_api| {
            Some(allow(
                "apigateway:POST",
                &apigateway_arn("/restapis/other0000/deployments*"),
            ))
        },
        read_served: false,
        write_served: false,
        denied: false,
    },
    Row {
        case: "an embedded grant of another service's actions",
        attached: &[BASIC_EXECUTION],
        inline: |_api| Some(allow("execute-api:Invoke", "*")),
        read_served: false,
        write_served: false,
        denied: false,
    },
    Row {
        case: "the AWS-managed administrator policy",
        attached: &[ADMINISTRATOR],
        inline: |_api| None,
        read_served: true,
        write_served: true,
        denied: false,
    },
    Row {
        case: "an explicit deny of this plane beside a grant of everything",
        attached: &[ADMINISTRATOR],
        inline: |_api| Some(allow_everything_but("apigateway:*")),
        read_served: false,
        write_served: false,
        denied: true,
    },
];

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is the AUTHORIZATION of a credential, not its signature. A handler's own
    // calls are signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// RULE: a control-plane call is served only where a document in force on the caller's
/// principal allows `apigateway:<the call's verb>` on the ARN of the path it addressed; every
/// other call is refused 403 `AccessDeniedException` with IAM's own sentence and performs
/// nothing.
#[tokio::test]
async fn a_control_plane_call_is_decided_by_every_document_in_force_on_the_caller() {
    let router = emulator();
    let api = api_with_a_method(&router, "control-plane-role-authorization").await;
    let deployments_path = format!("/restapis/{api}/deployments");
    let deployments_resource = apigateway_arn(&deployments_path);
    let collection_resource = apigateway_arn(COLLECTION_PATH);

    // Every deployment the SERVED writes made, in the order they were made: what the api must
    // hold after each row, so a refused write that still deployed fails here.
    let mut deployed: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("apigw-cp-role-{index}");
        let role_arn = given_role(
            &router,
            &name,
            &account_trust_policy(),
            row.attached,
            (row.inline)(&api).as_ref(),
        )
        .await;
        let (access_key_id, principal) = assumed_session(&router, &role_arn, &name).await;

        // The DISCOVERY read, judged as `apigateway:GET` on the collection's own ARN.
        let read = control_plane(&router, &access_key_id, "GET", COLLECTION_PATH, None).await;
        let case = format!("{} / GetRestApis", row.case);
        if row.read_served {
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected the granted read to be served, got {} {}",
                read.status, read.text
            );
            let listed: Vec<String> = read.json()[WIRE_PAGE_MEMBER]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            item.get("id").and_then(Value::as_str).map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                listed.contains(&api),
                "case {case:?}: expected the listing to report the api {api:?}, got {listed:?}"
            );
        } else {
            assert_refused(
                &case,
                &read,
                &principal,
                &action_for("GET"),
                &collection_resource,
                row.denied,
            );
        }

        // The deployment WRITE, judged as `apigateway:POST` on the deployments' own ARN.
        let write = control_plane(
            &router,
            &access_key_id,
            "POST",
            &deployments_path,
            Some(&json!({})),
        )
        .await;
        let case = format!("{} / CreateDeployment", row.case);
        if row.write_served {
            assert_eq!(
                write.status, 201,
                "case {case:?}: expected the granted write to be served, got {} {}",
                write.status, write.text
            );
            deployed.push(as_str(&format!("case {case:?}"), &write.json(), "id"));
        } else {
            assert_refused(
                &case,
                &write,
                &principal,
                &action_for("POST"),
                &deployments_resource,
                row.denied,
            );
        }

        // The other half of every row: what the api HOLDS now, which is exactly the
        // deployments the served writes made and nothing a refused one asked for.
        let held = deployment_ids(&router, &case, &api).await;
        assert_eq!(
            held, deployed,
            "case {case:?}: expected the api to hold exactly the deployments the served \
             writes made, got {held:?}"
        );
    }

    // The account's own published credential holds no IAM policies — the position AWS account
    // root credentials are in — so it is not bounded by this gate: the positive control that
    // the plane itself serves the very calls every refused row made.
    let read = control_plane(&router, ACCOUNT_KEY_ID, "GET", COLLECTION_PATH, None).await;
    assert_eq!(
        read.status, 200,
        "case \"the account's own published credential / GetRestApis\": expected the read to \
         be served, got {} {}",
        read.status, read.text
    );
    let write = control_plane(
        &router,
        ACCOUNT_KEY_ID,
        "POST",
        &deployments_path,
        Some(&json!({})),
    )
    .await;
    assert_eq!(
        write.status, 201,
        "case \"the account's own published credential / CreateDeployment\": expected the \
         write to be served, got {} {}",
        write.status, write.text
    );
}

/// Assert one refused call: the status, the error shape a client branches on, and IAM's own
/// sentence naming the principal, the action, the resource the request addressed and which of
/// IAM's rules refused it.
fn assert_refused(
    case: &str,
    answered: &Answered,
    principal: &str,
    action: &str,
    resource: &str,
    denied: bool,
) {
    let code = answered.error_code();
    assert_eq!(
        (answered.status, code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {}",
        answered.status,
        answered.text
    );
    let reason = if denied {
        EXPLICIT_DENY.to_string()
    } else {
        no_grant(action)
    };
    let expected = not_authorized(principal, action, resource, &reason);
    assert_eq!(
        answered.error_message(),
        expected,
        "case {case:?}: expected IAM's authorization sentence {expected:?}, got {}",
        answered.text
    );
}

/// The handler the probe function runs: `import boto3` and the control-plane client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than this call — and then
/// the ONE management call the event names, reporting either its result or the refusal API
/// Gateway answered it with. The invocation's payload IS what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

APIGATEWAY = boto3.client("apigateway")


def handler(event, context):
    try:
        if event["call"] == "create_deployment":
            created = APIGATEWAY.create_deployment(
                restApiId=event["api"], stageName=event["stage"]
            )
            return {"ok": True, "deployment_id": created["id"]}
        listed = APIGATEWAY.get_rest_apis(limit=500)
        return {"ok": True, "ids": [item["id"] for item in listed.get("items", [])]}
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
const STACK: &str = "apigw-cp-probe-stack";
const EXECUTION_ROLE: &str = "apigw-cp-probe-role";
const FUNCTION: &str = "apigw-cp-probe";

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
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`], so a loaded
/// host delays this rather than deciding it; every other answer is asserted at once.
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

/// The api ids a served `get_rest_apis` reported, as the handler passes them back.
fn listed_ids(reported: &Value) -> Vec<String> {
    reported["ids"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// RULE, for the principal a RUNNING function's own calls speak for: the same decision, taken
/// against the documents in force on its execution role at the instant of the call — so a
/// function whose role carries only the AWS-managed execution-role policy is refused the
/// management calls it makes, and the grant a client then puts on that same role serves them.
#[tokio::test]
async fn a_functions_control_plane_calls_are_decided_by_its_execution_roles_documents() {
    let router = emulator();
    let api = api_with_a_method(&router, "control-plane-function-authorization").await;
    let deployments_path = format!("/restapis/{api}/deployments");
    let principal = execution_session();

    // The execution role as a client creates one: trusted by Lambda, carrying the AWS-managed
    // execution-role policy and no statement about this plane at all.
    let role_arn = given_role(
        &router,
        EXECUTION_ROLE,
        &service_trust_policy(LAMBDA_PRINCIPAL),
        &[BASIC_EXECUTION],
        None,
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

    let write_event = json!({ "call": "create_deployment", "api": api, "stage": STAGE });
    let read_event = json!({ "call": "get_rest_apis" });

    // Neither call is covered by the logging grant the role carries, so each is refused for
    // the action ITS OWN verb names, on the ARN of the path it addressed.
    let refusals: &[(&str, &Value, &str, String)] = &[
        (
            "an ungranted function deploying the api",
            &write_event,
            "POST",
            apigateway_arn(&deployments_path),
        ),
        (
            "an ungranted function listing the account's apis",
            &read_event,
            "GET",
            apigateway_arn(COLLECTION_PATH),
        ),
    ];
    for (case, event, method, resource) in refusals {
        let reported = invoke(&router, case, event).await;
        let action = action_for(method);
        assert_eq!(
            (reported["ok"].clone(), reported["error_code"].clone()),
            (json!(false), json!(ACCESS_DENIED)),
            "case {case:?}: expected the call to be refused {ACCESS_DENIED}, got {reported}"
        );
        assert_eq!(
            reported["status"],
            json!(ACCESS_DENIED_STATUS),
            "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} on the wire, got {reported}"
        );
        assert_eq!(
            reported["message"],
            json!(not_authorized(
                &principal,
                &action,
                resource,
                &no_grant(&action)
            )),
            "case {case:?}: expected IAM's authorization sentence naming the function's \
             session, got {reported}"
        );
    }

    // Nothing the refused write asked for happened.
    let held = deployment_ids(&router, "after the refused deployment", &api).await;
    assert!(
        held.is_empty(),
        "case \"after the refused deployment\": expected the api to hold no deployment, got \
         {held:?}"
    );

    // The grant a client then writes on the SAME role, naming each verb on the path its own
    // call addresses: the identical calls are served from the next invocation on.
    put_role_policy(
        &router,
        EXECUTION_ROLE,
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": "apigateway:POST",
                    "Resource": apigateway_arn(&format!("{deployments_path}*")),
                },
                {
                    "Effect": "Allow",
                    "Action": "apigateway:GET",
                    "Resource": apigateway_arn("/restapis*"),
                },
            ],
        }),
    )
    .await;

    let case = "a granted function deploying the api";
    let deployed = invoke(&router, case, &write_event).await;
    assert_eq!(
        deployed["ok"],
        json!(true),
        "case {case:?}: expected the granted deployment to be served, got {deployed}"
    );
    let deployment = as_str(&format!("case {case:?}"), &deployed, "deployment_id");
    let held = deployment_ids(&router, case, &api).await;
    assert_eq!(
        held,
        vec![deployment.clone()],
        "case {case:?}: expected the api to hold exactly the deployment the handler made \
         ({deployment:?}), got {held:?}"
    );

    let case = "a granted function listing the account's apis";
    let listed = invoke(&router, case, &read_event).await;
    assert_eq!(
        listed["ok"],
        json!(true),
        "case {case:?}: expected the granted listing to be served, got {listed}"
    );
    let ids = listed_ids(&listed);
    assert!(
        ids.contains(&api),
        "case {case:?}: expected the listing to report the api {api:?}, got {ids:?}"
    );

    // The beats above put BOTH verbs on the role at once. The action is named after the
    // request's own HTTP METHOD, so a document granting ONE of them must serve exactly the
    // calls that use it and refuse the others — the half of the rule a role granted both
    // cannot show. Same role, same function, the inline document REPLACED between beats, so
    // the document in force is all that differs between them.
    //
    // One row per one-verb document: the verb it grants, the call that verb serves, and the
    // call of the other verb it therefore refuses with that call's own action and resource.
    struct OneVerb<'row> {
        granted: &'static str,
        /// The event whose call uses the granted verb, and whether that call is the WRITE —
        /// which is what makes a served answer a deployment the api then holds.
        served: &'row Value,
        served_is_the_write: bool,
        /// The event whose call uses the other verb, with the action and the resource its
        /// refusal must name.
        refused: &'row Value,
        refused_verb: &'static str,
        refused_resource: String,
    }
    let one_verb_rows = [
        OneVerb {
            granted: "GET",
            served: &read_event,
            served_is_the_write: false,
            refused: &write_event,
            refused_verb: "POST",
            refused_resource: apigateway_arn(&deployments_path),
        },
        OneVerb {
            granted: "POST",
            served: &write_event,
            served_is_the_write: true,
            refused: &read_event,
            refused_verb: "GET",
            refused_resource: apigateway_arn(COLLECTION_PATH),
        },
    ];

    // Every deployment the served writes have made, in the order they were made: what the api
    // must hold after each row.
    let mut made = vec![deployment];
    for row in &one_verb_rows {
        let granted_action = action_for(row.granted);
        let case = format!("a function granted only {granted_action}");
        put_role_policy(&router, EXECUTION_ROLE, &allow(&granted_action, "*")).await;

        // The call that uses the granted verb is served.
        let served = invoke(&router, &case, row.served).await;
        assert_eq!(
            served["ok"],
            json!(true),
            "case {case:?}: expected the call using the granted verb to be served, got {served}"
        );
        if row.served_is_the_write {
            made.push(as_str(&format!("case {case:?}"), &served, "deployment_id"));
        } else {
            let listed = listed_ids(&served);
            assert!(
                listed.contains(&api),
                "case {case:?}: expected the served listing to report the api {api:?}, got \
                 {listed:?}"
            );
        }

        // The call that uses the WITHHELD verb is refused for the action its own verb names, on
        // the ARN of the path it addressed.
        let withheld_action = action_for(row.refused_verb);
        let reported = invoke(&router, &case, row.refused).await;
        assert_eq!(
            (
                reported["ok"].clone(),
                reported["error_code"].clone(),
                reported["status"].clone(),
            ),
            (
                json!(false),
                json!(ACCESS_DENIED),
                json!(ACCESS_DENIED_STATUS),
            ),
            "case {case:?}: expected the {withheld_action} call to be refused \
             {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {reported}"
        );
        assert_eq!(
            reported["message"],
            json!(not_authorized(
                &principal,
                &withheld_action,
                &row.refused_resource,
                &no_grant(&withheld_action)
            )),
            "case {case:?}: expected IAM's sentence naming {withheld_action} on {}, got \
             {reported}",
            row.refused_resource
        );

        // And the api holds exactly the deployments the served writes made: nothing a refused
        // one asked for.
        let held = deployment_ids(&router, &case, &api).await;
        assert_eq!(
            held, made,
            "case {case:?}: expected the api to hold exactly the deployments the served writes \
             made, got {held:?}"
        );
    }
}
