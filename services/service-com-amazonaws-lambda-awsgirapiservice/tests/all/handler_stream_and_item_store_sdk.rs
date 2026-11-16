// SPDX-License-Identifier: CC0-1.0

//! The pipeline a DIRECTLY invoked handler drives with the AWS SDK its managed runtime
//! supplies, end to end over the wire: Lambda, Amazon Kinesis, Amazon DynamoDB, IAM and the
//! token service on ONE engine, a `python3.x` function whose module-scope code does
//! `import boto3`, and every item and every record its calls left behind read back where a
//! client reads them.
//!
//! RULE: a managed runtime puts the AWS SDK on the handler's import path, and a client the
//! handler builds from it calls the addressed service with temporary credentials of the
//! FUNCTION's execution role — "the execution role grants the function permission to use
//! Amazon Web Services services". Each such call is therefore decided by that service's own
//! authorization, against the ONE action the call asks for and the ONE resource it addresses:
//! served when a document in force on the role allows that action on that resource, and
//! refused with the service's own access-denied error otherwise — so a grant of a sibling
//! action, or of the same action on another resource of the same service, grants this call
//! nothing. Whatever the addressed service answered, its output members or its refusal,
//! reaches the handler as its own value or as its caught `botocore.exceptions.ClientError`;
//! none of it is a failure of the runtime, so a run whose module imported the SDK reports no
//! `FunctionError` at all, whichever way its calls were decided.
//!
//! The groups differ along what the rule generalises over: WHICH service the client is built
//! for (a record stream and an item store, two services whose grants are read under different
//! action namespaces), WHICH action is called (a record put onto a stream, an item write, an
//! item read), and HOW the role withholds it (no statement about the service at all, a
//! statement for a sibling action of it, and a statement for the same action on another
//! resource of it). The item store's write and read run in the same invocation as the stream
//! put, which is the stage shape of a stream pipeline: one run reads and writes items and
//! hands its work to the next stage as a record.
//!
//! Each group's steps run in ONE invocation, because an invocation is the dearest thing this
//! target does, and the last assertions read everything back through Kinesis's and DynamoDB's
//! OWN APIs: a step that reported a put without doing the work fails there, and so does a
//! refusal that wrote something anyway. The placement a served put reported is checked against
//! the record the shard really holds, so the shard id and sequence number the handler read back
//! are the ones the stream minted.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The DynamoDB wire call and the key attribute a table is keyed on are the ones the
// stream-delivery suite publishes, and the table fixture answering the ARN DynamoDB persisted
// is the bundled-SDK suite's — reached rather than written a second time here.
use crate::dynamodb_stream_event_source_delivery::{dynamodb, KEY};
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
use crate::kinesis_event_source_delivery::{given_stream, kinesis};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;
use crate::python_runtime_bundled_sdk::given_table_arn;

/// Where `CreateFunction` is bound, and the prefix every function request below is built on.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below runs, and the module and attribute its package carries its
/// handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The table every item below lives in.
const TABLE: &str = "stage-ledger";

/// The stream every record below is put onto, and another stream of the same account, which is
/// what one group's grant names instead so that the same action is granted on a resource the
/// call does not address.
const STREAM: &str = "stage-handoff";
/// See [`STREAM`].
const OTHER_STREAM: &str = "stage-elsewhere";

/// How many shards each stream is created with: one, so the records put onto it are the records
/// of one shard and the placement a put reported is checked against that shard's own.
const ONE_SHARD: i64 = 1;

/// The attribute every item below carries beside its key. It is not read by this emulator: it
/// is the handler's own, and what is asserted is that it round-trips.
const VALUE_ATTRIBUTE: &str = "stage";

/// The bytes one record carries, which are no text at all — a record's `Data` is a blob, so
/// what is asserted is that the bytes the handler put are the bytes the shard holds. The event
/// hands them to the handler base64-encoded and the handler decodes them, the way an SDK caller
/// passes bytes.
const RECORD_DATA: &[u8] = &[0x00, 0xff, 0x10, b'o', b'k'];

/// The error code and status each addressed service refuses an unauthorized request with:
/// DynamoDB's `AccessDeniedException` and Kinesis's, which its model declares with no
/// `@httpError` of its own and therefore at the client default
/// (`smithy_query(query="shape", shape="AccessDeniedException")`).
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 400;

/// The IAM actions of the three operations the groups' handlers call, each in the namespace the
/// owning service publishes its actions under (its `arnNamespace`,
/// `smithy_query(query="service_info")`).
const PUT_ITEM: &str = "dynamodb:PutItem";
/// See [`PUT_ITEM`].
const GET_ITEM: &str = "dynamodb:GetItem";
/// See [`PUT_ITEM`].
const PUT_RECORD: &str = "kinesis:PutRecord";

