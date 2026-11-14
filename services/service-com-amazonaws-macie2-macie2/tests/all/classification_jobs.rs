// SPDX-License-Identifier: CC0-1.0

//! The Macie classification-job plane — defining a sensitive data discovery
//! job and reading it back — end to end over the wire.
//!
//! Each case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called
//! directly.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * `CreateClassificationJob` PERSISTS every member of the request under a
//!   server-assigned id and reports that id with the job's ARN, so
//!   `DescribeClassificationJob` reports the stored job: the settings as sent,
//!   and the documented default for each one omitted;
//! * a job whose run starts when it is created is `RUNNING`, and a recurring
//!   job configured not to analyze existing objects has not run, so it is
//!   `IDLE`;
//! * a member a read reports only under some configurations — the managed data
//!   identifier list, the recurrence pattern, the custom data identifier list —
//!   is absent under every other one;
//! * a request outside a documented bound is refused 400 `ValidationException`
//!   and stores nothing, not even the client token it carried;
//! * a retried `clientToken` replays the job it minted rather than minting a
//!   second one;
//! * a job belongs to the region its create was signed for, and both
//!   operations are refused 403 until Macie is enabled there;
//! * a read of a job the region holds none of is refused 404
//!   `ResourceNotFoundException`, and a read changes nothing a later read sees.

use smithy_server_runtime::MultiServiceRouter;

use super::macie_session_and_custom_data_identifiers::{
    call_in, emulator, enable_in, error_code, error_message, reported_instant,
};

/// The region most cases sign for; it must reach every minted ARN.
const REGION: &str = "us-west-2";

/// A second region, for the cases that pin the per-region scope of a job.
const OTHER_REGION: &str = "eu-west-1";

/// The job collection's URI, from `CreateClassificationJob`'s `@http` binding.
const JOBS_URI: &str = "/jobs";

/// The `JobStatus` a job with a run in progress carries. An entry of the
/// modelled `JobStatus` enum
/// (`smithy_query(query="shape",shape="JobStatus")`).
const RUNNING: &str = "RUNNING";

/// The `JobStatus` of a recurring job with no run in progress and its next
/// scheduled run pending. An entry of that same enum.
const IDLE: &str = "IDLE";

/// The `JobStatus` of a one-time job whose run has finished. An entry of that
/// same enum.
const COMPLETE: &str = "COMPLETE";

/// The `ManagedDataIdentifierSelector` a job gets when the request names none.
/// Source: the member doc of `CreateClassificationJobRequest$managedDataIdentifierSelector`
/// — "RECOMMENDED (default) - Use the recommended set of managed data
/// identifiers."
const DEFAULT_SELECTOR: &str = "RECOMMENDED";

/// The sampling depth a job runs at when the request asks for no shallower one:
/// every eligible object, which is 100 percent.
const DEFAULT_SAMPLING_PERCENTAGE: i64 = 100;

/// A static bucket specification, the form most cases use.
fn buckets() -> serde_json::Value {
    serde_json::json!({
        "bucketDefinitions": [{
            "accountId": ::engine::emulator_account_id(),
            "buckets": ["sensitive-data", "archive"]
        }]
    })
}

/// A create body for a one-time job, with `extra` merged over it.
fn one_time(token: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "clientToken": token,
        "jobType": "ONE_TIME",
        "name": "quarterly-scan",
        "s3JobDefinition": buckets(),
    });
    merge(&mut body, extra);
    body
}

/// A create body for a recurring job with a daily recurrence pattern, with
/// `extra` merged over it.
fn scheduled(token: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "clientToken": token,
        "jobType": "SCHEDULED",
        "name": "nightly-scan",
        "s3JobDefinition": buckets(),
        "scheduleFrequency": { "dailySchedule": {} },
    });
    merge(&mut body, extra);
    body
}

/// Merge the members of `extra` into `body`, so a case states only what it
/// varies.
fn merge(body: &mut serde_json::Value, extra: serde_json::Value) {
    let serde_json::Value::Object(members) = extra else {
        return;
    };
    for (member, value) in members {
        body[member] = value;
    }
}

