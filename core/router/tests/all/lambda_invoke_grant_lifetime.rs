// SPDX-License-Identifier: CC0-1.0

//! Whether a signed `Invoke` RUNS the function, over the three crates that hold the decision:
//! IAM holds the caller's identity-based documents, the token service resolves which principal
//! the presented access key id speaks for, and Lambda holds the function's own resource-based
//! policy and decides the invocation before the deployment package runs.
//!
//! RULE: Lambda admits an invocation only while a document in force grants the invoking action
//! on the addressed function to the calling principal — one of that principal's own
//! identity-based policies, or the function's resource-based policy — and takes that decision
//! on the documents AS THEY STAND at the instant of the call; a caller no document in force
//! grants it is refused HTTP 403 `AccessDeniedException` whose message names the caller, the
//! action and the function's ARN, and nothing of the function runs for it.
//!
//! Only a cross-service suite can pin the whole of it: the users, the roles and their inline
//! documents are IAM's records, the session an assumed-role caller speaks for is the token
//! service's, and the refusal is Lambda's — so each crate on its own sees one side of the
//! union.
//!
//! The rows span what the rule generalises over: WHICH document carries the grant (the
//! principal's own inline policy, the function's resource policy, written with `AddPermission`)
//! and WHAT KIND of principal calls (an IAM user, an assumed-role session). Each row drives a
//! grant's whole life against one function — refused before it is written, served while it
//! stands, refused again once it is withdrawn — so a verdict cached from the first call, or a
//! withdrawal that leaves the function reachable, fails here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every call below is signed for, so the ARNs the decision is taken on are the ones
/// this Region's function carries.
const REGION: &str = "us-east-1";

/// An access key id neither IAM nor the token service holds a record for: it resolves to the
/// account's own identity, which no identity-based document bounds, so every fixture call is
/// made with it and no row's own credential decides the setup. `pub(crate)` so the complement
/// suite makes its fixtures with the SAME setup credential rather than a second spelling of one.
pub(crate) const ACCOUNT_KEY: &str = "EMULATORSETUPKEY0001";

/// Where Lambda binds the function collection, one invocation and a function's policy.
///
/// `pub(crate)` because the complement of this suite — the invocation TYPES one grant admits
/// (`crate::lambda_resource_policy_admits_each_invocation_type`) — addresses the same routes
/// through this one harness rather than carrying a second copy of it.
pub(crate) const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The function every row invokes, the stack that deploys it, and the handler module
/// CloudFormation archives inline source under.
const FUNCTION: &str = "xsvc-invoke-grant-lifetime";
const STACK: &str = "xsvc-invoke-grant-lifetime-stack";
const HANDLER: &str = "index.handler";

/// The runtime the function runs on. Which `python3.x` it is does not matter here; that the
/// SDK is on its import path is `lambda_python_runtime_bundles_the_aws_sdk.rs`.
const RUNTIME: &str = "python3.12";

/// The handler: its returned value is what proves the RUN happened rather than the call merely
/// having been admitted.
const SOURCE: &str = "def handler(event, context):\n    return {'ran': True}\n";

/// The action a synchronous invocation performs, as the IAM Service Authorization Reference
/// publishes it for `Invoke` ("Actions defined by AWS Lambda").
pub(crate) const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The name a row's inline identity document is embedded under, and the statement id its
/// resource-policy grant is written under.
const POLICY_NAME: &str = "in-force";
const STATEMENT_ID: &str = "granted-caller";

