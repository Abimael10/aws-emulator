// SPDX-License-Identifier: CC0-1.0

//! A managed master user secret does not outlive the resource whose credential it holds.
//!
//! The RULE under test, asserted over a table of owners rather than one recorded call:
//! the Secrets Manager secret RDS manages for a resource's master user password belongs to
//! that resource, so the delete of the resource deletes the secret too — after which
//! Secrets Manager can no longer resolve it and `DescribeSecret` on its ARN answers the
//! modelled `ResourceNotFoundException` at HTTP 400 with the fixed message that error class
//! carries, instead of the live record.
//!
//! What follows from that one rule, and is asserted on every row:
//!
//! * while the owner is there, its secret is there: `DescribeSecret` answers for it and
//!   reports `OwningService` `rds`;
//! * once the owner's delete is accepted, that same `DescribeSecret` stops answering and
//!   becomes `ResourceNotFoundException` — polled for, because Secrets Manager performs the
//!   deletion itself and nothing here may assert how fast it does;
//! * a resource the delete did not name keeps its own secret, so a delete takes the one
//!   secret it owns and no other.
//!
//! The rows differ along what the rule generalises over: the KIND of owner (DB cluster, DB
//! instance, tenant database — the three resources this service mints a managed secret for),
//! the engine family, and the SHAPE of the delete (skipping the final snapshot or taking
//! one). The second test adds the one owner a delete reaps INDIRECTLY: the member DB
//! instances of a Multi-AZ DB cluster, which "are terminated" with the cluster
//! (`DeleteDBCluster`) and so take their own managed secrets with them.
//!
//! Secrets Manager and KMS are mounted beside the RDS service over one engine
//! ([`crate::managed_master_user_password::emulator`]), because the secret read back here is
//! a real secret of this emulator, asked for through its own operations.

use std::time::{Duration, Instant};

use aws_sdk_secretsmanager::error::ProvideErrorMetadata;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_ok, text};
use crate::managed_master_user_password::{
    emulator, master_user_secret, secrets_manager, the_resource,
};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The message Secrets Manager words `ResourceNotFoundException` with for a secret it
/// cannot resolve: fixed text, with no variable part naming the secret that was asked for.
///
/// `pub(crate)` so the sibling suite that asserts the same refusal over the other axes of
/// the rule reads this one definition rather than writing a second copy of it.
pub(crate) const SECRET_NOT_FOUND: &str = "Secrets Manager can't find the specified secret.";

/// `ResourceNotFoundException` is modelled at HTTP 400 by this service's Smithy model
/// (`smithy_query(query="operation",operation="DescribeSecret")` on the Secrets Manager
/// model). `pub(crate)` for the same reason as [`SECRET_NOT_FOUND`].
pub(crate) const NOT_FOUND_STATUS: u16 = 400;

/// How long a poll below waits for a condition another service's clock makes true: long
/// enough that a loaded machine reaches it, because what is asserted is that it happens at
/// all and never how fast. `pub(crate)` for the same reason as [`SECRET_NOT_FOUND`].
pub(crate) const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again. `pub(crate)` for the same reason as
/// [`SECRET_NOT_FOUND`].
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The settings a Multi-AZ DB cluster requires, as `CreateDBCluster` documents them for
/// that cluster type ("This setting is required to create a Multi-AZ DB cluster").
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "100"),
    ("DBClusterInstanceClass", "db.m6gd.xlarge"),
    ("StorageType", "io1"),
    ("Iops", "1000"),
];

/// Which resource a row's secret belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Cluster,
    Instance,
    /// A tenant database of a multi-tenant DB instance, whose own master user password the
    /// create carries — what is managed here is the TENANT database's.
    TenantDatabase,
}

