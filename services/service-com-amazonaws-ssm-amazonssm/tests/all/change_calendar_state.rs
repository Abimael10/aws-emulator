// SPDX-License-Identifier: CC0-1.0

//! `GetCalendarState` end to end: the state of the Change Calendar documents a read names,
//! at the instant it asks about.
//!
//! The rule under test: the state of a Change Calendar document at an instant is read from
//! the iCalendar entry that document PERSISTED — the calendar is in its
//! `X-CALENDAR-TYPE` state (`DEFAULT_OPEN`/`DEFAULT_CLOSED`) outside its events and in the
//! other state throughout every occurrence of every event — and a read of several calendars
//! reports OPEN only where all of them are open, with `NextTransitionTime` the earliest
//! instant after the one asked about at which that combined state changes. A name no
//! document is held under is `InvalidDocument`, a document of any other type is
//! `InvalidDocumentType`, and content that is not a calendar entry this service reads is
//! `UnsupportedCalendarException`.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`error_code`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router. Nothing reaches
//! into the state struct: every calendar read here was first written by `CreateDocument`,
//! so a response that is not computed from persisted state cannot pass.
//!
//! The suites are tables whose rows differ along what the rule generalises over — the
//! calendar type, the form an event states its span in (`DTEND`, `DURATION`, an all-day
//! `DATE`, a `TZID` civil time), whether it recurs, the instant asked about, how many
//! calendars are read together, the name spelling, and the document type — rather than the
//! single calendar a recorded unit happened to read. Each case states an explicit `AtTime`,
//! so no case depends on how fast the host runs.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// The `X-CALENDAR-TYPE` of a calendar that is OPEN outside its events, so each event is a
/// closed period (`crate::state::change_calendar`).
const DEFAULT_OPEN: &str = "DEFAULT_OPEN";

/// The `X-CALENDAR-TYPE` of a calendar that is CLOSED outside its events, so each event is
/// an open period.
const DEFAULT_CLOSED: &str = "DEFAULT_CLOSED";

/// One iCalendar entry: the calendar-level properties a Change Calendar document carries,
/// around the events it holds.
fn calendar(calendar_type: &str, events: &[String]) -> String {
    let mut content = String::from(
        "BEGIN:VCALENDAR\r\nPRODID:-//emulator//change calendar//EN\r\nVERSION:2.0\r\n",
    );
    content.push_str(&format!("X-CALENDAR-TYPE:{calendar_type}\r\n"));
    for event in events {
        content.push_str(event);
    }
    content.push_str("END:VCALENDAR\r\n");
    content
}

/// One `VEVENT` of an entry: the lines a case states, around the identity every event
/// carries.
fn event(uid: &str, lines: &[&str]) -> String {
    let mut text = format!("BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{uid}\r\n");
    for line in lines {
        text.push_str(line);
        text.push_str("\r\n");
    }
    text.push_str("END:VEVENT\r\n");
    text
}

/// A calendar holding ONE event, which is the shape most rows need.
fn one_event_calendar(calendar_type: &str, lines: &[&str]) -> String {
    calendar(calendar_type, &[event("one", lines)])
}

/// An entry whose components nest `levels` deep: the shape that would drive a recursive
/// walk over caller-supplied content into the stack, so the service has to refuse it rather
/// than walk it.
fn nested_calendar(levels: usize) -> String {
    let mut inner = String::from("BEGIN:VEVENT\r\nUID:deep\r\nDTSTART:20240310T120000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n");
    for level in 0..levels {
        inner = format!("BEGIN:VTODO\r\nUID:level-{level}\r\n{inner}END:VTODO\r\n");
    }
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-CALENDAR-TYPE:{DEFAULT_OPEN}\r\n{inner}END:VCALENDAR\r\n"
    )
}

