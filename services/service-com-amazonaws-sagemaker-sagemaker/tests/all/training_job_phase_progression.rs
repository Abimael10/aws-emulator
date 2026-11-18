// SPDX-License-Identifier: CC0-1.0

//! How a training job's work PROGRESSES: the `SecondaryStatus` stages the job passes
//! through on its way to a terminal status, the history it reports for them, and the runtime
//! cap that can end it before they are over.
//!
//! Driven end to end through the same harness `training_job_execution` builds — SageMaker,
//! Amazon S3 and IAM on ONE shared engine, every call travelling the real wire — so what is
//! asserted is what an SDK client would see.
//!
//! RULE ONE — an accepted job whose run can read its channels and write its artifact
//! advances through the canonical secondary statuses
//! (`DescribeTrainingJob$SecondaryStatus`: `Starting`, then `Downloading` for a job with
//! data to bring down, `Training`, `Uploading`) and ends `Completed`/`Completed`, with
//! `SecondaryStatusTransitions` naming every stage it was in, in that order, each entry
//! starting where the one before it ended and carrying the `StatusMessage` that describes
//! that stage.
//!
//! RULE TWO — what fails a run is a LEG of its work that failed, not the SHAPE of the
//! caller's data: nothing in the API models what a channel's bytes mean, so data that holds
//! no record of the comma-separated form the built-in algorithms document still leaves a job
//! that completes, with the artifact written and no metric for a channel nothing was
//! measured over.
//!
//! RULE THREE — `StoppingCondition$MaxRuntimeInSeconds` is honoured: "if the job does not
//! complete during this time, SageMaker ends the job", which is the primary status `Stopped`
//! with the secondary `MaxRuntimeExceeded` beside it and no failure reason — the job ends
//! exactly at the cap — while a cap the work fits inside leaves the job `Completed`.
//!
//! RULE FOUR — the clock lays those stages out from the instant the job's RUN reported, never
//! from the instant the create that accepted it began: the run is performed inside that
//! create, so the FIRST read a caller can make still finds the job `InProgress` with a
//! `SecondaryStatus` out of the pre-upload set, and a poller still observes a stage of the
//! work in progress — however much work the run had to do before it could report. The model
//! artifact belongs to the stage that uploads it, so a poll that reports a pre-upload stage
//! finds nothing at the output location and the completed job's artifact is an object Amazon
//! S3 holds.
//!
//! The tables span what the rules generalise over: how many channels a job declares and
//! whether it declares any at all, the shape of the records they hold, where a runtime
//! cap falls relative to the work — at the modelled minimum, inside the work, and at the
//! values the member's own doc names as its default and its maximum — and how much work the
//! run does before it reports: the records its channels hold and how many objects they are
//! spread over.
//!
//! Nothing here is asserted on how fast this host ran it. Every claim about a stage's window
//! is weighed against the schedule the job itself reports in `SecondaryStatusTransitions`:
//! the stages of the run begin after the instant the caller held the create's answer, each is
//! in effect for a whole [`PHASE_SPAN`], and every read is held to the stage that schedule
//! says was due when the read was made ([`each_read_reports_the_stage_that_was_due`]). So a
//! loaded host that looks later than an idle one observes a later stage, and no claim rests on
//! a poll landing inside a window the host could step over.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_key, artifact_uri, channel, csv_records, read_and_write, str_field, training_body,
    Emulator, BUCKET,
};

/// The `SecondaryStatus` members an accepted job passes through while it is `InProgress`,
/// in the order `DescribeTrainingJob$SecondaryStatus`'s own doc lists them
/// (smithy_query query=doc shape=DescribeTrainingJobResponse member=SecondaryStatus), and
/// then the terminal members this crate's jobs end in.
const CANONICAL_ORDER: [&str; 7] = [
    "Starting",
    "Downloading",
    "Training",
    "Uploading",
    "Completed",
    "MaxRuntimeExceeded",
    "Stopped",
];

