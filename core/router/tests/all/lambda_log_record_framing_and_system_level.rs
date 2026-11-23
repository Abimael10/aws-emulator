// SPDX-License-Identifier: CC0-1.0

//! HOW MANY events one thing a run reported becomes in the function's Amazon CloudWatch Logs
//! group, and WHICH of the platform's own records of a run reach that group at all.
//!
//! RULE under test: **a write a handler makes STRAIGHT to one of its standard streams is
//! framed by AWS Lambda at its line breaks — one log event per line, each carrying the break
//! that closed it, and a line written in pieces still one event — while a record the managed
//! runtime's own logging interface produced (a `console` call, a `logging` record) is ONE event
//! however many line breaks its formatted message carries. The platform's own records of a run
//! are its SYSTEM logs: the execution environment's initialization is framed by its own records
//! (the phase starting, the runtime reporting that phase done, and the phase's report) before
//! the records of the invocation served in it, and Lambda sends each such record only at the
//! function's `LoggingConfig$SystemLogLevel` of detail and lower — the records of a phase and
//! the one closing the runtime's work on an invocation state the DETAIL of how the run was
//! served and travel at the most detailed level, while the two frames that state an invocation
//! happened and what it spent are the envelope the default level still admits. It is a filter of
//! the structured format, so a function on the plain-text format is sent the runtime's framing
//! whatever level is stored.**
//!
//! A client reads its function's log by matching records: a metric filter counting the lines a
//! handler printed, a subscription filter forwarding them, an operator grepping a stream. A
//! multi-line write delivered as one event is one match where there should be two, and a
//! several-line record split apart is one match per line of a message that was reported once —
//! both make the count a client alarms on wrong. And a function whose system level was set to
//! the most detailed reads nothing more than one left at the default unless the records that
//! level admits are really withheld at the default.
//!
//! Two services have to agree for that to hold — AWS Lambda runs the handler and delivers the
//! records as the function's execution role, and Amazon CloudWatch Logs is where a client reads
//! them — and CloudFormation deploys each row's role and function, so the suite lives here
//! rather than in any one crate. What the neighbours pin and this one does not restate:
//! [`crate::lambda_log_records_delivered_under_the_roles_logs_grant`] owns the BYTES of one
//! delivered record and the role gate on the delivery (and the wire harness both reuse comes
//! from [`crate::cloudformation_lambda_function_log_destination`]);
//! `lambda_invoke_request_id_correlates_with_its_log_group` owns the id the records are framed
//! by. What only this suite reaches is how many events one reported thing becomes, and which
//! platform records a system level admits.
//!
//! The rows differ along what the rule generalises over rather than restating one template: the
//! managed RUNTIME that wrote the records (each frames a console/logging record its own way),
//! the KIND of write (a runtime-framed record carrying breaks of its own — a `console.log` whose
//! message holds one, and a `logging` record carrying an exception's frames — a raw write
//! carrying several lines, and a raw line written in pieces across two writes), the `LogFormat`
//! the records are rendered in, and the system level in force — the most detailed, the default
//! (left unnamed and named outright), and one below every record the platform writes for a run.
//!
//! No expected value here comes from a recording: every marker is built by the row, and every
//! asserted record is built from it.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, lambda, messages_at_least, messages_in, trust_policy,
    wait_until_active, CREATE_COMPLETE, FUNCTION_TIMEOUT,
};
use crate::lambda_log_records_delivered_under_the_roles_logs_grant::{
    default_group, JSON_FORMAT, PLATFORM_PREFIX, TEXT_FORMAT, TEXT_FRAMES, TYPE_MEMBER,
};
use crate::query_wire::outputs_of;

/// The logical ids each row's template declares.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Logger";

/// The three CloudWatch Logs calls an invocation's log delivery makes, which is what AWS's own
/// basic execution policy grants a function. A role short of them leaves nothing to read, which
/// is [`crate::lambda_log_records_delivered_under_the_roles_logs_grant`]'s rule and not this
/// one.
const DELIVERY_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The kinds of platform record one run leaves, in the order the platform writes them: the
/// initialization of the execution environment, then the invocation served in it. Every one of
/// them reaches the group at the most detailed system level.
const EVERY_PLATFORM_KIND: [&str; 6] = [
    "platform.initStart",
    "platform.initRuntimeDone",
    "platform.initReport",
    "platform.start",
    "platform.runtimeDone",
    "platform.report",
];