/// `count` tags, each key `key_chars` characters wide and each value
/// `value_chars` wide, so a case states a tag bound in the terms the bound is
/// documented in.
fn tags(count: usize, key_chars: usize, value_chars: usize) -> serde_json::Value {
    let tags: serde_json::Map<String, serde_json::Value> = (0..count)
        .map(|index| {
            let key = format!("{index}{}", "k".repeat(key_chars.saturating_sub(1)));
            (
                key,
                serde_json::Value::String("v".repeat(value_chars)),
            )
        })
        .collect();
    serde_json::Value::Object(tags)
}

/// Create one job and hand back the id and ARN it reported.
async fn create(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
    case: &str,
) -> (String, String) {
    let (status, reply) = call_in(router, region, "POST", JOBS_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateClassificationJob must answer 200, got {status} with {reply:?}"
    );
    let job_id = reply["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create must report a jobId, got {reply:?}"))
        .to_string();
    let job_arn = reply["jobArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create must report a jobArn, got {reply:?}"))
        .to_string();
    (job_id, job_arn)
}

/// Read one job back.
async fn describe(
    router: &MultiServiceRouter,
    region: &str,
    job_id: &str,
) -> (u16, serde_json::Value) {
    call_in(router, region, "GET", &format!("{JOBS_URI}/{job_id}"), "").await
}

/// Assert `job_id` has the grammar this service issues a resource id in: 22
/// lowercase alphanumeric characters, the width and alphabet the only Macie id
/// grammar the model pins carries (`CreateAllowListResponse$id` is
/// `^[a-z0-9]{22}$`).
fn assert_job_id_grammar(job_id: &str, case: &str) {
    assert_eq!(
        job_id.chars().count(),
        22,
        "case {case:?}: a job id is 22 characters, got {job_id:?}"
    );
    assert!(
        job_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "case {case:?}: a job id is lowercase alphanumeric, got {job_id:?}"
    );
}

/// The ARN a job of `region` carries, built the way every Macie ARN is: the
/// service, the region the call addressed, the emulator account and the
/// resource.
fn expected_arn(region: &str, job_id: &str) -> String {
    format!(
        "arn:aws:macie2:{region}:{}:classification-job/{job_id}",
        ::engine::emulator_account_id()
    )
}

