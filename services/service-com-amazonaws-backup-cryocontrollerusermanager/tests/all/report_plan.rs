// SPDX-License-Identifier: CC0-1.0

//! `CreateReportPlan` and the read over what it stores (`DescribeReportPlan`).
//!
//! The rules under test:
//!
//! * a report plan is a STORED resource addressed by name: the body a create
//!   submits is what every later read of that name reports, with the members the
//!   model gives a rule for filled in — the delivery formats default to `CSV`
//!   where the create lists none, and `NumberOfFrameworks` is counted from the
//!   frameworks the setting names;
//! * the name identifies one plan of the account, so the first create of a name
//!   stores the one plan it has and every other create naming it is refused as
//!   already existing, leaving the stored plan untouched;
//! * a repeated `IdempotencyToken` takes no second action: it answers with the
//!   plan that token created;
//! * a report template or a delivery format outside the published values is
//!   refused `InvalidParameterValueException`, naming the member it rejected, and
//!   nothing is stored for it;
//! * a name the account holds no plan under is the one not-found refusal;
//! * a fresh plan's deployment is not finished when the create answers: the plan
//!   reports `CREATE_IN_PROGRESS` and reaches `COMPLETED` on the service's own
//!   clock, and a read of it never changes what a later read sees.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{call, error_code, reported_tags, router, string_at, ACCOUNT, REGION};

/// The deployment statuses a report plan passes through, as the model lists them
/// (`smithy_query` shape `ReportPlan`, member `DeploymentStatus`:
/// `CREATE_IN_PROGRESS | UPDATE_IN_PROGRESS | DELETE_IN_PROGRESS | COMPLETED`).
const STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const STATUS_COMPLETED: &str = "COMPLETED";

/// How long a poll loop looks for the deployment the CLOCK owes a fresh plan
/// before failing the case.
///
/// Generous on purpose: the transition falls due on the emulator's own clock, so
/// each case waits for the STATUS and this deadline only bounds a plan whose
/// deployment never finishes at all — a loaded or a slow host still sees one
/// that does.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between two looks. A poll INTERVAL, not a budget:
/// the loop gives up only at [`POLL_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// The most characters a `ReportPlanName` may carry, from the member's own length
/// bound (`smithy_query` operation `CreateReportPlan`: `len 1..256`).
const MAX_NAME_LENGTH: usize = 256;

/// `POST /audit/report-plans`, as the model binds the create.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/audit/report-plans", Some(&body.to_string())).await
}

/// `GET /audit/report-plans/{ReportPlanName}`, as the model binds the read.
async fn describe_plan(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GET", &format!("/audit/report-plans/{name}"), None).await
}

/// The plan body a read reports (`ReportPlan`), or `Value::Null`.
fn read_plan(body: &Value) -> Value {
    body.get("ReportPlan").cloned().unwrap_or(Value::Null)
}

/// A member of a body as a JSON value, or `Value::Null` when it is absent.
fn at(body: &Value, member: &str) -> Value {
    body.get(member).cloned().unwrap_or(Value::Null)
}

/// A name of `length` characters the `ReportPlanName` pattern admits
/// (`^[a-zA-Z][_a-zA-Z0-9]*$`): a letter first, then letters, digits and
/// underscores, so the length bound is exercised across that whole alphabet.
fn plan_name_of_length(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
    (0..length)
        .map(|i| {
            if i == 0 {
                'r'
            } else {
                char::from(ALPHABET[(i * 7 + length) % ALPHABET.len()])
            }
        })
        .collect()
}

/// The smallest create body the model admits: a name, a delivery channel naming
/// the bucket the reports land in, and a setting naming the template they are
/// built from.
fn minimal_body(name: &str, template: &str) -> Value {
    json!({
        "ReportPlanName": name,
        "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
        "ReportSetting": {"ReportTemplate": template},
    })
}

