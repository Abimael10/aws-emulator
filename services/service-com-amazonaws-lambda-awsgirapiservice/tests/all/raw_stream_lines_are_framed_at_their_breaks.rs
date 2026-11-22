// SPDX-License-Identifier: CC0-1.0

//! WHERE a handler's own output is cut into the events of the function's log group: at the
//! LINE BREAKS it wrote, never at the boundaries of the write calls it made them in.
//!
//! RULE: what a function writes straight to one of its standard streams is a byte STREAM, and
//! Lambda frames it into one CloudWatch Logs event per newline-terminated line, each event
//! carrying the break that closed it. So the framing follows the breaks alone: several lines
//! handed to one write call are still one event each, a line spread over several write calls is
//! still one event, a break that opens a call closes the line the call before it left open, a
//! line with nothing on it is an event of its own, and text the run never closed is the one
//! record it is. A record the managed runtime's own logging interface handed the platform
//! WHOLE (a `console` call, a `logging` record) is one event however many breaks its formatted
//! message carries, which is what keeps a multi-line diagnostic — a traceback, a tabular
//! report — attributable to the one call that reported it.
//!
//! That is what makes a function's log usable: a metric filter counting a line, a subscription
//! filter forwarding it and an operator grepping a stream all match events, so two lines
//! delivered as one event are one match where a client expects two, and the re-joined events of
//! a stream are the bytes the run wrote.
//!
//! Recorded behaviour only: how a run's log is framed is no part of the Smithy model — `Invoke`
//! models the answer to the CALL and not the log it leaves
//! (`smithy_query(query="operation",operation="Invoke")`) — so the rule above is the platform's
//! documented log behaviour, asserted here through the readers a client has.
//!
//! Each runtime drives ONE run carrying every row, in the order the table states them, over real
//! HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch Logs and IAM on ONE
//! engine — the harness [`crate::invocation_log_delivery`] owns, reused rather than copied — and
//! the delivered events are read back both per stream (`GetLogEvents`) and across the group
//! (`FilterLogEvents`), which is the read a client that has just invoked the function makes. The
//! rows share that one run, which is why the row whose last line the run never closes is last.
//!
//! The rows differ along what the rule generalises over: how many write calls the lines were
//! spread across (one, two, three), how many lines one call carried (one, two, three), where the
//! breaks sit relative to the calls (a call that closes no line, a call opening on a break, a
//! break on its own, a call carrying nothing at all), which standard stream took the write, and
//! whether the run closed its last line. The whole table runs on a nodejs and on a python
//! managed runtime, because the framing is the PLATFORM's and not one runtime's.
//!
//! What the neighbouring suites pin and this one does not restate:
//! [`crate::invocation_log_delivery`] owns WHERE a run's log goes and which of the three log
//! writes the execution role allows, [`crate::node_console_record_framing`] owns the FIELDS one
//! node console record is rendered into, and [`crate::invocation_log_record_format`] owns which
//! form the records take under each `LogFormat`. What only this suite reaches is the independence
//! of the framing from the WRITE CALLS — successive calls, lines spread across calls, a line
//! written in pieces, a line with nothing on it, a call carrying nothing — and that the same
//! table comes out the same on a python runtime; the rows that also appear in that node suite (a
//! single call carrying several lines, a line the run never closed, and the whole-record
//! contrast) are restated on purpose, because a table over the write calls is only read against
//! the one-call case if that case is in it.
//!
//! No expected value here comes from a recording: every line the table asserts is a line the
//! same table had the handler write.
//!
//! Every test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::json;

use crate::invocation_log_delivery::{
    create_function, emulator, events, invoke, searched, streams, Created, NODE_RUNTIME,
    PYTHON_RUNTIME,
};

