// SPDX-License-Identifier: CC0-1.0

//! `ListReportPlans` driven as real HTTP through the router.
//!
//! The rules under test:
//!
//! * a listing REPORTS the stored report plans: every plan `CreateReportPlan`
//!   inserted appears exactly once, in `ReportPlanName` order, carrying the SAME
//!   body `DescribeReportPlan` answers with for that name — the ARN the create
//!   minted, the description, the setting and the delivery channel as stored, the
//!   report timing, the creation instant and the deployment status — and a plan
//!   the account does not hold (never created, or deleted) appears not at all;
//! * the listing is cut into pages of at most `MaxResults` entries plus an opaque
//!   cursor that resumes exactly where the page stopped, and the pages
//!   concatenate to the whole result set in that one order;
//! * a `MaxResults` outside the model's `@range(1..1000)` is
//!   `InvalidParameterValueException`, and so is a `NextToken` that names no page
//!   of this result set;
//! * a listing is a REPORT: the only state it moves is the deployment the clock
//!   has already made due, so a plan whose due instant is still in the future is
//!   listed in flight however often it is listed, and one whose due instant has
//!   passed is listed deployed.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`. The deployment case STATES the instant the
//! clock owes a plan its deployment at, through the engine it keeps, so it waits
//! for no span and races none.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_backup_cryocontrollerusermanager::state::service_state;

use crate::recovery_point_deletion::router_with_engine;
use crate::vault_and_backup_job::{call, error_code, router, string_at, ACCOUNT, REGION};

/// The deployment statuses a report plan carries, as the model lists them
/// (`smithy_query` shape `ReportPlan`, member `DeploymentStatus`:
/// `CREATE_IN_PROGRESS | UPDATE_IN_PROGRESS | DELETE_IN_PROGRESS | COMPLETED`).
const STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
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

/// The ends of the modelled page-size range (`smithy_query` operation
/// `ListReportPlans`: `MaxResults` `range {min: 1, max: 1000}`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 1000;

/// How far from now the deployment case puts a due instant it wants on one side
/// of the present: generous in both directions, so neither side of the case
/// depends on how fast the host runs.
const WELL_AWAY: Duration = Duration::from_secs(3600);

/// `POST /audit/report-plans`, as the model binds the create.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/audit/report-plans", Some(&body.to_string())).await
}

/// `GET /audit/report-plans/{ReportPlanName}`, as the model binds the read.
async fn describe_plan(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GET", &format!("/audit/report-plans/{name}"), None).await
}

/// `DELETE /audit/report-plans/{ReportPlanName}`, as the model binds the delete.
async fn delete_plan(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/audit/report-plans/{name}"),
        None,
    )
    .await
}

/// `GET /audit/report-plans` — `ListReportPlans`'s HTTP binding, with whatever
/// query string the case under test asks for.
async fn list_plans(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    call(router, "GET", &format!("/audit/report-plans{query}"), None).await
}