/// The ARN prefix this service mints a report plan's ARN from: the name, plus a
/// uniquifier only the service knows, so a case asserts the prefix rather than
/// the whole string.
fn arn_prefix(name: &str) -> String {
    format!("arn:aws:backup:{REGION}:{ACCOUNT}:report-plan:{name}-")
}

/// A created report plan round-trips: the read reports the body that was stored,
/// with the documented default filled in where the create listed no format and
/// `NumberOfFrameworks` counted from the frameworks the setting names.
///
/// The rows differ along what the rule generalises over: the report template
/// (every published one), whether a description and an S3 key prefix are stated,
/// which delivery formats are listed (none, one, both), how many frameworks the
/// setting names (none, one, two), which accounts, organizational units and
/// Regions it covers, and the name's length — one character and the published
/// maximum of 256.
#[tokio::test]
async fn a_created_report_plan_round_trips_through_the_read() {
    let framework_arn = |name: &str| format!("arn:aws:backup:{REGION}:{ACCOUNT}:framework:{name}");
    let cases: Vec<(&str, Value)> = vec![
        (
            "a bare backup-job report: no description, no prefix, no formats",
            minimal_body("report_bare", REPORT_TEMPLATES[2]),
        ),
        (
            "a resource-compliance report over one framework, both formats",
            json!({
                "ReportPlanName": plan_name_of_length(1),
                "ReportPlanDescription": "nightly resource compliance",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports",
                    "S3KeyPrefix": "compliance/nightly",
                    "Formats": ["CSV", "JSON"],
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[0],
                    "FrameworkArns": [framework_arn("one")],
                    "Regions": ["*"],
                },
            }),
        ),
        (
            "a control-compliance report over two frameworks, JSON only, longest name",
            json!({
                "ReportPlanName": plan_name_of_length(MAX_NAME_LENGTH),
                "ReportPlanDescription": "control compliance across the organization",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports-two",
                    "S3KeyPrefix": "control",
                    "Formats": ["JSON"],
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[1],
                    "FrameworkArns": [framework_arn("one"), framework_arn("two")],
                    "Accounts": ["ROOT"],
                    "OrganizationUnits": ["ou-1111-22222222"],
                    "Regions": [REGION, "us-east-1"],
                },
            }),
        ),
        (
            "a copy-job report delivering explicitly as CSV",
            json!({
                "ReportPlanName": "report_copy_jobs",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports",
                    "Formats": ["CSV"],
                },
                "ReportSetting": {"ReportTemplate": REPORT_TEMPLATES[3]},
            }),
        ),
        (
            "a restore-job report with a key prefix and no formats",
            json!({
                "ReportPlanName": "report_restore_jobs",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports",
                    "S3KeyPrefix": "restore",
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[4],
                    "Accounts": ["ROOT"],
                },
            }),
        ),
        (
            "a scan-job report, the template the issue did not name",
            minimal_body("report_scan_jobs", REPORT_TEMPLATES[5]),
        ),
    ];

    for (case, body) in cases {
        let router = router();
        let name = string_at(&body, "ReportPlanName");
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            (status, error_code(&created).as_str()),
            (200, "<none>"),
            "case {case:?}: expected CreateReportPlan 200, got {status} {created:?}"
        );
        let arn = string_at(&created, "ReportPlanArn");
        assert_eq!(
            string_at(&created, "ReportPlanName"),
            name,
            "case {case:?}: the create must answer with the plan it stored, got {created:?}"
        );
        assert!(
            arn.starts_with(&arn_prefix(&name)),
            "case {case:?}: expected an ARN under {:?}, got {arn:?}",
            arn_prefix(&name)
        );
        assert!(
            created.get("CreationTime").is_some(),
            "case {case:?}: the create reports when it stored the plan, got {created:?}"
        );

        let (status, read) = describe_plan(&router, &name).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DescribeReportPlan 200, got {status} {read:?}"
        );
        let stored = read_plan(&read);
        assert_eq!(
            (
                string_at(&stored, "ReportPlanName"),
                string_at(&stored, "ReportPlanArn"),
                at(&stored, "CreationTime"),
                at(&stored, "ReportPlanDescription"),
            ),
            (
                name.clone(),
                arn.clone(),
                at(&created, "CreationTime"),
                at(&body, "ReportPlanDescription"),
            ),
            "case {case:?}: the read must report what the create stored, got {read:?}"
        );

        let submitted_channel = at(&body, "ReportDeliveryChannel");
        let channel = at(&stored, "ReportDeliveryChannel");
        assert_eq!(
            (
                string_at(&channel, "S3BucketName"),
                at(&channel, "S3KeyPrefix"),
            ),
            (
                string_at(&submitted_channel, "S3BucketName"),
                at(&submitted_channel, "S3KeyPrefix"),
            ),
            "case {case:?}: the delivery channel is stored as submitted, got {channel:?}"
        );
        assert_eq!(
            at(&channel, "Formats"),
            submitted_channel
                .get("Formats")
                .cloned()
                .unwrap_or_else(|| json!([DEFAULT_FORMAT])),
            "case {case:?}: an unlisted Formats defaults to {DEFAULT_FORMAT}, got {channel:?}"
        );

        let submitted_setting = at(&body, "ReportSetting");
        let setting = at(&stored, "ReportSetting");
        let frameworks = submitted_setting
            .get("FrameworkArns")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        // A scoping list the create named nothing for is reported as the empty
        // list rather than left out — the rule `report_plan_wire_form.rs` pins
        // over the whole class.
        let submitted_list = |member: &str| -> Value {
            submitted_setting
                .get(member)
                .cloned()
                .unwrap_or_else(|| json!([]))
        };
        assert_eq!(
            (
                string_at(&setting, "ReportTemplate"),
                at(&setting, "FrameworkArns"),
                at(&setting, "Accounts"),
                at(&setting, "OrganizationUnits"),
                at(&setting, "Regions"),
            ),
            (
                string_at(&submitted_setting, "ReportTemplate"),
                submitted_list("FrameworkArns"),
                submitted_list("Accounts"),
                submitted_list("OrganizationUnits"),
                submitted_list("Regions"),
            ),
            "case {case:?}: the report setting is stored as submitted, got {setting:?}"
        );
        assert_eq!(
            setting.get("NumberOfFrameworks").and_then(Value::as_i64),
            Some(i64::try_from(frameworks).expect("a framework count")),
            "case {case:?}: NumberOfFrameworks counts the frameworks the setting names, got \
             {setting:?}"
        );
        assert_eq!(
            string_at(&stored, "DeploymentStatus"),
            STATUS_CREATE_IN_PROGRESS,
            "case {case:?}: a plan whose deployment has not finished reports \
             {STATUS_CREATE_IN_PROGRESS}, got {stored:?}"
        );
        assert!(
            stored.get("LastAttemptedExecutionTime").is_none()
                && stored.get("LastSuccessfulExecutionTime").is_none(),
            "case {case:?}: a plan no report job has run reports neither instant, got {stored:?}"
        );
    }
}

