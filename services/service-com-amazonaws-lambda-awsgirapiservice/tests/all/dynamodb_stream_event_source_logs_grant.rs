// SPDX-License-Identifier: CC0-1.0

//! What a client can OBSERVE of the functions a DynamoDB table's stream triggers when their
//! execution roles differ in the CloudWatch Logs grants they carry — end to end over the wire,
//! with DynamoDB, DynamoDB Streams, CloudWatch Logs, IAM and the token service mounted beside
//! Lambda on ONE engine.
//!
//! RULE, in two halves, both asserted over the catalog below:
//!
//! * EVERY mapping of a stream is its own consumer of it. "Lambda reads items from the event
//!   source and invokes the function" (`CreateEventSourceMapping`) holds per mapping, each with
//!   a position of its own in the stream, so one change a table's stream captures is handed to
//!   the function of EVERY `Enabled` mapping on that stream — whatever the other mappings'
//!   functions are doing and whatever their roles are allowed.
//! * WHAT THAT INVOCATION LEAVES IN CLOUDWATCH LOGS IS THE ROLE'S TO ALLOW. Lambda writes the
//!   log of a run for the FUNCTION, judged against the policies in force on its execution role,
//!   so the function's log group is created only while that role allows `logs:CreateLogGroup`,
//!   a stream of the run is opened in it only while it allows `logs:CreateLogStream`, and the
//!   run's own lines are written only while it allows `logs:PutLogEvents`. A withheld write is
//!   not an invocation failure: the records are processed and the handler's side effects land
//!   either way, and a role carrying no logs grant at all leaves NOTHING behind for a client to
//!   read — not even the log group.
//!
//! Both halves are read where a client reads them. The invocation itself is observed through
//! the item the handler writes into a results table with its OWN SDK call — a marker no
//! emulator write produces, so a row passes only if the record really reached that function's
//! handler — and the log through CloudWatch Logs' own `DescribeLogGroups`,
//! `DescribeLogStreams` and `GetLogEvents`.
//!
//! The rows differ along what the rule generalises over: WHICH of the three log writes the role
//! carries (all of them, none of them, and each proper prefix of the three, plus the write
//! alone without the creates), and so which of the group, the stream and the lines the delivery
//! leaves behind. Every row's function is mapped to the SAME stream of the SAME table and ONE
//! item is written, so the fan-out half is the one write reaching all of them.
//!
//! Nothing here waits a span of its own choosing and no assertion reads the clock. Each state
//! is waited for by looking again until THAT state holds, against one deadline the single write
//! opens and every wait after it shares ([`CHAIN_BUDGET`]). The absence assertions are made only
//! once the row's mapping reports the attempt that consumed the record: Lambda delivers a run's
//! log BEFORE the invocation is answered and the mapping reports the attempt only after that
//! answer, so a group that is still absent then is one no delivery was going to create.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, given_mapping as create_stream_mapping, given_table, results_until, write_item, Write,
    KEY, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};
use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{
    await_function_active, await_mapping_state, call, router_on, LOOK_AGAIN, STATE_ENABLED,
};
use crate::invocation_log_delivery::{events, groups, streams, LOGS, LOG_WRITES};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every ARN below carries, which is the Region the harness signs for.
const REGION: &str = "us-east-1";

/// The IAM action namespace Amazon DynamoDB's calls are named under. CloudWatch Logs' own is
/// [`LOGS`], read from the suite that first needed it rather than spelled again here.
const DYNAMODB: &str = "dynamodb";

/// The default group a function's invocations are logged to: "By default, Lambda functions send
/// logs to a default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The calls CONSUMING a DynamoDB stream takes, which every role below is granted on the
/// mapped stream so that the mapping is wired at all and its poller may read
/// (`crate::event_source_mapping_least_privilege_role`).
const CONSUMING_CALLS: [&str; 3] = ["DescribeStream", "GetShardIterator", "GetRecords"];

/// The one write each handler makes with its own SDK client, granted on the results table so
/// that the side effect of a delivered record can land.
const PUT_ITEM: [&str; 1] = ["PutItem"];

/// The runtime every function below runs, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one run of a handler below is given, in the seconds `Timeout` is stated in.
/// Generous: no row here is about that bound, and a test binary runs its suites together.
const FUNCTION_TIMEOUT: i64 = 30;

/// How long the chain the ONE write starts is given for every state the clock owes it together
/// — each mapping's poller taking the record, the invocations running, their logs reaching
/// CloudWatch Logs and each mapping reporting the attempt that consumed the record.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged, and the waits SHARE
/// this one deadline rather than each taking a span of its own. The bound is taken from what the
/// emulator itself allows ONE invocation — the wait for a share of the run capacity
/// ([`PERMIT_WAIT`]) and the wait for the run's own answer ([`HOST_WAIT_CEILING`]) — with room
/// on top, because a whole-workspace run shares its cores with every other suite.
const CHAIN_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// What the handler prints for each record it was handed. ONE spelling of it, bound into the
/// handler's source and into what the assertions look for.
const MARKER: &str = "stream record: ";

