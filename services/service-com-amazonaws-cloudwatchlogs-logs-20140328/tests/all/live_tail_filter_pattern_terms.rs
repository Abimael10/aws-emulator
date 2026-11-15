// SPDX-License-Identifier: CC0-1.0

//! What `StartLiveTail` admits as its `logEventFilterPattern`: the terms of an unstructured
//! filter pattern, end to end.
//!
//! RULE: a bare unquoted term of the filter-and-pattern syntax is matched as a substring of
//! the log event's message and is spelled with the syntax's ordinary word characters — the
//! alphanumerics, `_` and the `-` a word carries inside it — the hyphen being special only
//! where a term STARTS, as the exclusion marker of a `-term`; a pattern whose terms are
//! spelled that way is therefore admitted by every member that takes a `FilterPattern` under
//! the modelled `@length(0..1024)` bound alone (source: `smithy_query(query="shape",
//! shape="FilterPattern")` — a plain string with that length and no `@pattern`), so
//! `StartLiveTail` answers one with HTTP 200 and a session that simply delivers the events
//! the pattern matches — none at all, for a term no event carries, which is the open idle
//! session a caller tails to watch for FUTURE events. Only a term carrying a character the
//! grammar admits nowhere, or a pattern past the modelled ceiling, is refused, as
//! `InvalidParameterException` at HTTP 400.
//!
//! The rows of the tables below differ along what the rule generalises over: where in a term
//! the hyphen sits (inside a word, between words, between a word and digits, through a dashed
//! identifier), which prefix the term carries (none, the `-` exclusion marker, the `?`
//! alternative marker), how many terms the pattern names, which other ordinary character the
//! term carries (`_`, digits), how much of the modelled length bound the pattern uses (one
//! character, the whole ceiling, one past it), and — for the refused rows — which character
//! the syntax admits in no bare term.
//!
//! The sibling suites cover the same syntax where it is already pinned: `FilterLogEvents` and
//! `TestMetricFilter` run the term table in `filter_log_events_search_reporting`, and
//! `live_tail_sessions` covers a session's selection, framing and refusals. This one is about
//! the pattern a live tail is STARTED with.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::filter_pattern::MAX_PATTERN_CHARS;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::ComAmazonawsCloudwatchlogsLogs20140328State;

use crate::harness::{base_ms, batch, Logs};
use crate::live_tail_sessions::{decode_frames, delivered_by};

/// The log group and stream every session below tails.
const GROUP: &str = "/aws/emulator/live-tail-terms";
const STREAM: &str = "terms-one";

/// The media type an event-stream body carries.
const EVENT_STREAM: &str = "application/vnd.amazon.eventstream";

/// The `:event-type` of the frame an event-stream response opens with (source:
/// `service_common::event_stream::INITIAL_RESPONSE_EVENT`).
const INITIAL_RESPONSE: &str = "initial-response";

/// How long a wait for a condition may take before the test gives up on it. Generous on
/// purpose: nothing here is timed, so a loaded host only has to reach the condition
/// eventually.
const CONDITION_DEADLINE: Duration = Duration::from_secs(120);

/// How often a wait re-reads the condition it is waiting for.
const CONDITION_POLL: Duration = Duration::from_millis(5);

/// The messages every row's session is handed, ingested while it is open. Their words carry
/// hyphens in the places a realistic log line does: inside a service name, between a name and
/// an instance number, and through a dashed identifier.
const INGESTED: [&str; 4] = [
    "start order-service-7 ready",
    "stop order-service-7 halted",
    "request 4f8c2a1e-0b71-4a6e-9f3a-2d5c1b7e8a90 handled",
    "cache_warm complete",
];

/// One admitted-pattern row: the pattern the session is started with and the indexes into
/// [`INGESTED`] of the events it must deliver.
struct Row {
    case: &'static str,
    pattern: Pattern,
    delivered: &'static [usize],
}

