// SPDX-License-Identifier: CC0-1.0

//! What a client reads back of a run's log when SEVERAL functions log to ONE group: the records
//! as the run wrote them, each in the stream of the invocation that wrote it.
//!
//! RULE: Lambda delivers every record of a run's log to CloudWatch Logs AS THE WRITE PRODUCED IT,
//! so each event's message keeps the line break that closed the record — the platform's own
//! `START`/`END`/`REPORT` frames and the handler's own records alike — and each run's records go
//! only into the stream opened for that invocation. Which group a function logs to does not
//! change either: it is the one its `LoggingConfig$LogGroup` names, else the default
//! `/aws/lambda/<function name>` ("By default, Lambda functions send logs to a default log group
//! named `/aws/lambda/<function name>`",
//! `smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`), and nothing says that
//! group is a function's alone — two functions naming one group leave two streams in it, each
//! holding one run's records whole. Under the plain-text `LogFormat` — "the format in which
//! Lambda sends your function's application and system logs to CloudWatch. Select between plain
//! text and structured JSON", which is what a function naming none carries
//! (`smithy_query(query="doc",shape="LoggingConfig",member="LogFormat")`) — a record a managed
//! runtime's own logging interface handed the platform whole arrives as ONE event of four
//! tab-separated fields (the instant, the invocation, the level word, the message), its own
//! breaks inside that one message, while what a handler wrote straight to a standard stream
//! arrives as the line it is.
//!
//! A client that reads its function's log back matches the bytes it knows the handler wrote
//! (`message == "<line>\n"`), and a line-oriented consumer — a metric filter, a subscription
//! filter, a shipper re-joining the events of a stream into the run's log — depends on the same
//! terminator; a record delivered without it matches nothing, and one delivered into the wrong
//! stream attributes a run's output to the other function sharing the group.
//!
//! What the neighbouring suites pin and this one does not repeat:
//! [`crate::invocation_log_delivery`] owns WHERE one function's log goes and which of the three
//! log writes the execution role allows, [`crate::node_console_record_framing`] owns the FIELDS
//! one node console record is rendered into,
//! [`crate::raw_stream_lines_are_framed_at_their_breaks`] owns where a raw stream write is cut
//! into events, and [`crate::invocation_log_record_format`] owns which form the records take
//! under each `LogFormat`. What only this suite reaches is ONE group shared by several functions:
//! that each run's records stay whole and stay in that run's own stream, read back by the exact
//! terminated message a client searches for.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the harness [`crate::invocation_log_delivery::emulator`]
//! builds, reused rather than copied — so what is asserted is what a client reads off
//! `DescribeLogStreams` and `GetLogEvents`. The rows differ along what the rule generalises over:
//! which managed runtime ran (two nodejs releases and python, because the delivery is the
//! platform's), which group the function logs to (one shared with another function and outside
//! the default prefix, one of its own, and the default one), how many records the handler left,
//! whether the runtime framed them or the handler wrote them straight to standard output, and how
//! many lines one record carries.
//!
//! No expected value here comes from a recording: every line asserted is a line this table had a
//! handler write, and the instant and the invocation each record names are read for their FORM.
//!
//! The test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::handler_logging_facility::{is_instant, request_id_of};
use crate::invocation_log_delivery::{
    create_function, emulator, events, invocation_request, streams, Created,
};
use crate::invoke_request_id::{is_host_bound, is_request_id, one_invocation_stream_at_a_time};

/// A well-formed execution role ARN the IAM store this harness mounts holds no record for, so
/// nothing is known about the role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The prefix of the group a function whose configuration names none logs to.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The ONE group two of the rows name: a name outside the default prefix, so the sharing is the
/// configuration's own rather than a consequence of the names the functions carry. It is within
/// `LoggingConfig$LogGroup`'s own constraints (`@length(min: 1, max: 512)` and
/// `@pattern("^[\\.\\-_/#A-Za-z0-9]+$")`, read off
/// `smithy_query(query="shape",shape="LoggingConfig")`).
const SHARED_GROUP: &str = "/scenario/log-records/shared-by-two";

/// The group the row that shares with nobody names, likewise outside the default prefix.
const OWN_GROUP: &str = "/scenario/log-records/one-functions-own";

/// What closes ONE record of a run's log, which is part of the message CloudWatch Logs holds.
const RECORD_TERMINATOR: &str = "\n";

