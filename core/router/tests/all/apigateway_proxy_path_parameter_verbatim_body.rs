// SPDX-License-Identifier: CC0-1.0

//! What a SIGNED caller of a deployed REST stage is served when the `AWS_IAM` method behind it
//! is an `AWS_PROXY` integration over a function that reads the item its PATH PARAMETER names.
//!
//! RULE: every request a deployed stage admits to an `AWS_PROXY` integration terminates in the
//! HTTP response the target function's proxy envelope describes, passed through unmodified —
//! its `statusCode` is the client's status, its `headers` are the client's headers, and its
//! `body` is the client's body byte for byte, whatever the handler composed that text from, so
//! the stage neither re-serializes it nor re-encodes a JSON document the handler embedded in it
//! as a string value; and the `{…}` segments of the method's resource path are handed to that
//! function as its event's `pathParameters`, so WHICH item the handler answers about is the one
//! the request addressed.
//!
//! "With Lambda proxy integration … API Gateway … uses the Lambda function's output as the HTTP
//! response body, status code and headers" (API Gateway Developer Guide, *Lambda proxy
//! integration*), which is a contract about the handler's own output: a status the handler chose
//! for a key its backend holds nothing under reaches the client as that status, carrying the
//! handler's own text.
//!
//! Six crates have to agree for that to hold — IAM holds the execution role and the caller's
//! `execute-api:Invoke` grant, STS mints the session the invoke is signed as, CloudFormation
//! archives the inline handler into a package this emulator can run, Lambda runs it, DynamoDB
//! serves the read, and API Gateway resolves the stage, authorizes the call, performs the proxy
//! invoke and maps the envelope back — so the suite lives here rather than in any one of them.
//! The sibling `apigateway_iam_authorized_invoke` suite pins the GATE's own decision and
//! `apigateway_proxy_stage_surfaces_its_handlers_envelope` pins WHICH status a handler's envelope
//! carries; what only this table reaches is the BODY's passthrough — a nested map, an embedded
//! JSON-schema string, a document carrying quotes and backslashes — and the path parameter that
//! decides which body is composed at all.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the item the handler
//! serializes (one carrying a nested map, a list, a number, a boolean and an embedded JSON
//! document; one carrying its key alone; one whose embedded document carries quotes and a
//! backslash), and WHETHER the path parameter names an item the table holds (three that do, one
//! that does not, answered the handler's own `404`).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region of the table, the function and
/// the api's own invoke host.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The table the handler reads, and the name of its string hash attribute.
const TABLE: &str = "xsvc-verbatim-agreements";
const KEY_ATTRIBUTE: &str = "id";

/// The function the stage's integration invokes, the role it runs as, and the stack that
/// archives its inline source into a deployment package.
const FUNCTION: &str = "xsvc-verbatim-agreement-reader";
const EXECUTION_ROLE: &str = "xsvc-verbatim-agreement-reader-role";
const STACK: &str = "xsvc-verbatim-agreement-stack";

/// The role the caller of the api assumes, and the session name STS records it under.
const CALLER_ROLE: &str = "xsvc-verbatim-agreement-caller";
const CALLER_SESSION: &str = "granted";

/// The stage the api is deployed to, the collection its one method hangs off, and the name of
/// the path parameter that names one agreement inside it.
const STAGE: &str = "live";
const COLLECTION: &str = "data-agreements";
const PARAMETER: &str = "agreementId";

/// How long one invocation of this handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The IAM actions the two roles are granted: the read the handler makes, and the invoke the
/// caller makes.
const READ_ACTION: &str = "dynamodb:GetItem";
const INVOKE_ACTION: &str = "execute-api:Invoke";

/// The status a stack has to reach before the function it declares can be invoked.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long to wait between looks at a transition another service owes, and how many looks to
/// take before giving up. A deadline, not a wait: every loop below stops at the CONDITION.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The media type the handler declares on the body it answers a found agreement with.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The sentence the HANDLER answers a key its table holds nothing under with. Its text is the
/// handler's own, which is exactly why the client has to be served it unchanged.
const NOT_FOUND_ERROR: &str = "Agreement not found";

/// The handler the stage's integration invokes: it reads the item its event's `pathParameters`
/// names and answers the proxy envelope the client is served — the item serialized as it stands,
/// or the handler's own `404`.
///
/// `TABLE_NAME` and `PARAMETER_NAME` are substituted before the stack is deployed.
const READER_HANDLER: &str = r##"
import json

import boto3


