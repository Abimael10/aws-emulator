// SPDX-License-Identifier: CC0-1.0

//! What a DB INSTANCE's role association and disassociation refuse when the association the
//! request names is already there, or was never made — and the sentence each refusal carries.
//!
//! The rules under test, each asserted over a table spanning what it generalises over rather
//! than over the one request that showed it:
//!
//! 1. One ROLE is associated with one DB instance once. A request naming a role the instance
//!    already acts under — for the feature it was associated for, or for another feature of
//!    the same engine — is the modelled `DBInstanceRoleAlreadyExists`, whose sentence names
//!    the role ARN, the `FeatureName` of the request and the instance, and tells the caller
//!    to check them. The association already in place is the only one the instance reports.
//! 2. A disassociation names an association by the (role, feature) PAIR, so a pair the
//!    instance does not carry — a role it has never been associated with, or one associated
//!    under another feature — is the modelled `DBInstanceRoleNotFound`, whose sentence names
//!    the same three values of the request in the same order.
//!
//! Both sentences are spelled out here rather than taken from the code under test, so the
//! table pins the wording. Every call goes over the wire against this service mounted beside
//! the emulated IAM the trust decision resolves against, exactly as the assembled router
//! mounts them, and the instance is waited for through the one poll loop this test target
//! shares (`lifecycle_poll_loops::poll_until`) rather than for a fixed span.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_error, expect_ok, text};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{associated_roles, emulator, role};

/// The master user every instance below is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Create a DB instance of `engine` and poll `DescribeDBInstances` until it reports
/// `available`, which is the status an association needs.
async fn available_instance(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    class: &str,
    storage: u32,
) {
    let request: Vec<(&str, String)> = params![
        "DBInstanceIdentifier" => identifier,
        "Engine" => engine,
        "DBInstanceClass" => class,
        "AllocatedStorage" => storage,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
    ]
    .to_vec();
    expect_ok(identifier, call(router, "CreateDBInstance", &request).await);
    let described = [("DBInstanceIdentifier", identifier)];
    poll_until(
        identifier,
        "the DB instance",
        || call(router, "DescribeDBInstances", &described),
        |status, body| status == 200 && body.contains("<DBInstanceStatus>available</DBInstanceStatus>"),
    )
    .await;
}

/// `AddRoleToDBInstance` / `RemoveRoleFromDBInstance` for one (instance, role, feature).
async fn role_call(
    router: &MultiServiceRouter,
    action: &str,
    identifier: &str,
    role_arn: &str,
    feature: &str,
) -> (u16, String) {
    call(
        router,
        action,
        &params![
            "DBInstanceIdentifier" => identifier,
            "RoleArn" => role_arn,
            "FeatureName" => feature,
        ],
    )
    .await
}

/// The `AssociatedRoles` a DB instance reports.
async fn instance_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    associated_roles(&described)
}

