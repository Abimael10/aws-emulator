// SPDX-License-Identifier: CC0-1.0

//! The `TOKEN` Lambda-authorizer gate of a deployed REST stage as a CLIENT OF THE RUNNING
//! EMULATOR reaches it: over a real TLS connection to the started binary, with the
//! request-signature verification that process does enforce switched on, and with the identity
//! source carrying a bearer token that is no SigV4 credential at all.
//!
//! RULE: **an invoke of a deployed REST stage is served from the api's own records whatever the
//! request's `Authorization` header carries — the header is the value the method's `Authorizer`
//! declares as its `identitySource`, not a credential of this emulator — so the gate answers
//! `200` with the integration's own body for a policy that allows `execute-api:Invoke` on the
//! invoked method, and a terminal `403` with API Gateway's sentence for one that denies it,
//! with the integration never dispatched and nothing written to the store the backend writes.**
//!
//! Why a suite that starts the binary: every other suite over this plane dispatches in process
//! with `set_request_authentication(false)`, because the control-plane calls that BUILD the api
//! are not signed there. That leaves one thing unasserted — that the emulator's own request
//! authentication, which the started process enforces, lets an invoke carrying a non-SigV4
//! `Authorization` header through to the stage. An invoke of a deployed stage is signed under the
//! `execute-api` signing name rather than this service's, and a bearer token is not signed at
//! all, so a front door that judged this header would refuse the one client the endpoint exists
//! for — and against the running emulator that is the difference between a client getting
//! `200`/`403` and getting nothing it asked for. The control-plane half of the setup IS signed
//! here, with the key pair the start published, exactly as an SDK signs it.
//!
//! The rows differ along what the rule generalises over: WHAT the authorizer's function returned
//! for the presented token (an `Allow` naming the invoked method, an explicit `Deny` of it) and
//! HOW the client addresses the stage (the `/_aws/execute-api/{api-id}/{stage}/{path}` address a
//! client that cannot resolve AWS's own hostnames is handed, and the api's own
//! `{api-id}.execute-api.{region}.amazonaws.com` host). The denied rows run after an admitted one
//! has already written, so an absence that only holds because the table is empty fails here.
//!
//! [`crate::apigateway_token_authorizer_gate_over_narrowed_invoke_grants`] pins the same gate's
//! full answer table in process, under the invocation grants a client narrows to one authorizer
//! and one method; this suite re-drives only the two decisive rows, on the started process.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::apigateway_token_authorizer_gate_over_narrowed_invoke_grants::{
    AUTHORIZER_HANDLER, BACKEND_HANDLER, EXPLICIT_DENY, HASH_KEY, PATH_MEMBER, RESOURCE_PATH,
    STAGE, TOKEN_HEADER, VERB,
};
use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};
use crate::query_wire::opt_element;

/// The account this emulator serves, which every grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack this suite deploys, and the status it waits for.
const STACK: &str = "started-authorizer-gate";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The names the template gives the three resources, so nothing has to be read back out of the
/// stack to address them.
const BACKEND_FUNCTION: &str = "started-authorizer-gate-backend";
const AUTHORIZER_FUNCTION: &str = "started-authorizer-gate-authorizer";
const TABLE: &str = "started-authorizer-gate-table";

/// The tokens the rows present, which the authorizer's function chooses its policy from.
const ALLOW_TOKEN: &str = "let-me-in";
const DENY_TOKEN: &str = "keep-me-out";

/// The member an authorization refusal states its sentence under, and the class it names.
const MESSAGE_MEMBER: &str = "Message";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// How long one invocation of either function is given, as its own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work the emulator owes — a stack settling, a function reaching `Active`,
/// a table reaching `ACTIVE` — and how long between two looks. Generous on purpose, so a loaded
/// host never decides the outcome, and well inside the time this suite is allowed.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The template the suite deploys: both handlers, archived into deployment packages by
/// CloudFormation, and the table the backend writes — each named, so the suite addresses them
/// without reading the stack's resources back.
fn template() -> String {
    let function = |name: &str, source: &str, environment: Value, policies: Value| {
        json!({
            "Type": "AWS::Serverless::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Timeout": FUNCTION_TIMEOUT,
                "InlineCode": source,
                "Environment": environment,
                "Policies": policies,
            },
        })
    };
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Backend": function(
                BACKEND_FUNCTION,
                BACKEND_HANDLER,
                json!({ "Variables": { "TABLE": TABLE } }),
                json!([{ "DynamoDBCrudPolicy": { "TableName": TABLE } }]),
            ),
            "Gate": function(AUTHORIZER_FUNCTION, AUTHORIZER_HANDLER, json!({}), json!([])),
            "Written": {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": {
                    "TableName": TABLE,
                    "PrimaryKey": { "Name": HASH_KEY, "Type": "String" },
                },
            },
        },
    })
    .to_string()
}