/// The `ReportPlans` member of a successful listing.
fn entries(body: &Value) -> Vec<Value> {
    body.get("ReportPlans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `ReportPlanName` of every entry of a page, in the order the page lists
/// them.
fn names(body: &Value) -> Vec<String> {
    entries(body)
        .iter()
        .map(|entry| string_at(entry, "ReportPlanName"))
        .collect()
}

/// One create request: a plan of `name` built from `template`, delivering in
/// `formats` (as submitted, so `None` leaves the member off entirely), covering
/// `frameworks` frameworks, with `description` when it is `Some`.
fn plan_request(
    name: &str,
    template: &str,
    formats: Option<&[&str]>,
    frameworks: usize,
    description: Option<&str>,
) -> Value {
    let mut channel = json!({"S3BucketName": "audit-reports"});
    if let Some(formats) = formats {
        channel["Formats"] = json!(formats);
    }
    let mut request = json!({
        "ReportPlanName": name,
        "ReportDeliveryChannel": channel,
        "ReportSetting": {
            "ReportTemplate": template,
            "FrameworkArns": (0..frameworks)
                .map(|index| json!(format!(
                    "arn:aws:backup:{REGION}:{ACCOUNT}:framework:Covered_{index}-0"
                )))
                .collect::<Vec<Value>>(),
        },
    });
    if let Some(description) = description {
        request["ReportPlanDescription"] = json!(description);
    }
    request
}

/// Create every plan a row asks for and return the create answers, keyed by
/// name.
async fn seed(router: &MultiServiceRouter, requests: &[Value]) -> Vec<(String, Value)> {
    let mut created = Vec::new();
    for request in requests {
        let (status, answer) = create_plan(router, request).await;
        assert_eq!(
            status, 200,
            "setup: expected CreateReportPlan 200 for {request:?}, got {status} {answer:?}"
        );
        created.push((string_at(request, "ReportPlanName"), answer));
    }
    created
}

/// The rows the "report what is stored" table walks: they differ in HOW MANY
/// plans the account holds, in the template each report is built from, in
/// whether the create listed delivery formats at all, in how many frameworks the
/// report covers, in whether a description was given, and in the order the
/// creates ran relative to the name order a listing must report.
fn stored_set_cases() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        ("an account holding no report plan at all", vec![]),
        (
            "one plan, no formats listed, no description, no framework covered",
            vec![plan_request("Alpha", REPORT_TEMPLATES[0], None, 0, None)],
        ),
        (
            "one plan listing both published formats and covering frameworks",
            vec![plan_request(
                "Everything",
                REPORT_TEMPLATES[1],
                Some(&["CSV", "JSON"]),
                3,
                Some("both formats, three frameworks"),
            )],
        ),
        (
            "three plans created in reverse name order",
            vec![
                plan_request(
                    "Zulu",
                    REPORT_TEMPLATES[2],
                    Some(&["JSON"]),
                    0,
                    Some("created first, listed last"),
                ),
                plan_request("Mike", REPORT_TEMPLATES[3], None, 1, None),
                plan_request(
                    "Alfa",
                    REPORT_TEMPLATES[4],
                    Some(&["CSV"]),
                    2,
                    Some("created last, listed first"),
                ),
            ],
        ),
        (
            "names that differ only in case and in digits",
            vec![
                plan_request("Report_2", REPORT_TEMPLATES[5], None, 0, None),
                plan_request("Report_10", REPORT_TEMPLATES[0], Some(&["JSON"]), 1, None),
                plan_request(
                    "report_2",
                    REPORT_TEMPLATES[1],
                    None,
                    0,
                    Some("lower case is a different name"),
                ),
            ],
        ),
        (
            "six plans, template and framework count walking the published sets",
            (0..6)
                .map(|index| {
                    plan_request(
                        &format!("Plan_{index}"),
                        REPORT_TEMPLATES[index % REPORT_TEMPLATES.len()],
                        (index % 3 == 0).then_some(&["CSV", "JSON"][..index % 2 + 1]),
                        index % 4,
                        (index % 2 == 0).then_some("every other row has a description"),
                    )
                })
                .collect(),
        ),
    ]
}

/// Every stored report plan is listed exactly once, in name order, reporting the
/// same body the read of that name reports; nothing else is listed.
#[tokio::test]
async fn a_listing_reports_every_stored_report_plan_once_in_name_order() {
    for (case, requests) in stored_set_cases() {
        // One router per row: the row's plans are the only ones the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        let created = seed(&router, &requests).await;

        let (status, listed) = list_plans(&router, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListReportPlans 200, got {status} {listed:?}"
        );

        let mut expected: Vec<String> = created.iter().map(|(name, _)| name.clone()).collect();
        expected.sort();
        assert_eq!(
            names(&listed),
            expected,
            "case {case:?}: expected every stored plan once in name order, got {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_none(),
            "case {case:?}: one page holds the whole set, so there is no cursor, got {listed:?}"
        );

        for entry in entries(&listed) {
            let name = string_at(&entry, "ReportPlanName");
            let (_, create_answer) = created
                .iter()
                .find(|(created_name, _)| *created_name == name)
                .unwrap_or_else(|| panic!("case {case:?}: {name:?} was never created"));

            assert_eq!(
                entry.get("ReportPlanArn"),
                create_answer.get("ReportPlanArn"),
                "case {case:?}: {name:?} must be listed under the ARN its create minted, \
                 got {entry:?}"
            );
            let prefix = format!("arn:aws:backup:{REGION}:{ACCOUNT}:report-plan:{name}-");
            assert!(
                string_at(&entry, "ReportPlanArn").starts_with(&prefix),
                "case {case:?}: {name:?} expected an ARN starting {prefix:?}, got {entry:?}"
            );
            assert_eq!(
                entry.get("CreationTime"),
                create_answer.get("CreationTime"),
                "case {case:?}: {name:?} must report the creation instant its create \
                 answered with, got {entry:?}"
            );

            // The listing and the describe project ONE stored record, so the
            // whole body a listing reports is the body the read reports.
            let (status, read) = describe_plan(&router, &name).await;
            assert_eq!(
                (status, read.get("ReportPlan")),
                (200, Some(&entry)),
                "case {case:?}: {name:?} listed and described must be the same plan, \
                 got {read:?}"
            );
        }

        // A plan the account does not hold is not listed: the first name of the
        // row is deleted, and a name no create ever took is asked about too.
        if let Some((name, _)) = created.first() {
            let (status, answer) = delete_plan(&router, name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteReportPlan 200 for {name:?}, got {status} \
                 {answer:?}"
            );
            let (status, after) = list_plans(&router, "").await;
            let mut remaining = expected.clone();
            remaining.retain(|held| held != name);
            assert_eq!(
                (status, names(&after)),
                (200, remaining),
                "case {case:?}: a deleted plan must not be listed, got {status} {after:?}"
            );
        }
        assert!(
            !names(&listed).contains(&"NeverCreated".to_string()),
            "case {case:?}: a name no create took must not be listed, got {listed:?}"
        );
    }
}