/// Lambda, Amazon Kinesis, Amazon DynamoDB, IAM and the token service over ONE shared engine,
/// as `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the stream and the table it reaches are the ones this suite reads, and
/// the role its credentials are minted of is the record IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One DynamoDB call that must be served, over the shared wire helper. The credential this
/// harness signs with is the account's own, so these are the OWNER's reads rather than a role's.
async fn dynamodb_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = dynamodb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DynamoDB {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// The same for one Kinesis call.
async fn kinesis_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = kinesis(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Kinesis {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// The handler every group runs: it imports the SDK and builds BOTH its clients at MODULE
/// scope, so a runtime shipping no SDK fails the initialization phase and no step runs at all;
/// then for each step the event names it calls that operation on the named client and reports
/// what came back — the service's own answer, or the refusal it raised as the SDK's own error
/// class carries it.
///
/// A step whose arguments carry `Data` carries it base64-encoded, and the handler decodes it to
/// the bytes the call is made with: the record's payload is a blob.
const HANDLER_SOURCE: &str = r#"
import base64

import boto3
from botocore.exceptions import ClientError

CLIENTS = {"dynamodb": boto3.client("dynamodb"), "kinesis": boto3.client("kinesis")}


def lambda_handler(event, context):
    results = []
    for step in event["steps"]:
        arguments = dict(step["arguments"])
        if "Data" in arguments:
            arguments["Data"] = base64.b64decode(arguments["Data"])
        try:
            answered = getattr(CLIENTS[step["client"]], step["operation"])(**arguments)
        except ClientError as error:
            results.append({
                "ok": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            })
            continue
        read = {"ok": True, "status": answered["ResponseMetadata"]["HTTPStatusCode"]}
        if "Item" in answered:
            read["item"] = answered["Item"]
        if "ShardId" in answered:
            read["placed"] = {
                "shard": answered["ShardId"],
                "sequence": answered["SequenceNumber"],
            }
        results.append(read)
    return {"results": results}
"#;

/// The role one group's function runs as.
fn role_of(slug: &str) -> String {
    format!("stage-{slug}")
}

/// The function one group invokes.
fn function_of(slug: &str) -> String {
    format!("stage-{slug}-caller")
}

/// The assumed-role session an SDK call made by an invocation of that group's function speaks
/// for: the session is named for the FUNCTION, and it is the principal a refusal names.
fn session_of(slug: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{}/{}",
        engine::emulator_account_id(),
        role_of(slug),
        function_of(slug)
    )
}

/// The partition one group's item lives under, so one group's item is its own.
fn partition(slug: &str) -> String {
    format!("{slug}-item")
}

/// The partition key one group's record is put under.
fn record_key(slug: &str) -> String {
    format!("{slug}-record")
}

/// The item a handler writes, as the wire carries attribute values.
fn item(slug: &str) -> Value {
    json!({ KEY: { "S": partition(slug) }, VALUE_ATTRIBUTE: { "S": slug } })
}

/// The input of a write of that item, of a read keyed on it, and of the record put.
fn put_item_input(slug: &str) -> Value {
    json!({ "TableName": TABLE, "Item": item(slug) })
}
/// See [`put_item_input`].
fn get_item_input(slug: &str) -> Value {
    json!({ "TableName": TABLE, "Key": { KEY: { "S": partition(slug) } } })
}
/// See [`put_item_input`].
fn put_record_input(slug: &str) -> Value {
    json!({
        "StreamName": STREAM,
        "PartitionKey": record_key(slug),
        "Data": base64::engine::general_purpose::STANDARD.encode(RECORD_DATA),
    })
}

/// What a served write must come to, what a served read of the item it wrote must come to, and
/// what a served record put must come to — the placement it reported is taken off the step and
/// checked against the shard's own record.
fn wrote() -> Value {
    json!({ "ok": true, "status": 200 })
}
/// See [`wrote`].
fn read_item(slug: &str) -> Value {
    json!({ "ok": true, "status": 200, "item": item(slug) })
}

/// The refusal the addressed service words when nothing in force permits the calling session
/// the action: its own error code and status, over the sentence that names who was refused,
/// which action on which resource, and the reason the refusal rests on.
fn refused(slug: &str, action: &str, resource: &str) -> Value {
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "status": ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} \
             because no identity-based policy allows the {action} action",
            session_of(slug)
        ),
    })
}

/// One step of a group's invocation: the client it is made on, the call the handler makes, and
/// what the SDK must have given the guest for it.
struct Step {
    case: &'static str,
    /// The service token the client was built with, which names the service addressed.
    client: &'static str,
    /// The client method, which names the operation.
    operation: &'static str,
    /// The keyword arguments it is called with: the operation's own input members.
    arguments: Value,
    /// What the handler must have read back, beside the placement a served put reports.
    expected: Value,
}

