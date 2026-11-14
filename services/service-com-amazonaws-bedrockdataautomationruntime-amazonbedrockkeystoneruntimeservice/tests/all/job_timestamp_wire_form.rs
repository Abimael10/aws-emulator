// SPDX-License-Identifier: CC0-1.0

//! The wire form of the two job instants `GetDataAutomationStatus` reports.
//!
//! THE RULE UNDER TEST: a `Timestamp` member whose model carries
//! `@timestampFormat("date-time")` goes onto the wire as an RFC 3339 date-time STRING, and a
//! timestamp member the model annotates with no format keeps the protocol default. The two
//! members this service declares — `GetDataAutomationStatusResponse$jobSubmissionTime` and
//! `$jobCompletionTime`, both `{"target": "Timestamp", "timestampFormat": "date-time"}` per
//! `smithy_query(query="shape", shape="GetDataAutomationStatusResponse")` — are therefore
//! strings, never the epoch-seconds numbers the protocol default would render.
//!
//! The rows below span the dimension the encoding has to hold over — the INSTANT: whole
//! seconds and sub-second fractions, instants before and long after the recorded one, one
//! past the 32-bit epoch, a job still running (so the optional completion instant is absent)
//! and a job that failed. No row's value is the recorded run's: the rule is about the type
//! and the round trip, not about any one instant.
//!
//! The second and third tests pin the member schemas the JSON codec reads that decision off,
//! because this shape's schema is hand-written (see
//! `src/ops/types/get_data_automation_status_types.rs`) and so can drift from the model: the
//! formats the model declares, and the wire name and member index of every member in the
//! order the shape declares them.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_smithy_schema::traits::TimestampFormat;
use aws_smithy_schema::Schema;
use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomationruntime_amazonbedrockkeystoneruntimeservice as bda;
use smithy_server_runtime::MultiServiceRouter;

use bda::ops::types::GetDataAutomationStatusOutput;
use bda::state::{AutomationJobStatus, Invocation};

use crate::support::{call, job_metadata_uri, PROFILE_ARN, REGION};

/// The prefix every seeded submission below writes its output under.
const OUTPUT_PREFIX: &str = "s3://out-bucket/results/";

/// The asset every seeded submission below points at.
const INPUT_ASSET: &str = "s3://in-bucket/assets/doc.pdf";

/// How far the parsed instant may sit from the stored one: the stored instant travels
/// through `aws_smithy_types::DateTime`, whose `date-time` rendering carries sub-second
/// precision, so a millisecond of slack is enough to let an exact instant through and far too
/// little to let a dropped fraction pass.
const TOLERANCE_SECONDS: f64 = 0.001;

/// One seeded job: the case name, the id its ARN is built around, when it was submitted, when
/// it finished (`None` while it is still running) and the status it holds.
struct Row {
    case: &'static str,
    id: &'static str,
    submitted: SystemTime,
    completed: Option<SystemTime>,
    status: AutomationJobStatus,
}

/// `SystemTime` for a number of seconds since the epoch, plus a number of milliseconds.
fn at(seconds: u64, millis: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(millis)
}

/// The jobs the wire assertions run over. Every instant is written here, so no row depends on
/// the host's clock — except the one still-running row, whose submission sits ahead of the
/// clock precisely so that NO lifecycle transition is owed to it while the test runs and its
/// completion instant stays absent.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "whole seconds",
            id: "whole-seconds",
            submitted: at(1_700_000_000, 0),
            completed: Some(at(1_700_000_001, 0)),
            status: AutomationJobStatus::Success,
        },
        Row {
            case: "sub-second fractions on both instants",
            id: "fractional-millis",
            submitted: at(1_762_000_000, 790),
            completed: Some(at(1_762_000_002, 40)),
            status: AutomationJobStatus::Success,
        },
        Row {
            case: "one second after the epoch",
            id: "epoch-adjacent",
            submitted: at(1, 0),
            completed: Some(at(1, 500)),
            status: AutomationJobStatus::Success,
        },
        Row {
            case: "a leap-day instant",
            id: "leap-day-2024",
            submitted: at(1_709_164_800, 0),
            completed: Some(at(1_709_164_803, 250)),
            status: AutomationJobStatus::Success,
        },
        Row {
            case: "past the 32-bit epoch",
            id: "beyond-2038",
            submitted: at(2_200_000_000, 125),
            completed: Some(at(2_200_000_007, 875)),
            status: AutomationJobStatus::ClientError,
        },
        Row {
            case: "still running, so no completion instant",
            id: "still-running",
            // Ahead of the clock: the job's first transition is not owed yet, whatever the
            // host does while this test runs.
            submitted: SystemTime::now() + Duration::from_secs(3_600),
            completed: None,
            status: AutomationJobStatus::Created,
        },
    ]
}

