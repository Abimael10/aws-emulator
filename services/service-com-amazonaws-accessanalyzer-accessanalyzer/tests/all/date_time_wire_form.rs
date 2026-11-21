// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of a timestamp, in both directions, driven over the real router.
//!
//! RULE: every timestamp member of this service targets `com.amazonaws.accessanalyzer#Timestamp`,
//! and that shape carries `@timestampFormat("date-time")`
//! (`smithy_query(query="shape",shape="Timestamp,CloudTrailDetails,JobDetails,CloudTrailProperties")`),
//! so an ISO 8601 / RFC 3339 date-time STRING is what a request may carry for one — the only
//! form an AWS SDK sends for such a member — and what a response reports for one. The
//! epoch-seconds NUMBER the `restJson1` protocol defaults to is not the modelled form, and a
//! value that is no date-time at all is refused with the modelled `ValidationException`.
//!
//! The rows span what the rule generalises over: the shape of the instant sent (whole second,
//! sub-second digits, a leap day, a far-future instant), which of a pair of range members the
//! request carries, and — for the reporting half — the member paths a read reports an instant
//! at, across three operations. The epoch-seconds number is a row of its own, because the JSON
//! codec reads a number as epoch seconds whatever format the member declares, so accepting the
//! date-time string must not stop accepting it.

use serde_json::{json, Value};

use crate::harness::{member, role_arn, trail_arn, Emulator, Reply};

/// The status a success that reports content carries.
const OK: u16 = 200;

/// The status every refusal of these operations carries.
const BAD_REQUEST: u16 = 400;

/// The error code of that refusal.
const VALIDATION: &str = "ValidationException";

/// A `StartPolicyGeneration` body whose analysed range carries the two values given, each as
/// the JSON the row chose (a date-time string, or the protocol's epoch-seconds number).
fn start_body(principal: &str, trail: &str, start_time: Value, end_time: Option<Value>) -> Value {
    let mut details = json!({
        "trails": [{ "cloudTrailArn": trail_arn(trail) }],
        "accessRole": role_arn("AccessAnalyzerMonitorServiceRole"),
        "startTime": start_time,
    });
    if let Some(end_time) = end_time {
        details["endTime"] = end_time;
    }
    json!({
        "policyGenerationDetails": { "principalArn": principal },
        "cloudTrailDetails": details,
    })
}

/// One timestamp member read off the wire, as epoch seconds, ONLY when it is reported in the
/// modelled `date-time` string form; `None` for an absent member, and for one reported in any
/// other JSON type — which is what makes an assertion on this an assertion about the form.
fn date_time_secs(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::String(text) => ::aws_smithy_types::DateTime::from_str(
            text,
            ::aws_smithy_types::date_time::Format::DateTime,
        )
        .ok()
        .map(|instant| instant.as_secs_f64()),
        _ => None,
    }
}

/// Whether `value` is reported as the date-time string of the instant `expected_secs`.
fn reports_instant(value: Option<&Value>, expected_secs: f64) -> bool {
    date_time_secs(value).is_some_and(|secs| (secs - expected_secs).abs() < 1e-6)
}

/// The `jobDetails` of a `GetGeneratedPolicy` reply.
fn job_details(case: &str, reply: &Reply) -> Value {
    reply
        .member("jobDetails")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected jobDetails in {reply:?}"))
}

/// The `cloudTrailProperties` of a `GetGeneratedPolicy` reply.
fn cloud_trail_properties(case: &str, reply: &Reply) -> Value {
    let result = reply
        .member("generatedPolicyResult")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected generatedPolicyResult in {reply:?}"));
    let properties = member(&result, "properties")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected properties in {result:?}"));
    member(&properties, "cloudTrailProperties")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected cloudTrailProperties in {properties:?}"))
}

/// One row of the accepted-range table.
struct RangeCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The `startTime` the request carries, as JSON, and the instant it names.
    start_time: Value,
    start_secs: f64,
    /// The `endTime` the request carries, `None` for the documented default ("the current
    /// time"), and the instant it names.
    end_time: Option<Value>,
    end_secs: Option<f64>,
}