/// One row of the round-trip table: a create body, and what a read of the job
/// it mints must report.
struct Case {
    /// What this row varies, named in every assertion it drives.
    name: &'static str,
    body: serde_json::Value,
    /// The `jobStatus` the job starts in.
    status: &'static str,
    /// Whether a run has started, which decides the run count and whether an
    /// error status for the last run is reported at all.
    run_started: bool,
    /// The members the read must report, with the value each must carry.
    reported: Vec<(&'static str, serde_json::Value)>,
    /// The members the read must NOT report.
    absent: Vec<&'static str>,
}

/// Every member of a create is stored and reported back, the documented default
/// is applied to every member a request omits, and a member that applies only
/// to some configurations is reported only there.
///
/// The rows differ along what the rule generalises over: the job's type and
/// recurrence pattern, whether it analyzes existing objects immediately, the
/// selection type and the identifier lists it names, the bucket specification's
/// form, the sampling depth, and the tags it carries.
#[tokio::test]
async fn a_create_stores_every_member_and_a_read_reports_the_stored_job() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let cases = vec![
        Case {
            name: "a one-time job with nothing optional named",
            body: one_time("t-one-time", serde_json::json!({})),
            status: RUNNING,
            run_started: true,
            reported: vec![
                ("jobType", serde_json::json!("ONE_TIME")),
                ("name", serde_json::json!("quarterly-scan")),
                ("s3JobDefinition", buckets()),
                (
                    "managedDataIdentifierSelector",
                    serde_json::json!(DEFAULT_SELECTOR),
                ),
                (
                    "samplingPercentage",
                    serde_json::json!(DEFAULT_SAMPLING_PERCENTAGE),
                ),
                ("initialRun", serde_json::json!(false)),
            ],
            absent: vec![
                "description",
                "scheduleFrequency",
                "allowListIds",
                "customDataIdentifierIds",
                "managedDataIdentifierIds",
                "tags",
                "userPausedDetails",
            ],
        },
        Case {
            name: "a one-time job with every optional member named",
            body: one_time(
                "t-one-time-full",
                serde_json::json!({
                    "description": "every optional member",
                    "allowListIds": ["allow-one", "allow-two"],
                    "customDataIdentifierIds": ["cdi-one"],
                    "managedDataIdentifierSelector": "INCLUDE",
                    "managedDataIdentifierIds": ["USA_SOCIAL_SECURITY_NUMBER"],
                    "samplingPercentage": 25,
                    "tags": { "team": "security", "tier": "gold" },
                    // A one-time job's `initialRun` is false however the
                    // request words it: "This value is also false for a
                    // one-time job."
                    "initialRun": true,
                }),
            ),
            status: RUNNING,
            run_started: true,
            reported: vec![
                ("description", serde_json::json!("every optional member")),
                ("allowListIds", serde_json::json!(["allow-one", "allow-two"])),
                ("customDataIdentifierIds", serde_json::json!(["cdi-one"])),
                ("managedDataIdentifierSelector", serde_json::json!("INCLUDE")),
                (
                    "managedDataIdentifierIds",
                    serde_json::json!(["USA_SOCIAL_SECURITY_NUMBER"]),
                ),
                ("samplingPercentage", serde_json::json!(25)),
                (
                    "tags",
                    serde_json::json!({ "team": "security", "tier": "gold" }),
                ),
                ("initialRun", serde_json::json!(false)),
            ],
            absent: vec!["scheduleFrequency", "userPausedDetails"],
        },
        Case {
            name: "a recurring job that analyzes existing objects immediately",
            body: scheduled("t-initial-run", serde_json::json!({ "initialRun": true })),
            status: RUNNING,
            run_started: true,
            reported: vec![
                ("jobType", serde_json::json!("SCHEDULED")),
                ("initialRun", serde_json::json!(true)),
                (
                    "scheduleFrequency",
                    serde_json::json!({ "dailySchedule": {} }),
                ),
            ],
            absent: vec!["userPausedDetails"],
        },
        Case {
            name: "a recurring job waiting for its first scheduled run",
            body: scheduled("t-no-initial-run", serde_json::json!({})),
            status: IDLE,
            run_started: false,
            reported: vec![
                ("jobType", serde_json::json!("SCHEDULED")),
                ("initialRun", serde_json::json!(false)),
            ],
            absent: vec!["userPausedDetails", "lastRunErrorStatus"],
        },
        Case {
            name: "a weekly recurrence pattern",
            body: scheduled(
                "t-weekly",
                serde_json::json!({
                    "scheduleFrequency": { "weeklySchedule": { "dayOfWeek": "SATURDAY" } }
                }),
            ),
            status: IDLE,
            run_started: false,
            reported: vec![(
                "scheduleFrequency",
                serde_json::json!({ "weeklySchedule": { "dayOfWeek": "SATURDAY" } }),
            )],
            absent: vec!["userPausedDetails"],
        },
        Case {
            name: "a monthly recurrence pattern on the last documented day",
            body: scheduled(
                "t-monthly",
                serde_json::json!({
                    "scheduleFrequency": { "monthlySchedule": { "dayOfMonth": 31 } }
                }),
            ),
            status: IDLE,
            run_started: false,
            reported: vec![(
                "scheduleFrequency",
                serde_json::json!({ "monthlySchedule": { "dayOfMonth": 31 } }),
            )],
            absent: vec!["userPausedDetails"],
        },
        Case {
            name: "a dynamic bucket specification and a scoped analysis",
            body: one_time(
                "t-dynamic",
                serde_json::json!({
                    "s3JobDefinition": {
                        "bucketCriteria": {
                            "includes": {
                                "and": [{
                                    "simpleCriterion": {
                                        "comparator": "EQ",
                                        "key": "S3_BUCKET_NAME",
                                        "values": ["sensitive-data"]
                                    }
                                }]
                            }
                        },
                        "scoping": {
                            "excludes": {
                                "and": [{
                                    "simpleScopeTerm": {
                                        "comparator": "EQ",
                                        "key": "OBJECT_EXTENSION",
                                        "values": ["log"]
                                    }
                                }]
                            }
                        }
                    }
                }),
            ),
            status: RUNNING,
            run_started: true,
            reported: vec![(
                "s3JobDefinition",
                serde_json::json!({
                    "bucketCriteria": {
                        "includes": {
                            "and": [{
                                "simpleCriterion": {
                                    "comparator": "EQ",
                                    "key": "S3_BUCKET_NAME",
                                    "values": ["sensitive-data"]
                                }
                            }]
                        }
                    },
                    "scoping": {
                        "excludes": {
                            "and": [{
                                "simpleScopeTerm": {
                                    "comparator": "EQ",
                                    "key": "OBJECT_EXTENSION",
                                    "values": ["log"]
                                }
                            }]
                        }
                    }
                }),
            )],
            absent: vec!["userPausedDetails"],
        },
        Case {
            name: "a job using no managed data identifier at all",
            body: one_time(
                "t-none-selector",
                serde_json::json!({
                    "managedDataIdentifierSelector": "NONE",
                    "customDataIdentifierIds": ["cdi-one", "cdi-two"],
                    "samplingPercentage": 0,
                }),
            ),
            status: RUNNING,
            run_started: true,
            reported: vec![
                ("managedDataIdentifierSelector", serde_json::json!("NONE")),
                (
                    "customDataIdentifierIds",
                    serde_json::json!(["cdi-one", "cdi-two"]),
                ),
                ("samplingPercentage", serde_json::json!(0)),
            ],
            // The identifier list a selection type of NONE documents no values
            // for is reported for none either.
            absent: vec!["managedDataIdentifierIds"],
        },
        Case {
            name: "a job excluding named managed data identifiers",
            body: one_time(
                "t-exclude-selector",
                serde_json::json!({
                    "managedDataIdentifierSelector": "EXCLUDE",
                    "managedDataIdentifierIds": ["ADDRESS", "NAME"],
                }),
            ),
            status: RUNNING,
            run_started: true,
            reported: vec![
                ("managedDataIdentifierSelector", serde_json::json!("EXCLUDE")),
                (
                    "managedDataIdentifierIds",
                    serde_json::json!(["ADDRESS", "NAME"]),
                ),
            ],
            absent: vec!["customDataIdentifierIds"],
        },
        Case {
            name: "a job using every managed data identifier",
            body: one_time(
                "t-all-selector",
                serde_json::json!({ "managedDataIdentifierSelector": "ALL" }),
            ),
            status: RUNNING,
            run_started: true,
            reported: vec![("managedDataIdentifierSelector", serde_json::json!("ALL"))],
            // ALL names no identifier explicitly, so none is reported.
            absent: vec!["managedDataIdentifierIds"],
        },
    ];

