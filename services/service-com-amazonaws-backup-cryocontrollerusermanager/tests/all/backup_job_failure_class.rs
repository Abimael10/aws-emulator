// SPDX-License-Identifier: CC0-1.0

//! WHICH failure class a started AWS Backup job settles in when both of the things that can
//! stop a backup are decidable at once: the permissions of the role it runs under, and whether
//! the service that owns the protected resource holds it.
//!
//! RULE under test: a started job's terminal state is decided by the FIRST of those two that
//! genuinely objects — the role's permissions, then the resource's existence — so a role the
//! account's IAM answers as ALLOWING the action that creates the recovery point of the named
//! type is never the reason a job fails, however the grant reached it (an inline document, a
//! managed policy of this account, or an AWS-owned managed policy the account attached), and a
//! job started under such a role for a `ResourceArn` whose owning service reports no such
//! resource settles `FAILED` in the resource-not-found class instead of the denial class. Only
//! a role IAM answers as NOT allowing that action — an explicit deny, or a policy set that
//! allows nothing — fails a job in the denial class, and it does so whether or not the resource
//! exists. Either way the job writes no recovery point and the vault keeps none, while a job
//! nothing objects to completes and lands the point it wrote.
//!
//! The two classes are what a client branches on, so each row asserts BOTH the
//! `MessageCategory` and the whole `StatusMessage`:
//!
//! * the denial class — the sentence IAM words a request no identity-based document allows
//!   with, around the role the request named, the creating action of the resource's type and
//!   the `ResourceArn`;
//! * the resource-not-found class — the sentence AWS words that class with, around the
//!   `ResourceArn` the request named.
//!
//! The rows span what the rule generalises over rather than one request: the FORM the role's
//! permissions take (the AWS-owned policy AWS publishes for the backup half of its service
//! role, a managed policy this account created, an inline document, an inline explicit deny,
//! and no permissions policy at all), crossed with HOW the protected resource stands (held by
//! its owning service, never created, created and then deleted, and of a type whose owning
//! service this emulator cannot ask at all). Over the whole table the category-filtered job
//! listing is read too, so the two classes are separable by a second reader and not only in one
//! job read.
//!
//! Source for the creating action of each type, and for the grants the published policy
//! carries: the per-resource-type grants AWS publishes for the backup role (AWS Backup,
//! *Managed policies*, `AWSBackupServiceRolePolicyForBackup`) — of each type's grants, the one
//! action that CREATES the recovery point.
//!
//! All three services are mounted on ONE engine: IAM, because the role question is put to it
//! over the emulator's own in-process callback and the documents it decides from are the ones
//! `CreateRole`, `PutRolePolicy`, `CreatePolicy` and `AttachRolePolicy` wrote through IAM's own
//! wire; DynamoDB, because the resource question is put to the service that OWNS the table and
//! the tables it answers about are the ones `CreateTable` and `DeleteTable` wrote through its
//! own wire.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::backup_job_under_the_published_service_role::{attach_role_policy, BACKUP_POLICY_ARN};
use crate::plan_and_selection_refusals::{create_role, form_value, put_role_policy};
use crate::restore_job::{drop_table, source_table};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, settled_backup_job, start_job, string_at, ACCOUNT, REGION,
};

/// The vault every job of this suite is started into.
const VAULT: &str = "failure-class-vault";

/// The service principal a backup role admits, so every row's call is accepted and only the
/// role's permissions and the resource decide the outcome. Source: AWS Backup's service
/// principal, as the trust policy of its service role carries it.
const SERVICE_PRINCIPAL: &str = "backup.amazonaws.com";

