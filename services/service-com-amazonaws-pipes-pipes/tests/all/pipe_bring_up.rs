// SPDX-License-Identifier: CC0-1.0

//! Whether a pipe is BROUGHT UP at all — end to end over the wire, with IAM, Amazon SQS and
//! CloudWatch mounted beside pipes on one engine (the harness of `tests/all/pipe_metrics.rs`,
//! reused rather than copied).
//!
//! The RULE asserted over the tables below: a lifecycle call that puts a pipe on its way to
//! a healthy state completes only if the pipe could REACH ITS SOURCE — the source has to be
//! a resource the service that owns it holds, and the pipe's execution role has to be one
//! this service can assume AND one whose policies leave it at least one of the calls its
//! source stage makes on that source — so a pipe that could not settles in the terminal
//! failure state of the move that was in flight (`CREATE_FAILED` for the creation,
//! `START_FAILED` for the start, `UPDATE_FAILED` for the reconfiguration), carrying a
//! non-empty `StateReason` that says which, and it runs nothing: nothing is consumed,
//! nothing is delivered and it publishes no counter at all.
//!
//! A role that withholds only SOME of those calls is deliberately NOT part of it, and
//! neither is the TARGET side: a pipe whose role may look at its source and not take its
//! records, or may not write to its target, comes UP and fails each execution at the stage
//! whose call is withheld (`tests/all/pipe_metrics.rs`,
//! `tests/all/pipe_source_checkout.rs`), which is what lets it move the record it withheld
//! as soon as that grant is in force. The last three rows of the create table are that
//! control, held here beside the refusals so the asymmetry is asserted and not assumed.
//!
//! `tests/all/pipe_unassumable_role.rs` is the trust document's own table — the first thing
//! this rule reads — and carries the rows that generalise over what a trust document says.
//!
//! The last table is the TEXT of what a pipe reports when the call that takes its source's
//! records is the one its role is missing: the two tables above assert where such a move
//! lands, and that one asserts the sentence, over the source kinds that decide which call is
//! named.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::{
    SERVICE_PRINCIPAL, STATE_REASON_LENGTH_MAX,
};
use service_com_amazonaws_pipes_pipes::state::metrics::PIPE_COUNTERS;
use service_com_amazonaws_pipes_pipes::state::pipe_state::{
    CREATE_FAILED, RUNNING, START_FAILED, STOPPED, UPDATE_FAILED,
};
use service_com_amazonaws_pipes_pipes::state::TRANSIENT_PIPE_STATES;
use service_com_amazonaws_pipes_pipes::state::USER_INITIATED_STATE_REASON;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{
    collect_at_least, collect_during, counted, given_queue, send, sqs, QUIET, VISIBLE_ATTRIBUTE,
};
use crate::pipe_dynamodb_stream_source::given_streamed_table;
use crate::pipe_kinesis_stream_source::given_stream;
use crate::pipe_lifecycle::{call, describe, Reply, REGION};
use crate::pipe_metrics::{emulator, given_role_stating, series, withdraw_from_role};

/// How long a pipe is given to settle out of the state a lifecycle call left it in. Well
/// above the state-settle span, so a row fails only when the pipe never settled at all.
const BUDGET: Duration = Duration::from_secs(10);

/// How long between two looks at a pipe while waiting for it to settle.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// `TargetParameters.InputTemplate`, so a delivered message carries the record's own body
/// and a delivery can be attributed: `<$.body>` selects the `body` of the record envelope.
const BODY_TEMPLATE: &str = "<$.body>";

/// The call a pipe whose role may not take an Amazon SQS source's records reports as the one
/// it does not have permission to make, as its own `StateReason` names it: the API action and
/// the service that action is called on. The WHOLE sentence that fragment sits in is pinned
/// by the sentence table at the end of this file; the two tables above assert only that the
/// sentence a failed move left names that call.
const RECEIVE_MESSAGE_ON_SQS: &str = "ReceiveMessage on SQS";

/// The ARN form Amazon SQS gives a queue of this account and Region, for the row whose
/// source queue is never created at all: there is no `CreateQueue` answer to read one off.
fn unheld_queue_arn(name: &str) -> String {
    format!(
        "arn:aws:sqs:{REGION}:{}:{name}",
        engine::emulator_account_id()
    )
}

/// Every Amazon SQS call a pipe with a queue source and a queue target makes, allowed on
/// exactly the resource each is made on.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` brings its healthy pipes up
/// under the same grants, through this one statement set rather than a copy of it.
pub(crate) fn every_call(source: &str, target: &str) -> Value {
    json!([
        {
            "Effect": "Allow",
            "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
            "Resource": source,
        },
        { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target },
    ])
}