/// The item every row's write makes a stream record of.
const ITEM: &str = "item-0";

/// How many records one batch of the mappings below holds, so the one record written is a batch
/// and an invocation of its own for every one of them.
const ONE_RECORD_PER_BATCH: i64 = 1;

/// One row: which of the three log writes the role carries, and what the delivery of its
/// function's run may then leave behind.
struct Row {
    case: &'static str,
    /// The actions of [`LOG_WRITES`] the role's policy allows, in any order.
    granted: &'static [&'static str],
    /// Whether the function's log group must exist afterwards.
    group: bool,
    /// Whether a stream of the run must be in it.
    stream: bool,
    /// Whether the run's own lines must be in that stream.
    lines: bool,
}

/// The catalog: the three log writes ([`LOG_WRITES`], whose order is the order the delivery
/// makes them in) granted in full, withheld in full, in each proper prefix of them, and as the
/// WRITE alone — a grant that can leave nothing behind, because the group it would write to is
/// never created.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every log write granted",
            granted: &LOG_WRITES,
            group: true,
            stream: true,
            lines: true,
        },
        Row {
            case: "no log write granted",
            granted: &[],
            group: false,
            stream: false,
            lines: false,
        },
        Row {
            case: "the group and the stream granted, the write withheld",
            granted: &LOG_WRITES[..2],
            group: true,
            stream: true,
            lines: false,
        },
        Row {
            case: "the group alone granted",
            granted: &LOG_WRITES[..1],
            group: true,
            stream: false,
            lines: false,
        },
        Row {
            case: "the write alone granted",
            granted: &LOG_WRITES[2..],
            group: false,
            stream: false,
            lines: false,
        },
    ]
}

/// Lambda, DynamoDB, DynamoDB Streams, CloudWatch Logs, IAM and the token service over ONE
/// shared engine, the shape `core/router/src/lib.rs` mounts them in: the stream is the one
/// DynamoDB publishes, the roles are the records IAM holds, the handler's own SDK call is
/// dispatched back into this router under a session of its execution role, and the log is read
/// where a client reads it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
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

/// The handler every function below runs: it PRINTS one line per record it was handed — which
/// is what a delivered log has to carry — and then writes one item into the results table with
/// its own SDK client, keyed by the function that processed the record, which is what says the
/// record reached this function at all.
fn recording_handler() -> String {
    format!(
        r#"
import os

import boto3

OUT_TABLE = os.environ["OUT_TABLE"]
LABEL = os.environ["LABEL"]
KEY = "{KEY}"
MARKER = "{MARKER}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    client = boto3.client("dynamodb")
    for record in records:
        key = record["dynamodb"]["Keys"][KEY]["S"]
        print(MARKER + key)
        client.put_item(
            TableName=OUT_TABLE,
            Item={{KEY: {{"S": LABEL + "/" + key}}}},
        )
    return {{"records": len(records)}}
"#
    )
}

/// The line the handler prints for the record of the item keyed `key`.
fn expected_line(key: &str) -> String {
    format!("{MARKER}{key}")
}

/// The marker item the function `label` writes for the record of the item keyed `key`.
fn marker_of(label: &str, key: &str) -> String {
    format!("{label}/{key}")
}

/// The ARN of one of this emulator's tables, as DynamoDB itself mints it — the resource a grant
/// about that table names.
fn table_arn(name: &str) -> String {
    service_com_amazonaws_dynamodb_dynamodb_20120810::state::table_arn(REGION, name)
}

/// The inline policy a row's role carries: the calls consuming the mapped stream, the one write
/// its handler makes, and the log writes the row grants.
fn policy(row: &Row, stream_arn: &str, results_arn: &str) -> Value {
    let mut statements = vec![
        allow(DYNAMODB, &CONSUMING_CALLS, stream_arn),
        allow(DYNAMODB, &PUT_ITEM, results_arn),
    ];
    if !row.granted.is_empty() {
        // Granted on every log resource, the way AWS's own basic-execution policy grants them,
        // so the row withholds an ACTION rather than a resource.
        statements.push(allow(LOGS, row.granted, "*"));
    }
    json!(statements)
}

/// A function of `name` running [`recording_handler`] as `role_arn`, told where to write its
/// marker, waited out to `Active`.
///
/// A creator of its own rather than the stream suite's
/// ([`crate::dynamodb_stream_event_source_delivery::given_function`]): every row here runs as a
/// ROLE of its own, which is the whole subject of this suite, while that fixture creates every
/// function under one role nothing is known about.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    results: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "OUT_TABLE": results, "LABEL": name } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(&recording_handler())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// The mapping between `function` and `stream_arn`, settled into `Enabled`, answered as its
/// UUID: the stream suite's own create — so the two cannot drift about what wiring a table's
/// stream to a function is — followed by the wait for the state its `Enabled` setting asks for.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
) -> String {
    let uuid = create_stream_mapping(
        router,
        case,
        function,
        stream_arn,
        TRIM_HORIZON,
        ONE_RECORD_PER_BATCH,
        false,
    )
    .await;
    await_mapping_state(router, case, &uuid, STATE_ENABLED).await;
    uuid
}

