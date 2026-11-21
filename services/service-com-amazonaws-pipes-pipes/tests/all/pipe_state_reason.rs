// SPDX-License-Identifier: CC0-1.0

//! What a pipe reports as its `StateReason`, end to end over the wire.
//!
//! Every assertion here goes through `MultiServiceRouter::dispatch` — the one path a
//! socket-served request takes — reusing the harness in `tests/all/pipe_lifecycle.rs`
//! rather than copying it, so what is tested is the HTTP behaviour of the lifecycle calls
//! read back through `DescribePipe`/`ListPipes` and not a Rust helper.
//!
//! RULE: a pipe's `StateReason` says HOW it came to be in the state it is in, so a state a
//! LIFECYCLE CALL drove it into — a creation into the `DesiredState` it asked for, a start,
//! a stop, an update — is reported with the fixed user-initiated reason token, from the
//! moment the call lands and for as long as the pipe rests there, by every read that
//! carries the member.
//!
//! Every pipe below reads an Amazon SQS QUEUE, which is not a source read per shard: a pipe
//! over one that is reports its poller's reading instead, which is the table of
//! `tests/all/pipe_kinesis_stream_source.rs`.
//!
//! The tables span the class that rule generalises over: which lifecycle call drove the
//! state, which state it drove the pipe into, whether the move is still in flight or has
//! settled, and the shape of the read (`DescribePipe`, an unpaged listing, a filtered one,
//! and each page of a paged one).

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::STATE_REASON_LENGTH_MAX;
use service_com_amazonaws_pipes_pipes::state::pipe_state::{
    CREATING, RUNNING, STARTING, STOPPED, STOPPING, UPDATING,
};
use service_com_amazonaws_pipes_pipes::state::USER_INITIATED_STATE_REASON;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_bring_up::settled;
use crate::pipe_lifecycle::{
    call, create, describe, list, minimal_body, router, Reply, PAST_SETTLE, REGION,
};
use crate::pipe_listing_state_reason::{paged_walk, MIN_LIMIT};

const SOURCE: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
const TARGET: &str = "arn:aws:sqs:us-west-2:000000000000:results";
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-role";

/// A `CreatePipe` body, optionally asking for a `DesiredState`.
fn create_body(desired_state: Option<&str>) -> Value {
    let mut body = minimal_body(SOURCE, TARGET);
    if let Some(state) = desired_state {
        body.as_object_mut()
            .expect("the create body is an object")
            .insert("DesiredState".to_string(), json!(state));
    }
    body
}

/// One lifecycle call on an existing pipe, named by the operation the case drives.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` drives the states of the
/// pipes it lists through the SAME request path rather than a copy of it.
pub(crate) async fn drive(router: &MultiServiceRouter, name: &str, operation: &str) -> Reply {
    match operation {
        "StartPipe" => {
            call(
                router,
                "POST",
                &format!("/v1/pipes/{name}/start"),
                None,
                REGION,
            )
            .await
        }
        "StopPipe" => {
            call(
                router,
                "POST",
                &format!("/v1/pipes/{name}/stop"),
                None,
                REGION,
            )
            .await
        }
        "UpdatePipe" => {
            call(
                router,
                "PUT",
                &format!("/v1/pipes/{name}"),
                Some(json!({ "RoleArn": ROLE })),
                REGION,
            )
            .await
        }
        other => panic!("case {name:?}: no lifecycle call named {other:?}"),
    }
}

