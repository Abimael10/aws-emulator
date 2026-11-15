// SPDX-License-Identifier: CC0-1.0

//! What a read of a log stream reports PER EVENT, and the form the continuation tokens of
//! that read travel in.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rules under test:
//!
//! * **Every event a read reports carries the id of the event** — an opaque fixed-width
//!   decimal id, unique among the events of the stream, the SAME id however the event is read
//!   (page size, direction, time window, masked or unmasked) and the same id the filtering
//!   read reports for it, which is the one the model declares the member for
//!   (`FilteredLogEvent$eventId`, "The ID of the event").
//! * **Every continuation token is a direction prefix and a FIXED-WIDTH decimal body** —
//!   `f/<body>`, `b/<body>`, with the trailing `/s` once that direction's scan has reached the
//!   end of the stream — so a client that persists or size-bounds a token sees one width
//!   whatever position, window or stream the token stands for, and no name of the request
//!   rides in it.
//!
//! The tables span what the rules generalise over — event count, page size, which end the
//! scan starts from, the time window, whether the read unmasks — rather than any one recorded
//! read; a token BODY is opaque, so no case asserts one.

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, group_with_stream, Logs};

/// The read under test, and the filtering read whose `eventId` member the model declares.
const GET: &str = "GetLogEvents";
const FILTER: &str = "FilterLogEvents";

/// How many characters a token's body and an event's id each carry. CloudWatch Logs renders
/// both at one fixed width; stated here independently of the crate under test, so a service
/// that widened or shortened either fails this suite.
const OPAQUE_WIDTH: usize = 56;

/// The log group and the log stream every case reads, named long enough that a token echoing
/// either of them back could not hide it inside one 56-character body.
const GROUP: &str = "/aws/vendedlogs/event-ids/a-destination-log-group-with-a-long-name";
const STREAM: &str = "a-log-stream-with-a-rather-long-name";

/// One token of a page, decomposed: the direction prefix, the body, and whether the
/// end-of-stream sentinel is there.
fn token_parts(case: &str, token: &str) -> (String, String, bool) {
    let (direction, rest) = token.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: a token carries a direction prefix, got {token:?}")
    });
    let (body, end_of_stream) = match rest.strip_suffix("/s") {
        Some(body) => (body, true),
        None => (rest, false),
    };
    (direction.to_string(), body.to_string(), end_of_stream)
}

/// Assert the form of both tokens of one page: each in its own direction's envelope, around a
/// body of exactly [`OPAQUE_WIDTH`] decimal digits that carries no name of the request.
fn assert_token_form(case: &str, page: &Value) {
    for (member, expected_direction) in [("nextForwardToken", "f"), ("nextBackwardToken", "b")] {
        let token = page
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: {member} is never null, got {page}"));
        let (direction, body, _) = token_parts(case, token);
        assert_eq!(
            direction, expected_direction,
            "case {case:?}: {member} carries the {expected_direction:?} prefix, got {token:?}"
        );
        assert_eq!(
            body.chars().count(),
            OPAQUE_WIDTH,
            "case {case:?}: {member} carries a {OPAQUE_WIDTH}-character body, got {token:?}"
        );
        assert!(
            body.bytes().all(|byte| byte.is_ascii_digit()),
            "case {case:?}: {member}'s body is decimal, got {token:?}"
        );
        assert!(
            !token.contains(GROUP) && !token.contains(STREAM),
            "case {case:?}: {member} carries no name of the request, got {token:?}"
        );
    }
}

/// The `(message, eventId)` pairs of a page, in the order reported.
fn reported(case: &str, page: &Value) -> Vec<(String, String)> {
    page.get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a read reports events, got {page}"))
        .iter()
        .map(|event| {
            let member = |key: &str| {
                event
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every event carries {key}, got {event}")
                    })
                    .to_string()
            };
            (member("message"), member("eventId"))
        })
        .collect()
}

/// A group with one stream holding `count` events one second apart, named `event-<i>`, and the
/// base timestamp the first of them was stamped at.
async fn stream_of(logs: &Logs, count: usize) -> (i64, Vec<String>) {
    group_with_stream(logs, GROUP, STREAM, None).await;
    let base = base_ms();
    let events: Vec<(i64, String)> = (0..count)
        .map(|index| {
            (
                base + i64::try_from(index).expect("a small index") * 1_000,
                format!("event-{index}"),
            )
        })
        .collect();
    if count > 0 {
        let wire: Vec<(i64, &str)> = events
            .iter()
            .map(|(timestamp, message)| (*timestamp, message.as_str()))
            .collect();
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": batch(&wire),
            }),
        )
        .await;
    }
    (
        base,
        events.into_iter().map(|(_, message)| message).collect(),
    )
}

