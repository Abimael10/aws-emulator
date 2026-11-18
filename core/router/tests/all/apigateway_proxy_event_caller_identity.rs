// SPDX-License-Identifier: CC0-1.0

//! What an `AWS_PROXY` integration's event says about WHO called a deployed REST stage, driven
//! end to end through the whole assembled router.
//!
//! Four services and one engine, so this is a router suite: API Gateway resolves the stage and
//! builds the event, Lambda runs the handler that reports it back, IAM holds the user whose
//! long-term access key signs the invoke, and CloudFormation archives the inline handler into
//! the deployment package Lambda runs.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A Lambda proxy integration's event (payload format `1.0`) carries a `requestContext`
//! > whose `identity` object says who made the call: `sourceIp` is the address the request
//! > arrived from — always present, and never the unspecified address — and `userAgent` is the
//! > user agent the request declared. For a method whose authorization AUTHENTICATES a
//! > principal (`authorizationType` `AWS_IAM`, decided from the request's SigV4 credential),
//! > the same object names that principal: `accessKey` is the key the request signed with,
//! > `accountId` the account the principal belongs to, `caller` and `user` its unique id and
//! > `userArn` its ARN. A method that authenticates nobody reports none of those members. Each
//! > value is read off THIS request and the records the emulator holds for the credential it
//! > presented, so two requests signed by two principals report two callers.
//!
//! The rows span what that rule generalises over: WHICH method answered (one whose
//! `authorizationType` is `AWS_IAM`, one whose is `NONE`), WHICH credential signed the request
//! (an IAM user's own long-term access key, a key no identity registry holds, none at all) and
//! WHAT the request says about the address it came from (no forwarded header, a chain of hops,
//! a single hop written with surrounding spaces).
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling `apigateway_iam_authorized_invoke` suite does.

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

/// The stage the api is deployed to.
const STAGE: &str = "prod";

/// The resource path of the `AWS_IAM` method, and of the one whose `authorizationType` is
/// `NONE`.
const IAM_PATH: &str = "visit";
const OPEN_PATH: &str = "open";

/// The function both methods proxy to, and the stack that declares it.
const FUNCTION: &str = "apigw-caller-identity";
const STACK: &str = "apigw-caller-identity-stack";

/// The execution role the function is declared with, which Lambda assumes to run it. The
/// handler makes no call of its own, so the role carries the trust policy alone.
const ROLE_NAME: &str = "caller-identity-lambda-exec";

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

/// The IAM user whose own long-term access key signs the `AWS_IAM` rows, and the inline policy
/// that lets it invoke the method.
const USER_NAME: &str = "visit-caller";
const POLICY_NAME: &str = "invoke-every-method";

/// An access key id no identity registry of this emulator holds a record for, which therefore
/// speaks for the account's own published identity.
const UNREGISTERED_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The user agent every row declares, which the event reports as `identity.userAgent`.
const USER_AGENT: &str = "caller-identity-probe/1.0";

/// The address a handler falls back to when the event carries no usable `sourceIp`, which is
/// exactly what the member must never be.
const UNSPECIFIED_ADDRESS: &str = "0.0.0.0";

/// The handler both methods proxy to: it reports the event's `requestContext.identity`
/// verbatim, so what is asserted is the event the function was handed.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    identity = (event.get("requestContext") or {}).get("identity")
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"identity": identity}),
    }
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
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
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// A SigV4 `Authorization` for `access_key_id` whose credential scope names `signing_name`,
/// which is what routes the request. The signature is never checked here — this router runs
/// with authentication off — but the KEY ID is what the invoke resolves its principal from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let credential = authorization(UNREGISTERED_KEY_ID, "apigateway");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
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
    .await;
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
    let credential = authorization(UNREGISTERED_KEY_ID, "lambda");
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
    let credential = authorization(UNREGISTERED_KEY_ID, signing_name);
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
        answered.status,
        200,
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

/// The ACCOUNT field of an ARN, which is what the identity's `accountId` has to name for a
/// principal of that account.
fn account_of(arn: &str) -> String {
    arn.split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("the ARN {arn:?} carries no account field"))
        .to_string()
}

/// Deploy the stack that declares the function: the inline source is archived into the
/// deployment package by CloudFormation, which is how a handler written here comes to be one
/// Lambda can run. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Reporter": {
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
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
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

/// Put one method of `verb` on `resource_id` with `authorization_type`, behind an `AWS_PROXY`
/// integration on `function_arn`.
async fn proxy_method(
    router: &MultiServiceRouter,
    api: &str,
    resource_id: &str,
    authorization_type: &str,
    function_arn: &str,
) {
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": authorization_type })),
        201,
    )
    .await;
    apigw(
        router,
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
}

/// Which credential a row signs its invoke with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signer {
    /// The IAM user's own long-term access key, which IAM holds a record of.
    IamUsersOwnKey,
    /// An access key id no identity registry holds, which speaks for the account itself.
    UnregisteredKey,
    /// No credential at all.
    Nothing,
}

