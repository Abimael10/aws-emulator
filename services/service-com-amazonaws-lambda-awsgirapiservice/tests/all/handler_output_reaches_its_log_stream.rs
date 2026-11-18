// SPDX-License-Identifier: CC0-1.0

//! What the HANDLER itself wrote, read back out of the function's CloudWatch Logs stream.
//!
//! RULE: everything a handler writes to its standard streams while an invocation runs is
//! delivered to the function's log group — the one its `LoggingConfig$LogGroup` names, else the
//! default `/aws/lambda/<function name>` ("Lambda functions send logs to a default log group
//! named `/aws/lambda/<function name>`",
//! `smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`) — as one log event per
//! line, in the order the run wrote them and between the runtime's own opening `START
//! RequestId:` frame and its closing `END`/`REPORT RequestId:` frames. HOW the handler wrote a
//! line does not change that (a `print`, a write straight to the standard error stream, a
//! record emitted through the language's logging facility, one write carrying a line break),
//! and neither does WHEN in the run it wrote it — a diagnostic a handler prints after an SDK
//! call another service refused is delivered exactly as one printed before any call is.
//!
//! This is the whole debugging channel a client has for its own function: an assertion over
//! what the function logged (an error diagnostic, a structured record, a metric line) can only
//! be made on records that are really delivered, so a run whose stream carries only the three
//! framing lines is indistinguishable from a handler that logged nothing.
//!
//! Three services have to agree for the rows below to hold — Lambda runs the handler and
//! delivers the run's log, Amazon DynamoDB answers the item call two of the handlers make, and
//! CloudWatch Logs holds the group and the stream a client reads — so each row drives real HTTP
//! through a `MultiServiceRouter` with the three mounted on ONE engine, and asserts only what
//! `DescribeLogGroups`, `DescribeLogStreams` and `GetLogEvents` report.
//!
//! The rows differ along what the rule generalises over: WHICH stream the handler wrote to
//! (standard output, standard error, the logging facility), HOW MANY lines the run left and how
//! it left them (one closed write, three of them, a write it never closed with a break, none at
//! all), and WHEN in the run the write happened (straight away, after an item call the service
//! REFUSED, and after one it SERVED).
//! What the neighbouring suites pin and this one does not repeat:
//! [`crate::invocation_log_delivery`] pins where a run's log goes and which of the three log
//! writes the execution role allows, and [`crate::invocation_log_record_format`] pins the FORM
//! each record takes under a function's `LogFormat`.
//!
//! Every test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invocation_log_delivery::{
    create_function, delivered, groups, invoke, streams, Created, PYTHON_RUNTIME,
};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN no IAM store here holds, so nothing is known about the
/// role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The prefix of the log group a function whose configuration names none logs to.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The framing the runtime writes around every run, which is what the handler's own lines sit
/// between.
const START_PREFIX: &str = "START RequestId: ";
const END_PREFIX: &str = "END RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// How many lines of framing one run leaves: the opening frame and the two closing ones.
const FRAMING_LINES: usize = 3;

/// The table one handler reads, its one key attribute, the key it reads and the value stored
/// under it.
const TABLE: &str = "handler-output-probe";
const KEY_ATTRIBUTE: &str = "pk";
const ITEM_KEY: &str = "probe";
const STORED_NOTE: &str = "stored-note";

/// A table name nothing here creates, which the refused row's handler addresses, and the error
/// code DynamoDB refuses an item call against a table that does not exist with
/// (`smithy_query(query="operation",operation="GetItem")` declares `ResourceNotFoundException`
/// among its errors).
const MISSING_TABLE: &str = "handler-output-absent";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// How long this suite waits for the table it created to become `ACTIVE`, and how often it
/// looks again: the transition is one the clock owes, so the suite waits for the CONDITION. The
/// budget is far longer than the span that service chooses, so a loaded host waits longer
/// rather than failing.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// Lambda, CloudWatch Logs and Amazon DynamoDB over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own item call and the delivery of the
/// run's log are both dispatched back into this same router.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// One Amazon DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer
/// 200 — the setup calls this suite makes as a client.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/dynamodb/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The table the served row's handler reads, waited out to `ACTIVE` — "you can perform read and
/// write operations only on an ACTIVE table" — holding the one item that row reports.
async fn given_table(router: &MultiServiceRouter, case: &str) {
    dynamodb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb(router, case, "DescribeTable", &json!({"TableName": TABLE})).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    dynamodb(
        router,
        case,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {
                KEY_ATTRIBUTE: {"S": ITEM_KEY},
                "note": {"S": STORED_NOTE},
            },
        }),
    )
    .await;
}

/// One row: the handler the row runs, and the lines the HANDLER itself must have contributed to
/// the stream, in the order it wrote them.
struct Row {
    case: &'static str,
    /// The handler's own source, which decides what the run writes.
    source: String,
    /// What each of the handler's own events must carry, in order. A row asserts a line
    /// CONTAINS its text, because a record emitted through the logging facility is delivered
    /// framed by the runtime that rendered it.
    expected: Vec<String>,
}

