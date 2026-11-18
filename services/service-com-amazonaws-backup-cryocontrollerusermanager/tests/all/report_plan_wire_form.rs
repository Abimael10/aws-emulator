// SPDX-License-Identifier: CC0-1.0

//! What the report-plan plane PUTS ON THE WIRE: the members every read of a
//! report plan reports, and the sentences its two refusals state.
//!
//! The rules under test:
//!
//! * each of a report setting's four scoping lists — the frameworks a report
//!   covers and the accounts, organizational units and Regions it is scoped to
//!   (`smithy_query` shape `ReportSetting`) — is reported as a LIST on every read
//!   of the plan: the entries the create named, or the empty list where it named
//!   none, never left out, so a client can tell "nothing scoped" from "not
//!   reported";
//! * a report plan reports the schedule its report runs on — how many hours apart
//!   two runs are and the hour of the day a run starts at — which no create
//!   states, so every stored plan carries this service's own schedule;
//! * the refusal of a create naming a plan the account already holds, and the
//!   refusal of a read or a delete of a name it holds none under, each state
//!   their own sentence around the name the request carried and the account it
//!   was made in.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{call_reported, error_code, router, string_at, ACCOUNT, REGION};

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

/// The scoping lists a report setting carries, as the model names them
/// (`smithy_query` shape `ReportSetting`: `FrameworkArns`, `Accounts`,
/// `OrganizationUnits` and `Regions`, each targeting `stringList`).
const SCOPING_LISTS: [&str; 4] = ["FrameworkArns", "Accounts", "OrganizationUnits", "Regions"];

/// The most characters a `ReportPlanName` may carry, from the member's own length
/// bound (`smithy_query` operation `CreateReportPlan`: `len 1..256`).
const MAX_NAME_LENGTH: usize = 256;

/// `POST /audit/report-plans`, as the model binds the create.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, String, Value) {
    call_reported(router, "POST", "/audit/report-plans", Some(&body.to_string())).await
}

/// `GET /audit/report-plans/{ReportPlanName}`, as the model binds the read.
async fn describe_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(router, "GET", &format!("/audit/report-plans/{name}"), None).await
}

/// `DELETE /audit/report-plans/{ReportPlanName}`, as the model binds the delete.
async fn delete_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(
        router,
        "DELETE",
        &format!("/audit/report-plans/{name}"),
        None,
    )
    .await
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
/// underscores.
fn plan_name_of_length(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
    (0..length)
        .map(|i| {
            if i == 0 {
                'w'
            } else {
                char::from(ALPHABET[(i * 11 + length) % ALPHABET.len()])
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

/// The schedule this service gives a report plan, which no create states: a run
/// once a day, from the top of the day.
fn service_report_timing() -> Value {
    json!({"ReportFrequencyHours": 24, "ReportStartHour": 0})
}

/// The sentence this service states for "the report plan asked about is not one
/// this account holds".
///
/// `pub(crate)` so the sibling suite that drives the same refusal through the
/// DELETE binding (`report_plan_deletion.rs`) asserts against THIS spelling
/// rather than keeping a second copy of it.
pub(crate) fn not_found_message(name: &str) -> String {
    format!("Report plan {name} not found in account {ACCOUNT}.")
}

/// The sentence this service states for "this account already holds the report
/// plan the create would duplicate".
fn already_exists_message(name: &str) -> String {
    format!("Report plan with name {name} already exists in account {ACCOUNT} .")
}

/// RULE over the report settings a create may carry: every read of the plan
/// reports each of the four scoping lists — as submitted, or empty where the
/// create named no entry — and reports the plan's report timing.
///
/// The rows differ along what the rule generalises over: which of the four lists
/// the create names (none, one, some, all four), how many entries each carries
/// (one, two), whether a list is submitted EXPLICITLY empty, and the report
/// template the plan is built from.
#[tokio::test]
async fn every_read_of_a_report_plan_reports_its_scoping_lists_and_its_timing() {
    let framework_arn = |name: &str| format!("arn:aws:backup:{REGION}:{ACCOUNT}:framework:{name}");
    let cases: Vec<(&str, Value)> = vec![
        (
            "a setting carrying only the report template",
            minimal_body("report_wire_bare", REPORT_TEMPLATES[2]),
        ),
        (
            "one framework covered, nothing else scoped",
            json!({
                "ReportPlanName": "report_wire_one_framework",
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[0],
                    "FrameworkArns": [framework_arn("one")],
                },
            }),
        ),
        (
            "accounts and Regions scoped, frameworks and units not",
            json!({
                "ReportPlanName": "report_wire_accounts_regions",
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[1],
                    "Accounts": ["ROOT"],
                    "Regions": ["*"],
                },
            }),
        ),
        (
            "all four lists named, two entries each",
            json!({
                "ReportPlanName": "report_wire_all_four",
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[1],
                    "FrameworkArns": [framework_arn("one"), framework_arn("two")],
                    "Accounts": ["ROOT", "111122223333"],
                    "OrganizationUnits": ["ou-1111-22222222", "ou-3333-44444444"],
                    "Regions": [REGION, "us-east-1"],
                },
            }),
        ),
        (
            "lists submitted explicitly empty",
            json!({
                "ReportPlanName": "report_wire_explicitly_empty",
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[5],
                    "FrameworkArns": [],
                    "Accounts": [],
                    "OrganizationUnits": [],
                    "Regions": [],
                },
            }),
        ),
        (
            "a restore-job report under the longest name the model admits",
            json!({
                "ReportPlanName": plan_name_of_length(MAX_NAME_LENGTH),
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[4],
                    "OrganizationUnits": ["ou-5555-66666666"],
                },
            }),
        ),
    ];

    for (case, body) in cases {
        let router = router();
        let name = string_at(&body, "ReportPlanName");
        let (status, errortype, created) = create_plan(&router, &body).await;
        assert_eq!(
            (status, errortype.as_str()),
            (200, "<unset>"),
            "case {case:?}: expected CreateReportPlan 200, got {status} {errortype} {created:?}"
        );

        let (status, _errortype, read) = describe_plan(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeReportPlan 200, got {status} {read:?}"
        );
        let stored = read_plan(&read);
        let setting = at(&stored, "ReportSetting");
        let submitted = at(&body, "ReportSetting");

        for member in SCOPING_LISTS {
            let expected = submitted
                .get(member)
                .cloned()
                .unwrap_or_else(|| json!([]));
            let reported = at(&setting, member);
            assert_eq!(
                reported, expected,
                "case {case:?}: {member} must be reported as a list, got {reported:?} in \
                 {setting:?}"
            );
        }
        assert_eq!(
            at(&stored, "ReportTiming"),
            service_report_timing(),
            "case {case:?}: the plan must report the schedule its report runs on, got {stored:?}"
        );

        // A read changes nothing a later read sees, the lists and the timing
        // included.
        let (status, _errortype, again) = describe_plan(&router, &name).await;
        assert_eq!(
            (status, read_plan(&again)),
            (200, stored.clone()),
            "case {case:?}: a read must not change what a later read reports, got {again:?}"
        );
    }
}