impl Owner {
    /// The operation that creates the resource carrying the managed secret.
    fn create(self) -> &'static str {
        match self {
            Self::Cluster => "CreateDBCluster",
            Self::Instance => "CreateDBInstance",
            Self::TenantDatabase => "CreateTenantDatabase",
        }
    }

    /// The operation that deletes it.
    fn delete(self) -> &'static str {
        match self {
            Self::Cluster => "DeleteDBCluster",
            Self::Instance => "DeleteDBInstance",
            Self::TenantDatabase => "DeleteTenantDatabase",
        }
    }

    /// The member the identifier of the resource the delete names travels in.
    fn identifier_member(self) -> &'static str {
        match self {
            Self::Cluster => "DBClusterIdentifier",
            Self::Instance | Self::TenantDatabase => "DBInstanceIdentifier",
        }
    }

    /// The element the resource is reported in.
    fn element(self) -> &'static str {
        match self {
            Self::Cluster => "DBCluster",
            Self::Instance => "DBInstance",
            Self::TenantDatabase => "TenantDatabase",
        }
    }
}

/// One row: the owner whose managed secret must go with it, and the delete that retires it.
struct ReapedCase {
    case: &'static str,
    owner: Owner,
    engine: &'static str,
    master_username: &'static str,
    /// The parameters the create of the owning resource adds beyond its identifier, its
    /// engine and its master username.
    extra: &'static [(&'static str, &'static str)],
    /// Whether the delete asks for a final snapshot rather than skipping it — the two
    /// shapes every delete in this service takes.
    final_snapshot: bool,
}

/// The rows: the three kinds of owner this service mints a managed secret for, over four
/// engine families and both shapes of delete.
const REAPED_CASES: &[ReapedCase] = &[
    ReapedCase {
        case: "an Aurora MySQL DB cluster, deleted skipping the final snapshot",
        owner: Owner::Cluster,
        engine: "aurora-mysql",
        master_username: "admin",
        extra: &[],
        final_snapshot: false,
    },
    ReapedCase {
        case: "an Aurora PostgreSQL DB cluster, deleted taking a final snapshot",
        owner: Owner::Cluster,
        engine: "aurora-postgresql",
        master_username: "postgres",
        extra: &[],
        final_snapshot: true,
    },
    ReapedCase {
        case: "an RDS for MySQL DB instance, deleted skipping the final snapshot",
        owner: Owner::Instance,
        engine: "mysql",
        master_username: "admin",
        extra: &[("DBInstanceClass", "db.t3.micro"), ("AllocatedStorage", "20")],
        final_snapshot: false,
    },
    ReapedCase {
        case: "an RDS for PostgreSQL DB instance, deleted taking a final snapshot",
        owner: Owner::Instance,
        engine: "postgres",
        master_username: "postgres",
        extra: &[("DBInstanceClass", "db.t3.micro"), ("AllocatedStorage", "20")],
        final_snapshot: true,
    },
    ReapedCase {
        case: "a tenant database of an Oracle container database, deleted skipping the \
               final snapshot",
        owner: Owner::TenantDatabase,
        engine: "oracle-ee-cdb",
        master_username: "tenantadmin",
        extra: &[("DBInstanceClass", "db.m5.large")],
        final_snapshot: false,
    },
];

/// The managed secret the resource `envelope` reports, as its ARN.
fn reported_secret_arn(case: &str, envelope: &str) -> String {
    let secret = master_user_secret(case, envelope).unwrap_or_else(|| {
        panic!("case {case:?}: expected the resource to report MasterUserSecret, got {envelope}")
    });
    text(&secret, "SecretArn")
}

/// Assert that Secrets Manager still holds the secret `arn` names, and that it is reported
/// as the secret RDS owns.
async fn still_held(case: &str, router: &MultiServiceRouter, arn: &str) {
    let described = secrets_manager(router)
        .describe_secret()
        .secret_id(arn)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: Secrets Manager must still hold the secret at {arn}, got \
                 {}",
                aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    assert_eq!(
        described.owning_service(),
        Some("rds"),
        "case {case:?}: the secret at {arn} must be reported as the one RDS owns, got {:?}",
        described.owning_service()
    );
}

