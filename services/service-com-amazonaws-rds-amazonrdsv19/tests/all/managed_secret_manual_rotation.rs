// SPDX-License-Identifier: CC0-1.0

//! A MANUAL rotation, asked of SECRETS MANAGER, of the secret RDS manages for a resource's
//! master user password — driven across the two services over one engine.
//!
//! THE RULE: a secret an Amazon Web Services service created and manages for one of its own
//! resources is under that service's MANAGED ROTATION — "For secrets that use managed
//! rotation, omit this field" (`RotateSecretRequest$RotationLambdaARN`), because the
//! credential is the owning service's to re-stake rather than a rotation function's. Three
//! things follow from that, and each is asserted on every row:
//!
//! * `DescribeSecret` reports rotation as turned ON for such a secret, configured by the
//!   service that manages it rather than by any `RotateSecret` call;
//! * a caller's `RotateSecret` that asks for nothing BUT the rotation is SERVED: the owning
//!   service performs it, so the credential the secret holds really changes — which only its
//!   owner may write — the master user it is for does not, and the resource goes on
//!   reporting that same secret;
//! * a `RotateSecret` that would CONFIGURE rotation on it (name a rotation function, a
//!   schedule, or an external rotation role) is still refused with
//!   `InvalidRequestException` and the service-linked-secret sentence, and changes nothing.
//!
//! The rows differ along what the rule generalises over: the kind of resource whose password
//! is managed (a DB instance, a DB cluster), the engine family, and whether the management
//! was turned on by the CREATE or by a later modification — the two paths that mint such a
//! secret. The last test is the other side of the rule: a secret a CALLER created is under no
//! managed rotation at all, so it reports no rotation state and a `RotateSecret` naming no
//! rotation function is refused exactly as before.
//!
//! Every wait here is a POLL of a reporting call through the loop this test target shares
//! ([`poll_until`]), so what ends it is the CONDITION — a transition this service's clock
//! owes — and never a fixed span.
//!
//! Secrets Manager and KMS are mounted beside the RDS service
//! ([`crate::managed_master_user_password::emulator`]), because the secret rotated here is a
//! real secret of this emulator and the rotation is a real call back into RDS.

use aws_sdk_secretsmanager::error::ProvideErrorMetadata;
use aws_sdk_secretsmanager::operation::rotate_secret::RotateSecretError;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use service_com_amazonaws_secretsmanager_secretsmanager::state as sm;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;
use crate::managed_master_user_password::{
    emulator, held_credentials, master_user_secret, secrets_manager, the_resource,
};

/// The status a resource reports once its create has settled.
const AVAILABLE: &str = "available";

/// Which resource a row manages the password of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Instance,
    Cluster,
}

impl Owner {
    fn create(self) -> &'static str {
        match self {
            Self::Instance => "CreateDBInstance",
            Self::Cluster => "CreateDBCluster",
        }
    }

    fn modify(self) -> &'static str {
        match self {
            Self::Instance => "ModifyDBInstance",
            Self::Cluster => "ModifyDBCluster",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Instance => "DescribeDBInstances",
            Self::Cluster => "DescribeDBClusters",
        }
    }

    fn identifier_member(self) -> &'static str {
        match self {
            Self::Instance => "DBInstanceIdentifier",
            Self::Cluster => "DBClusterIdentifier",
        }
    }

    fn element(self) -> &'static str {
        match self {
            Self::Instance => "DBInstance",
            Self::Cluster => "DBCluster",
        }
    }

    /// The member the resource's own lifecycle status travels in.
    fn status_member(self) -> &'static str {
        match self {
            Self::Instance => "DBInstanceStatus",
            Self::Cluster => "Status",
        }
    }
}

/// One row: the resource whose managed secret a caller rotates through Secrets Manager.
struct Row {
    case: &'static str,
    owner: Owner,
    engine: &'static str,
    master_username: &'static str,
    extra: &'static [(&'static str, &'static str)],
    /// `true` for a row that creates the resource WITHOUT management and turns it on with a
    /// later modification, the other path that mints such a secret.
    managed_by_modification: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an Aurora PostgreSQL DB cluster managed from its create",
        owner: Owner::Cluster,
        engine: "aurora-postgresql",
        master_username: "dbadmin",
        extra: &[],
        managed_by_modification: false,
    },
    Row {
        case: "an RDS for MySQL DB instance managed from its create",
        owner: Owner::Instance,
        engine: "mysql",
        master_username: "admin",
        extra: &[("DBInstanceClass", "db.t3.micro"), ("AllocatedStorage", "20")],
        managed_by_modification: false,
    },
    Row {
        case: "an RDS for PostgreSQL DB instance managed by a later modification",
        owner: Owner::Instance,
        engine: "postgres",
        master_username: "pgadmin",
        extra: &[("DBInstanceClass", "db.t4g.micro"), ("AllocatedStorage", "20")],
        managed_by_modification: true,
    },
];