/// A router serving only this service, and the engine its store lives in.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bda::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

/// Write one row's job into this service's store and answer its ARN.
///
/// The record is written rather than submitted so that the instants under test are the ones
/// the row names: a submission's instants are the clock's.
fn seed(engine: &engine::InMemoryEngine, row: &Row) -> String {
    let arn = format!(
        "arn:aws:bedrock:{REGION}:000000000000:data-automation-invocation/{}",
        row.id
    );
    let mut invocation = Invocation::new(
        arn.clone(),
        row.id.to_string(),
        INPUT_ASSET.to_string(),
        OUTPUT_PREFIX.to_string(),
        PROFILE_ARN.to_string(),
        row.submitted,
    );
    invocation.status = row.status;
    invocation.job_completion_time = row.completed;
    if row.status == AutomationJobStatus::ClientError {
        invocation.error_type = Some(bda::state::INPUT_ASSET_ABSENT_ERROR_TYPE.to_string());
        invocation.error_message = Some(bda::state::INPUT_ASSET_ABSENT_MESSAGE.to_string());
    }
    let state = bda::state::service_state(engine).expect("this service's store on the engine");
    assert!(
        state
            .lock()
            .admit_invocation(invocation, SystemTime::now()),
        "case {:?}: the store must admit the seeded job",
        row.case
    );
    arn
}

/// Assert that one member of `body` is a modelled `date-time`: a JSON STRING that parses with
/// `Format::DateTime`, carries the UTC designator, and names `want`. A JSON NUMBER here — the
/// protocol default a member schema reporting no format falls back to — is the defect these
/// rows pin.
fn assert_date_time(case: &str, body: &Value, member: &str, want: SystemTime) {
    let raw = body
        .get(member)
        .unwrap_or_else(|| panic!("case {case:?}: no {member:?} in {body}"));
    let text = raw.as_str().unwrap_or_else(|| {
        panic!(
            "case {case:?}: {member:?} must be an RFC 3339 date-time string, got {raw} in {body}"
        )
    });
    assert!(
        text.contains('T') && text.ends_with('Z'),
        "case {case:?}: {member:?} must be RFC 3339 in UTC, got {text:?}"
    );
    let parsed = DateTime::from_str(text, Format::DateTime)
        .unwrap_or_else(|error| panic!("case {case:?}: {member:?} = {text:?} is not a date-time: {error}"));
    let want_seconds = want
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    assert!(
        (parsed.as_secs_f64() - want_seconds).abs() <= TOLERANCE_SECONDS,
        "case {case:?}: {member:?} = {text:?} parses to {} seconds, expected {want_seconds}",
        parsed.as_secs_f64()
    );
}

#[tokio::test]
async fn both_job_instants_are_served_as_date_time_strings() {
    let (engine, router) = emulator();

    for row in rows() {
        let case = row.case;
        let arn = seed(&engine, &row);
        let answer = call(
            &router,
            "GetDataAutomationStatus",
            json!({"invocationArn": arn}),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from GetDataAutomationStatus, got {} / {}",
            answer.status, answer.body
        );
        let body = &answer.body;

        assert_date_time(case, body, "jobSubmissionTime", row.submitted);
        match row.completed {
            Some(completed) => assert_date_time(case, body, "jobCompletionTime", completed),
            None => assert!(
                body.get("jobCompletionTime").is_none_or(Value::is_null),
                "case {case:?}: a job with no completion instant must report none, got {body}"
            ),
        }

        // The members beside them keep the types the model gives them, so the hand-written
        // serializer this shape now uses writes the whole response and not only its two
        // timestamps.
        assert_eq!(
            body["status"].as_str(),
            Some(row.status.as_wire()),
            "case {case:?}: status must be the stored job status as a string, got {body}"
        );
        assert_eq!(
            body["outputConfiguration"]["s3Uri"].as_str(),
            Some(job_metadata_uri(OUTPUT_PREFIX, &arn).as_str()),
            "case {case:?}: the reported output location must be this job's metadata document, \
             got {body}"
        );
        let want_duration = row.completed.map(|completed| {
            i64::try_from(
                completed
                    .duration_since(row.submitted)
                    .expect("completion follows submission")
                    .as_secs(),
            )
            .expect("whole seconds of a job's duration")
        });
        assert_eq!(
            body["jobDurationInSeconds"].as_i64(),
            want_duration,
            "case {case:?}: jobDurationInSeconds must be the whole seconds between the two \
             instants, got {body}"
        );
        if row.status == AutomationJobStatus::ClientError {
            assert_eq!(
                body["errorType"].as_str(),
                Some(bda::state::INPUT_ASSET_ABSENT_ERROR_TYPE),
                "case {case:?}: a failed job reports its stored errorType, got {body}"
            );
            assert_eq!(
                body["errorMessage"].as_str(),
                Some(bda::state::INPUT_ASSET_ABSENT_MESSAGE),
                "case {case:?}: a failed job reports its stored errorMessage, got {body}"
            );
        } else {
            assert!(
                body.get("errorType").is_none_or(Value::is_null)
                    && body.get("errorMessage").is_none_or(Value::is_null),
                "case {case:?}: a job that did not fail reports no error members, got {body}"
            );
        }
    }
}