    let mut minted: Vec<String> = Vec::new();
    for case in cases {
        let name = case.name;
        let (job_id, job_arn) = create(&router, REGION, &case.body, name).await;
        assert_job_id_grammar(&job_id, name);
        assert_eq!(
            job_arn,
            expected_arn(REGION, &job_id),
            "case {name:?}: expected the job's ARN to name the region and the emulator account, \
             got {job_arn:?}"
        );
        assert!(
            !minted.contains(&job_id),
            "case {name:?}: {job_id:?} was already minted for another job"
        );
        minted.push(job_id.clone());

        let (status, reply) = describe(&router, REGION, &job_id).await;
        assert_eq!(
            status, 200,
            "case {name:?}: DescribeClassificationJob must answer 200, got {status} with {reply:?}"
        );
        assert_eq!(
            reply["jobId"],
            serde_json::json!(job_id),
            "case {name:?}: expected the job the create minted, got {reply:?}"
        );
        assert_eq!(
            reply["jobArn"],
            serde_json::json!(job_arn),
            "case {name:?}: expected the ARN the create reported, got {reply:?}"
        );
        // The status the job starts in, or — for a job whose run started, if
        // this read landed past the instant that run is due to end — the end
        // the clock owes it. Which of the two shows up depends on how fast the
        // host ran this table, so both are accepted here; the start and the end
        // are each pinned exactly by their own tests
        // (`a_read_reports_the_end_the_clock_owes_a_started_run` and the state
        // module's due-instant table).
        let settled = if case.body["jobType"] == serde_json::json!("SCHEDULED") {
            IDLE
        } else {
            COMPLETE
        };
        let reported_status = reply["jobStatus"].as_str().unwrap_or_default();
        let allowed: Vec<&str> = if case.run_started {
            vec![case.status, settled]
        } else {
            vec![case.status]
        };
        assert!(
            allowed.contains(&reported_status),
            "case {name:?}: expected jobStatus to be one of {allowed:?}, got \
             {reported_status:?}"
        );
        assert_eq!(
            reply["clientToken"],
            case.body["clientToken"],
            "case {name:?}: expected the clientToken the create carried, got {:?}",
            reply["clientToken"]
        );
        for (member, expected) in &case.reported {
            assert_eq!(
                &reply[member], expected,
                "case {name:?}: expected {member} to be {expected:?}, got {:?}",
                reply[member]
            );
        }
        for member in &case.absent {
            assert!(
                reply[member].is_null(),
                "case {name:?}: expected {member} to be absent, got {:?}",
                reply[member]
            );
        }

        // The run count is the number of runs the service started for the job.
        let expected_runs = if case.run_started { 1.0 } else { 0.0 };
        assert_eq!(
            reply["statistics"]["numberOfRuns"].as_f64(),
            Some(expected_runs),
            "case {name:?}: expected {expected_runs} run(s), got {:?}",
            reply["statistics"]
        );
        // Nothing in this emulator accesses the buckets a job addresses, so no
        // error status for a run is reported either way.
        assert!(
            reply["lastRunErrorStatus"].is_null(),
            "case {name:?}: expected no lastRunErrorStatus, got {:?}",
            reply["lastRunErrorStatus"]
        );

        // Both instants are reported in the form the model pins, and the run
        // instant of a job that has not run is the instant it was created.
        let created_at = reported_instant(name, "createdAt", &reply);
        let last_run_time = reported_instant(name, "lastRunTime", &reply);
        assert_eq!(
            created_at, last_run_time,
            "case {name:?}: a job whose run starts when it is created, and one that has not \
             run, both report the create instant as lastRunTime, got {created_at} and \
             {last_run_time}"
        );
    }
}

