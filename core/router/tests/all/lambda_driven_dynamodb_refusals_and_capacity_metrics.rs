// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own DynamoDB item call is answered with, what it costs the table,
//! and where the record of a refusal it did not catch ends up.
//!
//! RULE: a DynamoDB item call an AWS SDK makes from inside a running function is a real call
//! made as a session of the function's execution role, so DynamoDB decides and answers it
//! exactly as it answers a client's own call —
//!
//! * a call no identity-based policy in force on that role allows is refused
//!   `AccessDeniedException` (HTTP 400) worded as the assumed-role session, the IAM action the
//!   operation performs and the ARN of the table it addressed, and nothing of it lands;
//! * a call against a table that does not exist is refused `ResourceNotFoundException`
//!   (HTTP 400) with that service's fixed data-plane sentence;
//! * a call the policies allow is SERVED, and the capacity it reports as its own
//!   `ConsumedCapacity` is counted into that table's `AWS/DynamoDB` series —
//!   `ConsumedWriteCapacityUnits` for what a write spent, `ConsumedReadCapacityUnits` for what
//!   a read spent, dimensioned by `TableName` — so a `Sum` over the window the traffic fell in
//!   is exactly what the responses reported and a table nothing addressed has no datapoint at
//!   all;
//!
//! and either way the records the run wrote reach the function's own log group (the default
//! `/aws/lambda/<function name>`), the traceback of a refusal the handler left uncaught
//! included — which is also the invocation's own `X-Amz-Function-Error: Unhandled`, because
//! what decides that is whether the handler returned rather than what any service answered it.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), with neither an identity-based nor a resource-based allow a
//! request is denied (IAM User Guide, *Policy evaluation logic*), `PutItem` and `GetItem` each
//! declare `ResourceNotFoundException` as a modelled client error at HTTP 400 and carry
//! `ConsumedCapacity` in their output
//! (`smithy_query(query="operation", operation="PutItem,GetItem")`), and "Lambda functions send
//! logs to a default log group named `/aws/lambda/<function name>`"
//! (`smithy_query(query="doc", shape="LoggingConfig", member="LogGroup")`).
//!
//! Six crates have to agree for that to hold — CloudFormation deploys the probe functions, IAM
//! holds each role and its embedded document, the token service mints the session the handler's
//! call is signed as, Lambda runs the handler and supplies it the SDK, DynamoDB decides the
//! call and publishes what it cost, and CloudWatch holds both the series and the log group — so
//! the suite lives here: a crate-local suite can only ever see one side of it. What the
//! neighbours pin and this one does not repeat: `lambda_in_function_dynamodb_authorization`
//! drives the TABLE of identity documents one in-function item call is decided by,
//! `dynamodb_write_capacity_metrics_drive_the_alarm_chain` the alarm chain a CLIENT's own write
//! traffic drives, and `lambda_invoke_request_id_correlates_with_its_log_group` the framing
//! records of a run. What only this suite reaches is the capacity a LAMBDA-driven call charges
//! read back out of the metric plane, and the refusal an operator debugs from read back out of
//! the function's log group.
//!
//! The rows differ along what the rule generalises over: which half of the throughput the call
//! spends (a write, a read), HOW MANY calls one invocation makes (one, three — so a series
//! carrying one datapoint per invocation rather than per call fails a row), which refusal the
//! call meets (none, the authorization one, the not-found one), whether the handler catches it,
//! and which table it addresses (two metered tables, one that does not exist, and a control
//! table nothing addresses at all).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the tables and the functions
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every table ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The table the write probes address, the one the read probe addresses, and the control table
/// nothing addresses at all.
const WRITTEN_TABLE: &str = "xsvc-fn-ddb-written";
const READ_TABLE: &str = "xsvc-fn-ddb-read";
const QUIET_TABLE: &str = "xsvc-fn-ddb-quiet";

/// A table name this suite never creates, which one row's handler addresses.
const MISSING_TABLE: &str = "xsvc-fn-ddb-absent";

