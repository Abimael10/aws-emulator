// SPDX-License-Identifier: CC0-1.0

//! What a DynamoDB-stream event source mapping hands its function for each write made to the
//! streamed table, on the ASSEMBLED emulator, read where a client reads it — the handler's own
//! lines in the function's CloudWatch Logs group: DynamoDB holds the tables and mints their
//! streams, DynamoDB Streams serves those streams' shards and records, CloudFormation archives
//! the inline handler into the function's deployment package, IAM holds the execution role the
//! pollers read the streams as, Lambda polls the streams and RUNS the handler, and CloudWatch
//! Logs takes the log that run produced.
//!
//! RULE: a table's stream is "a flow of information about changes to items in the table"
//! (`StreamSpecification`), so an `Enabled` mapping whose `EventSourceArn` names it hands its
//! function exactly ONE record per item-level modification the table made — a write that
//! stored the values the item already held modified nothing and so has no record at all —
//! and each record names which modification it was (`eventName`), the changed item's `Keys`,
//! the images the stream's `StreamViewType` captures ("determines what information is written
//! to the stream for this table": the new image for `NEW_IMAGE`, the old one for `OLD_IMAGE`,
//! both for `NEW_AND_OLD_IMAGES`, neither for `KEYS_ONLY`) with the attributes' own types and
//! nesting intact, and the stream it came from as an `eventSourceARN`
//! (`arn:aws:dynamodb:<region>:<account>:table/<table>/stream/<label>`), out of which the
//! table the change was made on is parseable.
//!
//! Six crates have to agree for that to hold, so only the assembled emulator shows it end to
//! end. What the neighbours pin and this suite does not repeat:
//! `all/ddb_stream_lambda_esm_provisioning` pins that a stream-enabled table's stream is
//! mappable at all, `all/ddb_stream_lambda_esm_filter_criteria` pins WHICH of the records a
//! mapping polls are handed over, and `all/ddb_stream_lambda_custom_metric_alarm` pins what a
//! consumer publishes about its own work. What is added here is the CONTENT of the hand-over —
//! the images each view type prescribes and the table name the envelope carries — together with
//! the write that must produce no hand-over at all, which is the pair a transition-detecting
//! consumer is built on: such a handler compares the two images of a record, and can only do
//! that if both are there and if a write that changed nothing never reaches it.
//!
//! Every verdict is read off what the HANDLER wrote: it renders each record it was handed into
//! one line, so a record withheld from it cannot appear and a record of the wrong shape makes
//! the run raise instead of printing anything. ONE function serves every row, each row bringing
//! its own table — so the table a record is of can only come out of that record's own
//! `eventSourceARN`, and a name carried anywhere else would put every row's records under one
//! table.
//!
//! The rows differ along what the rule generalises over: which `StreamViewType` the stream
//! captures (each of the four the model declares), and so which images each record carries —
//! and every row drives all three modifications (an insert, a change, a removal) over typed and
//! nested attributes, beside the re-save that modifies nothing. The re-save is made BEFORE every
//! write whose record the row expects, and a shard is read in order, so once the last expected
//! record has been logged the re-save has been read past and its absence is a verdict rather
//! than a race. No step waits a span of its own choosing: every wait looks again for the state it
//! asserts, and all of them share ONE generous deadline.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router as emulator};
use crate::query_wire::{elements, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// DynamoDB's `X-Amz-Target` prefix, and CloudWatch Logs'.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";
/// See [`DYNAMODB_TARGET_PREFIX`].
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The stack the execution role and the one consumer function are deployed by, and the
/// `StackStatus` it has to reach.
const STACK: &str = "xsvc-stream-record-images";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The one execution role the function below runs as, and the function itself.
const ROLE: &str = "xsvc-stream-record-images-role";
/// See [`ROLE`].
const FUNCTION: &str = "xsvc-stream-record-consumer";

/// The group a function's invocations are logged to when it names none: "By default, Lambda
/// functions send logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The attribute every table below is keyed by.
const KEY: &str = "pk";

/// The two keys each row writes: the item every modification is made to, and the item that is
/// stored twice with exactly the same values.
const CHANGED_KEY: &str = "main";
/// See [`CHANGED_KEY`].
const RESAVED_KEY: &str = "unchanged";

/// The `StreamViewType` values the model declares
/// (`smithy_query(query="shape", shape="StreamViewType")`, DynamoDB): what the stream captures
/// for each change.
const KEYS_ONLY: &str = "KEYS_ONLY";
/// See [`KEYS_ONLY`].
const NEW_IMAGE: &str = "NEW_IMAGE";
/// See [`KEYS_ONLY`].
const OLD_IMAGE: &str = "OLD_IMAGE";
/// See [`KEYS_ONLY`].
const NEW_AND_OLD_IMAGES: &str = "NEW_AND_OLD_IMAGES";

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// Where every mapping starts reading: the oldest record in the shard, so a write made before
/// the mapping's poller first looked is still delivered.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// How many records one mapping takes per batch, and the batching window it gathers one in.
/// The batch ceiling is above every row's record count, so one invocation can be handed a
/// whole row.
const BATCH_SIZE: u64 = 25;
/// See [`BATCH_SIZE`].
const BATCHING_WINDOW_SECONDS: u64 = 1;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the records carry and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// What the handler writes before each record's own fields, what it writes where a record
/// carries no image at all, and the separator between the fields of one record. ONE spelling of
/// each, bound into the handler's source and into the markers the assertions build.
const MARKER: &str = "stream record: ";
/// See [`MARKER`].
const ABSENT: &str = "-";
/// See [`MARKER`].
const FIELD: char = '|';

/// How long an observation of work the CLOCK owes — a table reaching `ACTIVE`, the function
/// going `Active`, a mapping settling, the records reaching the handler and its log reaching
/// the group — waits before calling it a hang, and how long between two looks. Generous,
/// because a whole-workspace run shares its cores with every other suite: the suite fails only
/// when the work did not happen at all, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(240);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler the one function runs: for each record it is handed it prints ONE line carrying
/// the table the change was made on — taken out of the record's own `eventSourceARN` — the
/// change's name, the changed item's keys, and each of the two images, every attribute rendered
/// with its own type and nesting.
///
/// It refuses a record that does not name itself a DynamoDB stream record, and one whose
/// `eventSourceARN` is not a table stream's, so a hand-over in some other shape fails the run
/// rather than being logged as if it were right. It reaches for nothing outside the record: the
/// line it writes is the whole of its report.
const RENDERING_HANDLER: &str = r##"
MARKER = "stream record: "
EVENT_SOURCE = "aws:dynamodb"
TABLE_SEGMENT = ":table/"
STREAM_SEGMENT = "/stream/"
ABSENT = "-"
FIELD = "|"


def render(value):
    names = list(value.keys())
    if len(names) != 1:
        raise ValueError("an attribute value is not a one-member union: " + repr(value))
    kind = names[0]
    held = value[kind]
    if kind == "M":
        parts = []
        for name in sorted(list(held.keys())):
            parts.append(name + "=" + render(held[name]))
        return "M{" + ",".join(parts) + "}"
    if kind == "L":
        parts = []
        for member in held:
            parts.append(render(member))
        return "L[" + ",".join(parts) + "]"
    if kind == "BOOL" or kind == "NULL":
        if held:
            return kind + "(true)"
        return kind + "(false)"
    if kind == "SS" or kind == "NS" or kind == "BS":
        return kind + "[" + ",".join(sorted(list(held))) + "]"
    return kind + "(" + str(held) + ")"


def image(body, member):
    if member not in body:
        return ABSENT
    held = body[member]
    parts = []
    for name in sorted(list(held.keys())):
        parts.append(name + "=" + render(held[name]))
    return "{" + ",".join(parts) + "}"


def table_of(arn):
    if TABLE_SEGMENT not in arn or STREAM_SEGMENT not in arn:
        raise ValueError("eventSourceARN names no table stream: " + repr(arn))
    return arn.split(TABLE_SEGMENT, 1)[1].split(STREAM_SEGMENT, 1)[0]


def handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        if record["eventSource"] != EVENT_SOURCE:
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        body = record["dynamodb"]
        print(
            MARKER
            + FIELD.join(
                [
                    table_of(record["eventSourceARN"]),
                    record["eventName"],
                    image(body, "Keys"),
                    image(body, "NewImage"),
                    image(body, "OldImage"),
                ]
            )
        )
    return {"logged": len(records)}
"##;

/// One row: the view type the row's own table captures its changes in.
struct Row {
    case: &'static str,
    view_type: &'static str,
}

/// The rows: one per `StreamViewType` the model declares, so each image the member can
/// prescribe is covered, including the one that prescribes neither.
const ROWS: &[Row] = &[
    Row {
        case: "both-images",
        view_type: NEW_AND_OLD_IMAGES,
    },
    Row {
        case: "the-new-image-only",
        view_type: NEW_IMAGE,
    },
    Row {
        case: "the-old-image-only",
        view_type: OLD_IMAGE,
    },
    Row {
        case: "neither-image",
        view_type: KEYS_ONLY,
    },
];

/// One row's own source table, named off its position: every row's records reach the SAME
/// function and are read out of the SAME group, so the table name is what tells them apart and
/// it can only come out of the record's own `eventSourceARN`.
fn table_name(index: usize) -> String {
    format!("xsvc-stream-record-source-{index}")
}

/// The item a row stores FIRST under a key: typed and nested attributes, so a record's image
/// has to carry the types and the nesting as written rather than a flattened copy of them.
fn first_item(key: &str) -> Value {
    json!({
        KEY: {"S": key},
        "count": {"N": "1"},
        "labels": {"SS": ["x", "y"]},
        "nested": {"M": {"flag": {"BOOL": true}, "parts": {"L": [{"S": "a"}, {"N": "2"}]}}},
    })
}

/// How the handler renders [`first_item`]: every attribute under its name, in the order
/// `sorted` puts the names in, each value as its type and its nesting.
fn first_image(key: &str) -> String {
    format!(
        "{{count=N(1),labels=SS[x,y],nested=M{{flag=BOOL(true),parts=L[S(a),N(2)]}},\
         {KEY}=S({key})}}"
    )
}

/// The item the same key is written with SECOND: a different value for every attribute, at
/// every depth, so an image that is the other one is visible as such.
fn second_item(key: &str) -> Value {
    json!({
        KEY: {"S": key},
        "count": {"N": "2"},
        "labels": {"SS": ["y", "z"]},
        "nested": {"M": {"flag": {"BOOL": false}, "parts": {"L": [{"S": "b"}, {"N": "3"}]}}},
    })
}

/// How the handler renders [`second_item`]; see [`first_image`].
fn second_image(key: &str) -> String {
    format!(
        "{{count=N(2),labels=SS[y,z],nested=M{{flag=BOOL(false),parts=L[S(b),N(3)]}},\
         {KEY}=S({key})}}"
    )
}

/// How the handler renders the `Keys` of a record of the item keyed `key`: the table's key
/// schema names one attribute, so the key is that attribute alone whatever the item holds
/// beside it.
fn keys_image(key: &str) -> String {
    format!("{{{KEY}=S({key})}}")
}

/// Whether `view_type` captures the item as it looked AFTER the change.
fn captures_new_image(view_type: &str) -> bool {
    view_type == NEW_IMAGE || view_type == NEW_AND_OLD_IMAGES
}

/// Whether `view_type` captures the item as it looked BEFORE the change.
fn captures_old_image(view_type: &str) -> bool {
    view_type == OLD_IMAGE || view_type == NEW_AND_OLD_IMAGES
}

/// One logged record, as this suite names it: everything the handler reports about it, without
/// the marker it opens the line with.
fn marker(table: &str, event_name: &str, key: &str, new_image: &str, old_image: &str) -> String {
    let keys = keys_image(key);
    format!("{table}{FIELD}{event_name}{FIELD}{keys}{FIELD}{new_image}{FIELD}{old_image}")
}

/// How every marker of a record of `event_name` on the item keyed `key` opens, whatever images
/// it carries: the fields of the record that are decided before any image is.
fn marker_prefix(table: &str, event_name: &str, key: &str) -> String {
    let keys = keys_image(key);
    format!("{table}{FIELD}{event_name}{FIELD}{keys}{FIELD}")
}

/// The images a row's stream captures of one change, as the handler renders them: an image the
/// change does not have, and one the view type does not capture, are both absent.
fn images(
    view_type: &str,
    new_image: Option<String>,
    old_image: Option<String>,
) -> (String, String) {
    let shown = |image: Option<String>, captured: bool| {
        image
            .filter(|_| captured)
            .unwrap_or_else(|| ABSENT.to_string())
    };
    (
        shown(new_image, captures_new_image(view_type)),
        shown(old_image, captures_old_image(view_type)),
    )
}

/// Every record a row's writes produce, as the markers the handler must log for them.
///
/// The item that was not there is an `INSERT` carrying only the new image, the one written again
/// with other values a `MODIFY` carrying both, and the one taken away a `REMOVE` carrying only
/// the old one — each held to what the row's view type captures.
fn expected_markers(row: &Row, table: &str) -> Vec<String> {
    let (resaved_new, resaved_old) = images(row.view_type, Some(first_image(RESAVED_KEY)), None);
    let (inserted_new, inserted_old) = images(row.view_type, Some(first_image(CHANGED_KEY)), None);
    let (changed_new, changed_old) = images(
        row.view_type,
        Some(second_image(CHANGED_KEY)),
        Some(first_image(CHANGED_KEY)),
    );
    let (removed_new, removed_old) = images(row.view_type, None, Some(second_image(CHANGED_KEY)));
    vec![
        marker(table, INSERT, RESAVED_KEY, &resaved_new, &resaved_old),
        marker(table, INSERT, CHANGED_KEY, &inserted_new, &inserted_old),
        marker(table, MODIFY, CHANGED_KEY, &changed_new, &changed_old),
        marker(table, REMOVE, CHANGED_KEY, &removed_new, &removed_old),
    ]
}

/// One DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "dynamodb",
        "application/x-amz-json-1.0",
        &format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), as its status and