/// The ranges a start must accept, and report back at the same instants.
fn range_cases() -> Vec<RangeCase> {
    vec![
        RangeCase {
            case: "a whole second, as every SDK renders a date-time member",
            start_time: json!("2026-01-01T00:00:00Z"),
            start_secs: 1_767_225_600.0,
            end_time: Some(json!("2026-01-02T00:00:00Z")),
            end_secs: Some(1_767_312_000.0),
        },
        RangeCase {
            case: "sub-second digits on both ends",
            start_time: json!("2026-01-01T00:00:00.25Z"),
            start_secs: 1_767_225_600.25,
            end_time: Some(json!("2026-01-01T12:34:56.5Z")),
            end_secs: Some(1_767_270_896.5),
        },
        RangeCase {
            case: "a leap day, with the end left to its documented default",
            start_time: json!("2024-02-29T23:59:59Z"),
            start_secs: 1_709_251_199.0,
            end_time: None,
            end_secs: None,
        },
        RangeCase {
            case: "an instant far past the one this suite is written at",
            start_time: json!("2099-12-31T23:59:59Z"),
            start_secs: 4_102_444_799.0,
            end_time: Some(json!("2100-01-01T00:00:00Z")),
            end_secs: Some(4_102_444_800.0),
        },
        RangeCase {
            case: "the epoch-seconds number the protocol reads as an instant too",
            start_time: json!(1_767_225_600.0),
            start_secs: 1_767_225_600.0,
            end_time: Some(json!(1_767_312_000.0)),
            end_secs: Some(1_767_312_000.0),
        },
    ]
}

/// RULE: a `date-time` member is ACCEPTED in the RFC 3339 string form the model declares, and
/// the instant it named is REPORTED back in that same form.
#[tokio::test]
async fn a_date_time_range_is_accepted_and_reported_as_an_rfc3339_string() {
    let emulator = Emulator::new();
    for RangeCase {
        case,
        start_time,
        start_secs,
        end_time,
        end_secs,
    } in range_cases()
    {
        let principal = role_arn(&format!(
            "dtwf_{}",
            case.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
        ));
        let body = start_body(&principal, "dtwf-trail", start_time, end_time);
        let started = emulator.start_policy_generation(body).await;
        assert_eq!(
            (started.status, started.code()),
            (OK, ""),
            "case {case:?}: expected StartPolicyGeneration to answer {OK}, got {started:?}"
        );
        let job_id = started
            .member("jobId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !job_id.is_empty(),
            "case {case:?}: expected a jobId in the response, got {started:?}"
        );

        let read = emulator.get_generated_policy(&job_id, "").await;
        assert_eq!(
            (read.status, read.code()),
            (OK, ""),
            "case {case:?}: expected GetGeneratedPolicy to answer {OK}, got {read:?}"
        );
        let details = job_details(case, &read);
        let cloud_trail = cloud_trail_properties(case, &read);

        assert!(
            reports_instant(member(&cloud_trail, "startTime"), start_secs),
            "case {case:?}: expected startTime reported as the date-time string of \
             {start_secs}, got {:?}",
            member(&cloud_trail, "startTime")
        );
        match end_secs {
            Some(end_secs) => assert!(
                reports_instant(member(&cloud_trail, "endTime"), end_secs),
                "case {case:?}: expected endTime reported as the date-time string of \
                 {end_secs}, got {:?}",
                member(&cloud_trail, "endTime")
            ),
            // "If this is not included in the request, the default value is the current
            // time": the instant the job started, which is reported in the same form.
            None => assert!(
                date_time_secs(member(&cloud_trail, "endTime")).is_some()
                    && date_time_secs(member(&cloud_trail, "endTime"))
                        == date_time_secs(member(&details, "startedOn")),
                "case {case:?}: expected an omitted endTime to default to the date-time the \
                 job started at ({:?}), got {:?}",
                member(&details, "startedOn"),
                member(&cloud_trail, "endTime")
            ),
        }
        assert!(
            date_time_secs(member(&details, "startedOn")).is_some(),
            "case {case:?}: expected startedOn reported as a date-time string, got {details:?}"
        );
    }
}