def handler(event, context):
    requested = event["pathParameters"]["PARAMETER_NAME"]
    read = boto3.client("dynamodb").get_item(
        TableName="TABLE_NAME",
        Key={"id": {"S": requested}},
        ConsistentRead=True,
    )
    item = read.get("Item")
    if item is None:
        return {
            "statusCode": 404,
            "body": json.dumps({"error": "NOT_FOUND_ERROR"}),
        }
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(item, sort_keys=True, separators=(",", ":")),
    }
"##;

/// The handler source as it is deployed: the table it reads and the path parameter it reads the
/// key out of named in it.
fn handler_source() -> String {
    READER_HANDLER
        .replace("TABLE_NAME", TABLE)
        .replace("PARAMETER_NAME", PARAMETER)
        .replace("NOT_FOUND_ERROR", NOT_FOUND_ERROR)
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

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the table it reads is the table this suite seeds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Verifying the signature itself belongs to the emulator, and its own suite pins it: here the
    // credential scope is what routes a request and its access key id is what the invoke's
    // authorization resolves its principal from.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which resolves to no session and so speaks for the
/// account's own identity — what the SETUP calls are made as.
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

/// One awsQuery call of `service`, made as the account's own credential, which has to be
/// accepted; answers its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
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
    .await;
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
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust_policy)],
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

/// Create the table the handler reads and wait until DynamoDB reports it `ACTIVE` — "you can
/// perform read and write operations only on an ACTIVE table".
async fn create_table(router: &MultiServiceRouter) {
    let created = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateTable({TABLE:?}) must succeed, got {}",
        created.summary()
    );
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
}

/// Write one item, as it is given here — the attribute values the handler will serialize.
async fn put_item(router: &MultiServiceRouter, item: &Value) {
    let written = dynamodb(
        router,
        "PutItem",
        &json!({"TableName": TABLE, "Item": item.clone()}),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "setup: PutItem({item}) must succeed, got {}",
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

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its own
    // service owes: look until it reports it rather than sleeping a span on its behalf.
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

/// A JSON-schema document, as a caller stores one in a string attribute: the handler embeds this
/// TEXT in the body it answers with, so what reaches the client has to be this text escaped
/// once — never a re-encoding of it, and never the object it describes.
const SCHEMA_DOCUMENT: &str = r#"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"amount":{"type":"number"}},"required":["amount"],"type":"object"}"#;

/// A second embedded document, carrying the two characters a JSON string escapes — a quote and a
/// backslash — so a body that was re-encoded on the way out cannot look like one that was not.
const QUOTED_DOCUMENT: &str = r#"{"pattern":"^a\\d+$","title":"the \"quoted\" title"}"#;

/// One row: the agreement the request addresses through the path parameter, the item the table
/// holds under it (`None` for a key the table holds nothing under), and the document embedded in
/// that item, which the served body has to carry as a string.
struct Row {
    case: &'static str,
    agreement: &'static str,
    item: Option<Value>,
    embedded: Option<&'static str>,
}

/// The rows, in the order they are driven. The item of each is written with its attribute names
/// in ascending order, which is the order the handler's own `sort_keys` serialization emits, so
/// the expected body text is this value serialized.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an agreement carrying a nested map, a list, a number, a boolean and an \
                   embedded JSON-schema string",
            agreement: "agreement-7f3",
            item: Some(json!({
                "active": {"BOOL": true},
                "id": {"S": "agreement-7f3"},
                "metadata": {"M": {
                    "classification": {"S": "restricted"},
                    "owner": {"M": {
                        "email": {"S": "data-steward@example.com"},
                        "team": {"S": "ledger"},
                    }},
                    "retentionDays": {"N": "365"},
                }},
                "parties": {"L": [{"S": "ledger"}, {"S": "billing"}]},
                "revision": {"N": "3"},
                "schema": {"S": SCHEMA_DOCUMENT},
            })),
            embedded: Some(SCHEMA_DOCUMENT),
        },
        Row {
            case: "an agreement carrying its key alone",
            agreement: "agreement-bare",
            item: Some(json!({"id": {"S": "agreement-bare"}})),
            embedded: None,
        },
        Row {
            case: "an agreement whose embedded document carries a quote and a backslash",
            agreement: "agreement-quoted",
            item: Some(json!({
                "id": {"S": "agreement-quoted"},
                "schema": {"S": QUOTED_DOCUMENT},
            })),
            embedded: Some(QUOTED_DOCUMENT),
        },
        Row {
            case: "an agreement id the table holds no item under",
            agreement: "agreement-never-seeded",
            item: None,
            embedded: None,
        },
    ]
}