/// The `State` a function serves invocations in, and the stack status its deployment settles
/// at.
const FUNCTION_ACTIVE: &str = "Active";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long this suite waits for work the clock owes (the provisioning window a create
/// documents, the stack's own completion) before giving up, and how often it looks. Nothing
/// here sleeps for a fixed span on the emulator's behalf.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The timeout the function is created with. Nothing here tests a function's own timeout, so
/// it is given generous room: a run shares this process with every other test of this target,
/// and must not be stopped by a limit the suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// The emulator as it is ASSEMBLED, on one engine: Lambda, IAM, the token service and
/// CloudFormation all reach the same store, so the documents one writes are the ones another
/// decides against.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing: what names the caller is the access key id in the credential
    // scope, which is what the rows vary.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and whose access key id is
/// `access_key_id` — the pair that routes the request and names the principal it speaks for.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered.
pub(crate) struct Answer {
    pub(crate) status: u16,
    error_code: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub(crate) fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }

    /// The `Message` of an error document, whichever casing the protocol wrote it in.
    fn message(&self) -> String {
        let document = self.json();
        document
            .get("Message")
            .or_else(|| document.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// One Lambda `restJson1` call presenting `access_key_id`, carrying the headers `headers` names
/// beside the standing ones — which is how a caller asks for an invocation TYPE, the axis the
/// complement suite varies.
pub(crate) async fn lambda_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda", access_key_id));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        error_code: parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: answered.to_vec(),
    }
}

/// One Lambda call made with the account's own credential that must be served.
async fn lambda(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let answer = lambda_as(router, ACCOUNT_KEY, method, uri, body, &[]).await;
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: {what} must be served, got {} with {}",
        answer.status,
        answer.text()
    );
    answer
}

/// One awsQuery call against `signing_name`, made with the account's own credential, as its
/// status and its XML body.
async fn query_raw(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, ACCOUNT_KEY))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The same, where the call must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = query_raw(router, signing_name, version, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// A call whose answer CARRIES CREDENTIAL MATERIAL, so nothing here quotes it — not even on a
/// failure, which names the case and the call instead; answers its XML body.
async fn credential_query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = query_raw(router, signing_name, version, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} must be served, got {status}"
    );
    text
}

/// One IAM call that must be served.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    query(router, case, "iam", "2010-05-08", action, params).await
}

/// The access key id of a minted-credential answer, read without quoting that answer — not
/// even on a failure, because it carries credential material.
fn access_key_id(case: &str, minted: &str) -> String {
    opt_element(minted, "AccessKeyId").unwrap_or_else(|| {
        panic!("case {case:?}: the minted-credential answer carries no <AccessKeyId> element")
    })
}

/// The session ARN an `AssumeRole` answer reports for the session it vended, read the same way
/// and for the same reason.
fn session_arn(case: &str, assumed: &str) -> String {
    opt_element(assumed, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: the AssumeRole answer carries no <Arn> element"))
}

/// The IAM entity a row's identity-based grant is written on: an inline policy of a user, or of
/// a role.
enum Entity {
    User(String),
    Role(String),
}

/// A principal a row's invocations are signed as.
pub(crate) struct Principal {
    /// The access key id its calls present.
    pub(crate) access_key_id: String,
    /// The ARN a refusal must blame it by — what the token service reports for that key.
    pub(crate) arn: String,
    /// The entity whose inline document an identity grant is written on.
    entity: Entity,
    /// The spelling the FUNCTION's own policy names it by: a user by its own ARN, and a session
    /// by the ARN of the role it is a session of, which is the principal a resource-based
    /// policy of a role's caller names.
    pub(crate) statement_principal: String,
}

/// Create the IAM user `name`, holding no policy at all, and mint a long-term access key for
/// it.
pub(crate) async fn given_user(router: &MultiServiceRouter, case: &str, name: &str) -> Principal {
    let created = iam(router, case, "CreateUser", &[("UserName", name)]).await;
    let arn = element(&created, "Arn");
    let minted = credential_query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", name)],
    )
    .await;
    Principal {
        access_key_id: access_key_id(case, &minted),
        arn: arn.clone(),
        entity: Entity::User(name.to_string()),
        statement_principal: arn,
    }
}

/// Create the IAM role `name`, holding no policy at all, assume it through the token service,
/// and answer the session that call vended.
async fn given_session(router: &MultiServiceRouter, case: &str, name: &str) -> Principal {
    let account = ::engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let role_arn = element(&created, "Arn");
    let assumed = credential_query(
        router,
        case,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", "probe")],
    )
    .await;
    Principal {
        access_key_id: access_key_id(case, &assumed),
        arn: session_arn(case, &assumed),
        entity: Entity::Role(name.to_string()),
        statement_principal: role_arn,
    }
}

