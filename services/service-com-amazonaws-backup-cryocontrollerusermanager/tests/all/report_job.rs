// SPDX-License-Identifier: CC0-1.0

//! `StartReportJob` and the read over what it starts (`DescribeReportJob`).
//!
//! The rules under test:
//!
//! * a report job is an on-demand RUN OF A STORED REPORT PLAN: the start carries
//!   no template and no destination, so the job it begins reports the template
//!   and the delivery destination of the plan whose name the start addressed, and
//!   a name the account holds no plan under starts nothing (the one not-found
//!   refusal every report-plan call shares);
//! * the start answers with the `ReportJobId` the job is addressed by, and that
//!   id is what the read of the job resolves;
//! * a repeated `IdempotencyToken` takes no second action: it answers with the id
//!   of the job that token already started for the plan it was first used with;
//! * the report is not compiled when the start answers: the job reports the
//!   statuses AWS lists for one on the service's own clock, publishes no object
//!   key and no completion instant until it has COMPLETED, and a read of it never
//!   changes what a later read sees;
//! * a `ReportJobId` that is not of the form a report job's id takes names no job
//!   at all: it is refused as an invalid parameter value naming the member and
//!   stating that form, while an id of that form the account holds no job under is
//!   the not-found refusal;
//! * a completed run is an execution of its plan, so the plan reports when one of
//!   its jobs was last attempted and when one last succeeded.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{
    call, error_code, router, string_at, ACCOUNT, POLL_DEADLINE, POLL_WAIT,
};

/// The statuses a report job passes through, as the model lists them
/// (`smithy_query` shape `ReportJob`, member `Status`: `CREATED | RUNNING |
/// COMPLETED | FAILED`).
const STATUS_CREATED: &str = "CREATED";
const STATUS_RUNNING: &str = "RUNNING";
const STATUS_COMPLETED: &str = "COMPLETED";

/// The report templates a report may be built from, as the model lists them
/// (`smithy_query` shape `ReportSetting`, member `ReportTemplate`).
const REPORT_TEMPLATES: [&str; 6] = [
    "RESOURCE_COMPLIANCE_REPORT",
    "CONTROL_COMPLIANCE_REPORT",
    "BACKUP_JOB_REPORT",
    "COPY_JOB_REPORT",
    "RESTORE_JOB_REPORT",
    "SCAN_JOB_REPORT",
];

/// The format a plan delivers in when its create lists none: "If not specified,
/// the default format is CSV" (model doc for `ReportDeliveryChannel$Formats`).
const DEFAULT_FORMAT: &str = "CSV";

/// The most characters a `ReportPlanName` may carry, from the member's own
/// length bound (`smithy_query` operation `StartReportJob`: `len 1..256`).
const MAX_NAME_LENGTH: usize = 256;

/// `POST /audit/report-plans`, as the model binds the plan create.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/audit/report-plans", Some(&body.to_string())).await
}

/// `GET /audit/report-plans/{ReportPlanName}`, as the model binds the plan read.
async fn describe_plan(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GET", &format!("/audit/report-plans/{name}"), None).await
}

/// `POST /audit/report-jobs/{ReportPlanName}`, as the model binds the start; the
/// `IdempotencyToken` is a body member, so an unqualified start sends an empty
/// body.
async fn start_job(router: &MultiServiceRouter, plan_name: &str, token: Option<&str>) -> (u16, Value) {
    let body = match token {
        Some(token) => json!({"IdempotencyToken": token}),
        None => json!({}),
    };
    call(
        router,
        "POST",
        &format!("/audit/report-jobs/{plan_name}"),
        Some(&body.to_string()),
    )
    .await
}

/// `GET /audit/report-jobs/{ReportJobId}`, as the model binds the job read.
async fn describe_job(router: &MultiServiceRouter, report_job_id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/audit/report-jobs/{report_job_id}"),
        None,
    )
    .await
}