/// The two terminal `BackupJobState` values a started job reaches here, spelled out rather than
/// imported so a wrong constant in the implementation cannot make this suite pass (model enum
/// `BackupJobState`).
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// The `MessageCategory` of each outcome this suite distinguishes, spelled out here for the
/// same reason (the example strings the model's own documentation of
/// `DescribeBackupJobOutput$MessageCategory` gives, plus the class AWS answers for a protected
/// resource that does not exist).
const SUCCESS: &str = "SUCCESS";
const ACCESS_DENIED: &str = "AccessDenied";
const RESOURCE_NOT_FOUND: &str = "RESOURCE_NOT_FOUND";

/// The sentence this service states for "the recovery point named is not one this account
/// holds" — fixed text, naming no part of the request.
const RECOVERY_POINT_NOT_FOUND: &str = "Cannot find recovery point";

/// The action a backup of a DynamoDB table performs under the job's role, which every row whose
/// resource is a table is decided on.
const TABLE_CREATING_ACTION: &str = "dynamodb:CreateBackup";

/// The action a backup of an Amazon EFS file system performs under the job's role: the row whose
/// owning service this emulator cannot ask is still decided on the ROLE, so its role has to be
/// allowed this.
const FILE_SYSTEM_CREATING_ACTION: &str = "elasticfilesystem:Backup";

/// What the row's role carries as its permissions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InForce {
    /// The AWS-owned policy AWS publishes for the backup half of its service role, attached to
    /// the role — what AWS's own instructions have a client do.
    PublishedPolicy,
    /// A managed policy this ACCOUNT created granting the creating action, attached to the role.
    AccountManagedPolicy,
    /// An inline document on the role granting the creating action.
    InlineAllow,
    /// An inline document on the role DENYING the creating action.
    InlineDeny,
    /// No permissions policy at all: the role carries only its trust document.
    Nothing,
}

/// How the protected resource the row's job is started for stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Resource {
    /// A DynamoDB table the owning service holds.
    TableHeld,
    /// A DynamoDB table ARN of the right shape for a table nothing ever created.
    TableNeverCreated,
    /// A DynamoDB table created through the owning service's wire and then deleted again.
    TableDeleted,
    /// An Amazon EFS file system, a type whose owning service this emulator cannot ask.
    FileSystem,
}

/// The class a row's job must settle in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// `COMPLETED`: nothing objected, and the job landed its recovery point.
    Completed,
    /// `FAILED` in the denial class: the role is not allowed the creating action.
    Denied,
    /// `FAILED` in the resource-not-found class: the owning service holds no such resource.
    NotFound,
}

/// One row: what the role carries, what the resource is, and which class the job must settle in.
struct Case {
    /// What this row is about, named in every assertion of it.
    name: &'static str,
    /// The IAM role name the row creates, unique so each row's documents are its own.
    role_name: &'static str,
    /// The DynamoDB table name the row uses, unique so each row's resource is its own.
    table_name: &'static str,
    in_force: InForce,
    resource: Resource,
    outcome: Outcome,
}

