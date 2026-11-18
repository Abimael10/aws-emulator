// SPDX-License-Identifier: CC0-1.0

//! `DeleteReportPlan`: what a delete does to the report plan it names, and when
//! it is refused.
//!
//! The rules under test:
//!
//! * a delete REMOVES the report plan the name addresses, so the read of that
//!   name is afterwards refused exactly as a name that was never created is, the
//!   tag read that addressed the plan's ARN no longer resolves it, a second
//!   delete is refused the same way, and the name is free for a later create to
//!   take — which stores a NEW plan rather than resurrecting the deleted one;
//! * a delete is scoped to the plan it names: every other plan the account holds
//!   reads back unchanged;
//! * a delete REMOVES the plan whatever stage that plan's own deployment has
//!   reached, so a delete arriving while the deployment is still in flight
//!   answers 200 and leaves the name holding no plan — the model binds the
//!   delete as an `@idempotent` `DELETE` answering 200 with an empty body, and
//!   the only condition it refuses is a name the account holds no plan under:
//!   never `ConflictException`, which belongs to a concurrent action on the
//!   plan and not to any span this emulator chose for the deployment, so a
//!   create/use/delete sequence needs no retry and no settling wait;
//! * the one condition the delete DOES refuse states the service's not-found
//!   sentence verbatim — the same sentence the read of an unheld name states
//!   (`report_plan_wire_form.rs`, which owns the spelling).
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`. The in-flight case states when the deployment
//! the clock owes its plan falls due, through the engine it keeps, so it waits
//! for no span and races none.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_backup_cryocontrollerusermanager::state::service_state;

use crate::recovery_point_deletion::router_with_engine;
use crate::report_plan_wire_form::not_found_message;
use crate::vault_and_backup_job::{call_reported, error_code, router, string_at, ACCOUNT};

/// The non-terminal status a fresh report plan's deployment carries, and the
/// terminal one it reaches, as the model lists them (`smithy_query` shape
/// `ReportPlan`, member `DeploymentStatus`: `CREATE_IN_PROGRESS |
/// UPDATE_IN_PROGRESS | DELETE_IN_PROGRESS | COMPLETED`).
const STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const STATUS_COMPLETED: &str = "COMPLETED";

/// How long a poll loop looks for the deployment the CLOCK owes a fresh plan
/// before failing the case.
///
/// Generous on purpose: the transition falls due on the emulator's own clock, so
/// each case waits for the STATUS and this deadline only bounds a plan whose
/// deployment never finishes at all.
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

/// The most characters a `ReportPlanName` may carry, from the member's own
/// length bound (`smithy_query` operation `DeleteReportPlan`: `len 1..256`).
const MAX_NAME_LENGTH: usize = 256;

/// `POST /audit/report-plans`, as the model binds the create.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, String, Value) {
    call_reported(router, "POST", "/audit/report-plans", Some(&body.to_string())).await
}

/// `GET /audit/report-plans/{ReportPlanName}`, as the model binds the read.
async fn describe_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(router, "GET", &format!("/audit/report-plans/{name}"), None).await
}

/// `DELETE /audit/report-plans/{ReportPlanName}` — `DeleteReportPlan`'s HTTP
/// binding.
async fn delete_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(
        router,
        "DELETE",
        &format!("/audit/report-plans/{name}"),
        None,
    )
    .await
}

/// `GET /tags/{ResourceArn}` — the tag read that addresses a resource by ARN.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> (u16, String, Value) {
    call_reported(router, "GET", &format!("/tags/{arn}"), None).await
}

/// The plan body a read reports (`ReportPlan`), or `Value::Null`.
fn read_plan(body: &Value) -> Value {
    body.get("ReportPlan").cloned().unwrap_or(Value::Null)
}