/// Every event carries an id: fixed-width, decimal, one per event, and the SAME id whichever
/// way the stream is read — over a grid of page sizes, starting ends, windows and `unmask`.
#[tokio::test]
async fn every_event_carries_one_stable_opaque_id() {
    let logs = Logs::new();
    let (base, expected) = stream_of(&logs, 4).await;

    // The id of each message, learned from one whole-stream read, is what every other read
    // must agree with.
    let whole = logs
        .ok(
            GET,
            &json!({"logGroupName": GROUP, "logStreamName": STREAM, "startFromHead": true}),
        )
        .await;
    let ids = reported("the whole stream", &whole);
    assert_eq!(
        ids.iter().map(|(message, _)| message.clone()).collect::<Vec<_>>(),
        expected,
        "case \"the whole stream\": every event is reported, got {whole}"
    );
    for (message, id) in &ids {
        assert_eq!(
            id.chars().count(),
            OPAQUE_WIDTH,
            "case {message:?}: an event id is {OPAQUE_WIDTH} characters, got {id:?}"
        );
        assert!(
            id.bytes().all(|byte| byte.is_ascii_digit()),
            "case {message:?}: an event id is decimal, got {id:?}"
        );
    }
    let distinct: std::collections::BTreeSet<&String> = ids.iter().map(|(_, id)| id).collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "case \"the whole stream\": no two events share an id, got {ids:?}"
    );

    // (case, the request members beside the group and the stream)
    let reads: Vec<(&str, Value)> = vec![
        ("the same read again", json!({"startFromHead": true})),
        ("one event per page", json!({"startFromHead": true, "limit": 1})),
        ("two events per page", json!({"startFromHead": true, "limit": 2})),
        ("from the tail", json!({})),
        ("from the tail, one per page", json!({"limit": 1})),
        (
            "a window starting at the second event",
            json!({"startFromHead": true, "startTime": base + 1_000}),
        ),
        (
            "a window ending before the last event",
            json!({"startFromHead": true, "endTime": base + 3_000}),
        ),
        ("unmasked", json!({"startFromHead": true, "unmask": true})),
    ];
    for (case, extra) in &reads {
        let mut request = json!({"logGroupName": GROUP, "logStreamName": STREAM});
        for (key, value) in extra.as_object().expect("an object of members") {
            request[key] = value.clone();
        }
        let page = logs.ok(GET, &request).await;
        assert_token_form(case, &page);
        for (message, id) in reported(case, &page) {
            let expected_id = ids
                .iter()
                .find(|(reported_message, _)| *reported_message == message)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| panic!("case {case:?}: {message:?} is an event of the stream"));
            assert_eq!(
                id, expected_id,
                "case {case:?}: {message:?} keeps one id across reads, expected \
                 {expected_id:?}, got {id:?}"
            );
        }
    }

    // The filtering read declares the same member in the model, so the two reads must agree
    // on the id of each event.
    let filtered = logs
        .ok(FILTER, &json!({"logGroupName": GROUP, "logStreamNames": [STREAM]}))
        .await;
    let filtered_ids: Vec<(String, String)> = filtered["events"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"the filtering read\": expected events, got {filtered}"))
        .iter()
        .map(|event| {
            let member = |key: &str| {
                event
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case \"the filtering read\": every event carries {key}, got {event}")
                    })
                    .to_string()
            };
            (member("message"), member("eventId"))
        })
        .collect();
    assert_eq!(
        filtered_ids, ids,
        "case \"the filtering read\": both reads report one id per event, got {filtered_ids:?}"
    );
}

/// Every token of every page is in the fixed-width envelope — over a grid of event counts,
/// page sizes and starting ends — and the pages a forward walk visits still cover the stream
/// exactly once, so the new body form did not cost the walk its position.
#[tokio::test]
async fn every_continuation_token_is_a_fixed_width_opaque_body() {
    for count in 0_usize..=3 {
        for limit in 1_usize..=2 {
            for from_head in [true, false] {
                let logs = Logs::new();
                let (_, expected) = stream_of(&logs, count).await;
                let case = format!("{count} events, limit {limit}, startFromHead {from_head}");

                let mut walked: Vec<String> = Vec::new();
                let mut token: Option<String> = None;
                for _ in 0..(count + 2) {
                    let mut request = json!({
                        "logGroupName": GROUP,
                        "logStreamName": STREAM,
                        "startFromHead": from_head,
                        "limit": limit,
                    });
                    if let Some(token) = &token {
                        request["nextToken"] = json!(token);
                    }
                    let page = logs.ok(GET, &request).await;
                    assert_token_form(&case, &page);
                    let page_messages: Vec<String> = reported(&case, &page)
                        .into_iter()
                        .map(|(message, _)| message)
                        .collect();
                    if page_messages.is_empty() {
                        break;
                    }
                    walked.extend(page_messages);
                    token = Some(
                        page["nextForwardToken"]
                            .as_str()
                            .expect("a forward token")
                            .to_string(),
                    );
                }
                // A walk from the head covers the stream; one from the tail starts at the
                // last page and covers the rest from there.
                let covered = if from_head {
                    expected.clone()
                } else {
                    expected[expected.len().saturating_sub(limit)..].to_vec()
                };
                assert_eq!(
                    walked, covered,
                    "case {case:?}: the forward walk covers the stream once, in order"
                );
            }
        }
    }
}

/// A token of ANOTHER stream's scan is not a position in this one: it is refused as the
/// parameter it is, rather than answered over a different set of events.
#[tokio::test]
async fn a_token_of_another_scan_is_refused() {
    let logs = Logs::new();
    stream_of(&logs, 2).await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": "another-stream"}),
    )
    .await;

    let page = logs
        .ok(
            GET,
            &json!({"logGroupName": GROUP, "logStreamName": STREAM, "limit": 1}),
        )
        .await;
    let token = page["nextForwardToken"]
        .as_str()
        .expect("a forward token")
        .to_string();

    let code = logs
        .err(
            GET,
            &json!({
                "logGroupName": GROUP,
                "logStreamName": "another-stream",
                "nextToken": token,
            }),
        )
        .await;
    assert_eq!(
        code, "InvalidParameterException",
        "case \"a token of another stream's scan\": expected InvalidParameterException, got {code}"
    );
}