/// Deploy the function `function`, declared by the stack `stack`, and answer its ARN.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all. The names are parameters so the complement suite deploys a function of its own
/// through this one deploy-and-wait rather than through a second copy of it.
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    function: &str,
    stack: &str,
) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": SOURCE},
                },
            },
        },
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion and the function's own provisioning window are owed by the clock,
    // which no fixed sleep names: this looks until each holds.
    let mut last = String::new();
    for _ in 0..looks() {
        last = query(
            router,
            case,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        if element(&last, "StackStatus") == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&last, "StackStatus"),
        CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {last}"
    );

    let mut configuration = Value::Null;
    for _ in 0..looks() {
        configuration = lambda(
            router,
            case,
            "GetFunctionConfiguration",
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an active function must report its ARN, got {configuration}")
                })
                .to_string();
            return arn;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> u32 {
    u32::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(u32::MAX)
}

/// The execution role the function runs as: trusted by Lambda, which `CreateFunction` checks,
/// and carrying nothing else — what it may do is not what this suite is about.
pub(crate) async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    element(&created, "Arn")
}

/// Where a row's grant is written, and so how it is withdrawn again.
#[derive(Clone, Copy, Debug)]
enum Grant {
    /// The principal's OWN inline identity-based document (`iam:PutUserPolicy` /
    /// `iam:PutRolePolicy`, withdrawn with the matching delete).
    IdentityPolicy,
    /// The FUNCTION's own resource-based policy (`lambda:AddPermission`, withdrawn with
    /// `lambda:RemovePermission`).
    ResourcePolicy,
}

/// What kind of principal a row calls as.
#[derive(Clone, Copy, Debug)]
enum Caller {
    User,
    AssumedRole,
}

/// One row: the principal kind, and the document its grant lives in.
struct Row {
    case: &'static str,
    caller: Caller,
    grant: Grant,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an IAM user, granted by its own inline policy",
            caller: Caller::User,
            grant: Grant::IdentityPolicy,
        },
        Row {
            case: "an IAM user, granted by the function's resource policy",
            caller: Caller::User,
            grant: Grant::ResourcePolicy,
        },
        Row {
            case: "an assumed-role session, granted by the role's inline policy",
            caller: Caller::AssumedRole,
            grant: Grant::IdentityPolicy,
        },
        Row {
            case: "an assumed-role session, granted by the function's resource policy",
            caller: Caller::AssumedRole,
            grant: Grant::ResourcePolicy,
        },
    ]
}

/// The one sentence AWS words an authorization failure with, written out here rather than built
/// from the service's own helper so the wording has something to be wrong about.
pub(crate) fn refusal_message(caller_arn: &str, function_arn: &str) -> String {
    format!(
        "User: {caller_arn} is not authorized to perform: {INVOKE_ACTION} on resource: \
         {function_arn} because no identity-based policy allows the {INVOKE_ACTION} action"
    )
}

/// What the deployed handler returns, which is what an ADMITTED synchronous invocation answers:
/// the package really ran for it, where a refused call carries no such answer. Published beside
/// the harness so the complement suite cannot drift from the source deployed here.
pub(crate) fn handler_answer() -> Value {
    json!({"ran": true})
}