/// The delivery and nothing else: a role that may write the target and may not take the
/// source's records.
fn the_target_only(target: &str) -> Value {
    json!([{ "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target }])
}

/// Delete the queue at `url`, so a pipe's `Source` keeps its well-formed ARN while the
/// resource that ARN names is gone.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` takes a source away the same
/// way, through this one path rather than a copy of it.
pub(crate) async fn delete_queue(router: &MultiServiceRouter, case: &str, url: &str) {
    let (status, body) = sqs(router, "DeleteQueue", &json!({ "QueueUrl": url })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteQueue answered {status} {body}"
    );
}

/// `CreatePipe` for one case: the source and target it names, the role it acts as, and the
/// `DesiredState` it asks for when it asks for one.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` creates the pipes it lists
/// over the SAME request path.
pub(crate) async fn create_pipe(
    router: &MultiServiceRouter,
    name: &str,
    source_arn: &str,
    target_arn: &str,
    role_arn: &str,
    desired_state: Option<&str>,
) -> Reply {
    let mut body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": role_arn,
        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
        "TargetParameters": { "InputTemplate": BODY_TEMPLATE },
    });
    if let Some(desired_state) = desired_state {
        body["DesiredState"] = json!(desired_state);
    }
    call(router, "POST", &format!("/v1/pipes/{name}"), Some(body), REGION).await
}

/// `DescribePipe` once the pipe has settled out of whatever move was in flight, or a panic
/// naming the state it was still passing through when the budget ran out.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` waits for the pipes it lists
/// to come to rest the same way, reading the crate's own list of transient states.
pub(crate) async fn settled(router: &MultiServiceRouter, case: &str, name: &str) -> Reply {
    let deadline = Instant::now() + BUDGET;
    loop {
        let last = describe(router, name).await;
        assert_eq!(
            last.status, 200,
            "case {case:?}: DescribePipe answered {} {}",
            last.status, last.body
        );
        let state = last.string("CurrentState");
        // The states a pipe PASSES THROUGH on its way out of a lifecycle call, read from
        // the crate's own list so this cannot drift from it.
        if !TRANSIENT_PIPE_STATES.contains(&state.as_str()) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the pipe was still {state} after {BUDGET:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert the pipe `name` reports no datapoint of any counter: a pipe that was never brought
/// up publishes nothing at all, which is what tells it from one that ran and was refused.
async fn publishes_nothing(router: &MultiServiceRouter, case: &str, name: &str) {
    for metric in PIPE_COUNTERS {
        let (points, sum) = series(router, name, metric).await;
        assert_eq!(
            (points, sum),
            (0, 0.0),
            "case {case:?}: a pipe that never ran must publish no {metric} series, got \
             {points} datapoint(s) summing to {sum}"
        );
    }
}

/// Assert the `StateReason` of a failed move carries `mentions` — and names the source, for a
/// failure whose sentence is about the source RESOURCE (`source_arn`) rather than about the
/// call the pipe's role may not make — within the bound the member that reports it has.
///
/// `pub(crate)` because `tests/all/pipe_listing_state_reason.rs` reads the failure sentence a
/// LISTING reports the same way: one reading of what that sentence has to say.
pub(crate) fn reason_says(case: &str, reason: &str, source_arn: Option<&str>, mentions: &[&str]) {
    if let Some(source_arn) = source_arn {
        assert!(
            reason.contains(source_arn),
            "case {case:?}: expected the StateReason to name the source {source_arn}, got \
             {reason:?}"
        );
    }
    for mention in mentions {
        assert!(
            reason.contains(mention),
            "case {case:?}: expected the StateReason to carry {mention:?}, got {reason:?}"
        );
    }
    assert!(
        reason.chars().count() <= STATE_REASON_LENGTH_MAX,
        "case {case:?}: the StateReason is {} characters, past the \
         {STATE_REASON_LENGTH_MAX} a PipeStateReason may carry: {reason:?}",
        reason.chars().count()
    );
}

/// One row of the create table: how its source and role are wired, and where the creation
/// lands.
struct Row {
    case: &'static str,
    /// Whether the row's source queue is a resource the account holds when the pipe is
    /// created. A row whose source is not held either never created it (`created_source`
    /// is false) or created it and deleted it again, which is a well-formed queue ARN
    /// naming nothing.
    held_source: bool,
    /// Whether the row's source queue is created at all.
    created_source: bool,
    /// The statements the row's execution role carries, built from its source and target.
    policy: fn(&str, &str) -> Value,
    /// The `DesiredState` the creation asks for, or `None` to leave it at the default.
    desired_state: Option<&'static str>,
    /// The `CurrentState` the pipe has to settle in.
    settles_in: &'static str,
    /// Text the settled `StateReason` has to carry.
    reason_mentions: &'static [&'static str],
    /// Whether that sentence NAMES the source: it is about the source resource rather than
    /// about the call the pipe's role may not make.
    names_source: bool,
    /// Whether the pipe, having been brought up, must move its record to its target.
    delivers: bool,
}

/// What one row was given, so the assertions can read the resources it named.
struct Prepared<'row> {
    row: &'row Row,
    source_arn: String,
    /// The url of the row's source queue, for a row that created one.
    source_url: Option<String>,
    target_url: String,
    /// The body the row sent to its source, for a row whose source was there to take one.
    marker: Option<String>,
}

/// RULE: `CreatePipe` completes only if the pipe it creates could reach its source — the
/// source is a resource its owner holds, and the role the pipe acts as may make at least one
/// of the calls its source stage makes on that source — and a creation that could not settles
/// in `CREATE_FAILED` with a `StateReason` saying which, having consumed nothing, delivered
/// nothing and published nothing.
///
/// The rows differ along what the rule generalises over: WHY the pipe could not reach its
/// source (it was deleted, it was never created, its role's policies state nothing about it,
/// they state every call on another queue only, they deny every one of them), HOW a policy
/// names the call it allows (exactly, by wildcard), and which `DesiredState` the creation
/// asked for — plus the three controls that fix the rule's edge: a role that may reach the
/// source and not take its records, and one that may take them and not write the target,
/// each bring the pipe UP, because what a running pipe may not do is the data plane's to
/// fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_pipe_brings_up_only_a_pipe_that_could_poll_its_source() {
    let rows = [
        Row {
            case: "source-queue-deleted-before-the-pipe",
            held_source: false,
            created_source: true,
            policy: every_call,
            desired_state: Some(RUNNING),
            settles_in: CREATE_FAILED,
            reason_mentions: &["does not exist"],
            names_source: true,
            delivers: false,
        },
        Row {
            // The same failure where the queue was never created at all, and the creation
            // asked for STOPPED: a creation that cannot be completed lands in
            // `CREATE_FAILED` whichever state it was asked to reach.
            case: "source-queue-never-created",
            held_source: false,
            created_source: false,
            policy: every_call,
            desired_state: Some(STOPPED),
            settles_in: CREATE_FAILED,
            reason_mentions: &["does not exist"],
            names_source: true,
            delivers: false,
        },
        Row {
            // The policies in force on the role say nothing about the source at all, which
            // withholds every call its source stage would make on it.
            case: "role-stating-nothing-about-the-source-read",
            held_source: true,
            created_source: true,
            policy: |_source, target| the_target_only(target),
            desired_state: Some(RUNNING),
            settles_in: CREATE_FAILED,
            reason_mentions: &[RECEIVE_MESSAGE_ON_SQS],
            names_source: false,
            delivers: false,
        },
        Row {
            // Every source call allowed on another queue, which reaches none of them on
            // this one.
            case: "role-reading-another-queue-only",
            held_source: true,
            created_source: true,
            policy: |source, target| {
                json!([
                    {
                        "Effect": "Allow",
                        "Action": [
                            "sqs:ReceiveMessage",
                            "sqs:DeleteMessage",
                            "sqs:GetQueueAttributes",
                        ],
                        "Resource": format!("{source}-elsewhere"),
                    },
                    { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target },
                ])
            },
            desired_state: Some(RUNNING),
            settles_in: CREATE_FAILED,
            reason_mentions: &[RECEIVE_MESSAGE_ON_SQS],
            names_source: false,
            delivers: false,
        },
        Row {
            // Allowed everything and then denied every call the source stage makes on the
            // source: IAM's deny wins, so the pipe could not reach that source at all.
            case: "role-denied-every-source-call",
            held_source: true,
            created_source: true,
            policy: |source, _target| {
                json!([
                    { "Effect": "Allow", "Action": "sqs:*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": [
                            "sqs:ReceiveMessage",
                            "sqs:DeleteMessage",
                            "sqs:GetQueueAttributes",
                        ],
                        "Resource": source,
                    },
                ])
            },
            desired_state: None,
            settles_in: CREATE_FAILED,
            reason_mentions: &[RECEIVE_MESSAGE_ON_SQS],
            names_source: false,
            delivers: false,
        },
        Row {
            // CONTROL: the same wildcard grant with only the RECORD-TAKING call denied. The
            // pipe may still look at its source, so it is brought up — and it moves nothing,
            // because the call it needs to take a record is the one it may not make.
            case: "role-denied-only-the-source-read",
            held_source: true,
            created_source: true,
            policy: |source, _target| {
                json!([
                    { "Effect": "Allow", "Action": "sqs:*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "sqs:ReceiveMessage", "Resource": source },
                ])
            },
            desired_state: None,
            settles_in: RUNNING,
            reason_mentions: &[],
            names_source: false,
            delivers: false,
        },
        Row {
            // CONTROL: the source read allowed under a wildcard action and read with IAM's
            // own wildcards — the pipe comes up and moves its record.
            case: "role-reading-the-source-by-wildcard",
            held_source: true,
            created_source: true,
            policy: |source, target| {
                json!([
                    { "Effect": "Allow", "Action": "sqs:*Message", "Resource": source },
                    { "Effect": "Allow", "Action": "sqs:Send*", "Resource": target },
                ])
            },
            desired_state: None,
            settles_in: RUNNING,
            reason_mentions: &[],
            names_source: false,
            delivers: true,
        },
        Row {
            // CONTROL: the source side is in place and the TARGET side is not, which is a
            // pipe that comes up and fails its deliveries — not one that is never brought
            // up.
            case: "role-reading-the-source-and-not-writing-the-target",
            held_source: true,
            created_source: true,
            policy: |source, _target| {
                json!([{
                    "Effect": "Allow",
                    "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage"],
                    "Resource": source,
                }])
            },
            desired_state: Some(RUNNING),
            settles_in: RUNNING,
            reason_mentions: &[],
            names_source: false,
            delivers: false,
        },
    ];

    let router = emulator();
    // Every row's pipe is created first, so the rows' settle spans overlap instead of being
    // paid one after another.
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for row in &rows {
        let case = row.case;
        let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
        let (source_url, source_arn) = if row.created_source {
            let (url, arn) = given_queue(&router, &format!("{case}-src")).await;
            (Some(url), arn)
        } else {
            (None, unheld_queue_arn(&format!("{case}-src")))
        };
        let role = given_role_stating(
            &router,
            &format!("{case}-role"),
            SERVICE_PRINCIPAL,
            (row.policy)(&source_arn, &target_arn),
        )
        .await;
        // A record on the source before the pipe is created, so "it consumed nothing" is a
        // statement about a source that had something to give.
        let marker = match (row.held_source, source_url.as_deref()) {
            (true, Some(url)) => {
                let marker = format!("{case}-record");
                send(&router, url, std::slice::from_ref(&marker)).await;
                Some(marker)
            }
            _ => None,
        };
        if !row.held_source {
            if let Some(url) = source_url.as_deref() {
                delete_queue(&router, case, url).await;
            }
        }
        let created = create_pipe(
            &router,
            case,
            &source_arn,
            &target_arn,
            &role,
            row.desired_state,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        prepared.push(Prepared {
            row,
            source_arn,
            source_url,
            target_url,
            marker,
        });
    }

    for entry in &prepared {
        let case = entry.row.case;
        let reported = settled(&router, case, case).await;
        let state = reported.string("CurrentState");
        assert_eq!(
            state, entry.row.settles_in,
            "case {case:?}: expected the pipe to settle in {}, it reports {state} reason={:?}",
            entry.row.settles_in,
            reported.member("StateReason")
        );
        let reason = reported.string("StateReason");
        if entry.row.settles_in == CREATE_FAILED {
            reason_says(
                case,
                &reason,
                entry.row.names_source.then_some(entry.source_arn.as_str()),
                entry.row.reason_mentions,
            );
        } else {
            // A move that COULD be completed leaves the user-initiated reason its caller
            // drove it with.
            assert_eq!(
                reason, USER_INITIATED_STATE_REASON,
                "case {case:?}: expected the pipe that was brought up to report the \
                 user-initiated reason, got {reason:?}"
            );
        }
    }

    // What the pipe that was brought up DID: it moved its record.
    for entry in prepared.iter().filter(|entry| entry.row.delivers) {
        let case = entry.row.case;
        let marker = entry.marker.clone().unwrap_or_default();
        let delivered = collect_at_least(&router, &entry.target_url, 1, BUDGET).await;
        assert_eq!(
            delivered,
            vec![marker.clone()],
            "case {case:?}: expected the pipe that was brought up to deliver {marker:?}, got \
             {delivered:?}"
        );
    }

    // And what a pipe that was NEVER brought up did: nothing. The window a "nothing
    // happened" is decided over is WATCHED rather than waited out: a pipe that was going to
    // deliver would have delivered at some point in it, and looking all the way through the
    // window is what makes that independent of how fast the host got there.
    for entry in prepared
        .iter()
        .filter(|entry| entry.row.settles_in == CREATE_FAILED)
    {
        let case = entry.row.case;
        let delivered = collect_during(&router, &entry.target_url, QUIET).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: expected the target of a pipe that never ran to receive nothing \
             over the whole window, it received {delivered:?}"
        );
        publishes_nothing(&router, case, case).await;
        let held = counted(&router, &entry.target_url, &[VISIBLE_ATTRIBUTE]).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected the target of a pipe that never ran to hold nothing, it \
             holds {held}"
        );
        if let (Some(url), Some(marker)) = (entry.source_url.as_deref(), entry.marker.as_deref()) {
            let left = counted(&router, url, &[VISIBLE_ATTRIBUTE]).await;
            assert_eq!(
                left, 1,
                "case {case:?}: expected the record {marker:?} to be left on the source {}, \
                 which holds {left}",
                entry.source_arn
            );
        }
    }
}