/// The refusal `DescribeSecret` answers for `arn` once the resource that owned it is gone,
/// as `(HTTP status, error code, message)`.
///
/// Polled for the CONDITION rather than waited out: Secrets Manager performs the deletion of
/// a forcibly deleted secret itself, so this looks again until the lookup is refused or
/// [`DEADLINE`] passes.
async fn refused_describe(
    case: &str,
    router: &MultiServiceRouter,
    arn: &str,
) -> (u16, String, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match secrets_manager(router)
            .describe_secret()
            .secret_id(arn)
            .send()
            .await
        {
            Ok(answered) => {
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: DescribeSecret on {arn} must stop answering once the \
                     resource that owned the secret is deleted, got {answered:?}"
                );
            }
            Err(error) => {
                let status = error
                    .raw_response()
                    .map(|response| response.status().as_u16())
                    .unwrap_or_default();
                return (
                    status,
                    error.code().unwrap_or_default().to_string(),
                    error.message().unwrap_or_default().to_string(),
                );
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeDBInstances` until `identifier` reports `available`, which is the state a
/// tenant database can be created in.
async fn available_instance(case: &str, router: &MultiServiceRouter, identifier: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = expect_ok(
            case,
            call(
                router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => identifier],
            )
            .await,
        );
        let instance = the_resource(case, &described, "DBInstance");
        let status = text(&instance, "DBInstanceStatus");
        if status == "available" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB instance {identifier} must reach available, last reported \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the resource a row owns the secret of, and answer its envelope and the identifier
/// the delete names.
async fn created_owner(
    row: &ReapedCase,
    router: &MultiServiceRouter,
    index: usize,
) -> (String, String) {
    let identifier = format!("emu-reaped-{index}");
    let extra = row
        .extra
        .iter()
        .map(|(name, value)| (*name, (*value).to_string()));
    match row.owner {
        Owner::Cluster | Owner::Instance => {
            let mut request: Vec<(&str, String)> = params![
                row.owner.identifier_member() => identifier,
                "Engine" => row.engine,
                "MasterUsername" => row.master_username,
                "ManageMasterUserPassword" => true,
            ]
            .to_vec();
            request.extend(extra);
            let created = expect_ok(row.case, call(router, row.owner.create(), &request).await);
            (
                the_resource(row.case, &created, row.owner.element()),
                identifier,
            )
        }
        Owner::TenantDatabase => {
            // The container database the tenant database lives in carries its OWN master
            // user password, so the only managed secret in play is the tenant database's.
            let mut host: Vec<(&str, String)> = params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => row.engine,
                "MasterUsername" => "oracleadmin",
                "MasterUserPassword" => "the-callers-own-password",
                "MultiTenant" => true,
            ]
            .to_vec();
            host.extend(extra);
            expect_ok(row.case, call(router, "CreateDBInstance", &host).await);
            available_instance(row.case, router, &identifier).await;
            let created = expect_ok(
                row.case,
                call(
                    router,
                    row.owner.create(),
                    &params![
                        "DBInstanceIdentifier" => identifier,
                        "TenantDBName" => tenant_db_name(index),
                        "MasterUsername" => row.master_username,
                        "ManageMasterUserPassword" => true,
                    ],
                )
                .await,
            );
            (
                the_resource(row.case, &created, row.owner.element()),
                identifier,
            )
        }
    }
}

/// The tenant database a tenant-database row creates, whose name is a bare identifier.
fn tenant_db_name(index: usize) -> String {
    format!("emutenant{index}")
}

/// The delete of the resource a row owns the secret of, in the shape that row names.
async fn delete_owner(row: &ReapedCase, router: &MultiServiceRouter, index: usize, identifier: &str) {
    let mut request: Vec<(&str, String)> =
        params![row.owner.identifier_member() => identifier].to_vec();
    if row.owner == Owner::TenantDatabase {
        request.push(("TenantDBName", tenant_db_name(index)));
    }
    if row.final_snapshot {
        request.push((
            "FinalDBSnapshotIdentifier",
            format!("emu-reaped-final-{index}"),
        ));
    } else {
        request.push(("SkipFinalSnapshot", "true".to_string()));
    }
    expect_ok(row.case, call(router, row.owner.delete(), &request).await);
}

/// The secret RDS manages for a resource's master user password goes with that resource:
/// once the delete of the owner is accepted, `DescribeSecret` on the secret's ARN is the
/// modelled `ResourceNotFoundException` at HTTP 400 rather than the live record — while the
/// secret of a resource the delete did not name keeps answering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_the_owner_of_a_managed_secret_takes_the_secret_with_it() {
    let router = emulator();

    // The BYSTANDER: a managed cluster no delete below names, so every row also shows that
    // a delete takes the one secret it owns and no other.
    let bystander = expect_ok(
        "the bystander",
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-reaped-bystander",
                "Engine" => "aurora-mysql",
                "MasterUsername" => "admin",
                "ManageMasterUserPassword" => true,
            ],
        )
        .await,
    );
    let bystander_secret =
        reported_secret_arn("the bystander", &the_resource("the bystander", &bystander, "DBCluster"));

    for (index, row) in REAPED_CASES.iter().enumerate() {
        let (envelope, identifier) = created_owner(row, &router, index).await;
        let secret_arn = reported_secret_arn(row.case, &envelope);

        // While the owner is there, so is its secret.
        still_held(row.case, &router, &secret_arn).await;

        delete_owner(row, &router, index, &identifier).await;

        let refusal = refused_describe(row.case, &router, &secret_arn).await;
        assert_eq!(
            refusal,
            (
                NOT_FOUND_STATUS,
                "ResourceNotFoundException".to_string(),
                SECRET_NOT_FOUND.to_string()
            ),
            "case {:?}: expected HTTP {NOT_FOUND_STATUS} ResourceNotFoundException with the \
             fixed message for the secret of a deleted resource, got {refusal:?}",
            row.case
        );

        // And the resource the delete did not name still has its own secret.
        still_held(row.case, &router, &bystander_secret).await;
    }
}

/// The members of a Multi-AZ DB cluster "are terminated" with the cluster, so a member that
/// manages its own master user password takes its secret with it too — the same rule, reached
/// through the delete of another resource.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_multi_az_db_cluster_takes_its_members_managed_secrets_too() {
    let router = emulator();
    let case = "a Multi-AZ DB cluster and the member that manages its own password";
    let identifier = "emu-reaped-multiaz";

    let mut request: Vec<(&str, String)> = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => "mysql",
        "MasterUsername" => "admin",
        "ManageMasterUserPassword" => true,
    ]
    .to_vec();
    request.extend(
        MULTI_AZ_SETTINGS
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let created = expect_ok(case, call(&router, "CreateDBCluster", &request).await);
    let cluster_secret = reported_secret_arn(case, &the_resource(case, &created, "DBCluster"));

    let member = "emu-reaped-multiaz-1";
    let created_member = expect_ok(
        case,
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => member,
                "DBClusterIdentifier" => identifier,
                "Engine" => "mysql",
                "DBInstanceClass" => "db.m6gd.xlarge",
                "MasterUsername" => "admin",
                "ManageMasterUserPassword" => true,
            ],
        )
        .await,
    );
    let member_secret =
        reported_secret_arn(case, &the_resource(case, &created_member, "DBInstance"));
    assert_ne!(
        member_secret, cluster_secret,
        "case {case:?}: the member must manage a secret of its own, not the cluster's"
    );
    still_held(case, &router, &cluster_secret).await;
    still_held(case, &router, &member_secret).await;

    expect_ok(
        case,
        call(
            &router,
            "DeleteDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "SkipFinalSnapshot" => true,
            ],
        )
        .await,
    );

    for (owner, arn) in [("the cluster", &cluster_secret), ("its member", &member_secret)] {
        let refusal = refused_describe(case, &router, arn).await;
        assert_eq!(
            refusal,
            (
                NOT_FOUND_STATUS,
                "ResourceNotFoundException".to_string(),
                SECRET_NOT_FOUND.to_string()
            ),
            "case {case:?}: expected HTTP {NOT_FOUND_STATUS} ResourceNotFoundException with \
             the fixed message for the secret of {owner}, got {refusal:?}"
        );
    }
}