/// RULE over the names a request may carry: the create of a name the account
/// already holds states the already-exists sentence about that name and the
/// account, and the read or the delete of a name it holds none under states the
/// not-found sentence about them.
///
/// The rows differ along what the rule generalises over: the name's length (the
/// shortest and the longest the model admits), its casing, and whether it
/// carries underscores or digits.
#[tokio::test]
async fn the_report_plan_refusals_state_their_sentences_about_the_name_and_the_account() {
    let names = vec![
        ("the shortest name the model admits", "w".to_string()),
        ("a name carrying underscores", "report_wire_refused".to_string()),
        ("a mixed-case name carrying digits", "ReportWire09".to_string()),
        (
            "the longest name the model admits",
            plan_name_of_length(MAX_NAME_LENGTH),
        ),
    ];

    for (case, name) in names {
        let router = router();

        // A name this account holds no plan under: the read and the delete state
        // ONE sentence for it.
        let (status, errortype, missing) = describe_plan(&router, &name).await;
        assert_eq!(
            (
                status,
                errortype.as_str(),
                error_code(&missing).as_str(),
                string_at(&missing, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                "ResourceNotFoundException",
                not_found_message(&name).as_str(),
            ),
            "case {case:?}: a read of an unheld name is refused, got {status} {missing:?}"
        );
        let (status, errortype, undeleted) = delete_plan(&router, &name).await;
        assert_eq!(
            (
                status,
                errortype.as_str(),
                string_at(&undeleted, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                not_found_message(&name).as_str(),
            ),
            "case {case:?}: a delete of an unheld name is refused, got {status} {undeleted:?}"
        );

        // The name taken, a second create of it is refused with its own
        // sentence.
        let body = minimal_body(&name, REPORT_TEMPLATES[2]);
        let (status, _errortype, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup: the first create should succeed, got {status} {created:?}"
        );
        let (status, errortype, refused) = create_plan(&router, &body).await;
        assert_eq!(
            (
                status,
                errortype.as_str(),
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
            ),
            (
                400,
                "AlreadyExistsException",
                "AlreadyExistsException",
                already_exists_message(&name).as_str(),
            ),
            "case {case:?}: a create of a held name is refused, got {status} {refused:?}"
        );
    }
}
