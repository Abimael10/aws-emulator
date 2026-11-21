// SPDX-License-Identifier: CC0-1.0

//! The DEBUGGING lifecycle of an `AWS_IAM`-authorized REST method: inspect the method through
//! the control plane, inspect the caller's role through IAM, exercise the deployed stage, read
//! what the run left in CloudWatch Logs, and reap that log group again.
//!
//! RULE: **every call of that lifecycle terminates with an answer computed from the records as
//! they stand.** Concretely, and in the order a client makes them:
//!
//! * a control-plane read reports the methods the api holds — `GetResources` keys each
//!   resource's `resourceMethods` by the verbs PUT on that resource (the embedded `Method` only
//!   where `embed=methods` asked for it, and no map at all for a resource carrying no method),
//!   and `GetMethod` reports that method's own `authorizationType`;
//! * an IAM read reports the role and the inline document the estate created (`ListRoles`,
//!   `ListRolePolicies`, `GetRolePolicy`, `GetRole`);
//! * a request to the deployed stage is either SERVED by the method's integration or REFUSED,
//!   and never neither: a signed request whose principal no policy allows `execute-api:Invoke`
//!   on the method ARN for is answered `403` with the sentence IAM words an authorization
//!   failure in, and one it does allow is answered the integration's own response;
//! * `TestInvokeMethod` answers the status/body/log/latency envelope it models;
//! * the function's own log group holds the run's records, and a client's `DeleteLogGroup`
//!   takes it away — so the next `DescribeLogGroups` of that name reports nothing and a second
//!   delete is refused `ResourceNotFoundException`, which is what lets a teardown that reaps a
//!   log group until it is gone STOP;
//! * and the teardown itself terminates: each delete of the estate is answered its modelled
//!   success and what it removed is gone from the next read of the service that held it.
//!
//! Seven crates have to agree for that to hold (IAM holds the roles and documents, STS mints the
//! caller's sessions, CloudFormation archives the inline handler into a package this emulator
//! can run, Lambda runs it, DynamoDB serves the read behind it, API Gateway resolves the stage
//! and takes the authorization decision, CloudWatch Logs holds the group), so the suite lives
//! here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: WHICH resource and `embed` spelling a
//! control-plane read names, WHICH `authorizationType` the read method carries, and WHICH
//! credential signs the data-plane request (a session granted the invoke on every verb of the
//! stage, one granted only the method's own concrete ARN, one granted another verb, one granted
//! nothing at all, and no credential whatsoever).
//!
//! What the neighbouring suites pin and this one does not repeat:
//! `apigateway_iam_authorized_invoke` pins WHICH grants the gate admits,
//! `apigateway_proxy_stage_surfaces_its_handlers_envelope` pins the mapping of the handler's own
//! envelope, and `apigateway_proxy_lambda_ddb_pipeline` pins what an invocation leaves in the
//! function's log group. What only this suite reaches is the whole DEBUGGING sequence over one
//! estate: the control-plane and IAM reads a client derives the policy fix from, the simulated
//! invocation beside the signed one, and the terminating reap of the log group afterwards.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_provisions_logs_log_groups::wait_for_stack_gone;
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region of the table, the function,
/// the api's own invoke host and every ARN a policy names. It is the Region the shared stack
/// wait polls in ([`wait_for_stack_status`]).
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The table the handler reads, its key attribute, and the one agreement it holds.
const TABLE: &str = "xsvc-iam-debug-agreements";
const KEY_ATTRIBUTE: &str = "agreementId";
const AGREEMENT: &str = "agr-4711";
const AGREEMENT_TEXT: &str = "the agreement the handler answers with";

/// The function the stage's integration invokes, the role it runs as, and the stack that
/// archives its inline source into a deployment package.
const FUNCTION: &str = "xsvc-iam-debug-reader";
const EXECUTION_ROLE: &str = "xsvc-iam-debug-reader-role";
const STACK: &str = "xsvc-iam-debug-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to, the collection resource, the templated child under it that
/// carries the `AWS_IAM` method, and a sibling resource whose method is open.
const STAGE: &str = "beta";
const COLLECTION: &str = "agreements";
const CHILD: &str = "{agreementId}";
const OPEN_PATH: &str = "catalogue";

