// SPDX-License-Identifier: CC0-1.0

//! What a LISTING says about WHY each pipe is in the state it reports, end to end over the
//! wire, with IAM and Amazon SQS mounted beside pipes on one engine (the harness of
//! `tests/all/pipe_metrics.rs` and `tests/all/pipe_bring_up.rs`, reused rather than copied).
//!
//! RULE: a pipe's `StateReason` says how it came to be in the state it is in, and every read
//! that carries the member reports the pipe's OWN reason — so each entry of a listing carries
//! the same reason a read of that one pipe reports: the user-initiated token for a state its
//! caller drove it into, the sentence saying why for a move that could not be completed, and
//! its poller's reading for a pipe running over a source read per shard.
//!
//! `tests/all/pipe_state_reason.rs` is the table over the lifecycle calls and listing shapes
//! of pipes that all rest where their caller asked them to, and `tests/all/pipe_bring_up.rs`
//! is the table over the moves that could not be completed, read one pipe at a time. What is
//! asserted here and in neither of them is, first, the pipes with DIFFERING reasons in ONE
//! listing: that each entry carries its OWN reason, which is what tells a caller-stopped pipe
//! from a failed one without describing every listed pipe in turn. And second, what SELECTED
//! an entry: the member is on the entries of every modelled query — each ARN-prefix filter,
//! two filters together, each bound of the page size — over pipes whose source and target are
//! of differing KINDS, and the reason an entry carries is the very one a read of that one pipe
//! reports.

use serde_json::json;
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use service_com_amazonaws_pipes_pipes::state::pipe_state::{
    CREATE_FAILED, RUNNING, START_FAILED, STOPPED,
};
use service_com_amazonaws_pipes_pipes::state::{
    NO_RECORDS_PROCESSED_STATE_REASON, USER_INITIATED_STATE_REASON,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_bring_up::{create_pipe, delete_queue, every_call, reason_says, settled};
use crate::pipe_data_plane::given_queue;
use crate::pipe_lifecycle::{
    call, create, list, minimal_body, required_source_parameters, router, REGION,
};
use crate::pipe_metrics::{emulator, given_role_stating, withdraw_from_role};
use crate::pipe_state_reason::{drive, listed_reasons};

/// How many pages a paged walk reads before it gives up, so a listing that never stops
/// handing out tokens fails the case instead of looping for ever. Well above the number of
/// pipes any fixture of these suites holds.
const PAGE_BUDGET: usize = 16;

/// What a pipe's `StateReason` has to say, as the state it rests in was reached.
///
/// `pub(crate)` with [`assert_reason`] because
/// `tests/all/pipe_state_reason_after_recovery.rs` asks the same question of a pipe whose
/// reason had to CHANGE, through this one reading of what a reason has to say.
pub(crate) enum Reason {
    /// The pipe rests where its caller asked it to: the fixed user-initiated token.
    UserInitiated,
    /// The pipe rests in `RUNNING` over a source read PER SHARD whose records its poller
    /// has processed none of: the poller's own reading rather than the move that put the
    /// pipe there.
    PollerIdle,
    /// A move that could not be completed: the sentence names the source the pipe could not
    /// poll and carries `mentions` beside it, saying what stopped it.
    Failure {
        source_arn: String,
        mentions: &'static [&'static str],
    },
}

/// One pipe of the fixture: its name, the state it comes to rest in, and the reason it
/// therefore reports.
struct Listed {
    case: &'static str,
    state: &'static str,
    reason: Reason,
}

