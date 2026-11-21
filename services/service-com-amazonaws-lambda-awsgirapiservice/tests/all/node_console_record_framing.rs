// SPDX-License-Identifier: CC0-1.0

//! What ONE console call of a node handler becomes in the function's log group.
//!
//! RULE: under the plain-text `LoggingConfig$LogFormat` — "the format in which Lambda sends
//! your function's application and system logs to CloudWatch. Select between plain text and
//! structured JSON", which is what a function naming none carries
//! (`smithy_query(query="doc",shape="LoggingConfig",member="LogFormat")`) — a nodejs managed
//! runtime frames EACH console call as exactly one record of four tab-separated fields, the
//! instant it was written, the invocation's request id, the level word of the console method
//! that wrote it and the message, closed by one line break. The runtime handed the platform a
//! whole record, so that record is ONE log event however many line breaks its message carries:
//! the breaks inside the logged string stay inside the event's message, start no event of their
//! own, and the message is newline-terminated. A write the handler made STRAIGHT to one of its
//! standard streams is not such a record — it carries no level and no invocation — so the
//! platform frames it by its line breaks instead, one event per line, each carrying the break
//! that closed it and the last one carrying none when the handler never closed it.
//!
//! That is the whole difference between the two channels, and it is what makes a multi-line
//! diagnostic (a stack, a pretty-printed document, a tabular report) readable: it stays
//! attributable to the one call that wrote it, and re-joining the events of a stream gives back
//! the bytes the handler wrote.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than copied —
//! so what is asserted is what a client reads back with `GetLogEvents` and off the invocation's
//! own `X-Amz-Log-Result`.
//!
//! The rows differ along what the rule generalises over: HOW MANY lines one logged string
//! carries (one, two, three, five), WHICH breaks it carries (an empty line between two, a break
//! that ends the string), what else is in the message (an embedded tab, an indented frame of a
//! stack), WHICH console method wrote it and so which level the record states, and — for the
//! unframed channel — which standard stream the write went to, how many lines it carried and
//! whether the handler closed the last of them. The whole table runs on two of the nodejs
//! managed runtimes, because the rule is the runtime's and not one release's.
//!
//! What the neighbouring suites pin and this one does not repeat:
//! [`crate::invocation_log_delivery`] pins WHERE a run's log goes and which of the three log
//! writes the execution role allows, [`crate::invocation_log_record_format`] pins which FORM
//! the records take under each `LogFormat`, and
//! [`crate::handler_output_reaches_its_log_stream`] pins that a handler's own output reaches the
//! stream at all.
//!
//! Every test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::json;

use crate::handler_logging_facility::{is_instant, request_id_of};
use crate::invocation_log_delivery::{create_function, events, invoke, streams, Created};

/// A well-formed execution role ARN the IAM store this harness mounts holds no record for, so
/// nothing is known about the role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The prefix of the log group a function whose configuration names none logs to.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The platform's own frames around one run, which the handler's records sit between.
const INIT_START_PREFIX: &str = "INIT_START Runtime Version: ";
const START_PREFIX: &str = "START RequestId: ";
const END_PREFIX: &str = "END RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// How many records the platform writes itself around one run of an environment it built: the
/// record of the initialization, the opening frame and the two closing ones.
const FRAMING_RECORDS: usize = 4;

/// The field separator the runtime's text framing states, and how many fields it frames a
/// record into.
const FIELD_SEPARATOR: char = '\t';
const FRAMED_FIELDS: usize = 4;

/// What closes one record of a run's log.
const RECORD_TERMINATOR: &str = "\n";

/// The nodejs managed runtimes the table runs on. Source: the model's own `Runtime` enum
/// (`smithy_query(query="shape",shape="Runtime")`), whose `nodejs20.x` and `nodejs22.x` are two
/// releases of the same managed runtime.
const NODE_RUNTIMES: [&str; 2] = ["nodejs20.x", "nodejs22.x"];