/// The two `authorizationType` values this estate carries, from `Method$authorizationType`:
/// "valid values are `NONE` for open access, `AWS_IAM` for using AWS IAM permissions".
const AWS_IAM: &str = "AWS_IAM";
const NONE: &str = "NONE";

/// The IAM action an invoke of a deployed method asks for, and the one the handler's own read
/// performs.
const INVOKE_ACTION: &str = "execute-api:Invoke";
const READ_ACTION: &str = "dynamodb:GetItem";

/// The name the caller roles embed their invoke document under, which is what
/// `ListRolePolicies` lists and `GetRolePolicy` reads.
const INVOKE_POLICY: &str = "invoke";

/// The two sentences an API Gateway refusal of an `AWS_IAM` method carries: the IAM
/// authorization failure, with the clause naming WHY nothing allowed the call, and the answer a
/// request carrying no credential at all is given.
const NOT_AUTHORIZED_CLAUSE: &str = "is not authorized to perform: execute-api:Invoke";
const IMPLICIT_DENY_CLAUSE: &str =
    "because no identity-based policy allows the execute-api:Invoke action";
const MISSING_TOKEN: &str = "Missing Authentication Token";

/// The error CloudWatch Logs refuses a read or a delete of a group it does not hold with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The fixed framing the Lambda runtime writes for every run it finishes, which is what a
/// reader of the function's log group finds there.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The line the handler itself prints, so a reader can tell the run's own record from the
/// runtime's framing.
const HANDLED_PREFIX: &str = "read agreement ";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the lifecycle and not how fast an interpreter gets
/// through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created table reaching `ACTIVE`,
/// a created function reaching `Active`, a run's records reaching its log group — and how many
/// looks to take. The spans are those services' own, so no fixed sleep names the instant one
/// ends; the budget is one minute per wait, far above the seconds a transition takes so a
/// loaded host cannot decide the outcome, and low enough that a stuck emulator fails this
/// suite promptly instead of spinning.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 600;

/// The handler the stage's integration invokes: it reads the agreement the request's path names
/// with the SDK the runtime supplies, prints one line about it, and answers a proxy envelope.
///
/// `TABLE_NAME` and `KEY_NAME` are substituted before the stack is deployed.
const READER_HANDLER: &str = r##"
import json

import boto3


def handler(event, context):
    asked = (event.get("pathParameters") or {}).get("KEY_NAME")
    print("read agreement %s" % asked)
    read = boto3.client("dynamodb").get_item(
        TableName="TABLE_NAME",
        Key={"KEY_NAME": {"S": asked}},
        ConsistentRead=True,
    )
    item = read.get("Item")
    if item is None:
        return {
            "statusCode": 404,
            "headers": {"Content-Type": "application/json"},
            "body": json.dumps({"error": "no agreement under that id"}),
        }
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {"agreementId": item["KEY_NAME"]["S"], "text": item["text"]["S"]}
        ),
    }
"##;

/// The handler source as it is deployed: the table and key attribute it reads named in it.
fn handler_source() -> String {
    READER_HANDLER
        .replace("TABLE_NAME", TABLE)
        .replace("KEY_NAME", KEY_ATTRIBUTE)
}

/// The body the handler answers for the agreement the table holds, computed here rather than
/// read back out of the answer under test.
fn served_body() -> Value {
    json!({ "agreementId": AGREEMENT, "text": AGREEMENT_TEXT })
}

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The `Message` or `message` member an API Gateway answer carries, or the whole body.
    fn message(&self) -> String {
        let body = self.json();
        for name in ["Message", "message"] {
            if let Some(message) = body.get(name).and_then(Value::as_str) {
                return message.to_string();
            }
        }
        self.text()
    }

    /// The error shape a refusal names: the `x-amzn-errortype` header where the envelope
    /// carries one, else the body's `__type` discriminator, in both cases without the
    /// namespace a shape id may qualify it with.
    fn code(&self) -> String {
        let raw = self
            .header("x-amzn-errortype")
            .or_else(|| {
                self.json()
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        raw.rsplit('#')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the table it reads and the group its run logs to are the ones this
/// suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate: the credential
    // scope is what routes a request, and its access key id is what an invoke's authorization
    // resolves its principal from.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which resolves to no session and so speaks for the
/// account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name` and whose credential is
/// `access_key_id`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
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
        headers: parts.headers,
        body,
    }
}

/// One awsQuery call of `service`, made as the account's own credential.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(ACCOUNT_KEY_ID, service)),
        ],
        Bytes::from(body),
    )
    .await
}