/// The deployment the clock owes a fresh plan finishes on the service's own
/// clock, and a read of the plan never changes what a later read sees.
#[tokio::test]
async fn a_fresh_report_plan_deployment_completes_on_the_services_clock() {
    let router = router();
    let (status, created) = create_plan(&router, &minimal_body("report_deployed", "BACKUP_JOB_REPORT")).await;
    assert_eq!(
        status, 200,
        "setup: the create should succeed, got {status} {created:?}"
    );

    // Wait for the STATUS, not for a span: the transition falls due on the
    // emulator's clock.
    let deadline = Instant::now() + POLL_DEADLINE;
    let settled = loop {
        let (status, read) = describe_plan(&router, "report_deployed").await;
        assert_eq!(status, 200, "polling: expected 200, got {status} {read:?}");
        let stored = read_plan(&read);
        let deployment = string_at(&stored, "DeploymentStatus");
        if deployment == STATUS_COMPLETED {
            break stored;
        }
        assert_eq!(
            deployment, STATUS_CREATE_IN_PROGRESS,
            "polling: a plan on its way to {STATUS_COMPLETED} reports \
             {STATUS_CREATE_IN_PROGRESS} until it gets there, got {stored:?}"
        );
        assert!(
            Instant::now() < deadline,
            "expected the deployment to finish within {POLL_DEADLINE:?}, got {stored:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Every member a read reports stays what it was: the settle moves the
    // deployment status and nothing else.
    let (_, again) = describe_plan(&router, "report_deployed").await;
    assert_eq!(
        read_plan(&again),
        settled,
        "a read must not change what a later read of the same plan reports"
    );
    assert_eq!(
        string_at(&settled, "ReportPlanArn"),
        string_at(&created, "ReportPlanArn"),
        "the settled plan is the plan the create stored, got {settled:?}"
    );
}

/// The first create of a name stores the one plan that name has: every other
/// create naming it is refused as already existing, and the stored plan is left
/// as it was.
///
/// The rows differ in what the second create carries — the same body, another
/// template, another delivery channel — so the rule is asserted over the class
/// rather than over one repeat.
#[tokio::test]
async fn a_second_create_of_one_name_is_refused_and_changes_nothing() {
    let name = "report_contended";
    let seconds: Vec<(&str, Value)> = vec![
        ("the same body again", minimal_body(name, REPORT_TEMPLATES[2])),
        (
            "another report template",
            minimal_body(name, REPORT_TEMPLATES[1]),
        ),
        (
            "another delivery channel and a description",
            json!({
                "ReportPlanName": name,
                "ReportPlanDescription": "a different plan under the same name",
                "ReportDeliveryChannel": {
                    "S3BucketName": "other-bucket",
                    "Formats": ["JSON"],
                },
                "ReportSetting": {"ReportTemplate": REPORT_TEMPLATES[4]},
            }),
        ),
    ];

    for (case, second) in seconds {
        let router = router();
        let first_body = minimal_body(name, REPORT_TEMPLATES[2]);
        let (status, first) = create_plan(&router, &first_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup: the first create should succeed, got {status} {first:?}"
        );
        let (_, before) = describe_plan(&router, name).await;

        let (status, refused) = create_plan(&router, &second).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "AlreadyExistsException"),
            "case {case:?}: a create of a name the account holds is refused, got {status} \
             {refused:?}"
        );
        assert_eq!(
            string_at(&refused, "Message"),
            format!("Report plan with name {name} already exists in account {ACCOUNT} ."),
            "case {case:?}: the refusal names the plan and the account, got {refused:?}"
        );

        let (status, after) = describe_plan(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the stored plan must still be readable, got {status} {after:?}"
        );
        assert_eq!(
            read_plan(&after),
            read_plan(&before),
            "case {case:?}: a refused create must leave the stored plan untouched"
        );
    }
}