/// Create `name` as a Change Calendar document holding `content`, asserting it succeeded.
///
/// A Change Calendar entry is free text rather than a document tree, so it is persisted in
/// the `TEXT` format.
async fn create_calendar(router: &MultiServiceRouter, case: &str, name: &str, content: &str) {
    let (status, response) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "Content": content,
            "DocumentType": "ChangeCalendar",
            "DocumentFormat": "TEXT",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: CreateDocument {name} must succeed, got {status} {response}"
    );
}

/// `GetCalendarState` for `names` at `at_time`, returning `(status, body)`.
async fn get_state(
    router: &MultiServiceRouter,
    names: &[&str],
    at_time: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({ "CalendarNames": names });
    if let Some(at_time) = at_time {
        body["AtTime"] = json!(at_time);
    }
    call(router, "GetCalendarState", &body).await
}

/// `GetCalendarState` for `names` at `at_time`, asserting the read succeeded.
async fn state_ok(
    router: &MultiServiceRouter,
    case: &str,
    names: &[&str],
    at_time: Option<&str>,
) -> Value {
    let (status, response) = get_state(router, names, at_time).await;
    assert_eq!(
        status, 200,
        "case {case}: GetCalendarState {names:?} at {at_time:?} must succeed, got {status} \
         {response}"
    );
    response
}

/// The `State` a read reported.
fn state_of(case: &str, response: &Value) -> String {
    response["State"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a read reports State, got {response}"))
        .to_string()
}

/// The `NextTransitionTime` a read reported, or `None` when it reported none.
fn next_transition(response: &Value) -> Option<String> {
    response["NextTransitionTime"]
        .as_str()
        .map(str::to_string)
}

/// One instant a calendar is read at, and what the calendar's own content says the answer
/// is there.
struct Reading<'a> {
    case: &'a str,
    at_time: &'a str,
    state: &'a str,
    next: Option<&'a str>,
}

