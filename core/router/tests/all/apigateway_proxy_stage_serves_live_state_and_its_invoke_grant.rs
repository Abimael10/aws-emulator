// SPDX-License-Identifier: CC0-1.0

//! What a SigV4-signed request to an `AWS_IAM` method of a deployed REST stage behind an
//! `AWS_PROXY` integration is served, as the backend's state and the function's own invoke
//! grant stand AT THAT REQUEST.
//!
//! RULE: every request that reaches a deployed stage terminates in an HTTP response computed
//! from the records as they stand when it arrives — the deployment resolves the resource and
//! the method, the gate authorizes the call, and an `AWS_PROXY` integration is PERFORMED by
//! invoking the function the `uri` names, so the client is served that run's own envelope
//! (`statusCode`, `headers`, `body`, unmapped) over whatever the handler's own backend reads
//! answered then, and NOT a snapshot of what an earlier request saw. The grant that admits
//! the invocation is the function's resource-based policy, read the same way at every request:
//! a method whose function admits API Gateway nothing is answered that api's own
//! configuration-error response — `500`, `{"message":"Internal server error"}`,
//! `x-amzn-ErrorType: InternalServerErrorException` — and the integration is not performed.
//!
//! "With Lambda proxy integration ... API Gateway ... uses the Lambda function's output as the
//! HTTP response body, status code and headers" (API Gateway Developer Guide, *Lambda proxy
//! integration*), and API Gateway invokes the function as its own service principal, so the
//! grant is the one `AddPermission` writes and `RemovePermission` takes away
//! (`AddPermission$Principal`, `AddPermission$SourceArn`).
//!
//! Six crates have to agree for that to hold — IAM holds the roles, STS mints the caller's
//! session, CloudFormation archives the inline handler into a package this emulator can run,
//! Lambda runs it and holds the invoke grant, DynamoDB serves the reads the handler makes, and
//! API Gateway resolves the stage and maps the envelope — so the suite lives here rather than
//! in any one of them. The sibling `apigateway_iam_authorized_invoke` suite pins the GATE's own
//! decision and `apigateway_proxy_stage_surfaces_its_handlers_envelope` pins the mapping of a
//! status the handler chose; what only this suite reaches is the LIVENESS of both halves — the
//! table state a later request reads, and the grant a later request is judged by.
//!
//! The rows differ along what the rule generalises over: whether the addressed id is one the
//! table holds at all, what the item it holds LOOKS like (a nested map and a JSON-schema string,
//! then the same item rewritten with a new numeric attribute value and a list added), and the
//! state of the function's invoke grant (in force, withdrawn, written back). Every row asserts
//! the status, the response media type, the handler's own probe header and the body — so a
//! stage that answers nothing, answers a stale generation of the item, or keeps serving a
//! function it may no longer invoke fails here.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region of the table, the function
/// and the api's own invoke host.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the grant's `SourceArn` and the table's ARN carry.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The table the handler reads, and the two ids the rows address.
const TABLE: &str = "xsvc-live-agreements";
const SEEDED_ID: &str = "agreement-7f31";
const UNSEEDED_ID: &str = "agreement-none";

/// The JSON-schema document the seeded agreement carries as a STRING attribute: text whose own
/// quotes and `#` have to survive the handler's serialization and the response body intact.
const SCHEMA_TEXT: &str = "{\"$schema\":\"http://json-schema.org/draft-07/schema#\",\
                           \"type\":\"object\",\"required\":[\"party\",\"effectiveFrom\"]}";

/// The function the stage's integration invokes, and the role it runs as.
const FUNCTION: &str = "xsvc-live-agreement-reader";
const EXECUTION_ROLE: &str = "xsvc-live-agreement-reader-role";

/// The role a caller of the api assumes, which is granted the invoke of this stage's methods.
const CALLER_ROLE: &str = "xsvc-live-agreement-caller";