/// A create outside a documented bound is refused 400 `ValidationException`,
/// and the refusal stores nothing: the client token it carried still mints a
/// job afterwards.
///
/// The rows are the same call with ONE member taken outside its documented
/// bound, spanning each bound the create's own documentation states.
#[tokio::test]
async fn a_create_outside_a_documented_bound_is_refused_and_stores_nothing() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let token = "t-refused";
    let refused: Vec<(&str, serde_json::Value)> = vec![
        ("an empty name", one_time(token, serde_json::json!({ "name": "" }))),
        (
            "a name one past its maximum",
            one_time(token, serde_json::json!({ "name": "n".repeat(501) })),
        ),
        (
            "a description one past its maximum",
            one_time(token, serde_json::json!({ "description": "d".repeat(201) })),
        ),
        (
            "a sampling depth past the percentage range",
            one_time(token, serde_json::json!({ "samplingPercentage": 101 })),
        ),
        (
            "a negative sampling depth",
            one_time(token, serde_json::json!({ "samplingPercentage": -1 })),
        ),
        (
            "a one-time job carrying a recurrence pattern",
            one_time(
                token,
                serde_json::json!({ "scheduleFrequency": { "dailySchedule": {} } }),
            ),
        ),
        (
            "a recurring job carrying no recurrence pattern",
            serde_json::json!({
                "clientToken": token,
                "jobType": "SCHEDULED",
                "name": "nightly-scan",
                "s3JobDefinition": buckets(),
            }),
        ),
        (
            "a recurrence pattern naming nothing",
            scheduled(token, serde_json::json!({ "scheduleFrequency": {} })),
        ),
        (
            "two recurrence patterns at once",
            scheduled(
                token,
                serde_json::json!({
                    "scheduleFrequency": {
                        "dailySchedule": {},
                        "weeklySchedule": { "dayOfWeek": "MONDAY" }
                    }
                }),
            ),
        ),
        (
            "a monthly pattern one day before the first documented day",
            scheduled(
                token,
                serde_json::json!({ "scheduleFrequency": { "monthlySchedule": { "dayOfMonth": 0 } } }),
            ),
        ),
        (
            "a monthly pattern one day past the last documented day",
            scheduled(
                token,
                serde_json::json!({ "scheduleFrequency": { "monthlySchedule": { "dayOfMonth": 32 } } }),
            ),
        ),
        (
            "no bucket specification at all",
            one_time(token, serde_json::json!({ "s3JobDefinition": {} })),
        ),
        (
            "both a static and a dynamic bucket specification",
            one_time(
                token,
                serde_json::json!({
                    "s3JobDefinition": {
                        "bucketDefinitions": [{
                            "accountId": ::engine::emulator_account_id(),
                            "buckets": ["sensitive-data"]
                        }],
                        "bucketCriteria": {}
                    }
                }),
            ),
        ),
        (
            "a static bucket specification naming no account",
            one_time(
                token,
                serde_json::json!({ "s3JobDefinition": { "bucketDefinitions": [] } }),
            ),
        ),
        (
            "a selection type of ALL naming managed data identifiers",
            one_time(
                token,
                serde_json::json!({
                    "managedDataIdentifierSelector": "ALL",
                    "managedDataIdentifierIds": ["ADDRESS"]
                }),
            ),
        ),
        (
            "a selection type of INCLUDE naming no managed data identifier",
            one_time(
                token,
                serde_json::json!({ "managedDataIdentifierSelector": "INCLUDE" }),
            ),
        ),
        (
            "a selection type of NONE naming no custom data identifier",
            one_time(
                token,
                serde_json::json!({ "managedDataIdentifierSelector": "NONE" }),
            ),
        ),
        (
            "one tag too many",
            one_time(token, serde_json::json!({ "tags": tags(51, 1, 1) })),
        ),
        (
            "a tag key one character too long",
            one_time(token, serde_json::json!({ "tags": tags(1, 129, 1) })),
        ),
        (
            "a tag value one character too long",
            one_time(token, serde_json::json!({ "tags": tags(1, 1, 257) })),
        ),
    ];

    for (case, body) in refused {
        let (status, reply) = call_in(&router, REGION, "POST", JOBS_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {reply:?}"
        );
        assert!(
            !error_message(&reply).is_empty(),
            "case {case:?}: a refusal must state why, got {reply:?}"
        );
        assert!(
            reply["jobId"].is_null(),
            "case {case:?}: a refused create mints no job, got {reply:?}"
        );
    }

    // Nothing the refusals above carried was stored — the client token every
    // one of them used still mints a job.
    let (job_id, _) = create(
        &router,
        REGION,
        &one_time(token, serde_json::json!({})),
        "the token every refused create carried",
    )
    .await;
    let (status, reply) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        status, 200,
        "the job that token finally minted must be readable, got {status} with {reply:?}"
    );
}

