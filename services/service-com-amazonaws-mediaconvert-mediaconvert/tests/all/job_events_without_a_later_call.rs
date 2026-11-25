// SPDX-License-Identifier: CC0-1.0

//! A job's state change reaches EventBridge with NO later MediaConvert call.
//!
//! The RULE under test: a job's exit from `SUBMITTED` happens when the clock makes it due —
//! not when somebody next asks MediaConvert about the job — and the
//! `MediaConvert Job State Change` event that reports it is published onto the account's
//! default event bus at that moment, so a client following the documented event-driven
//! workflow (submit a job, then react to its state changes on an EventBridge rule whose
//! target is SQS, Lambda or SNS) observes the job's terminal status without calling
//! MediaConvert again. Its negative half holds just as firmly: a job that never begins —
//! one its `PAUSED` queue is holding, whether or not it is then cancelled — moves nowhere
//! and so publishes nothing. Source: `QueueStatus`, "If you pause a queue, jobs in that
//! queue won't begin", for what never moves, and `CreateJob$StatusUpdateInterval`, "how
//! often MediaConvert sends STATUS_UPDATE events to Amazon CloudWatch Events", which is the
//! model's own statement that this service publishes a job's progress.
//!
//! The rows span what the rule generalises over: how many jobs the clock owes a transition
//! (one, and several at once), whether the job's queue lets it begin at all, whether the
//! queue is activated afterwards — the other way a job comes to be owed its exit — and
//! whether the job was cancelled out of the queue first.
//!
//! No case asks MediaConvert for the job while it waits: the LAST MediaConvert request a
//! row makes is the submit of a WITNESS job, and the wait is bounded by that witness's own
//! event arriving rather than by a duration, so nothing here depends on how fast the
//! machine runs. The witness is submitted after every other call of the row, so its
//! transition falls due last: once its event has been delivered, a subject job's event has
//! either been delivered too or is never coming.
//!
//! EventBridge and SQS are mounted BESIDE MediaConvert on the SAME engine, so the
//! publication travels the real in-process callback, through the real bus, to a real SQS
//! target.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_mediaconvert_mediaconvert::ComAmazonawsMediaconvertMediaconvertState;

use super::job_lifecycle::{arn, settings as job_settings, Emulator, ROLE, SHORT_SETTLE};
// The bus, the rule, its SQS target and the drain are the ones `job_state_change_events`
// already builds for this service's events: one place that wires them beats a second copy
// that could drift from the delivery contract.
use super::job_state_change_events::{
    drain, events_for, given_queue, given_rule, grant_delivery, statuses_of, EVENT_SOURCE,
    JOB_STATE_CHANGE, MATCHING_RULE,
};
use super::queue_update::update_queue;

/// How long a case waits for a CONDITION — an event delivered, a job moved — before it
/// gives up. Generous: it is a deadline for a machine under load, not a span anything is
/// expected to take (every row's own span is [`SHORT_SETTLE`]).
const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll pauses before it looks again. Only how often the condition is checked,
/// never what the case asserts.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The span the one case that CANCELS a running job pins.
///
/// The two steps of a run divide the span between them, so a case that has to act while the
/// job runs waits half of it for the job to begin and then has the other half to act in —
/// which is what makes that case about the cancel rather than about how fast the machine
/// is.
const CANCELLABLE_SPAN: Duration = Duration::from_secs(5);

/// The queue the subject jobs of a row are submitted to.
const SUBJECT_QUEUE: &str = "events-subject";

/// The ACTIVE queue the witness job is submitted to, so a row whose subject queue is
/// `PAUSED` still has a job the clock is bound to move.
const WITNESS_QUEUE: &str = "events-witness";

/// A queue that stays `PAUSED` for a whole case, holding the job that is the negative
/// control of what the clock moves.
const HELD_QUEUE: &str = "events-held";

/// The terminal status a job of this emulator reaches once its run has ended.
const TERMINAL_STATUS: &str = "ERROR";