/// The stack the probe functions are declared in, and the `StackStatus` it must reach.
const STACK: &str = "xsvc-fn-ddb-capacity-probes";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` AWS Lambda reports a function it takes calls on in.
const ACTIVE: &str = "Active";

/// The code and status DynamoDB refuses an item call the policies in force do not allow with.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The code DynamoDB refuses an item call against a table that is not there with, and that
/// service's FIXED data-plane sentence for it — written out here rather than read from any
/// crate, so the wording has something to be wrong about.
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_MESSAGE: &str = "Requested resource not found";

/// The status both refusals come back on: each is a modelled client error of these operations
/// with no `@httpError` of its own
/// (`smithy_query(query="operation", operation="PutItem,GetItem")`).
const REFUSAL_STATUS: u64 = 400;

/// The signal an invocation whose handler raised carries, and the status such an invocation is
/// still answered on: the run failed, the call did not.
const UNHANDLED: &str = "Unhandled";

/// The namespace Amazon DynamoDB publishes a table's own metrics under, the two series a
/// request's consumption is counted into, and the dimension that names the table.
const DYNAMODB_NAMESPACE: &str = "AWS/DynamoDB";
const CONSUMED_WRITE_CAPACITY_UNITS: &str = "ConsumedWriteCapacityUnits";
const CONSUMED_READ_CAPACITY_UNITS: &str = "ConsumedReadCapacityUnits";
const TABLE_NAME_DIMENSION: &str = "TableName";

/// CloudWatch's signing name, which is what routes a request to it, and the `X-Amz-Target`
/// prefix its model declares.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The prefix of the default log group of a function whose `LoggingConfig` names none: "a
/// default log group named `/aws/lambda/<function name>`"
/// (`smithy_query(query="doc", shape="LoggingConfig", member="LogGroup")`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The record AWS Lambda closes every run with, naming the invocation it belongs to: the one
/// record a run that ENDED has written whatever the handler returned, so a search for it stands
/// for "this run's records reached the group".
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The first line of the report the Python runtime writes for an exception the handler left
/// uncaught, which is the traceback an operator debugs from.
const TRACEBACK_PREFIX: &str = "Traceback";

/// The resolution a standard-resolution series is read at, in seconds, and how far either side
/// of now a read of one looks. The window is far wider than any row's traffic needs so the
/// clock's choice of period boundary cannot decide an outcome; the sum over it is the units the
/// table consumed.
const PERIOD_SECONDS: i64 = 60;
const WINDOW_SECONDS: f64 = 1_800.0;

/// The largest difference two capacity figures may show and still be the same figure: both
/// sides are sums of whole units, so anything above rounding is a real divergence.
const TOLERANCE: f64 = 1e-9;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what DynamoDB answered and what it charged,
/// not how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work a service owes — a table reaching `ACTIVE`, a stack
/// reaching `CREATE_COMPLETE`, a function reaching `Active`, a run's records reaching its log
/// group — and how many looks to take. The spans are those services' own, so nothing here
/// sleeps a fixed span on their behalf, and the budget is generous so a loaded host is slower
/// without the rule being any different.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 600;

/// The handler every probe runs: it builds a DynamoDB client with the SDK the runtime supplies,
/// makes the item calls the event names with `ReturnConsumedCapacity` of `TOTAL`, and reports
/// the capacity those responses themselves carried — or, when the event says to catch, the
/// refusal DynamoDB answered with: its code, its sentence and the HTTP status it came back on.
/// An event that says not to catch leaves the refusal to propagate, which is what makes the run
/// itself fail and its traceback reach the log group.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def call(client, event):
    table = event["table"]
    if event["op"] == "write":
        spent = 0.0
        for index in range(event["calls"]):
            answered = client.put_item(
                TableName=table,
                Item={
                    "pk": {"S": "%s-%d" % (event["pk"], index)},
                    "body": {"S": event["body"]},
                },
                ReturnConsumedCapacity="TOTAL",
            )
            spent += answered["ConsumedCapacity"]["CapacityUnits"]
        return {"ok": True, "units": spent}
    answered = client.get_item(
        TableName=table,
        Key={"pk": {"S": "%s-0" % event["pk"]}},
        ConsistentRead=True,
        ReturnConsumedCapacity="TOTAL",
    )
    return {
        "ok": True,
        "units": answered["ConsumedCapacity"]["CapacityUnits"],
        "found": "Item" in answered,
    }


def handler(event, context):
    client = boto3.client("dynamodb")
    if not event["catch"]:
        return call(client, event)
    try:
        return call(client, event)
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the table it addresses, the series its consumption lands in and
/// the group its records reach are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and not any probe role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One `X-Amz-Target` RPC call of the service whose signing name is `signing_name`, as its
/// status and its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("authorization", authorization(signing_name))
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One DynamoDB operation over its `awsJson1_0` wire, made as the account's own credential.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "dynamodb",
        "application/x-amz-json-1.0",
        &format!("DynamoDB_20120810.{operation}"),
        body,
    )
    .await
}

/// One Amazon CloudWatch call over its `awsJson1_0` wire, which must be answered.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        "application/x-amz-json-1.0",
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire, as its status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "logs",
        "application/x-amz-json-1.1",
        &format!("Logs_20140328.{operation}"),
        body,
    )
    .await
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The class an invocation's answer reports the run's failure under, where it failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The ARN of a table of this account in this Region.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}")
}

/// WHICH half of the throughput one probe's call spends, and so which series it is counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `calls` single-item writes in one invocation.
    Write(u64),
    /// One strongly consistent read.
    Read,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::Write(_) => "write",
            Self::Read => "read",
        }
    }

    /// How many calls one invocation of it makes.
    fn calls(self) -> u64 {
        match self {
            Self::Write(calls) => calls,
            Self::Read => 1,
        }
    }

    /// The IAM action the operation performs, which is the action an authorization refusal
    /// names: DynamoDB's action list carries one `dynamodb:<OperationName>` per item operation.
    fn action(self) -> &'static str {
        match self {
            Self::Write(_) => "dynamodb:PutItem",
            Self::Read => "dynamodb:GetItem",
        }
    }

    /// The series the units it spends are counted into.
    fn metric(self) -> &'static str {
        match self {
            Self::Write(_) => CONSUMED_WRITE_CAPACITY_UNITS,
            Self::Read => CONSUMED_READ_CAPACITY_UNITS,
        }
    }
}

/// WHICH execution role the probe's function runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Every action of this service on every resource, beside the log actions.
    Granted,
    /// The log actions alone, which is what a least-privilege execution role carries.
    LogsOnly,
}

impl Role {
    /// The index of the role, and so of the function running as it, in the deployed stack.
    fn index(self) -> usize {
        match self {
            Self::Granted => 0,
            Self::LogsOnly => 1,
        }
    }

    /// The actions its embedded document allows besides the log actions; empty for the
    /// least-privilege role.
    fn actions(self) -> &'static [&'static str] {
        match self {
            Self::Granted => &["dynamodb:*"],
            Self::LogsOnly => &[],
        }
    }
}

/// What DynamoDB answers one probe's call with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The call is served, and what it reported spending is counted into its table's series.
    Served,
    /// Refused by the policies in force on the role.
    Denied,
    /// Refused because the table it addressed is not there.
    NotFound,
}

/// One probe: which role's function makes which calls against which table, whether the handler
/// catches a refusal, and what DynamoDB answers.
struct Probe {
    case: &'static str,
    role: Role,
    /// The table the handler addresses, and the key prefix it addresses in it.
    table: &'static str,
    pk: &'static str,
    op: Op,
    catches: bool,
    outcome: Outcome,
}

/// The probes, in the order they run: the read of `READ_TABLE` follows the write that planted
/// the item it reads, so no out-of-band call of this suite charges either metered table.
const PROBES: &[Probe] = &[
    Probe {
        case: "a granted role's single write is served and charged to its table",
        role: Role::Granted,
        table: WRITTEN_TABLE,
        pk: "served-single",
        op: Op::Write(1),
        catches: true,
        outcome: Outcome::Served,
    },
    Probe {
        case: "three writes in one invocation are charged as three",
        role: Role::Granted,
        table: WRITTEN_TABLE,
        pk: "served-three",
        op: Op::Write(3),
        catches: true,
        outcome: Outcome::Served,
    },
    Probe {
        case: "a role granting only the log actions is refused the write",
        role: Role::LogsOnly,
        table: WRITTEN_TABLE,
        pk: "denied-write",
        op: Op::Write(1),
        catches: true,
        outcome: Outcome::Denied,
    },
    Probe {
        case: "the same refusal left uncaught is the run's own failure",
        role: Role::LogsOnly,
        table: WRITTEN_TABLE,
        pk: "denied-uncaught",
        op: Op::Write(1),
        catches: false,
        outcome: Outcome::Denied,
    },
    Probe {
        case: "the same role is refused the read of the same table",
        role: Role::LogsOnly,
        table: WRITTEN_TABLE,
        pk: "denied-read",
        op: Op::Read,
        catches: true,
        outcome: Outcome::Denied,
    },
    Probe {
        case: "a granted write plants the item the read probe reads",
        role: Role::Granted,
        table: READ_TABLE,
        pk: "to-be-read",
        op: Op::Write(1),
        catches: true,
        outcome: Outcome::Served,
    },
    Probe {
        case: "a granted read is served and charged to the read half",
        role: Role::Granted,
        table: READ_TABLE,
        pk: "to-be-read",
        op: Op::Read,
        catches: true,
        outcome: Outcome::Served,
    },
    Probe {
        case: "a granted write to a table that does not exist is refused not-found",
        role: Role::Granted,
        table: MISSING_TABLE,
        pk: "absent-caught",
        op: Op::Write(1),
        catches: true,
        outcome: Outcome::NotFound,
    },
    Probe {
        case: "the not-found refusal left uncaught is the run's own failure",
        role: Role::Granted,
        table: MISSING_TABLE,
        pk: "absent-uncaught",
        op: Op::Write(1),
        catches: false,
        outcome: Outcome::NotFound,
    },
    Probe {
        case: "a granted read of a table that does not exist is refused the same way",
        role: Role::Granted,
        table: MISSING_TABLE,
        pk: "absent-read",
        op: Op::Read,
        catches: true,
        outcome: Outcome::NotFound,
    },
];

/// The role one probe runs as, and the function it runs in — named here because an
/// authorization refusal's own sentence names both: the identity a function's call is signed as
/// is the session `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-fn-ddb-capacity-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-fn-ddb-capacity-probe-{index}")
}