/// What takes the pipe's source side away AFTER the pipe has been created healthy.
#[derive(Clone, Copy)]
enum Break {
    /// Its source queue is deleted, so its `Source` names nothing.
    SourceDeleted,
    /// The policies of the role it HOLDS are rewritten to leave it no call on its source at
    /// all.
    SourceReadWithdrawn,
    /// It is re-roled onto another role that may make no call on its source.
    AnotherRole,
}

/// One row of the later-move table: what is taken away, which call is then made, and the
/// terminal failure state that move has.
struct Move {
    case: &'static str,
    broken: Break,
    /// Whether the move is a `StartPipe`; otherwise it is an `UpdatePipe` asking for
    /// `RUNNING`.
    starts: bool,
    settles_in: &'static str,
    /// Text the settled `StateReason` has to carry.
    reason_mentions: &'static [&'static str],
    /// Whether that sentence NAMES the source ([`Row::names_source`]).
    names_source: bool,
}

/// RULE: the same reading holds for every LATER lifecycle call that puts a pipe on its way to
/// running — a start and a reconfiguration each settle in their OWN terminal failure state
/// (`START_FAILED`, `UPDATE_FAILED`) with the `StateReason` saying why, and the pipe runs
/// nothing afterwards.
///
/// The rows differ along which side of the source went away (the resource, the permission of
/// the role in force, the role itself) and which call was in flight when it did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_move_fails_when_the_pipe_can_no_longer_poll_its_source() {
    let moves = [
        Move {
            case: "start-after-the-source-was-deleted",
            broken: Break::SourceDeleted,
            starts: true,
            settles_in: START_FAILED,
            reason_mentions: &["does not exist"],
            names_source: true,
        },
        Move {
            case: "start-after-the-source-read-was-withdrawn",
            broken: Break::SourceReadWithdrawn,
            starts: true,
            settles_in: START_FAILED,
            reason_mentions: &[RECEIVE_MESSAGE_ON_SQS],
            names_source: false,
        },
        Move {
            case: "update-onto-a-role-that-may-not-read-the-source",
            broken: Break::AnotherRole,
            starts: false,
            settles_in: UPDATE_FAILED,
            reason_mentions: &[RECEIVE_MESSAGE_ON_SQS],
            names_source: false,
        },
        Move {
            case: "update-after-the-source-was-deleted",
            broken: Break::SourceDeleted,
            starts: false,
            settles_in: UPDATE_FAILED,
            reason_mentions: &["does not exist"],
            names_source: true,
        },
    ];

    for row in &moves {
        let case = row.case;
        let router = emulator();
        let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
        let role_name = format!("{case}-role");
        let role = given_role_stating(
            &router,
            &role_name,
            SERVICE_PRINCIPAL,
            every_call(&source_arn, &target_arn),
        )
        .await;
        let created = create_pipe(
            &router,
            case,
            &source_arn,
            &target_arn,
            &role,
            Some(STOPPED),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let resting = settled(&router, case, case).await.string("CurrentState");
        assert_eq!(
            resting, STOPPED,
            "case {case:?}: expected the healthy pipe to rest STOPPED before the move, it \
             reports {resting}"
        );
        let marker = format!("{case}-record");
        send(&router, &source_url, std::slice::from_ref(&marker)).await;

        // Take the source side away, then make the move.
        let mut role_for_the_move = role.clone();
        match row.broken {
            Break::SourceDeleted => delete_queue(&router, case, &source_url).await,
            Break::SourceReadWithdrawn => {
                // The same one path `tests/all/pipe_metrics.rs` narrows a live role by.
                withdraw_from_role(&router, case, &role_name, &["sqs:SendMessage"]).await;
            }
            Break::AnotherRole => {
                role_for_the_move = given_role_stating(
                    &router,
                    &format!("{case}-role-b"),
                    SERVICE_PRINCIPAL,
                    the_target_only(&target_arn),
                )
                .await;
            }
        }
        let answered = if row.starts {
            call(
                &router,
                "POST",
                &format!("/v1/pipes/{case}/start"),
                None,
                REGION,
            )
            .await
        } else {
            call(
                &router,
                "PUT",
                &format!("/v1/pipes/{case}"),
                Some(json!({ "RoleArn": role_for_the_move, "DesiredState": RUNNING })),
                REGION,
            )
            .await
        };
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the move answered {} {}",
            answered.status, answered.body
        );

        let reported = settled(&router, case, case).await;
        let state = reported.string("CurrentState");
        assert_eq!(
            state, row.settles_in,
            "case {case:?}: expected the move to settle in {}, it reports {state} reason={:?}",
            row.settles_in,
            reported.member("StateReason")
        );
        reason_says(
            case,
            &reported.string("StateReason"),
            row.names_source.then_some(source_arn.as_str()),
            row.reason_mentions,
        );

        // And the pipe ran nothing: its target receives nothing over the whole window — the
        // invariant is watched through it rather than decided after it — and the record it
        // was sent is still on the source wherever that source is still there.
        let moved = collect_during(&router, &target_url, QUIET).await;
        assert!(
            moved.is_empty(),
            "case {case:?}: expected the target of a pipe that never ran to receive nothing \
             over the whole window, it received {moved:?}"
        );
        publishes_nothing(&router, case, case).await;
        let delivered = counted(&router, &target_url, &[VISIBLE_ATTRIBUTE]).await;
        assert_eq!(
            delivered, 0,
            "case {case:?}: expected the target of a pipe that never ran to hold nothing, it \
             holds {delivered}"
        );
        if !matches!(row.broken, Break::SourceDeleted) {
            let left = counted(&router, &source_url, &[VISIBLE_ATTRIBUTE]).await;
            assert_eq!(
                left, 1,
                "case {case:?}: expected the record {marker:?} to be left on the source \
                 {source_arn}, which holds {left}"
            );
        }
    }
}