/// The listing is paged: a page holds at most `MaxResults` entries plus a cursor,
/// and walking the cursors yields the whole result set exactly once in the order
/// one unpaginated call reports.
#[tokio::test]
async fn a_listing_pages_by_max_results_with_a_resumable_cursor() {
    let router = router();
    let requests: Vec<Value> = (0..7)
        .map(|index| {
            plan_request(
                &format!("Paged_{index}"),
                REPORT_TEMPLATES[index % REPORT_TEMPLATES.len()],
                None,
                index % 3,
                None,
            )
        })
        .collect();
    seed(&router, &requests).await;

    let (status, whole) = list_plans(&router, "").await;
    assert_eq!(
        status, 200,
        "expected the unpaginated listing 200, got {status} {whole:?}"
    );
    let expected = names(&whole);
    assert_eq!(
        expected.len(),
        requests.len(),
        "setup: expected every plan in one page, got {whole:?}"
    );

    // Page sizes below, at and above the number of plans held, plus the
    // published maximum.
    for page_size in [1_usize, 2, 3, 7, 8, 1000] {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?MaxResults={page_size}");
        let mut pages = 0;
        loop {
            let (status, page) = list_plans(&router, &query).await;
            assert_eq!(
                status, 200,
                "case page_size={page_size}: expected 200, got {status} {page:?}"
            );
            let page_names = names(&page);
            assert!(
                page_names.len() <= page_size,
                "case page_size={page_size}: a page must not exceed MaxResults, got {page:?}"
            );
            walked.extend(page_names);
            pages += 1;
            assert!(
                pages <= requests.len() + 1,
                "case page_size={page_size}: the walk must terminate, got {walked:?}"
            );
            match page.get("NextToken").and_then(Value::as_str) {
                Some(token) => query = format!("?MaxResults={page_size}&NextToken={token}"),
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case page_size={page_size}: the pages must concatenate to the whole set in order, \
             got {walked:?}"
        );
    }
}

/// A `MaxResults` the model's `@range` admits is served; one outside it is this
/// service's `InvalidParameterValueException`.
#[tokio::test]
async fn a_max_results_outside_the_published_range_is_refused() {
    let router = router();
    seed(
        &router,
        &[plan_request("Bounded", REPORT_TEMPLATES[0], None, 0, None)],
    )
    .await;

    let cases: [(&str, i64, u16); 7] = [
        ("at the published minimum", MAX_RESULTS_MIN, 200),
        ("at the published maximum", MAX_RESULTS_MAX, 200),
        ("inside the range", 42, 200),
        ("one below the published minimum", MAX_RESULTS_MIN - 1, 400),
        ("negative", -1, 400),
        ("one above the published maximum", MAX_RESULTS_MAX + 1, 400),
        ("far above", 100_000, 400),
    ];
    for (case, value, expected) in cases {
        let (status, body) = list_plans(&router, &format!("?MaxResults={value}")).await;
        assert_eq!(
            status, expected,
            "case {case:?} (MaxResults={value}): expected {expected}, got {status} {body:?}"
        );
        if expected == 400 {
            assert_eq!(
                error_code(&body).as_str(),
                "InvalidParameterValueException",
                "case {case:?} (MaxResults={value}): expected the modelled refusal, got {body:?}"
            );
            assert!(
                !string_at(&body, "Message").is_empty(),
                "case {case:?} (MaxResults={value}): the refusal states the condition, \
                 got {body:?}"
            );
        } else {
            assert_eq!(
                names(&body),
                vec!["Bounded".to_string()],
                "case {case:?} (MaxResults={value}): expected the stored plan, got {body:?}"
            );
        }
    }
}

/// A `NextToken` this result set never handed out names no page of it, and is
/// refused rather than silently answering the first page.
#[tokio::test]
async fn a_next_token_outside_this_result_set_is_refused() {
    let router = router();
    seed(
        &router,
        &[plan_request("Cursored", REPORT_TEMPLATES[0], None, 0, None)],
    )
    .await;

    let cases: [(&str, &str); 3] = [
        ("a bare word", "not-a-token"),
        // The hex encoding of a name the account holds no plan under — a cursor
        // of this service's own form pointing at nothing.
        (
            "this service's form naming an unheld plan",
            "4e65766572437265617465",
        ),
        ("odd-length hex", "abc"),
    ];
    for (case, token) in cases {
        let (status, body) = list_plans(&router, &format!("?NextToken={token}")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?} ({token:?}): expected the modelled refusal, got {status} {body:?}"
        );
    }

    // A cursor the listing DID hand out resolves.
    seed(
        &router,
        &[plan_request(
            "Cursored_Two",
            REPORT_TEMPLATES[1],
            Some(&["JSON"]),
            1,
            None,
        )],
    )
    .await;
    let (status, first) = list_plans(&router, "?MaxResults=1").await;
    let token = string_at(&first, "NextToken");
    assert_eq!(
        (status, token.is_empty()),
        (200, false),
        "expected a cursor from the first page, got {status} {first:?}"
    );
    let (status, second) = list_plans(&router, &format!("?MaxResults=1&NextToken={token}")).await;
    assert_eq!(
        (status, names(&second)),
        (200, vec!["Cursored_Two".to_string()]),
        "the cursor must resume at the next plan, got {status} {second:?}"
    );
}