/// One group: the statements of its function's execution role, and the steps one invocation of
/// that function makes.
struct Group {
    case: &'static str,
    /// What this group's role, function, partition and record are named after.
    slug: &'static str,
    /// The statements of the role's one inline identity policy.
    statements: Value,
    steps: Vec<Step>,
    /// Whether this group's record put was granted, so the stream must hold its record.
    puts_record: bool,
    /// Whether this group's item write was granted, so the table must hold its item.
    writes_item: bool,
}

/// The groups. Every action named in a grant and in a refusal is the IAM action of the
/// operation the step calls, and every resource is the stream or the table that call addressed.
fn groups(table_arn: &str, stream_arn: &str, other_stream_arn: &str) -> Vec<Group> {
    let granted = {
        let slug = "granted";
        Group {
            case: "a role granted the item write, the item read and the record put",
            slug,
            statements: json!([
                allow("dynamodb", &["PutItem", "GetItem"], table_arn),
                allow("kinesis", &["PutRecord"], stream_arn),
                logs_only(),
            ]),
            steps: vec![
                Step {
                    case: "the item write the role allows",
                    client: "dynamodb",
                    operation: "put_item",
                    arguments: put_item_input(slug),
                    expected: wrote(),
                },
                Step {
                    case: "the item read the role allows",
                    client: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(slug),
                    expected: read_item(slug),
                },
                Step {
                    case: "the record put the role allows",
                    client: "kinesis",
                    operation: "put_record",
                    arguments: put_record_input(slug),
                    expected: wrote(),
                },
            ],
            puts_record: true,
            writes_item: true,
        }
    };
    let ungranted = {
        let slug = "ungranted";
        Group {
            case: "a role whose policy says nothing about either service",
            slug,
            statements: json!([logs_only()]),
            steps: vec![
                Step {
                    case: "an item write nothing in force allows",
                    client: "dynamodb",
                    operation: "put_item",
                    arguments: put_item_input(slug),
                    expected: refused(slug, PUT_ITEM, table_arn),
                },
                Step {
                    case: "an item read nothing in force allows",
                    client: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(slug),
                    expected: refused(slug, GET_ITEM, table_arn),
                },
                Step {
                    case: "a record put nothing in force allows",
                    client: "kinesis",
                    operation: "put_record",
                    arguments: put_record_input(slug),
                    expected: refused(slug, PUT_RECORD, stream_arn),
                },
            ],
            puts_record: false,
            writes_item: false,
        }
    };
    let elsewhere = {
        let slug = "elsewhere";
        Group {
            case: "a role granted one action of each service, on the sibling action and on \
                   another stream",
            slug,
            statements: json!([
                allow("dynamodb", &["PutItem"], table_arn),
                allow("kinesis", &["PutRecord"], other_stream_arn),
                logs_only(),
            ]),
            steps: vec![
                Step {
                    case: "the one item action the role allows",
                    client: "dynamodb",
                    operation: "put_item",
                    arguments: put_item_input(slug),
                    expected: wrote(),
                },
                Step {
                    case: "the sibling item action, which that grant does not cover",
                    client: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(slug),
                    expected: refused(slug, GET_ITEM, table_arn),
                },
                Step {
                    case: "the record put, granted on a stream this call does not address",
                    client: "kinesis",
                    operation: "put_record",
                    arguments: put_record_input(slug),
                    expected: refused(slug, PUT_RECORD, stream_arn),
                },
            ],
            puts_record: false,
            writes_item: true,
        }
    };
    vec![granted, ungranted, elsewhere]
}

/// Create the function of one group, running the one handler as `role_arn`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
}