/// The services a pipe of ANY source kind reaches, on ONE engine: pipes, the IAM store its
/// execution role lives in, Amazon SQS, and the two STREAM services a pipe takes records
/// from.
///
/// The harness above (`tests/all/pipe_metrics.rs`) mounts the services a QUEUE-sourced pipe's
/// stages reach; the table below spans the source KINDS, because the call a pipe's role is
/// missing is the call for the kind of source it reads, so each kind's source is created the
/// way a client creates one and the pipe is pointed at the ARN that service answered with.
fn emulator_over_every_source() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router
}

/// Which kind of resource a row's pipe reads, which is what decides WHICH call its execution
/// role has to be granted for the pipe to be brought up.
#[derive(Clone, Copy)]
enum SourceKind {
    /// An Amazon SQS queue.
    Queue,
    /// The stream of a DynamoDB table.
    DynamoDbStream,
    /// A Kinesis data stream.
    KinesisStream,
}

/// The row's source: the resource of its kind, created the way a client creates one, and the
/// `SourceParameters` a pipe reading a source of that kind carries.
async fn given_source(
    router: &MultiServiceRouter,
    case: &str,
    kind: SourceKind,
) -> (String, Value) {
    let name = format!("{case}-src");
    match kind {
        SourceKind::Queue => {
            let (_url, arn) = given_queue(router, &name).await;
            (arn, json!({ "SqsQueueParameters": { "BatchSize": 1 } }))
        }
        SourceKind::DynamoDbStream => {
            let arn = given_streamed_table(router, &name, "NEW_IMAGE").await;
            (
                arn,
                json!({ "DynamoDBStreamParameters": { "StartingPosition": "LATEST" } }),
            )
        }
        SourceKind::KinesisStream => {
            let arn = given_stream(router, &name, 1).await;
            (
                arn,
                json!({ "KinesisStreamParameters": { "StartingPosition": "LATEST" } }),
            )
        }
    }
}