/// Assert every reading of the calendars `names` names.
async fn assert_readings(router: &MultiServiceRouter, names: &[&str], readings: &[Reading<'_>]) {
    for reading in readings {
        let response = state_ok(router, reading.case, names, Some(reading.at_time)).await;
        assert_eq!(
            state_of(reading.case, &response),
            reading.state,
            "case {}: {names:?} at {} is {}, got {response}",
            reading.case,
            reading.at_time,
            reading.state
        );
        assert_eq!(
            next_transition(&response).as_deref(),
            reading.next,
            "case {}: {names:?} at {} transitions next at {:?}, got {response}",
            reading.case,
            reading.at_time,
            reading.next
        );
        // "The time, as an ISO 8601 string, that you specified in your command."
        assert_eq!(
            response["AtTime"].as_str(),
            Some(reading.at_time),
            "case {}: a read reports the AtTime it was given, got {response}",
            reading.case
        );
    }
}

/// A default-open calendar is open outside its one event and closed throughout it, and a
/// default-closed calendar holding the same event is the exact inverse — the state at an
/// instant comes from the entry's own calendar type and event, not from the read.
#[tokio::test]
async fn calendar_type_decides_the_state_outside_an_event() {
    let router = router();
    let span = ["DTSTART:20240310T120000Z", "DTEND:20240310T140000Z"];
    create_calendar(
        &router,
        "default open",
        "OpenCalendar",
        &one_event_calendar(DEFAULT_OPEN, &span),
    )
    .await;
    create_calendar(
        &router,
        "default closed",
        "ClosedCalendar",
        &one_event_calendar(DEFAULT_CLOSED, &span),
    )
    .await;

    assert_readings(
        &router,
        &["OpenCalendar"],
        &[
            Reading {
                case: "open: before the event",
                at_time: "2024-03-10T11:59:59Z",
                state: "OPEN",
                next: Some("2024-03-10T12:00:00Z"),
            },
            Reading {
                case: "open: the first instant of the event",
                at_time: "2024-03-10T12:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T14:00:00Z"),
            },
            Reading {
                case: "open: inside the event",
                at_time: "2024-03-10T13:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T14:00:00Z"),
            },
            Reading {
                case: "open: the instant the event ends",
                at_time: "2024-03-10T14:00:00Z",
                state: "OPEN",
                next: None,
            },
            Reading {
                case: "open: long after the event",
                at_time: "2024-03-11T00:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;

    assert_readings(
        &router,
        &["ClosedCalendar"],
        &[
            Reading {
                case: "closed: before the event",
                at_time: "2024-03-10T11:59:59Z",
                state: "CLOSED",
                next: Some("2024-03-10T12:00:00Z"),
            },
            Reading {
                case: "closed: the first instant of the event",
                at_time: "2024-03-10T12:00:00Z",
                state: "OPEN",
                next: Some("2024-03-10T14:00:00Z"),
            },
            Reading {
                case: "closed: the instant the event ends",
                at_time: "2024-03-10T14:00:00Z",
                state: "CLOSED",
                next: None,
            },
        ],
    )
    .await;
}

/// Every form an event may state its span in covers the same instants: the `DTEND` it ends
/// at, the `DURATION` it lasts, the one day an all-day `DATE` event covers, and the civil
/// time a `TZID` states in an IANA zone.
#[tokio::test]
async fn an_event_spans_the_same_instants_in_every_form() {
    let router = router();

    create_calendar(
        &router,
        "DURATION",
        "DurationCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240310T120000Z", "DURATION:PT2H"],
        ),
    )
    .await;
    create_calendar(
        &router,
        "a week-long DURATION",
        "WeekCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240301T000000Z", "DURATION:P1W"],
        ),
    )
    .await;
    // RFC 5545: an event whose start is a DATE and which states no end covers that one day.
    create_calendar(
        &router,
        "an all-day DATE",
        "AllDayCalendar",
        &one_event_calendar(DEFAULT_OPEN, &["DTSTART;VALUE=DATE:20240310"]),
    )
    .await;
    // 09:00 in New York on that date is 14:00Z: the zone is UTC-5 before its March
    // transition.
    create_calendar(
        &router,
        "a TZID civil time",
        "ZonedCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &[
                "DTSTART;TZID=America/New_York:20240115T090000",
                "DTEND;TZID=America/New_York:20240115T100000",
            ],
        ),
    )
    .await;

    assert_readings(
        &router,
        &["DurationCalendar"],
        &[
            Reading {
                case: "DURATION: inside",
                at_time: "2024-03-10T13:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T14:00:00Z"),
            },
            Reading {
                case: "DURATION: after",
                at_time: "2024-03-10T14:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;
    assert_readings(
        &router,
        &["WeekCalendar"],
        &[
            Reading {
                case: "P1W: the last day of the week",
                at_time: "2024-03-07T23:59:59Z",
                state: "CLOSED",
                next: Some("2024-03-08T00:00:00Z"),
            },
            Reading {
                case: "P1W: one week on",
                at_time: "2024-03-08T00:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;
    assert_readings(
        &router,
        &["AllDayCalendar"],
        &[
            Reading {
                case: "DATE: the day before",
                at_time: "2024-03-09T23:59:59Z",
                state: "OPEN",
                next: Some("2024-03-10T00:00:00Z"),
            },
            Reading {
                case: "DATE: the middle of the day",
                at_time: "2024-03-10T12:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-11T00:00:00Z"),
            },
            Reading {
                case: "DATE: the day after",
                at_time: "2024-03-11T00:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;
    assert_readings(
        &router,
        &["ZonedCalendar"],
        &[
            Reading {
                case: "TZID: an hour before the civil time",
                at_time: "2024-01-15T13:00:00Z",
                state: "OPEN",
                next: Some("2024-01-15T14:00:00Z"),
            },
            Reading {
                case: "TZID: inside the civil hour",
                at_time: "2024-01-15T14:30:00Z",
                state: "CLOSED",
                next: Some("2024-01-15T15:00:00Z"),
            },
            Reading {
                case: "TZID: after it",
                at_time: "2024-01-15T15:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;
}

/// A recurring event closes the calendar at every occurrence its `RRULE` names, and the
/// occurrences its `EXDATE` excludes are not among them.
#[tokio::test]
async fn a_recurring_event_closes_every_occurrence() {
    let router = router();
    create_calendar(
        &router,
        "a daily recurrence",
        "DailyCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &[
                "DTSTART:20240101T000000Z",
                "DURATION:PT1H",
                "RRULE:FREQ=DAILY;COUNT=10",
            ],
        ),
    )
    .await;
    create_calendar(
        &router,
        "a daily recurrence with an exception",
        "ExceptedCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &[
                "DTSTART:20240101T000000Z",
                "DURATION:PT1H",
                "RRULE:FREQ=DAILY;COUNT=10",
                "EXDATE:20240105T000000Z",
            ],
        ),
    )
    .await;

    assert_readings(
        &router,
        &["DailyCalendar"],
        &[
            Reading {
                case: "RRULE: the first occurrence",
                at_time: "2024-01-01T00:30:00Z",
                state: "CLOSED",
                next: Some("2024-01-01T01:00:00Z"),
            },
            Reading {
                case: "RRULE: between two occurrences",
                at_time: "2024-01-05T01:30:00Z",
                state: "OPEN",
                next: Some("2024-01-06T00:00:00Z"),
            },
            Reading {
                case: "RRULE: a later occurrence",
                at_time: "2024-01-09T00:00:00Z",
                state: "CLOSED",
                next: Some("2024-01-09T01:00:00Z"),
            },
            Reading {
                case: "RRULE: after the count runs out",
                at_time: "2024-01-11T00:30:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;
    assert_readings(
        &router,
        &["ExceptedCalendar"],
        &[
            Reading {
                case: "EXDATE: the excluded occurrence",
                at_time: "2024-01-05T00:30:00Z",
                state: "OPEN",
                next: Some("2024-01-06T00:00:00Z"),
            },
            Reading {
                case: "EXDATE: the occurrence before it",
                at_time: "2024-01-04T00:30:00Z",
                state: "CLOSED",
                next: Some("2024-01-04T01:00:00Z"),
            },
        ],
    )
    .await;
}

/// Several calendars read together are OPEN only where ALL of them are open, so the next
/// transition is the earliest instant the INTERSECTION changes at — not the earliest
/// instant any one calendar changes at.
#[tokio::test]
async fn several_calendars_intersect() {
    let router = router();
    create_calendar(
        &router,
        "the earlier closed window",
        "EarlyCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240310T120000Z", "DTEND:20240310T140000Z"],
        ),
    )
    .await;
    create_calendar(
        &router,
        "the overlapping closed window",
        "LateCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240310T130000Z", "DTEND:20240310T150000Z"],
        ),
    )
    .await;
    // A default-closed calendar whose one event is its OPEN window, so reading it beside a
    // default-open one mixes both kinds.
    create_calendar(
        &router,
        "an open window",
        "WindowCalendar",
        &one_event_calendar(
            DEFAULT_CLOSED,
            &["DTSTART:20240310T100000Z", "DTEND:20240310T200000Z"],
        ),
    )
    .await;

    assert_readings(
        &router,
        &["EarlyCalendar", "LateCalendar"],
        &[
            Reading {
                case: "two calendars: both open",
                at_time: "2024-03-10T11:00:00Z",
                state: "OPEN",
                next: Some("2024-03-10T12:00:00Z"),
            },
            Reading {
                case: "two calendars: the first closed",
                at_time: "2024-03-10T12:30:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T15:00:00Z"),
            },
            Reading {
                case: "two calendars: both closed",
                at_time: "2024-03-10T13:30:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T15:00:00Z"),
            },
            Reading {
                case: "two calendars: only the second closed",
                at_time: "2024-03-10T14:30:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T15:00:00Z"),
            },
            Reading {
                case: "two calendars: both open again",
                at_time: "2024-03-10T15:00:00Z",
                state: "OPEN",
                next: None,
            },
        ],
    )
    .await;

    assert_readings(
        &router,
        &["EarlyCalendar", "WindowCalendar"],
        &[
            Reading {
                case: "mixed: outside the open window",
                at_time: "2024-03-10T09:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T10:00:00Z"),
            },
            Reading {
                case: "mixed: inside the open window, outside the closed one",
                at_time: "2024-03-10T11:00:00Z",
                state: "OPEN",
                next: Some("2024-03-10T12:00:00Z"),
            },
            Reading {
                case: "mixed: inside both",
                at_time: "2024-03-10T13:00:00Z",
                state: "CLOSED",
                next: Some("2024-03-10T14:00:00Z"),
            },
            Reading {
                case: "mixed: the open window alone again",
                at_time: "2024-03-10T14:00:00Z",
                state: "OPEN",
                next: Some("2024-03-10T20:00:00Z"),
            },
            Reading {
                case: "mixed: after the open window",
                at_time: "2024-03-10T20:00:00Z",
                state: "CLOSED",
                next: None,
            },
        ],
    )
    .await;

    // One calendar named twice is the intersection of a calendar with itself.
    assert_readings(
        &router,
        &["EarlyCalendar", "EarlyCalendar"],
        &[Reading {
            case: "one calendar named twice",
            at_time: "2024-03-10T12:30:00Z",
            state: "CLOSED",
            next: Some("2024-03-10T14:00:00Z"),
        }],
    )
    .await;
}

