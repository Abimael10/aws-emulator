// SPDX-License-Identifier: CC0-1.0

//! The handle a client holds after an invocation: the id the answer names its request by, and
//! what that one id finds in CloudWatch Logs.
//!
//! RULE: every answer of this service names the request it answered in the `x-amzn-requestid`
//! response header — which is what an AWS SDK surfaces as `ResponseMetadata.RequestId` — and the
//! answer of a call that RAN a function names that invocation's own id there, the id the runtime
//! writes into the run's `START`/`END`/`REPORT` frames and into the name of the stream it writes
//! them to. So a client holding nothing but that id finds the invocation's own log stream and the
//! records the run wrote between those frames, which is the documented way to diagnose one
//! invocation. A record a nodejs managed runtime framed under the plain-text
//! `LoggingConfig$LogFormat` — "the format in which Lambda sends your function's application and
//! system logs to CloudWatch. Select between plain text and structured JSON", the format a
//! function naming none carries
//! (`smithy_query(query="shape",shape="LoggingConfig,LogFormat")`) — stays ONE event however many
//! line breaks its message carries, so the multi-line diagnostic the client went looking for is
//! read back whole under that id.
//!
//! An id reported nowhere on the answer leaves the client with no handle at all: it cannot name
//! the invocation whose stream it has to read, and the join from the call to its log events is
//! not available at any later point.
//!
//! What the siblings pin and this one does not repeat: [`crate::invoke_request_id`] pins the id
//! an answer names its request by against the run's OWN reports (the handler's context, the
//! error document of a failed run, the log tail) and that an answer which ran nothing names a
//! fresh id; [`crate::node_console_record_framing`] pins the FRAMING of each console call, read
//! under the id the delivered opening frame states. Neither starts from the id the RESPONSE
//! reported, which is the only id a client actually holds.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the harness [`crate::invocation_log_delivery::emulator`]
//! builds, reused rather than copied — so what is asserted is what a client reads off `Invoke`,
//! `DescribeLogStreams` and `GetLogEvents`. The rows differ along what the rule generalises over:
//! which nodejs managed runtime ran, whether the create named a `LoggingConfig` at all and
//! whether the one it named carries a log group of its own, and how many lines and which breaks
//! the handler's one console call logs.
//!
//! The test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use base64::Engine as _;

use crate::handler_logging_facility::request_id_of;
use crate::invocation_log_delivery::{
    create_function_logging, emulator, events, invocation_request, streams, Created,
};
use crate::invoke_request_id::{is_host_bound, is_request_id, one_invocation_stream_at_a_time};

/// The header every answer names the request it answered in, and an SDK reads
/// `ResponseMetadata.RequestId` from.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// A well-formed execution role ARN the IAM store this harness mounts holds no record for, so
/// nothing is known about the role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The prefix of the group a function whose configuration names none logs to: "Lambda functions
/// send logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The plain-text `LogFormat` the rows that name one select. Source: the model's own `LogFormat`
/// enum, whose values are `JSON` and `Text`.
const TEXT_FORMAT: &str = "Text";

/// The field separator the plain-text framing states, and how many fields it frames one record
/// into: the instant it was written, the invocation, the level word and the message.
const FIELD_SEPARATOR: char = '\t';
const FRAMED_FIELDS: usize = 4;

/// What closes one record of a run's log.
const RECORD_TERMINATOR: &str = "\n";

/// The platform's own frames of one run, each of which states the invocation it frames.
const RUN_FRAMES: [&str; 3] = ["START", "END", "REPORT"];

/// How many times a row drives its invocation while the answer reports one of this PROCESS's own
/// host bounds instead of a run of the handler
/// ([`crate::invoke_request_id::is_host_bound`] is the one reading of such an answer).
const HOST_STOP_ATTEMPTS: usize = 6;

/// How long a row lets the suites holding this process's run slots get on with their own work
/// before it drives its invocation again. It waits on nothing of the emulator's: the bound is
/// this test binary's own.
const SLACK: Duration = Duration::from_millis(150);

/// One row: the runtime that runs, the logging configuration the create names, the group that
/// leaves the records in, and the one string the handler logs in ONE console call.
struct Row {
    case: &'static str,
    runtime: &'static str,
    /// The whole `LoggingConfig` the create carries, or `None` for a create that names none —
    /// whose function carries the applied default, plain text.
    logging_config: Option<Value>,
    /// The group the records are delivered to, or `None` for the function's default one.
    group: Option<&'static str>,
    /// The one string the handler logs, whose every line has to come back inside ONE event.
    logged: &'static str,
}

/// The rows, spanning the runtime, the logging configuration and the shape of the logged string.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a two-line console.log, on a function that named no logging configuration",
            runtime: "nodejs22.x",
            logging_config: None,
            group: None,
            logged: "JOIN-first\nJOIN-second",
        },
        Row {
            case: "a console.log whose message holds an empty line between two, under the \
                   plain-text format the create selected",
            runtime: "nodejs22.x",
            logging_config: Some(json!({ "LogFormat": TEXT_FORMAT })),
            group: None,
            logged: "GAPJOIN-above\n\nGAPJOIN-below",
        },
        Row {
            case: "a console.log of an indented stack, delivered to a log group of the \
                   function's own",
            runtime: "nodejs20.x",
            logging_config: Some(json!({
                "LogFormat": TEXT_FORMAT,
                "LogGroup": "/aws/lambda/log-join-group-of-its-own",
            })),
            group: Some("/aws/lambda/log-join-group-of-its-own"),
            logged: "STACKJOIN-Error: boom\n    at STACKJOIN-handler (/var/task/index.js:1:1)",
        },
        Row {
            case: "a console.log of one line, which carries no break at all",
            runtime: "nodejs20.x",
            logging_config: Some(json!({ "LogFormat": TEXT_FORMAT })),
            group: None,
            logged: "ONEJOIN-only",
        },
    ]
}