/// `CreatePipe` reading a source of any kind: the source parameters the kind needs travel
/// with the ARN, and no target parameters are asked for — no row below delivers anything.
async fn create_pipe_reading(
    router: &MultiServiceRouter,
    name: &str,
    source_arn: &str,
    source_parameters: &Value,
    target_arn: &str,
    role_arn: &str,
    desired_state: &str,
) -> Reply {
    let body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": role_arn,
        "SourceParameters": source_parameters,
        "DesiredState": desired_state,
    });
    call(router, "POST", &format!("/v1/pipes/{name}"), Some(body), REGION).await
}

/// Everything, on everything: the role a row that fails LATER is brought up under, so what
/// the later move takes away is the only thing missing.
fn every_resource() -> Value {
    json!([{ "Effect": "Allow", "Action": "*", "Resource": "*" }])
}

/// The sentence a pipe reports as its `StateReason` when the policies in force on its
/// execution role do not let it make the call that takes its source's records: the fixed text
/// of this failure class, with the API action and the service that call is made to as its
/// only variable parts.
///
/// Written out here rather than read from the crate, so a row asserts the TEXT a client reads
/// and not merely that it equals whatever the crate happens to build.
fn withheld_source_read_reason(api_action: &str, service: &str) -> String {
    format!(
        "Input parameter is invalid from the request due to : The provided customer role does \
         not have permissions to call {api_action} on {service}"
    )
}