/// The job body a read reports (`ReportJob`), or `Value::Null`.
fn read_job(body: &Value) -> Value {
    body.get("ReportJob").cloned().unwrap_or(Value::Null)
}

/// The plan body a read reports (`ReportPlan`), or `Value::Null`.
fn read_plan(body: &Value) -> Value {
    body.get("ReportPlan").cloned().unwrap_or(Value::Null)
}

/// A member of a body as a JSON value, or `Value::Null` when it is absent.
fn at(body: &Value, member: &str) -> Value {
    body.get(member).cloned().unwrap_or(Value::Null)
}

/// The object keys a job reports under its destination, as a list of strings.
fn published_keys(job: &Value) -> Vec<String> {
    at(job, "ReportDestination")
        .get("S3Keys")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .map(|key| key.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A name of `length` characters the `ReportPlanName` pattern admits
/// (`^[a-zA-Z][_a-zA-Z0-9]*$`): a letter first, then letters, digits and
/// underscores, so the length bound is exercised across that whole alphabet.
fn plan_name_of_length(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
    (0..length)
        .map(|i| {
            if i == 0 {
                'j'
            } else {
                char::from(ALPHABET[(i * 5 + length) % ALPHABET.len()])
            }
        })
        .collect()
}

/// The smallest plan create body the model admits, plus whatever the row states
/// about the delivery channel.
fn plan_body(name: &str, template: &str, channel: Value) -> Value {
    let mut body = json!({
        "ReportPlanName": name,
        "ReportSetting": {"ReportTemplate": template},
    });
    body["ReportDeliveryChannel"] = channel;
    body
}

/// A started report job reports the plan it is a run of: the plan's ARN, the
/// template the plan's setting names and the bucket the plan delivers to —
/// none of which the start request carries — and it reports no published report
/// while the run is still in flight.
///
/// The rows differ along what the rule generalises over: the report template
/// (every published one), whether the plan states an S3 key prefix, which
/// delivery formats it lists (none, one, both) and the plan name's length — one
/// character and the published maximum of 256.
#[tokio::test]
async fn a_started_report_job_reports_the_stored_plan_it_runs() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "a bare resource-compliance plan: no prefix, no formats",
            plan_body(
                "job_bare",
                REPORT_TEMPLATES[0],
                json!({"S3BucketName": "audit-reports"}),
            ),
        ),
        (
            "a control-compliance plan with a prefix, both formats, shortest name",
            plan_body(
                &plan_name_of_length(1),
                REPORT_TEMPLATES[1],
                json!({
                    "S3BucketName": "audit-reports-two",
                    "S3KeyPrefix": "control/nightly",
                    "Formats": ["CSV", "JSON"],
                }),
            ),
        ),
        (
            "a backup-job plan delivering JSON only",
            plan_body(
                "job_backup_jobs",
                REPORT_TEMPLATES[2],
                json!({"S3BucketName": "audit-reports", "Formats": ["JSON"]}),
            ),
        ),
        (
            "a copy-job plan delivering explicitly as CSV",
            plan_body(
                "job_copy_jobs",
                REPORT_TEMPLATES[3],
                json!({"S3BucketName": "copy-job-reports", "Formats": ["CSV"]}),
            ),
        ),
        (
            "a restore-job plan with a prefix and no formats, longest name",
            plan_body(
                &plan_name_of_length(MAX_NAME_LENGTH),
                REPORT_TEMPLATES[4],
                json!({"S3BucketName": "audit-reports", "S3KeyPrefix": "restore/"}),
            ),
        ),
        (
            "a scan-job plan, the template the issue did not name",
            plan_body(
                "job_scan_jobs",
                REPORT_TEMPLATES[5],
                json!({"S3BucketName": "scan-reports"}),
            ),
        ),
    ];

    for (case, body) in cases {
        let router = router();
        let name = string_at(&body, "ReportPlanName");
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup expected CreateReportPlan 200, got {status} {created:?}"
        );

        let (status, started) = start_job(&router, &name, None).await;
        assert_eq!(
            (status, error_code(&started).as_str()),
            (200, "<none>"),
            "case {case:?}: expected StartReportJob 200, got {status} {started:?}"
        );
        let report_job_id = string_at(&started, "ReportJobId");
        assert!(
            !report_job_id.is_empty(),
            "case {case:?}: the start must answer with the id the job is addressed by, \
             got {started:?}"
        );

        let (status, read) = describe_job(&router, &report_job_id).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DescribeReportJob 200, got {status} {read:?}"
        );
        let job = read_job(&read);
        assert_eq!(
            (
                string_at(&job, "ReportJobId"),
                string_at(&job, "ReportPlanArn"),
                string_at(&job, "ReportTemplate"),
                string_at(&at(&job, "ReportDestination"), "S3BucketName"),
            ),
            (
                report_job_id.clone(),
                string_at(&created, "ReportPlanArn"),
                string_at(&body["ReportSetting"], "ReportTemplate"),
                string_at(&body["ReportDeliveryChannel"], "S3BucketName"),
            ),
            "case {case:?}: the job must report the plan it runs, got {read:?}"
        );
        assert!(
            job.get("CreationTime").is_some(),
            "case {case:?}: the job reports when the start began it, got {read:?}"
        );

        // While the run is in flight the job is in one of the two non-terminal
        // statuses, has published nothing and has no completion instant.
        let status_now = string_at(&job, "Status");
        assert!(
            [STATUS_CREATED, STATUS_RUNNING, STATUS_COMPLETED].contains(&status_now.as_str()),
            "case {case:?}: expected one of the published statuses, got {read:?}"
        );
        if status_now != STATUS_COMPLETED {
            assert_eq!(
                (at(&job, "CompletionTime"), published_keys(&job)),
                (Value::Null, Vec::new()),
                "case {case:?}: a run still in flight has published no report, got {read:?}"
            );
        }
    }
}

