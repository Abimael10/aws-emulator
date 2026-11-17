// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own DynamoDB CONTROL-PLANE call is authorized against: the
//! policies in force on the function's execution role.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK on the function's import path, so
//! a handler that does `import boto3` and calls a service runs and reports either the result
//! or that service's own refusal — never a runtime fault of its own. The call is made as a
//! session of the function's execution role, so the service serving it decides it against
//! the policies in force on that role: a CONTROL-PLANE call on a table (describing it,
//! changing it, tagging it, backing it up, deleting it) no identity-based policy in force
//! allows is refused `AccessDeniedException` (HTTP 400, the status the service framework
//! behind DynamoDB answers an authorization refusal with) naming the assumed-role session,
//! the IAM action the operation performs and the table's ARN, and the refused call changes
//! nothing; a call those documents allow is served.
//!
//! Why the two halves belong in ONE suite: a least-privilege execution role is only worth
//! anything if the probe that exercises it RUNS. A handler that cannot import the SDK reports
//! the same runtime fault whether the policy allowed the call or refused it, so the allowed
//! and the denied halves become indistinguishable and an over-broad grant passes unnoticed.
//! Both halves are asserted per probe here: the invocation carries no `X-Amz-Function-Error`,
//! and the payload is the verdict DynamoDB reached.
//!
//! The grant one row carries is the action set the published SAM `DynamoDBCrudPolicy`
//! shorthand expands to — the item, query and scan actions plus `DescribeTable` — which is
//! what a client bounds a function's blast radius with: it must leave the items writable and
//! the table itself untouchable.
//!
//! Five crates have to agree for that to hold — IAM holds the role and its document, STS
//! mints the session the function's calls are signed with, CloudFormation archives the
//! inline handler into a deployment package, Lambda runs it and supplies the SDK, and
//! DynamoDB takes the decision — so the suite lives here. `lambda_in_function_dynamodb_
//! authorization` pins the same rule for the ITEM operations; what this one reaches is the
//! control plane, which a role scoped to a table's items must not reach.
//!
//! The rows differ along what the rule generalises over: what the role's document allows
//! (the CRUD-shaped set, every action of the service), and WHICH control-plane operation the
//! handler calls (a describe, a settings change, a tag write, a backup, a delete) beside one
//! item write that must stay served. A role the account grants NOTHING is the DynamoDB
//! crate's own `table_control_plane` suite's row, over the same twelve calls, so it is not
//! repeated here: what needs a function to be reached at all is the grant that serves the
//! items and refuses the table.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the table's ARN and every session's ARN carry.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` the functions' stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status DynamoDB refuses an unauthorized call with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created table reaching
/// `ACTIVE`, a stack reaching `CREATE_COMPLETE`, a created function reaching `Active` — and
/// how many looks to take. The spans are those services' own, so no fixed sleep names the
/// instant one ends, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The action set the published `DynamoDBCrudPolicy` SAM shorthand grants over one table:
/// the item, query and scan actions plus `DescribeTable` ("AWS SAM policy templates").
const CRUD_ACTIONS: &[&str] = &[
    "dynamodb:GetItem",
    "dynamodb:DeleteItem",
    "dynamodb:PutItem",
    "dynamodb:Scan",
    "dynamodb:Query",
    "dynamodb:UpdateItem",
    "dynamodb:BatchWriteItem",
    "dynamodb:BatchGetItem",
    "dynamodb:DescribeTable",
    "dynamodb:ConditionCheckItem",
];

/// The handler every row runs: it builds a DynamoDB client with the SDK the runtime supplies
/// and makes the one call the event names, reporting either that the call was served or the
/// refusal DynamoDB answered it with — its code, its sentence and the HTTP status it came
/// back on.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("dynamodb")
    table = event["table"]
    try:
        if event["op"] == "put_item":
            client.put_item(TableName=table, Item={"pk": {"S": event["pk"]}})
            return {"ok": True}
        if event["op"] == "describe_table":
            described = client.describe_table(TableName=table)
            return {"ok": True, "status": described["Table"]["TableStatus"]}
        if event["op"] == "update_table":
            client.update_table(TableName=table, DeletionProtectionEnabled=True)
            return {"ok": True}
        if event["op"] == "tag_resource":
            client.tag_resource(
                ResourceArn=event["table_arn"],
                Tags=[{"Key": "owner", "Value": "probe"}],
            )
            return {"ok": True}
        if event["op"] == "create_backup":
            client.create_backup(TableName=table, BackupName=event["backup"])
            return {"ok": True}
        client.delete_table(TableName=table)
        return {"ok": True}
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched
/// back into this same router, so the table it addresses is the table this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. Its credential is the long-term key every emulator publishes, so
/// these calls are the account's own and not any row's role's.
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

/// One DynamoDB operation over its `awsJson1_0` wire, made as the account's own credential.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("dynamodb"))
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
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

/// The ARN of a table of this account in this Region.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}")
}