/// One awsQuery call that has to be accepted, answered with its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let answered = query(router, service, version, action, members).await;
    assert_eq!(
        answered.status,
        200,
        "{service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One IAM call that has to be accepted.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    accepted_query(router, "iam", "2010-05-08", action, members).await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire, made as the account's own credential.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization(ACCOUNT_KEY_ID, "dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization(ACCOUNT_KEY_ID, "logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization(ACCOUNT_KEY_ID, "lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call, answered as it came back.
async fn apigw_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization(ACCOUNT_KEY_ID, "apigateway")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let answered = apigw_call(router, method, path, body).await;
    assert_eq!(
        answered.status,
        expected,
        "{what} must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// The ARN of the table the handler reads, which the execution role's grant names.
fn table_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}")
}

/// A trust policy naming one AWS service as the principal that may assume the role.
fn service_trust_policy(service_principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service_principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A trust policy letting this account's own principals assume the role, which is what a caller
/// of the api assumes a role of its own account with.
fn account_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// One identity-based document stating `effect` about `action` on `resource`.
fn policy_document(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

/// The document every execution role needs: the log actions a run writes with, and the read the
/// handler performs.
fn execution_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            { "Effect": "Allow", "Action": READ_ACTION, "Resource": table_arn() },
        ],
    })
    .to_string()
}

/// Create a role with `trust_policy` and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, trust_policy: &str) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust_policy)],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole({name:?}) must answer the role's ARN"))
}

/// Embed one inline document in a role.
async fn put_role_policy(router: &MultiServiceRouter, role: &str, policy: &str, document: &str) {
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", policy),
            ("PolicyDocument", document),
        ],
    )
    .await;
}

/// Assume `role_arn` and answer the access key id of the session STS minted.
async fn assume_role(router: &MultiServiceRouter, role_arn: &str, session: &str) -> String {
    let assumed = accepted_query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", session)],
    )
    .await;
    opt_element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("AssumeRole({role_arn:?}) must answer an access key id"))
}

/// Create the table the handler reads, holding the one agreement, and wait until DynamoDB
/// reports it `ACTIVE` — "you can perform read and write operations only on an ACTIVE table".
async fn create_table(router: &MultiServiceRouter) {
    let created = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
            "AttributeDefinitions": [
                { "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "CreateTable({TABLE:?}) must succeed, got {}",
        created.summary()
    );
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = dynamodb(router, "DescribeTable", &json!({ "TableName": TABLE }))
            .await
            .json();
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        described["Table"]["TableStatus"].as_str(),
        Some("ACTIVE"),
        "DynamoDB must report {TABLE:?} ACTIVE, got {described}"
    );
    let written = dynamodb(
        router,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {
                KEY_ATTRIBUTE: { "S": AGREEMENT },
                "text": { "S": AGREEMENT_TEXT },
            },
        }),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "PutItem({AGREEMENT:?}) must succeed, got {}",
        written.summary()
    );
}

/// Deploy the stack declaring the function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Reader": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": handler_source() },
                },
            },
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "the stack must reach {CREATE_COMPLETE}, got {described}"
    );

    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
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
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "Lambda must report {FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// The estate this suite drives, as the ids its own creation minted.
struct Estate {
    api: String,
    /// The templated child resource that carries the `AWS_IAM` method.
    child: String,
    /// The collection resource above it, which carries no method of its own.
    collection: String,
    /// The api's root resource.
    root: String,
    /// The sibling resource whose method is open.
    open: String,
}