/// `NextTransitionTime` is the instant the reported state stops holding: reading the
/// calendars AT it reports the other state, and reading them an instant before it reports
/// the same state as the original read.
///
/// Walked over every hour of the day the events fall in, so the invariant is checked across
/// the whole span rather than at the instants a table happened to pick.
#[tokio::test]
async fn the_next_transition_is_when_the_state_changes() {
    let router = router();
    create_calendar(
        &router,
        "a sweep",
        "SweepCalendar",
        &calendar(
            DEFAULT_OPEN,
            &[
                event(
                    "morning",
                    &["DTSTART:20240310T080000Z", "DTEND:20240310T093000Z"],
                ),
                event(
                    "afternoon",
                    &["DTSTART:20240310T140000Z", "DURATION:PT90M"],
                ),
            ],
        ),
    )
    .await;

    for hour in 0..24 {
        let at_time = format!("2024-03-10T{hour:02}:17:00Z");
        let case = format!("sweep at {at_time}");
        let response = state_ok(&router, &case, &["SweepCalendar"], Some(&at_time)).await;
        let state = state_of(&case, &response);
        // The entry's own events: closed 08:00–09:30 and 14:00–15:30, so the reading at
        // 17 minutes past the hour is inside an event in those four hours.
        let expected = if matches!(hour, 8 | 9 | 14 | 15) {
            "CLOSED"
        } else {
            "OPEN"
        };
        assert_eq!(
            state, expected,
            "case {case}: the calendar is {expected} at that instant, got {response}"
        );

        let Some(transition) = next_transition(&response) else {
            continue;
        };
        assert!(
            transition.as_str() > at_time.as_str(),
            "case {case}: the next transition {transition} is after the instant asked about"
        );
        let at_transition =
            state_ok(&router, &case, &["SweepCalendar"], Some(&transition)).await;
        assert_ne!(
            state_of(&case, &at_transition),
            state,
            "case {case}: the state at the next transition {transition} differs from {state}, \
             got {at_transition}"
        );
    }
}