/// The run a create starts ENDS: a read taken once that run is due to have
/// finished reports the status the clock owes — COMPLETE for a one-time job
/// ("Amazon Macie finished processing the data specified for the job") and IDLE
/// for a recurring one ("the previous scheduled run is complete and the next
/// scheduled run is pending") — while a recurring job that has not run keeps
/// waiting.
///
/// Each case waits for the CONDITION, re-reading the job until the run's end
/// shows up or a generous deadline passes, so nothing here depends on how fast
/// the host runs.
#[tokio::test]
async fn a_read_reports_the_end_the_clock_owes_a_started_run() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        (
            "a one-time job's only run",
            one_time("t-settles-once", serde_json::json!({})),
            COMPLETE,
        ),
        (
            "a recurring job's immediate first run",
            scheduled("t-settles-recurring", serde_json::json!({ "initialRun": true })),
            IDLE,
        ),
    ];

    for (case, body, expected) in cases {
        let (job_id, _) = create(&router, REGION, &body, case).await;
        let (_, first) = describe(&router, REGION, &job_id).await;
        assert_eq!(
            first["jobStatus"],
            serde_json::json!(RUNNING),
            "case {case:?}: a caller reading straight after the create still sees the run in \
             progress, got {:?}",
            first["jobStatus"]
        );

        // Generous: the span the emulator gives a started run is seconds, and
        // this only has to outlast a loaded host.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let settled = loop {
            let (_, reply) = describe(&router, REGION, &job_id).await;
            let status = reply["jobStatus"].as_str().unwrap_or_default().to_string();
            if status != RUNNING {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the started run never ended; the last read reported {reply:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(
            settled, expected,
            "case {case:?}: expected the run to end {expected:?}, got {settled:?}"
        );

        // The end is reported for good, and the read that reported it changed
        // nothing else about the job.
        let (status, again) = describe(&router, REGION, &job_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the job must still be readable, got {status} with {again:?}"
        );
        assert_eq!(
            again["jobStatus"],
            serde_json::json!(expected),
            "case {case:?}: expected {expected:?} on every later read, got {:?}",
            again["jobStatus"]
        );
        assert_eq!(
            again["lastRunTime"], first["lastRunTime"],
            "case {case:?}: the run's start instant must not move, got {:?} then {:?}",
            first["lastRunTime"], again["lastRunTime"]
        );
    }

    // A recurring job that was NOT configured to analyze existing objects has
    // started no run, so no end is owed: it stays IDLE.
    let (waiting_id, _) = create(
        &router,
        REGION,
        &scheduled("t-never-runs", serde_json::json!({})),
        "a recurring job waiting for its first scheduled run",
    )
    .await;
    let (_, waiting) = describe(&router, REGION, &waiting_id).await;
    assert_eq!(
        waiting["jobStatus"],
        serde_json::json!(IDLE),
        "a recurring job that has not run waits in IDLE, got {waiting:?}"
    );
    assert_eq!(
        waiting["statistics"]["numberOfRuns"].as_f64(),
        Some(0.0),
        "a job that has not run reports no runs, got {:?}",
        waiting["statistics"]
    );
}

/// A job whose settings name more list entries than this emulator stores for
/// one job is refused 402 `ServiceQuotaExceededException`, and a job at the
/// bound is accepted — the emulator's own bound on the size of one stored job.
#[tokio::test]
async fn a_job_larger_than_the_emulator_stores_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    // The bound counts every list entry the job would store; here they are all
    // buckets of one account, so the account itself is the one entry beside
    // them.
    let bound = 10_000;
    let at_bound = one_time(
        "t-at-bound",
        serde_json::json!({
            "s3JobDefinition": {
                "bucketDefinitions": [{
                    "accountId": ::engine::emulator_account_id(),
                    "buckets": (0..bound - 1)
                        .map(|index| serde_json::json!(format!("bucket-{index}")))
                        .collect::<Vec<_>>()
                }]
            }
        }),
    );
    let (job_id, _) = create(&router, REGION, &at_bound, "a job at the bound").await;
    let (status, reply) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        status, 200,
        "a job at the bound must be stored and readable, got {status} with {reply:?}"
    );

    let mut past_bound = at_bound.clone();
    past_bound["clientToken"] = serde_json::json!("t-past-bound");
    let buckets = past_bound["s3JobDefinition"]["bucketDefinitions"][0]["buckets"]
        .as_array_mut()
        .expect("the harness body names the buckets as an array");
    buckets.push(serde_json::json!("bucket-one-too-many"));
    let (status, reply) = call_in(
        &router,
        REGION,
        "POST",
        JOBS_URI,
        &past_bound.to_string(),
    )
    .await;
    assert_eq!(
        status, 402,
        "a job one entry past the bound must be refused, got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "ServiceQuotaExceededException",
        "expected the quota error the operation models, got {reply:?}"
    );
    assert!(
        reply["jobId"].is_null(),
        "a refused create mints no job, got {reply:?}"
    );
}