/// WHICH call one probe's handler makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// An item write, which a CRUD-shaped grant covers.
    PutItem,
    DescribeTable,
    UpdateTable,
    TagResource,
    CreateBackup,
    DeleteTable,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::PutItem => "put_item",
            Self::DescribeTable => "describe_table",
            Self::UpdateTable => "update_table",
            Self::TagResource => "tag_resource",
            Self::CreateBackup => "create_backup",
            Self::DeleteTable => "delete_table",
        }
    }

    /// The IAM action the operation performs, which is the action a refusal names: DynamoDB's
    /// action list carries one `dynamodb:<OperationName>` per operation.
    fn action(self) -> &'static str {
        match self {
            Self::PutItem => "dynamodb:PutItem",
            Self::DescribeTable => "dynamodb:DescribeTable",
            Self::UpdateTable => "dynamodb:UpdateTable",
            Self::TagResource => "dynamodb:TagResource",
            Self::CreateBackup => "dynamodb:CreateBackup",
            Self::DeleteTable => "dynamodb:DeleteTable",
        }
    }
}

/// One probe of a row's role: the call the handler makes, and whether the policies in force
/// let it through.
struct Probe {
    case: &'static str,
    op: Op,
    allowed: bool,
}

/// One row: the table it acts on, the actions its execution role's inline document allows
/// besides the log actions every execution role carries, and the probes taken against it.
///
/// A table of its own per row, so one row's delete cannot decide another row's probe.
struct Row {
    table: &'static str,
    actions: &'static [&'static str],
    probes: &'static [Probe],
}

const ROWS: &[Row] = &[
    // The case the rule is stated about: the action set a `DynamoDBCrudPolicy` shorthand
    // expands to leaves the items writable and the table itself untouchable.
    Row {
        table: "xsvc-in-fn-ddb-crud",
        actions: CRUD_ACTIONS,
        probes: &[
            Probe {
                case: "a CRUD-shaped grant writes an item",
                op: Op::PutItem,
                allowed: true,
            },
            Probe {
                case: "a CRUD-shaped grant describes the table",
                op: Op::DescribeTable,
                allowed: true,
            },
            Probe {
                case: "a CRUD-shaped grant changes no setting of the table",
                op: Op::UpdateTable,
                allowed: false,
            },
            Probe {
                case: "a CRUD-shaped grant tags nothing",
                op: Op::TagResource,
                allowed: false,
            },
            Probe {
                case: "a CRUD-shaped grant backs the table up nowhere",
                op: Op::CreateBackup,
                allowed: false,
            },
            Probe {
                case: "a CRUD-shaped grant cannot destroy the table it reads and writes",
                op: Op::DeleteTable,
                allowed: false,
            },
        ],
    },
    Row {
        table: "xsvc-in-fn-ddb-every-action",
        actions: &["dynamodb:*"],
        probes: &[
            Probe {
                case: "a role granting every action of the service tags the table",
                op: Op::TagResource,
                allowed: true,
            },
            Probe {
                case: "and backs it up by the same statement",
                op: Op::CreateBackup,
                allowed: true,
            },
            // The delete is the last probe of the last row, so no later probe needs the
            // table it removes.
            Probe {
                case: "and deletes it by the same statement",
                op: Op::DeleteTable,
                allowed: true,
            },
        ],
    },
];

/// The role one row runs as, and the function it runs in — named here because the refusal's
/// own sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-ddb-cp-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-ddb-cp-probe-{index}")
}

/// The sentence DynamoDB refuses a call nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about.
fn refusal_message(index: usize, table: &str, action: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: \
         {action} on resource: {} because no identity-based policy allows the {action} action",
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

/// The inline document one row's role carries: the log actions every execution role needs,
/// and the row's own statement where it has one — scoped to that row's table and its
/// indexes, which is how a published shorthand scopes its expansion.
fn role_document(row: &Row) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if !row.actions.is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": row.actions,
            "Resource": [
                table_arn(row.table),
                format!("{}/index/*", table_arn(row.table)),
            ],
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the role one row runs as, with the document [`role_document`] states, and answer
/// the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, index: usize, row: &Row) -> String {
    let name = role_name(index);
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
            ("PolicyDocument", &role_document(row)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Create one row's table and wait until DynamoDB reports it `ACTIVE`, asserting the ARN it
/// reports is the one the statements and the refusals name.
async fn create_table(router: &MultiServiceRouter, table: &str) {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": table,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTable({table:?}) must succeed, got {status} with {created}"
    );
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        let (_, answered) = dynamodb(router, "DescribeTable", &json!({"TableName": table})).await;
        described = answered;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        (
            described["Table"]["TableStatus"].as_str(),
            described["Table"]["TableArn"].as_str(),
        ),
        (Some("ACTIVE"), Some(table_arn(table).as_str())),
        "setup: DynamoDB must report {table:?} ACTIVE under the ARN a statement names, got \
         {described}"
    );
}

