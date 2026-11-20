// SPDX-License-Identifier: CC0-1.0

//! A managed master user secret whose owner is gone is a secret Secrets Manager cannot
//! resolve — however the caller names it, and whichever read asks for it.
//!
//! The RULE under test, asserted over a table rather than over one recorded call: the
//! Secrets Manager secret RDS manages for a resource's master user password belongs to that
//! resource, so the delete of the resource removes the secret, after which Secrets Manager
//! holds no secret under its ARN or its name at all — so EVERY form of `SecretId` that
//! resolved to it while the owner lived (the bare name, the full ARN, the partial ARN) and
//! EVERY read that answered for it resolve to nothing and are refused with the modelled
//! `ResourceNotFoundException` at HTTP 400 carrying the fixed message that error class
//! words, and the secret is no longer listed.
//!
//! `SecretId` is "The ARN or name of the secret", and an ARN may be given complete or
//! partial — "For an ARN, we recommend that you specify a complete ARN rather than a
//! partial ARN"
//! (`smithy_query(query="doc",shape="DescribeSecretRequest",member="SecretId")` on the
//! Secrets Manager model) — so those three forms are what the rule has to hold over, and
//! `DescribeSecret` and `GetSecretValue` are the two reads that answer with a secret the
//! caller names, each modelling `ResourceNotFoundException` as a client error at HTTP 400
//! (`smithy_query(query="operation",operation="DescribeSecret,GetSecretValue")`).
//!
//! What follows from that one rule, and is asserted on every row:
//!
//! * while the owner is there, all three forms resolve through both reads and report the
//!   same secret, and `ListSecrets` reports it — so the forms the refusals below are
//!   asserted on are forms that really did name the secret;
//! * once the owner's delete is accepted, all three forms are refused through both reads
//!   with the same HTTP status, error code and message, and `ListSecrets` reports the secret
//!   no longer;
//! * a secret no delete named keeps answering under all three of its own forms, so what
//!   went is the one secret whose owner went and not the ability to resolve a secret by name
//!   or by a partial ARN.
//!
//! The rows differ along the kind of owner, and the cluster row is the shape a Data API
//! caller provisions — an Aurora Serverless v2 cluster with the HTTP endpoint on, whose
//! master user password RDS manages — because that is the provisioning whose teardown this
//! rule is read through.
//!
//! Secrets Manager and KMS are mounted beside the RDS service
//! ([`crate::managed_master_user_password::emulator`]), because the secret read back here is
//! a real secret of this emulator, asked for through its own operations.

use std::time::Instant;

use aws_sdk_secretsmanager::error::ProvideErrorMetadata;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_ok, text};
use crate::managed_master_user_password::{
    emulator, master_user_secret, secrets_manager, the_resource,
};
// The refusal this rule is asserted against, and the poll that waits for it, are the ones
// the sibling suite defines for the same rule read through `DescribeSecret` alone: the
// fixed message and the modelled status have one definition in this crate's tests.
use crate::managed_secret_reaped_with_its_owner::{
    DEADLINE, NOT_FOUND_STATUS, POLL_INTERVAL, SECRET_NOT_FOUND,
};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The refusal a read answers with, as the three things a client branches on.
macro_rules! refusal_of {
    ($error:expr) => {{
        let error = $error;
        Refusal {
            status: error
                .raw_response()
                .map(|response| response.status().as_u16())
                .unwrap_or_default(),
            code: error.code().unwrap_or_default().to_string(),
            message: error.message().unwrap_or_default().to_string(),
        }
    }};
}

/// The number of random characters a secret's ARN ends in, after a hyphen — the part a
/// PARTIAL ARN drops.
const ARN_SUFFIX_LEN: usize = 6;

/// A read answered: the secret it reported, by the two members both reads carry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    arn: String,
    name: String,
}

/// A read refused: what a client sees of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    status: u16,
    code: String,
    message: String,
}

/// The refusal every read of a secret Secrets Manager cannot resolve answers with.
fn not_found() -> Refusal {
    Refusal {
        status: NOT_FOUND_STATUS,
        code: "ResourceNotFoundException".to_string(),
        message: SECRET_NOT_FOUND.to_string(),
    }
}

/// The two reads that answer with the secret a `SecretId` names.
#[derive(Debug, Clone, Copy)]
enum Read {
    DescribeSecret,
    GetSecretValue,
}

/// Both of them, which every form below is read through.
const READS: &[Read] = &[Read::DescribeSecret, Read::GetSecretValue];

impl Read {
    /// Ask this read for `secret_id`: the secret it reports, or the refusal it answers.
    async fn of(self, router: &MultiServiceRouter, secret_id: &str) -> Result<Resolved, Refusal> {
        match self {
            Self::DescribeSecret => match secrets_manager(router)
                .describe_secret()
                .secret_id(secret_id)
                .send()
                .await
            {
                Ok(answered) => Ok(Resolved {
                    arn: answered.arn().unwrap_or_default().to_string(),
                    name: answered.name().unwrap_or_default().to_string(),
                }),
                Err(error) => Err(refusal_of!(error)),
            },
            Self::GetSecretValue => match secrets_manager(router)
                .get_secret_value()
                .secret_id(secret_id)
                .send()
                .await
            {
                Ok(answered) => Ok(Resolved {
                    arn: answered.arn().unwrap_or_default().to_string(),
                    name: answered.name().unwrap_or_default().to_string(),
                }),
                Err(error) => Err(refusal_of!(error)),
            },
        }
    }
}