/// One row of the clock case below: the report plan to create, and what the
/// report of a run of it must be published as.
struct PublishedRow {
    case: &'static str,
    plan_name: &'static str,
    /// The key prefix the plan's delivery channel states, if any.
    prefix: Option<&'static str>,
    /// The formats the plan's create LISTS, or `None` for a create that lists
    /// none and so takes the documented default.
    listed_formats: Option<Vec<&'static str>>,
    /// The formats the plan therefore delivers in — one published object key
    /// each.
    delivered_formats: Vec<&'static str>,
}

/// A report job reaches its published report on the service's own clock: it
/// passes through the published statuses, then reports one object key per format
/// its plan delivers in, a completion instant, and nothing else that changes on a
/// later read. The plan it ran reports the attempt and the success.
///
/// The rows differ along what the rule generalises over: how many formats the
/// plan delivers in (none listed, so the documented default, one, and both) and
/// whether the plan states a key prefix. All three runs are started before any is
/// waited on, so the suite waits once rather than once per row.
#[tokio::test]
async fn a_report_job_publishes_its_report_on_the_services_clock() {
    let rows = vec![
        PublishedRow {
            case: "no format listed, so the documented default",
            plan_name: "job_default_format",
            prefix: None,
            listed_formats: None,
            delivered_formats: vec![DEFAULT_FORMAT],
        },
        PublishedRow {
            case: "one format and a key prefix",
            plan_name: "job_json_prefixed",
            prefix: Some("audit/json"),
            listed_formats: Some(vec!["JSON"]),
            delivered_formats: vec!["JSON"],
        },
        PublishedRow {
            case: "both formats, no prefix",
            plan_name: "job_both_formats",
            prefix: None,
            listed_formats: Some(vec!["CSV", "JSON"]),
            delivered_formats: vec!["CSV", "JSON"],
        },
    ];

    let router = router();
    let mut started: Vec<(PublishedRow, String)> = Vec::new();
    for row in rows {
        let PublishedRow {
            case,
            plan_name: name,
            prefix,
            ref listed_formats,
            ..
        } = row;
        let mut channel = json!({"S3BucketName": "audit-reports"});
        if let Some(prefix) = prefix {
            channel["S3KeyPrefix"] = json!(prefix);
        }
        if let Some(listed) = listed_formats {
            channel["Formats"] = json!(listed);
        }
        let body = plan_body(name, REPORT_TEMPLATES[2], channel);
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup expected CreateReportPlan 200, got {status} {created:?}"
        );

        // Before any run, the plan reports neither execution instant.
        let (status, read) = describe_plan(&router, name).await;
        assert_eq!(
            (
                status,
                at(&read_plan(&read), "LastAttemptedExecutionTime"),
                at(&read_plan(&read), "LastSuccessfulExecutionTime"),
            ),
            (200, Value::Null, Value::Null),
            "case {case:?}: a plan nothing has run reports no execution, got {read:?}"
        );

        let (status, job) = start_job(&router, name, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartReportJob 200, got {status} {job:?}"
        );
        let report_job_id = string_at(&job, "ReportJobId");

        // The start IS an attempt of the plan, whatever the run goes on to do.
        let (_, read) = describe_plan(&router, name).await;
        assert!(
            read_plan(&read).get("LastAttemptedExecutionTime").is_some(),
            "case {case:?}: the start is an attempted execution of the plan, got {read:?}"
        );

        started.push((row, report_job_id));
    }

    for (row, report_job_id) in started {
        let PublishedRow {
            case,
            plan_name: name,
            prefix,
            delivered_formats: formats,
            ..
        } = row;
        // Wait for the STATUS, not for a span: the transitions fall due on the
        // emulator's own clock.
        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        let settled = loop {
            let (status, read) = describe_job(&router, &report_job_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: polling expected DescribeReportJob 200, got {status} {read:?}"
            );
            let job = read_job(&read);
            let reported = string_at(&job, "Status");
            if reported == STATUS_COMPLETED {
                break job;
            }
            assert!(
                [STATUS_CREATED, STATUS_RUNNING].contains(&reported.as_str()),
                "case {case:?}: a run on its way to {STATUS_COMPLETED} reports a published \
                 non-terminal status until it gets there, got {job:?}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: expected the run to publish its report within \
                 {POLL_DEADLINE:?}, got {job:?}"
            );
            std::thread::sleep(POLL_WAIT);
        };

        assert!(
            settled.get("CompletionTime").is_some(),
            "case {case:?}: a published report carries the instant it was published, \
             got {settled:?}"
        );
        let keys = published_keys(&settled);
        assert_eq!(
            keys.len(),
            formats.len(),
            "case {case:?}: expected one object key per delivered format {formats:?}, \
             got {keys:?}"
        );
        for format in &formats {
            let extension = format.to_ascii_lowercase();
            assert!(
                keys.iter().any(|key| key.ends_with(&format!(".{extension}"))
                    && key.contains(&report_job_id)),
                "case {case:?}: expected a key of this run under .{extension}, got {keys:?}"
            );
        }
        if let Some(prefix) = prefix {
            assert!(
                keys.iter().all(|key| key.starts_with(prefix)),
                "case {case:?}: expected every key under the plan's prefix {prefix:?}, \
                 got {keys:?}"
            );
        }

        // A read never changes what a later read of the same job sees.
        let (_, again) = describe_job(&router, &report_job_id).await;
        assert_eq!(
            read_job(&again),
            settled,
            "case {case:?}: a read must not change what a later read reports"
        );

        // The plan the run belongs to reports that run as its last successful
        // execution, at the instant the job published its report.
        let (_, read) = describe_plan(&router, name).await;
        assert_eq!(
            at(&read_plan(&read), "LastSuccessfulExecutionTime"),
            at(&settled, "CompletionTime"),
            "case {case:?}: the plan must report the run that completed, got {read:?}"
        );
    }
}

