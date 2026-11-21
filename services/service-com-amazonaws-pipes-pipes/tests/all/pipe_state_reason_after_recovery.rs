// SPDX-License-Identifier: CC0-1.0

//! What a pipe reports as its `StateReason` once a LATER lifecycle call has moved it out of
//! a state an earlier move failed into — end to end over the wire, with IAM, Amazon SQS and
//! CloudWatch mounted beside pipes on one engine (the harness of `tests/all/pipe_metrics.rs`
//! and `tests/all/pipe_bring_up.rs`, reused rather than copied).
//!
//! RULE: a pipe's `StateReason` says how it came to be in the state it is in NOW, so a state
//! a lifecycle call drove it into is reported with the fixed user-initiated token by every
//! read that carries the member — whatever state the pipe was in before, including one an
//! earlier move failed into, whose sentence the new state's reason replaces.
//!
//! `tests/all/pipe_state_reason.rs` is the table over the lifecycle calls that drive a
//! HEALTHY pipe, `tests/all/pipe_bring_up.rs` the table over the moves that cannot be
//! completed, and `tests/all/pipe_listing_state_reason.rs` the one over pipes with differing
//! reasons in one listing. What is asserted here and in none of them is the pipe whose reason
//! has to CHANGE: a failed move's sentence must not outlive the state it described, and the
//! control row — a pipe left where its creation failed — fixes the other edge of the same
//! rule, so the user-initiated token is not simply what every pipe reports.

use serde_json::json;
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use service_com_amazonaws_pipes_pipes::state::pipe_state::{
    CREATE_FAILED, RUNNING, STARTING, START_FAILED, STOPPED, UPDATING,
};
use service_com_amazonaws_pipes_pipes::state::TRANSIENT_PIPE_STATES;
use service_com_amazonaws_pipes_pipes::state::USER_INITIATED_STATE_REASON;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_bring_up::{create_pipe, delete_queue, every_call, settled};
use crate::pipe_data_plane::given_queue;
use crate::pipe_lifecycle::{call, describe, list, Reply, REGION};
use crate::pipe_listing_state_reason::{
    assert_reason, paged_walk, reason_for, Reason, MIN_LIMIT,
};
use crate::pipe_metrics::{emulator, given_role_stating, withdraw_from_role};
use crate::pipe_state_reason::listed_reasons;

/// Every Amazon SQS call a pipe with a queue source and a queue target makes, as the
/// identity policy of a LIVE role names them — the set a row gives back to a role it took
/// the source read away from. Source: the statements of
/// `tests/all/pipe_bring_up.rs::every_call`, which is the create-time form of the same set.
const EVERY_PIPE_CALL: &[&str] = &[
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
    "sqs:SendMessage",
];

/// How the row's pipe comes to rest in a state a move FAILED into.
#[derive(Clone, Copy)]
enum Failed {
    /// Its creation could not be completed, because the role it acts as may not take the
    /// source's records.
    CreateWithoutSourceRead,
    /// Its creation could not be completed, because the queue its `Source` names is gone.
    CreateWithSourceGone,
    /// It was created healthy and STOPPED, and the start that followed could not be
    /// completed, because the source read was withdrawn from the role it holds.
    StartWithoutSourceRead,
}

/// The lifecycle call the row then drives the pipe out of that state with — the calls that
/// put a pipe back on its way to the state its caller wants (`StartPipe`, `UpdatePipe`),
/// each made once the pipe can poll its source again.
#[derive(Clone, Copy)]
enum Recovery {
    /// `StartPipe`.
    Start,
    /// `UpdatePipe` asking for this `DesiredState`.
    UpdateTo(&'static str),
    /// Nothing: the control row, left resting where its move failed.
    None,
}

/// One row: how the pipe failed, which call drove it out, and where that left it.
struct Row {
    /// Also the pipe's name and the stem of its queues and role, so every row is
    /// independent.
    case: &'static str,
    failed: Failed,
    /// The state the failed move settles in, before the recovery call.
    failed_state: &'static str,
    /// What the failed move's `StateReason` has to carry.
    failure_mentions: &'static [&'static str],
    recovery: Recovery,
    /// The transient state the recovery call reports, or `None` for the control row.
    transient: Option<&'static str>,
    /// The state the pipe rests in once the recovery call has landed.
    rests_in: &'static str,
    /// The `DesiredState` the pipe reports once the recovery call has landed.
    desired_state: &'static str,
}

/// Which of the fixture's pipes one listing is expected to return.
type Matches = Box<dyn for<'row> Fn(&Listed<'row>) -> bool>;

/// What one row left behind, for the listings every row's pipe is then read through
/// together.
struct Listed<'row> {
    row: &'row Row,
    source_arn: String,
    /// The reason the pipe has to report now: the user-initiated token for a row whose
    /// recovery call drove its state, the failed move's own sentence for the control row.
    reason: Reason,
}

