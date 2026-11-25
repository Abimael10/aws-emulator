// SPDX-License-Identifier: CC0-1.0

//! `DeleteReportPlan`: the namespace the delete resolves its name in, and what a
//! served delete leaves alone.
//!
//! The rule under test: the delete resolves its `ReportPlanName` ONLY among the
//! report plans the account holds, removes exactly that one record, and changes
//! no other resource — the audit frameworks the removed plan's `ReportSetting`
//! covered included, since the frameworks a report covers are references the plan
//! carries and not resources it owns. A name the account holds no report plan
//! under is the one condition the delete refuses (400
//! `ResourceNotFoundException`, stating this service's report-plan not-found
//! sentence), and it is refused even where another kind of resource — an audit
//! framework, a backup vault — carries that very name.
//!
//! The sibling suite `report_plan_deletion.rs` pins what the delete does to the
//! plan it names; this one pins what it does to everything else, which is
//! nothing. The rows differ along what the rule generalises over: the report
//! template, how many frameworks the plan covers, whether the account holds a
//! framework the plan does NOT cover, which kind of resource holds the name no
//! report plan is held under, and whether the account holds another report plan
//! beside it.
//!
//! Every case drives real HTTP through the router; the request harness, the
//! report-plan bindings and the framework helpers are the shared ones of this
//! test binary (`vault_and_backup_job.rs`, `report_plan_deletion.rs`,
//! `framework.rs`, `framework_deletion.rs`), so there is ONE wire path per
//! operation. Nothing here waits for a span: the deployment a fresh framework is
//! owed is waited for by looking again until it is reported.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::framework::{create_framework, describe_framework, text};
use crate::framework_deletion::settled_framework;
use crate::report_plan_deletion::{
    create_plan, delete_plan, describe_plan, read_plan, REPORT_TEMPLATES,
};
use crate::report_plan_wire_form::not_found_message;
use crate::vault_and_backup_job::{call, create_vault, error_code, router, string_at};

/// A published Backup Audit Manager control identifier, so each framework a row
/// creates carries a control AWS names (`smithy_query` shape `Control`, member
/// `ControlName`).
const HELD_CONTROL: &str = "BACKUP_RECOVERY_POINT_ENCRYPTED";

/// A report plan body naming `framework_arns` as the frameworks its report
/// covers; naming none where the list is empty, which is what a report over no
/// framework carries.
fn plan_over(name: &str, template: &str, framework_arns: &[String]) -> Value {
    let mut setting = json!({"ReportTemplate": template});
    if !framework_arns.is_empty() {
        setting["FrameworkArns"] = json!(framework_arns);
        setting["Regions"] = json!(["*"]);
    }
    json!({
        "ReportPlanName": name,
        "ReportDeliveryChannel": {"S3BucketName": "audit-reports"},
        "ReportSetting": setting,
    })
}

/// One row of the covering table.
struct CoveringCase {
    case: &'static str,
    plan: &'static str,
    template: &'static str,
    /// The frameworks the plan's `ReportSetting` names.
    covered: &'static [&'static str],
    /// The frameworks the account holds that the plan does NOT name.
    uncovered: &'static [&'static str],
}

/// The rows the covering table walks: they differ in the report template, in how
/// many frameworks the plan covers (none, one, two) and in whether the account
/// holds a framework the plan leaves out.
fn covering_cases() -> Vec<CoveringCase> {
    vec![
        CoveringCase {
            case: "a resource-compliance report covering one framework",
            plan: "plan_over_one_framework",
            template: REPORT_TEMPLATES[0],
            covered: &["CoveredFrameworkOne"],
            uncovered: &[],
        },
        CoveringCase {
            case: "a control-compliance report covering two frameworks, a third held beside them",
            plan: "plan_over_two_frameworks",
            template: REPORT_TEMPLATES[1],
            covered: &["CoveredFrameworkOne", "CoveredFrameworkTwo"],
            uncovered: &["FrameworkKeptAside"],
        },
        CoveringCase {
            case: "a backup-job report covering no framework at all",
            plan: "plan_over_no_framework",
            template: REPORT_TEMPLATES[2],
            covered: &[],
            uncovered: &["FrameworkKeptAside"],
        },
    ]
}