/// How a row spells its pattern: as written, or as the longest one the model admits (built at
/// run time from the modelled ceiling).
enum Pattern {
    Written(&'static str),
    /// A single term of exactly [`MAX_PATTERN_CHARS`] ordinary word characters.
    LongestAdmissible,
}

impl Pattern {
    fn spelled(&self) -> String {
        match self {
            Self::Written(pattern) => (*pattern).to_string(),
            Self::LongestAdmissible => longest_admissible_term(),
        }
    }
}

/// A bare term of exactly the modelled `FilterPattern` ceiling, spelled with the ordinary word
/// characters a term admits: letters and the hyphens between them. It starts with a letter, so
/// its leading character is part of the term rather than the exclusion marker, and no message
/// carries it.
fn longest_admissible_term() -> String {
    let mut term = String::new();
    while term.chars().count() < MAX_PATTERN_CHARS {
        // A hyphen every fifth character, so the term is a run of hyphen-joined words rather
        // than one undifferentiated word.
        term.push(if term.chars().count() % 5 == 4 { '-' } else { 'q' });
    }
    term
}

/// The admitted-pattern table.
static ROWS: [Row; 9] = [
    Row {
        case: "a single hyphenated term no message carries, which is the idle session",
        pattern: Pattern::Written("absent-from-every-message"),
        delivered: &[],
    },
    Row {
        case: "a hyphenated term two messages carry inside a longer word",
        pattern: Pattern::Written("order-service"),
        delivered: &[0, 1],
    },
    Row {
        case: "a hyphenated term ending in digits",
        pattern: Pattern::Written("order-service-7"),
        delivered: &[0, 1],
    },
    Row {
        case: "a dashed identifier one message carries",
        pattern: Pattern::Written("4f8c2a1e-0b71-4a6e-9f3a-2d5c1b7e8a90"),
        delivered: &[2],
    },
    Row {
        case: "a hyphenated term and a plain one, both required",
        pattern: Pattern::Written("order-service-7 halted"),
        delivered: &[1],
    },
    Row {
        case: "a leading hyphen excluding a hyphenated term",
        pattern: Pattern::Written("-order-service"),
        delivered: &[2, 3],
    },
    Row {
        case: "the alternative prefix on a hyphenated and an underscored term",
        pattern: Pattern::Written("?order-service ?cache_warm"),
        delivered: &[0, 1, 3],
    },
    Row {
        case: "a word carrying an underscore",
        pattern: Pattern::Written("cache_warm"),
        delivered: &[3],
    },
    Row {
        case: "a term of exactly the modelled length ceiling",
        pattern: Pattern::LongestAdmissible,
        delivered: &[],
    },
];

/// A session started with a pattern the term grammar admits is served, and delivers exactly
/// the events the pattern matches — including none at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pattern_of_ordinary_term_characters_opens_a_session_that_matches_it() {
    // Each row tails its OWN emulator, so every row's session runs at the same time: a
    // session stays open for the seconds its buffer holds updates for, which running the
    // table one row at a time would wait out once per row.
    let mut running = Vec::new();
    for row in &ROWS {
        running.push(tokio::spawn(check_pattern(row)));
    }
    for row in running {
        row.await.expect("a pattern row's task did not panic");
    }
}

/// One row of [`ROWS`]: open the session, ingest [`INGESTED`] while it is open, and assert
/// what it delivered.
async fn check_pattern(row: &'static Row) {
    let case = row.case;
    let pattern = row.pattern.spelled();
    let logs = Arc::new(Logs::new());
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;

    let request = json!({
        "logGroupIdentifiers": [GROUP],
        "logEventFilterPattern": pattern,
    });
    let serving = {
        let logs = Arc::clone(&logs);
        tokio::spawn(async move { logs.call_raw("StartLiveTail", &request).await })
    };
    // Only the events ingested while the session is open are delivered, so the write waits
    // for the session to be registered — polling for that condition, never sleeping a span.
    await_open_session(case, &logs).await;
    ingest(&logs).await;

    let (status, headers, body) = serving
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the session task panicked: {error}"));
    assert_eq!(
        status,
        200,
        "case {case:?}: expected HTTP 200 for the pattern {pattern:?}, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    let media_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    assert_eq!(
        media_type, EVENT_STREAM,
        "case {case:?}: expected an event-stream body, got {media_type:?}"
    );

    let frames = decode_frames(case, &body);
    let types: Vec<&str> = frames
        .iter()
        .map(|frame| frame.event_type.as_str())
        .collect();
    assert_eq!(
        types.first().copied(),
        Some(INITIAL_RESPONSE),
        "case {case:?}: a served session opens with the frame the output arrives in, got \
         {types:?}"
    );
    assert_eq!(
        types.get(1).copied(),
        Some("sessionStart"),
        "case {case:?}: an admitted pattern starts a session, got {types:?}"
    );
    assert!(
        types.iter().skip(2).all(|event| *event == "sessionUpdate"),
        "case {case:?}: every frame behind the sessionStart is an update, got {types:?}"
    );
    assert!(
        types.len() > 2,
        "case {case:?}: an open session sends at least one update, got {types:?}"
    );

    // The session echoes the pattern it was started with, byte for byte: what it admitted is
    // what the request spelled.
    let start = &frames[1].payload;
    assert_eq!(
        start["logEventFilterPattern"],
        json!(pattern),
        "case {case:?}: sessionStart echoes the logEventFilterPattern, got {start}"
    );

    let delivered: Vec<String> = frames
        .iter()
        .skip(2)
        .flat_map(|frame| delivered_by(case, &frame.payload))
        .map(|(message, _, _)| message)
        .collect();
    let expected: Vec<String> = row
        .delivered
        .iter()
        .map(|index| INGESTED[*index].to_string())
        .collect();
    assert_eq!(
        delivered, expected,
        "case {case:?}: expected the pattern {pattern:?} to deliver {expected:?}, got \
         {delivered:?}"
    );
}