/// Whether a refusal NAMES the report plan the call addressed and the account it
/// was made in, which is what every report-plan-scoped refusal of this service
/// states. The not-found sentence itself is pinned where the read of an unheld
/// name is (`report_plan.rs`), so a delete case asserts the condition and that
/// the sentence is about the plan, rather than keeping a second copy of it.
fn names_the_plan_and_the_account(message: &str, name: &str) -> bool {
    message.contains(name) && message.contains(ACCOUNT)
}

/// A name of `length` characters the `ReportPlanName` pattern admits
/// (`^[a-zA-Z][_a-zA-Z0-9]*$`): a letter first, then letters, digits and
/// underscores, so the length bound is exercised across that whole alphabet.
fn plan_name_of_length(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
    (0..length)
        .map(|i| {
            if i == 0 {
                'd'
            } else {
                char::from(ALPHABET[(i * 5 + length) % ALPHABET.len()])
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

/// Create the plan `body` names, answering its ARN.
async fn create_one_plan(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, _errortype, created) = create_plan(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateReportPlan 200, got {status} {created:?}"
    );
    string_at(&created, "ReportPlanArn")
}

/// Wait for the deployment the clock owes the plan `name` names. Waits for the
/// STATUS rather than for a span: the transition falls due on the emulator's own
/// clock.
async fn wait_for_the_deployment(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, _errortype, read) = describe_plan(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: polling the deployment expected 200, got {status} {read:?}"
        );
        let stored = read_plan(&read);
        if string_at(&stored, "DeploymentStatus") == STATUS_COMPLETED {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to finish within {POLL_DEADLINE:?}, got \
             {stored:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the plan `body` names and wait for the deployment the clock owes it,
/// answering the plan's ARN.
async fn create_settled_plan(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let name = body["ReportPlanName"]
        .as_str()
        .expect("the create body names the plan")
        .to_string();
    let arn = create_one_plan(router, case, body).await;
    wait_for_the_deployment(router, case, &name).await;
    arn
}

/// The rows the delete table walks. They differ along what the rule generalises
/// over: the report template, whether a description, an S3 key prefix, delivery
/// formats, covered frameworks, Regions and tags are stated, and the name's
/// length — a plain one, the shortest the model admits and the published
/// maximum of 256.
fn deleted_plan_cases() -> Vec<(&'static str, Value)> {
    let framework_arn =
        |name: &str| format!("arn:aws:backup:us-west-2:{ACCOUNT}:framework:{name}-1");
    vec![
        (
            "a bare backup-job report: no description, no prefix, no formats",
            minimal_body("report_deleted_bare", REPORT_TEMPLATES[2]),
        ),
        (
            "a resource-compliance report over one framework, both formats, tagged",
            json!({
                "ReportPlanName": "report_deleted_compliance",
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
                "ReportPlanTags": {"team": "audit", "env": "test"},
            }),
        ),
        (
            "a scan-job report under the shortest name the model admits",
            json!({
                "ReportPlanName": plan_name_of_length(1),
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {"ReportTemplate": REPORT_TEMPLATES[5]},
            }),
        ),
        (
            "a control-compliance report under the longest name the model admits",
            json!({
                "ReportPlanName": plan_name_of_length(MAX_NAME_LENGTH),
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports-two",
                    "Formats": ["JSON"],
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[1],
                    "FrameworkArns": [framework_arn("one"), framework_arn("two")],
                    "Accounts": ["ROOT"],
                },
                "ReportPlanTags": {"owner": "backup"},
            }),
        ),
    ]
}

/// A delete removes the report plan: the read of its name is then refused
/// exactly as a name that was never created is, the tag read of its ARN no
/// longer resolves it, a second delete is refused the same way, and a later
/// create may take the name again.
#[tokio::test]
async fn a_deleted_report_plan_is_unreadable_and_its_name_is_free_again() {
    for (case, body) in deleted_plan_cases() {
        // One router per row: the row's account holds nothing another row stored.
        let router = router();
        let name = body["ReportPlanName"]
            .as_str()
            .expect("the create body names the plan")
            .to_string();
        let arn = create_settled_plan(&router, case, &body).await;

        let (status, errortype, deleted) = delete_plan(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str()),
            (200, "<unset>"),
            "case {case:?}: expected DeleteReportPlan 200, got {status} {errortype} {deleted:?}"
        );

        // The read of a deleted name is the refusal a never-created name gets.
        let (status, errortype, missing) = describe_plan(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&missing).as_str()),
            (400, "ResourceNotFoundException", "ResourceNotFoundException"),
            "case {case:?}: a deleted plan must be unreadable, got {status} {missing:?}"
        );
        assert!(
            names_the_plan_and_the_account(&string_at(&missing, "Message"), &name),
            "case {case:?}: the refusal must name the plan and the account, got {missing:?}"
        );

        // The tags the plan carried went with it: its ARN names no taggable
        // resource of this account any more.
        let (status, errortype, untagged) = list_tags(&router, &arn).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&untagged).as_str()),
            (400, "ResourceNotFoundException", "ResourceNotFoundException"),
            "case {case:?}: the deleted plan's ARN must resolve to nothing, got {status} \
             {untagged:?}"
        );

        // So is a second delete of the same name.
        let (status, errortype, again) = delete_plan(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&again).as_str()),
            (400, "ResourceNotFoundException", "ResourceNotFoundException"),
            "case {case:?}: a second delete must be refused, got {status} {again:?}"
        );
        assert_eq!(
            string_at(&again, "Message"),
            string_at(&missing, "Message"),
            "case {case:?}: the delete and the read must state ONE not-found sentence, got \
             {again:?}"
        );

        // The name is free again, and the plan created under it is the NEW one
        // rather than the deleted one.
        let (status, _errortype, recreated) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the name must be free after the delete, got {status} {recreated:?}"
        );
        assert_ne!(
            string_at(&recreated, "ReportPlanArn"),
            arn,
            "case {case:?}: the re-created plan is a new resource, got {recreated:?}"
        );
        let (status, _errortype, read) = describe_plan(&router, &name).await;
        assert_eq!(
            (status, string_at(&read_plan(&read), "ReportPlanArn")),
            (200, string_at(&recreated, "ReportPlanArn")),
            "case {case:?}: the re-created plan reads back, got {status} {read:?}"
        );
    }
}