/// The `StateReason` each listed pipe reported, keyed by the pipe's name, in the order the
/// listing returned them. `None` for an entry that omitted the member altogether, which is
/// the omission under test.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` reads the entries of its
/// listings the same way, so there is ONE reader of the `Pipes[]` shape.
pub(crate) fn listed_reasons(reply: &Reply) -> Vec<(String, Option<String>)> {
    reply
        .member("Pipes")
        .as_array()
        .map(|pipes| {
            pipes
                .iter()
                .map(|pipe| {
                    (
                        pipe.get("Name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        pipe.get("StateReason")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the lifecycle table: the call that drove the pipe's state, and the states
/// that call passes the pipe through.
struct Row {
    /// Also the pipe's name, so every row is independent.
    case: &'static str,
    /// The `DesiredState` the pipe is created with, or `None` for a create that names none.
    desired_state: Option<&'static str>,
    /// The lifecycle call that drives the state under test, or `None` for the row the
    /// creation itself drives.
    operation: Option<&'static str>,
    /// The transient `PipeState` that call reports while its move is in flight.
    transient: &'static str,
    /// The `PipeState` the pipe settles in.
    settled: &'static str,
}

/// RULE: whichever lifecycle call drove the pipe's current state, `DescribePipe` reports
/// the user-initiated reason for it — while the move is still in flight and once it has
/// settled.
///
/// The rows differ along the call that drove the state, the state it drove the pipe into,
/// and the state the pipe was in before it.
#[tokio::test]
async fn describe_pipe_reports_the_user_initiated_reason_for_every_caller_driven_state() {
    let rows = vec![
        Row {
            case: "created-default",
            desired_state: None,
            operation: None,
            transient: "CREATING",
            settled: "RUNNING",
        },
        Row {
            case: "created-running",
            desired_state: Some("RUNNING"),
            operation: None,
            transient: "CREATING",
            settled: "RUNNING",
        },
        Row {
            case: "created-stopped",
            desired_state: Some("STOPPED"),
            operation: None,
            transient: "CREATING",
            settled: "STOPPED",
        },
        Row {
            case: "started-from-stopped",
            desired_state: Some("STOPPED"),
            operation: Some("StartPipe"),
            transient: "STARTING",
            settled: "RUNNING",
        },
        Row {
            case: "stopped-from-running",
            desired_state: Some("RUNNING"),
            operation: Some("StopPipe"),
            transient: "STOPPING",
            settled: "STOPPED",
        },
        Row {
            case: "updated-while-running",
            desired_state: Some("RUNNING"),
            operation: Some("UpdatePipe"),
            transient: "UPDATING",
            settled: "RUNNING",
        },
    ];

    for row in rows {
        let (case, transient, settled) = (row.case, row.transient, row.settled);
        let router = router();
        let created = create(&router, case, create_body(row.desired_state)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe must succeed, got {created:?}"
        );

        // A case that drives a LATER call lets the creation settle first, so the state
        // under test is the one that call drove and not the creation's.
        let driving = match row.operation {
            None => created,
            Some(operation) => {
                tokio::time::sleep(PAST_SETTLE).await;
                let reply = drive(&router, case, operation).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: {operation} must succeed, got {reply:?}"
                );
                reply
            }
        };
        assert_eq!(
            driving.string("CurrentState"),
            transient,
            "case {case:?}: expected the call to report the pipe {transient}, got {driving:?}"
        );

        // While the move is still in flight: the pipe is in a transient state its caller
        // asked for, so the reason is already the user-initiated one.
        let described = describe(&router, case).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribePipe must find the pipe, got {described:?}"
        );
        let in_flight = described.member("StateReason").as_str().map(str::to_string);
        assert_eq!(
            in_flight.as_deref(),
            Some(USER_INITIATED_STATE_REASON),
            "case {case:?}: expected StateReason {USER_INITIATED_STATE_REASON} while the \
             move is in flight, got {in_flight:?}"
        );

        // And once the move has landed, the reason still says how the pipe got there.
        tokio::time::sleep(PAST_SETTLE).await;
        let described = describe(&router, case).await;
        assert_eq!(
            described.string("CurrentState"),
            settled,
            "case {case:?}: expected the pipe to settle in {settled}, got {described:?}"
        );
        let rested = described.member("StateReason").as_str().map(str::to_string);
        assert_eq!(
            rested.as_deref(),
            Some(USER_INITIATED_STATE_REASON),
            "case {case:?}: expected StateReason {USER_INITIATED_STATE_REASON} for a pipe \
             resting in the state its caller asked for, got {rested:?}"
        );
    }
}

/// RULE: as above, read through `ListPipes`: every listed pipe carries the reason for the
/// state it is in, whichever listing shape returned it.
///
/// The rows differ along the shape of the read — unpaged, filtered by name prefix, filtered
/// by desired state, and each page of a paged listing — over a set of pipes whose states
/// were driven by different lifecycle calls.
#[tokio::test]
async fn every_listed_pipe_reports_the_reason_for_the_state_it_is_in() {
    let router = router();
    // Three pipes whose resting states were driven differently: one left as created, one
    // stopped by a later call, one started by a later call.
    let pipes: Vec<(&str, Option<&str>, Option<&str>)> = vec![
        ("list-a-created", None, None),
        ("list-b-stopped", Some("RUNNING"), Some("StopPipe")),
        ("list-c-started", Some("STOPPED"), Some("StartPipe")),
    ];
    for (name, desired_state, _) in &pipes {
        let created = create(&router, name, create_body(*desired_state)).await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: CreatePipe must succeed, got {created:?}"
        );
    }
    tokio::time::sleep(PAST_SETTLE).await;
    for (name, _, operation) in &pipes {
        if let Some(operation) = operation {
            let reply = drive(&router, name, operation).await;
            assert_eq!(
                reply.status, 200,
                "case {name:?}: {operation} must succeed, got {reply:?}"
            );
        }
    }
    tokio::time::sleep(PAST_SETTLE).await;

    // (case, the query, the names the listing is expected to return)
    let listings: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "unpaged",
            String::new(),
            vec!["list-a-created", "list-b-stopped", "list-c-started"],
        ),
        (
            "name-prefix",
            "?NamePrefix=list-b".to_string(),
            vec!["list-b-stopped"],
        ),
        (
            "desired-state-filter",
            "?DesiredState=STOPPED".to_string(),
            vec!["list-b-stopped"],
        ),
        (
            "current-state-filter",
            "?CurrentState=RUNNING".to_string(),
            vec!["list-a-created", "list-c-started"],
        ),
    ];
    for (case, query, expected_names) in listings {
        let listed = list(&router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListPipes must succeed, got {listed:?}"
        );
        let reasons = listed_reasons(&listed);
        let names: Vec<&str> = reasons.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names, expected_names,
            "case {case:?}: expected the listing to return {expected_names:?}, got {names:?}"
        );
        for (name, reason) in &reasons {
            assert_eq!(
                reason.as_deref(),
                Some(USER_INITIATED_STATE_REASON),
                "case {case:?}: expected pipe {name:?} to carry StateReason \
                 {USER_INITIATED_STATE_REASON}, got {reason:?}"
            );
        }
    }

    // A paged listing: the member is on every entry of the first page and of the
    // continuation the `NextToken` resumes.
    let first = list(&router, "?Limit=2").await;
    let first_reasons = listed_reasons(&first);
    assert_eq!(
        first_reasons.len(),
        2,
        "case \"page-1\": expected two entries on a Limit=2 page, got {first:?}"
    );
    let token = first.string("NextToken");
    assert!(
        !token.is_empty(),
        "case \"page-1\": expected a NextToken for the remaining pipe, got {first:?}"
    );
    let second = list(&router, &format!("?Limit=2&NextToken={token}")).await;
    let second_reasons = listed_reasons(&second);
    assert_eq!(
        second_reasons.len(),
        1,
        "case \"page-2\": expected the one remaining pipe, got {second:?}"
    );
    for (page, reasons) in [("page-1", first_reasons), ("page-2", second_reasons)] {
        for (name, reason) in reasons {
            assert_eq!(
                reason.as_deref(),
                Some(USER_INITIATED_STATE_REASON),
                "case {page:?}: expected pipe {name:?} to carry StateReason \
                 {USER_INITIATED_STATE_REASON}, got {reason:?}"
            );
        }
    }
}

/// One row of the in-flight table: the move a lifecycle call puts in flight, and the
/// transient `PipeState` the call reports while that move is still on its way.
struct InFlight {
    /// Also the pipe's name, so every row is independent of the others.
    case: &'static str,
    /// The `DesiredState` its creation asks for, or `None` to leave it at the default.
    desired_state: Option<&'static str>,
    /// The lifecycle call driven once the creation has come to rest, or `None` for the row
    /// whose in-flight move IS the creation.
    operation: Option<&'static str>,
    /// The `PipeState` that call reports while the move it started is in flight.
    transient: &'static str,
}

/// The `StateReason` a read of ONE pipe reports at this instant, or `None` when the read
/// carried the member not at all.
async fn described_reason(router: &MultiServiceRouter, case: &str) -> Option<String> {
    let described = describe(router, case).await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: DescribePipe must find the pipe, got {described:?}"
    );
    described.member("StateReason").as_str().map(str::to_string)
}