/// The field separator a record the runtime framed carries, and how many fields it frames one
/// into: the instant it was written, the invocation, the level word and the message.
const FIELD_SEPARATOR: char = '\t';
const FRAMED_FIELDS: usize = 4;

/// The level word a `console.log` record states. Source: the level of the console method that
/// wrote it, as the nodejs managed runtime's plain-text format states it.
const INFO_LEVEL: &str = "INFO";

/// The platform's own frames of ONE invocation, by the words each opens with. Each names the
/// invocation it frames, and each is one record of the run's log.
const RUN_FRAMES: [&str; 3] = ["START RequestId: ", "END RequestId: ", "REPORT RequestId: "];

/// The version segment the stream of an invocation of the unpublished function carries.
const UNPUBLISHED_SEGMENT: &str = "[$LATEST]";

/// How many times a row drives its invocation while the answer reports one of this PROCESS's own
/// host bounds instead of a run of the handler ([`is_host_bound`] is the one reading of such an
/// answer).
const HOST_STOP_ATTEMPTS: usize = 6;

/// How long a row lets the suites holding this process's run slots get on with their own work
/// before it drives its invocation again. It waits on nothing of the emulator's: the bound is
/// this test binary's own.
const SLACK: Duration = Duration::from_millis(150);

/// ONE record the handler leaves behind: the whole string it logged in one call, and whether the
/// managed runtime's own logging interface framed it.
struct Record {
    /// The one string the call logs, which must come back as the message of one event, closed by
    /// the record terminator.
    logged: &'static str,
    /// Whether the runtime framed the record (a node `console` call) rather than the handler
    /// writing it straight to standard output.
    framed: bool,
}

/// One function of the table: the runtime that runs it, where it logs, and what it logs.
struct Row {
    case: &'static str,
    runtime: &'static str,
    /// The group its `LoggingConfig` names, or `None` for the default one of its name.
    log_group: Option<&'static str>,
    /// The records its run leaves, in the order the handler logs them.
    records: &'static [Record],
}

/// The table: two functions sharing one group, one with a group of its own and one on the default
/// group of its name.
const ROWS: &[Row] = &[
    Row {
        case: "a python handler printing one line into a group it shares with another function",
        runtime: "python3.12",
        log_group: Some(SHARED_GROUP),
        records: &[Record {
            logged: "WHO-python-shared",
            framed: false,
        }],
    },
    Row {
        case: "a node handler logging into that same shared group, one line and one of two",
        runtime: "nodejs20.x",
        log_group: Some(SHARED_GROUP),
        records: &[
            Record {
                logged: "WHO-node-single",
                framed: true,
            },
            Record {
                logged: "WHO-node-pair-first\nWHO-node-pair-second",
                framed: true,
            },
        ],
    },
    Row {
        case: "a python handler printing two lines into a group of its own",
        runtime: "python3.12",
        log_group: Some(OWN_GROUP),
        records: &[
            Record {
                logged: "WHO-python-own-first",
                framed: false,
            },
            Record {
                logged: "WHO-python-own-second",
                framed: false,
            },
        ],
    },
    Row {
        case: "a node handler logging a three-line message into the default group of its name",
        runtime: "nodejs22.x",
        log_group: None,
        records: &[Record {
            logged: "WHO-node-default-a\nWHO-node-default-b\nWHO-node-default-c",
            framed: true,
        }],
    },
];

/// `text` as a source literal of either runtime — escaped by the JSON serializer rather than by
/// hand, because a JSON string literal is both a JavaScript and a Python one for the plain text
/// the table logs.
fn literal(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// The handler a row runs: one call per record, then a value of its own so a run that failed is
/// told from one that logged nothing.
fn handler_source(row: &Row) -> String {
    if row.runtime.starts_with("python") {
        let mut source = String::from("def lambda_handler(event, context):\n");
        for record in row.records {
            source.push_str(&format!("    print({})\n", literal(record.logged)));
        }
        source.push_str("    return {'ok': True}\n");
        source
    } else {
        let mut source = String::from("exports.handler = async () => {\n");
        for record in row.records {
            source.push_str(&format!("    console.log({});\n", literal(record.logged)));
        }
        source.push_str("    return { ok: true };\n};\n");
        source
    }
}

/// What one `Invoke` answered: the status, the log tail it reported, its payload and that
/// payload's text.
struct Answered {
    status: u16,
    tail: String,
    payload: Value,
    text: String,
}

/// `Invoke` the function `name` synchronously, asking for the log tail.
async fn invoke_once(router: &MultiServiceRouter, name: &str) -> Answered {
    let response = router
        .dispatch(invocation_request(name, &[("x-amz-log-type", "Tail")]))
        .await;
    let (parts, body) = response.into_parts();
    let tail = parts
        .headers
        .get("x-amz-log-result")
        .and_then(|value| value.to_str().ok())
        .map(|encoded| {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap_or_default();
            String::from_utf8_lossy(&decoded).to_string()
        })
        .unwrap_or_default();
    Answered {
        status: parts.status.as_u16(),
        tail,
        payload: serde_json::from_slice(&body).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&body).to_string(),
    }
}