/// The stages of its work a run brings data down for: what RULE ONE's history must name for
/// a job whose channels name an Amazon S3 location.
const WITH_DOWNLOAD: [&str; 4] = ["Starting", "Downloading", "Training", "Uploading"];

/// The same for a job that declares no channel at all: there is nothing to download, which
/// `DescribeTrainingJob$SecondaryStatus` calls "an optional stage".
const WITHOUT_DOWNLOAD: [&str; 3] = ["Starting", "Training", "Uploading"];

/// The statuses nothing leaves on its own, so a poll that reaches one is over.
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The `SecondaryStatus` members a job carries BEFORE its model artifact is uploaded: the
/// stage it is accepted in, and the stages of work that precede the upload
/// (`DescribeTrainingJob$SecondaryStatus`).
const PRE_UPLOAD: [&str; 3] = ["Starting", "Downloading", "Training"];

/// The stages of work only a RUNNING job is in, i.e. the pre-upload stages the run performs
/// rather than the one the create accepts the job in.
const IN_PROGRESS_STAGES: [&str; 2] = ["Downloading", "Training"];

/// How long a poll below gives the clock to settle a job: far longer than the span the state
/// gives a job's work, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops, and the
/// interval is a fraction of [`PHASE_SPAN`], so a poller looks many times inside every stage
/// the clock lays out.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long the clock keeps a job in each stage of its work, out of the crate's own state
/// rather than restated here: every claim below about a stage's window is weighed against the
/// schedule the emulator actually lays out.
const PHASE_SPAN: Duration =
    service_com_amazonaws_sagemaker_sagemaker::state::TRAINING_JOB_PHASE_SPAN;

/// The slack a comparison of two instants allows for carrying them as the epoch seconds a
/// timestamp travels the wire as: well under a millisecond, and orders below every span
/// compared against.
const TIMESTAMP_SLACK: f64 = 0.001;

/// An instant as the epoch seconds the wire carries it in, so an instant the caller held can
/// be weighed against one a response reports.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .expect("a host clock at or after the epoch")
        .as_secs_f64()
}

/// One read a caller made: the `SecondaryStatus` it reported, and the bracket of instants it
/// was made inside — what the caller held just before the call, and just after its answer.
///
/// The bracket is what makes an observation checkable without depending on how fast this host
/// ran it: a read whose whole bracket falls inside one stage's window must report THAT stage,
/// whereas a read the host delayed across a boundary may report either side of it. So a stage
/// a poller looked inside of is one the poller observed, however long any single call took.
#[derive(Debug, Clone)]
struct Read {
    status: String,
    before: f64,
    after: f64,
}

/// One `SecondaryStatusTransitions` entry as [`history`] reads it: the status, the instant the
/// job entered it, the instant it left it (`None` while it has not) and the message beside it.
type Stage = (String, f64, Option<f64>, Option<String>);

/// Describe `job` once, bracketed by the instants the caller held around the call.
async fn describe(emulator: &Emulator, job: &str) -> (Value, Read) {
    let before = epoch_seconds(SystemTime::now());
    let described = emulator
        .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
        .await;
    let read = Read {
        status: str_field(&described, "SecondaryStatus"),
        before,
        after: epoch_seconds(SystemTime::now()),
    };
    (described, read)
}