/// Which form of `SecretId` names the secret.
#[derive(Debug, Clone, Copy)]
enum IdForm {
    /// The bare secret name.
    Name,
    /// The complete ARN, as the owning resource reported it.
    FullArn,
    /// The ARN without the trailing hyphen and six random characters.
    PartialArn,
}

/// All three, which is what "the ARN or name of the secret" spans.
const ID_FORMS: &[IdForm] = &[IdForm::Name, IdForm::FullArn, IdForm::PartialArn];

impl IdForm {
    /// This form of the secret `resolved` reports.
    fn of(self, case: &str, resolved: &Resolved) -> String {
        match self {
            Self::Name => resolved.name.clone(),
            Self::FullArn => resolved.arn.clone(),
            Self::PartialArn => partial_arn_of_reported_arn(case, &resolved.arn),
        }
    }
}

/// The partial ARN of the `arn` Secrets Manager reported: the ARN with its trailing hyphen
/// and six random characters removed, the form a caller that never saw the full ARN can
/// still build. Fails the case when the reported ARN does not end that way, because then
/// there is no third form to assert the rule over.
fn partial_arn_of_reported_arn(case: &str, arn: &str) -> String {
    let (head, suffix) = arn.rsplit_once('-').unwrap_or_else(|| {
        panic!("case {case:?}: a secret's ARN ends in a hyphen and six characters, got {arn:?}")
    });
    assert_eq!(
        suffix.len(),
        ARN_SUFFIX_LEN,
        "case {case:?}: a secret's ARN ends in {ARN_SUFFIX_LEN} random characters, got {arn:?}"
    );
    head.to_string()
}

/// One row: the resource whose managed secret must go with it.
struct OwnerCase {
    case: &'static str,
    identifier: &'static str,
    /// The operation that creates the owner, and the one that deletes it.
    create: &'static str,
    delete: &'static str,
    /// The member the owner's identifier travels in, and the element it is reported in.
    identifier_member: &'static str,
    element: &'static str,
    engine: &'static str,
    master_username: &'static str,
    /// What the create adds beyond its identifier, its engine, its master username and the
    /// managed master user password.
    extra: &'static [(&'static str, &'static str)],
}

/// The rows: a DB cluster and a DB instance, the two resources a create mints a managed
/// master user secret for on its own. The cluster is the Data API provisioning — Aurora
/// Serverless v2 with the HTTP endpoint on.
const OWNERS: &[OwnerCase] = &[
    OwnerCase {
        case: "an Aurora Serverless v2 DB cluster serving the Data API",
        identifier: "emu-secretid-cluster",
        create: "CreateDBCluster",
        delete: "DeleteDBCluster",
        identifier_member: "DBClusterIdentifier",
        element: "DBCluster",
        engine: "aurora-postgresql",
        master_username: "postgres",
        extra: &[
            ("EnableHttpEndpoint", "true"),
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "2.0"),
        ],
    },
    OwnerCase {
        case: "an RDS for MySQL DB instance",
        identifier: "emu-secretid-instance",
        create: "CreateDBInstance",
        delete: "DeleteDBInstance",
        identifier_member: "DBInstanceIdentifier",
        element: "DBInstance",
        engine: "mysql",
        master_username: "admin",
        extra: &[("DBInstanceClass", "db.t3.micro"), ("AllocatedStorage", "20")],
    },
];