/// A delete is scoped to the plan it names: the other plans the account holds
/// read back exactly as they were, whichever of them was deleted.
#[tokio::test]
async fn deleting_one_report_plan_leaves_every_other_plan_untouched() {
    // The rows differ in WHICH of the three held plans is deleted, so the rule
    // is asserted over the plan's position in the store rather than over one
    // plan.
    let bodies = [
        minimal_body("report_kept_first", REPORT_TEMPLATES[2]),
        minimal_body("report_kept_middle", REPORT_TEMPLATES[3]),
        minimal_body("report_kept_last", REPORT_TEMPLATES[4]),
    ];
    for deleted_index in 0..bodies.len() {
        let case = format!("deleting plan {deleted_index} of {}", bodies.len());
        let router = router();
        let mut stored = Vec::new();
        // Every plan is created FIRST, so the deployments the clock owes them
        // fall due together rather than one after the other.
        for body in &bodies {
            create_one_plan(&router, &case, body).await;
        }
        for body in &bodies {
            let name = body["ReportPlanName"].as_str().expect("a named plan");
            wait_for_the_deployment(&router, &case, name).await;
            let (_, _errortype, read) = describe_plan(&router, name).await;
            stored.push((name.to_string(), read_plan(&read)));
        }

        let (deleted_name, _) = stored[deleted_index].clone();
        let (status, _errortype, deleted) = delete_plan(&router, &deleted_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteReportPlan 200, got {status} {deleted:?}"
        );

        for (index, (name, before)) in stored.iter().enumerate() {
            let (status, errortype, read) = describe_plan(&router, name).await;
            if index == deleted_index {
                assert_eq!(
                    (status, errortype.as_str()),
                    (400, "ResourceNotFoundException"),
                    "case {case:?}: the deleted plan {name:?} must be gone, got {status} {read:?}"
                );
            } else {
                assert_eq!(
                    (status, read_plan(&read)),
                    (200, before.clone()),
                    "case {case:?}: plan {name:?} must read back unchanged, got {status} {read:?}"
                );
            }
        }
    }
}