/// A repeated `IdempotencyToken` takes no second action: it answers with the plan
/// that token created, even when the body it carries differs, and the store still
/// holds that one plan.
#[tokio::test]
async fn a_repeated_idempotency_token_replays_the_first_create() {
    let router = router();
    let mut body = minimal_body("report_replayed", REPORT_TEMPLATES[2]);
    body["IdempotencyToken"] = json!("one-report-plan-only");

    let (status, first) = create_plan(&router, &body).await;
    assert_eq!(status, 200, "the first create should succeed, got {first:?}");

    // The SAME token on a body naming another plan: the token identifies the
    // call, so what it replays is the plan it already created.
    let mut retried = minimal_body("report_replayed_other", REPORT_TEMPLATES[1]);
    retried["IdempotencyToken"] = json!("one-report-plan-only");
    let (status, replayed) = create_plan(&router, &retried).await;
    assert_eq!(
        (status, error_code(&replayed).as_str()),
        (200, "<none>"),
        "a replayed IdempotencyToken must answer as the first call did, got {status} {replayed:?}"
    );
    assert_eq!(
        (
            string_at(&replayed, "ReportPlanName"),
            string_at(&replayed, "ReportPlanArn"),
            at(&replayed, "CreationTime"),
        ),
        (
            string_at(&first, "ReportPlanName"),
            string_at(&first, "ReportPlanArn"),
            at(&first, "CreationTime"),
        ),
        "a replay must report the identifiers the first call reported, got {replayed:?}"
    );

    let (status, read) = describe_plan(&router, "report_replayed_other").await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ResourceNotFoundException"),
        "a replay must store no second plan, got {status} {read:?}"
    );

    // A create of ANOTHER name with no token IS a second plan, which is what
    // makes the replay above about the token.
    let (status, second) =
        create_plan(&router, &minimal_body("report_second", REPORT_TEMPLATES[3])).await;
    assert_eq!(status, 200, "a second name should be stored, got {second:?}");
    let (status, read) = describe_plan(&router, "report_second").await;
    assert_eq!(status, 200, "the second plan must be readable, got {read:?}");
}