/// The member schema the JSON codec reads the wire form off: each member the model marks
/// `@timestampFormat("date-time")` reports `date-time`, and a member the model annotates with
/// no format reports none at all.
///
/// This is what fails if this shape goes back to the vendored codegen's output, which drops
/// the trait.
#[test]
fn every_modelled_date_time_member_schema_carries_the_date_time_format() {
    // Source: `smithy_query(query="shape", shape="GetDataAutomationStatusResponse")`, which
    // reports `timestampFormat: date-time` on exactly these two members — the only
    // `Timestamp` members this service's model declares.
    for (case, schema) in [
        (
            "GetDataAutomationStatusResponse$jobSubmissionTime",
            &GetDataAutomationStatusOutput::M_JOB_SUBMISSION_TIME,
        ),
        (
            "GetDataAutomationStatusResponse$jobCompletionTime",
            &GetDataAutomationStatusOutput::M_JOB_COMPLETION_TIME,
        ),
    ] {
        let format = schema.timestamp_format().map(|carried| carried.format());
        assert!(
            matches!(format, Some(TimestampFormat::DateTime)),
            "case {case:?}: the member schema must carry @timestampFormat(date-time), got \
             {format:?}"
        );
    }

    // The model names no format for any other member of the shape, so none of them carries
    // one: the fix put a format exactly where the model declares one.
    for (case, schema) in [
        (
            "GetDataAutomationStatusResponse$status",
            &GetDataAutomationStatusOutput::M_STATUS,
        ),
        (
            "GetDataAutomationStatusResponse$errorType",
            &GetDataAutomationStatusOutput::M_ERROR_TYPE,
        ),
        (
            "GetDataAutomationStatusResponse$errorMessage",
            &GetDataAutomationStatusOutput::M_ERROR_MESSAGE,
        ),
        (
            "GetDataAutomationStatusResponse$outputConfiguration",
            &GetDataAutomationStatusOutput::M_OUTPUT_CONFIGURATION,
        ),
        (
            "GetDataAutomationStatusResponse$jobDurationInSeconds",
            &GetDataAutomationStatusOutput::M_JOB_DURATION_IN_SECONDS,
        ),
    ] {
        let format = schema.timestamp_format().map(|carried| carried.format());
        assert!(
            format.is_none(),
            "case {case:?}: the model annotates no format for this member, got {format:?}"
        );
    }
}

/// The hand-written member list cannot drift from the model: every member schema reports the
/// wire name, the shape type and the member index the model gives it, in the order the shape
/// declares them.
#[test]
fn the_hand_written_member_list_matches_the_models_order() {
    // Source: `smithy_query(query="shape", shape="GetDataAutomationStatusResponse")`, whose
    // member order is the order below; the enum member is string-encoded, as every Smithy
    // `enum` is.
    let want: Vec<(&str, ::aws_smithy_schema::ShapeType)> = vec![
        ("status", ::aws_smithy_schema::ShapeType::String),
        ("errorType", ::aws_smithy_schema::ShapeType::String),
        ("errorMessage", ::aws_smithy_schema::ShapeType::String),
        (
            "outputConfiguration",
            ::aws_smithy_schema::ShapeType::Structure,
        ),
        ("jobSubmissionTime", ::aws_smithy_schema::ShapeType::Timestamp),
        ("jobCompletionTime", ::aws_smithy_schema::ShapeType::Timestamp),
        ("jobDurationInSeconds", ::aws_smithy_schema::ShapeType::Integer),
    ];

    let members: Vec<&Schema> = GetDataAutomationStatusOutput::SCHEMA.members().to_vec();
    assert_eq!(
        members.len(),
        want.len(),
        "the shape declares {} members, the schema names {}",
        want.len(),
        members.len()
    );
    for (index, ((name, shape_type), member)) in want.iter().zip(members).enumerate() {
        assert_eq!(
            member.member_name(),
            Some(*name),
            "member {index}: expected the wire name {name:?}, got {:?}",
            member.member_name()
        );
        assert_eq!(
            member.member_index(),
            Some(index),
            "member {name:?}: expected member index {index}, got {:?}",
            member.member_index()
        );
        assert_eq!(
            member.shape_type(),
            *shape_type,
            "member {name:?}: expected shape type {shape_type:?}, got {:?}",
            member.shape_type()
        );
    }
}