/// One signed API Gateway control-plane call that has to answer `expected`, and its body parsed.
fn apigw(
    emulator: &Started,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, text) = emulator.call(
        "apigateway",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    assert_eq!(
        status, expected,
        "setup {what:?}: must answer HTTP {expected}, got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// One signed Amazon DynamoDB call over its `awsJson1_0` wire, parsed.
fn dynamodb(emulator: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "dynamodb",
        &format!("DynamoDB_20120810.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    assert_eq!(
        status, 200,
        "DynamoDB {operation} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Create the stack and wait until it reports `CREATE_COMPLETE`.
fn deploy(emulator: &Started) {
    let body = template();
    let (status, text) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and the
            // role the transform synthesizes for each function with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    );
    assert_eq!(
        status, 200,
        "setup: CreateStack must be accepted, got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    let deadline = Instant::now() + BUDGET;
    let mut reported = String::new();
    let mut described = String::new();
    while Instant::now() < deadline {
        let (status, body) = emulator.cloudformation("DescribeStacks", &[("StackName", STACK)]);
        assert_eq!(
            status, 200,
            "setup: DescribeStacks must answer 200, got {status} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if reported == CREATE_COMPLETE
            || reported.ends_with("_FAILED")
            || reported.contains("ROLLBACK")
        {
            break;
        }
        std::thread::sleep(LOOK_AGAIN);
    }
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reported:?} with {described}; the \
         log:\n{}",
        emulator.log()
    );
}

/// The ARN Lambda reports for `name`, once it reports it `Active`.
fn active_function_arn(emulator: &Started, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let (status, text) = emulator.call(
            "lambda",
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
            Vec::new(),
        );
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({name:?}) must answer 200, got {status} with {text}"
        );
        let reported: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if reported.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break reported;
        }
        std::thread::sleep(LOOK_AGAIN);
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {name:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Look until DynamoDB first reports the table `ACTIVE`.
fn wait_until_table_active(emulator: &Started) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb(emulator, "DescribeTable", &json!({ "TableName": TABLE }));
        let status = described
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str);
        if status == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: DynamoDB must report {TABLE:?} ACTIVE, got {described}"
        );
        std::thread::sleep(LOOK_AGAIN);
    }
}

/// Grant `apigateway.amazonaws.com` the invocation of `name`, narrowed to `source_arn`.
fn grant(emulator: &Started, name: &str, statement: &str, source_arn: &str) {
    let (status, text) = emulator.call(
        "lambda",
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        &[("content-type", "application/json")],
        json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": source_arn,
        })
        .to_string()
        .into_bytes(),
    );
    assert_eq!(
        status, 201,
        "setup: AddPermission({name:?}, {source_arn:?}) must answer 201, got {status} with {text}"
    );
}