/// `text` as a JavaScript string literal — escaped by the JSON serializer rather than by hand,
/// because a JSON string literal is a JavaScript one.
fn js_literal(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// The handler a row runs: ONE console call carrying the whole logged string, then a value of its
/// own so a run that failed is told from one that logged nothing.
fn handler_source(logged: &str) -> String {
    format!(
        "exports.handler = async () => {{\n    console.log({});\n    return {{ ok: true }};\n}};\n",
        js_literal(logged)
    )
}

/// What one `Invoke` answered: the status, the id it named its request by, the log tail it
/// reported, and its payload.
struct Answered {
    status: u16,
    request_id: String,
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
    let header = |header: &str| {
        parts
            .headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let tail = header("x-amz-log-result")
        .map(|encoded| {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap_or_default();
            String::from_utf8_lossy(&decoded).to_string()
        })
        .unwrap_or_default();
    Answered {
        status: parts.status.as_u16(),
        request_id: header(REQUEST_ID_HEADER).unwrap_or_default(),
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

/// RULE: the id an invocation's answer names its request by is the invocation's own, so that one
/// id finds the run's stream in CloudWatch Logs, the frames that name it, and the whole of the
/// one console call the handler made as ONE event.
#[tokio::test]
async fn an_invocations_answer_names_the_invocation_its_log_is_found_by() {
    let _stream = one_invocation_stream_at_a_time().await;
    let mut named: Vec<String> = Vec::new();
    let mut read = 0_usize;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("log-join-{index}");
        let group = row
            .group
            .map(str::to_string)
            .unwrap_or_else(|| format!("{DEFAULT_GROUP_PREFIX}{name}"));
        let source = handler_source(row.logged);
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: &source,
                runtime: row.runtime,
                role: ROLE,
                log_group: None,
            },
            row.logging_config.as_ref(),
        )
        .await;

        let answered = invoke_run(&router, &name).await;
        if is_host_bound(answered.status, &answered.text) {
            continue;
        }
        read += 1;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected 200 from Invoke, got {} {}",
            answered.status,
            answered.text
        );
        assert_eq!(
            answered.payload["ok"],
            json!(true),
            "case {case:?}: expected the handler to run to its own end, got {}",
            answered.payload
        );
        let id = answered.request_id.clone();
        assert!(
            is_request_id(&id),
            "case {case:?}: expected the answer to name its request in {REQUEST_ID_HEADER}, got \
             {id:?}"
        );
        assert!(
            !named.contains(&id),
            "case {case:?}: every invocation is named by an id of its own, got {id:?} twice"
        );
        named.push(id.clone());

        // The whole join a client can make: the id the answer reported, and nothing else.
        let held = streams(&router, case, &group).await;
        let token = id.replace('-', "");
        let found: Vec<&String> = held
            .iter()
            .filter(|stream| stream.contains(&token))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected exactly one stream of {group:?} named for the invocation \
             {id:?}, got {held:?}"
        );
        let messages: Vec<String> = events(&router, case, &group, found[0])
            .await
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        for frame in RUN_FRAMES {
            let expected = format!("{frame} RequestId: {id}");
            let carrying = messages
                .iter()
                .filter(|message| message.starts_with(&expected))
                .count();
            assert_eq!(
                carrying, 1,
                "case {case:?}: expected exactly one delivered event opening with {expected:?}, \
                 got {carrying} in {messages:?}"
            );
        }
        // The two readings of one run agree on which invocation it was: the id the answer
        // reported and the id the run's own opening frame states.
        let framed = request_id_of(case, &answered.tail);
        assert_eq!(
            framed, id,
            "case {case:?}: the run's log names the invocation {framed:?}, so the answer must \
             name that invocation, got {id:?}"
        );

        // ONE event carries the whole console call, whatever breaks its message holds.
        let terminated = format!("{}{RECORD_TERMINATOR}", row.logged);
        let whole: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                message.splitn(FRAMED_FIELDS, FIELD_SEPARATOR).nth(3) == Some(terminated.as_str())
            })
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            whole.len(),
            1,
            "case {case:?}: expected exactly one event whose message is the whole logged string \
             {terminated:?}, got {messages:?}"
        );
        for line in row.logged.split('\n').filter(|line| !line.is_empty()) {
            let carrying = messages
                .iter()
                .filter(|message| message.contains(line))
                .count();
            assert_eq!(
                carrying, 1,
                "case {case:?}: expected the line {line:?} to stay inside that one event, got \
                 {carrying} event(s) carrying it in {messages:?}"
            );
        }
        let record = &messages[whole[0]];
        let fields: Vec<&str> = record.splitn(FRAMED_FIELDS, FIELD_SEPARATOR).collect();
        assert_eq!(
            fields.len(),
            FRAMED_FIELDS,
            "case {case:?}: expected {FRAMED_FIELDS} tab-separated fields, got {record:?}"
        );
        assert_eq!(
            fields[1], id,
            "case {case:?}: expected the record to name the invocation the answer named, got {:?}",
            fields[1]
        );
    }
    assert!(
        read > 0,
        "no row of this table was answered by a run at all: every one was ended by this \
         process's own host bounds, so the rule was never observed"
    );
}