/// Create one framework per name and answer, for each, its name, the ARN the
/// create minted and the read that reports its settled deployment.
///
/// Every framework is created FIRST, so the deployments the clock owes them fall
/// due together rather than one after the other, and the read is taken once each
/// deployment is reported terminal — so the body a case compares against after
/// the delete is one the clock no longer moves.
async fn held_frameworks(
    router: &MultiServiceRouter,
    case: &str,
    names: &[&str],
) -> Vec<(String, String, Value)> {
    let mut arns = Vec::new();
    for name in names {
        let (status, created) = create_framework(
            router,
            &json!({
                "FrameworkName": name,
                "FrameworkControls": [{"ControlName": HELD_CONTROL}],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateFramework 200 for {name:?}, got {status} {created:?}"
        );
        arns.push(text(&created, "FrameworkArn"));
    }
    let mut held = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let settled = settled_framework(router, name).await;
        held.push(((*name).to_string(), arns[index].clone(), settled));
    }
    held
}

/// A served delete removes the report plan and NOTHING else: every audit
/// framework the account holds — the ones the removed plan's report covered and
/// the ones it did not — reads back member for member as it did before the
/// delete.
#[tokio::test]
async fn deleting_a_report_plan_leaves_every_framework_it_covered_untouched() {
    for row in covering_cases() {
        // One router per row: the row's account holds only what the row created.
        let router = router();
        let mut names: Vec<&str> = row.covered.to_vec();
        names.extend_from_slice(row.uncovered);
        let held = held_frameworks(&router, row.case, &names).await;
        let covered_arns: Vec<String> = held
            .iter()
            .take(row.covered.len())
            .map(|(_, arn, _)| arn.clone())
            .collect();

        let body = plan_over(row.plan, row.template, &covered_arns);
        let (status, _errortype, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreateReportPlan 200, got {status} {created:?}",
            row.case
        );
        // The plan really does cover the frameworks the row named: the count the
        // read reports is the number of frameworks the report covers.
        let (status, _errortype, read) = describe_plan(&router, row.plan).await;
        let stored = read_plan(&read);
        assert_eq!(
            (status, stored["ReportSetting"]["NumberOfFrameworks"].as_u64()),
            (200, Some(row.covered.len() as u64)),
            "case {:?}: the stored plan must cover the row's frameworks, got {status} {stored:?}",
            row.case
        );
        assert_eq!(
            stored["ReportSetting"]["FrameworkArns"],
            json!(covered_arns),
            "case {:?}: the stored plan must name the row's framework ARNs, got {stored:?}",
            row.case
        );

        let (status, errortype, deleted) = delete_plan(&router, row.plan).await;
        assert_eq!(
            (status, errortype.as_str(), deleted.clone()),
            (200, "<unset>", Value::Null),
            "case {:?}: expected DeleteReportPlan 200 with an empty body, got {status} \
             {errortype} {deleted:?}",
            row.case
        );

        // The plan is gone...
        let (status, errortype, missing) = describe_plan(&router, row.plan).await;
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
                not_found_message(row.plan).as_str(),
            ),
            "case {:?}: the deleted plan must be gone, got {status} {missing:?}",
            row.case
        );

        // ...and every framework of the account is exactly as it was, whether
        // the removed plan's report covered it or not.
        for (name, _arn, before) in &held {
            let (status, after) = describe_framework(&router, name).await;
            assert_eq!(
                (status, &after),
                (200, before),
                "case {:?}: framework {name:?} must read back unchanged, got {status} {after:?}",
                row.case
            );
        }
    }
}

/// Which other kind of resource holds the name the delete below is sent for.
enum Holder {
    Framework,
    Vault,
}

/// One row of the namespace table.
struct NamespaceCase {
    case: &'static str,
    /// The name the delete addresses: one no report plan is held under.
    name: &'static str,
    holder: Holder,
    /// A report plan of ANOTHER name the account holds while the delete is made.
    other_plan: Option<&'static str>,
}

