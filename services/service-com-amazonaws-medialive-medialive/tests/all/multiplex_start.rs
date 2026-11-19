// SPDX-License-Identifier: CC0-1.0

//! `StartMultiplex`: running a stored multiplex, end to end over the restJson1
//! wire.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables: the recorded case (a start of a multiplex holding no
//! program, which the real service accepts) is ONE row of them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_lifecycle::wait_for_the_clock;
use crate::multiplex_deletion::delete_multiplex;
use crate::multiplex_lifecycle::{create_body, created, describe_multiplex, settings, MULTIPLEXES};
use crate::multiplex_programs::{create_program, program_settings};
use crate::multiplex_stop::stop_multiplex;
use crate::wire::{call, error_code, error_message, medialive};

/// How many availability zones every multiplex here is created in — the two
/// `CreateMultiplexRequest$AvailabilityZones` documents ("You must specify
/// exactly two"), which is also the number of pipelines a RUNNING multiplex
/// reports as healthy, one per zone. Shared with the multiplex STOP suite, which
/// asserts the same count on the multiplex it stops.
pub(crate) const ZONES: i64 = 2;

/// `StartMultiplex`'s binding (`smithy_query(query="operation",
/// operation="StartMultiplex")`: `POST /prod/multiplexes/{MultiplexId}/start`,
/// success 202). Shared with the multiplex STOP suite, which starts a multiplex
/// to have one running to stop, and with the multiplex DELETION suite, which
/// drives a multiplex into the states a delete refuses.
pub(crate) async fn start_multiplex(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "POST", &format!("{MULTIPLEXES}/{id}/start"), None).await
}

/// A multiplex created under `name` with `multiplex_settings`, as its minted id.
pub(crate) async fn multiplex_with_settings(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    multiplex_settings: Value,
) -> String {
    let multiplex = created(
        router,
        case,
        create_body(name, &format!("token-{name}"), multiplex_settings),
    )
    .await;
    multiplex["id"].as_str().unwrap_or_default().to_string()
}

/// Create `count` programs in `multiplex_id`, each with its own name and its own
/// `ProgramNumber`, asserting every create succeeded.
pub(crate) async fn add_programs(
    router: &MultiServiceRouter,
    case: &str,
    multiplex_id: &str,
    count: i64,
) {
    for index in 0..count {
        let name = format!("program-{index}");
        let (status, program) = create_program(
            router,
            multiplex_id,
            &name,
            &format!("token-{multiplex_id}-{index}"),
            program_settings(index + 1),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateMultiplexProgram {name}: expected 201, got {status}: {program}"
        );
    }
}