/// The rows, each one way a handler leaves output behind.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one line printed to standard output",
            source: "def lambda_handler(event, context):\n    print(\"probe-one-line\")\n    \
                     return {\"ok\": True}\n"
                .to_string(),
            expected: vec!["probe-one-line".to_string()],
        },
        Row {
            case: "three lines printed in the order they were written",
            source: "def lambda_handler(event, context):\n    for index in range(3):\n        \
                     print(\"probe-line-%d\" % index)\n    return {\"ok\": True}\n"
                .to_string(),
            expected: vec![
                "probe-line-0".to_string(),
                "probe-line-1".to_string(),
                "probe-line-2".to_string(),
            ],
        },
        Row {
            case: "a write the handler never closed with a line break",
            source: "import sys\n\n\ndef lambda_handler(event, context):\n    \
                     sys.stdout.write(\"probe-unclosed\")\n    return {\"ok\": True}\n"
                .to_string(),
            expected: vec!["probe-unclosed".to_string()],
        },
        Row {
            case: "a line written straight to the standard error stream",
            source: "import sys\n\n\ndef lambda_handler(event, context):\n    \
                     sys.stderr.write(\"probe-on-stderr\\n\")\n    return {\"ok\": True}\n"
                .to_string(),
            expected: vec!["probe-on-stderr".to_string()],
        },
        Row {
            case: "a record emitted through the logging facility",
            source: "import logging\n\n\ndef lambda_handler(event, context):\n    \
                     logging.getLogger().error(\"probe-through-logging\")\n    return {\"ok\": \
                     True}\n"
                .to_string(),
            expected: vec!["probe-through-logging".to_string()],
        },
        Row {
            case: "a diagnostic printed after an item call the service refused",
            source: format!(
                "import boto3\nfrom botocore.exceptions import ClientError\n\n\ndef \
                 lambda_handler(event, context):\n    client = boto3.client(\"dynamodb\")\n    \
                 try:\n        client.get_item(TableName=\"{MISSING_TABLE}\", \
                 Key={{\"{KEY_ATTRIBUTE}\": {{\"S\": \"{ITEM_KEY}\"}}}})\n    except ClientError \
                 as error:\n        print(\"probe-refused \" + \
                 error.response[\"Error\"][\"Code\"])\n        return {{\"ok\": True}}\n    \
                 return {{\"ok\": False}}\n"
            ),
            expected: vec![format!("probe-refused {NOT_FOUND}")],
        },
        Row {
            case: "a diagnostic printed after an item call the service served",
            source: format!(
                "import boto3\n\n\ndef lambda_handler(event, context):\n    client = \
                 boto3.client(\"dynamodb\")\n    answered = \
                 client.get_item(TableName=\"{TABLE}\", Key={{\"{KEY_ATTRIBUTE}\": {{\"S\": \
                 \"{ITEM_KEY}\"}}}})\n    print(\"probe-read \" + \
                 answered[\"Item\"][\"note\"][\"S\"])\n    return {{\"ok\": True}}\n"
            ),
            expected: vec![format!("probe-read {STORED_NOTE}")],
        },
        Row {
            case: "a handler that writes nothing, which leaves the framing alone",
            source: "def lambda_handler(event, context):\n    return {\"ok\": True}\n".to_string(),
            expected: Vec::new(),
        },
    ]
}

/// Where the line starting with `prefix` sits among `lines`.
fn framed_at(lines: &[String], prefix: &str) -> Option<usize> {
    lines.iter().position(|line| line.starts_with(prefix))
}

/// Whatever the handler wrote, every line of it is an event of the run's stream, in the order
/// it was written and between the runtime's own frames.
#[tokio::test]
async fn a_handlers_own_output_reaches_its_log_stream_between_the_runtimes_frames() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    given_table(&router, "the table the served row reads").await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("handler-output-{index}");
        let group = format!("{DEFAULT_GROUP_PREFIX}{name}");
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: &row.source,
                runtime: PYTHON_RUNTIME,
                role: ROLE,
                log_group: None,
            },
        )
        .await;

        // The run itself: every handler reports that it took the branch it was written for, so
        // a row whose line is missing is a missing line rather than a handler that never ran.
        let (reported, payload) = invoke(&router, case, &name).await;
        assert_eq!(
            payload["ok"],
            json!(true),
            "case {case:?}: expected the handler to run to its own end, got {payload}"
        );

        let found = groups(&router, case, &group).await;
        assert_eq!(
            found,
            vec![group.clone()],
            "case {case:?}: expected the invocation to log to {group:?}, got {found:?}"
        );
        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for one invocation, got {held:?}"
        );
        let (lines, _) = delivered(&router, case, &group, &held[0]).await;

        // The frames, and the count of lines they frame: exactly the handler's own.
        let opened = framed_at(&lines, START_PREFIX);
        let closed = framed_at(&lines, END_PREFIX);
        let reported_at = framed_at(&lines, REPORT_PREFIX);
        assert_eq!(
            (opened, closed, reported_at),
            (
                Some(0),
                Some(row.expected.len() + 1),
                Some(row.expected.len() + 2)
            ),
            "case {case:?}: expected {} of the handler's own lines framed by START, END and \
             REPORT, got {lines:?}",
            row.expected.len()
        );
        assert_eq!(
            lines.len(),
            row.expected.len() + FRAMING_LINES,
            "case {case:?}: expected {} line(s) of the handler's own beside the framing, got \
             {lines:?}",
            row.expected.len()
        );

        // Each of the handler's lines is an event of its own, in the order it was written and
        // inside the frames.
        let (opening, closing) = (
            opened.unwrap_or_default(),
            closed.unwrap_or(lines.len().saturating_sub(2)),
        );
        let mut previous = opening;
        for expected in &row.expected {
            let at = lines.iter().position(|line| line.contains(expected));
            let at = at.unwrap_or_else(|| {
                panic!("case {case:?}: expected a line carrying {expected:?}, got {lines:?}")
            });
            assert!(
                at > previous && at < closing,
                "case {case:?}: expected the line carrying {expected:?} after position \
                 {previous} and before the closing frame at {closing}, got {at} in {lines:?}"
            );
            previous = at;
            // The log tail the invocation itself reported carries the same line, so the two
            // readings of one run's output agree.
            assert!(
                reported.contains(expected),
                "case {case:?}: expected the invocation's own log tail to carry {expected:?}, \
                 got {reported:?}"
            );
        }
    }
}