/// Write [`INGESTED`] as one batch, stamped inside the ingestion window.
async fn ingest(logs: &Logs) {
    let at = base_ms();
    let events: Vec<(i64, &str)> = INGESTED
        .iter()
        .enumerate()
        .map(|(index, message)| (at + i64::try_from(index).unwrap_or_default(), *message))
        .collect();
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": STREAM,
            "logEvents": batch(&events),
        }),
    )
    .await;
}

/// How many Live Tail sessions the store holds as OPEN.
fn open_sessions(logs: &Logs) -> usize {
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let store = state.lock();
    store.live_tail_sessions.len()
}

/// Wait until the store holds an open session, which is what says the session has SUBSCRIBED:
/// only what is ingested after that reaches it. Waits for the CONDITION rather than for a span
/// of the clock.
async fn await_open_session(case: &str, logs: &Logs) {
    let give_up_at = Instant::now() + CONDITION_DEADLINE;
    loop {
        let registered = open_sessions(logs);
        if registered >= 1 {
            return;
        }
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: expected the request to open a Live Tail session, got {registered}"
        );
        tokio::time::sleep(CONDITION_POLL).await;
    }
}

/// One refused-pattern row: the pattern, and the message the refusal states.
struct Refused {
    case: &'static str,
    pattern: Pattern,
    message: RefusalMessage,
}

/// How a refused row states the message it expects.
enum RefusalMessage {
    Written(&'static str),
    /// The modelled length bound, worded by this service's constraint envelope.
    PastTheLengthCeiling,
}

impl RefusalMessage {
    fn spelled(&self) -> String {
        match self {
            Self::Written(message) => (*message).to_string(),
            Self::PastTheLengthCeiling => format!(
                "1 validation error detected: Value at 'logEventFilterPattern' failed to \
                 satisfy constraint: Member must have length less than or equal to \
                 {MAX_PATTERN_CHARS}"
            ),
        }
    }
}

/// The refused-pattern table: a pattern past the modelled ceiling, and the characters the bare
/// term grammar admits nowhere — each refused term named in the refusal, so a caller can see
/// which part of the pattern to spell as a `"…"` string or a `%…%` expression instead.
static REFUSED: [Refused; 5] = [
    Refused {
        case: "one character past the modelled length ceiling",
        pattern: Pattern::LongestAdmissible,
        message: RefusalMessage::PastTheLengthCeiling,
    },
    Refused {
        case: "a slash opening a path-like term",
        pattern: Pattern::Written("GET /health"),
        message: RefusalMessage::Written("Invalid character(s) in term '/'"),
    },
    Refused {
        case: "a colon ending a bare term",
        pattern: Pattern::Written("order-service-7:"),
        message: RefusalMessage::Written("Invalid character(s) in term ':'"),
    },
    Refused {
        case: "an arrow between two hyphenated terms",
        pattern: Pattern::Written("order-service->cache-warm"),
        message: RefusalMessage::Written("Invalid character(s) in term '>'"),
    },
    Refused {
        case: "a quoted string that never closes",
        pattern: Pattern::Written("\"order-service"),
        message: RefusalMessage::Written("Invalid character(s) in term '\"order-service'"),
    },
];

/// A pattern the term grammar does not admit, and one past the modelled length bound, are
/// refused as the operation's modelled client error — and no session is opened for either.
#[tokio::test]
async fn a_pattern_the_term_grammar_rejects_refuses_the_session() {
    let logs = Logs::new();
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;

    for row in &REFUSED {
        let case = row.case;
        // The length row is the longest admissible term plus ONE character, which is what
        // puts it past the ceiling; every other row is the pattern as written.
        let pattern = match row.message {
            RefusalMessage::PastTheLengthCeiling => format!("{}q", row.pattern.spelled()),
            RefusalMessage::Written(_) => row.pattern.spelled(),
        };
        let expected = row.message.spelled();
        let (code, message) = logs
            .refusal(
                "StartLiveTail",
                &json!({
                    "logGroupIdentifiers": [GROUP],
                    "logEventFilterPattern": pattern,
                }),
            )
            .await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {code} with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        assert_eq!(
            open_sessions(&logs),
            0,
            "case {case:?}: a refused request opens no session"
        );
    }
}