/// Create the row's owner and answer the ARN of the managed secret it reports.
async fn created_owner(row: &OwnerCase, router: &MultiServiceRouter) -> String {
    let mut request: Vec<(&str, String)> = params![
        row.identifier_member => row.identifier,
        "Engine" => row.engine,
        "MasterUsername" => row.master_username,
        "ManageMasterUserPassword" => true,
    ]
    .to_vec();
    request.extend(
        row.extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let created = expect_ok(row.case, call(router, row.create, &request).await);
    let envelope = the_resource(row.case, &created, row.element);
    let secret = master_user_secret(row.case, &envelope).unwrap_or_else(|| {
        panic!(
            "case {:?}: expected the resource to report MasterUserSecret, got {envelope}",
            row.case
        )
    });
    text(&secret, "SecretArn")
}

/// Delete the row's owner, skipping the final snapshot.
async fn delete_owner(row: &OwnerCase, router: &MultiServiceRouter) {
    expect_ok(
        row.case,
        call(
            router,
            row.delete,
            &params![
                row.identifier_member => row.identifier,
                "SkipFinalSnapshot" => true,
            ],
        )
        .await,
    );
}

/// Every secret name `ListSecrets` reports.
async fn listed_names(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let listed = secrets_manager(router)
        .list_secrets()
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: ListSecrets must answer, got {}",
                aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    listed
        .secret_list()
        .iter()
        .map(|entry| entry.name().unwrap_or_default().to_string())
        .collect()
}

/// Assert the secret `resolved` names answers under all three forms of `SecretId` through
/// both reads, reporting itself every time.
async fn resolves_every_way(case: &str, router: &MultiServiceRouter, resolved: &Resolved) {
    for form in ID_FORMS {
        let secret_id = form.of(case, resolved);
        for read in READS {
            let answered = read.of(router, &secret_id).await;
            assert_eq!(
                answered.as_ref(),
                Ok(resolved),
                "case {case:?}: {read:?} on the {form:?} {secret_id:?} must report the \
                 secret, got {answered:?}"
            );
        }
    }
}

/// Wait until `DescribeSecret` stops answering for `secret_id`, the condition the delete of
/// the owner makes true, and assert the refusal it then gives.
///
/// Polled for the CONDITION rather than waited out: nothing here may assert how fast
/// Secrets Manager performs the deletion, only that it happens. The first refusal observed
/// is asserted here, and the forms are then each read at the call site.
async fn wait_until_refused(case: &str, router: &MultiServiceRouter, secret_id: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match Read::DescribeSecret.of(router, secret_id).await {
            Err(refusal) => {
                assert_eq!(
                    refusal,
                    not_found(),
                    "case {case:?}: DescribeSecret on {secret_id:?} must be refused with HTTP \
                     {NOT_FOUND_STATUS} ResourceNotFoundException and the fixed message once \
                     the resource that owned the secret is deleted, got {refusal:?}"
                );
                return;
            }
            Ok(answered) => assert!(
                Instant::now() < deadline,
                "case {case:?}: DescribeSecret on {secret_id:?} must stop answering once the \
                 resource that owned the secret is deleted, got {answered:?}"
            ),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The secret RDS manages for a resource's master user password goes with that resource, so
/// after the delete none of the three forms of `SecretId` that named it resolves through
/// either read: each is the modelled `ResourceNotFoundException` at HTTP 400 with the fixed
/// message, the secret is no longer listed, and a secret no delete named still answers under
/// all three of its own forms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reaped_managed_secret_resolves_under_no_form_of_secret_id() {
    let router = emulator();

    // The BYSTANDER: a secret of the caller's own, which no delete below names.
    let bystander_case = "a secret no delete names";
    let created = secrets_manager(&router)
        .create_secret()
        .name("emu-secretid-bystander-credentials")
        .secret_string(r#"{"username":"bystander","password":"the-callers-own-password"}"#)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {bystander_case:?}: the bystander secret must be created, got {}",
                aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    let bystander = Resolved {
        arn: created.arn().unwrap_or_default().to_string(),
        name: created.name().unwrap_or_default().to_string(),
    };
    resolves_every_way(bystander_case, &router, &bystander).await;

    for row in OWNERS {
        let secret_arn = created_owner(row, &router).await;
        // The secret's own identity, as Secrets Manager reports it: the NAME the other two
        // forms are built beside comes from the service, never from a value written here.
        let resolved = Read::DescribeSecret
            .of(&router, &secret_arn)
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {:?}: Secrets Manager must hold the secret at {secret_arn} while its \
                     owner lives, got {refusal:?}",
                    row.case
                )
            });
        assert_eq!(
            resolved.arn, secret_arn,
            "case {:?}: the secret must report the ARN the resource named, got {:?}",
            row.case, resolved.arn
        );
        resolves_every_way(row.case, &router, &resolved).await;
        let listed = listed_names(row.case, &router).await;
        assert!(
            listed.contains(&resolved.name),
            "case {:?}: ListSecrets must report the secret {:?} while its owner lives, got \
             {listed:?}",
            row.case,
            resolved.name
        );

        delete_owner(row, &router).await;
        wait_until_refused(row.case, &router, &secret_arn).await;

        // The secret is gone, so no form of its id resolves through either read.
        for form in ID_FORMS {
            let secret_id = form.of(row.case, &resolved);
            for read in READS {
                let answered = read.of(&router, &secret_id).await;
                assert_eq!(
                    answered,
                    Err(not_found()),
                    "case {:?}: {read:?} on the {form:?} {secret_id:?} of a secret whose owner \
                     was deleted must be refused with HTTP {NOT_FOUND_STATUS} \
                     ResourceNotFoundException and the fixed message, got {answered:?}",
                    row.case
                );
            }
        }
        let listed = listed_names(row.case, &router).await;
        assert!(
            !listed.contains(&resolved.name),
            "case {:?}: ListSecrets must no longer report the secret {:?} of a deleted \
             resource, got {listed:?}",
            row.case,
            resolved.name
        );

        // And the secret no delete named still answers under every one of its forms.
        resolves_every_way(bystander_case, &router, &bystander).await;
    }
}