/// A well-formed execution role ARN the IAM store this harness mounts holds no record for, so
/// nothing is known about the role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The prefix of the log group a function whose configuration names none logs to.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The records the PLATFORM writes around one run of an environment it built, by the opening of
/// each: the record of the initialization, the invocation's opening frame and its two closing
/// ones. A function naming no `LogFormat` takes the plain-text one, which is the form these are
/// read in here — and no record of the handler's own opens with any of them.
const PLATFORM_FRAMES: [&str; 4] = [
    "INIT_START Runtime Version: ",
    "START RequestId: ",
    "END RequestId: ",
    "REPORT RequestId: ",
];

/// Which standard stream one row's writes go to.
#[derive(Debug, Clone, Copy)]
enum Channel {
    /// The process's standard output.
    StandardOutput,
    /// Its standard error.
    StandardError,
}

/// One thing the handler reports by writing STRAIGHT to a standard stream: the calls it makes,
/// and the events those calls must leave in the group.
struct Written {
    case: &'static str,
    /// The stream the calls are made on.
    channel: Channel,
    /// The strings the handler writes, in order, one write call per entry.
    writes: &'static [&'static str],
    /// The events the calls must leave, in order and exactly as a reader hands them: a message
    /// is the line with the break that closed it, and no break at all for a line the run never
    /// closed.
    expected: &'static [&'static str],
}

/// The table. The row whose last line the run never CLOSES is last, because the bytes of an
/// unterminated line are continued by whatever that run writes next.
const ROWS: &[Written] = &[
    Written {
        case: "two successive calls, each writing one whole line",
        channel: Channel::StandardOutput,
        writes: &["SUCCESSIVE-first\n", "SUCCESSIVE-second\n"],
        expected: &["SUCCESSIVE-first\n", "SUCCESSIVE-second\n"],
    },
    Written {
        case: "three successive calls, each writing one whole line",
        channel: Channel::StandardOutput,
        writes: &["TRIPLE-a\n", "TRIPLE-b\n", "TRIPLE-c\n"],
        expected: &["TRIPLE-a\n", "TRIPLE-b\n", "TRIPLE-c\n"],
    },
    Written {
        case: "one call carrying two whole lines",
        channel: Channel::StandardOutput,
        writes: &["ONECALL-first\nONECALL-second\n"],
        expected: &["ONECALL-first\n", "ONECALL-second\n"],
    },
    Written {
        case: "one call carrying three whole lines",
        channel: Channel::StandardOutput,
        writes: &["ONECALLTHREE-a\nONECALLTHREE-b\nONECALLTHREE-c\n"],
        expected: &[
            "ONECALLTHREE-a\n",
            "ONECALLTHREE-b\n",
            "ONECALLTHREE-c\n",
        ],
    },
    Written {
        case: "three lines spread unevenly over two calls, the second opening on the break that \
               closes the line the first left open",
        channel: Channel::StandardOutput,
        writes: &["SPREAD-a\nSPREAD-b", "\nSPREAD-c\n"],
        expected: &["SPREAD-a\n", "SPREAD-b\n", "SPREAD-c\n"],
    },
    Written {
        case: "one line written in pieces over three calls, none of the first two closing it",
        channel: Channel::StandardOutput,
        writes: &["PIECED-one", "-two", "-three\n"],
        expected: &["PIECED-one-two-three\n"],
    },
    Written {
        case: "a line with nothing on it between two lines that carry text",
        channel: Channel::StandardOutput,
        writes: &["BLANK-above\n", "\n", "BLANK-below\n"],
        expected: &["BLANK-above\n", "\n", "BLANK-below\n"],
    },
    Written {
        case: "a call carrying nothing at all between two whole lines",
        channel: Channel::StandardOutput,
        writes: &["EMPTYCALL-above\n", "", "EMPTYCALL-below\n"],
        expected: &["EMPTYCALL-above\n", "EMPTYCALL-below\n"],
    },
    Written {
        case: "two successive calls on standard error, each writing one whole line",
        channel: Channel::StandardError,
        writes: &["ERRSUCC-first\n", "ERRSUCC-second\n"],
        expected: &["ERRSUCC-first\n", "ERRSUCC-second\n"],
    },
    Written {
        case: "a last line the run never closed with a break",
        channel: Channel::StandardError,
        writes: &["UNCLOSED-tail"],
        expected: &["UNCLOSED-tail"],
    },
];