/// RULE: the stage answers every admitted request with the handler's own envelope — its status,
/// its headers, and its body byte for byte — about the item the request's path parameter named.
#[tokio::test]
async fn a_proxy_stage_serves_the_handlers_body_verbatim_for_the_key_its_path_parameter_named() {
    let router = emulator();
    let rows = rows();

    // 1. The table, holding the seeded agreements.
    create_table(&router).await;
    for row in &rows {
        if let Some(item) = &row.item {
            put_item(&router, item).await;
        }
    }

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

    // 3. The api: `GET /{COLLECTION}/{{PARAMETER}}` behind `AWS_IAM`, wired to an `AWS_PROXY`
    //    integration on that function, deployed to one stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "verbatim-proxy-api",
            "endpointConfiguration": {"types": ["REGIONAL"]},
        })),
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
        Some(json!({"pathPart": COLLECTION})),
        201,
    )
    .await;
    let collection_id = as_str("CreateResource(collection)", &collection, "id");
    let item_resource = apigw(
        &router,
        "CreateResource(item)",
        "POST",
        &format!("/restapis/{api}/resources/{collection_id}"),
        Some(json!({"pathPart": format!("{{{PARAMETER}}}")})),
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
        Some(json!({"authorizationType": "AWS_IAM"})),
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

    // The grant that admits an invocation from this stage's own `GET` methods, scoped the way the
    // Developer Guide scopes one: the source ARN of the method the stage invokes from.
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{STAGE}/GET/{COLLECTION}/*"
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission must answer 201, got {}",
        granted.summary()
    );
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({"stageName": STAGE})),
        201,
    )
    .await;

    // 4. The caller: a session of a role granted `execute-api:Invoke` on this stage's `GET`
    //    methods. The ACCOUNT field is written as the wildcard IAM admits, so the document names
    //    this method without this suite restating the account contract.
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
    let caller_key = assume_role(&router, &caller_role, CALLER_SESSION).await;

    // 5. The rows: one signed GET of the stage's invoke URL each.
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    for row in &rows {
        let case = row.case;
        let answered = dispatch(
            &router,
            "GET",
            &invoke_host,
            &format!("/{STAGE}/{COLLECTION}/{}", row.agreement),
            &[(
                "authorization",
                &authorization(&caller_key, "execute-api"),
            )],
            Bytes::new(),
        )
        .await;

        let Some(item) = &row.item else {
            // The key names no item: the client is served the HANDLER's own answer about it —
            // its status and its own sentence, neither of them the stage's.
            assert_eq!(
                answered.status,
                404,
                "case {case:?}: expected the handler's own 404 for a key its table holds \
                 nothing under, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.json(),
                json!({"error": NOT_FOUND_ERROR}),
                "case {case:?}: expected the handler's own body {:?}, got {}",
                json!({"error": NOT_FOUND_ERROR}).to_string(),
                answered.summary()
            );
            continue;
        };

        // The body the handler composed: the item as it stands, serialized with its attribute
        // names in ascending order, which is what the client has to be served byte for byte.
        let expected_text =
            serde_json::to_string(item).expect("the seeded item serializes as JSON text");
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the handler's own 200, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.header("content-type").as_deref(),
            Some(JSON_MEDIA_TYPE),
            "case {case:?}: expected the handler's own Content-Type {JSON_MEDIA_TYPE:?}, got \
             {:?} with {}",
            answered.header("content-type"),
            answered.summary()
        );
        assert_eq!(
            answered.text(),
            expected_text,
            "case {case:?}: expected the handler's body byte for byte, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.json(),
            *item,
            "case {case:?}: expected the served body to be the seeded item attribute for \
             attribute, got {}",
            answered.summary()
        );

        if let Some(embedded) = row.embedded {
            // The embedded document is a STRING attribute: it reaches the client as that string,
            // escaped exactly once, and the text inside it is still the document that was stored.
            assert_eq!(
                answered.json()["schema"]["S"].as_str(),
                Some(embedded),
                "case {case:?}: expected the embedded document to survive as its own string, \
                 got {}",
                answered.summary()
            );
            let escaped_once = serde_json::to_string(&Value::String(embedded.to_string()))
                .expect("a string serializes as JSON text");
            assert!(
                answered.text().contains(&escaped_once),
                "case {case:?}: expected the served body to carry {escaped_once:?} escaped \
                 exactly once, got {}",
                answered.summary()
            );
            let reparsed: Value = serde_json::from_str(embedded).unwrap_or(Value::Null);
            assert!(
                reparsed.is_object(),
                "case {case:?}: expected the served document to still parse as the JSON object \
                 it was stored as, got {reparsed}"
            );
        }
    }
}