/// WHO the event the row's call produced must name as its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The IAM user this suite created: every caller member is that user's own record.
    TheIamUser,
    /// The account's own published identity: an IAM principal of the same account, and NOT
    /// the user above.
    TheAccountsOwnIdentity,
    /// The method authenticates nobody, so no caller member is reported at all.
    Nobody,
}

/// One invoke of a deployed method: the path it addresses, what it signs with, what it says
/// about where it came from, and who the event must name.
struct Row {
    case: &'static str,
    path: &'static str,
    signer: Signer,
    /// The `X-Forwarded-For` the request carries, or `None` for a request no forwarder
    /// recorded an address for.
    forwarded: Option<&'static str>,
    source_ip: &'static str,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "an AWS_IAM method signed with an IAM user's own access key, reached directly",
        path: IAM_PATH,
        signer: Signer::IamUsersOwnKey,
        forwarded: None,
        source_ip: "127.0.0.1",
        expected: Expected::TheIamUser,
    },
    Row {
        case: "the same method and key behind a chain of forwarders",
        path: IAM_PATH,
        signer: Signer::IamUsersOwnKey,
        forwarded: Some("203.0.113.7, 198.51.100.2"),
        source_ip: "203.0.113.7",
        expected: Expected::TheIamUser,
    },
    Row {
        case: "the same method and key behind one forwarded hop written with spaces",
        path: IAM_PATH,
        signer: Signer::IamUsersOwnKey,
        forwarded: Some("  198.51.100.77  "),
        source_ip: "198.51.100.77",
        expected: Expected::TheIamUser,
    },
    Row {
        case: "an AWS_IAM method signed with a key no identity registry holds",
        path: IAM_PATH,
        signer: Signer::UnregisteredKey,
        forwarded: Some("192.0.2.33"),
        source_ip: "192.0.2.33",
        expected: Expected::TheAccountsOwnIdentity,
    },
    Row {
        case: "a method whose authorizationType is NONE, signed all the same",
        path: OPEN_PATH,
        signer: Signer::IamUsersOwnKey,
        forwarded: None,
        source_ip: "127.0.0.1",
        expected: Expected::Nobody,
    },
    Row {
        case: "a method whose authorizationType is NONE, carrying no credential at all",
        path: OPEN_PATH,
        signer: Signer::Nothing,
        forwarded: Some("198.51.100.9, 203.0.113.1"),
        source_ip: "198.51.100.9",
        expected: Expected::Nobody,
    },
];

/// The IAM user as IAM holds it: the three facts the event's caller members name it by.
struct IamUser {
    access_key_id: String,
    user_id: String,
    arn: String,
}