/// The rows the namespace table walks: they differ in the kind of resource that
/// holds the name, in the name itself and in whether the account holds a report
/// plan under another name at the time of the delete.
///
/// Every name satisfies both patterns in play, `ReportPlanName`
/// (`^[a-zA-Z][_a-zA-Z0-9]*$`) and `BackupVaultName`
/// (`^[a-zA-Z0-9\-\_]{2,50}$`), so the delete is refused for the NAMESPACE it
/// resolves in rather than for the spelling of the name.
fn namespace_cases() -> Vec<NamespaceCase> {
    vec![
        NamespaceCase {
            case: "an audit framework holds the name and no report plan does",
            name: "SharedAuditLabel",
            holder: Holder::Framework,
            other_plan: None,
        },
        NamespaceCase {
            case: "a backup vault holds the name, one report plan held under another name",
            name: "shared_vault_label",
            holder: Holder::Vault,
            other_plan: Some("plan_kept_elsewhere"),
        },
        NamespaceCase {
            case: "a framework holds the name, one report plan held under another name",
            name: "Shared_Audit_Label_Two",
            holder: Holder::Framework,
            other_plan: Some("plan_kept_beside_it"),
        },
    ]
}

/// The delete resolves its name among REPORT PLANS only: a name the account
/// holds a framework or a backup vault under, but no report plan, is refused the
/// report-plan not-found sentence, and the resource that does hold the name — and
/// any report plan held under another name — is left untouched.
#[tokio::test]
async fn a_delete_resolves_its_name_among_report_plans_only() {
    for row in namespace_cases() {
        // One router per row: the row's account holds only what the row created.
        let router = router();
        match row.holder {
            Holder::Framework => {
                let (status, created) = create_framework(
                    &router,
                    &json!({
                        "FrameworkName": row.name,
                        "FrameworkControls": [{"ControlName": HELD_CONTROL}],
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected CreateFramework 200, got {status} {created:?}",
                    row.case
                );
            }
            Holder::Vault => {
                let (status, created) = create_vault(&router, row.name, "{}").await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected CreateBackupVault 200, got {status} {created:?}",
                    row.case
                );
            }
        }
        if let Some(other) = row.other_plan {
            let (status, _errortype, created) =
                create_plan(&router, &plan_over(other, REPORT_TEMPLATES[3], &[])).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected CreateReportPlan 200 for {other:?}, got {status} {created:?}",
                row.case
            );
        }

        let (status, errortype, refused) = delete_plan(&router, row.name).await;
        assert_eq!(
            (
                status,
                errortype.as_str(),
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
            ),
            (
                400,
                "ResourceNotFoundException",
                "ResourceNotFoundException",
                not_found_message(row.name).as_str(),
            ),
            "case {:?}: a name holding no report plan must be refused as one, got {status} \
             {errortype} {refused:?}",
            row.case
        );

        // The resource that DOES hold the name is still held under it.
        match row.holder {
            Holder::Framework => {
                let (status, kept) = describe_framework(&router, row.name).await;
                assert_eq!(
                    (status, text(&kept, "FrameworkName").as_str()),
                    (200, row.name),
                    "case {:?}: the framework must still be held, got {status} {kept:?}",
                    row.case
                );
            }
            Holder::Vault => {
                let (status, kept) =
                    call(&router, "GET", &format!("/backup-vaults/{}", row.name), None).await;
                assert_eq!(
                    (status, string_at(&kept, "BackupVaultName").as_str()),
                    (200, row.name),
                    "case {:?}: the vault must still be held, got {status} {kept:?}",
                    row.case
                );
            }
        }

        // So is the report plan the account holds under another name.
        if let Some(other) = row.other_plan {
            let (status, _errortype, kept) = describe_plan(&router, other).await;
            assert_eq!(
                (status, string_at(&read_plan(&kept), "ReportPlanName").as_str()),
                (200, other),
                "case {:?}: plan {other:?} must read back, got {status} {kept:?}",
                row.case
            );
        }
    }
}