/// RULE's negative side: a value that is no `date-time` at all is refused with the modelled
/// `ValidationException`, at the request boundary, rather than being read as some other
/// instant.
///
/// The rows span the ways a string can fail to be one: text that is no timestamp, epoch
/// seconds written as a string, a date with no time of day, and a date-time carrying an offset
/// instead of `Z`.
#[tokio::test]
async fn a_value_that_is_no_date_time_is_refused() {
    let cases: [(&str, Value); 4] = [
        ("text that names no instant", json!("not-a-date")),
        ("epoch seconds written as a string", json!("1767225600")),
        ("a date with no time of day", json!("2026-01-01")),
        ("a date-time carrying an offset", json!("2026-01-01T00:00:00+01:00")),
    ];
    let emulator = Emulator::new();
    for (case, start_time) in cases {
        let body = start_body(&role_arn("dtwf_refused"), "dtwf-refused-trail", start_time, None);
        let reply = emulator.start_policy_generation(body).await;
        assert_eq!(
            (reply.status, reply.code()),
            (BAD_REQUEST, VALIDATION),
            "case {case:?}: expected {BAD_REQUEST} {VALIDATION}, got {reply:?}"
        );
    }
}

/// RULE, on the reporting half alone: EVERY member a read reports an instant at reports it as
/// an RFC 3339 date-time string, across the operations that report one.
#[tokio::test]
async fn every_reported_instant_is_an_rfc3339_string() {
    let emulator = Emulator::new();

    // An analyzer, whose `createdAt` is one of this service's date-time members.
    let created = emulator
        .create_analyzer(json!({ "analyzerName": "dtwf_analyzer", "type": "ACCOUNT" }))
        .await;
    assert_eq!(
        (created.status, created.code()),
        (OK, ""),
        "CreateAnalyzer must succeed, got {created:?}"
    );
    let analyzer = emulator.get_analyzer("dtwf_analyzer").await;
    assert_eq!(
        (analyzer.status, analyzer.code()),
        (OK, ""),
        "GetAnalyzer must succeed, got {analyzer:?}"
    );

    let principal = role_arn("dtwf_reported");
    let started = emulator
        .start_policy_generation(start_body(
            &principal,
            "dtwf-reported-trail",
            json!("2026-03-04T05:06:07Z"),
            Some(json!("2026-03-05T05:06:07Z")),
        ))
        .await;
    assert_eq!(
        (started.status, started.code()),
        (OK, ""),
        "StartPolicyGeneration must succeed, got {started:?}"
    );
    let job_id = started
        .member("jobId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let read = emulator.get_generated_policy(&job_id, "").await;
    let listed = emulator
        .list_policy_generations(&format!("?principalArn={principal}"))
        .await;
    let listed_job = listed
        .member("policyGenerations")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| panic!("ListPolicyGenerations must report the job, got {listed:?}"));

    // (the member path, the value the read reported at it)
    let analyzer_summary = analyzer
        .member("analyzer")
        .cloned()
        .unwrap_or_else(|| panic!("GetAnalyzer must report an analyzer, got {analyzer:?}"));
    let details = job_details("reported", &read);
    let cloud_trail = cloud_trail_properties("reported", &read);
    let reported: [(&str, Option<&Value>); 5] = [
        ("GetAnalyzer analyzer.createdAt", member(&analyzer_summary, "createdAt")),
        ("GetGeneratedPolicy jobDetails.startedOn", member(&details, "startedOn")),
        (
            "GetGeneratedPolicy cloudTrailProperties.startTime",
            member(&cloud_trail, "startTime"),
        ),
        (
            "GetGeneratedPolicy cloudTrailProperties.endTime",
            member(&cloud_trail, "endTime"),
        ),
        (
            "ListPolicyGenerations policyGenerations[].startedOn",
            member(&listed_job, "startedOn"),
        ),
    ];
    for (path, value) in reported {
        assert!(
            date_time_secs(value).is_some(),
            "case {path:?}: expected an RFC 3339 date-time string, got {value:?}"
        );
    }
}