/// The one message the handler hands its runtime's own logging interface, which frames it as a
/// WHOLE record: the contrast the rule draws, so the table's per-line framing is not read as a
/// rule about line breaks wherever they appear.
const FRAMED_LOGGED: &str = "FRAMED-first\nFRAMED-second\nFRAMED-third";

/// What every line of that message carries, which is how its one record is told from the
/// handler's raw ones.
const FRAMED_TOKEN: &str = "FRAMED-";

/// The language of a managed runtime, which decides how the table's calls are written and how a
/// whole record is handed to the platform.
#[derive(Debug, Clone, Copy)]
enum Language {
    Node,
    Python,
}

/// One managed runtime the table runs on.
struct Target {
    /// The `Runtime` the function is created with — the two this target's suites run on
    /// ([`crate::invocation_log_delivery`]'s `NODE_RUNTIME` and `PYTHON_RUNTIME`), each a member
    /// of the model's own `Runtime` enum (`smithy_query(query="shape",shape="Runtime")`).
    runtime: &'static str,
    language: Language,
}

/// The two runtimes: the framing is the platform's, so one row of the table has to come out the
/// same whichever language wrote it.
const TARGETS: &[Target] = &[
    Target {
        runtime: NODE_RUNTIME,
        language: Language::Node,
    },
    Target {
        runtime: PYTHON_RUNTIME,
        language: Language::Python,
    },
];

/// `text` as a string literal of either language — escaped by the JSON serializer rather than by
/// hand, because a JSON string literal is both a JavaScript and a python one.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

impl Target {
    /// The deployment package's one module: the framed record first, then every write of the
    /// table in the order it states them.
    fn source(&self) -> String {
        match self.language {
            Language::Node => {
                let mut source = String::from("exports.handler = async () => {\n");
                source.push_str(&format!("    console.log({});\n", quoted(FRAMED_LOGGED)));
                for row in ROWS {
                    let stream = match row.channel {
                        Channel::StandardOutput => "stdout",
                        Channel::StandardError => "stderr",
                    };
                    for written in row.writes {
                        source.push_str(&format!(
                            "    process.{stream}.write({});\n",
                            quoted(written)
                        ));
                    }
                }
                source.push_str("    return { ok: true };\n};\n");
                source
            }
            Language::Python => {
                let mut source = String::from(
                    "import logging\nimport sys\n\n_logger = logging.getLogger()\n_logger\
                     .setLevel(logging.INFO)\n\n\ndef lambda_handler(event, context):\n",
                );
                source.push_str(&format!("    _logger.info({})\n", quoted(FRAMED_LOGGED)));
                for row in ROWS {
                    let stream = match row.channel {
                        Channel::StandardOutput => "stdout",
                        Channel::StandardError => "stderr",
                    };
                    for written in row.writes {
                        source.push_str(&format!(
                            "    sys.{stream}.write({})\n",
                            quoted(written)
                        ));
                    }
                }
                source.push_str("    return {\"ok\": True}\n");
                source
            }
        }
    }
}

/// Whether `message` is one of the records the platform wrote itself around the run.
fn is_platform_frame(message: &str) -> bool {
    PLATFORM_FRAMES
        .iter()
        .any(|frame| message.starts_with(frame))
}