/// A document allowing the invoking action on `resource` and nothing else.
fn allow_invoke(resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": INVOKE_ACTION,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// Put the row's grant in force.
async fn write_grant(
    router: &MultiServiceRouter,
    case: &str,
    principal: &Principal,
    grant: Grant,
    function_arn: &str,
) {
    match grant {
        Grant::IdentityPolicy => {
            let document = allow_invoke(function_arn);
            match &principal.entity {
                Entity::User(name) => {
                    iam(
                        router,
                        case,
                        "PutUserPolicy",
                        &[
                            ("UserName", name),
                            ("PolicyName", POLICY_NAME),
                            ("PolicyDocument", &document),
                        ],
                    )
                    .await;
                }
                Entity::Role(name) => {
                    iam(
                        router,
                        case,
                        "PutRolePolicy",
                        &[
                            ("RoleName", name),
                            ("PolicyName", POLICY_NAME),
                            ("PolicyDocument", &document),
                        ],
                    )
                    .await;
                }
            }
        }
        Grant::ResourcePolicy => {
            add_permission(
                router,
                case,
                FUNCTION,
                STATEMENT_ID,
                &principal.statement_principal,
            )
            .await;
        }
    }
}

/// Take the row's grant back out.
async fn withdraw_grant(
    router: &MultiServiceRouter,
    case: &str,
    principal: &Principal,
    grant: Grant,
) {
    match grant {
        Grant::IdentityPolicy => match &principal.entity {
            Entity::User(name) => {
                iam(
                    router,
                    case,
                    "DeleteUserPolicy",
                    &[("UserName", name), ("PolicyName", POLICY_NAME)],
                )
                .await;
            }
            Entity::Role(name) => {
                iam(
                    router,
                    case,
                    "DeleteRolePolicy",
                    &[("RoleName", name), ("PolicyName", POLICY_NAME)],
                )
                .await;
            }
        },
        Grant::ResourcePolicy => {
            remove_permission(router, case, FUNCTION, STATEMENT_ID).await;
        }
    }
}

/// The statuses Lambda answers an invocation it has not DECIDED with, each of which says "not
/// now" and carries no authorization verdict: no room in the account's concurrent executions
/// (`TooManyRequestsException`, HTTP 429), or the function not being `Active` yet
/// (`ResourceConflictException`, HTTP 409). Whether one arrives is decided by what the rest of
/// this process is running at that instant and by nothing a row varies, so [`invoke_decided`]
/// LOOKS AGAIN on one rather than reading a verdict out of an invocation that was never decided.
/// Nothing is tolerated by looking again: one of these still standing at the deadline fails the
/// row.
///
/// `pub(crate)` for the reason [`invoke_decided`] is.
pub(crate) const UNSERVED: &[u16] = &[409, 429];

/// How long [`invoke_decided`] keeps looking for a decision, call and look-again together, and
/// how long between two looks. It is many times what a decision costs, so a loaded host cannot
/// decide the outcome, and a call that does not answer at all fails the row with its own message
/// rather than hanging the suite.
pub(crate) const DECISION_DEADLINE: Duration = Duration::from_secs(120);

/// Invoke `function` as the credential `access_key_id` names, asking for the type `headers`
/// states, and answer the DECISION: looks again, every [`LOOK`] until [`DECISION_DEADLINE`]
/// passes, for as long as Lambda answers one of the statuses that carry no verdict
/// ([`UNSERVED`]), and fails the row naming the case when a call has not answered by that same
/// deadline — which is the whole span this reader can take, call and look-again together.
///
/// `pub(crate)` because every suite over these routes reads an invocation's verdict this one
/// way, so none of them can drift into tolerating a "not now" as an answer
/// (`crate::lambda_resource_policy_admits_each_invocation_type`,
/// `crate::lambda_invoke_grant_revocation_scope`).
pub(crate) async fn invoke_decided(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    access_key_id: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let deadline = Instant::now() + DECISION_DEADLINE;
    let uri = format!("{FUNCTIONS_URI}/{function}/invocations");
    let payload = json!({"probe": true});
    // The last answer that carried no verdict, so the failure at the deadline reports what
    // Lambda kept saying rather than only that the span ran out.
    let mut unserved: Option<String> = None;
    loop {
        // What is left of the one deadline: the whole reading — the call and the looking
        // again — is bounded by that deadline rather than by it once per call.
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "case {case:?}: Lambda never had room to decide the invocation of {function:?} within \
             {DECISION_DEADLINE:?}, last answered {}",
            unserved.as_deref().unwrap_or("nothing at all")
        );
        let call = lambda_as(
            router,
            access_key_id,
            "POST",
            &uri,
            Some(&payload),
            headers,
        );
        let answer = tokio::time::timeout(left, call).await.unwrap_or_else(|_| {
            panic!(
                "case {case:?}: the invocation of {function:?} had no answer within \
                 {DECISION_DEADLINE:?}"
            )
        });
        if !UNSERVED.contains(&answer.status) {
            return answer;
        }
        unserved = Some(format!("{} with {}", answer.status, answer.text()));
        tokio::time::sleep(LOOK).await;
    }
}