/// The item the table holds under `key`, read through DynamoDB's own strongly consistent
/// `GetItem`, or `None` when it holds none.
fn item_under(emulator: &Started, key: &str) -> Option<Value> {
    dynamodb(
        emulator,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { HASH_KEY: { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .get("Item")
    .cloned()
}

/// One row: the token presented, how the stage is addressed, and what it owes.
struct Row {
    case: &'static str,
    token: &'static str,
    /// Whether the client addresses the api's own `execute-api` host rather than the address this
    /// emulator publishes for a client that cannot resolve it.
    own_host: bool,
    status: u16,
    /// The sentence a refused answer states, or `None` for the row the integration serves.
    message: Option<&'static str>,
    integration_runs: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an Allow for the invoked method, on the address this emulator publishes",
        token: ALLOW_TOKEN,
        own_host: false,
        status: 200,
        message: None,
        integration_runs: true,
    },
    Row {
        case: "an explicit Deny of the invoked method, once the table already holds an item",
        token: DENY_TOKEN,
        own_host: false,
        status: 403,
        message: Some(EXPLICIT_DENY),
        integration_runs: false,
    },
    Row {
        case: "an Allow for the invoked method, on the api's own execute-api host",
        token: ALLOW_TOKEN,
        own_host: true,
        status: 200,
        message: None,
        integration_runs: true,
    },
    Row {
        case: "an explicit Deny of the invoked method, on the api's own execute-api host",
        token: DENY_TOKEN,
        own_host: true,
        status: 403,
        message: Some(EXPLICIT_DENY),
        integration_runs: false,
    },
];

/// RULE: the running emulator serves an invoke of a deployed `CUSTOM`-gated stage from the api's
/// records whatever the `Authorization` header carries, and the authorizer's own policy decides
/// it before the integration is dispatched.
#[test]
fn a_started_emulator_serves_a_token_gated_rest_stage_to_an_unsigned_client() {
    let emulator = Started::start();
    deploy(&emulator);
    let backend_arn = active_function_arn(&emulator, BACKEND_FUNCTION);
    let authorizer_arn = active_function_arn(&emulator, AUTHORIZER_FUNCTION);
    wait_until_table_active(&emulator);

    let api_record = apigw(
        &emulator,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "started-authorizer-gate-api" })),
        201,
    );
    let api = as_str("CreateRestApi", &api_record, "id");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let resource = apigw(
        &emulator,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    );
    let resource_id = as_str("CreateResource", &resource, "id");
    let authorizer = apigw(
        &emulator,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "token-gate",
            "type": "TOKEN",
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{authorizer_arn}/\
                 invocations"
            ),
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    );
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    grant(
        &emulator,
        AUTHORIZER_FUNCTION,
        "apigw-authorizer",
        &format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/authorizers/{authorizer_id}"),
    );
    grant(
        &emulator,
        BACKEND_FUNCTION,
        "apigw-backend",
        &format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/{VERB}/{RESOURCE_PATH}"),
    );
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/{VERB}");
    apigw(
        &emulator,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer_id })),
        201,
    );
    apigw(
        &emulator,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{backend_arn}/\
                 invocations"
            ),
        })),
        201,
    );
    apigw(
        &emulator,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    );

    let mut owed: Vec<(String, bool)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let key = format!("item-{index}");
        let (host, path) = if row.own_host {
            (
                format!("{api}.execute-api.{REGION}.amazonaws.com"),
                format!("/{STAGE}/{RESOURCE_PATH}?key={key}"),
            )
        } else {
            (
                emulator.authority(),
                format!("/_aws/execute-api/{api}/{STAGE}/{RESOURCE_PATH}?key={key}"),
            )
        };
        // The identity source carries a bearer token and the request is signed with nothing: the
        // position every client of a deployed stage is in.
        let (status, text) = emulator.unsigned(
            &host,
            VERB,
            &path,
            &[(TOKEN_HEADER, row.token)],
            Vec::new(),
        );
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {} from the stage, got {status} with {text}; the \
             log:\n{}",
            row.status,
            emulator.log()
        );
        let answered: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match row.message {
            Some(sentence) => assert_eq!(
                answered.get(MESSAGE_MEMBER).and_then(Value::as_str),
                Some(sentence),
                "case {case:?}: expected {MESSAGE_MEMBER:?} to state {sentence:?}, got {text}"
            ),
            None => assert_eq!(
                answered,
                json!({ "ok": true }),
                "case {case:?}: expected the integration's own answer, got {text}"
            ),
        }

        owed.push((key, row.integration_runs));
        for (written, expected) in &owed {
            let held = item_under(&emulator, written);
            let wanted = expected.then(|| {
                json!({
                    HASH_KEY: { "S": written },
                    PATH_MEMBER: { "S": format!("/{RESOURCE_PATH}") },
                })
            });
            assert_eq!(
                held, wanted,
                "case {case:?}: expected the table to hold {wanted:?} under {written:?}, got \
                 {held:?}"
            );
        }
    }

    // The class a refusal names, read once over a fresh request: the header travels beside the
    // body on the same answer, so a row that read the body has already shown the status.
    let (status, text) = emulator.unsigned(
        &emulator.authority(),
        VERB,
        &format!("/_aws/execute-api/{api}/{STAGE}/{RESOURCE_PATH}?key=classed"),
        &[(TOKEN_HEADER, DENY_TOKEN)],
        Vec::new(),
    );
    assert_eq!(
        status, 403,
        "the refusal's class: expected HTTP 403, got {status} with {text}"
    );
    let classed = emulator
        .client()
        .get(format!(
            "https://{}/_aws/execute-api/{api}/{STAGE}/{RESOURCE_PATH}?key=classed",
            emulator.authority()
        ))
        .header(TOKEN_HEADER, DENY_TOKEN)
        .send()
        .expect("the started emulator answers the invoke");
    assert_eq!(
        classed
            .headers()
            .get(ERROR_TYPE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(ACCESS_DENIED_EXCEPTION),
        "the refusal's class: expected {ERROR_TYPE_HEADER:?} to name {ACCESS_DENIED_EXCEPTION:?}, \
         got {:?}",
        classed.headers()
    );
    assert_eq!(
        item_under(&emulator, "classed"),
        None,
        "the refusal's class: the refused request must have written nothing"
    );
}