/// The statuses a job that RUNS moves into, and so the events it publishes: it begins
/// processing (`PROGRESSING`) and then its run ends.
const RUN: &[&str] = &["PROGRESSING", TERMINAL_STATUS];

/// What a job that never begins publishes.
const NOTHING: &[&str] = &[];

/// One row of the table.
struct Row {
    case: &'static str,
    /// The status the subject queue is created with: `PAUSED` holds its jobs, so they
    /// never begin.
    queue_status: &'static str,
    /// Whether `UpdateQueue` activates that queue after the subjects were submitted —
    /// the other door that leaves the clock owing a job its exit from `SUBMITTED`.
    activate_after_submit: bool,
    /// Whether every subject is cancelled out of its queue before anything is due.
    cancel_after_submit: bool,
    /// How many subject jobs the row submits.
    subjects: usize,
    /// The `detail.status` of every event each subject must have published, in the order
    /// it moved into them.
    published: &'static [&'static str],
    /// The status each subject must report at the end of the row.
    job_status: &'static str,
}

/// The rows: the number of jobs owed a run, whether their queue lets them begin, whether
/// it is activated afterwards, and whether they were cancelled out of it first.
const ROWS: &[Row] = &[
    Row {
        case: "one job on an ACTIVE queue",
        queue_status: "ACTIVE",
        activate_after_submit: false,
        cancel_after_submit: false,
        subjects: 1,
        published: RUN,
        job_status: TERMINAL_STATUS,
    },
    Row {
        case: "three jobs on an ACTIVE queue, each owed its own run",
        queue_status: "ACTIVE",
        activate_after_submit: false,
        cancel_after_submit: false,
        subjects: 3,
        published: RUN,
        job_status: TERMINAL_STATUS,
    },
    Row {
        case: "a job its PAUSED queue is holding",
        queue_status: "PAUSED",
        activate_after_submit: false,
        cancel_after_submit: false,
        subjects: 1,
        published: NOTHING,
        job_status: "SUBMITTED",
    },
    Row {
        case: "a job released by activating its queue",
        queue_status: "PAUSED",
        activate_after_submit: true,
        cancel_after_submit: false,
        subjects: 1,
        published: RUN,
        job_status: TERMINAL_STATUS,
    },
    Row {
        case: "a job cancelled while its PAUSED queue was holding it",
        queue_status: "PAUSED",
        activate_after_submit: false,
        cancel_after_submit: true,
        subjects: 1,
        published: NOTHING,
        job_status: "CANCELED",
    },
];