/// When the row's execution role loses the source read, which decides which lifecycle call is
/// the move that cannot be completed.
#[derive(Clone, Copy)]
enum Withheld {
    /// The role never had it: the CREATION is the move that fails.
    Creation,
    /// The pipe was created healthy and is re-roled onto a role that allows everything and
    /// DENIES the source read: the reconfiguration is the move that fails.
    Reconfiguration,
    /// The pipe was created healthy and STOPPED, and the source read is then withdrawn from
    /// the live role it holds: the start is the move that fails.
    Start,
}

/// One row of the sentence table: which kind of source the pipe reads, when its role loses
/// the call that takes that source's records, and what the pipe then reports.
struct Sentence {
    case: &'static str,
    source: SourceKind,
    withheld: Withheld,
    /// The IAM action that call is authorized under, which is what a row that DENIES it
    /// names in its policy.
    iam_action: &'static str,
    /// The IAM actions of the OTHER calls the same source stage makes on that source, which a
    /// row that denies the call above denies beside it: a role left any one of them is a role
    /// under which the pipe still comes up ([`crate::pipe_bring_up`]'s create table has that
    /// control), so a row about the FAILURE's own sentence withholds the whole set.
    other_actions: &'static [&'static str],
    /// The name of the API action, and the service it is called on, that the pipe has to
    /// report as the call it does not have permission to make.
    api_action: &'static str,
    service: &'static str,
    /// The terminal failure state of the move that was in flight.
    settles_in: &'static str,
}