/// The pipe names a listing returned, in the order it returned them.
fn names_of_entries(entries: &[(String, Option<String>)]) -> Vec<&str> {
    entries.iter().map(|(name, _)| name.as_str()).collect()
}

/// Leave the LIVE role `name` allowing every call a pipe makes again: the grant a row took
/// away, put back through the one path that replaces a role's inline policy. Named for what
/// the call does here, because that path is reached through the helper that narrows a role.
async fn restore_every_call(router: &MultiServiceRouter, case: &str, name: &str) {
    withdraw_from_role(router, case, name, EVERY_PIPE_CALL).await;
}

/// The recovery call of one row, or `None` for the control row that makes none.
async fn recover(
    router: &MultiServiceRouter,
    case: &str,
    recovery: Recovery,
    role_arn: &str,
) -> Option<Reply> {
    match recovery {
        Recovery::Start => Some(
            call(
                router,
                "POST",
                &format!("/v1/pipes/{case}/start"),
                None,
                REGION,
            )
            .await,
        ),
        Recovery::UpdateTo(desired_state) => Some(
            call(
                router,
                "PUT",
                &format!("/v1/pipes/{case}"),
                Some(json!({ "RoleArn": role_arn, "DesiredState": desired_state })),
                REGION,
            )
            .await,
        ),
        Recovery::None => None,
    }
}