/// Rule: `StartMultiplex` runs a stored multiplex that is stopped — it answers
/// 202 with that multiplex now `STARTING`, with no pipeline healthy yet, and the
/// clock settles it to `RUNNING`, where one pipeline per availability zone is
/// healthy; nothing else about the stored multiplex moves, and the reader
/// reports the same transition.
///
/// How many programs the multiplex holds is NOT a precondition: the recorded
/// `StartMultiplex` call runs a multiplex holding none, so the rows differ along
/// the program count, from none upwards, and along the `MultiplexSettings`
/// values — a start reads the multiplex's state, not the shape of what it
/// carries.
#[tokio::test]
async fn a_stopped_multiplex_starts_whatever_it_holds() {
    // (case, multiplex name, program count, multiplex settings)
    let rows: [(&str, &str, i64, Value); 5] = [
        ("no programs at all", "empty", 0, settings(2_000_000, 1)),
        ("one program", "single", 1, settings(1_000_000, 0)),
        ("two programs", "pair", 2, settings(20_000_000, 42)),
        ("three programs", "trio", 3, settings(100_000_000, 65_535)),
        (
            "every optional setting",
            "buffered",
            1,
            json!({
                "transportStreamBitrate": 5_000_000,
                "transportStreamId": 7,
                "maximumVideoBufferDelayMilliseconds": 1_000,
                "transportStreamReservedBitrate": 100_000,
            }),
        ),
    ];

    for (case, name, programs, multiplex_settings) in rows {
        let router = medialive();
        let id = multiplex_with_settings(&router, case, name, multiplex_settings).await;
        add_programs(&router, case, &id, programs).await;
        // A multiplex is started from the stopped state, so the create's own
        // transition is let come due first.
        wait_for_the_clock();
        let (status, idle) = describe_multiplex(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeMultiplex: expected 200, got {status}: {idle}"
        );
        assert_eq!(
            idle["state"],
            json!("IDLE"),
            "case {case:?}: the test did not reach IDLE, it reached {:?}",
            idle["state"]
        );
        assert_eq!(
            idle["programCount"],
            json!(programs),
            "case {case:?}: expected the programs the multiplex holds, got {:?}",
            idle["programCount"]
        );

        let (status, starting) = start_multiplex(&router, &id).await;
        assert_eq!(
            status, 202,
            "case {case:?}: StartMultiplex: expected 202, got {status}: {starting}"
        );
        assert_eq!(
            starting["state"],
            json!("STARTING"),
            "case {case:?}: expected StartMultiplex to answer STARTING, got {:?}",
            starting["state"]
        );
        assert_eq!(
            starting["pipelinesRunningCount"],
            json!(0),
            "case {case:?}: expected no pipeline healthy while STARTING, got {:?}",
            starting["pipelinesRunningCount"]
        );
        // The start moved the state and nothing else: every other member is the
        // stored multiplex's, read back from the store rather than from this
        // request.
        let mut expected = idle.clone();
        expected["state"] = json!("STARTING");
        assert_eq!(
            starting, expected,
            "case {case:?}: expected only the state to have moved, got {starting}"
        );

        // The transition the clock owes: `STARTING` settles to `RUNNING`, where
        // one pipeline per availability zone is healthy.
        wait_for_the_clock();
        let (status, running) = describe_multiplex(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeMultiplex after the clock: expected 200, got {status}: {running}"
        );
        assert_eq!(
            running["state"],
            json!("RUNNING"),
            "case {case:?}: expected a started multiplex to settle to RUNNING, got {:?}",
            running["state"]
        );
        assert_eq!(
            running["pipelinesRunningCount"],
            json!(ZONES),
            "case {case:?}: expected one healthy pipeline per availability zone, got {:?}",
            running["pipelinesRunningCount"]
        );
        let mut expected = idle.clone();
        expected["state"] = json!("RUNNING");
        expected["pipelinesRunningCount"] = json!(ZONES);
        assert_eq!(
            running, expected,
            "case {case:?}: expected only the state and the pipeline count to have moved, got {running}"
        );
    }
}

/// Rule: a start reads the multiplex its `MultiplexId` names — a `MultiplexId`
/// no multiplex of the account has is a 404 `NotFoundException` and starts
/// nothing, and starting one multiplex leaves every other multiplex of the
/// account where it was.
#[tokio::test]
async fn a_start_touches_only_the_multiplex_it_names() {
    let router = medialive();
    let started = multiplex_with_settings(&router, "neighbours", "started", settings(2_000_000, 1)).await;
    let untouched =
        multiplex_with_settings(&router, "neighbours", "untouched", settings(3_000_000, 2)).await;
    add_programs(&router, "neighbours", &untouched, 1).await;
    wait_for_the_clock();

    for (case, id) in [
        ("never minted", "9999999"),
        ("not a number at all", "no-such-multiplex"),
    ] {
        let (status, body) = start_multiplex(&router, id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Multiplex {id} not found"),
            "case {case:?}: expected the refusal to name the id asked for, got {body}"
        );
    }

    let (status, body) = start_multiplex(&router, &started).await;
    assert_eq!(
        status, 202,
        "StartMultiplex: expected 202, got {status}: {body}"
    );
    let (status, neighbour) = describe_multiplex(&router, &untouched).await;
    assert_eq!(
        status, 200,
        "DescribeMultiplex: expected 200, got {status}: {neighbour}"
    );
    assert_eq!(
        neighbour["state"],
        json!("IDLE"),
        "expected the multiplex the start did not name to stay stopped, got {:?}",
        neighbour["state"]
    );
    assert_eq!(
        neighbour["pipelinesRunningCount"],
        json!(0),
        "expected no pipeline healthy on the multiplex the start did not name, got {:?}",
        neighbour["pipelinesRunningCount"]
    );
}