/// Assert the `StateReason` `read` reported for the pipe `case` is the one the state it
/// rests in was reached by.
pub(crate) fn assert_reason(case: &str, read: &str, reported: Option<&str>, expected: &Reason) {
    let Some(reason) = reported else {
        panic!("case {case:?}: {read} returned the pipe with no StateReason at all");
    };
    match expected {
        Reason::UserInitiated => assert_eq!(
            reason, USER_INITIATED_STATE_REASON,
            "case {case:?}: expected {read} to report the user-initiated reason \
             {USER_INITIATED_STATE_REASON} for a state the caller drove, got {reason:?}"
        ),
        Reason::PollerIdle => assert_eq!(
            reason, NO_RECORDS_PROCESSED_STATE_REASON,
            "case {case:?}: expected {read} to report the poller's reading \
             {NO_RECORDS_PROCESSED_STATE_REASON} for a pipe running over a stream whose \
             records it has processed none of, got {reason:?}"
        ),
        Reason::Failure {
            source_arn,
            mentions,
        } => {
            // The failure sentence's own reading — it names the source, says what stopped
            // the move and stays within the member's bound — is `tests/all/pipe_bring_up.rs`
            // ONE reading of it, so this suite asserts no second version of it.
            reason_says(
                &format!("{case} as {read} reports it"),
                reason,
                source_arn,
                mentions,
            );
            assert_ne!(
                reason, USER_INITIATED_STATE_REASON,
                "case {case:?}: a move that could not be completed must not be reported as \
                 user-initiated, {read} reported {reason:?}"
            );
        }
    }
}

/// The `StateReason` the entries of one listing reported for the pipe `case`, or `None` when
/// the entry carried none. Which pipes a listing returned at all is asserted separately, off
/// the same entries, so `None` here is the member missing and not the pipe missing.
///
/// `pub(crate)` with [`assert_reason`], because
/// `tests/all/pipe_state_reason_after_recovery.rs` reads the entry of a listed pipe the same
/// way.
pub(crate) fn reason_for<'entries>(
    entries: &'entries [(String, Option<String>)],
    case: &str,
) -> Option<&'entries str> {
    entries
        .iter()
        .find(|(name, _)| name == case)
        .and_then(|(_, reason)| reason.as_deref())
}

/// A role called `{case}-role` allowing every call a pipe over `source_arn`/`target_arn`
/// makes; answers the ARN IAM persisted. One role per pipe, so narrowing one leaves the
/// others' grants where they were.
async fn role_for(
    router: &MultiServiceRouter,
    case: &str,
    source_arn: &str,
    target_arn: &str,
) -> String {
    given_role_stating(
        router,
        &format!("{case}-role"),
        SERVICE_PRINCIPAL,
        every_call(source_arn, target_arn),
    )
    .await
}