/// Stand the api up: `/<collection>/{agreementId}` with an `AWS_IAM` `GET` behind an
/// `AWS_PROXY` integration on `function_arn`, a sibling `/<open>` with an open `GET` behind a
/// `MOCK` integration, and a deployment of both to the stage.
async fn deploy_api(router: &MultiServiceRouter, function_arn: &str) -> Estate {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "xsvc-iam-debug-api" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");

    let collection = as_str(
        "CreateResource",
        &apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": COLLECTION })),
            201,
        )
        .await,
        "id",
    );
    let child = as_str(
        "CreateResource",
        &apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{collection}"),
            Some(json!({ "pathPart": CHILD })),
            201,
        )
        .await,
        "id",
    );
    let method = format!("/restapis/{api}/resources/{child}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": AWS_IAM })),
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
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;

    let open = as_str(
        "CreateResource",
        &apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": OPEN_PATH })),
            201,
        )
        .await,
        "id",
    );
    let open_method = format!("/restapis/{api}/resources/{open}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &open_method,
        Some(json!({ "authorizationType": NONE })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{open_method}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    Estate {
        api,
        child,
        collection,
        root,
        open,
    }
}

/// What a `GetResources` read of one resource must report under `resourceMethods`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Methods {
    /// No map at all: the resource carries no method.
    None,
    /// The verbs the resource carries, each under an EMPTY `Method` body: what a read that did
    /// not ask for the embedded methods reports.
    VerbsOnly(&'static str),
    /// The verb with its own `Method` embedded, carrying this `authorizationType`.
    Embedded(&'static str, &'static str),
}

/// One control-plane read of the api's resources.
struct ResourceRow {
    case: &'static str,
    /// The `embed` query the read carries, as it is spelled on the wire.
    embed: &'static str,
    /// Which resource of the estate the row is about, and what it must report.
    expected: Methods,
    /// The resource's own `path`, computed from what the estate declared.
    path: String,
    /// The resource's id.
    id: String,
}

/// How an `execute-api` refusal reports the ACCOUNT field of the method ARN it names: all but
/// the last four digits replaced by asterisks, so the sentence handed an unauthorized caller
/// does not publish the whole account id. Computed here from the account rather than written
/// out, so the row asserts the FORM and not one account's digits.
fn masked_account() -> String {
    let unmasked = ACCOUNT.len().saturating_sub(4);
    format!("{}{}", "*".repeat(unmasked), &ACCOUNT[unmasked..])
}

/// The sentence an API Gateway refusal must carry, for the method ARN of this request.
fn denied_message(principal_role: &str, session: &str, api: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{principal_role}/{session} \
         {NOT_AUTHORIZED_CLAUSE} on resource: \
         arn:aws:execute-api:{REGION}:{}:{api}/{STAGE}/GET/{COLLECTION}/{AGREEMENT} \
         {IMPLICIT_DENY_CLAUSE}",
        masked_account()
    )
}

/// What the gate owes one data-plane row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owed {
    /// The request was authorized and the integration's own answer is what the client holds.
    Served,
    /// Nothing in force allows the invoke: `403` with the IAM authorization sentence.
    NotAuthorized,
    /// The method is `AWS_IAM` and the request carried no credential at all.
    MissingToken,
}

/// One request to the deployed stage: the credential that signs it, and what the gate owes it.
struct InvokeRow {
    case: &'static str,
    /// The role whose session signs the request, with the session name it was minted under, or
    /// `None` to carry no credential at all.
    signer: Option<(String, &'static str, String)>,
    owed: Owed,
}

#[tokio::test]
async fn the_iam_authorized_rest_lifecycle_answers_every_read_invoke_and_reap() {
    let router = emulator();

    // 1. The estate: the table and its one agreement, the function that reads it, and the api
    //    that fronts it, deployed.
    create_table(&router).await;
    let execution_role = create_role(
        &router,
        EXECUTION_ROLE,
        &service_trust_policy("lambda.amazonaws.com"),
    )
    .await;
    put_role_policy(&router, EXECUTION_ROLE, "execution", &execution_document()).await;
    let function_arn = deploy_function(&router, &execution_role).await;
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
        "AddPermission must answer 201, got {}",
        granted.summary()
    );
    let estate = deploy_api(&router, &function_arn).await;