/// Assert every entry of one listing carries the reason the pipe it names reached its
/// state by — the user-initiated token for a move its caller drove — and that the value is
/// a legal one of the member that reports it.
///
/// `STATE_REASON_LENGTH_MAX` is the upper bound of `PipeStateReason`, the shape `Pipe`'s
/// `StateReason` targets (`@length(min: 0, max: 512)`), read from the crate rather than
/// restated here.
fn assert_each_entry_carries_the_reason(case: &str, entries: &[(String, Option<String>)]) {
    for (name, reason) in entries {
        let Some(reason) = reason.as_deref() else {
            panic!(
                "case {case:?}: the listing returned pipe {name:?} with no StateReason at \
                 all"
            );
        };
        assert_eq!(
            reason, USER_INITIATED_STATE_REASON,
            "case {case:?}: expected pipe {name:?} to carry StateReason \
             {USER_INITIATED_STATE_REASON} for a move its caller drove, got {reason:?}"
        );
        assert!(
            reason.chars().count() <= STATE_REASON_LENGTH_MAX,
            "case {case:?}: the StateReason of pipe {name:?} is {} characters, past the \
             {STATE_REASON_LENGTH_MAX} a PipeStateReason may carry: {reason:?}",
            reason.chars().count()
        );
    }
}