/// The member `name` of the identity object the handler reported, or `None` when the event
/// carried none.
fn identity_member(identity: &Value, name: &str) -> Option<String> {
    identity
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[test]
fn a_proxy_events_identity_names_the_address_and_the_principal_of_the_call() {
    std::thread::Builder::new()
        .name("apigateway-proxy-event-caller-identity".to_string())
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

                // 1. The function the two methods proxy to, the role Lambda runs it as, and
                //    the grant that admits an invocation from API Gateway.
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
                    granted.status,
                    201,
                    "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
                    granted.status,
                    granted.text()
                );

                // 2. The IAM user whose own long-term key signs the AWS_IAM rows, granted the
                //    invoke its method's authorization asks IAM about.
                let created = iam(&router, "CreateUser", &[("UserName", USER_NAME)]).await;
                let user_arn = element(&created, "Arn");
                let user_id = element(&created, "UserId");
                iam(
                    &router,
                    "PutUserPolicy",
                    &[
                        ("UserName", USER_NAME),
                        ("PolicyName", POLICY_NAME),
                        (
                            "PolicyDocument",
                            &json!({
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": "execute-api:Invoke",
                                    "Resource": "*",
                                }],
                            })
                            .to_string(),
                        ),
                    ],
                )
                .await;
                let minted = iam(&router, "CreateAccessKey", &[("UserName", USER_NAME)]).await;
                let user = IamUser {
                    access_key_id: element(&minted, "AccessKeyId"),
                    user_id,
                    arn: user_arn,
                };
                let account = account_of(&user.arn);

                // 3. The api: one AWS_IAM method and one whose authorizationType is NONE, each
                //    behind an AWS_PROXY integration on that function, deployed.
                let api_record = apigw(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "caller-identity-api" })),
                    201,
                )
                .await;
                let api = member("CreateRestApi", &api_record, "id");
                let root = member("CreateRestApi", &api_record, "rootResourceId");
                for (path_part, authorization_type) in
                    [(IAM_PATH, "AWS_IAM"), (OPEN_PATH, "NONE")]
                {
                    let resource = apigw(
                        &router,
                        "CreateResource",
                        "POST",
                        &format!("/restapis/{api}/resources/{root}"),
                        Some(json!({ "pathPart": path_part })),
                        201,
                    )
                    .await;
                    let resource_id = member("CreateResource", &resource, "id");
                    proxy_method(
                        &router,
                        &api,
                        &resource_id,
                        authorization_type,
                        &function_arn,
                    )
                    .await;
                }
                apigw(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // 4. One invoke per row, and what the event the function was handed said about
                //    the caller.
                let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
                for row in ROWS {
                    let case = row.case;
                    let credential = match row.signer {
                        Signer::IamUsersOwnKey => {
                            Some(authorization(&user.access_key_id, "execute-api"))
                        }
                        Signer::UnregisteredKey => {
                            Some(authorization(UNREGISTERED_KEY_ID, "execute-api"))
                        }
                        Signer::Nothing => None,
                    };
                    let mut headers: Vec<(&str, &str)> = vec![("user-agent", USER_AGENT)];
                    if let Some(credential) = credential.as_deref() {
                        headers.push(("authorization", credential));
                    }
                    if let Some(forwarded) = row.forwarded {
                        headers.push(("x-forwarded-for", forwarded));
                    }
                    let answered = dispatch(
                        &router,
                        "GET",
                        &invoke_host,
                        &format!("/{STAGE}/{path}", path = row.path),
                        &headers,
                        Bytes::new(),
                    )
                    .await;
                    assert_eq!(
                        answered.status, 200,
                        "case {case:?}: expected the handler's 200 from the stage, got {} with \
                         {:?}",
                        answered.status,
                        answered.text()
                    );
                    let body = answered.json();
                    let identity = body.get("identity").cloned().unwrap_or(Value::Null);
                    assert!(
                        identity.is_object(),
                        "case {case:?}: expected the event's requestContext.identity to be an \
                         object, got {body}"
                    );

                    // The address the call came from: present, the one the request says it
                    // arrived from, and never the unspecified address a handler falls back to.
                    let source_ip = identity_member(&identity, "sourceIp");
                    assert_eq!(
                        source_ip.as_deref(),
                        Some(row.source_ip),
                        "case {case:?}: expected identity.sourceIp {:?}, got {identity}",
                        row.source_ip
                    );
                    assert_ne!(
                        source_ip.as_deref(),
                        Some(UNSPECIFIED_ADDRESS),
                        "case {case:?}: identity.sourceIp must be a real caller address, got \
                         {identity}"
                    );
                    assert_eq!(
                        identity_member(&identity, "userAgent").as_deref(),
                        Some(USER_AGENT),
                        "case {case:?}: expected identity.userAgent {USER_AGENT:?}, got \
                         {identity}"
                    );

                    // WHO the method's authorization authenticated for the call.
                    let caller_members = ["accessKey", "accountId", "caller", "user", "userArn"];
                    match row.expected {
                        Expected::TheIamUser => {
                            let wanted: &[(&str, &str)] = &[
                                ("accessKey", &user.access_key_id),
                                ("accountId", &account),
                                ("caller", &user.user_id),
                                ("user", &user.user_id),
                                ("userArn", &user.arn),
                            ];
                            for (name, expected) in wanted {
                                let reported = identity_member(&identity, name);
                                assert_eq!(
                                    reported.as_deref(),
                                    Some(*expected),
                                    "case {case:?}: expected identity.{name} {expected:?}, got \
                                     {identity}"
                                );
                            }
                        }
                        Expected::TheAccountsOwnIdentity => {
                            assert_eq!(
                                identity_member(&identity, "accessKey").as_deref(),
                                Some(UNREGISTERED_KEY_ID),
                                "case {case:?}: expected identity.accessKey to be the key the \
                                 request signed with, got {identity}"
                            );
                            assert_eq!(
                                identity_member(&identity, "accountId").as_deref(),
                                Some(account.as_str()),
                                "case {case:?}: expected identity.accountId {account:?}, got \
                                 {identity}"
                            );
                            let arn = identity_member(&identity, "userArn").unwrap_or_default();
                            assert!(
                                arn.starts_with("arn:") && arn.contains(&account),
                                "case {case:?}: expected identity.userArn to name an IAM \
                                 principal of account {account:?}, got {identity}"
                            );
                            assert_ne!(
                                arn, user.arn,
                                "case {case:?}: expected a caller other than the IAM user, got \
                                 {identity}"
                            );
                            let caller = identity_member(&identity, "caller").unwrap_or_default();
                            assert_eq!(
                                Some(caller.as_str()),
                                identity_member(&identity, "user").as_deref(),
                                "case {case:?}: expected identity.caller and identity.user to \
                                 name one principal, got {identity}"
                            );
                            assert!(
                                !caller.is_empty() && caller != user.user_id,
                                "case {case:?}: expected the account's own principal id, got \
                                 {identity}"
                            );
                        }
                        Expected::Nobody => {
                            for name in caller_members {
                                assert_eq!(
                                    identity_member(&identity, name),
                                    None,
                                    "case {case:?}: a method that authenticates nobody reports \
                                     no identity.{name}, got {identity}"
                                );
                            }
                        }
                    }
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