/// Move the instant the stored plan `name` is owed its deployment at, so a case
/// observes the in-flight plan and the deployed one WITHOUT waiting for or
/// racing the span the service chose: a due instant in the future keeps the plan
/// in flight for as long as the case needs, and one in the past is exactly the
/// state the span passing leaves. The handlers read that instant on the next
/// call, so nothing is settled here.
fn given_the_deployment_falls_due_at(
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    due: SystemTime,
) {
    let state = service_state(&**engine).expect("the backup store");
    let mut store = state.lock();
    let plan = store
        .report_plans
        .get_mut(name)
        .expect("the plan the create stored");
    plan.settles_at = due;
}

/// A listing reports the deployment the clock has already made due and nothing
/// else: the plan still owed its deployment is listed in flight however often it
/// is listed, and the plan whose due instant has passed is listed deployed, with
/// every other member of both unchanged.
#[tokio::test]
async fn a_listing_reports_only_the_deployment_the_clock_made_due() {
    let (router, engine) = router_with_engine();
    seed(
        &router,
        &[
            plan_request("Settling_Due", REPORT_TEMPLATES[0], None, 0, None),
            plan_request(
                "Settling_Pending",
                REPORT_TEMPLATES[1],
                Some(&["JSON"]),
                2,
                Some("two of a kind"),
            ),
        ],
    )
    .await;

    let now = SystemTime::now();
    let cases: [(&str, SystemTime, &str); 2] = [
        (
            "Settling_Due",
            now - WELL_AWAY,
            STATUS_COMPLETED,
        ),
        (
            "Settling_Pending",
            now + WELL_AWAY,
            STATUS_CREATE_IN_PROGRESS,
        ),
    ];
    for (name, due, _) in cases {
        given_the_deployment_falls_due_at(&engine, name, due);
    }

    let (status, listed) = list_plans(&router, "").await;
    assert_eq!(
        status, 200,
        "expected ListReportPlans 200, got {status} {listed:?}"
    );
    for (name, _, expected) in cases {
        let entry = entries(&listed)
            .into_iter()
            .find(|entry| string_at(entry, "ReportPlanName") == name)
            .unwrap_or_else(|| panic!("case {name:?}: the stored plan must be listed"));
        assert_eq!(
            string_at(&entry, "DeploymentStatus").as_str(),
            expected,
            "case {name:?}: expected the deployment status its due instant gives, got {entry:?}"
        );
        // The listing and the read of the same name agree, because the listing
        // materialised what the clock owed rather than reporting something of
        // its own.
        let (status, read) = describe_plan(&router, name).await;
        assert_eq!(
            (status, read.get("ReportPlan")),
            (200, Some(&entry)),
            "case {name:?}: listed and described must be the same plan, got {read:?}"
        );
    }

    // Repeating the listing answers exactly the same thing: a read moves
    // nothing a later reader sees.
    let (status, again) = list_plans(&router, "").await;
    assert_eq!(
        (status, &again),
        (200, &listed),
        "two listings taken with the same due instants must agree, got {status} {again:?}"
    );
}