/// A report plan body outside the published values is refused
/// `InvalidParameterValueException`, naming the member it rejected — and nothing
/// is stored for it.
///
/// The rows span the class: a template that is no published template (unknown,
/// differently cased, empty) and a format that is no published format, each
/// beside the member the refusal has to name.
#[tokio::test]
async fn a_report_plan_body_outside_the_published_values_is_refused() {
    let templates: Vec<(&str, Value)> = vec![
        ("a template nothing publishes", json!("AUDIT_REPORT")),
        (
            "a published template in the wrong case",
            json!("backup_job_report"),
        ),
        ("an empty template", json!("")),
    ];
    for (case, template) in templates {
        let router = router();
        let mut body = minimal_body("report_template", REPORT_TEMPLATES[2]);
        body["ReportSetting"]["ReportTemplate"] = template;
        assert_refused(&router, case, &body, "ReportTemplate").await;
    }

    let formats: Vec<(&str, Value)> = vec![
        ("a format nothing publishes", json!(["PDF"])),
        (
            "a published format in the wrong case",
            json!(["csv"]),
        ),
        (
            "one published format beside one that is not",
            json!(["JSON", "PARQUET"]),
        ),
    ];
    for (case, listed) in formats {
        let router = router();
        let mut body = minimal_body("report_formats", REPORT_TEMPLATES[2]);
        body["ReportDeliveryChannel"]["Formats"] = listed;
        assert_refused(&router, case, &body, "Formats").await;
    }
}

/// The one refusal every row of the table above expects: 400
/// `InvalidParameterValueException` naming `member`, and no plan stored.
async fn assert_refused(router: &MultiServiceRouter, case: &str, body: &Value, member: &str) {
    let (status, refused) = create_plan(router, body).await;
    assert_eq!(
        (
            status,
            error_code(&refused).as_str(),
            string_at(&refused, "Message").as_str(),
            string_at(&refused, "Context").as_str(),
        ),
        (
            400,
            "InvalidParameterValueException",
            format!("Parameter {member} is invalid.").as_str(),
            member,
        ),
        "case {case:?}: expected the refusal to name {member}, got {status} {refused:?}"
    );
    let name = string_at(body, "ReportPlanName");
    let (status, read) = describe_plan(router, &name).await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ResourceNotFoundException"),
        "case {case:?}: a refused create stores nothing, got {status} {read:?}"
    );
}