    // 2. The control-plane reads a client inspects the deployed method through. Every row is one
    //    `GetResources` of the whole api, read for the row's own resource.
    let method_arn_prefix = format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{}", estate.api);
    let resource_rows: Vec<ResourceRow> = vec![
        ResourceRow {
            case: "the root resource, which carries no method",
            embed: "",
            expected: Methods::None,
            path: "/".to_string(),
            id: estate.root.clone(),
        },
        ResourceRow {
            case: "the collection resource, which carries no method either",
            embed: "",
            expected: Methods::None,
            path: format!("/{COLLECTION}"),
            id: estate.collection.clone(),
        },
        ResourceRow {
            case: "the templated child, read without asking for the embedded methods",
            embed: "",
            expected: Methods::VerbsOnly("GET"),
            path: format!("/{COLLECTION}/{CHILD}"),
            id: estate.child.clone(),
        },
        ResourceRow {
            case: "the templated child, read with embed=methods",
            embed: "?embed=methods",
            expected: Methods::Embedded("GET", AWS_IAM),
            path: format!("/{COLLECTION}/{CHILD}"),
            id: estate.child.clone(),
        },
        ResourceRow {
            case: "the open sibling, read with embed=methods",
            embed: "?embed=methods",
            expected: Methods::Embedded("GET", NONE),
            path: format!("/{OPEN_PATH}"),
            id: estate.open.clone(),
        },
    ];
    for row in &resource_rows {
        let case = row.case;
        let listed = apigw(
            &router,
            "GetResources",
            "GET",
            &format!("/restapis/{}/resources{}", estate.api, row.embed),
            None,
            200,
        )
        .await;
        // `Resources$items` travels under its `jsonName`, `item`
        // (`smithy_query(query="operation",operation="GetResources")`).
        let items = listed
            .get("item")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let found = items
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(row.id.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: GetResources must report resource {:?}, got {listed}", row.id)
            });
        assert_eq!(
            found.get("path").and_then(Value::as_str),
            Some(row.path.as_str()),
            "case {case:?}: expected path {:?}, got {found}",
            row.path
        );
        let reported = found.get("resourceMethods");
        match row.expected {
            // "A resource with no methods reports no map at all": the member carries nothing,
            // which is an absent member — not the empty map, which would say the resource has
            // a (zero-length) method collection.
            Methods::None => assert!(
                reported.is_none_or(Value::is_null),
                "case {case:?}: expected no resourceMethods map at all, got {found}"
            ),
            Methods::VerbsOnly(verb) => assert_eq!(
                reported,
                Some(&json!({ verb: {} })),
                "case {case:?}: expected the verb {verb:?} under an empty Method, got {found}"
            ),
            Methods::Embedded(verb, authorization_type) => {
                let embedded = reported
                    .and_then(|map| map.get(verb))
                    .unwrap_or_else(|| panic!(
                        "case {case:?}: expected the embedded {verb:?} method, got {found}"
                    ));
                assert_eq!(
                    embedded.get("authorizationType").and_then(Value::as_str),
                    Some(authorization_type),
                    "case {case:?}: expected authorizationType {authorization_type:?}, got \
                     {embedded}"
                );
                assert_eq!(
                    embedded.get("httpMethod").and_then(Value::as_str),
                    Some(verb),
                    "case {case:?}: expected the embedded method to name its verb, got {embedded}"
                );
            }
        }
    }

    // `GetMethod` reports the same `authorizationType` the embedded read did, for each method
    // the estate put.
    for (case, resource, expected) in [
        ("the IAM-authorized method", &estate.child, AWS_IAM),
        ("the open method", &estate.open, NONE),
    ] {
        let read = apigw(
            &router,
            "GetMethod",
            "GET",
            &format!("/restapis/{}/resources/{resource}/methods/GET", estate.api),
            None,
            200,
        )
        .await;
        assert_eq!(
            (
                read.get("authorizationType").and_then(Value::as_str),
                read.get("httpMethod").and_then(Value::as_str),
            ),
            (Some(expected), Some("GET")),
            "case {case:?}: expected GetMethod to report authorizationType {expected:?} on GET, \
             got {read}"
        );
    }

    // 3. The callers: one session per grant the rule generalises over.
    let every_verb_role = "xsvc-iam-debug-every-verb";
    let this_method_role = "xsvc-iam-debug-this-method";
    let other_verb_role = "xsvc-iam-debug-other-verb";
    let ungranted_role = "xsvc-iam-debug-ungranted";
    let mut signers: Vec<(String, &'static str, String)> = Vec::new();
    for (role, document) in [
        (
            every_verb_role,
            Some(policy_document(
                "Allow",
                INVOKE_ACTION,
                &format!("{method_arn_prefix}/{STAGE}/GET/*"),
            )),
        ),
        (
            this_method_role,
            Some(policy_document(
                "Allow",
                INVOKE_ACTION,
                &format!("{method_arn_prefix}/{STAGE}/GET/{COLLECTION}/{AGREEMENT}"),
            )),
        ),
        (
            other_verb_role,
            Some(policy_document(
                "Allow",
                INVOKE_ACTION,
                &format!("{method_arn_prefix}/{STAGE}/POST/*"),
            )),
        ),
        (ungranted_role, None),
    ] {
        let arn = create_role(&router, role, &account_trust_policy()).await;
        if let Some(document) = &document {
            put_role_policy(&router, role, INVOKE_POLICY, &document.to_string()).await;
        }
        let session = "debug-session";
        let key_id = assume_role(&router, &arn, session).await;
        signers.push((role.to_string(), session, key_id));
    }
    let signer = |role: &str| -> (String, &'static str, String) {
        signers
            .iter()
            .find(|(name, _, _)| name == role)
            .cloned()
            .unwrap_or_else(|| panic!("the suite minted a session of {role:?}"))
    };

    // 4. The IAM reads a client derives the policy fix from: the role, its inline documents, and
    //    the document itself.
    let listed_roles = iam(&router, "ListRoles", &[]).await;
    for role in [
        EXECUTION_ROLE,
        every_verb_role,
        this_method_role,
        other_verb_role,
        ungranted_role,
    ] {
        assert!(
            elements(&listed_roles, "RoleName").iter().any(|name| name == role),
            "ListRoles must report {role:?}, got {listed_roles}"
        );
    }
    for (case, role, documents) in [
        ("the restricted caller", other_verb_role, vec![INVOKE_POLICY]),
        ("the caller granted nothing", ungranted_role, vec![]),
    ] {
        let listed = iam(&router, "ListRolePolicies", &[("RoleName", role)]).await;
        assert_eq!(
            elements(&listed, "member"),
            documents
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected ListRolePolicies to report {documents:?}, got {listed}"
        );
    }
    let read_document = iam(
        &router,
        "GetRolePolicy",
        &[("RoleName", other_verb_role), ("PolicyName", INVOKE_POLICY)],
    )
    .await;
    // "The policy document returned in this response is URL-encoded compliant with RFC 3986"
    // (`GetRolePolicy`), so the comparison with the document that was put reads it through the
    // decode a caller performs.
    let decoded = ::smithy_server_runtime::percent_decode(
        &opt_element(&read_document, "PolicyDocument").unwrap_or_default(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&decoded).unwrap_or(Value::Null),
        policy_document(
            "Allow",
            INVOKE_ACTION,
            &format!("{method_arn_prefix}/{STAGE}/POST/*")
        ),
        "GetRolePolicy must report the document that was put, got {decoded}"
    );
    let read_role = iam(&router, "GetRole", &[("RoleName", other_verb_role)]).await;
    assert_eq!(
        element(&read_role, "RoleName"),
        other_verb_role,
        "GetRole must report the role that was asked for, got {read_role}"
    );

    // 5. The data plane: one request per caller, to the method the control-plane reads just
    //    described.
    let invoke_rows: Vec<InvokeRow> = vec![
        InvokeRow {
            case: "a session granted the invoke on every verb of the stage",
            signer: Some(signer(every_verb_role)),
            owed: Owed::Served,
        },
        InvokeRow {
            case: "a session granted the method's own concrete ARN",
            signer: Some(signer(this_method_role)),
            owed: Owed::Served,
        },
        InvokeRow {
            case: "a session granted another verb only",
            signer: Some(signer(other_verb_role)),
            owed: Owed::NotAuthorized,
        },
        InvokeRow {
            case: "a session whose role holds no document at all",
            signer: Some(signer(ungranted_role)),
            owed: Owed::NotAuthorized,
        },
        InvokeRow {
            case: "no credential at all",
            signer: None,
            owed: Owed::MissingToken,
        },
    ];
    let invoke_host = format!("{}.execute-api.{REGION}.amazonaws.com", estate.api);
    let request_path = format!("/{STAGE}/{COLLECTION}/{AGREEMENT}");
    for row in &invoke_rows {
        let case = row.case;
        let credential = row
            .signer
            .as_ref()
            .map(|(_, _, key_id)| authorization(key_id, "execute-api"));
        let headers: Vec<(&str, &str)> = credential
            .as_deref()
            .map(|value| vec![("authorization", value)])
            .unwrap_or_default();
        let answered = dispatch(
            &router,
            "GET",
            &invoke_host,
            &request_path,
            &headers,
            Bytes::new(),
        )
        .await;
        match row.owed {
            Owed::Served => {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: expected the integration's 200, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.json(),
                    served_body(),
                    "case {case:?}: expected the handler's own body, got {}",
                    answered.summary()
                );
            }
            Owed::NotAuthorized => {
                let (role, session, _) = row
                    .signer
                    .as_ref()
                    .expect("a refused row signs its request");
                assert_eq!(
                    answered.status,
                    403,
                    "case {case:?}: expected the gate's 403, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.message(),
                    denied_message(role, session, &estate.api),
                    "case {case:?}: expected the IAM authorization sentence for this method ARN, \
                     got {}",
                    answered.summary()
                );
            }
            Owed::MissingToken => assert_eq!(
                (answered.status, answered.message()),
                (403, MISSING_TOKEN.to_string()),
                "case {case:?}: expected 403 {MISSING_TOKEN:?}, got {}",
                answered.summary()
            ),
        }
    }

    // 6. The simulated invocation beside the signed ones: the envelope `TestInvokeMethod` models.
    let simulated = apigw(
        &router,
        "TestInvokeMethod",
        "POST",
        &format!(
            "/restapis/{}/resources/{}/methods/GET",
            estate.api, estate.child
        ),
        Some(json!({ "pathWithQueryString": format!("/{COLLECTION}/{AGREEMENT}") })),
        200,
    )
    .await;
    assert_eq!(
        simulated.get("status").and_then(Value::as_i64),
        Some(200),
        "TestInvokeMethod must report the status the simulated call was answered with, got \
         {simulated}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            simulated
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )
        .unwrap_or(Value::Null),
        served_body(),
        "TestInvokeMethod must report the body the integration answered, got {simulated}"
    );
    assert!(
        simulated
            .get("log")
            .and_then(Value::as_str)
            .is_some_and(|log| log.contains("Method completed with status: 200")),
        "TestInvokeMethod must report the execution log of the simulated call, got {simulated}"
    );
    assert!(
        simulated.get("latency").and_then(Value::as_i64).is_some(),
        "TestInvokeMethod must report the latency it measured, got {simulated}"
    );

    // 7. What the runs left in CloudWatch Logs, and the reap that takes the group away again.
    //    "By default, Lambda functions send logs to a default log group named
    //    `/aws/lambda/<function name>`" (`LoggingConfig$logGroup`).
    let group = format!("/aws/lambda/{FUNCTION}");
    let mut messages: Vec<String> = Vec::new();
    for _ in 0..LOOKS {
        let answered = logs(&router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
        if answered.status == 200 {
            messages = answered
                .json()
                .get("events")
                .and_then(Value::as_array)
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| event.get("message").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if messages.iter().any(|line| line.starts_with(REPORT_PREFIX)) {
                break;
            }
        }
        tokio::time::sleep(LOOK).await;
    }
    assert!(
        messages
            .iter()
            .any(|line| line.starts_with(&format!("{HANDLED_PREFIX}{AGREEMENT}"))),
        "the function's log group must hold the line the handler printed, got {messages:?}"
    );
    assert!(
        messages.iter().any(|line| line.starts_with(REPORT_PREFIX)),
        "the function's log group must hold the runtime's closing record, got {messages:?}"
    );

    let listed = logs(
        &router,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    assert_eq!(
        listed
            .json()
            .get("logGroups")
            .and_then(Value::as_array)
            .map(|groups| groups
                .iter()
                .filter_map(|held| held.get("logGroupName").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<String>>()),
        Some(vec![group.clone()]),
        "DescribeLogGroups must report the group the run wrote to, got {}",
        listed.summary()
    );

    let reaped = logs(&router, "DeleteLogGroup", &json!({ "logGroupName": group })).await;
    assert_eq!(
        reaped.status,
        200,
        "DeleteLogGroup must take the group away, got {}",
        reaped.summary()
    );
    let after = logs(
        &router,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    assert_eq!(
        after
            .json()
            .get("logGroups")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "a reaped group must be gone from the next read, got {}",
        after.summary()
    );
    for (case, answered) in [
        (
            "a second delete of the reaped group",
            logs(&router, "DeleteLogGroup", &json!({ "logGroupName": group })).await,
        ),
        (
            "a read of the reaped group",
            logs(&router, "FilterLogEvents", &json!({ "logGroupName": group })).await,
        ),
    ] {
        assert_eq!(
            answered.status,
            400,
            "case {case:?}: expected the 400 CloudWatch Logs refuses a group it does not hold \
             with, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.code(),
            RESOURCE_NOT_FOUND,
            "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {}",
            answered.summary()
        );
    }

    // 8. The teardown: every delete the lifecycle's last phases make answers terminally, and
    //    what it removed is gone from the next read of the service that held it.
    let dropped_stage = apigw_call(
        &router,
        "DELETE",
        &format!("/restapis/{}/stages/{STAGE}", estate.api),
        None,
    )
    .await;
    assert_eq!(
        dropped_stage.status,
        202,
        "DeleteStage must answer its modelled 202, got {}",
        dropped_stage.summary()
    );
    let read_stage = apigw_call(
        &router,
        "GET",
        &format!("/restapis/{}/stages/{STAGE}", estate.api),
        None,
    )
    .await;
    assert_eq!(
        read_stage.status,
        404,
        "a deleted stage must be gone from the next read, got {}",
        read_stage.summary()
    );
    let dropped_api = apigw_call(&router, "DELETE", &format!("/restapis/{}", estate.api), None).await;
    assert_eq!(
        dropped_api.status,
        202,
        "DeleteRestApi must answer its modelled 202, got {}",
        dropped_api.summary()
    );
    let read_api = apigw_call(&router, "GET", &format!("/restapis/{}", estate.api), None).await;
    assert_eq!(
        read_api.status,
        404,
        "a deleted api must be gone from the next read, got {}",
        read_api.summary()
    );

    accepted_query(
        &router,
        "cloudformation",
        "2010-05-15",
        "DeleteStack",
        &[("StackName", STACK)],
    )
    .await;
    let (gone, described) = wait_for_stack_gone(&router, STACK).await;
    assert!(
        gone,
        "the stack's deletion must finish, last DescribeStacks answered {described}"
    );
    let read_function = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        None,
    )
    .await;
    assert_eq!(
        read_function.status,
        404,
        "the function the deleted stack declared must be gone from Lambda, got {}",
        read_function.summary()
    );

    let dropped_table = dynamodb(&router, "DeleteTable", &json!({ "TableName": TABLE })).await;
    assert_eq!(
        dropped_table.status,
        200,
        "DeleteTable must be accepted, got {}",
        dropped_table.summary()
    );
    let mut described_table = dropped_table;
    for _ in 0..LOOKS {
        described_table = dynamodb(&router, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described_table.status != 200 {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        described_table.code(),
        RESOURCE_NOT_FOUND,
        "a deleted table must stop being addressable, got {}",
        described_table.summary()
    );

    for role in [
        EXECUTION_ROLE,
        every_verb_role,
        this_method_role,
        other_verb_role,
        ungranted_role,
    ] {
        let listed = iam(&router, "ListRolePolicies", &[("RoleName", role)]).await;
        for policy in elements(&listed, "member") {
            iam(
                &router,
                "DeleteRolePolicy",
                &[("RoleName", role), ("PolicyName", &policy)],
            )
            .await;
        }
        iam(&router, "DeleteRole", &[("RoleName", role)]).await;
        let read = query(
            &router,
            "iam",
            "2010-05-08",
            "GetRole",
            &[("RoleName", role)],
        )
        .await;
        assert_eq!(
            read.status,
            404,
            "a deleted role must be gone from the next read of {role:?}, got {}",
            read.summary()
        );
    }
}