/// One console call the handler makes: the method it calls, the level word that call's record
/// states, and the whole string it logs.
struct Framed {
    case: &'static str,
    /// The console method the handler calls.
    method: &'static str,
    /// The level word the record must carry. Source: the level of the console method that
    /// wrote it, as the nodejs managed runtime's text format states it — `console.log` and
    /// `console.info` at INFO, `console.debug` at DEBUG, `console.warn` at WARN and
    /// `console.error` at ERROR.
    level: &'static str,
    /// The one string the call logs, which must arrive whole as the record's fourth field.
    logged: &'static str,
}

/// The console calls: one row per shape of message the rule generalises over.
const FRAMED_ROWS: &[Framed] = &[
    Framed {
        case: "a console.log of one line, which carries no break at all",
        method: "log",
        level: "INFO",
        logged: "SINGLE-only",
    },
    Framed {
        case: "a console.log of two lines",
        method: "log",
        level: "INFO",
        logged: "TWO-first\nTWO-second",
    },
    Framed {
        case: "a console.log of three lines",
        method: "log",
        level: "INFO",
        logged: "THREE-first\nTHREE-second\nTHREE-third",
    },
    Framed {
        case: "a console.log of five lines",
        method: "log",
        level: "INFO",
        logged: "FIVE-a\nFIVE-b\nFIVE-c\nFIVE-d\nFIVE-e",
    },
    Framed {
        case: "a console.log whose message holds an empty line between two",
        method: "log",
        level: "INFO",
        logged: "GAP-above\n\nGAP-below",
    },
    Framed {
        case: "a console.log whose message already ends in a break",
        method: "log",
        level: "INFO",
        logged: "TRAILING-first\nTRAILING-second\n",
    },
    Framed {
        case: "a console.log whose message holds a tab of its own beside its breaks",
        method: "log",
        level: "INFO",
        logged: "TABBED-left\tTABBED-right\nTABBED-below",
    },
    Framed {
        case: "a console.log of an indented stack, the shape a caught failure is reported in",
        method: "log",
        level: "INFO",
        logged: "STACK-Error: boom\n    at STACK-handler (/var/task/index.js:1:1)\n    at \
                 STACK-caller (/var/task/index.js:2:2)",
    },
    Framed {
        case: "a console.info of two lines, which states the same level as console.log",
        method: "info",
        level: "INFO",
        logged: "INFOCALL-first\nINFOCALL-second",
    },
    Framed {
        case: "a console.error of two lines",
        method: "error",
        level: "ERROR",
        logged: "ERRCALL-first\nERRCALL-second",
    },
    Framed {
        case: "a console.warn of two lines",
        method: "warn",
        level: "WARN",
        logged: "WARNCALL-first\nWARNCALL-second",
    },
    Framed {
        case: "a console.debug of two lines",
        method: "debug",
        level: "DEBUG",
        logged: "DEBUGCALL-first\nDEBUGCALL-second",
    },
];

/// One write the handler makes STRAIGHT to a standard stream, which the platform frames by its
/// lines rather than by the call.
struct Raw {
    case: &'static str,
    /// The standard stream it is written to, as the node process names it.
    stream: &'static str,
    /// The bytes of that one write.
    written: &'static str,
    /// The events it must leave, in order and exactly as the group holds them.
    expected: &'static [&'static str],
}

/// The raw writes. The write the handler never CLOSES is last, because the bytes of an
/// unterminated line are continued by whatever the run writes to that stream next.
const RAW_ROWS: &[Raw] = &[
    Raw {
        case: "one write to standard output carrying two lines",
        stream: "stdout",
        written: "RAWOUT-first\nRAWOUT-second\n",
        expected: &["RAWOUT-first\n", "RAWOUT-second\n"],
    },
    Raw {
        case: "one write to standard output carrying three lines",
        stream: "stdout",
        written: "RAWTHREE-a\nRAWTHREE-b\nRAWTHREE-c\n",
        expected: &["RAWTHREE-a\n", "RAWTHREE-b\n", "RAWTHREE-c\n"],
    },
    Raw {
        case: "one write to standard error carrying two lines",
        stream: "stderr",
        written: "RAWERR-first\nRAWERR-second\n",
        expected: &["RAWERR-first\n", "RAWERR-second\n"],
    },
    Raw {
        case: "a write the handler never closed with a break",
        stream: "stdout",
        written: "RAWOPEN-unclosed",
        expected: &["RAWOPEN-unclosed"],
    },
];