/// Rule: a multiplex runs from the stopped state only, so a start sent while it
/// is in any other state is refused with a 409 `ConflictException` carrying this
/// service's wording for that class — which names the multiplex and the state it
/// is in — and moves nothing: a start is never a no-op.
///
/// The rows span every state this slice can put a multiplex in besides the
/// stopped one. The states an accepted delete puts it in are refused the same
/// way, on the recorded evidence `crate::multiplex_start_after_delete` pins.
#[tokio::test]
async fn a_multiplex_starts_only_from_the_stopped_state() {
    // (case, the state the multiplex is in when the start is sent)
    let conflicts: [(&str, &str); 3] = [
        ("while still creating", "CREATING"),
        ("while already starting", "STARTING"),
        ("while already running", "RUNNING"),
    ];

    for (case, state) in conflicts {
        let router = medialive();
        let id = multiplex_with_settings(&router, case, "stateful", settings(2_000_000, 1)).await;
        // A program can be created in a multiplex that is still coming up, so
        // every row carries one: the state is what the row varies.
        add_programs(&router, case, &id, 1).await;
        if state != "CREATING" {
            wait_for_the_clock();
            let (status, started) = start_multiplex(&router, &id).await;
            assert_eq!(
                status, 202,
                "case {case:?}: StartMultiplex: expected 202, got {status}: {started}"
            );
        }
        if state == "RUNNING" {
            wait_for_the_clock();
        }

        let (status, observed) = describe_multiplex(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeMultiplex: expected 200, got {status}: {observed}"
        );
        assert_eq!(
            observed["state"],
            json!(state),
            "case {case:?}: the test did not reach {state}, it reached {:?}",
            observed["state"]
        );

        let (status, body) = start_multiplex(&router, &id).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected starting a {state} multiplex to conflict, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            format!("Multiplex {id} cannot be started while in {state} state"),
            "case {case:?}: expected the refusal to name the multiplex and the state it is in, got {body}"
        );

        // The refused start moved nothing.
        let (status, after) = describe_multiplex(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeMultiplex after the refusal: expected 200, got {status}: {after}"
        );
        assert_eq!(
            after["state"], observed["state"],
            "case {case:?}: expected the refused start to leave the state alone, got {after}"
        );
    }
}

// ---------------------------------------------------------------------------
// The `Message` a start refused on the multiplex's STATE carries.
//
// RULE: a start sent while the multiplex is in any `MultiplexState`
// (`smithy_query(query="shape", shape="MultiplexState")`) other than the stopped
// one is refused with the modelled 409 `ConflictException`
// (`smithy_query(query="shape", shape="ConflictException")`) carrying this
// refusal class's sentence — "Multiplex <MultiplexId> cannot be started while in
// <MultiplexState> state". That text is FIXED; its two variable parts are the
// multiplex the request named, taken from the `MultiplexId` URI label, and the
// state read out of the record the start was judged against. Nothing else of
// that multiplex is in it — not its name, not its ARN — and the sentence depends
// neither on which state refused the start nor on which entry point asked, so
// the single-resource `StartMultiplex` and the `multiplexIds` arm of
// `BatchStart` word it identically.
//
// The tables below are over that whole CLASS: the recorded case (a start sent
// while the record read `DELETING`, whose classification
// `crate::multiplex_start_after_delete` pins) is ONE row of them.
// ---------------------------------------------------------------------------

/// `BatchStart`'s binding (`smithy_query(query="operation",
/// operation="BatchStart")`: POST /prod/batch/start, success 200).
const BATCH_START: &str = "/prod/batch/start";

/// The error a refusal on the resource's STATE carries, which on this service is
/// the modelled exception's shape name, both in an error body and in the `Code`
/// of a `BatchStart` failed entry.
const CONFLICT: &str = "ConflictException";

/// The one `MultiplexState` a start acts from.
const IDLE: &str = "IDLE";

/// The state an accepted start answers with.
const STARTING: &str = "STARTING";