/// RULE: a handler's writes to its standard streams reach the function's log group as one event
/// per newline-terminated line — whatever the write calls those lines were made in — while a
/// record the runtime handed the platform whole stays one event carrying every line of its
/// message.
#[tokio::test]
async fn raw_stream_lines_are_one_event_each_however_they_were_written() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    // The events of the handler's own raw writes, in the order the table states them, and the
    // bytes the run wrote to its streams — the two readings every row is judged against.
    let expected: Vec<&str> = ROWS
        .iter()
        .flat_map(|row| row.expected.iter().copied())
        .collect();
    let written: String = ROWS
        .iter()
        .flat_map(|row| row.writes.iter().copied())
        .collect();

    for (index, target) in TARGETS.iter().enumerate() {
        let case = target.runtime;
        let router = emulator();
        let name = format!("raw-line-framing-{index}");
        let group = format!("{DEFAULT_GROUP_PREFIX}{name}");
        let source = target.source();
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: &source,
                runtime: target.runtime,
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
        // The messages exactly as the group holds them: a record's own terminator is part of the
        // message, so nothing here takes it off.
        let messages: Vec<String> = events(&router, case, &group, &held[0])
            .await
            .into_iter()
            .map(|(_, message)| message)
            .collect();

        // The platform's own frames are each there once, so the records between them are the
        // handler's own and the count below is not read against a missing frame.
        for frame in PLATFORM_FRAMES {
            let found = messages
                .iter()
                .filter(|message| message.starts_with(frame))
                .count();
            assert_eq!(
                found, 1,
                "case {case:?}: expected one record opening {frame:?}, got {found} in \
                 {messages:?}"
            );
        }

        // 1. The handler's RAW records, in order and byte for byte: one per line the run wrote,
        //    whatever write call carried it. A line delivered with another, or split from its
        //    own break, shows up here.
        let raw: Vec<&str> = messages
            .iter()
            .filter(|message| !is_platform_frame(message) && !message.contains(FRAMED_TOKEN))
            .map(String::as_str)
            .collect();
        assert_eq!(
            raw, expected,
            "case {case:?}: expected one event per line the run wrote, got {raw:?}"
        );

        // 2. Row by row, so a failure names the write pattern it belongs to — and the
        //    invocation's own log tail carries the row's bytes as the run wrote them, which is
        //    the second reading of the same output.
        for row in ROWS {
            let row_case = format!("{case} / {}", row.case);
            for message in row.expected {
                let found = raw.iter().filter(|held| *held == message).count();
                assert_eq!(
                    found, 1,
                    "case {row_case:?}: expected exactly one event carrying {message:?}, got \
                     {found} in {raw:?}"
                );
            }
            let bytes = row.writes.concat();
            assert!(
                tail.contains(&bytes),
                "case {row_case:?}: expected the invocation's own log tail to carry {bytes:?}, \
                 got {tail:?}"
            );
        }

        // 3. Re-joining those events gives back exactly the bytes the run wrote to its streams:
        //    the framing cuts the stream up and adds nothing to it.
        let rejoined: String = raw.concat();
        assert_eq!(
            rejoined, written,
            "case {case:?}: expected the events to rejoin into the bytes the run wrote, got \
             {rejoined:?}"
        );

        // 4. The contrast: the message handed to the runtime's logging interface is ONE event
        //    carrying every one of its lines, so a break inside a whole record starts no event.
        let framed: Vec<&String> = messages
            .iter()
            .filter(|message| message.contains(FRAMED_TOKEN))
            .collect();
        assert_eq!(
            framed.len(),
            1,
            "case {case:?}: expected the runtime's record to be one event, got {framed:?}"
        );
        for line in FRAMED_LOGGED.split('\n') {
            assert!(
                framed[0].contains(line),
                "case {case:?}: expected the one record to carry {line:?}, got {:?}",
                framed[0]
            );
        }

        // 5. The same reading across the whole group, which is the read a client knowing the
        //    group but not the stream makes.
        let found = searched(&router, case, &group).await;
        for message in &expected {
            let carried = found.iter().filter(|held| held.as_str() == *message).count();
            assert_eq!(
                carried, 1,
                "case {case:?}: expected FilterLogEvents to report exactly one event carrying \
                 {message:?}, got {carried} in {found:?}"
            );
        }
    }
}