/// parsed body.
///
/// Unlike the DynamoDB calls above, this one is NOT held to 200: a group or a stream no
/// delivery has created yet is answered the modelled `ResourceNotFoundException`, which is
/// exactly what a reader sees while the chain is still in flight.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "logs",
        "application/x-amz-json-1.1",
        &format!("{LOGS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// One `X-Amz-Target` RPC call against the service whose signing name is `signing_name`, as its
/// status and parsed body.
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
        .header("authorization", authorization(signing_name, REGION))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda", REGION))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudFormation `awsQuery` call, as its status and XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudformation", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document the execution role carries: the log actions an execution role needs for
/// its function's own log to be delivered, and the stream reads a DynamoDB-stream mapping's
/// poller makes.
///
/// The actions a DynamoDB-stream poller is decided on are the stream's own three
/// (`DescribeStream`, `GetShardIterator`, `GetRecords`), with the account-wide `ListStreams`
/// beside them as AWS's own managed execution-role policy for that source
/// (`AWSLambdaDynamoDBExecutionRole`) writes it.
fn role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:PutLogEvents",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": [
                    "dynamodb:DescribeStream",
                    "dynamodb:GetShardIterator",
                    "dynamodb:GetRecords",
                    "dynamodb:ListStreams",
                ],
                "Resource": "*",
            },
        ],
    })
}