/// A delete that arrives while the plan's deployment is still in flight removes
/// the plan all the same: it answers 200 and the name afterwards holds no plan,
/// exactly as the delete of a plan whose deployment has finished does.
#[tokio::test]
async fn a_delete_during_the_deployment_removes_the_plan() {
    // The rows differ in the body the in-flight plan carries, so the rule is
    // not keyed to one shape of plan.
    let cases: Vec<(&str, Value)> = vec![
        (
            "a bare backup-job report",
            minimal_body("report_deploying_bare", REPORT_TEMPLATES[2]),
        ),
        (
            "a described copy-job report delivering as JSON",
            json!({
                "ReportPlanName": "report_deploying_copy",
                "ReportPlanDescription": "copy jobs of the day",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports",
                    "Formats": ["JSON"],
                },
                "ReportSetting": {"ReportTemplate": REPORT_TEMPLATES[3]},
            }),
        ),
    ];

    for (case, body) in cases {
        // The router built on an engine this case KEEPS — the sibling
        // recovery-point suite's seam, since the shared `router()` keeps no
        // handle on its engine — so the case can say when the deployment the
        // clock owes the plan falls due instead of racing the span.
        let (router, engine) = router_with_engine();
        let name = body["ReportPlanName"].as_str().expect("a named plan");
        let arn = create_one_plan(&router, case, &body).await;

        // The plan's deployment is owed at an instant this case states, so it is
        // still in flight when the delete arrives however fast or slow the host
        // runs.
        let far_ahead = SystemTime::now() + Duration::from_secs(3_600);
        given_the_deployment_falls_due_at(&engine, name, far_ahead);
        let (status, _errortype, in_flight) = describe_plan(&router, name).await;
        assert_eq!(
            (
                status,
                string_at(&read_plan(&in_flight), "ReportPlanArn").as_str(),
                string_at(&read_plan(&in_flight), "DeploymentStatus").as_str()
            ),
            (200, arn.as_str(), STATUS_CREATE_IN_PROGRESS),
            "case {case:?}: setup: the plan must still be deploying, got {status} {in_flight:?}"
        );

        let (status, errortype, deleted) = delete_plan(&router, name).await;
        assert_eq!(
            (status, errortype.as_str()),
            (200, "<unset>"),
            "case {case:?}: a delete during the deployment must be served, got {status} \
             {errortype} {deleted:?}"
        );

        let (status, errortype, missing) = describe_plan(&router, name).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&missing).as_str()),
            (400, "ResourceNotFoundException", "ResourceNotFoundException"),
            "case {case:?}: the deleted plan must be unreadable, got {status} {missing:?}"
        );
        assert!(
            names_the_plan_and_the_account(&string_at(&missing, "Message"), name),
            "case {case:?}: the refusal must name the plan and the account, got {missing:?}"
        );
    }
}