/// The kinds a function filtered at the DOCUMENTED DEFAULT system level is sent: the envelope
/// every invocation is reported by — that one happened, and what it spent. The records of the
/// environment's phases and the one closing the runtime's work on the invocation state the
/// detail of HOW the run was served, which the level above alone admits.
///
/// Source: the levels AWS states for the system records of a run
/// (<https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html>).
const ENVELOPE_PLATFORM_KINDS: [&str; 2] = ["platform.start", "platform.report"];

/// The node handler the node rows deploy. It reports the same text four ways, so one row shows
/// every form of write the rule distinguishes:
///
/// * a `console` call whose formatted message carries a line break of its own,
/// * one raw write to the standard output carrying two whole lines,
/// * and one raw line written in TWO pieces, the first of them closing on no break.
const NODE_HANDLER: &str = r#"
exports.handler = async (event) => {
    const m = event.marker;
    console.log(`framed-one-${m}\nframed-two-${m}`);
    process.stdout.write(`raw-one-${m}\nraw-two-${m}\n`);
    process.stderr.write(`pieced-${m}`);
    process.stderr.write(`-rest-${m}\n`);
    return { logged: m };
};
"#;

/// The python handler the python rows deploy: the same four forms through that runtime's own
/// interfaces — a `logging` record carrying an exception's frames (whose formatted message
/// holds several line breaks), one `print` of two lines, and one line written to the standard
/// output in two pieces.
const PYTHON_HANDLER: &str = r#"
import logging
import sys

_logger = logging.getLogger()
_logger.setLevel(logging.INFO)


def handler(event, context):
    m = event["marker"]
    try:
        raise ValueError("framed-two-%s" % m)
    except ValueError:
        _logger.error("framed-one-%s" % m, exc_info=True)
    print("raw-one-%s\nraw-two-%s" % (m, m))
    sys.stdout.write("pieced-%s" % m)
    sys.stdout.write("-rest-%s\n" % m)
    return {"logged": m}
"#;

/// One row: the runtime, the handler it deploys, the format its records are rendered in and the
/// system level in force.
struct Row {
    case: &'static str,
    /// The `FunctionName` the template states, which is also the subject of its default group.
    function: &'static str,
    runtime: &'static str,
    source: &'static str,
    /// The `LoggingConfig$LogFormat` the records are rendered in.
    format: &'static str,
    /// The `LoggingConfig$SystemLogLevel` stored for the function, or `None` for a function
    /// that names none.
    system_level: Option<&'static str>,
    /// The kinds of platform record this row must find in its group, in order — empty for a
    /// row whose level admits none of them, and empty for a plain-text row, whose framing is
    /// read as text instead.
    platform_kinds: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a node run on the plain-text format, which filters by no level",
        function: "xsvc-log-framing-node-text",
        runtime: "nodejs22.x",
        source: NODE_HANDLER,
        format: TEXT_FORMAT,
        system_level: None,
        platform_kinds: &[],
    },
    Row {
        case: "a python run on the structured format at the most detailed system level",
        function: "xsvc-log-framing-python-debug",
        runtime: "python3.12",
        source: PYTHON_HANDLER,
        format: JSON_FORMAT,
        system_level: Some("DEBUG"),
        platform_kinds: &EVERY_PLATFORM_KIND,
    },
    Row {
        case: "a node run on the structured format naming no system level, which is the \
               documented default",
        function: "xsvc-log-framing-node-default",
        runtime: "nodejs22.x",
        source: NODE_HANDLER,
        format: JSON_FORMAT,
        system_level: None,
        platform_kinds: &ENVELOPE_PLATFORM_KINDS,
    },
    Row {
        case: "a python run on the structured format at the default level named outright, which \
               withholds the detail of how the run was served",
        function: "xsvc-log-framing-python-info",
        runtime: "python3.12",
        source: PYTHON_HANDLER,
        format: JSON_FORMAT,
        system_level: Some("INFO"),
        platform_kinds: &ENVELOPE_PLATFORM_KINDS,
    },
    Row {
        case: "a python run on the plain-text format, whose logging record carries an \
               exception's frames",
        function: "xsvc-log-framing-python-text",
        runtime: "python3.12",
        source: PYTHON_HANDLER,
        format: TEXT_FORMAT,
        system_level: None,
        platform_kinds: &[],
    },
    Row {
        case: "a python run on the structured format at a system level below every record the \
               platform writes for a run",
        function: "xsvc-log-framing-python-warn",
        runtime: "python3.12",
        source: PYTHON_HANDLER,
        format: JSON_FORMAT,
        system_level: Some("WARN"),
        platform_kinds: &[],
    },
];