/// The rows: every permission form crossed with the resource standings that make the two
/// failure classes distinguishable.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "the published policy, a table the owning service holds",
            role_name: "class-published-held",
            table_name: "class-held",
            in_force: InForce::PublishedPolicy,
            resource: Resource::TableHeld,
            outcome: Outcome::Completed,
        },
        Case {
            name: "the published policy, a table nothing ever created",
            role_name: "class-published-never",
            table_name: "class-never",
            in_force: InForce::PublishedPolicy,
            resource: Resource::TableNeverCreated,
            outcome: Outcome::NotFound,
        },
        Case {
            name: "the published policy, a table the owning service no longer holds",
            role_name: "class-published-deleted",
            table_name: "class-deleted",
            in_force: InForce::PublishedPolicy,
            resource: Resource::TableDeleted,
            outcome: Outcome::NotFound,
        },
        Case {
            name: "a managed policy of this account, a table the owning service holds",
            role_name: "class-account-managed",
            table_name: "class-account-managed",
            in_force: InForce::AccountManagedPolicy,
            resource: Resource::TableHeld,
            outcome: Outcome::Completed,
        },
        Case {
            name: "an inline grant, a table nothing ever created",
            role_name: "class-inline-never",
            table_name: "class-inline-never",
            in_force: InForce::InlineAllow,
            resource: Resource::TableNeverCreated,
            outcome: Outcome::NotFound,
        },
        Case {
            name: "no permissions policy, a table the owning service holds",
            role_name: "class-bare-held",
            table_name: "class-bare-held",
            in_force: InForce::Nothing,
            resource: Resource::TableHeld,
            outcome: Outcome::Denied,
        },
        Case {
            name: "no permissions policy, a table nothing ever created",
            role_name: "class-bare-never",
            table_name: "class-bare-never",
            in_force: InForce::Nothing,
            resource: Resource::TableNeverCreated,
            outcome: Outcome::Denied,
        },
        Case {
            name: "an inline deny, a table the owning service holds",
            role_name: "class-deny-held",
            table_name: "class-deny-held",
            in_force: InForce::InlineDeny,
            resource: Resource::TableHeld,
            outcome: Outcome::Denied,
        },
        Case {
            name: "the published policy, a file system whose owning service cannot be asked",
            role_name: "class-published-filesystem",
            table_name: "class-published-filesystem",
            in_force: InForce::PublishedPolicy,
            resource: Resource::FileSystem,
            outcome: Outcome::Completed,
        },
    ]
}

impl Case {
    /// The action the backup of this row's resource performs under the role, which the denial of
    /// such a job has to name.
    fn creating_action(&self) -> &'static str {
        match self.resource {
            Resource::FileSystem => FILE_SYSTEM_CREATING_ACTION,
            _ => TABLE_CREATING_ACTION,
        }
    }
}