/// The sentence a change to a secret another service manages is refused with, as that
/// service's own state module words it for the owning service RDS is reported as.
fn managed_elsewhere_sentence() -> String {
    sm::service_owned::managed_by_another_service_message(st::service_id())
}

/// Poll the resource's own reporting call until it is available AND the secret it reports has
/// reached `active`, answering the resource envelope it settled on.
async fn poll_until_secret_is_active(
    router: &MultiServiceRouter,
    case: &str,
    owner: Owner,
    identifier: &str,
) -> String {
    let params = [(owner.identifier_member(), identifier.to_string())];
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reporting MasterUserSecret.SecretStatus active"),
        || call(router, owner.describe(), &params),
        |status, body| {
            status == 200
                && optional_text(body, owner.status_member()).as_deref() == Some(AVAILABLE)
                && subtrees(body, "MasterUserSecret")
                    .first()
                    .and_then(|secret| optional_text(secret, "SecretStatus"))
                    .as_deref()
                    == Some(st::MASTER_USER_SECRET_STATUS_ACTIVE)
        },
    )
    .await;
    the_resource(case, &body, owner.element())
}

/// Create the row's resource with its master user password managed by RDS — by the create, or
/// by the modification the row asks for — and answer the ARN of the secret it reports once
/// that secret is active.
async fn managed_resource(router: &MultiServiceRouter, row: &Row, identifier: &str) -> String {
    let mut params: Vec<(&str, String)> = vec![
        (row.owner.identifier_member(), identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("MasterUsername", row.master_username.to_string()),
    ];
    params.extend(
        row.extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    if row.managed_by_modification {
        params.push(("MasterUserPassword", "a-password-of-its-own".to_string()));
    } else {
        params.push(("ManageMasterUserPassword", "true".to_string()));
    }
    expect_ok(row.case, call(router, row.owner.create(), &params).await);
    if row.managed_by_modification {
        // The resource has to be available before it takes a modification, which is the
        // transition its create owes; waited for by polling the reporting call.
        let params = [(row.owner.identifier_member(), identifier.to_string())];
        poll_until(
            row.case,
            &format!("{identifier} reaching {AVAILABLE}"),
            || call(router, row.owner.describe(), &params),
            |status, body| {
                status == 200
                    && optional_text(body, row.owner.status_member()).as_deref() == Some(AVAILABLE)
            },
        )
        .await;
        expect_ok(
            row.case,
            call(
                router,
                row.owner.modify(),
                &[
                    (row.owner.identifier_member(), identifier),
                    ("ManageMasterUserPassword", "true"),
                    ("ApplyImmediately", "true"),
                ],
            )
            .await,
        );
    }
    let settled = poll_until_secret_is_active(router, row.case, row.owner, identifier).await;
    let secret = master_user_secret(row.case, &settled).unwrap_or_else(|| {
        panic!(
            "case {:?}: expected the resource to report a MasterUserSecret, got {settled}",
            row.case
        )
    });
    text(&secret, "SecretArn")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_managed_master_user_secret_is_rotated_through_secrets_manager_by_rds() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-managed-rotate-{index}");
        let secret_arn = managed_resource(&router, row, &identifier).await;
        let before = held_credentials(row.case, &router, &secret_arn).await;
        let before_version = secrets_manager(&router)
            .get_secret_value()
            .secret_id(&secret_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: the managed secret is readable: {error}", row.case)
            })
            .version_id()
            .unwrap_or_default()
            .to_string();

        // 1. The secret reports the ownership AND the rotation of the service that manages
        //    it: rotation is on for it without any caller ever having configured one.
        let described = secrets_manager(&router)
            .describe_secret()
            .secret_id(&secret_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {:?}: Secrets Manager holds no secret at {secret_arn}: {error}",
                    row.case
                )
            });
        assert_eq!(
            described.owning_service(),
            Some(st::service_id()),
            "case {:?}: expected the managed secret to report its owning service, got {:?}",
            row.case,
            described.owning_service()
        );
        assert_eq!(
            described.rotation_enabled(),
            Some(true),
            "case {:?}: expected RotationEnabled true for a secret under managed rotation, \
             got {:?}",
            row.case,
            described.rotation_enabled()
        );
        assert!(
            described.rotation_lambda_arn().is_none(),
            "case {:?}: a secret under managed rotation names no rotation function, got {:?}",
            row.case,
            described.rotation_lambda_arn()
        );
        // The schedule the managing service publishes for such a secret — its own value, not
        // this test's: `st::MANAGED_ROTATION_INTERVAL_DAYS` is where RDS states it.
        assert_eq!(
            described
                .rotation_rules()
                .and_then(|rules| rules.automatically_after_days()),
            Some(st::MANAGED_ROTATION_INTERVAL_DAYS),
            "case {:?}: expected the schedule RDS sets on the secret it manages, got {:?}",
            row.case,
            described.rotation_rules()
        );
        assert!(
            described.next_rotation_date().is_some(),
            "case {:?}: expected the next rotation of a scheduled secret to be reported",
            row.case
        );
        // The AWS-applied tags such a secret carries: the owning service, applied by Secrets
        // Manager itself, and the resource the credential belongs to, applied by RDS.
        let tags: Vec<(&str, &str)> = described
            .tags()
            .iter()
            .map(|tag| (tag.key().unwrap_or_default(), tag.value().unwrap_or_default()))
            .collect();
        assert!(
            tags.contains(&(sm::service_owned::OWNING_SERVICE_TAG, st::service_id())),
            "case {:?}: expected the owning-service tag on the managed secret, got {tags:?}",
            row.case
        );
        assert!(
            described.last_rotated_date().is_some(),
            "case {:?}: expected the credential RDS staked when it created the secret to be \
             reported as its last rotation",
            row.case
        );

        // 2. A caller's rotation that asks for nothing but the rotation is SERVED, and the
        //    owning service performed it: the secret holds a new password — which only its
        //    owner may write — under a new version, for the same master user.
        let rotated = secrets_manager(&router)
            .rotate_secret()
            .secret_id(&secret_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {:?}: expected RotateSecret on the managed secret to be served: \
                     {error}",
                    row.case
                )
            });
        assert_eq!(
            rotated.arn(),
            Some(secret_arn.as_str()),
            "case {:?}: expected the rotation to answer the secret's own ARN, got {:?}",
            row.case,
            rotated.arn()
        );
        assert_eq!(
            rotated.name(),
            described.name(),
            "case {:?}: expected the rotation to answer the secret's own name, got {:?}",
            row.case,
            rotated.name()
        );
        let after = held_credentials(row.case, &router, &secret_arn).await;
        assert_ne!(
            after.password, before.password,
            "case {:?}: expected the rotation to stake a new password in the secret",
            row.case
        );
        assert_eq!(
            after.username, before.username,
            "case {:?}: a rotation keeps the master user it is for, got {:?}",
            row.case, after.username
        );
        assert_eq!(
            st::check_master_user_password(row.engine, &after.password),
            Ok(()),
            "case {:?}: the rotated password must satisfy MasterUserPassword for engine {}",
            row.case,
            row.engine
        );
        let after_version = secrets_manager(&router)
            .get_secret_value()
            .secret_id(&secret_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the rotated secret is readable: {error}",
                    row.case
                )
            })
            .version_id()
            .unwrap_or_default()
            .to_string();
        assert_ne!(
            after_version, before_version,
            "case {:?}: expected the rotation to leave a new version current",
            row.case
        );
        assert_eq!(
            rotated.version_id(),
            Some(after_version.as_str()),
            "case {:?}: expected the rotation to report the version it left current, got {:?}",
            row.case,
            rotated.version_id()
        );

        // 3. And the rotation is recorded as one that COMPLETED, on the one secret the
        //    resource goes on reporting.
        let after_described = secrets_manager(&router)
            .describe_secret()
            .secret_id(&secret_arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {:?}: the secret is described: {error}", row.case));
        assert!(
            after_described.last_rotated_date() > described.last_rotated_date(),
            "case {:?}: expected the completed rotation to move LastRotatedDate, got {:?} \
             after {:?}",
            row.case,
            after_described.last_rotated_date(),
            described.last_rotated_date()
        );
        assert!(
            after_described.next_rotation_date() > described.next_rotation_date(),
            "case {:?}: expected the next rotation to be counted from the one that just \
             completed, got {:?} after {:?}",
            row.case,
            after_described.next_rotation_date(),
            described.next_rotation_date()
        );
        let settled = poll_until_secret_is_active(&router, row.case, row.owner, &identifier).await;
        let reported = master_user_secret(row.case, &settled).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the resource to still report its secret, got {settled}",
                row.case
            )
        });
        assert_eq!(
            text(&reported, "SecretArn"),
            secret_arn,
            "case {:?}: a rotation stakes a new version of the SAME secret",
            row.case
        );
    }
}

