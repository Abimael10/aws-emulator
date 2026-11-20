// SPDX-License-Identifier: CC0-1.0

//! Cancelling a matchmaking request over the wire: `StopMatchmaking` stops work on the
//! ticket a caller names, so the stored ticket moves to the modelled `CANCELLED` status
//! and carries the instant it stopped being processed — whatever form its id takes,
//! whichever in-process status it was in, and whatever the request it carries.
//!
//! A ticket id no ticket carries is refused, a request the service has already finished
//! with cannot be stopped, and a cancelled ticket is out of the clock's progression: it is
//! neither picked up nor timed out afterwards.
//!
//! The not-found refusal has its own section at the end of this suite: its message is the
//! WHOLE fixed sentence that error class is worded with, so it names no part of the request.
//!
//! The harness (`GameLift`, `wait_for_due_transitions`) is the one `hosting_loop` already
//! drives the production dispatch path with; the matchmaker helpers are `matchmaking`'s.

use std::time::Duration;

use serde_json::{json, Value};

use crate::hosting_loop::{error_code, error_message, wait_for_due_transitions, GameLift};
use crate::matchmaking::matchmaker;

/// One matchmaking request to cancel.
struct Case {
    label: &'static str,
    /// The `TicketId` the request names, or `None` to let the service mint one — "if no
    /// ticket ID is specified here, Amazon GameLift Servers will generate one in the form
    /// of a UUID".
    ticket_id: Option<String>,
    players: Value,
    /// The in-process status the ticket is stopped in: `QUEUED` straight after the start,
    /// `SEARCHING` once the service has picked it up.
    stop_while: &'static str,
}

/// Start `case`'s request on `configuration` and return the ticket it reports.
async fn start(gamelift: &GameLift, configuration: &str, case: &Case) -> Value {
    let mut request = json!({"ConfigurationName": configuration, "Players": &case.players});
    if let Some(ticket_id) = &case.ticket_id {
        request["TicketId"] = json!(ticket_id);
    }
    let started = gamelift.ok("StartMatchmaking", request).await;
    let ticket = started["MatchmakingTicket"].clone();
    assert_eq!(
        ticket["Status"],
        json!("QUEUED"),
        "case {:?}: a new ticket must be QUEUED, got {ticket}",
        case.label
    );
    ticket
}

/// Cancel the ticket `started` names and assert the service reports it cancelled: the
/// stop answers an empty struct, and the ticket reads back `CANCELLED`, stamped with the
/// instant it stopped and otherwise unchanged. Answers the cancelled ticket.
async fn cancel(gamelift: &GameLift, label: &str, started: &Value) -> Value {
    let ticket_id = started["TicketId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: the ticket must carry an id, got {started}"))
        .to_string();

    // "If the operation is successful, the service sends back an empty JSON struct with the
    // HTTP 200 response (not an empty HTTP body)."
    let stopped = gamelift
        .ok("StopMatchmaking", json!({"TicketId": &ticket_id}))
        .await;
    assert_eq!(
        stopped,
        json!({}),
        "case {label:?}: a successful stop must answer an empty JSON struct, got {stopped}"
    );

    // "If successful, work on the ticket is stopped, and the ticket status is changed to
    // CANCELLED": the stored ticket is what every later read serves.
    let described = gamelift
        .ok("DescribeMatchmaking", json!({"TicketIds": [&ticket_id]}))
        .await;
    let ticket = described["TicketList"][0].clone();
    assert_eq!(
        ticket["Status"],
        json!("CANCELLED"),
        "case {label:?}: a stopped ticket must be CANCELLED, got {ticket}"
    );
    // "Time stamp indicating when the matchmaking request stopped being processed due to
    // successful completion, timeout, or cancellation."
    let end_time = ticket["EndTime"].as_f64().unwrap_or_else(|| {
        panic!("case {label:?}: a cancelled ticket must carry an EndTime, got {ticket}")
    });
    let start_time = ticket["StartTime"].as_f64().unwrap_or_else(|| {
        panic!("case {label:?}: a ticket must carry a StartTime, got {ticket}")
    });
    assert!(
        end_time >= start_time,
        "case {label:?}: the request stopped being processed no earlier than it started, \
         got EndTime {end_time} and StartTime {start_time}"
    );
    // The cancellation changed the status and the end time, and nothing the request
    // carried: the ticket still names the matchmaker it was started on and the players it
    // was started for.
    for member in [
        "TicketId",
        "ConfigurationName",
        "ConfigurationArn",
        "StartTime",
        "Players",
    ] {
        assert_eq!(
            ticket[member], started[member],
            "case {label:?}: {member} must be unchanged by the cancellation, got {ticket}"
        );
    }
    // `StatusReason` is the "code to explain the current status", which the service fills
    // for a request that did not complete on its own; a request the caller stopped carries
    // none, and no match was made for it, so it reports no connection information either.
    assert!(
        ticket["StatusReason"].is_null(),
        "case {label:?}: a ticket the caller stopped carries no StatusReason, got {ticket}"
    );
    assert!(
        ticket["GameSessionConnectionInfo"].is_null(),
        "case {label:?}: a cancelled ticket matched nothing, so it reports no game session \
         connection information, got {ticket}"
    );
    ticket
}