/// Backup, DynamoDB and IAM over ONE shared engine: the role question is put to IAM and the
/// resource question to the service that owns the resource, both over the emulator's own
/// in-process callback, so both have to be mounted beside Backup for either to answer.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A trust document admitting [`SERVICE_PRINCIPAL`], so every row's role is one AWS Backup can
/// assume and only its permissions decide the role half of the outcome.
fn backup_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A one-statement identity document: `effect` for `action` on every resource, so a row's
/// outcome turns on the ACTION rather than on a resource scope.
fn document(effect: &str, action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": action,
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create a managed policy of THIS account carrying `document`, through IAM's own awsQuery wire,
/// and answer the ARN IAM minted for it — read off the response rather than constructed, so what
/// is attached is the policy IAM holds.
async fn create_policy(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let body = format!(
        "Action=CreatePolicy&Version=2010-05-08&PolicyName={}&PolicyDocument={}",
        form_value(name),
        form_value(document)
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let xml = String::from_utf8(response.into_body().to_vec()).expect("a UTF-8 response body");
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreatePolicy 200, got {status} {xml}"
    );
    let start = xml
        .find("<Arn>")
        .unwrap_or_else(|| panic!("setup {name:?}: CreatePolicy returned no <Arn>: {xml}"))
        + "<Arn>".len();
    let end = xml[start..]
        .find("</Arn>")
        .unwrap_or_else(|| panic!("setup {name:?}: CreatePolicy left <Arn> unclosed: {xml}"));
    xml[start..start + end].to_string()
}

/// Provision the row's role: created through IAM's wire, then given whatever
/// [`Case::in_force`] says it carries. Answers the ARN IAM minted for it.
async fn provisioned_role(router: &MultiServiceRouter, case: &Case) -> String {
    let role_arn = create_role(router, "/", case.role_name, &backup_trust()).await;
    let action = case.creating_action();
    match case.in_force {
        InForce::PublishedPolicy => {
            attach_role_policy(router, case.role_name, BACKUP_POLICY_ARN).await;
        }
        InForce::AccountManagedPolicy => {
            let policy_arn =
                create_policy(router, case.role_name, &document("Allow", action)).await;
            attach_role_policy(router, case.role_name, &policy_arn).await;
        }
        InForce::InlineAllow => {
            put_role_policy(router, case.role_name, &document("Allow", action)).await;
        }
        InForce::InlineDeny => {
            put_role_policy(router, case.role_name, &document("Deny", action)).await;
        }
        InForce::Nothing => {}
    }
    role_arn
}

/// Bring the row's protected resource into the standing [`Case::resource`] names, and answer the
/// `ResourceArn` the job is started for.
async fn provisioned_resource(router: &MultiServiceRouter, case: &Case) -> String {
    match case.resource {
        Resource::TableHeld => source_table(router, case.table_name, false).await,
        Resource::TableDeleted => {
            let arn = source_table(router, case.table_name, false).await;
            drop_table(router, case.table_name).await;
            arn
        }
        Resource::TableNeverCreated => format!(
            "arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{}",
            case.table_name
        ),
        Resource::FileSystem => format!(
            "arn:aws:elasticfilesystem:{REGION}:{ACCOUNT}:file-system/fs-0123456789abcdef0"
        ),
    }
}

/// The denial a job whose role is not allowed `action` on `resource` reports, as IAM words a
/// request no identity-based document allows.
fn denial(role_arn: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {role_arn} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The message AWS words the resource-not-found class with, around the `ResourceArn` the request
/// named.
fn not_found(resource: &str) -> String {
    format!("Requested resource not found: ResourceArn: {resource} not found")
}

/// The `RecoveryPointArn`s the vault holds for `resource_arn`.
async fn points_of(router: &MultiServiceRouter, case: &str, resource_arn: &str) -> Vec<String> {
    let path = format!("/backup-vaults/{VAULT}/recovery-points?resourceArn={resource_arn}");
    let (status, listed) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListRecoveryPointsByBackupVault 200, got {status} {listed:?}"
    );
    listed["RecoveryPoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| string_at(point, "RecoveryPointArn"))
        .collect()
}

/// The `BackupJobId`s one `ListBackupJobs` page reports for `filter`.
async fn listed_jobs(router: &MultiServiceRouter, filter: &str) -> Vec<String> {
    let (status, listed) = call(router, "GET", &format!("/backup-jobs?{filter}"), None).await;
    assert_eq!(
        status, 200,
        "expected ListBackupJobs 200 for {filter:?}, got {status} {listed:?}"
    );
    listed["BackupJobs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|job| string_at(job, "BackupJobId"))
        .collect()
}

/// RULE: a started backup job settles in the class of the first thing that genuinely objects —
/// the role's permissions, then the protected resource's existence — so a role IAM answers as
/// allowing the creating action never fails a job in the denial class, and such a job started
/// for a resource its owning service reports absent settles in the resource-not-found class
/// instead; a role IAM answers as not allowing that action fails the job in the denial class
/// whether the resource exists or not; and either failure leaves the vault no recovery point.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jobs_failure_class_is_the_first_thing_that_genuinely_objects() {
    let router = emulator();
    let (status, created) = create_vault(&router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBackupVault 200, got {status} {created:?}"
    );

    let cases = cases();
    // Every job is started FIRST and read afterwards, so the suite waits out the settle span
    // once rather than once per row.
    let mut started: Vec<(String, String, String, String)> = Vec::new();
    for case in &cases {
        let role_arn = provisioned_role(&router, case).await;
        let resource_arn = provisioned_resource(&router, case).await;
        let body = json!({
            "BackupVaultName": VAULT,
            "ResourceArn": resource_arn,
            "IamRoleArn": role_arn,
        })
        .to_string();
        let (status, job) = start_job(&router, &body).await;
        // The call is ACCEPTED whatever the role allows and whatever the resource turns out to
        // be: a backup that cannot be performed fails later, and this response is not where
        // that shows.
        assert_eq!(
            status, 200,
            "case {:?}: expected StartBackupJob 200, got {status} {job:?}",
            case.name
        );
        started.push((
            string_at(&job, "BackupJobId"),
            string_at(&job, "RecoveryPointArn"),
            role_arn,
            resource_arn,
        ));
    }

    for (case, (job_id, point_arn, role_arn, resource_arn)) in cases.iter().zip(&started) {
        let settled = settled_backup_job(&router, case.name, job_id).await;
        let expected_state = match case.outcome {
            Outcome::Completed => COMPLETED,
            Outcome::Denied | Outcome::NotFound => FAILED,
        };
        let state = string_at(&settled, "State");
        assert_eq!(
            state, expected_state,
            "case {:?}: expected the job to settle {expected_state:?}, got {state:?} in \
             {settled:?}",
            case.name
        );
        let category = string_at(&settled, "MessageCategory");
        let message = settled.get("StatusMessage").and_then(Value::as_str);
        let points = points_of(&router, case.name, resource_arn).await;
        let (point_status, point) = call(
            &router,
            "GET",
            &format!("/backup-vaults/{VAULT}/recovery-points/{point_arn}"),
            None,
        )
        .await;

        match case.outcome {
            Outcome::Completed => {
                assert_eq!(
                    (category.as_str(), message),
                    (SUCCESS, None),
                    "case {:?}: a job nothing objected to reports the success category and no \
                     status message, got {settled:?}",
                    case.name
                );
                assert_eq!(
                    points,
                    vec![point_arn.clone()],
                    "case {:?}: expected the vault to hold the point the job wrote",
                    case.name
                );
                assert_eq!(
                    (point_status, string_at(&point, "Status").as_str()),
                    (200, COMPLETED),
                    "case {:?}: the point the job wrote reads back completed, got {point_status} \
                     {point:?}",
                    case.name
                );
                continue;
            }
            Outcome::Denied => {
                let expected = denial(role_arn, case.creating_action(), resource_arn);
                assert_eq!(
                    (category.as_str(), message),
                    (ACCESS_DENIED, Some(expected.as_str())),
                    "case {:?}: expected the denial class and its sentence, got {settled:?}",
                    case.name
                );
            }
            Outcome::NotFound => {
                let expected = not_found(resource_arn);
                assert_eq!(
                    (category.as_str(), message),
                    (RESOURCE_NOT_FOUND, Some(expected.as_str())),
                    "case {:?}: expected the resource-not-found class and its sentence, got \
                     {settled:?}",
                    case.name
                );
            }
        }
        // A backup that captured nothing wrote no recovery point, so the vault holds none for
        // this resource, and the ARN the start answered with resolves to nothing.
        assert!(
            points.is_empty(),
            "case {:?}: a failed job leaves the vault no recovery point, got {points:?}",
            case.name
        );
        assert_eq!(
            (
                point_status,
                error_code(&point).as_str(),
                string_at(&point, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", RECOVERY_POINT_NOT_FOUND),
            "case {:?}: the recovery point of a failed job resolves to nothing, got \
             {point_status} {point:?}",
            case.name
        );
    }

    // A SECOND reader of the same jobs: the category filter selects exactly the jobs of each
    // class, which is what a client branching on the class reads.
    for (class, outcome) in [
        (SUCCESS, Outcome::Completed),
        (ACCESS_DENIED, Outcome::Denied),
        (RESOURCE_NOT_FOUND, Outcome::NotFound),
    ] {
        let expected: Vec<&String> = cases
            .iter()
            .zip(&started)
            .filter(|(case, _)| case.outcome == outcome)
            .map(|(_, (job_id, _, _, _))| job_id)
            .collect();
        let listed = listed_jobs(&router, &format!("messageCategory={class}")).await;
        assert_eq!(
            listed.len(),
            expected.len(),
            "class {class:?}: expected {} jobs, got {listed:?}",
            expected.len()
        );
        for job_id in expected {
            assert!(
                listed.contains(job_id),
                "class {class:?}: expected the listing to report {job_id:?}, got {listed:?}"
            );
        }
    }
}