/// The rows the no-retry table walks: the body the plan is created from, and how
/// many READS the case makes between the create and the delete.
///
/// That count is the stage of the plan's own deployment the delete arrives at —
/// nothing in the row waits for or polls that deployment, so the delete is sent
/// as fast as the host will send it, which is exactly what a programmatic
/// create/use/delete sequence does. The bodies differ along what the rule
/// generalises over: the report template, whether a description, a key prefix,
/// delivery formats, covered frameworks and tags are stated, and the name's
/// length — a plain one, the shortest the model admits and the published maximum.
fn immediate_delete_cases() -> Vec<(&'static str, Value, usize)> {
    vec![
        (
            "the delete is the very next call after the create",
            minimal_body("report_noretry_bare", REPORT_TEMPLATES[2]),
            0,
        ),
        (
            "one read of the plan between the create and the delete",
            json!({
                "ReportPlanName": "report_noretry_restore",
                "ReportPlanDescription": "restore jobs of the day",
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports",
                    "S3KeyPrefix": "restore/daily",
                    "Formats": ["JSON", "CSV"],
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[4],
                    "FrameworkArns": [
                        format!("arn:aws:backup:us-west-2:{ACCOUNT}:framework:one-1"),
                    ],
                },
                "ReportPlanTags": {"team": "audit"},
            }),
            1,
        ),
        (
            "two reads between the create and the delete, shortest name",
            json!({
                "ReportPlanName": plan_name_of_length(1),
                "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
                "ReportSetting": {"ReportTemplate": REPORT_TEMPLATES[0]},
            }),
            2,
        ),
        (
            "the delete is the very next call, longest name the model admits",
            json!({
                "ReportPlanName": plan_name_of_length(MAX_NAME_LENGTH),
                "ReportDeliveryChannel": {
                    "S3BucketName": "audit-reports-two",
                    "Formats": ["JSON"],
                },
                "ReportSetting": {
                    "ReportTemplate": REPORT_TEMPLATES[1],
                    "Accounts": ["ROOT"],
                },
            }),
            0,
        ),
    ]
}

/// A delete of a plan the account holds is SERVED however recently that plan was
/// created: it answers 200 with an empty body and the plan is gone, so a client
/// never has to retry a teardown. The only condition the delete refuses is a
/// name the account holds no plan under, and it states this service's not-found
/// sentence verbatim for it.
#[tokio::test]
async fn a_delete_following_its_create_is_served_without_a_retry() {
    for (case, body, reads_before_delete) in immediate_delete_cases() {
        // One router per row: the row's account holds nothing another row stored.
        let router = router();
        let name = body["ReportPlanName"]
            .as_str()
            .expect("the create body names the plan")
            .to_string();
        create_one_plan(&router, case, &body).await;

        // Reads only — nothing waits for the deployment the clock owes the plan,
        // so whichever status it reports here the delete below must still be
        // served.
        for read_index in 0..reads_before_delete {
            let (status, _errortype, read) = describe_plan(&router, &name).await;
            let deployment_status = string_at(&read_plan(&read), "DeploymentStatus");
            assert_eq!(
                status, 200,
                "case {case:?}: read {read_index} of the plan expected 200, got {status} {read:?}"
            );
            assert!(
                [STATUS_CREATE_IN_PROGRESS, STATUS_COMPLETED].contains(&deployment_status.as_str()),
                "case {case:?}: read {read_index} must report a modelled deployment status, got \
                 {deployment_status:?}"
            );
        }

        // ONE delete, not a retry loop: the first one must be served.
        let (status, errortype, deleted) = delete_plan(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), deleted.clone()),
            (200, "<unset>", Value::Null),
            "case {case:?}: the first delete must be served with an empty body, got {status} \
             {errortype} {deleted:?}"
        );

        // And the plan is gone: the read of its name states the not-found
        // sentence, as does a further delete of it.
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
            "case {case:?}: the deleted plan must be gone, got {status} {missing:?}"
        );
        let (status, errortype, again) = delete_plan(&router, &name).await;
        assert_eq!(
            (
                status,
                errortype.as_str(),
                error_code(&again).as_str(),
                string_at(&again, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                "ResourceNotFoundException",
                not_found_message(&name).as_str(),
            ),
            "case {case:?}: the delete of a name holding no plan states the not-found sentence, \
             got {status} {again:?}"
        );
    }
}