/// The states a start is refused in: every value `MultiplexState`
/// (`smithy_query(query="shape", shape="MultiplexState")`) models that this
/// service can put a multiplex in, besides the one state a start accepts.
/// `CREATE_FAILED` and `RECOVERING` are the enum's other two values and no
/// operation of this emulator reaches either, so no start can be sent to a
/// multiplex in them.
const REFUSING_STATES: [&str; 6] = [
    "CREATING",
    STARTING,
    "RUNNING",
    "STOPPING",
    "DELETING",
    "DELETED",
];

/// Generous room for a state the clock owes to come due. The waits below are
/// POLLS, so this bound only has to be long enough that a loaded host still
/// reaches the condition.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How often [`in_state`] looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The sentence MediaLive refuses a multiplex start with while the multiplex is
/// in `state`, written out here rather than read from the crate so the test pins
/// the text instead of repeating the implementation.
fn not_startable(id: &str, state: &str) -> String {
    format!("Multiplex {id} cannot be started while in {state} state")
}

/// One multiplex shape a row builds: what it is called, the `MultiplexSettings`
/// it carries, whether it carries tags and how many programs it holds — the
/// things this refusal's wording must not depend on.
struct Shape {
    name: &'static str,
    multiplex_settings: Value,
    tags: Option<Value>,
    programs: i64,
}

/// The shapes the rows cycle through, so each state's refusal is asserted on a
/// multiplex that differs in everything but its state.
fn shapes() -> [Shape; 3] {
    [
        Shape {
            name: "plain",
            multiplex_settings: settings(10_000_000, 1),
            tags: None,
            programs: 0,
        },
        Shape {
            name: "多重化",
            multiplex_settings: json!({
                "transportStreamBitrate": 5_000_000,
                "transportStreamId": 42,
                "maximumVideoBufferDelayMilliseconds": 1_000,
                "transportStreamReservedBitrate": 100_000,
            }),
            tags: Some(json!({ "env": "test", "team": "playout" })),
            programs: 1,
        },
        Shape {
            name: "crowded",
            multiplex_settings: settings(100_000_000, 65_535),
            tags: None,
            programs: 3,
        },
    ]
}

/// A multiplex built to `shape`, as `(id, arn)`.
async fn multiplex_of(router: &MultiServiceRouter, case: &str, shape: &Shape) -> (String, String) {
    let mut body = create_body(
        shape.name,
        &format!("token-{}", shape.name),
        shape.multiplex_settings.clone(),
    );
    if let Some(tags) = shape.tags.clone() {
        body["tags"] = tags;
    }
    let multiplex = created(router, case, body).await;
    let id = multiplex["id"].as_str().unwrap_or_default().to_string();
    let arn = multiplex["arn"].as_str().unwrap_or_default().to_string();
    add_programs(router, case, &id, shape.programs).await;
    (id, arn)
}

/// The multiplex `id`, read back through `DescribeMultiplex` — a record a delete
/// has been accepted for stays resolvable, so this is the reader for every state
/// these tables reach.
async fn described(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, described) = describe_multiplex(router, id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMultiplex({id}): expected 200, got {status}: {described}"
    );
    described
}