/// A calendar is reached by bare name or by document ARN, and reading it twice reports the
/// same answer — the read computes from the persisted entry and changes nothing a later
/// caller sees, including the document's own content.
#[tokio::test]
async fn a_read_resolves_either_name_spelling_and_changes_nothing() {
    let router = router();
    let content = one_event_calendar(
        DEFAULT_OPEN,
        &["DTSTART:20240310T120000Z", "DTEND:20240310T140000Z"],
    );
    create_calendar(&router, "either spelling", "SpellingCalendar", &content).await;
    let arn = format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/SpellingCalendar");

    for (case, name) in [
        ("a bare name", "SpellingCalendar".to_string()),
        ("a document ARN", arn),
    ] {
        let first = state_ok(&router, case, &[&name], Some("2024-03-10T13:00:00Z")).await;
        let again = state_ok(&router, case, &[&name], Some("2024-03-10T13:00:00Z")).await;
        assert_eq!(
            first, again,
            "case {case}: two reads of one calendar at one instant report the same answer"
        );
        assert_eq!(
            state_of(case, &first),
            "CLOSED",
            "case {case}: the calendar is closed inside its event, got {first}"
        );
    }

    // The document the calendar lives in is untouched by the reads above.
    let (status, document) = call(
        &router,
        "GetDocument",
        &json!({ "Name": "SpellingCalendar" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetDocument must succeed after the reads, got {status} {document}"
    );
    assert_eq!(
        document["Content"].as_str(),
        Some(content.as_str()),
        "the calendar document still holds the content it was created with, got {document}"
    );
}

/// A read that states no `AtTime` uses the current time: a calendar whose one event covers
/// this century is closed, and the one whose event already ended is open.
#[tokio::test]
async fn a_read_without_at_time_uses_the_current_time() {
    let router = router();
    create_calendar(
        &router,
        "a century-long event",
        "CenturyCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20000101T000000Z", "DTEND:21000101T000000Z"],
        ),
    )
    .await;
    create_calendar(
        &router,
        "an event long past",
        "PastCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20000101T000000Z", "DTEND:20000102T000000Z"],
        ),
    )
    .await;

    for (case, name, expected) in [
        ("inside a century-long event", "CenturyCalendar", "CLOSED"),
        ("after an event long past", "PastCalendar", "OPEN"),
    ] {
        let response = state_ok(&router, case, &[name], None).await;
        assert_eq!(
            state_of(case, &response),
            expected,
            "case {case}: the calendar is {expected} now, got {response}"
        );
        // "If you don't specify a time, GetCalendarState uses the current time", which the
        // answer reports as the ISO 8601 instant it was computed at.
        let at_time = response["AtTime"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a read reports AtTime, got {response}"));
        assert!(
            at_time.len() == 20 && at_time.ends_with('Z'),
            "case {case}: AtTime is an ISO 8601 instant, got {at_time}"
        );
    }
}