/// The stack that archives the inline handler into a deployment package, and the status it has
/// to reach.
const STACK: &str = "xsvc-live-agreement-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to, and the two path parts of its one resource:
/// `/data-agreements/{agreementId}`.
const STAGE: &str = "prod";
const COLLECTION_PATH: &str = "data-agreements";
const ID_PARAMETER: &str = "agreementId";

/// The IAM actions involved: the one an invoke of a deployed method asks for, the one the
/// handler's read performs, and the one API Gateway's invocation of the function asks for.
const INVOKE_ACTION: &str = "execute-api:Invoke";
const READ_ACTION: &str = "dynamodb:GetItem";
const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// The service principal API Gateway invokes a function as, which the grant names.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The id of the one statement the rows withdraw and write back.
const STATEMENT_ID: &str = "apigw-invoke-prod";

/// The response header the handler's envelope carries, which is how a row observes that the
/// integration was performed at all and that the headers served are the handler's own.
const PROBE_HEADER: &str = "x-probe";

/// The media type the handler declares for every envelope it returns.
const JSON_MEDIA_TYPE: &str = "application/json";

/// What API Gateway answers a method whose integration it could not perform with, and the
/// exception class that answer names in the header an SDK reads an error code out of.
const CONFIGURATION_ERROR_STATUS: u16 = 500;
const ERROR_TYPE_HEADER: &str = "x-amzn-ErrorType";
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the serving and not how fast an interpreter
/// gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created table reaching `ACTIVE`,
/// a created function reaching `Active` — and how many looks to take. The spans are those
/// services' own, so no fixed sleep names the instant one ends, and the deadline is generous so
/// a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler the stage's integration invokes: it reads the agreement the path named with the
/// SDK the runtime supplies and answers a proxy envelope carrying DynamoDB's own
/// attribute-value map for it, verbatim, or its own `404` where the table holds none.
///
/// `TABLE_NAME` and `ID_PARAM` are substituted before the stack is deployed.
const READER_HANDLER: &str = r##"
import json

import boto3


def handler(event, context):
    asked = (event.get("pathParameters") or {}).get("ID_PARAM") or ""
    read = boto3.client("dynamodb").get_item(
        TableName="TABLE_NAME",
        Key={"id": {"S": asked}},
        ConsistentRead=True,
    )
    item = read.get("Item")
    if item is None:
        return {
            "statusCode": 404,
            "headers": {"Content-Type": "application/json", "x-probe": "absent"},
            "body": json.dumps({"error": "no agreement under that id", "id": asked}),
        }
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json", "x-probe": "served"},
        "body": json.dumps(item),
    }
"##;

/// The handler source as it is deployed: the table it reads and the path parameter it reads the
/// id out of named in it.
fn handler_source() -> String {
    READER_HANDLER
        .replace("TABLE_NAME", TABLE)
        .replace("ID_PARAM", ID_PARAMETER)
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

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the table it reads is the table this suite writes.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate: the
    // credential scope is what routes a request, and its access key id is what the invoke's
    // authorization resolves its principal from.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which resolves to no session and so speaks for
/// the account's own identity.
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
        "setup: {service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
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

/// One Amazon DynamoDB call that has to be accepted.
async fn accepted_dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) {
    let answered = dynamodb(router, operation, body).await;
    assert_eq!(
        answered.status,
        200,
        "setup: DynamoDB {operation} must answer 200, got {}",
        answered.summary()
    );
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

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
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
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what:?}: must answer HTTP {expected}, got {}",
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
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// The ARN of the table the handler reads, which its execution role's grant names.
fn table_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}")
}

/// A trust policy naming one AWS service as the principal that may assume the role.
fn service_trust_policy(service_principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": service_principal},
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
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// One identity-based document stating `effect` about `action` on `resource`.
fn policy_document(effect: &str, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": action, "Resource": resource}],
    })
    .to_string()
}

/// The document every execution role needs: the log actions a run writes with.
fn logging_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create a role with `trust_policy` and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, trust_policy: &str) -> String {
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", trust_policy),
        ],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer the role's ARN"))
}