/// The other calls each kind of source's own stage makes on the source, beside the one that
/// takes its records. Source: the calls the pipes data plane makes on each source kind — a
/// queue's delete and attributes read, a stream's shard listing and iterator.
const QUEUE_OTHER_CALLS: &[&str] = &["sqs:DeleteMessage", "sqs:GetQueueAttributes"];
const DYNAMODB_STREAM_OTHER_CALLS: &[&str] =
    &["dynamodb:GetShardIterator", "dynamodb:DescribeStream"];
const KINESIS_STREAM_OTHER_CALLS: &[&str] = &["kinesis:GetShardIterator", "kinesis:ListShards"];

/// RULE: a pipe whose execution role is granted NONE of the calls its source stage makes on
/// its source cannot be brought up, and what it reports as the reason is the call that TAKES
/// THE RECORDS and nothing else — the name of the API action and the service it is made to, for
/// the kind of resource the pipe's `Source` names — in the fixed wording of that failure class
/// ([`withheld_source_read_reason`]), whichever lifecycle call was in flight and however the
/// grants came to be missing.
///
/// The rows differ along what the rule generalises over: the KIND of source (an Amazon SQS
/// queue, a DynamoDB stream, a Kinesis data stream), which decides the call that is named;
/// WHICH lifecycle call was in flight (a creation, a reconfiguration, a start), which decides
/// the terminal state; and HOW the grants are missing (policies stating nothing about the
/// source, an explicit deny over a wildcard allow, grants withdrawn from a live role), which
/// changes nothing about the sentence.
///
/// The two tables above assert where such a move LANDS and that the pipe then runs nothing;
/// this one asserts the exact text, which is what a client reading a deployment's failure is
/// shown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reason_a_withheld_source_read_leaves_names_that_call() {
    let rows = [
        Sentence {
            case: "withheld-queue-at-create",
            source: SourceKind::Queue,
            withheld: Withheld::Creation,
            iam_action: "sqs:ReceiveMessage",
            other_actions: QUEUE_OTHER_CALLS,
            api_action: "ReceiveMessage",
            service: "SQS",
            settles_in: CREATE_FAILED,
        },
        Sentence {
            case: "withheld-queue-at-update",
            source: SourceKind::Queue,
            withheld: Withheld::Reconfiguration,
            iam_action: "sqs:ReceiveMessage",
            other_actions: QUEUE_OTHER_CALLS,
            api_action: "ReceiveMessage",
            service: "SQS",
            settles_in: UPDATE_FAILED,
        },
        Sentence {
            case: "withheld-queue-at-start",
            source: SourceKind::Queue,
            withheld: Withheld::Start,
            iam_action: "sqs:ReceiveMessage",
            other_actions: QUEUE_OTHER_CALLS,
            api_action: "ReceiveMessage",
            service: "SQS",
            settles_in: START_FAILED,
        },
        Sentence {
            case: "withheld-dynamodb-stream-at-create",
            source: SourceKind::DynamoDbStream,
            withheld: Withheld::Creation,
            iam_action: "dynamodb:GetRecords",
            other_actions: DYNAMODB_STREAM_OTHER_CALLS,
            api_action: "GetRecords",
            service: "DynamoDB",
            settles_in: CREATE_FAILED,
        },
        Sentence {
            case: "withheld-dynamodb-stream-at-update",
            source: SourceKind::DynamoDbStream,
            withheld: Withheld::Reconfiguration,
            iam_action: "dynamodb:GetRecords",
            other_actions: DYNAMODB_STREAM_OTHER_CALLS,
            api_action: "GetRecords",
            service: "DynamoDB",
            settles_in: UPDATE_FAILED,
        },
        Sentence {
            case: "withheld-kinesis-stream-at-create",
            source: SourceKind::KinesisStream,
            withheld: Withheld::Creation,
            iam_action: "kinesis:GetRecords",
            other_actions: KINESIS_STREAM_OTHER_CALLS,
            api_action: "GetRecords",
            service: "Kinesis",
            settles_in: CREATE_FAILED,
        },
    ];

    // A CONSTANTS-IN-SYNC guard, not a check on the emulator: the two tables above look for a
    // FRAGMENT of this failure's sentence, and this is what keeps that fragment the tail of the
    // whole sentence the rows below assert, so editing one of the two constants and not the
    // other fails here rather than quietly leaving those tables looking for text no pipe
    // reports.
    assert!(
        withheld_source_read_reason("ReceiveMessage", "SQS").ends_with(RECEIVE_MESSAGE_ON_SQS),
        "expected the queue rows' sentence to end with the fragment the tables above look \
         for, {RECEIVE_MESSAGE_ON_SQS:?}, got {:?}",
        withheld_source_read_reason("ReceiveMessage", "SQS")
    );

    for row in &rows {
        let case = row.case;
        let router = emulator_over_every_source();
        let (_target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
        let (source_arn, source_parameters) = given_source(&router, case, row.source).await;
        let role_name = format!("{case}-role");
        // A row whose creation is the failing move is created under a role that may write the
        // target and nothing else; a row that fails LATER is brought up healthy first.
        let created_under = match row.withheld {
            Withheld::Creation => the_target_only(&target_arn),
            Withheld::Reconfiguration | Withheld::Start => every_resource(),
        };
        let role = given_role_stating(&router, &role_name, SERVICE_PRINCIPAL, created_under).await;
        let created_desired = match row.withheld {
            Withheld::Start => STOPPED,
            _ => RUNNING,
        };
        let created = create_pipe_reading(
            &router,
            case,
            &source_arn,
            &source_parameters,
            &target_arn,
            &role,
            created_desired,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let resting = settled(&router, case, case).await;

        // The healthy pipe really came up, so what the later move reports is that move's own
        // failure and not a pipe that never ran.
        match row.withheld {
            Withheld::Creation => {}
            Withheld::Reconfiguration => assert_eq!(
                resting.string("CurrentState"),
                RUNNING,
                "case {case:?}: expected the pipe to come up before the move, it reports {} \
                 reason={:?}",
                resting.string("CurrentState"),
                resting.member("StateReason")
            ),
            Withheld::Start => assert_eq!(
                resting.string("CurrentState"),
                STOPPED,
                "case {case:?}: expected the healthy pipe to rest {STOPPED} before the move, \
                 it reports {} reason={:?}",
                resting.string("CurrentState"),
                resting.member("StateReason")
            ),
        }

        // Take the source read away and make the move it is missing from.
        match row.withheld {
            Withheld::Creation => {}
            Withheld::Reconfiguration => {
                // Every call that stage makes on the source, denied over a wildcard allow: a
                // role left ANY of them is a role the pipe still comes up under, so the move
                // this row is about withholds the whole set.
                let mut denied_actions = vec![row.iam_action];
                denied_actions.extend_from_slice(row.other_actions);
                let denied = given_role_stating(
                    &router,
                    &format!("{case}-role-b"),
                    SERVICE_PRINCIPAL,
                    json!([
                        { "Effect": "Allow", "Action": "*", "Resource": "*" },
                        {
                            "Effect": "Deny",
                            "Action": denied_actions,
                            "Resource": source_arn.clone(),
                        },
                    ]),
                )
                .await;
                let answered = call(
                    &router,
                    "PUT",
                    &format!("/v1/pipes/{case}"),
                    Some(json!({ "RoleArn": denied, "DesiredState": RUNNING })),
                    REGION,
                )
                .await;
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: UpdatePipe answered {} {}",
                    answered.status, answered.body
                );
            }
            Withheld::Start => {
                withdraw_from_role(&router, case, &role_name, &["sqs:SendMessage"]).await;
                let answered = call(
                    &router,
                    "POST",
                    &format!("/v1/pipes/{case}/start"),
                    None,
                    REGION,
                )
                .await;
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: StartPipe answered {} {}",
                    answered.status, answered.body
                );
            }
        }

        let reported = settled(&router, case, case).await;
        let state = reported.string("CurrentState");
        assert_eq!(
            state, row.settles_in,
            "case {case:?}: expected the move to settle in {}, it reports {state} reason={:?}",
            row.settles_in,
            reported.member("StateReason")
        );
        let expected = withheld_source_read_reason(row.api_action, row.service);
        assert_eq!(
            reported.string("StateReason"),
            expected,
            "case {case:?}: expected the StateReason {expected:?}, got {:?}",
            reported.string("StateReason")
        );
    }
}