/// A name the account holds no report plan under is the one not-found refusal,
/// whatever else the store holds.
#[tokio::test]
async fn a_read_of_an_unknown_report_plan_name_is_refused() {
    let router = router();
    let (status, created) =
        create_plan(&router, &minimal_body("report_present", REPORT_TEMPLATES[2])).await;
    assert_eq!(
        status, 200,
        "setup: the create should succeed, got {created:?}"
    );

    for name in ["report_absent", "Report_Present", "REPORT_PRESENT"] {
        let (status, refused) = describe_plan(&router, name).await;
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
            "case {name:?}: a name holding no plan is not found, got {status} {refused:?}"
        );
    }
}

/// The tags a create assigns to the report plan are the plan's own: the tag read
/// that addresses a resource by ARN reports them, as it does for every other
/// resource of this service that carries both an ARN and tags.
#[tokio::test]
async fn the_tags_a_create_assigns_are_reported_for_the_plans_arn() {
    for (case, tags) in [
        ("no tags assigned", json!({})),
        ("one tag assigned", json!({"team": "audit"})),
        (
            "several tags assigned",
            json!({"team": "audit", "env": "test", "owner": "backup"}),
        ),
    ] {
        let router = router();
        let mut body = minimal_body("report_tagged", REPORT_TEMPLATES[2]);
        body["ReportPlanTags"] = tags.clone();
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create should succeed, got {status} {created:?}"
        );
        let arn = string_at(&created, "ReportPlanArn");
        let (status, listed) = call(&router, "GET", &format!("/tags/{arn}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListTags 200 for {arn:?}, got {status} {listed:?}"
        );
        assert_eq!(
            reported_tags(&listed),
            tags,
            "case {case:?}: the tag read must report the tags the create assigned, got {listed:?}"
        );
    }
}

/// A create carrying more tags than one resource may hold is refused
/// `LimitExceededException` and stores nothing, while a create at the ceiling is
/// stored and reports every tag.
///
/// The rows differ in how many tags the request carries: the ceiling this
/// service applies to the tags of a resource, and one past it.
#[tokio::test]
async fn a_create_past_the_tag_ceiling_is_refused() {
    // The ceiling `TagResource` applies to one resource's tags, so a create
    // cannot store a plan carrying more tags than a tagging call may put on it.
    const MAX_TAGS_PER_RESOURCE: usize = 50;
    let tags_of = |count: usize| -> Value {
        let mut map = serde_json::Map::new();
        for index in 0..count {
            map.insert(format!("key{index}"), json!(format!("value{index}")));
        }
        Value::Object(map)
    };

    let router = router();
    let mut body = minimal_body("report_at_the_ceiling", REPORT_TEMPLATES[2]);
    body["ReportPlanTags"] = tags_of(MAX_TAGS_PER_RESOURCE);
    let (status, created) = create_plan(&router, &body).await;
    assert_eq!(
        (status, error_code(&created).as_str()),
        (200, "<none>"),
        "case {MAX_TAGS_PER_RESOURCE} tags: a create at the ceiling is stored, got {status} \
         {created:?}"
    );
    let arn = string_at(&created, "ReportPlanArn");
    let (_, listed) = call(&router, "GET", &format!("/tags/{arn}"), None).await;
    assert_eq!(
        reported_tags(&listed),
        tags_of(MAX_TAGS_PER_RESOURCE),
        "case {MAX_TAGS_PER_RESOURCE} tags: every tag the create assigned is reported, got \
         {listed:?}"
    );

    let over = MAX_TAGS_PER_RESOURCE + 1;
    let mut body = minimal_body("report_past_the_ceiling", REPORT_TEMPLATES[2]);
    body["ReportPlanTags"] = tags_of(over);
    let (status, refused) = create_plan(&router, &body).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "LimitExceededException"),
        "case {over} tags: a create past the ceiling is refused, got {status} {refused:?}"
    );
    let (status, read) = describe_plan(&router, "report_past_the_ceiling").await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ResourceNotFoundException"),
        "case {over} tags: a refused create stores nothing, got {status} {read:?}"
    );
}