/// A repeated `IdempotencyToken` starts no second run: it answers with the id of
/// the job that token started. A different token, no token at all, and the same
/// token against ANOTHER report plan each start a job of their own — the token
/// identifies otherwise identical calls, and a start naming another plan is not
/// one of those.
#[tokio::test]
async fn a_repeated_idempotency_token_starts_no_second_report_job() {
    let router = router();
    for name in ["job_idempotent", "job_idempotent_other"] {
        let body = plan_body(
            name,
            REPORT_TEMPLATES[2],
            json!({"S3BucketName": "audit-reports"}),
        );
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "setup expected CreateReportPlan 200 for {name:?}, got {status} {created:?}"
        );
    }

    let (status, first) = start_job(&router, "job_idempotent", Some("token-one")).await;
    assert_eq!(
        status, 200,
        "expected StartReportJob 200, got {status} {first:?}"
    );
    let first_id = string_at(&first, "ReportJobId");

    for case in ["the same token again", "the same token a third time"] {
        let (status, replayed) = start_job(&router, "job_idempotent", Some("token-one")).await;
        assert_eq!(
            (status, string_at(&replayed, "ReportJobId")),
            (200, first_id.clone()),
            "case {case:?}: a replay must answer with the id the first start answered, \
             got {replayed:?}"
        );
    }

    for (case, plan_name, token) in [
        ("another token", "job_idempotent", Some("token-two")),
        ("no token at all", "job_idempotent", None),
        (
            "the same token against another plan",
            "job_idempotent_other",
            Some("token-one"),
        ),
    ] {
        let (status, other) = start_job(&router, plan_name, token).await;
        let other_id = string_at(&other, "ReportJobId");
        assert_eq!(status, 200, "case {case:?}: expected 200, got {other:?}");
        assert_ne!(
            other_id, first_id,
            "case {case:?}: a start that is not a replay begins its own run, got {other:?}"
        );
        let (status, read) = describe_job(&router, &other_id).await;
        assert_eq!(
            (status, string_at(&read_job(&read), "ReportPlanArn").is_empty()),
            (200, false),
            "case {case:?}: the second run must be readable, got {status} {read:?}"
        );
    }

    // A token the account holds a job for says nothing about a plan it holds no
    // job for: the start still takes the plan's own not-found refusal.
    let (status, refused) = start_job(&router, "job_absent", Some("token-one")).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ResourceNotFoundException"),
        "a replayed token must not start a run of a plan the account does not hold, \
         got {status} {refused:?}"
    );
}