/// A name no document is held under is `InvalidDocument`, whatever its spelling.
#[tokio::test]
async fn a_name_no_document_holds_is_invalid_document() {
    let router = router();
    create_calendar(
        &router,
        "one held calendar",
        "HeldCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240310T120000Z", "DTEND:20240310T140000Z"],
        ),
    )
    .await;

    let missing_arn = format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/MissingCalendar");
    for (case, names, missing) in [
        ("a bare name nothing holds", vec!["MissingCalendar"], "MissingCalendar"),
        (
            "an ARN nothing holds",
            vec![missing_arn.as_str()],
            missing_arn.as_str(),
        ),
        (
            "one held calendar beside a missing one",
            vec!["HeldCalendar", "MissingCalendar"],
            "MissingCalendar",
        ),
        (
            "a differently cased spelling of a held calendar",
            vec!["heldcalendar"],
            "heldcalendar",
        ),
    ] {
        let (status, response) = get_state(&router, &names, Some("2024-03-10T13:00:00Z")).await;
        assert_eq!(
            status, 400,
            "case {case}: {names:?} names no calendar, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidDocument",
            "case {case}: the refusal is InvalidDocument, got {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("The specified Change Calendar Document '{missing}' does not exist."),
            "case {case}: the refusal names the calendar document it could not find, got \
             {response}"
        );
    }
}