/// Look again until the ticket `ticket_id` names reads `status`, and answer it: the clock
/// owes a ticket's pick-up and its request timeout at an INSTANT, so the condition is what
/// this waits for rather than a span it times. Fails once a deadline far longer than any
/// transition this suite starts has passed.
async fn wait_for_ticket_status(gamelift: &GameLift, ticket_id: &str, status: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let described = gamelift
            .ok("DescribeMatchmaking", json!({"TicketIds": [ticket_id]}))
            .await;
        let ticket = described["TicketList"][0].clone();
        if ticket["Status"] == json!(status) {
            return ticket;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ticket {ticket_id:?} must reach {status} before the deadline, got {ticket}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Every matchmaking request that is still being processed is cancelled by
/// `StopMatchmaking`, and reads back `CANCELLED` through `DescribeMatchmaking`.
///
/// The rows differ along what the rule generalises over: the form of the ticket id (a
/// caller-chosen one, one carrying the dots the modelled `MatchmakingIdStringModel`
/// pattern `^[a-zA-Z0-9-\.]*$` allows, one of the modelled maximum length 128, and one the
/// service mints), the number of players the request carries and what each brings, and the
/// in-process status the ticket is stopped in.
#[tokio::test]
async fn a_request_being_processed_is_cancelled_in_every_form_it_takes() {
    let gamelift = GameLift::new();
    let configuration = matchmaker(&gamelift, "cancel-matchmaker", 43200).await;

    let cases = vec![
        Case {
            label: "one player, a caller-chosen ticket id, stopped while QUEUED",
            ticket_id: Some("cancel-one-player".to_string()),
            players: json!([{"PlayerId": "player-1"}]),
            stop_while: "QUEUED",
        },
        Case {
            label: "a ticket id carrying the dots the modelled pattern allows",
            ticket_id: Some("cancel.dotted.id-2".to_string()),
            players: json!([{"PlayerId": "player-2"}]),
            stop_while: "QUEUED",
        },
        Case {
            // `MatchmakingIdStringModel` is len 0..128, so this id is at the bound.
            label: "a ticket id of the modelled maximum length",
            ticket_id: Some("c".repeat(128)),
            players: json!([{"PlayerId": "player-3"}]),
            stop_while: "QUEUED",
        },
        Case {
            label: "three players with attributes and latencies, stopped while QUEUED",
            ticket_id: Some("cancel-party".to_string()),
            players: json!([
                {
                    "PlayerId": "player-4",
                    "PlayerAttributes": {"skill": {"N": 42.5}, "role": {"S": "medic"}},
                    "LatencyInMs": {"us-west-2": 30},
                },
                {"PlayerId": "player-5", "Team": "red"},
                {"PlayerId": "player-6", "Team": "blue"},
            ]),
            stop_while: "QUEUED",
        },
        Case {
            label: "no ticket id at all, which the service mints, stopped while SEARCHING",
            ticket_id: None,
            players: json!([{"PlayerId": "player-7"}]),
            stop_while: "SEARCHING",
        },
        Case {
            label: "a caller-chosen ticket id, stopped while SEARCHING",
            ticket_id: Some("cancel-searching".to_string()),
            players: json!([{"PlayerId": "player-8"}, {"PlayerId": "player-9"}]),
            stop_while: "SEARCHING",
        },
    ];

    // Every request is started first, so the one wait below carries every row that must be
    // stopped past the pick-up to its `SEARCHING` status.
    let mut started: Vec<(&Case, Value)> = Vec::new();
    for case in &cases {
        let ticket = start(&gamelift, &configuration, case).await;
        started.push((case, ticket));
    }

    for (case, ticket) in &started {
        if case.stop_while == "QUEUED" {
            cancel(&gamelift, case.label, ticket).await;
        }
    }

    wait_for_due_transitions();
    for (case, ticket) in &started {
        if case.stop_while != "SEARCHING" {
            continue;
        }
        let ticket_id = ticket["TicketId"].as_str().unwrap_or_default().to_string();
        let described = gamelift
            .ok("DescribeMatchmaking", json!({"TicketIds": [&ticket_id]}))
            .await;
        assert_eq!(
            described["TicketList"][0]["Status"],
            json!("SEARCHING"),
            "case {:?}: a picked-up ticket must be SEARCHING before it is stopped, got {described}",
            case.label
        );
        cancel(&gamelift, case.label, ticket).await;
    }

    // Cancelling those requests left every OTHER ticket where it was: the rows stopped
    // while QUEUED are still CANCELLED, and the pick-up the clock owed them never moved
    // them back.
    let ids: Vec<String> = started
        .iter()
        .map(|(_, ticket)| ticket["TicketId"].as_str().unwrap_or_default().to_string())
        .collect();
    let described = gamelift
        .ok("DescribeMatchmaking", json!({"TicketIds": &ids}))
        .await;
    let statuses: Vec<Value> = described["TicketList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|ticket| ticket["Status"].clone())
        .collect();
    assert_eq!(
        statuses,
        vec![json!("CANCELLED"); cases.len()],
        "every stopped ticket must stay CANCELLED, got {described}"
    );
}

/// A cancelled request is out of the progression the clock drives: past the instants its
/// pick-up and its request timeout were due on, it is still `CANCELLED` and still carries
/// the instant the caller stopped it.
#[tokio::test]
async fn a_cancelled_request_is_neither_picked_up_nor_timed_out() {
    let gamelift = GameLift::new();
    // The rows differ along the matchmaker's request timeout: one that cannot elapse
    // during the test, and the shortest the model allows
    // (`MatchmakingRequestTimeoutInteger`, range 1..43200), which elapses while the test
    // waits.
    let patient = matchmaker(&gamelift, "patient-cancel-matchmaker", 43200).await;
    let impatient = matchmaker(&gamelift, "impatient-cancel-matchmaker", 1).await;

    let mut cancelled: Vec<(&str, String, Value)> = Vec::new();
    for (label, configuration, ticket_id) in [
        (
            "a matchmaker whose request timeout cannot elapse",
            &patient,
            "cancel-patient",
        ),
        (
            "a matchmaker with a one-second request timeout",
            &impatient,
            "cancel-impatient",
        ),
    ] {
        let case = Case {
            label,
            ticket_id: Some(ticket_id.to_string()),
            players: json!([{"PlayerId": "player-1"}]),
            stop_while: "QUEUED",
        };
        let started = start(&gamelift, configuration, &case).await;
        let ticket = cancel(&gamelift, label, &started).await;
        cancelled.push((label, ticket_id.to_string(), ticket));
    }

    // Past the pick-up span AND past the one-second request timeout.
    wait_for_due_transitions();
    std::thread::sleep(Duration::from_millis(1_100));
    for (label, ticket_id, before) in &cancelled {
        let described = gamelift
            .ok("DescribeMatchmaking", json!({"TicketIds": [ticket_id]}))
            .await;
        let after = described["TicketList"][0].clone();
        assert_eq!(
            after, *before,
            "case {label:?}: a cancelled ticket must be unchanged by the clock, got {after}"
        );
    }
}

/// `StopMatchmaking` refuses a ticket id no ticket carries, and a request the service has
/// already finished with: there is no work left on it to stop.
///
/// The rows differ along why the request cannot be served: the ticket does not exist (in
/// each form an absent id takes), or it exists in a terminal status — one the caller
/// cancelled, and one that timed out on its own.
#[tokio::test]
async fn a_request_that_is_not_being_processed_is_refused() {
    let gamelift = GameLift::new();
    let patient = matchmaker(&gamelift, "refusal-matchmaker", 43200).await;
    let impatient = matchmaker(&gamelift, "refusal-impatient-matchmaker", 1).await;

    // A ticket id that names nothing is the modelled `NotFoundException` — "the requested
    // resource was not found[;] the resource was either not created yet or deleted" — whose
    // whole sentence is this refusal's fixed text, naming no part of the request.
    for (label, ticket_id) in [
        ("an id no ticket was ever started with", "cancel-absent"),
        ("the empty id the modelled pattern admits", ""),
        ("an id in the UUID form the service mints", "11111111-2222-3333-4444-555555555555"),
    ] {
        let (status, body) = gamelift
            .call("StopMatchmaking", &json!({"TicketId": ticket_id}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "NotFoundException"),
            "case {label:?}: an absent ticket must be refused with NotFoundException, got \
             {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            STOP_MATCHMAKING_TICKET_NOT_FOUND,
            "case {label:?}: the refusal must answer the one fixed not-found sentence, got \
             {body}"
        );
    }

    // A ticket in a terminal status: one the caller already cancelled, and one the clock
    // timed out.
    gamelift
        .ok(
            "StartMatchmaking",
            json!({
                "TicketId": "refusal-cancelled",
                "ConfigurationName": &patient,
                "Players": [{"PlayerId": "player-1"}],
            }),
        )
        .await;
    gamelift
        .ok("StopMatchmaking", json!({"TicketId": "refusal-cancelled"}))
        .await;
    gamelift
        .ok(
            "StartMatchmaking",
            json!({
                "TicketId": "refusal-timed-out",
                "ConfigurationName": &impatient,
                "Players": [{"PlayerId": "player-2"}],
            }),
        )
        .await;

    for (label, ticket_id, expected_status) in [
        (
            "a request the caller already cancelled",
            "refusal-cancelled",
            "CANCELLED",
        ),
        (
            "a request that timed out on its own",
            "refusal-timed-out",
            "TIMED_OUT",
        ),
    ] {
        // The timed-out row reaches its status once the request timeout the matchmaker was
        // created with is due, so the condition is waited for rather than a span timed.
        let before = wait_for_ticket_status(&gamelift, ticket_id, expected_status).await;

        let (status, body) = gamelift
            .call("StopMatchmaking", &json!({"TicketId": ticket_id}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidRequestException"),
            "case {label:?}: a request that is not being processed must be refused, got \
             {status} {body}"
        );
        assert!(
            error_message(&body).contains(expected_status),
            "case {label:?}: the refusal must state the status the ticket is in, got {body}"
        );

        // The refused stop changed nothing: the ticket keeps the status it ended in.
        let described = gamelift
            .ok("DescribeMatchmaking", json!({"TicketIds": [ticket_id]}))
            .await;
        assert_eq!(
            described["TicketList"][0], before,
            "case {label:?}: a refused stop must leave the ticket as it was, got {described}"
        );
    }
}

// --- The WORDING of the not-found refusal -----------------------------------------------
//
// The rule these two tests pin: a `TicketId` that resolves to no stored ticket is refused
// with HTTP 400 and the modelled `NotFoundException`, whose `Message` is the WHOLE fixed
// sentence this error class is worded with — it has no variable part at all, so neither the
// id the request carried nor anything read off the store appears in it, and every
// unresolvable id is answered with the very same string. The service words the same missing
// ticket differently per operation: `DescribeMatchmaking`, which may name several tickets
// at once, answers its own modelled `InvalidRequestException` naming the one id that
// resolved to nothing.

/// The one sentence `StopMatchmaking` answers for a ticket id that resolves to nothing. The
/// entire string is this error class's fixed text; it interpolates nothing.
const STOP_MATCHMAKING_TICKET_NOT_FOUND: &str = "Specified matchmaking ticket does not exist.";

/// What `DescribeMatchmaking` answers for the one id among its `TicketIds` that resolves to
/// nothing — the per-operation wording this family does NOT share.
fn describe_not_found_message(ticket_id: &str) -> String {
    format!("Matchmaking ticket not found: {ticket_id}")
}

/// The modelled maximum length of `StopMatchmaking$TicketId`
/// (`smithy_query operation=StopMatchmaking`: `MatchmakingIdStringModel`, length 0..128,
/// pattern `^[a-zA-Z0-9-\.]*$`).
const MAX_TICKET_ID_LENGTH: usize = 128;

/// The ticket the rows below store, so each refusal is checked against a store that is NOT
/// empty and can be shown to survive it.
const STORED_TICKET: &str = "stored-ticket-1";

/// A request timeout that cannot elapse while these tests run, so the stored ticket stays in
/// process (`MatchmakingRequestTimeoutInteger`, range 1..43200).
const PATIENT_TIMEOUT: i64 = 43200;

/// What the store holds for the row's id before the refused request is sent — why the id
/// resolves to nothing.
#[derive(Clone, Copy, PartialEq)]
enum Setup {
    /// No matchmaking request was ever started with the id, and the store holds the
    /// sentinel ticket.
    NeverStarted,
    /// Nothing at all is stored: no matchmaker, no ticket.
    EmptyStore,
    /// A matchmaking CONFIGURATION holds the name — a matchmaker is not a ticket.
    ConfigurationOfTheSameName,
}

/// One request whose ticket id resolves to nothing.
struct Absent {
    label: &'static str,
    ticket_id: String,
    setup: Setup,
}

fn absent_cases() -> Vec<Absent> {
    vec![
        Absent {
            label: "a hyphenated id no request was ever started with",
            ticket_id: "missing-ticket-probe".to_string(),
            setup: Setup::NeverStarted,
        },
        Absent {
            // `MatchmakingIdStringModel` is len 0..128, so this id is at the minimum.
            label: "the empty id the modelled length minimum admits",
            ticket_id: String::new(),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "a one-character id",
            ticket_id: "z".to_string(),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "an id carrying the dots the modelled pattern allows",
            ticket_id: "missing.ticket.probe".to_string(),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "an id in the UUID form the service mints for a request without one",
            ticket_id: "3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string(),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "an id at the modelled maximum length",
            ticket_id: "m".repeat(MAX_TICKET_ID_LENGTH),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "a stored ticket's id with one more character on the end",
            ticket_id: format!("{STORED_TICKET}0"),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "a stored ticket's id in another case",
            ticket_id: STORED_TICKET.to_uppercase(),
            setup: Setup::NeverStarted,
        },
        Absent {
            label: "an id no request was started with, against a store holding nothing",
            ticket_id: "missing-ticket-probe".to_string(),
            setup: Setup::EmptyStore,
        },
        Absent {
            label: "a name only a matchmaking configuration holds",
            ticket_id: "configuration-only".to_string(),
            setup: Setup::ConfigurationOfTheSameName,
        },
    ]
}

/// Start the sentinel matchmaking request on `configuration` and answer the ticket.
async fn start_stored_ticket(gamelift: &GameLift, configuration: &str) -> Value {
    let started = gamelift
        .ok(
            "StartMatchmaking",
            json!({
                "TicketId": STORED_TICKET,
                "ConfigurationName": configuration,
                "Players": [{"PlayerId": "player-1"}],
            }),
        )
        .await;
    started["MatchmakingTicket"].clone()
}

/// Every ticket id that resolves to no stored ticket is refused with the one fixed
/// not-found sentence, and the refusal leaves the store as it was.
///
/// The rows differ along what the rule generalises over: the form and length of the id
/// (empty, one character, hyphenated, dotted, the UUID form the service mints, the modelled
/// maximum length, a near-miss of a stored id), why it resolves to nothing (never started,
/// held by another resource kind), and whether the store holds any ticket at all.
#[tokio::test]
async fn an_unresolvable_ticket_id_is_refused_with_the_one_fixed_not_found_sentence() {
    let mut answered: Vec<(&str, String)> = Vec::new();

    for case in absent_cases() {
        let label = case.label;
        let gamelift = GameLift::new();
        let mut stored: Option<Value> = None;
        if case.setup != Setup::EmptyStore {
            let configuration = matchmaker(&gamelift, "wording-matchmaker", PATIENT_TIMEOUT).await;
            stored = Some(start_stored_ticket(&gamelift, &configuration).await);
        }
        if case.setup == Setup::ConfigurationOfTheSameName {
            matchmaker(&gamelift, &case.ticket_id, PATIENT_TIMEOUT).await;
        }

        let (status, body) = gamelift
            .call("StopMatchmaking", &json!({"TicketId": &case.ticket_id}))
            .await;
        let code = error_code(&body);
        let message = error_message(&body);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "NotFoundException", STOP_MATCHMAKING_TICKET_NOT_FOUND),
            "case {label:?}: StopMatchmaking on {:?} must answer 400 NotFoundException \
             {STOP_MATCHMAKING_TICKET_NOT_FOUND:?}, got {status} {code} {message:?}",
            case.ticket_id
        );
        // The sentence has no variable part: the id the request carried is not in it, and
        // neither is the id of the ticket the store does hold.
        if !case.ticket_id.is_empty() {
            assert!(
                !message.contains(&case.ticket_id),
                "case {label:?}: the refusal must not name the requested id, got {message:?}"
            );
        }
        assert!(
            !message.contains(STORED_TICKET),
            "case {label:?}: the refusal must name nothing read off the store, got {message:?}"
        );
        answered.push((label, message));

        // The same absent id is refused by `DescribeMatchmaking` too — with THAT operation's
        // own wording, which names the id that resolved to nothing.
        let expected_describe = describe_not_found_message(&case.ticket_id);
        let (status, body) = gamelift
            .call(
                "DescribeMatchmaking",
                &json!({"TicketIds": [&case.ticket_id]}),
            )
            .await;
        let code = error_code(&body);
        let message = error_message(&body);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "InvalidRequestException", expected_describe.as_str()),
            "case {label:?}: DescribeMatchmaking on {:?} must answer 400 \
             InvalidRequestException {expected_describe:?}, got {status} {code} {message:?}",
            case.ticket_id
        );

        // The refused requests started nothing and stopped nothing: the stored ticket is
        // still there and still in process.
        if let Some(before) = &stored {
            let described = gamelift
                .ok("DescribeMatchmaking", json!({"TicketIds": [STORED_TICKET]}))
                .await;
            let after = described["TicketList"][0].clone();
            for member in ["TicketId", "ConfigurationName", "StartTime", "Players"] {
                assert_eq!(
                    after[member], before[member],
                    "case {label:?}: {member} of the stored ticket must be untouched by a \
                     refused stop, got {after}"
                );
            }
            assert!(
                after["EndTime"].is_null(),
                "case {label:?}: the stored ticket must still be in process after a refused \
                 stop, got {after}"
            );
        }
    }

    // Every row was answered with the very same string, so the sentence is a constant and
    // not a template.
    let mut distinct: Vec<&String> = Vec::new();
    for (_, message) in &answered {
        if !distinct.contains(&message) {
            distinct.push(message);
        }
    }
    assert_eq!(
        distinct.len(),
        1,
        "every unresolvable ticket id must be answered with one and the same sentence, got \
         {answered:?}"
    );
}