/// A retried `clientToken` replays the job it minted; a different token mints
/// another job.
#[tokio::test]
async fn a_retried_client_token_replays_the_job_it_minted() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let body = one_time("t-retried", serde_json::json!({}));
    let (first_id, first_arn) = create(&router, REGION, &body, "the first call").await;
    for case in ["the same body again", "the same token, a different name"] {
        let retried = if case == "the same body again" {
            body.clone()
        } else {
            one_time("t-retried", serde_json::json!({ "name": "renamed" }))
        };
        let (replayed_id, replayed_arn) = create(&router, REGION, &retried, case).await;
        assert_eq!(
            (replayed_id.as_str(), replayed_arn.as_str()),
            (first_id.as_str(), first_arn.as_str()),
            "case {case:?}: expected the job the first call minted, got {replayed_id:?}"
        );
    }

    let (other_id, _) = create(
        &router,
        REGION,
        &one_time("t-other", serde_json::json!({})),
        "a different token",
    )
    .await;
    assert_ne!(
        other_id, first_id,
        "a different clientToken must mint another job, got {other_id:?}"
    );

    // The replay reported the FIRST job, so the name the retry carried changed
    // nothing about it.
    let (_, reply) = describe(&router, REGION, &first_id).await;
    assert_eq!(
        reply["name"],
        serde_json::json!("quarterly-scan"),
        "a replayed create leaves the job it replays untouched, got {reply:?}"
    );
}