/// The sentence DynamoDB refuses an item call nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about.
fn denial_message(index: usize, action: &str, table: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: {action} \
         on resource: {} because no identity-based policy allows the {action} action",
        role_name(index),
        function_name(index),
        table_arn(table),
    )
}

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The embedded document one role carries: the log actions every execution role needs, and the
/// role's own statement where it has one.
fn role_document(role: Role) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if !role.actions().is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": role.actions(),
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create one probe role with the document [`role_document`] states, and answer the ARN IAM
/// minted for it.
async fn create_role(router: &MultiServiceRouter, role: Role) -> String {
    let name = role_name(role.index());
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", &name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", &name),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", &role_document(role)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Create one on-demand table, asserting the create was accepted. The creates of the tables a
/// run needs are all made before any of them is waited on, so they settle together.
async fn create_table(router: &MultiServiceRouter, name: &str) {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTable({name:?}) must succeed, got {status} with {created}"
    );
}

/// Wait until DynamoDB reports `name` `ACTIVE` — "you can perform read and write operations only
/// on an ACTIVE table" — asserting the ARN it reports is the one a statement and a refusal name.
async fn wait_table_active(router: &MultiServiceRouter, name: &str) {
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        let (_, answered) = dynamodb(router, "DescribeTable", &json!({"TableName": name})).await;
        described = answered;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        described["Table"]["TableStatus"].as_str(),
        Some("ACTIVE"),
        "setup: DynamoDB must report {name:?} ACTIVE, got {described}"
    );
    assert_eq!(
        described["Table"]["TableArn"].as_str(),
        Some(table_arn(name).as_str()),
        "setup: the table's ARN is what a statement and a refusal name, got {described}"
    );
}