impl Row {
    /// The `LoggingConfig` this row's function carries.
    fn logging_config(&self) -> Value {
        let mut logging = Map::new();
        logging.insert("LogFormat".to_string(), json!(self.format));
        if let Some(level) = self.system_level {
            logging.insert("SystemLogLevel".to_string(), json!(level));
        }
        Value::Object(logging)
    }

    /// The template this row deploys: its execution role, and the function that runs as it.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                ROLE_ID: {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": trust_policy(),
                        "Policies": [{
                            "PolicyName": "log-delivery",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": DELIVERY_ACTIONS,
                                    "Resource": "*"
                                }]
                            }
                        }]
                    }
                },
                FUNCTION_ID: {
                    "Type": "AWS::Lambda::Function",
                    "Properties": {
                        "FunctionName": self.function,
                        "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                        "Runtime": self.runtime,
                        "Handler": "index.handler",
                        "Timeout": FUNCTION_TIMEOUT,
                        "LoggingConfig": self.logging_config(),
                        "Code": { "ZipFile": self.source }
                    }
                }
            },
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

/// Deploy `row`'s stack and answer the function name it published, with the function ready to
/// take calls and reporting the logging configuration the template asked for.
async fn given_deployed(router: &MultiServiceRouter, row: &Row, stack: &str) -> String {
    let case = row.case;
    let template = row.template();
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no function name"));
    let configuration = wait_until_active(router, case, &function).await;
    let logging = configuration.get("LoggingConfig").cloned().unwrap_or(Value::Null);
    assert_eq!(
        logging.get("LogFormat").and_then(Value::as_str),
        Some(row.format),
        "case {case:?}: expected AWS Lambda to report the declared format, got {configuration}"
    );
    if let Some(level) = row.system_level {
        assert_eq!(
            logging.get("SystemLogLevel").and_then(Value::as_str),
            Some(level),
            "case {case:?}: expected AWS Lambda to report the declared system level, got \
             {configuration}"
        );
    }
    function
}

/// `Invoke` the function `name` with `marker`, asserting the invocation itself was clean.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, marker: &str) {
    let invoked = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        Some(json!({ "marker": marker })),
    )
    .await;
    assert_eq!(
        invoked.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke of {name:?}, got {} with {:?}",
        invoked.status,
        invoked.text()
    );
    assert_eq!(
        invoked.function_error, None,
        "case {case:?}: expected no FunctionError, got {:?} with {:?}",
        invoked.function_error,
        invoked.text()
    );
    assert_eq!(
        invoked.json().get("logged").and_then(Value::as_str),
        Some(marker),
        "case {case:?}: expected the handler's own result for {marker:?}, got {:?}",
        invoked.text()
    );
}

/// The kinds of platform record `messages` holds, in the order they were delivered.
fn platform_kinds(messages: &[String]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| {
            serde_json::from_str::<Value>(message.trim_end()).ok().and_then(|record| {
                record
                    .get(TYPE_MEMBER)
                    .and_then(Value::as_str)
                    .filter(|kind| kind.starts_with(PLATFORM_PREFIX))
                    .map(str::to_string)
            })
        })
        .collect()
}