/// The names of one listing's entries, in the order it returned them.
fn entry_names(entries: &[(String, Option<String>)]) -> Vec<&str> {
    entries.iter().map(|(name, _)| name.as_str()).collect()
}

/// RULE: every `Pipe` summary `ListPipes` returns carries the listed pipe's own
/// `StateReason` — the very value a read of that one pipe reports at the same instant —
/// whichever listing shape selected the entry, including while the move the entry reports
/// is still in flight.
///
/// The rows differ along what the rule generalises over: which lifecycle call put the move
/// in flight (a creation into the default state, a creation into the state it asked for, a
/// start, a stop, a reconfiguration), the transient state the pipe is therefore in, the
/// shape of the listing (unpaged, a name prefix matching one pipe, a name prefix matching
/// every pipe, a source prefix, a target prefix, and every page of a paged walk at two page
/// bounds), and whether the move is still in flight or has come to rest.
#[tokio::test]
async fn every_listing_carries_each_pipes_reason_while_its_move_is_still_in_flight() {
    let rows = vec![
        InFlight {
            case: "flight-created-default",
            desired_state: None,
            operation: None,
            transient: CREATING,
        },
        InFlight {
            case: "flight-created-stopped",
            desired_state: Some(STOPPED),
            operation: None,
            transient: CREATING,
        },
        InFlight {
            case: "flight-started",
            desired_state: Some(STOPPED),
            operation: Some("StartPipe"),
            transient: STARTING,
        },
        InFlight {
            case: "flight-stopped",
            desired_state: Some(RUNNING),
            operation: Some("StopPipe"),
            transient: STOPPING,
        },
        InFlight {
            case: "flight-updated",
            desired_state: Some(RUNNING),
            operation: Some("UpdatePipe"),
            transient: UPDATING,
        },
    ];
    let router = router();

    // The rows whose move under test is a LATER call are created first and left to come to
    // rest, so what is in flight when the listings are taken is that call's move and not
    // the creation's. The wait is a POLL of the pipe, never a fixed pause.
    let driven = || rows.iter().filter(|row| row.operation.is_some());
    for row in driven() {
        let created = create(&router, row.case, create_body(row.desired_state)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe must succeed, got {created:?}",
            row.case
        );
    }
    for row in driven() {
        settled(&router, row.case, row.case).await;
    }
    for row in driven() {
        let operation = row.operation.expect("the row drives a later call");
        let answered = drive(&router, row.case, operation).await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: {operation} must succeed, got {answered:?}",
            row.case
        );
        assert_eq!(
            answered.string("CurrentState"),
            row.transient,
            "case {:?}: expected {operation} to report the pipe {}, got {answered:?}",
            row.case,
            row.transient
        );
    }
    // The creation rows are created LAST, so their own move is the one in flight.
    for row in rows.iter().filter(|row| row.operation.is_none()) {
        let created = create(&router, row.case, create_body(row.desired_state)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe must succeed, got {created:?}",
            row.case
        );
        assert_eq!(
            created.string("CurrentState"),
            row.transient,
            "case {:?}: expected CreatePipe to report the pipe {}, got {created:?}",
            row.case,
            row.transient
        );
    }

    let mut every_pipe: Vec<&str> = rows.iter().map(|row| row.case).collect();
    every_pipe.sort_unstable();

    // Every modelled way of selecting an entry whose selection does not turn on the state
    // the pipe is in, so each row's expected entries are the same whether the moves are
    // still in flight or have landed by the time the listing is taken.
    let listings: Vec<(&str, String, Vec<&str>)> = vec![
        ("unpaged", String::new(), every_pipe.clone()),
        (
            "name-prefix-of-one",
            "?NamePrefix=flight-started".to_string(),
            vec!["flight-started"],
        ),
        (
            "name-prefix-shared-by-every-pipe",
            "?NamePrefix=flight-".to_string(),
            every_pipe.clone(),
        ),
        (
            "source-prefix-at-the-whole-arn",
            format!("?SourcePrefix={SOURCE}"),
            every_pipe.clone(),
        ),
        (
            "target-prefix-of-the-service",
            "?TargetPrefix=arn:aws:sqs".to_string(),
            every_pipe.clone(),
        ),
    ];
    for (case, query, expected) in listings {
        let listed = list(&router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListPipes must succeed, got {listed:?}"
        );
        let entries = listed_reasons(&listed);
        assert_eq!(
            entry_names(&entries),
            expected,
            "case {case:?}: expected the listing to return {expected:?}, got {listed:?}"
        );
        assert_each_entry_carries_the_reason(case, &entries);
    }

    // Both page bounds of a paged walk: the member is on the entry of every page, and the
    // pages together are the whole inventory in name order.
    for limit in [MIN_LIMIT, 2] {
        let walk = format!("paged-walk-at-{limit}");
        let entries = paged_walk(&router, &walk, limit).await;
        assert_eq!(
            entry_names(&entries),
            every_pipe,
            "case {walk:?}: expected the pages together to return {every_pipe:?}"
        );
        assert_each_entry_carries_the_reason(&walk, &entries);
    }

    // And the reason a listing projects is the very one a read of that ONE pipe reports.
    let in_flight = list(&router, "").await;
    let projected = listed_reasons(&in_flight);
    for (name, reason) in &projected {
        let described = described_reason(&router, name).await;
        assert_eq!(
            reason.as_deref(),
            described.as_deref(),
            "case \"in-flight-projection\": expected the listed StateReason of pipe \
             {name:?} to be the one DescribePipe reports ({described:?}), got {reason:?}"
        );
    }

    // Once every move has come to rest — the listing a client polling a start/stop to
    // converge takes — the member is still there, and still the per-pipe read's value.
    for row in &rows {
        settled(&router, row.case, row.case).await;
    }
    let rested = list(&router, "").await;
    let entries = listed_reasons(&rested);
    assert_eq!(
        entry_names(&entries),
        every_pipe,
        "case \"rested\": expected the listing to return {every_pipe:?}, got {rested:?}"
    );
    assert_each_entry_carries_the_reason("rested", &entries);
    for (name, reason) in &entries {
        let described = described_reason(&router, name).await;
        assert_eq!(
            reason.as_deref(),
            described.as_deref(),
            "case \"rested\": expected the listed StateReason of pipe {name:?} to be the \
             one DescribePipe reports ({described:?}), got {reason:?}"
        );
    }
}