/// RULE: every entry of a `ListPipes` page carries the pipe's own `StateReason` — the same
/// value a read of that one pipe reports — whatever listing shape returned it and however
/// the pipe came to be in the state it rests in.
///
/// The rows differ along what the rule generalises over: how the pipe reached its state (a
/// creation into the state it asked for, a later stop, a creation that could not be
/// completed, a start that could not be completed), the state it rests in (`RUNNING`,
/// `STOPPED`, `CREATE_FAILED`, `START_FAILED`), the reason it therefore reports (the
/// user-initiated token, two different failure sentences), and the shape of the read
/// (unpaged, each modelled filter, and every page of a paged walk).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_listing_reports_the_reason_each_listed_pipe_reached_its_state_by() {
    let router = emulator();
    let (_target_url, target_arn) = given_queue(&router, "reason-target").await;

    // A pipe resting in the state its CREATION asked for.
    let running = "reason-running";
    let (_url, running_source) = given_queue(&router, "reason-running-src").await;
    let running_role = role_for(&router, running, &running_source, &target_arn).await;
    let created = create_pipe(
        &router,
        running,
        &running_source,
        &target_arn,
        &running_role,
        Some(RUNNING),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {running:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    // A pipe a later STOP drove out of the state its creation asked for.
    let stopped = "reason-stopped";
    let (_url, stopped_source) = given_queue(&router, "reason-stopped-src").await;
    let stopped_role = role_for(&router, stopped, &stopped_source, &target_arn).await;
    let created = create_pipe(
        &router,
        stopped,
        &stopped_source,
        &target_arn,
        &stopped_role,
        Some(RUNNING),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {stopped:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    // A pipe whose CREATION could not be completed: its source queue is gone before the
    // pipe names it, so there is nothing for the pipe to poll.
    let create_failed = "reason-create-failed";
    let (gone_url, gone_source) = given_queue(&router, "reason-create-failed-src").await;
    let create_failed_role = role_for(&router, create_failed, &gone_source, &target_arn).await;
    delete_queue(&router, create_failed, &gone_url).await;
    let created = create_pipe(
        &router,
        create_failed,
        &gone_source,
        &target_arn,
        &create_failed_role,
        Some(RUNNING),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {create_failed:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    // A pipe created healthy and STOPPED, whose START then could not be completed because
    // the role it acts as lost the call that takes its source's records.
    let start_failed = "reason-start-failed";
    let (_url, start_failed_source) = given_queue(&router, "reason-start-failed-src").await;
    let start_failed_role =
        role_for(&router, start_failed, &start_failed_source, &target_arn).await;
    let created = create_pipe(
        &router,
        start_failed,
        &start_failed_source,
        &target_arn,
        &start_failed_role,
        Some(STOPPED),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {start_failed:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    // Every creation has landed before the later moves are asked for: a lifecycle call
    // arriving while one is in flight is refused.
    for case in [running, stopped, create_failed, start_failed] {
        settled(&router, case, case).await;
    }

    let answered = call(
        &router,
        "POST",
        &format!("/v1/pipes/{stopped}/stop"),
        None,
        REGION,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {stopped:?}: StopPipe answered {} {}",
        answered.status, answered.body
    );
    withdraw_from_role(
        &router,
        start_failed,
        &format!("{start_failed}-role"),
        &["sqs:SendMessage"],
    )
    .await;
    let answered = call(
        &router,
        "POST",
        &format!("/v1/pipes/{start_failed}/start"),
        None,
        REGION,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {start_failed:?}: StartPipe answered {} {}",
        answered.status, answered.body
    );

    let fixture = [
        Listed {
            case: running,
            state: RUNNING,
            reason: Reason::UserInitiated,
        },
        Listed {
            case: stopped,
            state: STOPPED,
            reason: Reason::UserInitiated,
        },
        Listed {
            case: create_failed,
            state: CREATE_FAILED,
            reason: Reason::Failure {
                source_arn: gone_source,
                mentions: &["does not exist"],
            },
        },
        Listed {
            case: start_failed,
            state: START_FAILED,
            reason: Reason::Failure {
                source_arn: start_failed_source,
                mentions: &["sqs:ReceiveMessage"],
            },
        },
    ];

    // What a read of ONE pipe reports, once every move has come to rest: the state it
    // settled in and the reason that state was reached by.
    for pipe in &fixture {
        let case = pipe.case;
        let reported = settled(&router, case, case).await;
        let state = reported.string("CurrentState");
        assert_eq!(
            state, pipe.state,
            "case {case:?}: expected the pipe to rest in {}, it reports {state} reason={:?}",
            pipe.state,
            reported.member("StateReason")
        );
        assert_reason(
            case,
            "DescribePipe",
            reported.member("StateReason").as_str(),
            &pipe.reason,
        );
    }

    // (the listing's case, its query, the pipes it has to return)
    let listings: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "unpaged",
            String::new(),
            vec![create_failed, running, start_failed, stopped],
        ),
        (
            "name-prefix",
            format!("?NamePrefix={create_failed}"),
            vec![create_failed],
        ),
        (
            "name-prefix-matching-two",
            "?NamePrefix=reason-st".to_string(),
            vec![start_failed, stopped],
        ),
        (
            "current-state-filter",
            format!("?CurrentState={START_FAILED}"),
            vec![start_failed],
        ),
        (
            "desired-state-filter",
            format!("?DesiredState={STOPPED}"),
            vec![stopped],
        ),
        (
            "limit-covering-every-match",
            "?Limit=10".to_string(),
            vec![create_failed, running, start_failed, stopped],
        ),
    ];
    for (listing, query, expected) in listings {
        let listed = list(&router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {listing:?}: ListPipes answered {} {}",
            listed.status, listed.body
        );
        let reasons = listed_reasons(&listed);
        let names: Vec<&str> = reasons.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names, expected,
            "case {listing:?}: expected the listing to return {expected:?}, got {names:?}"
        );
        for pipe in fixture.iter().filter(|pipe| expected.contains(&pipe.case)) {
            assert_reason(
                pipe.case,
                &format!("the {listing:?} listing"),
                reason_for(&reasons, pipe.case),
                &pipe.reason,
            );
        }
    }

    // A paged walk at the smallest limit: the member is on the entry of EVERY page, and
    // the pages together are the whole inventory, in name order, with a reason on each pipe.
    let walked = paged_walk(&router, "paged-walk", MIN_LIMIT).await;
    let walked_names: Vec<&str> = walked.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        walked_names,
        vec![create_failed, running, start_failed, stopped],
        "case \"paged-walk\": expected the pages to enumerate every pipe, got {walked_names:?}"
    );
    for pipe in &fixture {
        assert_reason(
            pipe.case,
            "the paged walk",
            reason_for(&walked, pipe.case),
            &pipe.reason,
        );
    }
}

/// The largest page a caller may ask for. Source: the `@range(min: 1, max: 100)` of
/// `LimitMax100`, the shape `ListPipes`' `Limit` member targets.
const MAX_LIMIT: usize = 100;

/// The smallest page a caller may ask for — the other bound of that same range.
///
/// `pub(crate)` with [`paged_walk`], because
/// `tests/all/pipe_state_reason_after_recovery.rs` walks its own fixture at the same bound.
pub(crate) const MIN_LIMIT: usize = 1;

/// One pipe of the endpoint fixture: the resources it reads and writes, and how the state it
/// comes to rest in was reached.
struct Wired {
    /// Also the pipe's name, so every row is independent of the others.
    case: &'static str,
    source: &'static str,
    target: &'static str,
    /// The `DesiredState` its creation asks for, or `None` to leave it at the default.
    desired_state: Option<&'static str>,
    /// The lifecycle calls driven after the creation, in order; each is made only once the
    /// move before it has come to rest.
    then: &'static [&'static str],
    /// The `CurrentState` the pipe rests in once every one of those has landed.
    rests_in: &'static str,
}

/// Every page of a walk bounded by `limit`, as `(name, StateReason)` entries in the order the
/// pages returned them. Each page is read with the token the page before it handed back, so
/// what is walked is the service's OWN continuation and not a re-query.
///
/// The ONE page walk of the listing suites: `pub(crate)` because
/// `tests/all/pipe_state_reason_after_recovery.rs` reads the pages of its own fixture through
/// it rather than a second copy of the continuation loop.
pub(crate) async fn paged_walk(
    router: &MultiServiceRouter,
    walk: &str,
    limit: usize,
) -> Vec<(String, Option<String>)> {
    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    let mut query = format!("?Limit={limit}");
    for page in 1..=PAGE_BUDGET {
        let listed = list(router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {walk:?} page {page}: ListPipes answered {} {}",
            listed.status, listed.body
        );
        let reasons = listed_reasons(&listed);
        assert!(
            !reasons.is_empty() && reasons.len() <= limit,
            "case {walk:?} page {page}: expected between 1 and {limit} entries on a page, \
             got {reasons:?}"
        );
        entries.extend(reasons);
        let token = listed.string("NextToken");
        if token.is_empty() {
            return entries;
        }
        query = format!("?Limit={limit}&NextToken={token}");
    }
    panic!("case {walk:?}: the listing was still handing out a NextToken after {PAGE_BUDGET} pages");
}

/// The `StateReason` a read of ONE pipe reported for `case`, from the describes taken once
/// every move had come to rest.
fn described_reason<'reads>(reads: &'reads [(&str, String)], case: &str) -> &'reads str {
    reads
        .iter()
        .find(|(name, _)| *name == case)
        .map(|(_, reason)| reason.as_str())
        .unwrap_or_else(|| panic!("case {case:?}: the fixture never described this pipe"))
}