/// The keys the results table holds, which are the markers the handlers wrote.
async fn markers(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<String> {
    let (status, scanned) = dynamodb(router, "Scan", &json!({ "TableName": table })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Scan answered {status} {scanned}"
    );
    scanned["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.pointer(&format!("/{KEY}/S"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until the results table holds the marker `wanted`, answering what it held last.
async fn marker_written(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    wanted: &str,
    deadline: Instant,
) -> Vec<String> {
    loop {
        let held = markers(router, case, table).await;
        if held.iter().any(|marker| marker == wanted) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every log line the group `group` holds, across every stream of it, as a client reads them.
async fn logged_lines(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for stream in streams(router, case, group).await {
        for (_, message) in events(router, case, group, &stream).await {
            lines.push(message);
        }
    }
    lines
}

/// Wait until the group `group` holds `wanted`, answering what it held last.
async fn logged(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: &str,
    deadline: Instant,
) -> Vec<String> {
    loop {
        let lines = logged_lines(router, case, group).await;
        if lines.iter().any(|line| line.contains(wanted)) || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: one change a table's stream captures is handed to the function of every mapping on
/// that stream, and what each of those invocations leaves in CloudWatch Logs is exactly what
/// its own execution role allows.
#[tokio::test]
async fn every_mapping_of_one_stream_is_delivered_and_logs_only_what_its_role_allows() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a stream mapped to functions whose roles differ in their log grants";
    let router = emulator();
    let source = "ddb-logs-grant-source";
    let results = "ddb-logs-grant-results";
    let stream_arn = given_table(&router, case, source, Some(NEW_AND_OLD_IMAGES)).await;
    assert!(
        !stream_arn.is_empty(),
        "case {case:?}: the table created with a stream must report its LatestStreamArn"
    );
    given_table(&router, case, results, None).await;
    let results_arn = table_arn(results);

    // Every row's function, each running as a role of its own, all mapped to the ONE stream.
    let rows = rows();
    let mut mapped: Vec<(String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let function = format!("ddb-logs-grant-fn-{index}");
        let role = given_role(
            &router,
            row.case,
            &format!("ddb-logs-grant-role-{index}"),
            Some(policy(row, &stream_arn, &results_arn)),
        )
        .await;
        given_function(&router, row.case, &function, &role, results).await;
        let uuid = given_mapping(&router, row.case, &function, &stream_arn).await;
        mapped.push((function, uuid));
    }

    // The ONE client action. Everything after it is the mappings' own work, and the patience
    // below is for THAT.
    write_item(&router, case, source, ITEM, Write::Insert).await;
    let deadline = Instant::now() + CHAIN_BUDGET;

    for (row, (function, uuid)) in rows.iter().zip(mapped.iter()) {
        let case = row.case;
        // The fan-out half: this mapping's function processed the one record written, which the
        // marker its handler wrote with its own SDK call is the evidence of.
        let wanted = marker_of(function, ITEM);
        let held = marker_written(&router, case, results, &wanted, deadline).await;
        assert!(
            held.iter().any(|marker| marker == &wanted),
            "case {case:?}: the record must reach {function:?}, so {wanted:?} must be in the \
             results table, got {held:?}"
        );
        // And the mapping reports the attempt that consumed it. That report is made after the
        // invocation was answered, and a run's log is delivered BEFORE it is answered, so from
        // here on what the group holds is all the delivery was ever going to put there.
        let reported = results_until(&router, case, uuid, LAST_PROCESSING_RESULT_OK, deadline).await;
        assert_eq!(
            reported.last().map(String::as_str),
            Some(LAST_PROCESSING_RESULT_OK),
            "case {case:?}: the mapping must report the attempt that consumed its record as \
             {LAST_PROCESSING_RESULT_OK}, saw {reported:?}"
        );

        let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
        let line = expected_line(ITEM);
        let lines = if row.lines {
            logged(&router, case, &group, &line, deadline).await
        } else {
            logged_lines(&router, case, &group).await
        };
        let named = groups(&router, case, &group).await;
        assert_eq!(
            named.iter().any(|found| found == &group),
            row.group,
            "case {case:?}: expected the group {group:?} to exist: {}, got {named:?}",
            row.group
        );
        let opened = streams(&router, case, &group).await;
        assert_eq!(
            !opened.is_empty(),
            row.stream,
            "case {case:?}: expected a stream of the run in {group:?}: {}, got {opened:?}",
            row.stream
        );
        if row.lines {
            assert!(
                lines.iter().any(|written| written.contains(&line)),
                "case {case:?}: the handler's line {line:?} must be delivered to {group:?}, got \
                 {lines:?}"
            );
        } else {
            assert!(
                lines.is_empty(),
                "case {case:?}: a role short of the write must leave no line in {group:?}, got \
                 {lines:?}"
            );
        }
    }
}