/// `text` as a JavaScript string literal — escaped by the JSON serializer rather than by hand,
/// because a JSON string literal is a JavaScript one.
fn js_literal(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// The handler the table runs: one console call per framed row, then the raw writes, in the
/// order the tables state them.
fn handler_source() -> String {
    let mut source = String::from("exports.handler = async () => {\n");
    for row in FRAMED_ROWS {
        source.push_str(&format!(
            "    console.{}({});\n",
            row.method,
            js_literal(row.logged)
        ));
    }
    for row in RAW_ROWS {
        source.push_str(&format!(
            "    process.{}.write({});\n",
            row.stream,
            js_literal(row.written)
        ));
    }
    source.push_str("    return { ok: true };\n};\n");
    source
}

/// The invocation the delivered records are attributed to, read off the opening frame the
/// platform wrote for this run — through the one reading of that frame this target has
/// ([`request_id_of`]), over the records of the stream rejoined into the log they are.
fn invocation_of(case: &str, messages: &[String]) -> String {
    let named = request_id_of(case, &messages.concat());
    assert!(
        crate::invoke_request_id::is_request_id(&named),
        "case {case:?}: expected the opening frame to name the invocation, got {named:?}"
    );
    named
}

/// Where the message carrying `token` sits among `messages`, and how many of them carry it.
fn carrying(messages: &[String], token: &str) -> (Option<usize>, usize) {
    let found: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.contains(token))
        .map(|(at, _)| at)
        .collect();
    (found.first().copied(), found.len())
}