/// A `RotateSecret` that would CONFIGURE rotation on a secret RDS manages is still the write
/// of a secret the caller does not own: it is refused, and the credential is left alone.
///
/// The rows differ along which configuration parameter the call carries — the rotation
/// function, the schedule, an external rotation role — because it is CONFIGURING rotation
/// that is refused, not the member that configures it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuring_rotation_on_a_managed_secret_is_refused() {
    let router = emulator();
    let row = &ROWS[0];
    let identifier = "emu-managed-rotate-config";
    let secret_arn = managed_resource(&router, row, identifier).await;
    let held = held_credentials("the managed secret", &router, &secret_arn).await;

    for (case, configured) in [
        ("a rotation function of the caller's", "lambda"),
        ("a rotation schedule", "schedule"),
        ("an external rotation role", "role"),
    ] {
        let asking = secrets_manager(&router)
            .rotate_secret()
            .secret_id(&secret_arn);
        let asking = match configured {
            "lambda" => asking.rotation_lambda_arn(format!(
                "arn:aws:lambda:{}:000000000000:function:a-callers-own-rotator",
                crate::db_instances::REGION
            )),
            "schedule" => asking.rotation_rules(
                aws_sdk_secretsmanager::types::RotationRulesType::builder()
                    .automatically_after_days(30)
                    .build(),
            ),
            _ => asking.external_secret_rotation_role_arn(
                "arn:aws:iam::000000000000:role/a-callers-own-rotation-role",
            ),
        };
        let refused = asking
            .send()
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: expected the configuring call to be refused"));
        let service_error = refused.into_service_error();
        assert!(
            matches!(service_error, RotateSecretError::InvalidRequestException(_)),
            "case {case:?}: expected InvalidRequestException, got {service_error:?}"
        );
        assert_eq!(
            ProvideErrorMetadata::message(&service_error),
            Some(managed_elsewhere_sentence().as_str()),
            "case {case:?}: expected the service-linked-secret sentence, got {service_error:?}"
        );
    }

    // Every refusal was inert: the secret still holds exactly what RDS last staked.
    let after = held_credentials("the managed secret", &router, &secret_arn).await;
    assert_eq!(
        after.password, held.password,
        "case \"a refused configuration\": expected the managed credentials to be left alone"
    );
}