/// Poll `DescribeTrainingJob` until `job` reaches a status nothing leaves on its own,
/// answering that last response and every read made on the way.
async fn poll_until_terminal(emulator: &Emulator, job: &str, case: &str) -> (Value, Vec<Read>) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut reads: Vec<Read> = Vec::new();
    loop {
        let (described, read) = describe(emulator, job).await;
        reads.push(read);
        if TERMINAL.contains(&str_field(&described, "TrainingJobStatus").as_str()) {
            return (described, reads);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job {job} must reach a terminal status, last read {described}, \
             having been in {:?}",
            statuses_observed(&reads)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The distinct `SecondaryStatus` values a sequence of reads reported, in the order they were
/// reported in.
fn statuses_observed(reads: &[Read]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for read in reads {
        if seen.last() != Some(&read.status) {
            seen.push(read.status.clone());
        }
    }
    seen
}

/// Weigh every read against the job's own history: a read made wholly inside one stage's
/// window reported THAT stage.
///
/// This is what makes the stages' observability checkable on any host. A poller that looked
/// while a stage was in effect reported it — so every stage a read looked inside of is one the
/// poller observed — and a read a loaded host delayed into the next stage is held to the stage
/// that was due when it was finally made rather than to the one it would have found on an idle
/// host. The last entry is open-ended: the job is still in the status it ended in.
fn each_read_reports_the_stage_that_was_due(
    transitions: &[Stage],
    reads: &[Read],
    settled: &Value,
    case: &str,
) {
    let last = transitions.len().saturating_sub(1);
    for read in reads {
        for (index, (status, start, end, _)) in transitions.iter().enumerate() {
            let left = end.filter(|_| index < last);
            // Conservatively inside: a bracket that reaches either boundary is one the clock
            // may have moved the job across, and says nothing about which stage was due.
            let inside = read.before >= start + TIMESTAMP_SLACK
                && left.is_none_or(|left| read.after <= left - TIMESTAMP_SLACK);
            assert!(
                !inside || read.status == *status,
                "case {case:?}: a read made inside the {status} stage reported {:?}, \
                 got {settled}",
                read.status
            );
        }
    }
}

/// RULE FOUR, weighed against the job's own history rather than against how fast this host
/// ran: the stages the run performed are laid out from the instant the run REPORTED — which is
/// inside the create that accepted the job — so every one of them is still ahead of the instant
/// the caller held that create's answer, each is in effect for exactly one [`PHASE_SPAN`]
/// however much work the run did, and one of those windows belongs to a stage of the work in
/// progress for a poller to observe.
///
/// The stage the job was ACCEPTED in is the one exception: it runs from the creation instant
/// to the first stage of the run, so it lasts a span plus whatever the create spent on the
/// work before it could report.
fn stages_are_laid_out_after_the_create_answered(
    transitions: &[Stage],
    answered: f64,
    settled: &Value,
    case: &str,
) {
    let span = PHASE_SPAN.as_secs_f64();
    let last = transitions.len().saturating_sub(1);
    for (index, (status, start, end, _)) in transitions.iter().enumerate() {
        if index > 0 {
            assert!(
                *start > answered,
                "case {case:?}: the {status} stage must still be ahead of the instant the \
                 create answered, which was {answered}, got {settled}"
            );
        }
        if index < last {
            let left = end.unwrap_or_else(|| {
                panic!("case {case:?}: expected the {status} entry to have ended in {settled}")
            });
            let held = left - start;
            assert!(
                held >= span - TIMESTAMP_SLACK,
                "case {case:?}: the {status} stage must be in effect for a whole {PHASE_SPAN:?}, \
                 got {held} seconds of it in {settled}"
            );
            assert!(
                index == 0 || held <= span + TIMESTAMP_SLACK,
                "case {case:?}: the {status} stage of the run must be in effect for exactly \
                 {PHASE_SPAN:?}, got {held} seconds of it in {settled}"
            );
        }
    }
    assert!(
        transitions
            .get(1..last)
            .unwrap_or_default()
            .iter()
            .any(|(status, ..)| IN_PROGRESS_STAGES.contains(&status.as_str())),
        "case {case:?}: expected a whole-span window on one of the stages of the work in \
         progress {IN_PROGRESS_STAGES:?} for a poller to observe, got {settled}"
    );
}

/// The `SecondaryStatusTransitions` a read reports, as (status, start, end, message) rows.
fn history(described: &Value, case: &str) -> Vec<Stage> {
    described
        .get("SecondaryStatusTransitions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected SecondaryStatusTransitions in {described}"))
        .iter()
        .map(|entry| {
            (
                entry
                    .get("Status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("StartTime")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| panic!("case {case:?}: an entry with no StartTime in {described}")),
                entry.get("EndTime").and_then(Value::as_f64),
                entry
                    .get("StatusMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect()
}

/// Where `status` sits in the canonical order, so a sequence of observations can be weighed
/// against it.
fn order_of(status: &str, case: &str) -> usize {
    CANONICAL_ORDER
        .iter()
        .position(|known| *known == status)
        .unwrap_or_else(|| {
            panic!("case {case:?}: {status:?} is not one of the modelled statuses a job reaches")
        })
}

/// RULE ONE and RULE TWO: an accepted job advances through the canonical stages of its work
/// to `Completed`, and reports the history of the stages it was in.
///
/// The rows differ along how many channels the job declares, whether it declares any at all,
/// and the shape of the records they hold.
#[tokio::test]
async fn an_accepted_job_advances_through_the_canonical_stages_of_its_work() {
    /// One case: the job's name, the channels it declares (none at all when `None`), the
    /// stages its work must pass through, and whether the run measured a metric.
    struct Row {
        case: &'static str,
        job: &'static str,
        channels: Option<Value>,
        stages: &'static [&'static str],
        measured: bool,
    }

    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("phase-progression-role", Some(&read_and_write()))
        .await;
    emulator
        .given_object(BUCKET, "phases/train.csv", &csv_records(6, 2))
        .await;
    emulator
        .given_object(BUCKET, "phases/validation.csv", &csv_records(3, 2))
        .await;
    // Records of a form the comma-separated reading does not hold: one column only, and
    // fields that are not numbers. The run reads neither as a record.
    emulator
        .given_object(BUCKET, "phases/one-column.csv", "1\n2\n3\n")
        .await;
    emulator
        .given_object(BUCKET, "phases/notes.csv", "alpha,beta\ngamma,delta\n")
        .await;

    let rows = vec![
        Row {
            case: "one channel of comma-separated records",
            job: "phases-one-channel",
            channels: Some(json!([channel(
                "train",
                &format!("s3://{BUCKET}/phases/train.csv"),
                "S3Prefix"
            )])),
            stages: &WITH_DOWNLOAD,
            measured: true,
        },
        Row {
            case: "a training channel beside a validation channel",
            job: "phases-two-channels",
            channels: Some(json!([
                channel(
                    "train",
                    &format!("s3://{BUCKET}/phases/train.csv"),
                    "S3Prefix"
                ),
                channel(
                    "validation",
                    &format!("s3://{BUCKET}/phases/validation.csv"),
                    "S3Prefix"
                ),
            ])),
            stages: &WITH_DOWNLOAD,
            measured: true,
        },
        Row {
            case: "no channel at all, so nothing to download",
            job: "phases-no-channel",
            channels: None,
            stages: &WITHOUT_DOWNLOAD,
            measured: false,
        },
        Row {
            case: "records of one column, which hold no record of the channel's form",
            job: "phases-one-column",
            channels: Some(json!([channel(
                "train",
                &format!("s3://{BUCKET}/phases/one-column.csv"),
                "S3Prefix"
            )])),
            stages: &WITH_DOWNLOAD,
            measured: false,
        },
        Row {
            case: "fields that are not numbers",
            job: "phases-not-numbers",
            channels: Some(json!([channel(
                "train",
                &format!("s3://{BUCKET}/phases/notes.csv"),
                "S3Prefix"
            )])),
            stages: &WITH_DOWNLOAD,
            measured: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let output = format!("s3://{BUCKET}/phase-output/{}/", row.job);
        let mut body = training_body(
            row.job,
            &role,
            row.channels.clone().unwrap_or_else(|| json!([])),
            &output,
        );
        if row.channels.is_none() {
            body.as_object_mut()
                .expect("the request body is an object")
                .remove("InputDataConfig");
        }
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;
        // The first instant a caller can hold this create's answer, taken with no await
        // between it and the answer itself. The job is accepted with no stage of its work
        // begun and the move out of that is the clock's, so at THIS instant the job is
        // `InProgress` in the stage it was accepted in — which the job's own history is held
        // to below, pinning the instant the caller got the answer rather than whenever this
        // host got round to reading.
        let answered = epoch_seconds(SystemTime::now());

        // And the first read a caller can make, weighed against that history like every other.
        let (_, first) = describe(&emulator, row.job).await;

        let (settled, mut reads) = poll_until_terminal(&emulator, row.job, case).await;
        reads.insert(0, first);
        let observed = statuses_observed(&reads);
        assert_eq!(
            (
                str_field(&settled, "TrainingJobStatus"),
                str_field(&settled, "SecondaryStatus"),
            ),
            ("Completed".to_string(), "Completed".to_string()),
            "case {case:?}: expected a run that read its channels and wrote its artifact to \
             complete, got {settled}"
        );
        assert!(
            settled.get("FailureReason").is_none(),
            "case {case:?}: expected no FailureReason beside a completed job, got {settled}"
        );

        // Every status a poller saw is in the canonical order.
        let mut reached = 0;
        for status in &observed {
            let position = order_of(status, case);
            assert!(
                position >= reached,
                "case {case:?}: {status:?} is out of the canonical order, having been in \
                 {observed:?}"
            );
            reached = position;
        }

        // The history names every stage the job was in, in the canonical order, each entry
        // beginning where the one before it ended, and the last one ending when the job did.
        let transitions = history(&settled, case);
        // The stages between the accepted job and its terminal status are observable: each
        // is in effect for a whole span that begins after this create answered, and every
        // read this poller made inside one of those windows reported it.
        stages_are_laid_out_after_the_create_answered(&transitions, answered, &settled, case);
        each_read_reports_the_stage_that_was_due(&transitions, &reads, &settled, case);
        let expected: Vec<String> = row
            .stages
            .iter()
            .map(|stage| (*stage).to_string())
            .chain(["Completed".to_string()])
            .collect();
        assert_eq!(
            transitions
                .iter()
                .map(|(status, ..)| status.clone())
                .collect::<Vec<_>>(),
            expected,
            "case {case:?}: expected the history to name the stages of the work, got {settled}"
        );
        for (index, pair) in transitions.windows(2).enumerate() {
            assert_eq!(
                pair[0].2,
                Some(pair[1].1),
                "case {case:?}: entry {index} must end where the next one begins, got {settled}"
            );
        }
        for (status, _, _, message) in &transitions {
            assert!(
                message.as_deref().is_some_and(|text| !text.is_empty()),
                "case {case:?}: the {status} entry must describe the progress within it, \
                 got {settled}"
            );
        }
        let end_time = settled.get("TrainingEndTime").and_then(Value::as_f64);
        assert_eq!(
            transitions.last().and_then(|(_, _, end, _)| *end),
            end_time,
            "case {case:?}: the last entry must end when the job ended, got {settled}"
        );

        // The artifact the run uploaded is an object Amazon S3 holds at the URI the describe
        // reports, whatever the records it fitted on.
        let artifact = settled
            .get("ModelArtifacts")
            .and_then(|artifacts| artifacts.get("S3ModelArtifacts"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            artifact,
            artifact_uri(&output, row.job),
            "case {case:?}: expected the artifact under the declared output path, got {settled}"
        );
        assert!(
            emulator
                .object_bytes(BUCKET, &artifact_key(&output, row.job))
                .await
                .is_some(),
            "case {case:?}: expected {artifact} to name an object Amazon S3 holds"
        );
        assert_eq!(
            settled.get("FinalMetricDataList").is_some(),
            row.measured,
            "case {case:?}: a metric is reported for the channels the run measured one over \
             and for no others, got {settled}"
        );
    }
}

/// RULE THREE: a runtime cap the job's work does not fit inside ends it at the cap, with the
/// `Stopped`/`MaxRuntimeExceeded` pair the model groups together and no failure reason; a cap
/// the work fits inside leaves the job `Completed`.
///
/// The rows differ along where the cap falls: at the modelled minimum, inside the work, and
/// at the two values `StoppingCondition$MaxRuntimeInSeconds`'s own doc names — "the default
/// value is 1 day", "the maximum value is 28 days" — both of which the work fits well
/// inside, plus a job that states no stopping condition at all.
#[tokio::test]
async fn a_runtime_cap_the_work_does_not_fit_inside_ends_the_job_at_the_cap() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("runtime-cap-role", Some(&read_and_write()))
        .await;
    emulator
        .given_object(BUCKET, "caps/train.csv", &csv_records(5, 2))
        .await;

    // Each row: the case, the job's name, the cap it states (none at all for `None`), and the
    // (TrainingJobStatus, SecondaryStatus) pair it must end in.
    let rows: [(&str, &str, Option<i64>, &str, &str); 5] = [
        (
            "the modelled minimum cap",
            "cap-minimum",
            Some(1),
            "Stopped",
            "MaxRuntimeExceeded",
        ),
        (
            "a cap inside the span of the work",
            "cap-inside-the-work",
            Some(5),
            "Stopped",
            "MaxRuntimeExceeded",
        ),
        (
            "the cap the member documents as its default",
            "cap-one-day",
            Some(86_400),
            "Completed",
            "Completed",
        ),
        (
            "the cap the member documents as its maximum",
            "cap-28-days",
            Some(2_419_200),
            "Completed",
            "Completed",
        ),
        (
            "no stopping condition at all",
            "cap-absent",
            None,
            "Completed",
            "Completed",
        ),
    ];

    for (case, job, cap, status, secondary) in rows {
        let output = format!("s3://{BUCKET}/cap-output/{job}/");
        let mut body = training_body(
            job,
            &role,
            json!([channel(
                "train",
                &format!("s3://{BUCKET}/caps/train.csv"),
                "S3Prefix"
            )]),
            &output,
        );
        match cap {
            Some(seconds) => body["StoppingCondition"] = json!({ "MaxRuntimeInSeconds": seconds }),
            None => {
                body.as_object_mut()
                    .expect("the request body is an object")
                    .remove("StoppingCondition");
            }
        }
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;

        let (settled, reads) = poll_until_terminal(&emulator, job, case).await;
        let observed = statuses_observed(&reads);
        assert_eq!(
            (
                str_field(&settled, "TrainingJobStatus"),
                str_field(&settled, "SecondaryStatus"),
            ),
            (status.to_string(), secondary.to_string()),
            "case {case:?}: expected the job to end {status}/{secondary}, got {settled}, \
             having been in {observed:?}"
        );
        assert!(
            settled.get("FailureReason").is_none(),
            "case {case:?}: a job the cap ended carries no failure reason, got {settled}"
        );
        let transitions = history(&settled, case);
        // Whatever the cap did to the schedule, every read this poller made inside one of the
        // windows the history reports found the status that window names.
        each_read_reports_the_stage_that_was_due(&transitions, &reads, &settled, case);
        let last = transitions
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a history entry in {settled}"));
        assert_eq!(
            last.0, secondary,
            "case {case:?}: the last history entry names the status the job ended in, \
             got {settled}"
        );

        // A job the cap ended ends exactly at the cap, measured from the instant it was
        // created: the seconds the request asked for and no more.
        let created_at = settled
            .get("CreationTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a CreationTime in {settled}"));
        let ended_at = settled
            .get("TrainingEndTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a TrainingEndTime in {settled}"));
        if secondary == "MaxRuntimeExceeded" {
            let cap = cap.expect("a capped case states its cap") as f64;
            assert!(
                (ended_at - created_at - cap).abs() < 0.001,
                "case {case:?}: expected the job to end {cap} seconds after it was created, \
                 got {settled}"
            );
        } else {
            assert!(
                ended_at > created_at,
                "case {case:?}: expected the job to end after it was created, got {settled}"
            );
        }
    }
}

/// RULE FOUR: the first read a caller can make — straight after `CreateTrainingJob` answers —
/// finds the job `InProgress` in a pre-upload stage, a poller then observes a stage of its
/// work in progress, and the history names every stage it was in, however much work the run
/// the create performed server-side had to do before it could report.
///
/// Both of those are weighed against the job's own schedule rather than against this host's
/// speed: the stages of the run all begin after the instant the caller held the create's
/// answer and each is in effect for a whole span, so the window the first read lands in is a
/// pre-upload one and a poller looking again more often than a span looks inside a stage of
/// the work in progress — and every read made inside one of those windows is held to the
/// stage that window names.
///
/// The rows differ along how much work that is: one small object, several objects of wider
/// records under one prefix, two channels addressed differently, and no channel at all.
#[tokio::test]
async fn the_stages_stay_observable_however_much_work_the_run_did() {
    /// One case: the job's name, the objects it stages, the channels it declares (none at all
    /// when `None`) and the stages its history must name.
    struct Row {
        case: &'static str,
        job: &'static str,
        /// (key, body) of every object this row stages in [`BUCKET`].
        objects: Vec<(String, String)>,
        channels: Option<Value>,
        stages: &'static [&'static str],
    }

    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("stage-observability-role", Some(&read_and_write()))
        .await;

    // A channel whose records are spread over several objects of one prefix, each longer and
    // wider than the single-object case: the run reads every one of them, so this is the row
    // whose work before reporting is the largest.
    let spread: Vec<(String, String)> = (0..12)
        .map(|part| {
            (
                format!("stages/spread/part-{part:02}.csv"),
                csv_records(400, 8),
            )
        })
        .collect();

    let rows = vec![
        Row {
            case: "one object of records",
            job: "stages-one-object",
            objects: vec![("stages/one/train.csv".to_string(), csv_records(6, 2))],
            channels: Some(json!([channel(
                "train",
                &format!("s3://{BUCKET}/stages/one/"),
                "S3Prefix"
            )])),
            stages: &WITH_DOWNLOAD,
        },
        Row {
            case: "a prefix holding several objects of wider records",
            job: "stages-spread-objects",
            objects: spread,
            channels: Some(json!([channel(
                "train",
                &format!("s3://{BUCKET}/stages/spread/"),
                "S3Prefix"
            )])),
            stages: &WITH_DOWNLOAD,
        },
        Row {
            case: "a training channel beside a validation channel",
            job: "stages-two-channels",
            objects: vec![
                ("stages/two/train.csv".to_string(), csv_records(20, 4)),
                (
                    "stages/two/validation/holdout.csv".to_string(),
                    csv_records(10, 4),
                ),
            ],
            channels: Some(json!([
                channel(
                    "train",
                    &format!("s3://{BUCKET}/stages/two/train.csv"),
                    "S3Prefix"
                ),
                channel(
                    "validation",
                    &format!("s3://{BUCKET}/stages/two/validation/"),
                    "S3Prefix"
                ),
            ])),
            stages: &WITH_DOWNLOAD,
        },
        Row {
            case: "no channel at all, so nothing to bring down",
            job: "stages-no-channel",
            objects: Vec::new(),
            channels: None,
            stages: &WITHOUT_DOWNLOAD,
        },
    ];

    for row in rows {
        let case = row.case;
        for (key, body) in &row.objects {
            emulator.given_object(BUCKET, key, body).await;
        }
        let output = format!("s3://{BUCKET}/stage-output/{}/", row.job);
        let mut body = training_body(
            row.job,
            &role,
            row.channels.clone().unwrap_or_else(|| json!([])),
            &output,
        );
        if row.channels.is_none() {
            body.as_object_mut()
                .expect("the request body is an object")
                .remove("InputDataConfig");
        }
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;
        // The first instant a caller can hold this create's answer, taken with no await
        // between it and the answer itself. However much work the run had to do before the
        // create could answer, the stages of that work are laid out from the instant the run
        // REPORTED, so at THIS instant the job is `InProgress` in a pre-upload stage and
        // neither its upload nor its outcome is due — which the job's own history is held to
        // below, pinning the instant the caller got the answer rather than whenever this host
        // got round to reading.
        let answered = epoch_seconds(SystemTime::now());

        // And the first read a caller can make, weighed against that history like every other.
        let (_, first) = describe(&emulator, row.job).await;

        // Polled to the terminal status, correlating what the job reports with what the output
        // location holds: the artifact belongs to the stage that uploads it, so a poll that
        // reports a pre-upload stage must not find the object there.
        let artifact = artifact_key(&output, row.job);
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut reads: Vec<Read> = vec![first];
        let settled = loop {
            let (described, read) = describe(&emulator, row.job).await;
            let secondary = read.status.clone();
            reads.push(read);
            if PRE_UPLOAD.contains(&secondary.as_str())
                && emulator.object_bytes(BUCKET, &artifact).await.is_some()
            {
                // The stage may have fallen due between the describe above and the listing, so
                // the job is read again: an object beside a job a FRESH read still reports in a
                // pre-upload stage is one that appeared too early.
                let (again, read) = describe(&emulator, row.job).await;
                let still = read.status.clone();
                reads.push(read);
                assert!(
                    !PRE_UPLOAD.contains(&still.as_str()),
                    "case {case:?}: the artifact {artifact} appeared while the job was still in \
                     the pre-upload stage {still:?}, got {again}"
                );
            }
            if TERMINAL.contains(&str_field(&described, "TrainingJobStatus").as_str()) {
                break described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the job must reach a terminal status, last read {described}, \
                 having been in {:?}",
                statuses_observed(&reads)
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        let observed = statuses_observed(&reads);
        assert_eq!(
            (
                str_field(&settled, "TrainingJobStatus"),
                str_field(&settled, "SecondaryStatus"),
            ),
            ("Completed".to_string(), "Completed".to_string()),
            "case {case:?}: expected the job to complete, got {settled}, having been in \
             {observed:?}"
        );
        // And the artifact the completed job names is an object Amazon S3 holds: the stage
        // that uploads it is over.
        assert!(
            emulator.object_bytes(BUCKET, &artifact).await.is_some(),
            "case {case:?}: expected {artifact} to name an object Amazon S3 holds once the job \
             completed, got {settled}"
        );
        // Every status observed — the first read's included — belongs to this job's own work.
        assert!(
            observed
                .iter()
                .all(|status| row.stages.contains(&status.as_str()) || status == "Completed"),
            "case {case:?}: expected every observation to be a stage of this job's work {:?}, \
             having been in {observed:?}",
            row.stages
        );

        // The history names the stage the job was accepted in, every stage its run went
        // through and the status it ended in, in that order, each beginning after the one
        // before it — and the first of them when the job was created.
        let transitions = history(&settled, case);
        // However much work the run did, each stage of it is still ahead of the instant this
        // create answered and is in effect for a whole span, one of them being a stage of the
        // work in progress — so the first read a caller can make finds a pre-upload stage and
        // a poller looking again more often than a span observes the work going on. And every
        // read this poller did make inside one of those windows reported that very stage.
        stages_are_laid_out_after_the_create_answered(&transitions, answered, &settled, case);
        each_read_reports_the_stage_that_was_due(&transitions, &reads, &settled, case);
        let expected: Vec<String> = row
            .stages
            .iter()
            .map(|stage| (*stage).to_string())
            .chain(["Completed".to_string()])
            .collect();
        assert_eq!(
            transitions
                .iter()
                .map(|(status, ..)| status.clone())
                .collect::<Vec<_>>(),
            expected,
            "case {case:?}: expected the history to name every stage of the work, got {settled}"
        );
        let created_at = settled
            .get("CreationTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a CreationTime in {settled}"));
        assert_eq!(
            transitions.first().map(|(_, start, ..)| *start),
            Some(created_at),
            "case {case:?}: the job was accepted in its first stage, so that entry begins when \
             the job was created, got {settled}"
        );
        for (index, pair) in transitions.windows(2).enumerate() {
            assert!(
                pair[1].1 > pair[0].1,
                "case {case:?}: entry {index} ({:?}) must begin before the next one ({:?}), \
                 got {settled}",
                pair[0].0,
                pair[1].0
            );
        }
    }
}