/// A document of any type other than `ChangeCalendar` holds no calendar entry, so a read of
/// it is `InvalidDocumentType` — for a document a caller created here and for one Systems
/// Manager publishes alike.
#[tokio::test]
async fn a_document_of_another_type_is_invalid_document_type() {
    let router = router();
    let created: &[(&str, &str, &str, &str, &str)] = &[
        (
            "a Command document",
            "CommandDocument",
            "Command",
            "JSON",
            r#"{"schemaVersion":"2.2","mainSteps":[{"action":"aws:runShellScript","name":"run","inputs":{"runCommand":["echo hi"]}}]}"#,
        ),
        (
            "an Automation document",
            "AutomationDocument",
            "Automation",
            "YAML",
            "schemaVersion: '0.3'\nmainSteps:\n  - name: wait\n    action: 'aws:sleep'\n    inputs:\n      Duration: PT1S\n",
        ),
        (
            "a Session document",
            "SessionDocument",
            "Session",
            "JSON",
            r#"{"schemaVersion":"1.0","sessionType":"Standard_Stream","inputs":{"idleSessionTimeout":"20"}}"#,
        ),
    ];
    for (case, name, document_type, format, content) in created {
        let (status, response) = call(
            &router,
            "CreateDocument",
            &json!({
                "Name": name,
                "Content": content,
                "DocumentType": document_type,
                "DocumentFormat": format,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDocument must succeed, got {status} {response}"
        );
    }

    // A document Systems Manager publishes resolves without any create, and it is a
    // Command document rather than a calendar.
    for (case, name) in [
        ("a created Command document", "CommandDocument"),
        ("a created Automation document", "AutomationDocument"),
        ("a created Session document", "SessionDocument"),
        ("a published Command document", "AWS-RunShellScript"),
    ] {
        let (status, response) = get_state(&router, &[name], Some("2024-03-10T13:00:00Z")).await;
        assert_eq!(
            status, 400,
            "case {case}: {name} is no calendar, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidDocumentType",
            "case {case}: the refusal is InvalidDocumentType, got {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("A document exists with the name '{name}', but it is not of type 'ChangeCalendar'."),
            "case {case}: the refusal says the document exists and is of another type, got \
             {response}"
        );
    }
}

/// Content that is not a Change Calendar entry this service reads is
/// `UnsupportedCalendarException`, whichever part of the entry is unreadable.
#[tokio::test]
async fn content_that_is_no_calendar_entry_is_unsupported() {
    let router = router();
    let rows: &[(&str, &str, String)] = &[
        (
            "free text that is no iCalendar entry at all",
            "NotACalendar",
            "this is not a calendar".to_string(),
        ),
        (
            "a calendar type the service doesn't know",
            "StrangeTypeCalendar",
            one_event_calendar("DEFAULT_MAYBE", &["DTSTART:20240310T120000Z"]),
        ),
        (
            "an event that states no start",
            "NoStartCalendar",
            calendar(DEFAULT_OPEN, &[event("one", &["DTEND:20240310T140000Z"])]),
        ),
        (
            "an event whose start is no date-time",
            "BadStartCalendar",
            one_event_calendar(DEFAULT_OPEN, &["DTSTART:the tenth of March"]),
        ),
        (
            "an event whose duration states months",
            "MonthlyDurationCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &["DTSTART:20240310T120000Z", "DURATION:P1M"],
            ),
        ),
        (
            "an event whose recurrence can't be expanded",
            "BadRuleCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &[
                    "DTSTART:20240310T120000Z",
                    "DURATION:PT1H",
                    "RRULE:FREQ=NEVER",
                ],
            ),
        ),
        (
            "an event that states a time zone that is no IANA zone",
            "BadZoneCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &["DTSTART;TZID=Mars/Olympus:20240310T120000", "DURATION:PT1H"],
            ),
        ),
        (
            "an event that ends before it starts",
            "BackwardsCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &["DTSTART:20240310T140000Z", "DTEND:20240310T120000Z"],
            ),
        ),
        (
            "an event whose start falls outside the years this service reads",
            "AncientCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &["DTSTART:00001231T000000Z", "DURATION:PT1H"],
            ),
        ),
        (
            "components nested deeper than the service walks",
            "DeeplyNestedCalendar",
            nested_calendar(64),
        ),
        (
            "more events than the service reads",
            "CrowdedCalendar",
            calendar(
                DEFAULT_OPEN,
                &(0..600)
                    .map(|index| {
                        event(
                            &format!("event-{index}"),
                            &["DTSTART:20240310T120000Z", "DURATION:PT1M"],
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "a recurrence naming more occurrences than one read walks",
            "EndlessCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &[
                    "DTSTART:20240101T000000Z",
                    "DURATION:PT1S",
                    "RRULE:FREQ=SECONDLY",
                ],
            ),
        ),
        (
            "a recurrence whose occurrences before that time outnumber what one read walks",
            "LongPastCalendar",
            one_event_calendar(
                DEFAULT_OPEN,
                &[
                    "DTSTART:19700101T000000Z",
                    "DURATION:PT1S",
                    "RRULE:FREQ=MINUTELY",
                ],
            ),
        ),
    ];

    for (case, name, content) in rows {
        create_calendar(&router, case, name, content).await;
        let (status, response) = get_state(&router, &[name], Some("2024-03-10T13:00:00Z")).await;
        assert_eq!(
            status, 400,
            "case {case}: {name} holds no readable calendar entry, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "UnsupportedCalendarException",
            "case {case}: the refusal is UnsupportedCalendarException, got {response}"
        );
        assert!(
            error_message(&response).contains(*name),
            "case {case}: the refusal names the document, got {response}"
        );
    }
}