/// The other side of the rule: a secret a CALLER created is under no managed rotation, so it
/// reports no rotation state at all and a `RotateSecret` that names no rotation function is
/// refused for naming none — the documented cause, unchanged by anything above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_callers_own_secret_is_under_no_managed_rotation() {
    let router = emulator();
    let created = secrets_manager(&router)
        .create_secret()
        .name("emu-caller-rotation")
        .secret_string("{\"username\":\"someone\",\"password\":\"theirs\"}")
        .send()
        .await
        .expect("a caller creates a secret of its own");
    let arn = created.arn().expect("the create reports an ARN").to_string();
    let described = secrets_manager(&router)
        .describe_secret()
        .secret_id(&arn)
        .send()
        .await
        .expect("the caller's own secret is described");
    assert_eq!(
        described.rotation_enabled(),
        None,
        "case \"a caller's own secret\": expected no RotationEnabled at all, got {:?}",
        described.rotation_enabled()
    );
    let refused = secrets_manager(&router)
        .rotate_secret()
        .secret_id(&arn)
        .send()
        .await
        .expect_err("case \"a caller's own secret\": expected the rotation to be refused");
    let service_error = refused.into_service_error();
    assert_eq!(
        ProvideErrorMetadata::message(&service_error),
        Some(sm::NO_ROTATION_FUNCTION_MESSAGE),
        "case \"a caller's own secret\": expected the no-rotation-function sentence, got \
         {service_error:?}"
    );
}