/// The stack declaring one function per row, each running as that row's role: the inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all.
fn function_stack(role_arns: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, role_arn) in role_arns.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
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
    stack: &str,
    role_arns: &[String],
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
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
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
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
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Invoke `function` with `event` until the request is SERVED, answering what it was served
/// with.
///
/// A host running many handlers at once has only so much engine memory and so many run
/// threads to hold them in, and an invocation that finds none is answered the service's own
/// capacity refusal (`TooManyRequestsException`, HTTP 429) or is given up on (HTTP 500) —
/// neither of which is the decision under test. Those two are retried, as a client retries
/// a throttled invocation, until one is served or a generous deadline passes; every other
/// answer, success or failure, is the one this probe is about and is returned at once.
async fn invoke_until_served(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let path = format!("/2015-03-31/functions/{function}/invocations");
    let mut last = (0, http::HeaderMap::new(), Value::Null);
    for _ in 0..LOOKS {
        let answered = lambda(router, "POST", &path, event).await;
        if !matches!(answered.0, 429 | 500) {
            return answered;
        }
        last = answered;
        tokio::time::sleep(LOOK).await;
    }
    panic!(
        "case {case:?}: the invocation was never served; last answer {} with {}",
        last.0, last.2
    );
}

/// What the table `table` looks like to the ACCOUNT's own credential now: its status, its
/// deletion-protection flag, its tags and how many backups the account holds of it — the
/// state a refused control-plane call must have left exactly as it was.
async fn table_state(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    let (status, described) = dynamodb(router, "DescribeTable", &json!({"TableName": table})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTable({table:?}) must succeed, got {status} with {described}"
    );
    let (status, tags) = dynamodb(
        router,
        "ListTagsOfResource",
        &json!({"ResourceArn": table_arn(table)}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTagsOfResource({table:?}) must succeed, got {status} with {tags}"
    );
    let (status, backups) = dynamodb(router, "ListBackups", &json!({"TableName": table})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBackups({table:?}) must succeed, got {status} with {backups}"
    );
    json!({
        "status": described["Table"]["TableStatus"].clone(),
        "protected": described["Table"]["DeletionProtectionEnabled"].clone(),
        "tags": tags["Tags"].clone(),
        "backups": backups["BackupSummaries"].as_array().map_or(0, Vec::len),
    })
}

/// Every probe: the function's own control-plane call is decided against the policies in
/// force on its execution role, the handler RUNS to report that decision, and a refused call
/// leaves the table as it was.
#[tokio::test]
async fn an_in_function_control_plane_call_is_decided_against_its_execution_role() {
    let router = build_router();
    let mut role_arns = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        create_table(&router, row.table).await;
        role_arns.push(create_role(&router, index, row).await);
    }
    let physical = deploy_functions(&router, "xsvc-in-fn-ddb-cp-probes", &role_arns).await;

    for (index, row) in ROWS.iter().enumerate() {
        let logical = format!("Probe{index}");
        let function = physical.get(&logical).cloned().unwrap_or_else(|| {
            panic!("setup: the stack must report a physical id for {logical}")
        });
        assert_eq!(
            function,
            function_name(index),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(index)
        );
        wait_until_active(&router, &function).await;

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let before = table_state(&router, case, row.table).await;
            let event = json!({
                "op": probe.op.event(),
                "table": row.table,
                "table_arn": table_arn(row.table),
                "pk": format!("row-{index}-probe-{probe_index}"),
                "backup": format!("probe-backup-{index}-{probe_index}"),
            });
            let (status, headers, payload) = invoke_until_served(
                &router,
                case,
                &function,
                event.to_string().as_bytes(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the invocation must be served, got {status} with {payload}"
            );
            // The SDK the runtime supplies is on the handler's import path, so the handler
            // runs and reports a verdict rather than faulting: a fault here makes the
            // allowed and the denied halves of the probe indistinguishable.
            let faulted = headers
                .get("x-amz-function-error")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            assert_eq!(
                faulted, None,
                "case {case:?}: the handler must run to completion, got {faulted:?} with \
                 {payload}"
            );

            // What the handler reports is what DynamoDB answered its call with.
            let expected = match (probe.allowed, probe.op) {
                (true, Op::DescribeTable) => json!({"ok": true, "status": "ACTIVE"}),
                (true, _) => json!({"ok": true}),
                (false, op) => json!({
                    "ok": false,
                    "code": ACCESS_DENIED,
                    "message": refusal_message(index, row.table, op.action()),
                    "status": ACCESS_DENIED_STATUS,
                }),
            };
            assert_eq!(
                payload, expected,
                "case {case:?}: expected the handler to report {expected}, got {payload}"
            );

            // And what the TABLE looks like afterwards: a refused call changed nothing at
            // all, while an allowed one is free to change what it addressed.
            if !probe.allowed {
                let after = table_state(&router, case, row.table).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: the refused call must leave the table as it was, got \
                     {after} against {before}"
                );
            }
        }
    }
}