/// `Invoke` this suite's own function synchronously as `access_key_id`, through the one reader
/// of an invocation's verdict above.
async fn invoke(router: &MultiServiceRouter, case: &str, access_key_id: &str) -> Answer {
    invoke_decided(router, case, FUNCTION, access_key_id, &[]).await
}

/// `AddPermission`: one statement under `statement_id` granting `principal` the invoking action
/// on `function`, written as the account's own credential, which is who administers a function's
/// policy.
///
/// `pub(crate)` for the reason [`invoke_decided`] is: the suites over these routes write a grant
/// this one way.
pub(crate) async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    principal: &str,
) {
    let added = lambda_as(
        router,
        ACCOUNT_KEY,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        Some(&json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": principal,
        })),
        &[],
    )
    .await;
    assert_eq!(
        added.status,
        201,
        "case {case:?}: AddPermission({statement_id}) must answer 201, got {} with {}",
        added.status,
        added.text()
    );
}

/// `RemovePermission`: take the statement `statement_id` back out of `function`'s policy.
///
/// `pub(crate)` for the reason [`add_permission`] is.
pub(crate) async fn remove_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
) {
    let removed = lambda_as(
        router,
        ACCOUNT_KEY,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{function}/policy/{statement_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "case {case:?}: RemovePermission({statement_id}) must answer 204, got {} with {}",
        removed.status,
        removed.text()
    );
}

/// Assert that `answer` is the authorization refusal, and that no handler ran for it.
pub(crate) fn assert_refused(case: &str, phase: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status,
        403,
        "case {case:?} {phase}: expected HTTP 403, got {} with {}",
        answer.status,
        answer.text()
    );
    let code = answer.error_code.clone().unwrap_or_default();
    assert!(
        code.contains("AccessDeniedException"),
        "case {case:?} {phase}: expected the AccessDeniedException code, got {code:?} with {}",
        answer.text()
    );
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?} {phase}: expected the refusal {expected:?}, got {}",
        answer.text()
    );
    assert!(
        answer.json().get("ran").is_none(),
        "case {case:?} {phase}: a refused invocation must run no handler, got {}",
        answer.text()
    );
}

/// Assert that `answer` is the handler's own result.
fn assert_ran(case: &str, phase: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        200,
        "case {case:?} {phase}: expected HTTP 200, got {} with {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.json(),
        handler_answer(),
        "case {case:?} {phase}: expected the handler's own result, got {}",
        answer.text()
    );
}

/// Every row: the invocation is refused until a document grants it, served while that document
/// stands, and refused again the moment it is withdrawn.
#[tokio::test]
async fn an_invocation_is_served_only_while_a_document_in_force_grants_it() {
    let router = build_router();
    let setup = "the shared function";
    let role = given_execution_role(&router, setup, "xsvc-invoke-grant-lifetime-exec").await;
    let function_arn = given_function(&router, setup, &role, FUNCTION, STACK).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let principal = match row.caller {
            Caller::User => {
                given_user(&router, case, &format!("xsvc-invoke-caller-{index}")).await
            }
            Caller::AssumedRole => {
                given_session(&router, case, &format!("xsvc-invoke-role-{index}")).await
            }
        };
        let expected = refusal_message(&principal.arn, &function_arn);

        let before = invoke(&router, case, &principal.access_key_id).await;
        assert_refused(case, "before any grant", &before, &expected);

        write_grant(&router, case, &principal, row.grant, &function_arn).await;
        let granted = invoke(&router, case, &principal.access_key_id).await;
        assert_ran(case, "while the grant stands", &granted);

        withdraw_grant(&router, case, &principal, row.grant).await;
        let after = invoke(&router, case, &principal.access_key_id).await;
        assert_refused(case, "after the grant was withdrawn", &after, &expected);
    }
}