/// The same call, driven again while the answer reports one of this process's own host bounds
/// rather than what the run came to.
async fn invoke_run(router: &MultiServiceRouter, name: &str) -> Answered {
    let mut answered = invoke_once(router, name).await;
    for _ in 1..HOST_STOP_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text) {
            break;
        }
        tokio::time::sleep(SLACK).await;
        answered = invoke_once(router, name).await;
    }
    answered
}

/// The MESSAGE text of one delivered record: the whole message for a record the handler wrote
/// straight to a standard stream, and the fourth field for one the runtime framed — which is
/// where that framing carries the message.
fn delivered_text(message: &str, framed: bool) -> Option<&str> {
    if !framed {
        return Some(message);
    }
    let fields: Vec<&str> = message.splitn(FRAMED_FIELDS, FIELD_SEPARATOR).collect();
    (fields.len() == FRAMED_FIELDS).then(|| fields[3])
}

/// Which of `held` hold a record whose message text is `expected`, and how many events of each
/// carry it — the read a client searching a group for a line it knows it logged makes.
async fn streams_carrying(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    held: &[String],
    expected: &str,
    framed: bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for stream in held {
        let carrying = events(router, case, group, stream)
            .await
            .into_iter()
            .filter(|(_, message)| delivered_text(message, framed) == Some(expected))
            .count();
        if carrying > 0 {
            assert_eq!(
                carrying, 1,
                "case {case:?}: expected the record {expected:?} in {stream:?} once, got \
                 {carrying} events carrying it"
            );
            found.push(stream.clone());
        }
    }
    found
}

/// What one row's run left behind: the group it logged to, the invocation it was, and the stream
/// that invocation opened.
struct Ran {
    case: &'static str,
    group: String,
    request_id: String,
    stream: String,
    records: &'static [Record],
}