/// An `AtTime` that states no instant, and a request that names no calendar at all, are
/// refused as bad requests rather than answered.
#[tokio::test]
async fn a_bad_request_is_refused() {
    let router = router();
    create_calendar(
        &router,
        "a calendar to read",
        "RefusalCalendar",
        &one_event_calendar(
            DEFAULT_OPEN,
            &["DTSTART:20240310T120000Z", "DTEND:20240310T140000Z"],
        ),
    )
    .await;

    for (case, at_time) in [
        ("prose", "yesterday"),
        ("an empty instant", " "),
        ("a date-time with no digits where they belong", "2024-13-45T99:99:99Z"),
        ("a bare number", "1710075600"),
        ("a year outside the ones this service reads", "0000-01-01T00:00:00Z"),
        // ISO 8601 as the API documents it is the Extended form carrying an offset, which
        // is what every AWS SDK sends; the iCalendar basic form and a bare date are not it.
        ("an iCalendar basic date-time", "20240310T130000Z"),
        ("a bare date", "2024-03-10"),
        ("a date-time carrying no offset", "2024-03-10T13:00:00"),
    ] {
        let (status, response) =
            get_state(&router, &["RefusalCalendar"], Some(at_time)).await;
        assert_eq!(
            status, 400,
            "case {case}: AtTime '{at_time}' states no instant, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case}: the refusal is a ValidationException, got {response}"
        );
    }

    let (status, response) = get_state(&router, &[], Some("2024-03-10T13:00:00Z")).await;
    assert_eq!(
        status, 400,
        "a read that names no calendar is refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "the refusal of a read that names no calendar is a ValidationException, got {response}"
    );

    // The emulator's own bound on how many calendars one read may name
    // (`crate::state::change_calendar::MAX_CALENDARS_PER_READ`): the request states the
    // list, so the work one call asks for grows with it.
    let limit = service_com_amazonaws_ssm_amazonssm::state::change_calendar::MAX_CALENDARS_PER_READ;
    let within: Vec<&str> = vec!["RefusalCalendar"; limit];
    let response = state_ok(
        &router,
        "the most calendars one read may name",
        &within,
        Some("2024-03-10T13:00:00Z"),
    )
    .await;
    assert_eq!(
        state_of("the most calendars one read may name", &response),
        "CLOSED",
        "a read naming the bound's worth of calendars is served, got {response}"
    );

    let past: Vec<&str> = vec!["RefusalCalendar"; limit + 1];
    let (status, response) = get_state(&router, &past, Some("2024-03-10T13:00:00Z")).await;
    assert_eq!(
        status, 400,
        "a read naming one calendar past the bound is refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "the refusal past the bound is a ValidationException, got {response}"
    );
}