/// A start that names no stored report plan begins nothing: a name the account
/// holds no plan under is the one not-found refusal, and a name outside the
/// modelled pattern or length bound is refused before the handler runs. A read of
/// an id no job was started under is refused the same way the plan read is.
#[tokio::test]
async fn a_start_outside_the_stored_plans_or_the_name_rule_is_refused() {
    let router = router();
    let body = plan_body(
        "job_present",
        REPORT_TEMPLATES[2],
        json!({"S3BucketName": "audit-reports"}),
    );
    let (status, created) = create_plan(&router, &body).await;
    assert_eq!(
        status, 200,
        "setup expected CreateReportPlan 200, got {status} {created:?}"
    );

    // A name the account holds no plan under: never created, differently cased,
    // and one whose plan has been deleted.
    let (status, deleted) = call(
        &router,
        "DELETE",
        "/audit/report-plans/job_present",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup expected DeleteReportPlan 200, got {status} {deleted:?}"
    );
    for name in ["job_absent", "Job_Present", "job_present"] {
        let (status, refused) = start_job(&router, name, None).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                format!("Report plan {name} not found in account {ACCOUNT}.").as_str(),
            ),
            "case {name:?}: a name holding no plan starts nothing, got {status} {refused:?}"
        );
    }

    // A name the model does not admit at all: the pattern is
    // `^[a-zA-Z][_a-zA-Z0-9]*$` and the length bound is 1..256.
    for (case, name) in [
        ("a name starting with a digit", "9job".to_string()),
        ("a name carrying a hyphen", "job-one".to_string()),
        ("a name one past the length bound", plan_name_of_length(MAX_NAME_LENGTH + 1)),
    ] {
        let (status, refused) = start_job(&router, &name, None).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?}: a name the model does not admit is refused, got {status} {refused:?}"
        );
    }

    // An id of the form a report job's id takes, under which no job was started.
    for (case, report_job_id) in [
        (
            "a lower-case id in the minted form",
            "00000000-0000-0000-0000-000000000001",
        ),
        (
            "an upper-case id in the minted form",
            "8F81F553-3A74-4A3F-B93D-B3360DC80C50",
        ),
    ] {
        let (status, refused) = describe_job(&router, report_job_id).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                format!("Report job {report_job_id} not found in account {ACCOUNT}.").as_str(),
            ),
            "case {case:?}: an id holding no job is not found, got {status} {refused:?}"
        );
    }
}