/// RULE: every record of a run reaches the function's log group closed by the break that ended
/// the write it was made in — the platform's own frames included — and reaches the stream of ITS
/// invocation alone, whether the group is the function's default one, one of its own, or one it
/// shares with another function.
#[tokio::test]
async fn a_shared_log_group_holds_each_runs_records_whole_and_apart() {
    let _stream = one_invocation_stream_at_a_time().await;
    // ONE engine for the whole table, so a group two rows name really is one group.
    let router = emulator();
    let mut ran: Vec<Ran> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("shared-log-group-{index}");
        let group = row
            .log_group
            .map(str::to_string)
            .unwrap_or_else(|| format!("{DEFAULT_GROUP_PREFIX}{name}"));
        let source = handler_source(row);
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: &source,
                runtime: row.runtime,
                role: ROLE,
                log_group: row.log_group,
            },
        )
        .await;

        let answered = invoke_run(&router, &name).await;
        if is_host_bound(answered.status, &answered.text) {
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200 from Invoke, got {} {}",
            answered.status, answered.text
        );
        assert_eq!(
            answered.payload["ok"],
            json!(true),
            "case {case:?}: expected the handler to run to its own end, got {}",
            answered.payload
        );

        // The invocation this run was, read off the opening frame the platform wrote for it.
        let request_id = request_id_of(case, &answered.tail);
        assert!(
            is_request_id(&request_id),
            "case {case:?}: expected the opening frame to name the invocation, got {request_id:?}"
        );
        // The stream that invocation opened, named for the version that executed and the run's
        // own id.
        let held = streams(&router, case, &group).await;
        let tail = format!("{UNPUBLISHED_SEGMENT}{}", request_id.replace('-', ""));
        let mine: Vec<String> = held
            .iter()
            .filter(|stream| stream.ends_with(&tail))
            .cloned()
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "case {case:?}: expected exactly one stream of {group:?} named for the invocation \
             {request_id:?}, got {held:?}"
        );
        let stream = mine[0].clone();

        let messages: Vec<String> = events(&router, case, &group, &stream)
            .await
            .into_iter()
            .map(|(_, message)| message)
            .collect();

        // Every delivered record carries the break that closed the write it was made in, and
        // exactly one of them: the terminator is part of the message a client reads back.
        for message in &messages {
            assert!(
                message.ends_with(RECORD_TERMINATOR)
                    && !message.ends_with(&format!("{RECORD_TERMINATOR}{RECORD_TERMINATOR}")),
                "case {case:?}: expected every delivered record closed by exactly one break, got \
                 {message:?}"
            );
        }

        // The platform's own frames: one each, naming this invocation, each one record.
        for frame in RUN_FRAMES {
            let opening = format!("{frame}{request_id}");
            let carrying = messages
                .iter()
                .filter(|message| message.starts_with(&opening))
                .count();
            assert_eq!(
                carrying, 1,
                "case {case:?}: expected exactly one delivered event opening with {opening:?}, \
                 got {carrying} in {messages:?}"
            );
        }

        // The handler's own records: one event each, carrying the whole logged string with the
        // terminator, and — where the runtime framed it — the instant, the invocation and the
        // level beside it.
        for record in row.records {
            let expected = format!("{}{RECORD_TERMINATOR}", record.logged);
            let carrying: Vec<&String> = messages
                .iter()
                .filter(|message| delivered_text(message, record.framed) == Some(expected.as_str()))
                .collect();
            assert_eq!(
                carrying.len(),
                1,
                "case {case:?}: expected exactly one event whose message is the whole logged \
                 string {expected:?}, got {messages:?}"
            );
            // Every line of the logged string is in that one event: a continuation line
            // delivered on its own, or dropped, shows up here.
            for line in record.logged.split('\n').filter(|line| !line.is_empty()) {
                let holding = messages
                    .iter()
                    .filter(|message| message.contains(line))
                    .count();
                assert_eq!(
                    holding, 1,
                    "case {case:?}: expected the line {line:?} to stay inside that one event, got \
                     {holding} event(s) carrying it in {messages:?}"
                );
            }
            if record.framed {
                let message = carrying[0];
                let fields: Vec<&str> = message.splitn(FRAMED_FIELDS, FIELD_SEPARATOR).collect();
                assert!(
                    is_instant(fields[0]),
                    "case {case:?}: expected the instant the record was written, got {:?}",
                    fields[0]
                );
                assert_eq!(
                    fields[1], request_id,
                    "case {case:?}: expected the record to name the invocation {request_id:?}, \
                     got {:?}",
                    fields[1]
                );
                assert_eq!(
                    fields[2], INFO_LEVEL,
                    "case {case:?}: expected the level {INFO_LEVEL:?}, got {:?}",
                    fields[2]
                );
            }
            // The log tail the invocation itself reported carries the same record, so the two
            // readings of one run's output agree on it.
            assert!(
                answered.tail.contains(record.logged),
                "case {case:?}: expected the invocation's own log tail to carry the whole record \
                 {:?}, got {:?}",
                record.logged,
                answered.tail
            );
        }

        ran.push(Ran {
            case,
            group,
            request_id,
            stream,
            records: row.records,
        });
    }
    assert!(
        !ran.is_empty(),
        "no row of this table was answered by a run at all: every one was ended by this process's \
         own host bounds, so the rule was never observed"
    );

    // Every run's records are in the stream of ITS invocation and in no other stream of the
    // group — which is what a client sharing one group between functions depends on, and the
    // read it makes is for the exact terminated message it knows the handler logged.
    for run in &ran {
        let case = run.case;
        let held = streams(&router, case, &run.group).await;
        let sharing: Vec<&Ran> = ran.iter().filter(|other| other.group == run.group).collect();
        assert_eq!(
            held.len(),
            sharing.len(),
            "case {case:?}: expected {group:?} to hold one stream per run delivered to it, got \
             {held:?}",
            group = run.group
        );
        for record in run.records {
            let expected = format!("{}{RECORD_TERMINATOR}", record.logged);
            let found = streams_carrying(
                &router,
                case,
                &run.group,
                &held,
                &expected,
                record.framed,
            )
            .await;
            assert_eq!(
                found,
                vec![run.stream.clone()],
                "case {case:?}: expected the record {expected:?} of the invocation \
                 {request_id:?} in exactly its own stream {stream:?}, found in {found:?}",
                request_id = run.request_id,
                stream = run.stream
            );
        }
    }
}