/// The multiplex `id`, read back once its `State` is `expected`.
///
/// A poll, not a sleep: a state the clock owes is reached when it is reached, so
/// the test looks again until the condition holds or [`POLL_DEADLINE`] passes.
async fn in_state(router: &MultiServiceRouter, case: &str, id: &str, expected: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < POLL_DEADLINE {
        last = described(router, case, id).await;
        if last["state"] == json!(expected) {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: multiplex {id} never reached {expected:?}, last read {last}");
}

/// Drive the multiplex `id` into `state` through the operations a client uses,
/// asserting every step was accepted.
///
/// A fresh multiplex is already coming up, and every other state is reached from
/// the stopped one the create settles into: a start takes it up, a stop brings a
/// running one back down, and a delete leaves the two tear-down states.
async fn drive_to(router: &MultiServiceRouter, case: &str, id: &str, state: &str) {
    if state == "CREATING" {
        return;
    }
    in_state(router, case, id, IDLE).await;
    match state {
        "STARTING" | "RUNNING" | "STOPPING" => {
            let (status, started) = start_multiplex(router, id).await;
            assert_eq!(
                status, 202,
                "case {case:?}: StartMultiplex: expected 202, got {status}: {started}"
            );
            if state != STARTING {
                in_state(router, case, id, "RUNNING").await;
            }
            // A multiplex stops from the running state, so `STOPPING` is one
            // step past `RUNNING`.
            if state == "STOPPING" {
                let (status, stopping) = stop_multiplex(router, id).await;
                assert_eq!(
                    status, 202,
                    "case {case:?}: StopMultiplex: expected 202, got {status}: {stopping}"
                );
            }
        }
        "DELETING" | "DELETED" => {
            let (status, deleting) = delete_multiplex(router, id).await;
            assert_eq!(
                status, 202,
                "case {case:?}: DeleteMultiplex: expected 202, got {status}: {deleting}"
            );
            if state == "DELETED" {
                in_state(router, case, id, "DELETED").await;
            }
        }
        other => panic!("case {case:?}: no step of this service reaches {other}"),
    }
}

/// The multiplex `id` as a reader reports it, asserted to be in `state` — the
/// record the start under test is about to be judged against.
async fn judged_against(router: &MultiServiceRouter, case: &str, id: &str, state: &str) -> Value {
    let record = described(router, case, id).await;
    assert_eq!(
        record["state"],
        json!(state),
        "case {case:?}: expected the multiplex in {state}, got {:?}",
        record["state"]
    );
    record
}

/// Assert `message` is this class's sentence for `id` in `state`, and that it
/// carries nothing of the multiplex besides the id the request named.
fn assert_class_message(case: &str, message: &str, id: &str, state: &str, hidden: &[&str]) {
    assert_eq!(
        message,
        not_startable(id, state),
        "case {case:?}: expected {:?}, got {message:?}",
        not_startable(id, state)
    );
    for leaked in hidden {
        assert!(
            !message.contains(leaked),
            "case {case:?}: expected the message not to carry {leaked:?}, got {message:?}"
        );
    }
}

/// Assert the refused start wrote nothing: everything the record carries besides
/// the two members the CLOCK alone still moves — the state a transient one
/// settles into and the pipeline count that follows it — reads back exactly as
/// it did an instant before the refusal, and the record never entered the
/// `STARTING` an accepted start would have written.
fn wrote_nothing(case: &str, before: &Value, after: &Value, state: &str) {
    let clockless = |record: &Value| {
        let mut record = record.clone();
        if let Some(members) = record.as_object_mut() {
            members.remove("state");
            members.remove("pipelinesRunningCount");
        }
        record
    };
    assert_eq!(
        clockless(before),
        clockless(after),
        "case {case:?}: expected the refused start to leave the record alone, got {after}"
    );
    if state != STARTING {
        assert_ne!(
            after["state"],
            json!(STARTING),
            "case {case:?}: expected the refused start to have begun no start, got {after}"
        );
    }
}

/// The state the start about to be sent will be judged against: the record read
/// immediately before it, with the state it reports, asserted to still be one a
/// start refuses — a transient state the clock owes a successor settles when it
/// comes due, so a row names the state of the read that PRECEDES each call
/// rather than assuming the record stood still.
async fn about_to_refuse(router: &MultiServiceRouter, case: &str, id: &str) -> (Value, String) {
    let record = described(router, case, id).await;
    let state = record["state"].as_str().unwrap_or_default().to_string();
    assert!(
        REFUSING_STATES.contains(&state.as_str()),
        "case {case:?}: expected a state a start refuses, got {record}"
    );
    (record, state)
}

/// Rule: a start sent to a multiplex that has not stopped answers 409
/// `ConflictException` with this class's sentence, naming the multiplex the
/// request asked for and the state its record is in and nothing else of that
/// multiplex — from EITHER entry point, the single-resource `StartMultiplex` and
/// the `multiplexIds` arm of `BatchStart`, which carries that same code and
/// sentence per failed entry while the batch call itself answers 200 — and
/// neither refusal writes anything.
///
/// The rows are every state a start is refused in, each on a multiplex of a
/// different shape (name, settings, tags, programs held), so the sentence is
/// shown to depend on the state and the id alone.
#[tokio::test]
async fn a_start_refused_on_the_state_carries_the_classs_sentence() {
    for (index, state) in REFUSING_STATES.into_iter().enumerate() {
        let shape = &shapes()[index % shapes().len()];
        let case = format!("a start of a {state} {:?} multiplex", shape.name);
        let router = medialive();
        let (id, arn) = multiplex_of(&router, &case, shape).await;
        drive_to(&router, &case, &id, state).await;
        let before = judged_against(&router, &case, &id, state).await;

        // The single-resource form.
        let (status, refusal) = start_multiplex(&router, &id).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the modelled 409, got {status}: {refusal}"
        );
        assert_eq!(
            error_code(status, &refusal),
            CONFLICT,
            "case {case:?}: expected {CONFLICT}, got {refusal}"
        );
        assert_class_message(
            &case,
            &error_message(&refusal),
            &id,
            state,
            &[shape.name, arn.as_str()],
        );

        // The batch form, judged against the record as it reads just before it.
        let (_, batched_state) = about_to_refuse(&router, &case, &id).await;
        let (status, response) = call(
            &router,
            "POST",
            BATCH_START,
            Some(json!({ "multiplexIds": [id.clone()] })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a batch reports each resource on its own, so the call itself \
             answers 200, got {status}: {response}"
        );
        assert_eq!(
            response["successful"],
            json!([]),
            "case {case:?}: expected no accepted start, got {response}"
        );
        assert_eq!(
            response["failed"],
            json!([{
                "id": id,
                "arn": arn,
                "code": CONFLICT,
                "message": not_startable(&id, &batched_state),
            }]),
            "case {case:?}: expected the batch's failure entry to carry this class's sentence \
             for the {batched_state} state, got {:?}",
            response["failed"]
        );
        assert_class_message(
            &case,
            response["failed"][0]["message"]
                .as_str()
                .unwrap_or_default(),
            &id,
            &batched_state,
            &[shape.name],
        );

        let after = described(&router, &case, &id).await;
        wrote_nothing(&case, &before, &after, state);
    }
}

/// The control: the state is the only thing either entry point refuses on, so
/// the very same start is ACCEPTED once the multiplex has stopped — whatever it
/// is called, carries or holds, and whichever entry point asks. Without this row
/// the sentence above could be the answer to any start at all.
#[tokio::test]
async fn the_same_start_is_accepted_once_the_multiplex_has_stopped() {
    for (index, shape) in shapes().iter().enumerate() {
        let case = format!("the stopped {:?} multiplex", shape.name);
        let router = medialive();
        let (id, _) = multiplex_of(&router, &case, shape).await;
        // A new multiplex is still coming up, which is itself a refusing state;
        // the clock settles it into the one state a start accepts.
        in_state(&router, &case, &id, IDLE).await;

        // Odd rows ask through the batch, even rows through the single-resource
        // start, so both entry points are shown accepting.
        if index % 2 == 0 {
            let (status, accepted) = start_multiplex(&router, &id).await;
            assert_eq!(
                status, 202,
                "case {case:?}: StartMultiplex: expected 202, got {status}: {accepted}"
            );
            assert_eq!(
                accepted["state"],
                json!(STARTING),
                "case {case:?}: expected the accepted start to answer {STARTING}, got {accepted}"
            );
        } else {
            let (status, response) = call(
                &router,
                "POST",
                BATCH_START,
                Some(json!({ "multiplexIds": [id.clone()] })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: BatchStart: expected 200, got {status}: {response}"
            );
            assert_eq!(
                response["failed"],
                json!([]),
                "case {case:?}: expected the stopped multiplex to be accepted, got {response}"
            );
            assert_eq!(
                response["successful"][0]["state"],
                json!(STARTING),
                "case {case:?}: expected the accepted start to report {STARTING}, got {response}"
            );
        }

        // And the start the service accepted really runs the multiplex: the
        // clock settles it into the state a further start is then refused in.
        in_state(&router, &case, &id, "RUNNING").await;
    }
}