/// A job belongs to the region its create was signed for, both operations need
/// a Macie session in the region they address, and a read of a job the region
/// holds none of is refused.
#[tokio::test]
async fn a_job_is_scoped_to_its_region_and_needs_a_session_there() {
    let router = emulator();

    // Until Macie is enabled, neither operation serves the region.
    let (status, reply) = call_in(
        &router,
        REGION,
        "POST",
        JOBS_URI,
        &one_time("t-no-session", serde_json::json!({})).to_string(),
    )
    .await;
    assert_eq!(
        status, 403,
        "a create must be refused until Macie is enabled, got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "AccessDeniedException",
        "a create without a session must be refused AccessDeniedException, got {reply:?}"
    );
    assert_eq!(
        error_message(&reply),
        "Macie is not enabled.",
        "a create without a session must state why, got {reply:?}"
    );

    enable_in(&router, REGION, "{}").await;
    let (job_id, _) = create(
        &router,
        REGION,
        &one_time("t-scoped", serde_json::json!({})),
        "a job of the signed region",
    )
    .await;

    let (status, reply) = describe(&router, OTHER_REGION, &job_id).await;
    assert_eq!(
        status, 403,
        "a read of a region with no session must be refused, got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "AccessDeniedException",
        "a read without a session must be refused AccessDeniedException, got {reply:?}"
    );

    // With Macie enabled in the other region, the job of the first region is
    // still not one of its jobs.
    enable_in(&router, OTHER_REGION, "{}").await;
    let (status, reply) = describe(&router, OTHER_REGION, &job_id).await;
    assert_eq!(
        status, 404,
        "a job of another region is not found, got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "ResourceNotFoundException",
        "a job of another region must be refused ResourceNotFoundException, got {reply:?}"
    );
    assert_eq!(
        error_message(&reply),
        format!("Classification Job not found for id: {job_id}"),
        "the refusal names the id the request asked for, got {reply:?}"
    );

    // The job is still readable in the region that holds it, and a read leaves
    // it exactly as it was.
    let (status, first) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        status, 200,
        "the job must still be readable where it was created, got {status} with {first:?}"
    );
    let (status, second) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        status, 200,
        "a second read must answer 200, got {status} with {second:?}"
    );
    assert_eq!(
        first, second,
        "a read must not change what a later read sees, got {first:?} then {second:?}"
    );
}

/// A read of an id no job carries is refused 404, whatever the id looks like.
#[tokio::test]
async fn a_read_of_an_unknown_job_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for case in [
        "0123456789abcdefghijkl",
        "3ce05dbb7ec5505def334104bc9be17f",
        "not-a-job-id",
    ] {
        let (status, reply) = describe(&router, REGION, case).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {reply:?}"
        );
        assert_eq!(
            error_message(&reply),
            format!("Classification Job not found for id: {case}"),
            "case {case:?}: the refusal names the id the request asked for, got {reply:?}"
        );
    }
}