/// The template the suite deploys: the one execution role and the one rendering function. The
/// inline source is archived into the deployment package by CloudFormation, which is how a
/// function whose handler is written here comes to exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "consume-and-log",
                        "PolicyDocument": role_document(),
                    }],
                },
            },
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": RENDERING_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the stack and wait for `CREATE_COMPLETE`, reporting what failed when it does not
/// reach it.
async fn deploy(router: &MultiServiceRouter) {
    let template = stack_template();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares a named IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let (reported, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
}

/// Wait until Lambda reports the consumer `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, deadline: Instant) {
    loop {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {FUNCTION:?} Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Ask for one row's stream-enabled source table, without waiting for it: every row's table is
/// asked for before any of them is waited out, so the span the clock owes each create is spent
/// once for all of them rather than once per row.
async fn create_table(router: &MultiServiceRouter, case: &str, name: &str, view_type: &str) {
    ddb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {"StreamEnabled": true, "StreamViewType": view_type},
        }),
    )
    .await;
}

/// Wait until the table `name` reports `ACTIVE` with a stream, answered as the ARN of the stream
/// it minted.
async fn await_stream_arn(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    deadline: Instant,
) -> String {
    loop {
        let described = ddb(router, case, "DescribeTable", &json!({"TableName": name})).await;
        let active = described["Table"]["TableStatus"] == json!("ACTIVE");
        if let (true, Some(arn)) = (active, described["Table"]["LatestStreamArn"].as_str()) {
            return arn.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table {name:?} never reached ACTIVE with a stream, last read \
             {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The table name the stream `stream_arn` belongs to, read out of the ARN the way the handler
/// reads it — so the name a row expects in its markers comes from the ARN DynamoDB minted and
/// not from the name the row asked for.
fn table_of(case: &str, stream_arn: &str) -> String {
    let after = stream_arn.split_once(":table/").unwrap_or_else(|| {
        panic!("case {case:?}: {stream_arn:?} names no table the stream belongs to")
    });
    after
        .1
        .split_once("/stream/")
        .unwrap_or_else(|| {
            panic!("case {case:?}: {stream_arn:?} carries no stream segment after its table")
        })
        .0
        .to_string()
}

/// Make every write of a row against `table`, in the order the stream captures them.
///
/// The re-save comes SECOND, before every write whose record the row expects, so the shard is
/// read past it by the time the last expected record has been logged.
async fn write_items(router: &MultiServiceRouter, case: &str, table: &str) {
    let put = |item: Value| json!({"TableName": table, "Item": item});
    ddb(router, case, "PutItem", &put(first_item(RESAVED_KEY))).await;
    // The same key stored with exactly the values it already holds: no modification of the
    // item, so the stream is a sequence that has no record of it.
    ddb(router, case, "PutItem", &put(first_item(RESAVED_KEY))).await;
    ddb(router, case, "PutItem", &put(first_item(CHANGED_KEY))).await;
    ddb(router, case, "PutItem", &put(second_item(CHANGED_KEY))).await;
    ddb(
        router,
        case,
        "DeleteItem",
        &json!({"TableName": table, "Key": {KEY: {"S": CHANGED_KEY}}}),
    )
    .await;
}

/// Ask for one row's mapping from `stream_arn` to the one consumer, without waiting for it,
/// answered as the UUID Lambda minted: every row's mapping is asked for before any of them is
/// waited out, so their pollers start together.
async fn create_mapping(router: &MultiServiceRouter, case: &str, stream_arn: &str) -> String {
    let request = json!({
        "FunctionName": FUNCTION,
        "EventSourceArn": stream_arn,
        "StartingPosition": STARTING_POSITION,
        "BatchSize": BATCH_SIZE,
        "MaximumBatchingWindowInSeconds": BATCHING_WINDOW_SECONDS,
        "Enabled": true,
    });
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, Some(&request)).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    created["UUID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
        .to_string()
}

/// Wait until Lambda reports the mapping `uuid` names `Enabled`, which is the state in which it
/// "is active" and reads its source.
async fn await_enabled(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    deadline: Instant,
) {
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if status == 200 && read["State"].as_str() == Some("Enabled") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported Enabled, last read {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every marker the handler has logged to `group` so far, across every stream of it, as a client
/// reads them: `DescribeLogStreams` and then `GetLogEvents` on each stream, keeping of each line
/// what the handler wrote after its marker.
///
/// A group no delivery has created yet holds none, which is what this answers while the chain is
/// still in flight. The markers are a SET: a batch whose attempt did not complete is attempted
/// again — "failed records are retried until the record expires" (`MaximumRetryAttempts`) — so
/// one record can be logged more than once without any record having been logged that should not
/// have been. Whether a record is delivered exactly once is a different rule and not this
/// suite's.
async fn logged_markers(router: &MultiServiceRouter, group: &str) -> BTreeSet<String> {
    let mut markers: BTreeSet<String> = BTreeSet::new();
    let (status, described) =
        logs(router, "DescribeLogStreams", &json!({"logGroupName": group})).await;
    // The group is created by the first delivery, so until one has arrived there is nothing to
    // read and no stream to read it from.
    if status != 200 {
        return markers;
    }
    let streams = described["logStreams"].as_array().cloned().unwrap_or_default();
    for stream in &streams {
        let name = stream["logStreamName"].as_str().unwrap_or_default();
        let (status, read) = logs(
            router,
            "GetLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": name,
                "startFromHead": true,
            }),
        )
        .await;
        if status != 200 {
            continue;
        }
        for event in read["events"].as_array().cloned().unwrap_or_default() {
            let message = event["message"].as_str().unwrap_or_default();
            if let Some(at) = message.find(MARKER) {
                markers.insert(message[at + MARKER.len()..].trim().to_string());
            }
        }
    }
    markers
}

/// Look at `group` again until every marker of `expected` has been logged, answering every
/// marker seen.
async fn awaited_markers(
    router: &MultiServiceRouter,
    group: &str,
    expected: &[String],
    deadline: Instant,
) -> BTreeSet<String> {
    loop {
        let seen = logged_markers(router, group).await;
        if expected.iter().all(|marker| seen.contains(marker)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "expected the handler to be handed {expected:?}, it logged {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: an `Enabled` DynamoDB-stream mapping hands its function one record per item-level
/// modification of the streamed table — and none for a write that modified nothing — each
/// naming the modification, the item's keys, the images the stream's `StreamViewType`
/// captures with their types and nesting intact, and the stream's own ARN, out of which the
/// table the change was made on is parseable.
#[tokio::test]
async fn stream_records_carry_the_view_types_images_and_no_record_is_made_of_an_unmodified_write() {
    let router = emulator();
    deploy(&router).await;

    // ONE deadline for every state the clock owes this suite, opened once the fixture the whole
    // chain needs is standing: the steps SHARE that patience rather than each taking a span of
    // its own, so the suite is as patient as it can be and still terminates.
    let deadline = Instant::now() + DEADLINE;
    wait_until_active(&router, deadline).await;

    // Each row's own table, asked for together so the span the clock owes each create is spent
    // once for all of them.
    let tables: Vec<String> = (0..ROWS.len()).map(table_name).collect();
    for (row, table) in ROWS.iter().zip(&tables) {
        create_table(&router, row.case, table, row.view_type).await;
    }

    // Then each row's writes, and its mapping — every write is made before the mapping exists,
    // so a mapping reading from the oldest retained record reads the whole sequence in the
    // order the table made it. The mappings are asked for together too, so their pollers start
    // together rather than one span apart.
    let mut expected: Vec<String> = Vec::new();
    let mut forbidden: Vec<(&str, String)> = Vec::new();
    let mut mappings: Vec<(&str, String)> = Vec::new();
    for (row, asked) in ROWS.iter().zip(&tables) {
        let case = row.case;
        let stream_arn = await_stream_arn(&router, case, asked, deadline).await;
        let table = table_of(case, &stream_arn);
        assert_eq!(
            &table, asked,
            "case {case:?}: the stream ARN {stream_arn:?} must name the table its records are \
             of, got {table:?}"
        );
        write_items(&router, case, &table).await;
        mappings.push((case, create_mapping(&router, case, &stream_arn).await));
        expected.extend(expected_markers(row, &table));
        forbidden.push((case, marker_prefix(&table, MODIFY, RESAVED_KEY)));
    }
    for (case, uuid) in &mappings {
        await_enabled(&router, case, uuid, deadline).await;
    }

    let group = format!("{DEFAULT_GROUP_PREFIX}{FUNCTION}");
    let seen = awaited_markers(&router, &group, &expected, deadline).await;
    for (case, resaved) in &forbidden {
        assert!(
            !seen.iter().any(|logged| logged.starts_with(resaved)),
            "case {case:?}: storing {RESAVED_KEY:?} with the values it already held modifies the \
             item not at all, so the stream carries no record of it and nothing opening \
             {resaved:?} may be logged, got {seen:?}"
        );
    }
    let wanted: BTreeSet<String> = expected.iter().cloned().collect();
    assert_eq!(
        seen, wanted,
        "the mappings must hand over exactly the records their streams captured, one per \
         item-level modification and with the images each view type prescribes"
    );
}