/// The one shard of the stream `name`, and every record that shard holds, read from its trim
/// horizon the way a consumer reads them.
async fn shard_records(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, Vec<Value>) {
    let listed = kinesis_ok(router, case, "ListShards", &json!({ "StreamName": name })).await;
    let shard = listed["Shards"][0]["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no shard in {listed}"))
        .to_string();
    let iterator = kinesis_ok(
        router,
        case,
        "GetShardIterator",
        &json!({
            "StreamName": name,
            "ShardId": shard,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    let read = kinesis_ok(
        router,
        case,
        "GetRecords",
        &json!({ "ShardIterator": iterator["ShardIterator"] }),
    )
    .await;
    let records = read["Records"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no Records in {read}"));
    (shard, records)
}

/// What a handler's own SDK calls may do is decided per action and per resource by the policies
/// in force on its execution role, whichever service it addresses — and what that service
/// answered, served or refused, is what the handler read back.
#[tokio::test]
async fn the_execution_roles_grants_decide_each_service_a_handler_reaches() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a python handler's own stream and item store calls";
    let router = emulator();

    let table_arn = given_table_arn(&router, case, TABLE).await;
    let stream_arn = given_stream(&router, case, STREAM, ONE_SHARD).await;
    let other_stream_arn = given_stream(&router, case, OTHER_STREAM, ONE_SHARD).await;
    let groups = groups(&table_arn, &stream_arn, &other_stream_arn);

    let mut role_arns: Vec<String> = Vec::with_capacity(groups.len());
    for group in &groups {
        role_arns.push(
            given_role(
                &router,
                group.case,
                &role_of(group.slug),
                Some(group.statements.clone()),
            )
            .await,
        );
    }

    // Every function first, so the provisioning spans the clock owes run down together rather
    // than one after another.
    for (group, role_arn) in groups.iter().zip(&role_arns) {
        given_function(&router, group.case, &function_of(group.slug), role_arn).await;
    }
    for group in &groups {
        await_function_active(&router, group.case, &function_of(group.slug)).await;
    }

    // What each group's run reported, and the placement its served record put read back.
    let mut placements: Vec<(String, Value)> = Vec::new();
    for group in &groups {
        let steps: Vec<Value> = group
            .steps
            .iter()
            .map(|step| {
                json!({
                    "client": step.client,
                    "operation": step.operation,
                    "arguments": step.arguments,
                })
            })
            .collect();
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", function_of(group.slug)),
            &json!({ "steps": steps }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected HTTP 200 from Invoke, got {} {}",
            group.case,
            answered.status,
            answered.text()
        );
        let payload = answered.json();
        // The handler RAN: a runtime that could not import the SDK fails the initialization
        // phase, which Invoke reports as a function error instead of a result.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {:?}: expected the handler's own result, got a function error with {payload}",
            group.case
        );
        assert_eq!(
            payload["results"].as_array().map(Vec::len),
            Some(group.steps.len()),
            "case {:?}: expected one result per step, got {payload}",
            group.case
        );
        for (index, step) in group.steps.iter().enumerate() {
            let mut actual = payload["results"][index].clone();
            let placed = actual
                .as_object_mut()
                .and_then(|reported| reported.remove("placed"));
            assert_eq!(
                actual, step.expected,
                "case {:?} / {:?}: expected {}, got {actual} (the whole run: {payload})",
                group.case, step.case, step.expected
            );
            if let Some(placed) = placed {
                placements.push((record_key(group.slug), placed));
            }
        }
    }

    // What DynamoDB ITSELF holds, read where a client reads it: every partition a served write
    // wrote, and every partition a refused write must have left alone.
    for group in &groups {
        let read = dynamodb_ok(&router, group.case, "GetItem", &get_item_input(group.slug)).await;
        if group.writes_item {
            assert_eq!(
                read["Item"],
                item(group.slug),
                "case {:?}: expected DynamoDB to hold the item the handler wrote, got {read}",
                group.case
            );
        } else {
            assert_eq!(
                read.get("Item"),
                None,
                "case {:?}: expected the refused write to have stored nothing, got {read}",
                group.case
            );
        }
    }

    // What the STREAM itself holds, read where a client reads it: one record per group whose
    // put was served, under that group's own partition key and carrying the exact bytes it put,
    // and nothing from a group whose put was refused. The stream the one mis-scoped grant named
    // instead was addressed by nobody, so it is empty.
    let expected_data = base64::engine::general_purpose::STANDARD.encode(RECORD_DATA);
    let (shard, records) = shard_records(&router, case, STREAM).await;
    let mut minted: BTreeMap<String, Value> = BTreeMap::new();
    for record in &records {
        assert_eq!(
            record["Data"].as_str(),
            Some(expected_data.as_str()),
            "case {case:?}: expected the record to carry the bytes the handler put, got {record}"
        );
        let key = record["PartitionKey"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no PartitionKey in {record}"))
            .to_string();
        minted.insert(
            key,
            json!({ "shard": shard, "sequence": record["SequenceNumber"] }),
        );
    }
    let held: BTreeSet<String> = minted.keys().cloned().collect();
    let expected_keys: BTreeSet<String> = groups
        .iter()
        .filter(|group| group.puts_record)
        .map(|group| record_key(group.slug))
        .collect();
    assert_eq!(
        held, expected_keys,
        "case {case:?}: expected the shard to hold the records the served puts made, got \
         {records:?}"
    );
    // The placement each served put reported is the one the stream minted for that record.
    for (key, placed) in &placements {
        assert_eq!(
            minted.get(key),
            Some(placed),
            "case {case:?} / {key:?}: expected the served put to have reported the placement the \
             stream minted, got {placed}"
        );
    }

    let (_other_shard, elsewhere) = shard_records(&router, case, OTHER_STREAM).await;
    assert!(
        elsewhere.is_empty(),
        "case {case:?}: expected the stream no call addressed to hold nothing, got \
         {elsewhere:?}"
    );
}