/// RULE: each console call of a nodejs managed runtime reaches the function's log group as
/// exactly ONE event — four tab-separated fields, the whole logged string as the fourth, its
/// own breaks inside it and one break closing it — while a write straight to a standard stream
/// reaches it as one event per line.
#[tokio::test]
async fn one_console_call_is_one_event_carrying_the_whole_logged_string() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let source = handler_source();
    for (index, runtime) in NODE_RUNTIMES.iter().enumerate() {
        let case = *runtime;
        let router = crate::invocation_log_delivery::emulator();
        let name = format!("console-framing-{index}");
        let group = format!("{DEFAULT_GROUP_PREFIX}{name}");
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: &source,
                runtime,
                role: ROLE,
                log_group: None,
            },
        )
        .await;

        let (tail, payload) = invoke(&router, case, &name).await;
        assert_eq!(
            payload["ok"],
            json!(true),
            "case {case:?}: expected the handler to run to its own end, got {payload}"
        );
        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for one invocation, got {held:?}"
        );
        // The messages exactly as the group holds them: a record's own terminator is part of
        // the message, so nothing here takes it off.
        let messages: Vec<String> = events(&router, case, &group, &held[0])
            .await
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        let request_id = invocation_of(case, &messages);
        // The frames the handler's own records sit between. A missing one is a failure of its
        // own rather than a wider window for the records to be found in.
        let opened = carrying(&messages, START_PREFIX).0.unwrap_or_else(|| {
            panic!("case {case:?}: expected the run's opening frame, got {messages:?}")
        });
        let closed = carrying(&messages, END_PREFIX).0.unwrap_or_else(|| {
            panic!("case {case:?}: expected the run's closing frame, got {messages:?}")
        });

        for row in FRAMED_ROWS {
            let row_case = format!("{case} / {}", row.case);
            // EVERY line of the logged string is carried by one and the same event: the framed
            // record. A continuation line delivered on its own — or dropped — shows up here.
            let mut positions = Vec::new();
            for line in row.logged.split('\n').filter(|line| !line.is_empty()) {
                let (at, count) = carrying(&messages, line);
                assert_eq!(
                    count, 1,
                    "case {row_case:?}: expected exactly one event carrying {line:?}, got \
                     {count} in {messages:?}"
                );
                positions.push(at.unwrap_or_default());
            }
            let at = *positions.first().unwrap_or_else(|| {
                panic!("case {row_case:?}: every row logs at least one line of text")
            });
            assert!(
                positions.iter().all(|position| *position == at),
                "case {row_case:?}: expected every line of the message inside the one framed \
                 event, got the lines at {positions:?} in {messages:?}"
            );
            assert!(
                at > opened && at < closed,
                "case {row_case:?}: expected the record between the opening frame at {opened} \
                 and the closing one at {closed}, got {at}"
            );

            // The four fields: the instant, the invocation, the level, and the message with its
            // own breaks and the one that closes it.
            let message = &messages[at];
            let fields: Vec<&str> = message.splitn(FRAMED_FIELDS, FIELD_SEPARATOR).collect();
            assert_eq!(
                fields.len(),
                FRAMED_FIELDS,
                "case {row_case:?}: expected {FRAMED_FIELDS} tab-separated fields, got \
                 {message:?}"
            );
            assert!(
                is_instant(fields[0]),
                "case {row_case:?}: expected the instant the record was written, got {:?}",
                fields[0]
            );
            assert_eq!(
                fields[1], request_id,
                "case {row_case:?}: expected the record to name the invocation \
                 {request_id:?}, got {:?}",
                fields[1]
            );
            assert_eq!(
                fields[2], row.level,
                "case {row_case:?}: expected the level {:?}, got {:?}",
                row.level, fields[2]
            );
            let expected = format!("{}{RECORD_TERMINATOR}", row.logged);
            assert_eq!(
                fields[3], expected,
                "case {row_case:?}: expected the whole logged string as the message, got {:?}",
                fields[3]
            );
            // The log tail the invocation itself reported carries the same message, so the two
            // readings of one run's output agree on it.
            assert!(
                tail.contains(row.logged),
                "case {row_case:?}: expected the invocation's own log tail to carry the whole \
                 message, got {tail:?}"
            );
        }

        for row in RAW_ROWS {
            let row_case = format!("{case} / {}", row.case);
            let first = row.expected[0];
            let at = carrying(&messages, first).0.unwrap_or_else(|| {
                panic!("case {row_case:?}: expected an event carrying {first:?}, got {messages:?}")
            });
            let delivered: Vec<&str> = messages
                .iter()
                .skip(at)
                .take(row.expected.len())
                .map(String::as_str)
                .collect();
            assert_eq!(
                delivered, row.expected,
                "case {row_case:?}: expected one event per line of the write, got {delivered:?}"
            );
        }

        // One event per console call and one per raw line, and nothing else beside the
        // platform's own frames: a message split into several events, or a call that left
        // none, changes this count.
        let own: usize = FRAMED_ROWS.len()
            + RAW_ROWS
                .iter()
                .map(|row| row.expected.len())
                .sum::<usize>();
        assert_eq!(
            messages.len(),
            own + FRAMING_RECORDS,
            "case {case:?}: expected {own} event(s) of the handler's own beside the platform's \
             {FRAMING_RECORDS} frames, got {messages:?}"
        );
        let frames: Vec<&String> = messages
            .iter()
            .filter(|message| {
                [
                    INIT_START_PREFIX,
                    START_PREFIX,
                    END_PREFIX,
                    REPORT_PREFIX,
                ]
                .iter()
                .any(|prefix| message.starts_with(prefix))
            })
            .collect();
        assert_eq!(
            frames.len(),
            FRAMING_RECORDS,
            "case {case:?}: expected the platform's own frames around the run, got {frames:?}"
        );
    }
}