/// Embed one inline document in a role.
async fn put_role_policy(router: &MultiServiceRouter, role: &str, policy: &str, document: &str) {
    accepted_query(
        router,
        "iam",
        "2010-05-08",
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
        .unwrap_or_else(|| panic!("setup: AssumeRole({role_arn:?}) must answer an access key id"))
}

/// The agreement this suite seeds, as DynamoDB's own attribute-value map: a nested map and a
/// JSON-schema STRING among its attributes, which is what the handler serialises verbatim.
fn seeded_item() -> Value {
    json!({
        "id": {"S": SEEDED_ID},
        "version": {"N": "1"},
        "schema": {"S": SCHEMA_TEXT},
        "parties": {"M": {
            "owner": {"S": "team-alpha"},
            "consumer": {"S": "team-beta"},
        }},
    })
}

/// The same agreement after the rewrite: a new value under the numeric attribute, and a list
/// attribute the item did not carry before.
fn rewritten_item() -> Value {
    json!({
        "id": {"S": SEEDED_ID},
        "version": {"N": "2"},
        "schema": {"S": SCHEMA_TEXT},
        "parties": {"M": {
            "owner": {"S": "team-alpha"},
            "consumer": {"S": "team-beta"},
        }},
        "scopes": {"L": [{"S": "read"}, {"S": "write"}]},
    })
}

/// Create the table the handler reads and wait until DynamoDB reports it `ACTIVE` — "you can
/// perform read and write operations only on an ACTIVE table".
async fn create_table(router: &MultiServiceRouter) {
    accepted_dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = dynamodb(router, "DescribeTable", &json!({"TableName": TABLE}))
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
        "setup: DynamoDB must report {TABLE:?} ACTIVE, got {described}"
    );
    assert_eq!(
        described["Table"]["TableArn"].as_str(),
        Some(table_arn().as_str()),
        "setup: the table's ARN is what the execution role's grant names, got {described}"
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
                    "Code": {"ZipFile": handler_source()},
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
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
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
        "setup: Lambda must report {FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Write the statement that admits an invocation API Gateway makes for this stage's `GET`
/// methods, through `AddPermission` — whose answer is "201".
async fn add_invoke_permission(router: &MultiServiceRouter, what: &str, api: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": STATEMENT_ID,
            "Action": INVOKE_FUNCTION_ACTION,
            "Principal": APIGATEWAY_PRINCIPAL,
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{STAGE}/GET/{COLLECTION_PATH}/*"
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup {what:?}: AddPermission({STATEMENT_ID:?}) must answer 201, got {}",
        granted.summary()
    );
}

/// Take that statement back out again, through `RemovePermission` — whose answer is "204".
async fn remove_invoke_permission(router: &MultiServiceRouter, what: &str) {
    let removed = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{FUNCTION}/policy/{STATEMENT_ID}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "setup {what:?}: RemovePermission({STATEMENT_ID:?}) must answer 204, got {}",
        removed.summary()
    );
}

/// What this suite does to the records the next row's request is served from, BEFORE making it.
#[derive(Debug, Clone, Copy)]
enum Before {
    /// Nothing: the seeded agreement stands and the function's invoke grant is in force.
    Nothing,
    /// `UpdateItem` rewrites the agreement's version and adds a list attribute to it.
    RewriteItem,
    /// `RemovePermission` drops the function's `apigateway.amazonaws.com` statement.
    WithdrawInvokeGrant,
    /// `AddPermission` writes that statement back.
    RestoreInvokeGrant,
}

/// WHICH generation of the agreement the handler's envelope carries.
#[derive(Debug, Clone, Copy)]
enum Served {
    /// The item as it was seeded.
    Seeded,
    /// The item as the rewrite left it.
    Rewritten,
    /// No item at all: the handler's own `404` body for the id addressed.
    Absent,
}

/// What the client is owed.
#[derive(Debug, Clone, Copy)]
enum Owed {
    /// The handler's own envelope, mapped: this status, this `x-probe` header, and the body it
    /// serialised for that generation of the item.
    Envelope {
        status: u16,
        probe: &'static str,
        served: Served,
    },
    /// API Gateway may not invoke the function, so the integration is not performed and the
    /// api's own configuration-error response is what answers the request.
    ConfigurationError,
}

/// One request to the deployed stage.
struct Row {
    case: &'static str,
    before: Before,
    /// The id in the request's path, which is what the handler reads the agreement under.
    addressed: &'static str,
    owed: Owed,
}

const ROWS: &[Row] = &[
    Row {
        case: "the seeded agreement is served verbatim through the granted proxy",
        before: Before::Nothing,
        addressed: SEEDED_ID,
        owed: Owed::Envelope {
            status: 200,
            probe: "served",
            served: Served::Seeded,
        },
    },
    Row {
        case: "an id the table holds no agreement under earns the handler's own 404",
        before: Before::Nothing,
        addressed: UNSEEDED_ID,
        owed: Owed::Envelope {
            status: 404,
            probe: "absent",
            served: Served::Absent,
        },
    },
    Row {
        case: "the agreement rewritten in the table is served on the next signed request",
        before: Before::RewriteItem,
        addressed: SEEDED_ID,
        owed: Owed::Envelope {
            status: 200,
            probe: "served",
            served: Served::Rewritten,
        },
    },
    Row {
        case: "the function's invoke grant is withdrawn, so the stage answers its own 500",
        before: Before::WithdrawInvokeGrant,
        addressed: SEEDED_ID,
        owed: Owed::ConfigurationError,
    },
    Row {
        case: "the grant is written back and the identical request is served again",
        before: Before::RestoreInvokeGrant,
        addressed: SEEDED_ID,
        owed: Owed::Envelope {
            status: 200,
            probe: "served",
            served: Served::Rewritten,
        },
    },
];

/// The body the handler's envelope carries for each generation of the agreement, which is the
/// body the client is served.
fn expected_body(served: Served, addressed: &str) -> Value {
    match served {
        Served::Seeded => seeded_item(),
        Served::Rewritten => rewritten_item(),
        Served::Absent => json!({"error": "no agreement under that id", "id": addressed}),
    }
}

#[tokio::test]
async fn a_deployed_proxy_stage_serves_live_table_state_and_the_invoke_grant_as_it_stands() {
    let router = emulator();

    // 1. The table the handler reads, holding the one seeded agreement.
    create_table(&router).await;
    accepted_dynamodb(
        &router,
        "PutItem",
        &json!({"TableName": TABLE, "Item": seeded_item()}),
    )
    .await;

    // 2. The function, running as a role that may read exactly that table.
    let execution_role = create_role(
        &router,
        EXECUTION_ROLE,
        &service_trust_policy("lambda.amazonaws.com"),
    )
    .await;
    put_role_policy(&router, EXECUTION_ROLE, "logging", &logging_document()).await;
    put_role_policy(
        &router,
        EXECUTION_ROLE,
        "table-read",
        &policy_document("Allow", READ_ACTION, &table_arn()),
    )
    .await;
    let function_arn = deploy_function(&router, &execution_role).await;

    // 3. The api: one AWS_IAM method on /data-agreements/{agreementId} behind an AWS_PROXY
    //    integration on that function, deployed to its stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({"name": "live-agreements-api"})),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &api_record, "id");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let collection = apigw(
        &router,
        "CreateResource(collection)",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({"pathPart": COLLECTION_PATH})),
        201,
    )
    .await;
    let collection_id = as_str("CreateResource(collection)", &collection, "id");
    let item_resource = apigw(
        &router,
        "CreateResource(item)",
        "POST",
        &format!("/restapis/{api}/resources/{collection_id}"),
        Some(json!({"pathPart": format!("{{{ID_PARAMETER}}}")})),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource(item)", &item_resource, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({
            "authorizationType": "AWS_IAM",
            "requestParameters": {format!("method.request.path.{ID_PARAMETER}"): true},
        })),
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
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({"stageName": STAGE})),
        201,
    )
    .await;

    // The grant that admits an invocation API Gateway makes for this stage's GET methods.
    add_invoke_permission(&router, "the initial grant", &api).await;

    // 4. The caller: a session of a role granted the invoke of this stage's GET methods. The
    //    ACCOUNT field of the method ARN is written as the wildcard IAM admits, so the document
    //    names this method without this suite restating the account contract.
    let caller_role = create_role(&router, CALLER_ROLE, &account_trust_policy()).await;
    put_role_policy(
        &router,
        CALLER_ROLE,
        "invoke",
        &policy_document(
            "Allow",
            INVOKE_ACTION,
            &format!("arn:aws:execute-api:{REGION}:*:{api}/{STAGE}/GET/*"),
        ),
    )
    .await;
    let caller_key = assume_role(&router, &caller_role, "granted-caller").await;

    // 5. The rows, in order: each one's own signed request to the deployed stage's invoke URL.
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    for row in ROWS {
        let case = row.case;
        match row.before {
            Before::Nothing => {}
            Before::RewriteItem => {
                accepted_dynamodb(
                    &router,
                    "UpdateItem",
                    &json!({
                        "TableName": TABLE,
                        "Key": {"id": {"S": SEEDED_ID}},
                        "UpdateExpression": "SET #version = :version, scopes = :scopes",
                        "ExpressionAttributeNames": {"#version": "version"},
                        "ExpressionAttributeValues": {
                            ":version": {"N": "2"},
                            ":scopes": {"L": [{"S": "read"}, {"S": "write"}]},
                        },
                    }),
                )
                .await;
            }
            Before::WithdrawInvokeGrant => remove_invoke_permission(&router, case).await,
            Before::RestoreInvokeGrant => add_invoke_permission(&router, case, &api).await,
        }

        let answered = dispatch(
            &router,
            "GET",
            &invoke_host,
            &format!("/{STAGE}/{COLLECTION_PATH}/{}", row.addressed),
            &[("authorization", &authorization(&caller_key, "execute-api"))],
            Bytes::new(),
        )
        .await;

        match row.owed {
            Owed::Envelope {
                status,
                probe,
                served,
            } => {
                assert_eq!(
                    answered.status,
                    status,
                    "case {case:?}: expected the handler's own statusCode {status}, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.header("content-type").as_deref(),
                    Some(JSON_MEDIA_TYPE),
                    "case {case:?}: expected the handler's {JSON_MEDIA_TYPE:?} media type, got \
                     {:?} with {}",
                    answered.header("content-type"),
                    answered.summary()
                );
                assert_eq!(
                    answered.header(PROBE_HEADER).as_deref(),
                    Some(probe),
                    "case {case:?}: expected the handler's {PROBE_HEADER:?} header {probe:?}, \
                     got {:?} with {}",
                    answered.header(PROBE_HEADER),
                    answered.summary()
                );
                assert_eq!(
                    answered.json(),
                    expected_body(served, row.addressed),
                    "case {case:?}: expected the handler's own body for the {served:?} \
                     agreement, got {}",
                    answered.summary()
                );
            }
            Owed::ConfigurationError => {
                assert_eq!(
                    answered.status,
                    CONFIGURATION_ERROR_STATUS,
                    "case {case:?}: expected the api's configuration-error \
                     {CONFIGURATION_ERROR_STATUS}, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.header(ERROR_TYPE_HEADER).as_deref(),
                    Some(INTERNAL_SERVER_ERROR_EXCEPTION),
                    "case {case:?}: expected {ERROR_TYPE_HEADER}: \
                     {INTERNAL_SERVER_ERROR_EXCEPTION}, got {:?} with {}",
                    answered.header(ERROR_TYPE_HEADER),
                    answered.summary()
                );
                assert_eq!(
                    answered.json(),
                    json!({"message": "Internal server error"}),
                    "case {case:?}: expected the configuration-error body, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.header(PROBE_HEADER),
                    None,
                    "case {case:?}: a function API Gateway may not invoke must not run, got {}",
                    answered.summary()
                );
            }
        }
    }
}
