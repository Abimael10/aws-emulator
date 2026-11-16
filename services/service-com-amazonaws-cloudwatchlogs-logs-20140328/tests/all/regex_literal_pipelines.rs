// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a Logs Insights QL pipeline whose `parse` or `filter` pattern is a
//! REGULAR EXPRESSION carrying quote characters.
//!
//! The rule under test: a quote inside a regular-expression literal is a character of the
//! expression, not the delimiter of a string — so a pipeline whose pattern matches quoted text
//! (the idiom for pulling a value out of a JSON line, `/"tag": "(?<tg>[^"]+)"/`) still splits
//! into the stages the `|` separators mark, compiles, and runs. A `/` the query means as
//! DIVISION opens no literal, and a `|` inside a literal is part of the expression rather than
//! a stage separator.
//!
//! The tables span what the rule generalises over: how many quotes the pattern carries (an odd
//! number is what a pattern matching one quoted value has, an even one what a pattern matching
//! two has), which command carries the pattern (`parse`, `filter`), whether a stage follows the
//! one holding it, whether the pattern itself carries a `|`, and whether the `/` characters in a
//! stage are division rather than a literal.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::query_compile::compile;

use crate::harness::{batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";

/// The log group and stream every case queries.
const GROUP: &str = "/insights/regex-literals";
const STREAM: &str = "stream";

/// The events every case queries: JSON lines whose values are quoted, which is what a pattern
/// matching quoted text is written for.
const MESSAGES: [&str; 2] = [
    r#"{"tag": "alpha", "ms": 20}"#,
    r#"{"tag": "beta", "ms": 40}"#,
];

/// How long a case waits for a started query to finish before giving up — generous room for a
/// loaded host, since what is waited for is the status the query reports and never this bound.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that status is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The log group holding [`MESSAGES`], and the query window in the epoch SECONDS
/// `StartQuery`'s range is specified in.
async fn group_with_events(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    let base = now_ms() / 1_000 * 1_000 - 60 * 1_000;
    let events: Vec<(i64, &str)> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| (base + index as i64 * 1_000, *message))
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
    let now_secs = now_ms() / 1_000;
    (now_secs - 3_600, now_secs + 60)
}