/// The stack declaring one function per role, each running as that role: the inline source is
/// archived into the deployment package by CloudFormation, which is how a function whose
/// handler is written here comes to exist at all.
fn function_stack(role_arns: &[(Role, String)]) -> String {
    let mut resources = serde_json::Map::new();
    for (role, role_arn) in role_arns {
        resources.insert(
            format!("Probe{}", role.index()),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(role.index()),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the functions' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    role_arns: &[(Role, String)],
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, STACK).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} {ACTIVE}, last reported {last}");
}

/// Wall-clock now as epoch seconds, the clock the series' windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The units `metric` carries for `table`, summed over the window around now: the `Sum` of a
/// consumption series is the capacity the table spent in that window.
async fn series_sum(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    table: &str,
) -> (f64, usize) {
    let at = now_secs();
    let answered = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": DYNAMODB_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": TABLE_NAME_DIMENSION, "Value": table}],
            "StartTime": at - WINDOW_SECONDS,
            "EndTime": at + WINDOW_SECONDS,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    let datapoints = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let sum = datapoints
        .iter()
        .filter_map(|datapoint| datapoint["Sum"].as_f64())
        .sum();
    (sum, datapoints.len())
}

/// Every message the function's own log group holds, looked for again until one of them carries
/// `term` or the looks run out. A run's records are delivered off the answer's path, so this
/// waits for the CONDITION rather than for a span; a group nothing has written to yet does not
/// exist, which that service answers as a refusal rather than as an empty list.
async fn logged_until(router: &MultiServiceRouter, group: &str, term: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for _ in 0..LOOKS {
        let (status, answered) =
            logs(router, "FilterLogEvents", &json!({"logGroupName": group})).await;
        if status == 200 {
            found = answered["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|event| event["message"].as_str())
                .map(str::to_string)
                .collect();
            if found.iter().any(|message| message.contains(term)) {
                return found;
            }
        }
        tokio::time::sleep(LOOK).await;
    }
    found
}

/// Every probe: what DynamoDB answered the function's own call with, what it charged the table,
/// and what the function's log group holds for the run.
#[tokio::test]
async fn in_function_item_calls_are_answered_charged_and_logged_as_dynamodb_answers_a_client() {
    let router = build_router();
    // Every create first, then every wait: the three tables are independent, so they settle
    // together rather than one after another.
    for table in [WRITTEN_TABLE, READ_TABLE, QUIET_TABLE] {
        create_table(&router, table).await;
    }
    for table in [WRITTEN_TABLE, READ_TABLE, QUIET_TABLE] {
        wait_table_active(&router, table).await;
    }

    let mut role_arns = Vec::new();
    for role in [Role::Granted, Role::LogsOnly] {
        role_arns.push((role, create_role(&router, role).await));
    }
    let physical = deploy_functions(&router, &role_arns).await;
    for (role, _) in &role_arns {
        let logical = format!("Probe{}", role.index());
        let function = physical.get(&logical).cloned().unwrap_or_else(|| {
            panic!("setup: the stack must report a physical id for {logical}")
        });
        assert_eq!(
            function,
            function_name(role.index()),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(role.index())
        );
        wait_until_active(&router, &function).await;
    }

    // What the served calls have reported spending so far, per table and per series: the figure
    // each table's own series has to carry, built only out of what the responses themselves
    // said.
    let mut reported: BTreeMap<(&str, &str), f64> = BTreeMap::new();

    for probe in PROBES {
        let case = probe.case;
        let index = probe.role.index();
        let function = function_name(index);
        let body = format!("written-by-{}", probe.pk);
        let event = json!({
            "op": probe.op.event(),
            "calls": probe.op.calls(),
            "table": probe.table,
            "pk": probe.pk,
            "body": body,
            "catch": probe.catches,
        });
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} with {payload}"
        );

        // The sentence DynamoDB answers this probe's call with, where it refuses it.
        let refusal = match probe.outcome {
            Outcome::Served => None,
            Outcome::Denied => Some(denial_message(index, probe.op.action(), probe.table)),
            Outcome::NotFound => Some(NOT_FOUND_MESSAGE.to_string()),
        };

        if probe.catches {
            assert_eq!(
                function_error(&headers),
                None,
                "case {case:?}: a handler that catches the refusal runs to completion, got {:?} \
                 with {payload}",
                function_error(&headers)
            );
            match (&refusal, probe.op) {
                (None, Op::Write(_)) => {
                    assert_eq!(
                        payload["ok"],
                        json!(true),
                        "case {case:?}: the served write must be reported served, got {payload}"
                    );
                }
                (None, Op::Read) => {
                    assert_eq!(
                        payload["ok"],
                        json!(true),
                        "case {case:?}: the served read must be reported served, got {payload}"
                    );
                    assert_eq!(
                        payload["found"],
                        json!(true),
                        "case {case:?}: the read must find the item the earlier write planted, \
                         got {payload}"
                    );
                }
                (Some(message), _) => {
                    let code = match probe.outcome {
                        Outcome::Denied => ACCESS_DENIED,
                        _ => NOT_FOUND,
                    };
                    let expected = json!({
                        "ok": false,
                        "code": code,
                        "message": message,
                        "status": REFUSAL_STATUS,
                    });
                    assert_eq!(
                        payload, expected,
                        "case {case:?}: expected the handler to report {expected}, got {payload}"
                    );
                }
            }
        } else {
            // The refusal was left to propagate: the RUN failed, so the invocation carries the
            // function error and the run's own error document.
            assert_eq!(
                function_error(&headers).as_deref(),
                Some(UNHANDLED),
                "case {case:?}: an uncaught refusal must surface as the function's own error, \
                 got {:?} with {payload}",
                function_error(&headers)
            );
            let sentence = refusal
                .clone()
                .expect("a probe that does not catch is one whose call is refused");
            let reported_message = payload["errorMessage"].as_str().unwrap_or_default();
            assert!(
                reported_message.contains(&sentence),
                "case {case:?}: the error document must carry what DynamoDB refused the call \
                 with ({sentence:?}), got {payload}"
            );
            assert!(
                payload["errorType"]
                    .as_str()
                    .is_some_and(|kind| !kind.is_empty()),
                "case {case:?}: the error document must name the class that was raised, got \
                 {payload}"
            );
        }

        // Whatever the outcome, the run's records reach the function's own log group — and for
        // a refusal the handler left uncaught, the traceback the operator debugs from is among
        // them, carrying the sentence DynamoDB refused the call with.
        let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
        let term = if probe.catches {
            REPORT_PREFIX.to_string()
        } else {
            refusal
                .clone()
                .expect("a probe that does not catch is one whose call is refused")
        };
        let held = logged_until(&router, &group, &term).await;
        assert!(
            held.iter().any(|message| message.contains(&term)),
            "case {case:?}: expected the group {group:?} to hold a record carrying {term:?}, got \
             {held:?}"
        );
        if !probe.catches {
            assert!(
                held.iter().any(|message| message.contains(TRACEBACK_PREFIX)),
                "case {case:?}: expected the group {group:?} to hold the traceback of the run \
                 that failed, got {held:?}"
            );
        }

        // What the call reported spending, and so what its table's series has to carry: a
        // refusal reports no capacity member at all, and a served call reports the responses'
        // own figures.
        if probe.outcome == Outcome::Served {
            let spent = payload["units"].as_f64().unwrap_or_default();
            assert!(
                spent > 0.0,
                "case {case:?}: a served call must report the capacity it spent, got {payload}"
            );
            *reported
                .entry((probe.table, probe.op.metric()))
                .or_insert(0.0) += spent;
        } else {
            assert!(
                payload.get("units").is_none(),
                "case {case:?}: a refused call reports no capacity at all, got {payload}"
            );
        }

        // The series of the table this probe addressed, read where a client reads it.
        if probe.table != MISSING_TABLE {
            let expected = reported
                .get(&(probe.table, probe.op.metric()))
                .copied()
                .unwrap_or(0.0);
            let (published, datapoints) =
                series_sum(&router, case, probe.op.metric(), probe.table).await;
            assert!(
                (published - expected).abs() < TOLERANCE,
                "case {case:?}: expected the {} series of {:?} to carry the {expected} units the \
                 responses reported, got {published} over {datapoints} datapoint(s)",
                probe.op.metric(),
                probe.table
            );
        }
    }

    // Both metered tables end holding exactly what their served calls reported, in each half of
    // the throughput — so a write counted into the read series, or an index's units folded into
    // the table's, fails here.
    let whole_run = "the whole run, read once more at the end";
    for table in [WRITTEN_TABLE, READ_TABLE] {
        for metric in [CONSUMED_WRITE_CAPACITY_UNITS, CONSUMED_READ_CAPACITY_UNITS] {
            let expected = reported.get(&(table, metric)).copied().unwrap_or(0.0);
            let (published, datapoints) = series_sum(&router, whole_run, metric, table).await;
            assert!(
                (published - expected).abs() < TOLERANCE,
                "case {whole_run:?}: expected the {metric} series of {table:?} to carry the \
                 {expected} units its served calls reported, got {published} over {datapoints} \
                 datapoint(s)"
            );
        }
    }

    // And the control table nothing addressed has no datapoint in either series: a table whose
    // consumption is published for it by something other than its own traffic fails here.
    let control = "the control table nothing addressed";
    for metric in [CONSUMED_WRITE_CAPACITY_UNITS, CONSUMED_READ_CAPACITY_UNITS] {
        let (published, datapoints) = series_sum(&router, control, metric, QUIET_TABLE).await;
        assert_eq!(
            datapoints, 0,
            "case {control:?}: expected no {metric} datapoint for the untouched {QUIET_TABLE:?}, \
             got {datapoints} summing to {published}"
        );
    }
}