/// RULE: the `StateReason` of a pipe describes the state it is in NOW — so the state a
/// later lifecycle call drove it into is reported with the user-initiated token, replacing
/// the sentence of the move that failed before it, and a pipe left resting where its move
/// failed keeps that sentence; both are read back the same way through `DescribePipe` and
/// through every shape of `ListPipes`.
///
/// The rows differ along what the rule generalises over: which move failed (a creation, a
/// start), why it failed (the role may not take the source's records, the source queue is
/// gone), the failure sentence it therefore left, which call drove the pipe out of it (a
/// start, a reconfiguration), and the state that call left the pipe resting in (`RUNNING`,
/// `STOPPED`) — plus the control row that makes no later call at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_lifecycle_call_replaces_the_reason_the_failed_move_left() {
    // The token's own TEXT, pinned once against what a pipe resting in the state its caller
    // asked for reports, so the rows below test the reason a read carries and not merely that
    // it equals whatever this crate's constant happens to say.
    assert_eq!(
        USER_INITIATED_STATE_REASON, "USER_INITIATED",
        "expected the user-initiated reason token to be the one a caller-driven state is \
         reported with, got {USER_INITIATED_STATE_REASON:?}"
    );
    let rows = [
        Row {
            case: "reason-restarted",
            failed: Failed::StartWithoutSourceRead,
            failed_state: START_FAILED,
            failure_mentions: &["ReceiveMessage on SQS"],
            recovery: Recovery::Start,
            transient: Some(STARTING),
            rests_in: RUNNING,
            desired_state: RUNNING,
        },
        Row {
            case: "reason-stopped-after-start",
            failed: Failed::StartWithoutSourceRead,
            failed_state: START_FAILED,
            failure_mentions: &["ReceiveMessage on SQS"],
            recovery: Recovery::UpdateTo(STOPPED),
            transient: Some(UPDATING),
            rests_in: STOPPED,
            desired_state: STOPPED,
        },
        Row {
            case: "reason-updated",
            failed: Failed::CreateWithoutSourceRead,
            failed_state: CREATE_FAILED,
            failure_mentions: &["ReceiveMessage on SQS"],
            recovery: Recovery::UpdateTo(RUNNING),
            transient: Some(UPDATING),
            rests_in: RUNNING,
            desired_state: RUNNING,
        },
        Row {
            // The failure the SOURCE RESOURCE caused rather than the role, recovered once
            // the queue the pipe polls is there again.
            case: "reason-restarted-after-create",
            failed: Failed::CreateWithSourceGone,
            failed_state: CREATE_FAILED,
            failure_mentions: &["does not exist"],
            recovery: Recovery::Start,
            transient: Some(STARTING),
            rests_in: RUNNING,
            desired_state: RUNNING,
        },
        Row {
            // CONTROL: nothing drove this pipe out of the state its creation failed in, so
            // the sentence saying why is still the reason it is in that state.
            case: "reason-left-failed",
            failed: Failed::CreateWithoutSourceRead,
            failed_state: CREATE_FAILED,
            failure_mentions: &["ReceiveMessage on SQS"],
            recovery: Recovery::None,
            transient: None,
            rests_in: CREATE_FAILED,
            desired_state: RUNNING,
        },
    ];

    let router = emulator();
    // One target queue for the whole fixture, so a listing filtered by target prefix spans
    // every row while each row keeps its own source.
    let (_target_url, target_arn) = given_queue(&router, "reason-recovery-target").await;

    let mut fixture: Vec<Listed<'_>> = Vec::new();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
        let role_name = format!("{case}-role");
        let role_arn = given_role_stating(
            &router,
            &role_name,
            SERVICE_PRINCIPAL,
            every_call(&source_arn, &target_arn),
        )
        .await;

        // What the row takes away BEFORE its creation, for a row whose creation is the move
        // that fails.
        match row.failed {
            Failed::CreateWithoutSourceRead => {
                withdraw_from_role(&router, case, &role_name, &["sqs:SendMessage"]).await;
            }
            Failed::CreateWithSourceGone => delete_queue(&router, case, &source_url).await,
            Failed::StartWithoutSourceRead => {}
        }

        // The creation itself: a row whose START is the failing move is created healthy and
        // STOPPED, so the start is what puts it on its way to running.
        let created_desired = match row.failed {
            Failed::StartWithoutSourceRead => STOPPED,
            _ => RUNNING,
        };
        let created = create_pipe(
            &router,
            case,
            &source_arn,
            &target_arn,
            &role_arn,
            Some(created_desired),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let resting = settled(&router, case, case).await;

        // For a row whose start is the failing move: the healthy pipe rests STOPPED, the
        // source read is withdrawn from the role it holds, and the start that follows is
        // the move that cannot be completed.
        if matches!(row.failed, Failed::StartWithoutSourceRead) {
            let state = resting.string("CurrentState");
            assert_eq!(
                state, STOPPED,
                "case {case:?}: expected the healthy pipe to rest {STOPPED} before the \
                 failing start, it reports {state}"
            );
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

        // The state the failed move left the pipe in, and the sentence saying why: what the
        // recovery call then has to replace.
        let failed = settled(&router, case, case).await;
        let state = failed.string("CurrentState");
        assert_eq!(
            state, row.failed_state,
            "case {case:?}: expected the move that could not be completed to settle in {}, \
             it reports {state} reason={:?}",
            row.failed_state,
            failed.member("StateReason")
        );
        let failure_reason = Reason::Failure {
            // Only the failure the SOURCE RESOURCE caused names the source: the sentence a
            // role without the source read leaves names the CALL it may not make.
            source_arn: matches!(row.failed, Failed::CreateWithSourceGone)
                .then(|| source_arn.clone()),
            mentions: row.failure_mentions,
        };
        assert_reason(
            case,
            "DescribePipe before the recovery call",
            failed.member("StateReason").as_str(),
            &failure_reason,
        );

        // The source side the row took away is put back, so the recovery call is one the
        // pipe can complete: the withdrawn call returns to the live role through the one path
        // that replaces its inline policy, and a deleted source queue is created again under
        // the name its ARN carries. The control row gets neither.
        if !matches!(row.recovery, Recovery::None) {
            match row.failed {
                Failed::CreateWithSourceGone => {
                    let (_url, restored) = given_queue(&router, &format!("{case}-src")).await;
                    assert_eq!(
                        restored, source_arn,
                        "case {case:?}: expected the re-created source queue to be the one the \
                         pipe names, got {restored}"
                    );
                }
                Failed::CreateWithoutSourceRead | Failed::StartWithoutSourceRead => {
                    restore_every_call(&router, case, &role_name).await;
                }
            }
        }
        let expected_reason = match recover(&router, case, row.recovery, &role_arn).await {
            None => failure_reason,
            Some(answered) => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: the recovery call answered {} {}",
                    answered.status, answered.body
                );
                let reported = answered.string("CurrentState");
                let transient = row
                    .transient
                    .expect("a recovery row names the transient state its call reports");
                assert_eq!(
                    reported, transient,
                    "case {case:?}: expected the recovery call to report the pipe \
                     {transient}, it reports {reported}"
                );

                // The reason WHILE the move is in flight: the pipe is in a state its caller
                // asked for, so the failed move's sentence is already gone. Asserted under
                // the condition that the move has not settled yet rather than on any
                // assumption about how fast this read arrives; the settled read below is
                // what every row asserts unconditionally.
                let in_flight = describe(&router, case).await;
                assert_eq!(
                    in_flight.status, 200,
                    "case {case:?}: DescribePipe answered {} {}",
                    in_flight.status, in_flight.body
                );
                if TRANSIENT_PIPE_STATES.contains(&in_flight.string("CurrentState").as_str()) {
                    assert_reason(
                        case,
                        "DescribePipe while the recovery move is in flight",
                        in_flight.member("StateReason").as_str(),
                        &Reason::UserInitiated,
                    );
                }
                Reason::UserInitiated
            }
        };

        // And where the pipe came to rest, with the reason for THAT state.
        let rested = settled(&router, case, case).await;
        let state = rested.string("CurrentState");
        assert_eq!(
            state, row.rests_in,
            "case {case:?}: expected the pipe to rest in {}, it reports {state} reason={:?}",
            row.rests_in,
            rested.member("StateReason")
        );
        let desired = rested.string("DesiredState");
        assert_eq!(
            desired, row.desired_state,
            "case {case:?}: expected the pipe to report DesiredState {}, it reports \
             {desired}",
            row.desired_state
        );
        assert_reason(
            case,
            "DescribePipe once the recovery move has landed",
            rested.member("StateReason").as_str(),
            &expected_reason,
        );

        fixture.push(Listed {
            row,
            source_arn,
            reason: expected_reason,
        });
    }

    // Every listing shape the operation models, over the whole fixture: each entry carries
    // the reason for the state that entry reports, which is the reason a read of that one
    // pipe reported above.
    let every_case = |_entry: &Listed<'_>| true;
    let listings: Vec<(&str, String, Matches)> = vec![
        ("unpaged", String::new(), Box::new(every_case)),
        (
            "name-prefix-matching-two",
            "?NamePrefix=reason-restarted".to_string(),
            Box::new(|entry: &Listed<'_>| entry.row.case.starts_with("reason-restarted")),
        ),
        (
            "current-state-running",
            format!("?CurrentState={RUNNING}"),
            Box::new(|entry: &Listed<'_>| entry.row.rests_in == RUNNING),
        ),
        (
            "current-state-create-failed",
            format!("?CurrentState={CREATE_FAILED}"),
            Box::new(|entry: &Listed<'_>| entry.row.rests_in == CREATE_FAILED),
        ),
        (
            "desired-state-stopped",
            format!("?DesiredState={STOPPED}"),
            Box::new(|entry: &Listed<'_>| entry.row.desired_state == STOPPED),
        ),
        (
            "target-prefix-every-pipe",
            format!("?TargetPrefix={target_arn}"),
            Box::new(every_case),
        ),
        (
            "target-prefix-matching-none",
            format!("?TargetPrefix={target_arn}-elsewhere"),
            Box::new(|_entry: &Listed<'_>| false),
        ),
    ];
    for (listing, query, matches) in listings {
        assert_listing(&router, listing, &query, &fixture, &matches).await;
    }

    // A source prefix names ONE pipe's queue, so it selects that pipe alone; the row the
    // issue cited is one of these, and so is every other.
    for entry in &fixture {
        let case = entry.row.case;
        let query = format!("?SourcePrefix={}", entry.source_arn);
        assert_listing(&router, case, &query, &fixture, &|listed: &Listed<'_>| {
            listed.row.case == case
        })
        .await;
    }

    // And a paged walk at the smallest limit: the member is on the entry of EVERY page, and
    // the pages together are the whole fixture in name order.
    let walked = paged_walk(&router, "paged-walk", MIN_LIMIT).await;
    assert_eq!(
        names_of_entries(&walked),
        expected_names(&fixture, &every_case),
        "case \"paged-walk\": expected the pages to enumerate every pipe in name order, got \
         {:?}",
        names_of_entries(&walked)
    );
    for entry in &fixture {
        assert_reason(
            entry.row.case,
            "the paged walk",
            reason_for(&walked, entry.row.case),
            &entry.reason,
        );
    }
}

/// The names the fixture's matching pipes have, in the order a listing returns them: name
/// order, which is the order `ListPipes` enumerates in.
fn expected_names(
    fixture: &[Listed<'_>],
    matches: &dyn Fn(&Listed<'_>) -> bool,
) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = fixture
        .iter()
        .filter(|entry| matches(entry))
        .map(|entry| entry.row.case)
        .collect();
    names.sort_unstable();
    names
}

/// Assert one listing returned exactly the fixture's matching pipes, each carrying the
/// reason for the state it is in.
async fn assert_listing(
    router: &MultiServiceRouter,
    listing: &str,
    query: &str,
    fixture: &[Listed<'_>],
    matches: &dyn Fn(&Listed<'_>) -> bool,
) {
    let listed = list(router, query).await;
    assert_eq!(
        listed.status, 200,
        "case {listing:?}: ListPipes answered {} {}",
        listed.status, listed.body
    );
    let entries = listed_reasons(&listed);
    assert_eq!(
        names_of_entries(&entries),
        expected_names(fixture, matches),
        "case {listing:?}: expected the listing to return {:?}, got {:?}",
        expected_names(fixture, matches),
        names_of_entries(&entries)
    );
    for entry in fixture.iter().filter(|entry| matches(entry)) {
        assert_reason(
            entry.row.case,
            &format!("the {listing:?} listing"),
            reason_for(&entries, entry.row.case),
            &entry.reason,
        );
    }
}