/// The not-found sentence is the answer to a ticket id that RESOLVES to nothing, and to
/// nothing else: a stored ticket is stopped, the same id then resolves to a ticket in a
/// terminal status and is refused as such, and an id the modelled constraints reject never
/// reaches the lookup at all.
#[tokio::test]
async fn the_not_found_sentence_is_answered_only_for_an_id_that_resolves_to_nothing() {
    let gamelift = GameLift::new();
    let configuration = matchmaker(&gamelift, "served-matchmaker", PATIENT_TIMEOUT).await;
    start_stored_ticket(&gamelift, &configuration).await;

    // The stored ticket is served, not refused.
    let stopped = gamelift
        .ok("StopMatchmaking", json!({"TicketId": STORED_TICKET}))
        .await;
    assert_eq!(
        stopped,
        json!({}),
        "a stored ticket in process must be stopped, got {stopped}"
    );

    // The id still resolves — to a ticket the service has finished with — so the refusal is
    // about the ticket's status, NOT about a missing resource.
    let (status, body) = gamelift
        .call("StopMatchmaking", &json!({"TicketId": STORED_TICKET}))
        .await;
    let code = error_code(&body);
    let message = error_message(&body);
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidRequestException"),
        "a ticket that is no longer being processed must be refused as such, got {status} {body}"
    );
    assert_ne!(
        message, STOP_MATCHMAKING_TICKET_NOT_FOUND,
        "a ticket that resolves must not be answered with the not-found sentence, got {body}"
    );

    // An id the modelled `MatchmakingIdStringModel` rejects is a constraint violation: one
    // character past the length maximum, and a character the pattern does not admit.
    for (label, ticket_id) in [
        (
            "one character past the modelled maximum length",
            "m".repeat(MAX_TICKET_ID_LENGTH + 1),
        ),
        (
            "a character the modelled pattern does not admit",
            "not a ticket".to_string(),
        ),
    ] {
        let (status, body) = gamelift
            .call("StopMatchmaking", &json!({"TicketId": &ticket_id}))
            .await;
        let code = error_code(&body);
        let message = error_message(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {label:?}: a malformed ticket id must be refused as a constraint \
             violation, got {status} {body}"
        );
        assert_ne!(
            message, STOP_MATCHMAKING_TICKET_NOT_FOUND,
            "case {label:?}: a malformed id must not reach the ticket lookup, got {body}"
        );
    }
}