/// A `ReportJobId` that is not of the form a report job's id takes names no
/// report job at all: it is refused as an invalid parameter value naming the
/// member and stating the form, rather than resolved and reported as a job the
/// account does not hold.
///
/// The rows span the class: labels made only of the whitespace forms a client can
/// put in a path, a word that is no identifier, a 36-character value that is not
/// the hyphenated form, that form with a group of the wrong length and with a
/// character outside hex, and a well-formed id with whitespace around it.
#[tokio::test]
async fn a_report_job_id_outside_the_identifier_form_is_refused_as_invalid() {
    let router = router();
    let body = plan_body(
        "job_blank_id",
        REPORT_TEMPLATES[2],
        json!({"S3BucketName": "audit-reports"}),
    );
    let (status, created) = create_plan(&router, &body).await;
    assert_eq!(
        status, 200,
        "setup expected CreateReportPlan 200, got {status} {created:?}"
    );
    let (status, started) = start_job(&router, "job_blank_id", None).await;
    assert_eq!(
        status, 200,
        "setup expected StartReportJob 200, got {status} {started:?}"
    );

    // An empty label is not a request to this operation at all: `GET
    // /audit/report-jobs` is the listing's own route, so every row below is a
    // label a client can actually address a report job by. Each row carries the
    // label as it is sent and the id it decodes to, which the refusal states back.
    for (case, label, stated) in [
        ("a single space", "%20", " "),
        ("several spaces", "%20%20%20", "   "),
        ("a tab", "%09", "\t"),
        ("a word that is no identifier", "not_a_report_job", "not_a_report_job"),
        (
            "36 characters that are not the hyphenated form",
            "000000000000000000000000000000000000",
            "000000000000000000000000000000000000",
        ),
        (
            "the hyphenated form with a short group",
            "00000000-0000-0000-000-000000000001",
            "00000000-0000-0000-000-000000000001",
        ),
        (
            "the hyphenated form with a character outside hex",
            "0000000g-0000-0000-0000-000000000001",
            "0000000g-0000-0000-0000-000000000001",
        ),
        (
            "a well-formed id with a space around it",
            "%2000000000-0000-0000-0000-000000000001%20",
            " 00000000-0000-0000-0000-000000000001 ",
        ),
    ] {
        let (status, refused) = describe_job(&router, label).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
            ),
            (
                400,
                "InvalidParameterValueException",
                format!("Invalid report job ID {stated}. The report job ID must be a valid UUID.")
                    .as_str(),
            ),
            "case {case:?}: an id outside the identifier form is refused as invalid, \
             got {status} {refused:?}"
        );
        assert_eq!(
            string_at(&refused, "Context"),
            "ReportJobId",
            "case {case:?}: the refusal must name the member it rejected, got {refused:?}"
        );
    }

    // The job that WAS started is still readable, so the refusals above are
    // about the id the call carried and nothing else.
    let (status, read) = describe_job(&router, &string_at(&started, "ReportJobId")).await;
    assert_eq!(
        status, 200,
        "the started job must still be readable, got {status} {read:?}"
    );
}