/// Assert the `Message` of an answered error.
fn expect_message(case: &str, body: &str, expected: &str) {
    let message = text(body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// One row of the second-association table: the feature the role is first associated for,
/// and the feature the second request names.
struct AlreadyRow {
    case: &'static str,
    /// The engine the instance runs, so the rule is asserted across engine families and not
    /// only on the one whose feature names the recorded request used.
    engine: &'static str,
    /// The class and storage that engine is OFFERED with, which differ by family.
    class: &'static str,
    storage: u32,
    /// The `FeatureName` the first, accepted association names.
    held: &'static str,
    /// The `FeatureName` the second, refused one names.
    named: &'static str,
}

/// The rows span what the rule generalises over: a DIFFERENT feature of the same engine and
/// another engine family's feature names, over the `SupportedFeatureNames` each family
/// publishes (`DescribeDBEngineVersions`, which is where a client reads them).
const ALREADY_ROWS: &[AlreadyRow] = &[
    AlreadyRow {
        case: "the same role for another feature of the PostgreSQL engine",
        engine: "postgres",
        class: "db.t3.micro",
        storage: 20,
        held: "s3Import",
        named: "s3Export",
    },
    AlreadyRow {
        case: "the same role for the PostgreSQL engine's Lambda feature",
        engine: "postgres",
        class: "db.t3.micro",
        storage: 20,
        held: "s3Export",
        named: "Lambda",
    },
    AlreadyRow {
        case: "the same role for another feature of a SQL Server engine",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage: 30,
        held: "S3_INTEGRATION",
        named: "SQLSERVER_BACKUP_RESTORE",
    },
];

/// Rule 1: a role a DB instance is already associated with is the modelled already-exists
/// fault, whichever feature the second request names, and the instance still reports exactly
/// the association it had.
#[tokio::test]
async fn a_role_a_db_instance_already_acts_under_is_the_already_exists_fault() {
    let (router, _engine) = emulator();

    for (index, row) in ALREADY_ROWS.iter().enumerate() {
        let identifier = format!("emu-already-{index}");
        available_instance(&router, &identifier, row.engine, row.class, row.storage).await;
        let role_arn = role(
            &router,
            &format!("emu-already-role-{index}"),
            RDS_PRINCIPAL,
            None,
        )
        .await;
        expect_ok(
            row.case,
            role_call(
                &router,
                "AddRoleToDBInstance",
                &identifier,
                &role_arn,
                row.held,
            )
            .await,
        );

        let body = expect_error(
            row.case,
            role_call(
                &router,
                "AddRoleToDBInstance",
                &identifier,
                &role_arn,
                row.named,
            )
            .await,
            400,
            "DBInstanceRoleAlreadyExists",
        );
        expect_message(
            row.case,
            &body,
            &format!(
                "The {role_arn} IAM role or {} feature is already associated with the \
                 {identifier} DB instance. Check the name and try again.",
                row.named
            ),
        );

        let reported = instance_roles(&router, row.case, &identifier).await;
        assert_eq!(
            reported,
            vec![(
                role_arn.clone(),
                row.held.to_string(),
                "ACTIVE".to_string()
            )],
            "case {:?}: expected only the association already in place, got {reported:?}",
            row.case
        );
    }
}

/// One row of the no-such-association table: what the instance carries and what the
/// disassociation names.
struct NotFoundRow {
    case: &'static str,
    /// The `FeatureName` the instance holds the role under, or `None` for a role it has no
    /// association with at all.
    held: Option<&'static str>,
    /// The `FeatureName` the disassociation names.
    named: &'static str,
}

/// The rows span both ways a (role, feature) pair can be absent: the role is associated under
/// ANOTHER feature, and the role is not associated at all.
const NOT_FOUND_ROWS: &[NotFoundRow] = &[
    NotFoundRow {
        case: "a role associated under another feature",
        held: Some("s3Export"),
        named: "s3Import",
    },
    NotFoundRow {
        case: "a role associated under no feature at all",
        held: None,
        named: "s3Import",
    },
    NotFoundRow {
        case: "a role associated under the feature the request does not name",
        held: Some("s3Import"),
        named: "Lambda",
    },
];

/// Rule 2: a disassociation of a (role, feature) pair the instance does not carry is the
/// modelled not-found fault, naming the role, the feature and the instance of the request.
#[tokio::test]
async fn a_pair_the_db_instance_does_not_carry_is_the_role_not_found_fault() {
    let (router, _engine) = emulator();
    let identifier = "emu-pair-not-found";
    available_instance(&router, identifier, "postgres", "db.t3.micro", 20).await;

    for (index, row) in NOT_FOUND_ROWS.iter().enumerate() {
        let role_arn = role(
            &router,
            &format!("emu-pair-role-{index}"),
            RDS_PRINCIPAL,
            None,
        )
        .await;
        if let Some(held) = row.held {
            expect_ok(
                row.case,
                role_call(&router, "AddRoleToDBInstance", identifier, &role_arn, held).await,
            );
        }
        let body = expect_error(
            row.case,
            role_call(
                &router,
                "RemoveRoleFromDBInstance",
                identifier,
                &role_arn,
                row.named,
            )
            .await,
            404,
            "DBInstanceRoleNotFound",
        );
        expect_message(
            row.case,
            &body,
            &format!(
                "The {role_arn} IAM role doesn't match with the {} feature for the \
                 {identifier} DB instance. Check and try again.",
                row.named
            ),
        );
        // The association the instance DID carry is untouched by the refusal, and is taken
        // away again so the next row starts from the same instance.
        if let Some(held) = row.held {
            let reported = instance_roles(&router, row.case, identifier).await;
            assert_eq!(
                reported,
                vec![(
                    role_arn.clone(),
                    held.to_string(),
                    "ACTIVE".to_string()
                )],
                "case {:?}: a refused disassociation changes nothing, got {reported:?}",
                row.case
            );
            expect_ok(
                row.case,
                role_call(&router, "RemoveRoleFromDBInstance", identifier, &role_arn, held).await,
            );
        }
        let reported = instance_roles(&router, row.case, identifier).await;
        assert!(
            reported.is_empty(),
            "case {:?}: expected no association left, got {reported:?}",
            row.case
        );
    }
}