/// RULE: every element of the list `ListPipes` returns carries that pipe's own `StateReason`
/// — the reason for the state it is in, the same value a read of that one pipe reports —
/// whichever modelled query selected the element and whichever page it arrived on.
///
/// The rows differ along what the rule generalises over: the KIND of resource each pipe
/// names (an Amazon SQS queue, a Kinesis stream, a self-managed Kafka broker as source; a
/// queue and a Lambda function as target), the modelled query the entry is selected by
/// (none, each ARN-prefix filter at a whole ARN and at a shared prefix, a non-ARN source
/// scheme, a state filter, and two filters together), the number of entries the query
/// returns (one, some, all), how each pipe reached its state (its creation, a later stop, a
/// later start, a create/start/stop sequence, a later reconfiguration), and the page bound
/// the entry arrives under (both bounds of the `Limit` range, and every continuation of the
/// walk at the smaller one).
#[tokio::test]
async fn every_modelled_query_returns_each_listed_pipes_own_reason() {
    let router = router();
    // The endpoints are the forms this service's own filter table spans. Each pipe rests in a
    // state its CALLER drove it into, so it reports the user-initiated reason — except the
    // one running over a source read PER SHARD, which reports what its poller made of its
    // last pass over a stream nothing has been put on.
    let fixture = [
        Wired {
            case: "listed-kafka-cycled",
            source: "smk://b-1.broker.kafka.us-west-2.amazonaws.com:9092",
            target: "arn:aws:lambda:us-west-2:000000000000:function:listed-handler",
            desired_state: Some(STOPPED),
            then: &["StartPipe", "StopPipe"],
            rests_in: STOPPED,
        },
        Wired {
            case: "listed-kinesis-started",
            source: "arn:aws:kinesis:us-west-2:000000000000:stream/listed-events",
            target: "arn:aws:sqs:us-west-2:000000000000:listed-results",
            desired_state: Some(STOPPED),
            then: &["StartPipe"],
            rests_in: RUNNING,
        },
        Wired {
            case: "listed-sqs-created",
            source: "arn:aws:sqs:us-west-2:000000000000:listed-orders",
            target: "arn:aws:sqs:us-west-2:000000000000:listed-results",
            desired_state: None,
            then: &[],
            rests_in: RUNNING,
        },
        Wired {
            case: "listed-sqs-stopped",
            source: "arn:aws:sqs:us-west-2:000000000000:listed-returns",
            target: "arn:aws:lambda:us-west-2:000000000000:function:listed-handler",
            desired_state: Some(RUNNING),
            then: &["StopPipe"],
            rests_in: STOPPED,
        },
        Wired {
            case: "listed-sqs-updated",
            source: "arn:aws:sqs:us-west-2:000000000000:listed-updates",
            target: "arn:aws:sqs:us-west-2:000000000000:listed-results",
            desired_state: Some(RUNNING),
            then: &["UpdatePipe"],
            rests_in: RUNNING,
        },
    ];

    for pipe in &fixture {
        let case = pipe.case;
        let mut body = minimal_body(pipe.source, pipe.target);
        if let Some(desired_state) = pipe.desired_state {
            body["DesiredState"] = json!(desired_state);
        }
        // A source whose own parameters shape requires a member cannot be created without
        // it, so each row carries the one its source type needs.
        if let Some(parameters) = required_source_parameters(pipe.source) {
            body["SourceParameters"] = parameters;
        }
        let created = create(&router, case, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
    }

    // The later calls, each made once the move before it has come to rest: a lifecycle call
    // arriving while one is still in flight is refused.
    for pipe in &fixture {
        let case = pipe.case;
        for operation in pipe.then {
            settled(&router, case, case).await;
            let answered = drive(&router, case, operation).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: {operation} answered {} {}",
                answered.status, answered.body
            );
        }
    }

    // What each pipe of the fixture has to report, by the state it rests in and the source
    // it reads: a pipe RUNNING over a Kinesis stream is read per shard, so its reason is its
    // poller's reading rather than the move that put it there.
    let expected_reason = |case: &str| -> Reason {
        let pipe = fixture
            .iter()
            .find(|pipe| pipe.case == case)
            .unwrap_or_else(|| panic!("case {case:?}: the listing returned no fixture pipe"));
        if pipe.rests_in == RUNNING && pipe.source.starts_with("arn:aws:kinesis:") {
            Reason::PollerIdle
        } else {
            Reason::UserInitiated
        }
    };

    // What a read of ONE pipe reports once every move has landed: the state it settled in
    // and the reason that state was reached by. The listings below are then asserted to
    // report the SAME reason for the same pipe.
    let mut described: Vec<(&str, String)> = Vec::new();
    for pipe in &fixture {
        let case = pipe.case;
        let reported = settled(&router, case, case).await;
        let state = reported.string("CurrentState");
        assert_eq!(
            state, pipe.rests_in,
            "case {case:?}: expected the pipe to rest in {}, it reports {state} reason={:?}",
            pipe.rests_in,
            reported.member("StateReason")
        );
        assert_reason(
            case,
            "DescribePipe",
            reported.member("StateReason").as_str(),
            &expected_reason(case),
        );
        described.push((case, reported.string("StateReason")));
    }

    let every_pipe: Vec<&str> = fixture.iter().map(|pipe| pipe.case).collect();
    // (the listing's case, its query, the pipes it has to return, in name order)
    let listings: Vec<(&str, String, Vec<&str>)> = vec![
        ("unpaged", String::new(), every_pipe.clone()),
        (
            "source-prefix-at-a-whole-arn",
            "?SourcePrefix=arn:aws:kinesis:us-west-2:000000000000:stream/listed-events"
                .to_string(),
            vec!["listed-kinesis-started"],
        ),
        (
            "source-prefix-shared-by-three",
            "?SourcePrefix=arn:aws:sqs".to_string(),
            vec!["listed-sqs-created", "listed-sqs-stopped", "listed-sqs-updated"],
        ),
        (
            "source-prefix-of-a-non-arn-scheme",
            "?SourcePrefix=smk://".to_string(),
            vec!["listed-kafka-cycled"],
        ),
        (
            "target-prefix-of-another-service",
            "?TargetPrefix=arn:aws:lambda".to_string(),
            vec!["listed-kafka-cycled", "listed-sqs-stopped"],
        ),
        (
            "target-prefix-at-a-whole-arn",
            "?TargetPrefix=arn:aws:sqs:us-west-2:000000000000:listed-results".to_string(),
            vec![
                "listed-kinesis-started",
                "listed-sqs-created",
                "listed-sqs-updated",
            ],
        ),
        (
            "current-state-filter",
            format!("?CurrentState={STOPPED}"),
            vec!["listed-kafka-cycled", "listed-sqs-stopped"],
        ),
        (
            "name-prefix-and-current-state",
            format!("?NamePrefix=listed-sqs&CurrentState={STOPPED}"),
            vec!["listed-sqs-stopped"],
        ),
        (
            "desired-state-and-target-prefix",
            format!("?DesiredState={RUNNING}&TargetPrefix=arn:aws:sqs"),
            vec![
                "listed-kinesis-started",
                "listed-sqs-created",
                "listed-sqs-updated",
            ],
        ),
        (
            "limit-at-the-maximum",
            format!("?Limit={MAX_LIMIT}"),
            every_pipe.clone(),
        ),
    ];
    for (listing, query, expected) in listings {
        let listed = list(&router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {listing:?}: ListPipes answered {} {}",
            listed.status, listed.body
        );
        let reasons = listed_reasons(&listed);
        let names: Vec<&str> = reasons.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names, expected,
            "case {listing:?}: expected the listing to return {expected:?}, got {names:?}"
        );
        for case in &expected {
            let reported = reason_for(&reasons, case);
            assert_reason(
                case,
                &format!("the {listing:?} listing"),
                reported,
                &expected_reason(case),
            );
            let one_pipe = described_reason(&described, case);
            assert_eq!(
                reported,
                Some(one_pipe),
                "case {case:?}: the {listing:?} listing reports {reported:?}, while a read of \
                 that one pipe reports {one_pipe:?}"
            );
        }
    }

    // Both ends of the `Limit` range: the member is on the entry of every page either walk
    // returns, and each walk's pages together are the whole inventory in name order.
    for limit in [MIN_LIMIT, MAX_LIMIT] {
        let walk = format!("paged-walk-at-{limit}");
        let walked = paged_walk(&router, &walk, limit).await;
        let walked_names: Vec<&str> = walked.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            walked_names, every_pipe,
            "case {walk:?}: expected the pages to enumerate every pipe, got {walked_names:?}"
        );
        for case in &every_pipe {
            let reported = reason_for(&walked, case);
            assert_reason(case, &walk, reported, &expected_reason(case));
            let one_pipe = described_reason(&described, case);
            assert_eq!(
                reported,
                Some(one_pipe),
                "case {case:?}: the {walk:?} walk reports {reported:?}, while a read of that \
                 one pipe reports {one_pipe:?}"
            );
        }
    }
}