/// Submit one job to `queue` and answer its id.
async fn submit(emulator: &Emulator, case: &str, queue: &str, index: usize) -> String {
    let created = emulator
        .create_job(json!({
            "role": ROLE,
            "queue": queue,
            "settings": job_settings(
                &format!("s3://example-bucket/in/pump-{index}.mp4"),
                "_a",
            ),
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateJob answered {} with body {}",
        created.status, created.body
    );
    let id = created.string("/job/id");
    assert!(
        !id.is_empty(),
        "case {case:?}: expected a minted job id, got {}",
        created.body
    );
    // Whatever the clock does next, the submit itself answers the non-terminal status: the
    // span is never zero, so a caller acting straight after the submit still sees it.
    assert_eq!(
        created.string("/job/status"),
        "SUBMITTED",
        "case {case:?}: CreateJob must answer SUBMITTED, got {}",
        created.body
    );
    id
}

/// Create the MediaConvert queue `name` with `status`.
async fn given_job_queue(emulator: &Emulator, case: &str, name: &str, status: &str) {
    let created = emulator
        .create_queue(json!({ "name": name, "status": status }))
        .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateQueue answered {} with body {}",
        created.status, created.body
    );
}

/// Every event delivered to the queue at `url` by the time the WITNESS job's run has been
/// announced to its end, accumulated across polls.
///
/// The wait ends on a CONDITION — the witness's own terminal event, the one the clock is
/// bound to produce — and the deadline is only there so a broken emulator fails instead of
/// hanging. Not one MediaConvert request is made in here.
async fn events_until_witness(emulator: &Emulator, case: &str, url: &str, witness: &str) -> Vec<Value> {
    let started = Instant::now();
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(emulator, case, url).await);
        if statuses_of(&events_for(&collected, witness)).contains(&TERMINAL_STATUS) {
            // One more pass, so an event delivered beside the witness's is collected too
            // rather than left on the queue by the batch that answered.
            collected.extend(drain(emulator, case, url).await);
            return collected;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the witness job {witness}'s own run was never announced to its \
             end, so the clock published nothing at all; collected {collected:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// A job's state change is published as the clock makes it due, with no MediaConvert call
/// after the submit — and a job that never begins publishes nothing.
#[tokio::test]
async fn a_due_transition_is_published_with_no_later_mediaconvert_call() {
    for row in ROWS {
        let case = row.case;
        // The settle span is state, so each row pins it instead of racing the real clock.
        let emulator = Emulator::with_settle_span(SHORT_SETTLE)
            .mount(service_com_amazonaws_eventbridge_awsevents::EmulatedService::new)
            .mount(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new);

        let (url, queue_arn) = given_queue(&emulator, case, "mediaconvert-events").await;
        let rule = given_rule(
            &emulator,
            case,
            MATCHING_RULE,
            json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
            &queue_arn,
        )
        .await;
        grant_delivery(&emulator, case, &url, &queue_arn, &rule).await;

        given_job_queue(&emulator, case, SUBJECT_QUEUE, row.queue_status).await;
        given_job_queue(&emulator, case, WITNESS_QUEUE, "ACTIVE").await;

        let mut subjects = Vec::new();
        for index in 0..row.subjects {
            subjects.push(submit(&emulator, case, SUBJECT_QUEUE, index).await);
        }
        if row.cancel_after_submit {
            for id in &subjects {
                let cancelled = emulator.cancel_job(id).await;
                assert_eq!(
                    cancelled.status, 202,
                    "case {case:?}: CancelJob answered {} with body {}",
                    cancelled.status, cancelled.body
                );
            }
        }
        if row.activate_after_submit {
            let resumed = update_queue(&emulator, SUBJECT_QUEUE, json!({ "status": "ACTIVE" })).await;
            assert_eq!(
                resumed.status, 200,
                "case {case:?}: UpdateQueue answered {} with body {}",
                resumed.status, resumed.body
            );
        }
        // The LAST MediaConvert request of the row, and the one whose events bound the
        // wait: it is submitted after every other call, so its run ends last of all.
        let witness = submit(&emulator, case, WITNESS_QUEUE, row.subjects).await;

        let delivered = events_until_witness(&emulator, case, &url, &witness).await;

        for id in &subjects {
            let own = events_for(&delivered, id);
            assert_eq!(
                statuses_of(&own),
                row.published,
                "case {case:?}: expected the statuses {:?} published for job {id}, got \
                 {own:?}",
                row.published
            );
            for event in &own {
                assert_eq!(
                    event["source"],
                    json!(EVENT_SOURCE),
                    "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
                );
                assert_eq!(
                    event["detail-type"],
                    json!(JOB_STATE_CHANGE),
                    "case {case:?}: expected detail-type {JOB_STATE_CHANGE}, got {event}"
                );
                assert_eq!(
                    event["resources"],
                    json!([arn(&format!("jobs/{id}"))]),
                    "case {case:?}: expected the job's ARN in resources, got {event}"
                );
            }
            if let Some(event) = own.last() {
                // The last status announced for the job is the status the job ends in, so
                // the event stream ends where the control plane stands.
                assert_eq!(
                    event["detail"]["status"],
                    json!(row.job_status),
                    "case {case:?}: expected the last detail.status {}, got {event}",
                    row.job_status
                );
            }
        }

        // Only NOW is MediaConvert asked about the subjects: the status each one reports is
        // the status its event carried (or, for a job that never began, the status that
        // explains the silence), so the event stream and the control plane agree.
        for id in &subjects {
            let read = emulator.get_job(id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetJob answered {} with body {}",
                read.status, read.body
            );
            assert_eq!(
                read.string("/job/status"),
                row.job_status,
                "case {case:?}: expected the job {id} to report {}, got {}",
                row.job_status,
                read.body
            );
        }
    }
}

/// A job cancelled WHILE IT RUNS announces that move: the run had begun, so the status it
/// moves into on the cancel is published like the one the clock moved it into, and the
/// stream ends where the control plane stands. The job cancelled BEFORE it began is the
/// negative half the table above covers.
#[tokio::test]
async fn a_cancel_of_a_running_job_publishes_the_status_it_moved_into() {
    let case = "a job cancelled while it was running";
    // A span wide enough to act inside: the case waits for the job to BEGIN — half the span
    // out — and then has the other half before the clock would end the run on its own.
    let emulator = Emulator::with_settle_span(CANCELLABLE_SPAN)
        .mount(service_com_amazonaws_eventbridge_awsevents::EmulatedService::new)
        .mount(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new);

    let (url, queue_arn) = given_queue(&emulator, case, "mediaconvert-events").await;
    let rule = given_rule(
        &emulator,
        case,
        MATCHING_RULE,
        json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
        &queue_arn,
    )
    .await;
    grant_delivery(&emulator, case, &url, &queue_arn, &rule).await;
    given_job_queue(&emulator, case, SUBJECT_QUEUE, "ACTIVE").await;

    let id = submit(&emulator, case, SUBJECT_QUEUE, 0).await;
    // Wait for the CONDITION the cancel needs — the job running — reading the store, so no
    // MediaConvert request is what moved it.
    let started = Instant::now();
    while stored_status(&emulator, &id) != "PROGRESSING" {
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the clock never began the job {id}; it is {:?}",
            stored_status(&emulator, &id)
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }

    let cancelled = emulator.cancel_job(&id).await;
    assert_eq!(
        cancelled.status, 202,
        "case {case:?}: a running job must be cancellable, got {} with body {}",
        cancelled.status, cancelled.body
    );

    // Both moves of the job: the one the clock made and the one the cancel made.
    let mut collected: Vec<Value> = Vec::new();
    let started = Instant::now();
    loop {
        collected.extend(drain(&emulator, case, &url).await);
        let own = events_for(&collected, &id);
        if statuses_of(&own) == ["PROGRESSING", "CANCELED"] || started.elapsed() >= DEADLINE {
            assert_eq!(
                statuses_of(&own),
                ["PROGRESSING", "CANCELED"],
                "case {case:?}: expected the job's two moves published for {id}, got {own:?}"
            );
            break;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }

    let read = emulator.get_job(&id).await;
    assert_eq!(
        read.string("/job/status"),
        "CANCELED",
        "case {case:?}: expected the cancelled job to report CANCELED, got {}",
        read.body
    );
}

/// The clock itself, with no bus mounted at all: a job's run — out of `SUBMITTED`, and on
/// to the status it ends in — is materialised in the STORE the handlers write to, with no
/// MediaConvert request after the submit, which is what makes the publication above
/// possible and what a client that polls nothing depends on.
///
/// The rows differ in what came to owe the run: a submit onto an `ACTIVE` queue, and the
/// activation of the `PAUSED` queue a job was already waiting in. A job whose queue keeps
/// holding it is the negative control in both, asserted against the same store at the same
/// instant.
#[tokio::test]
async fn the_clock_moves_a_job_in_the_store_with_no_later_mediaconvert_call() {
    for (case, start_paused) in [
        ("a job submitted to an ACTIVE queue", false),
        ("a job released by activating its queue", true),
    ] {
        let emulator = Emulator::with_settle_span(SHORT_SETTLE);
        let subject_status = if start_paused { "PAUSED" } else { "ACTIVE" };
        given_job_queue(&emulator, case, SUBJECT_QUEUE, subject_status).await;
        given_job_queue(&emulator, case, HELD_QUEUE, "PAUSED").await;

        let moving = submit(&emulator, case, SUBJECT_QUEUE, 0).await;
        // Held by its own queue for the whole case: the clock owes it nothing, so it is
        // what says the clock moves what it owes and nothing else.
        let held = submit(&emulator, case, HELD_QUEUE, 1).await;
        if start_paused {
            let resumed = update_queue(&emulator, SUBJECT_QUEUE, json!({ "status": "ACTIVE" })).await;
            assert_eq!(
                resumed.status, 200,
                "case {case:?}: UpdateQueue answered {} with body {}",
                resumed.status, resumed.body
            );
        }

        // The store the handlers write to, read directly through the harness's own
        // poll-until helper: a MediaConvert request would materialise the run itself, which
        // is the very thing under test.
        emulator.await_run(case, &moving).await;
        assert_eq!(
            stored_status(&emulator, &moving),
            TERMINAL_STATUS,
            "case {case:?}: expected the clock's own run to end in {TERMINAL_STATUS}, got \
             {:?}",
            stored_status(&emulator, &moving)
        );
        assert_eq!(
            stored_status(&emulator, &held),
            "SUBMITTED",
            "case {case:?}: a job its PAUSED queue is holding must not begin, got {:?}",
            stored_status(&emulator, &held)
        );
    }
}

/// The clock survives the DEATH of the pump that was driving it: a job submitted after a
/// pump has gone without standing down still has its run materialised and announced, with no
/// MediaConvert call after the submit.
///
/// The rule: a store's clock pump is a single claim, and the claim belongs to the pump that
/// holds it — so a pump that ends without standing down must leave it released. A pump can
/// end that way: the work it drives panics, its task is aborted, or the runtime it was
/// spawned on goes away. A claim held by a pump that is gone would stop this store's clock
/// for the life of the process, and with it every event a later job's run would publish —
/// which is exactly what a client that submits a job and then only listens depends on.
///
/// The case takes a pump down the way those ends do, by driving the submit that STARTS it on
/// a runtime it then drops, and then judges a SECOND job submitted from a fresh one. Nothing
/// is asserted about the first job: its run may have been claimed by the pump that died, and
/// what this case is about is the clock being left usable, not that job.
#[test]
fn a_pump_that_ended_without_standing_down_leaves_the_clock_usable() {
    let case = "a pump taken down with the runtime it was spawned on";
    let emulator = Emulator::with_settle_span(SHORT_SETTLE)
        .mount(service_com_amazonaws_eventbridge_awsevents::EmulatedService::new)
        .mount(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new);

    let first = tokio::runtime::Runtime::new().expect("a runtime to drive the first submit on");
    let url = first.block_on(async {
        let (url, queue_arn) = given_queue(&emulator, case, "mediaconvert-events").await;
        let rule = given_rule(
            &emulator,
            case,
            MATCHING_RULE,
            json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
            &queue_arn,
        )
        .await;
        grant_delivery(&emulator, case, &url, &queue_arn, &rule).await;
        given_job_queue(&emulator, case, SUBJECT_QUEUE, "ACTIVE").await;
        // The submit that starts this store's pump, on the runtime that is about to go.
        submit(&emulator, case, SUBJECT_QUEUE, 0).await;
        url
    });
    // And the pump goes with it, without ever standing down.
    drop(first);

    let second = tokio::runtime::Runtime::new().expect("a runtime to drive the second submit on");
    second.block_on(async {
        let subject = submit(&emulator, case, SUBJECT_QUEUE, 1).await;
        let started = Instant::now();
        let mut collected: Vec<Value> = Vec::new();
        loop {
            // SQS only: not one MediaConvert request is made while waiting, so nothing here
            // can be what materialised the run.
            collected.extend(drain(&emulator, case, &url).await);
            let own = events_for(&collected, &subject);
            if statuses_of(&own) == RUN {
                return;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "case {case:?}: expected the statuses {RUN:?} published for the job {subject} \
                 submitted after the pump was gone, got {own:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    });
}

/// The status the STORE holds for the job `id`.
fn stored_status(emulator: &Emulator, id: &str) -> String {
    emulator
        .engine
        .state::<ComAmazonawsMediaconvertMediaconvertState>()
        .lock()
        .jobs
        .get(id)
        .map(|job| job.status.clone())
        .unwrap_or_default()
}