/// Start `query_string`, wait until it reports a finished status, and answer its rows as
/// field/value maps.
///
/// The wait is on the CONDITION the query reaches, looked at again until it holds or
/// [`COMPLETION_DEADLINE`] passes, so nothing here depends on how fast the host runs.
async fn completed_rows(
    logs: &Logs,
    case: &str,
    window: (i64, i64),
    query_string: &str,
) -> Vec<BTreeMap<String, String>> {
    let (status, body) = logs
        .call(
            START,
            &json!({
                "logGroupName": GROUP,
                "startTime": window.0,
                "endTime": window.1,
                "queryString": query_string,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the query to be accepted, got {status} with {body}"
    );
    let id = body["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an accepted start answers a queryId, got {body}"))
        .to_string();
    let deadline = std::time::Instant::now() + COMPLETION_DEADLINE;
    loop {
        let answer = logs.ok(RESULTS, &json!({"queryId": id})).await;
        if answer["status"].as_str() == Some("Complete") {
            return answer["results"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: results are a list, got {answer}"))
                .iter()
                .map(|row| {
                    row.as_array()
                        .unwrap_or_else(|| panic!("case {case:?}: a row is a list, got {row}"))
                        .iter()
                        .map(|field| {
                            (
                                field["field"].as_str().unwrap_or_default().to_string(),
                                field["value"].as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect()
                })
                .collect();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the query never completed, last answer {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a pipeline whose pattern carries quotes runs, and answers the values the pattern
/// pulled out of the quoted text.
#[tokio::test]
async fn a_pattern_carrying_quotes_runs_the_whole_pipeline() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    // (case, the query string, the field to read off each row, the values it must carry in row
    // order)
    let cases: Vec<(&str, &str, &str, Vec<&str>)> = vec![
        (
            // Five quotes: an odd number, so reading them as string delimiters would leave one
            // open and swallow the two separators that follow.
            "a capture of one quoted JSON value, with two stages after it",
            r#"parse @message /"tag": "(?<tg>[^"]+)"/ | sort tg asc | display tg"#,
            "tg",
            vec!["alpha", "beta"],
        ),
        (
            "the same capture with no stage after it",
            r#"parse @message /"tag": "(?<tg>[^"]+)"/ | sort tg desc"#,
            "tg",
            vec!["beta", "alpha"],
        ),
        (
            "a filter matching quoted text, with a stage after it",
            r#"filter @message like /"tag": "alpha"/ | display @message"#,
            "@message",
            vec![MESSAGES[0]],
        ),
        (
            // An even number of quotes AND a `|` of its own: the alternation is part of the
            // expression, so it neither splits the pipeline nor pairs with anything.
            "a filter whose pattern alternates two quoted values",
            r#"filter @message like /"alpha"|"beta"/ | sort @timestamp asc | display @message"#,
            "@message",
            vec![MESSAGES[0], MESSAGES[1]],
        ),
        (
            "a capture naming the field in a later stats stage",
            r#"parse @message /"tag": "(?<tg>[^"]+)"/ | stats count(*) as n by tg | sort tg asc"#,
            "n",
            vec!["1", "1"],
        ),
    ];

    for (case, query_string, field, expected) in &cases {
        let produced = completed_rows(&logs, case, window, query_string).await;
        let values: Vec<String> = produced
            .iter()
            .map(|row| row.get(*field).cloned().unwrap_or_default())
            .collect();
        assert_eq!(
            values,
            expected
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected {field} to read {expected:?}, got {produced:?}"
        );
    }
}

/// RULE: the stage split itself is unchanged — a pattern's quotes and `|` stay inside the stage
/// that carries them, and a `/` the query means as division opens no literal.
#[test]
fn the_stages_a_pipeline_splits_into_are_the_commands_it_names() {
    // (case, the query string, the commands the pipeline must compile to)
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "a parse pattern matching one quoted value, then two stages",
            r#"parse @message /"tag": "(?<tg>[^"]+)"/ | sort tg asc | display tg"#,
            vec!["parse", "sort", "display"],
        ),
        (
            "a filter pattern matching quoted text, then a stage",
            r#"filter @message like /"tag": "alpha"/ | display @message"#,
            vec!["filter", "display"],
        ),
        (
            "a pattern whose own alternation carries a command word",
            r#"filter @message like /"alpha"|"display"/ | limit 1"#,
            vec!["filter", "limit"],
        ),
        (
            // Two `/` the query means as division, with no `|` between them: the stage is read
            // exactly as written.
            "two divisions in one stage, then a stage",
            "fields ms / 2 as half, ms / 4 as quarter | sort half desc",
            vec!["fields", "sort"],
        ),
        (
            "one division and a later stage",
            "fields ms / 2 as half | limit 1",
            vec!["fields", "limit"],
        ),
        (
            "a glob pattern in quotes, then a stage",
            r#"parse @message "tag=*" as tg | display tg"#,
            vec!["parse", "display"],
        ),
        (
            // A `/` meant as division opens no literal, so it cannot pair with a `/` standing
            // inside a quoted value later in the same stage: that value's quotes still pair,
            // and the separator after it still separates.
            "a division and a quoted value carrying a slash, then a stage",
            r#"filter ms / 1000 > 1 and path = "/health" | limit 10"#,
            vec!["filter", "limit"],
        ),
        (
            // The alternation inside a pattern is the expression's own, whichever word follows
            // it — including a word that is a command of the language.
            "a pattern whose alternation spells a command word, then a stage",
            r"filter @message like /WARN|limit/ | limit 10",
            vec!["filter", "limit"],
        ),
        (
            "a pattern after the symbol match operator, with no space before it",
            r"filter @message =~/GET|fields/ | limit 1",
            vec!["filter", "limit"],
        ),
    ];

    for (case, query_string, commands) in &cases {
        let compiled = compile(query_string)
            .unwrap_or_else(|failure| panic!("case {case:?}: expected {query_string:?} to compile, got {failure:?}"));
        assert_eq!(
            compiled.commands,
            commands
                .iter()
                .map(|command| (*command).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the stages {commands:?}, got {:?}",
            compiled.commands
        );
        // The arguments travel exactly as written, so the text each stage runs over is the
        // caller's own.
        let rebuilt: String = compiled
            .stages
            .iter()
            .map(|stage| format!("{}{}", stage.command, stage.arguments))
            .collect::<Vec<String>>()
            .join(" | ");
        assert_eq!(
            rebuilt.replace(' ', "").to_lowercase(),
            query_string.replace(' ', "").to_lowercase(),
            "case {case:?}: the stages carry the query's own text, got {rebuilt:?}"
        );
    }
}

/// What the rule does NOT change: a `|` that separates two stages still separates them when the
/// stage before it carries a pattern, and a word after a separator that is no command is still
/// the query's own mistake.
#[test]
fn a_separator_after_a_pattern_still_separates_the_stages() {
    let refused = compile(r#"parse @message /"tag": "(?<tg>[^"]+)"/ | frobnicate tg"#)
        .expect_err("a word that is no command of the language does not compile");
    assert!(
        refused.message.contains("frobnicate"),
        "the refusal names the word that is no command, got {refused:?}"
    );
}