/// How many of `messages` carry `needle`.
fn carrying(messages: &[String], needle: &str) -> usize {
    messages
        .iter()
        .filter(|message| message.contains(needle))
        .count()
}

/// The group's messages, polled until every one of `awaited` is carried by one of them or the
/// polls run out. The delivery of a run's log is not part of the invocation's answer, so this
/// waits for the CONDITION rather than for a span.
async fn messages_carrying(
    router: &MultiServiceRouter,
    group: &str,
    awaited: &[String],
) -> Vec<String> {
    let mut held = messages_at_least(router, group, awaited.len()).await;
    for _ in 0..POLLS {
        if awaited
            .iter()
            .all(|needle| held.iter().any(|message| message.contains(needle)))
        {
            return held;
        }
        tokio::time::sleep(POLL).await;
        held = messages_in(router, group).await;
    }
    held
}

/// How long to wait between looks at something a service owes, and how many looks to take. The
/// budget is far longer than any span these services choose, so a loaded host is slower without
/// the rule being any different; nothing here asserts on elapsed time.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);
const POLLS: usize = 600;

/// RULE: a raw write to a standard stream becomes one event per line and a runtime-framed
/// record becomes one event whatever breaks its message carries, and the platform's records of
/// a run reach the group only at the function's system level of detail and lower.
#[tokio::test]
async fn a_runs_records_are_framed_by_their_writer_and_filtered_by_the_system_level() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("xsvc-log-framing-{index}");
        let function = given_deployed(&router, row, &stack).await;
        let group = default_group(&function);
        let marker = format!("{function}-{index}");

        invoke(&router, case, &function, &marker).await;

        // 1. The three raw lines the run wrote are the three events a client matching them
        //    finds, each closed by its own break — including the one written in two pieces,
        //    which is ONE line and so one event.
        let raw_one = format!("raw-one-{marker}\n");
        let raw_two = format!("raw-two-{marker}\n");
        let pieced = format!("pieced-{marker}-rest-{marker}\n");
        let awaited: Vec<String> = vec![raw_one.clone(), raw_two.clone(), pieced.clone()];
        let held = messages_carrying(&router, &group, &awaited).await;
        for expected in [&raw_one, &raw_two, &pieced] {
            let found = held.iter().filter(|message| *message == expected).count();
            assert_eq!(
                found, 1,
                "case {case:?}: expected exactly one event carrying the raw line {expected:?}, \
                 got {found} in {held:?}"
            );
        }

        // 2. The record the RUNTIME framed is one event, carrying both lines of its formatted
        //    message: a client matching either line matches that one event and no other.
        let framed_one = format!("framed-one-{marker}");
        let framed_two = format!("framed-two-{marker}");
        for part in [&framed_one, &framed_two] {
            let found = carrying(&held, part);
            assert_eq!(
                found, 1,
                "case {case:?}: expected exactly one event carrying {part:?}, got {found} in \
                 {held:?}"
            );
        }
        let both: Vec<&String> = held
            .iter()
            .filter(|message| message.contains(&framed_one) && message.contains(&framed_two))
            .collect();
        assert_eq!(
            both.len(),
            1,
            "case {case:?}: expected the runtime's record to be ONE event carrying both lines \
             of its message, got {both:?} in {held:?}"
        );

        // 3. Which of the platform's own records of the run reached the group: exactly the
        //    kinds this row's system level admits, in the order the platform writes them.
        let kinds = platform_kinds(&held);
        let expected: Vec<String> = row
            .platform_kinds
            .iter()
            .map(|kind| (*kind).to_string())
            .collect();
        assert_eq!(
            kinds, expected,
            "case {case:?}: expected the platform records {expected:?}, got {kinds:?} in \
             {held:?}"
        );

        // 4. A function on the plain-text format is sent the runtime's own framing instead,
        //    which that format carries as lines rather than as records.
        if row.format == TEXT_FORMAT {
            for frame in TEXT_FRAMES {
                assert_eq!(
                    carrying(&held, frame),
                    1,
                    "case {case:?}: expected one record starting {frame:?} in {group:?}, got \
                     {held:?}"
                );
            }
        }
    }
}
