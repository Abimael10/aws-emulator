// SPDX-License-Identifier: CC0-1.0

//! What `StartExportTask` answers for an `IamRoleArn` this account's IAM resolves NO role
//! for, whatever makes it unresolvable.
//!
//! The rule under test: **the KMS grant that encrypts an export is created FOR the role the
//! request names, so an `IamRoleArn` that resolves to no role of this account — it names no
//! IAM role of this account at all, or this account's IAM holds none under the name it
//! carries — is a grant that cannot be created, and is refused with the modelled
//! `KMSKeyNotAccessibleFault` (HTTP 400, `Sender`) worded as the generic "specified KMS key"
//! sentence naming the KEY the request's `KmsKeyId` resolves to — the key the grant would have
//! been created on.** The grant is resolved THROUGH the role, so that refusal is decided
//! before anything about the key itself, and the key's OWN policy decides nothing for such a
//! request: a principal that was never resolved cannot be authorized or refused by it, so the
//! refusal is never worded as the key's own inaccessibility.
//!
//! The rows span what the rule generalises over — the forms a value that resolves to no role
//! of this account takes (a bare role name, a value that is no ARN at all, an IAM ARN of this
//! account that names no role, an IAM-shaped ARN carrying a Region, an ARN of another
//! service, an ARN of another account, and a well-formed role ARN of this account IAM holds
//! nothing under) — crossed with every spelling the request may name the one key by, and with
//! a reference KMS resolves no key for, which the sentence can only name as the request
//! carried it and which is answered on the grant all the same. Two controls keep the
//! neighbouring rules where they are: the export role this account really holds is ACCEPTED,
//! and that same resolved role against a key whose own policy authorizes it for nothing is
//! refused with the sentence a key that is itself unusable carries, about the reference the
//! request carried.
//!
//! Every call goes over the wire as a real awsQuery form POST, with IAM, KMS and Amazon S3
//! mounted beside this service on ONE engine exactly as the assembled router mounts them, and
//! the snapshot the export reads is waited for through this target's own poll loop.

use crate::db_instances::{call, expect_error, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    available_instance, create_bucket, emulator, export_role_policy, kms, role,
};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An account that is NOT this emulator's, for the row naming a role of another account.
const OTHER_ACCOUNT: &str = "123456789012";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The sentence the grant that could not be created is refused with, spelled out here so the
/// table pins the wording rather than the code under test: its variable part is the KEY the
/// request's reference resolved to.
fn grant_not_created(key: &str) -> String {
    format!(
        "The specified KMS key [{key}] does not exist, is not enabled or you do not have \
         permissions to access it."
    )
}

/// The sentence a key that is itself unusable carries, likewise spelled out here: the row
/// that keeps the key's own wording where it is.
fn key_not_accessible(key_id: &str) -> String {
    format!("The KMS key {key_id} doesn't exist or is disabled.")
}

/// A key policy whose ONE statement decides about `principal` alone: in KMS "no AWS
/// principal has any permissions to a KMS key unless the key policy allows it", so a key
/// carrying this policy authorizes every OTHER principal for nothing.
fn key_policy_allowing(principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":"kms:*","Resource":"*"}}]}}"#
    )
}

/// Which spelling of the encryption key a row passes — `KmsKeyId` accepts "the key ARN, key
/// ID, alias ARN, or alias name" (source: that member's own documentation), and all four of
/// them name the one key the sentence is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyReference {
    /// The bare key id KMS minted.
    Id,
    /// The key's own ARN.
    Arn,
    /// The name of an alias this fixture pointed at that key.
    AliasName,
    /// That alias's own ARN.
    AliasArn,
    /// A key ARN KMS holds nothing under: the reference that resolves to NO key, which the
    /// sentence can only name as the request carried it.
    Unresolvable,
}

/// What a row passes as `IamRoleArn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleReference {
    /// The NAME of the export role this fixture really created, rather than its ARN: IAM
    /// holds the role, and the value names no IAM role ARN at all, so nothing resolves a
    /// principal from it.
    TheExportRolesBareName,
    /// A value that is no ARN of any form.
    NoArnAtAll,
    /// An IAM ARN of this account whose resource names an IAM entity that is not a role.
    AnotherEntityOfThisAccount,
    /// A role ARN carrying a Region, which no IAM ARN does.
    RoleArnWithARegion,
    /// An STS assumed-role session ARN: an ARN of another service.
    AnotherServicesArn,
    /// A well-formed role ARN of ANOTHER account, whose roles the only IAM this emulator can
    /// ask holds nothing of.
    OfAnotherAccount,
    /// A well-formed role ARN of THIS account that IAM holds no role under.
    AbsentHere,
    /// The export role this fixture really created, by its ARN: the resolved principal the
    /// two controls use.
    TheExportRole,
}

/// One row: the value the export is asked to run under, the spelling of the key, and the
/// identifier the call names.
struct Row {
    case: &'static str,
    identifier: &'static str,
    role: RoleReference,
    key: KeyReference,
}

/// The rows the generic sentence is asserted over: every form of a value that resolves to no
/// role of this account, crossed with the spellings the one key is named by.
const ROWS: &[Row] = &[
    Row {
        case: "the bare NAME of a role IAM really holds",
        identifier: "emu-unresolved-bare-name",
        role: RoleReference::TheExportRolesBareName,
        key: KeyReference::Arn,
    },
    Row {
        case: "the bare NAME of a role IAM really holds, with the key named by its id",
        identifier: "emu-unresolved-bare-name-id",
        role: RoleReference::TheExportRolesBareName,
        key: KeyReference::Id,
    },
    Row {
        case: "a value that is no ARN at all",
        identifier: "emu-unresolved-no-arn",
        role: RoleReference::NoArnAtAll,
        key: KeyReference::Arn,
    },
    Row {
        case: "an IAM ARN of this account naming an entity that is no role",
        identifier: "emu-unresolved-other-entity",
        role: RoleReference::AnotherEntityOfThisAccount,
        key: KeyReference::Id,
    },
    Row {
        case: "a role ARN carrying a Region, which no IAM ARN does",
        identifier: "emu-unresolved-regional-arn",
        role: RoleReference::RoleArnWithARegion,
        key: KeyReference::Arn,
    },
    Row {
        case: "an ARN of another service",
        identifier: "emu-unresolved-other-service",
        role: RoleReference::AnotherServicesArn,
        key: KeyReference::Id,
    },
    Row {
        case: "a well-formed role ARN of another account",
        identifier: "emu-unresolved-other-account",
        role: RoleReference::OfAnotherAccount,
        key: KeyReference::Arn,
    },
    Row {
        case: "a role ARN of this account IAM holds nothing under",
        identifier: "emu-unresolved-absent-here",
        role: RoleReference::AbsentHere,
        key: KeyReference::Id,
    },
    Row {
        case: "a role IAM holds nothing under, with the key named by an alias",
        identifier: "emu-unresolved-absent-alias",
        role: RoleReference::AbsentHere,
        key: KeyReference::AliasName,
    },
    Row {
        case: "a role IAM holds nothing under, with the key named by that alias's ARN",
        identifier: "emu-unresolved-absent-alias-arn",
        role: RoleReference::AbsentHere,
        key: KeyReference::AliasArn,
    },
    Row {
        case: "a role IAM holds nothing under, with a key reference KMS resolves nothing for",
        identifier: "emu-unresolved-absent-and-no-key",
        role: RoleReference::AbsentHere,
        key: KeyReference::Unresolvable,
    },
];

/// Everything these rows need: the snapshot the export reads, the destination bucket, the
/// encryption key in every spelling a request may name it by, and the one role IAM really
/// holds.
struct Fixture {
    router: MultiServiceRouter,
    bucket: String,
    snapshot_arn: String,
    key_id: String,
    key_arn: String,
    alias_name: String,
    alias_arn: String,
    export_role_name: String,
    export_role_arn: String,
}

/// A bare key id KMS holds nothing under, for the row whose reference resolves to no key at
/// all.
const UNRESOLVABLE_KEY: &str = "11111111-2222-3333-4444-555555555555";

impl Fixture {
    /// The `KmsKeyId` a row passes.
    fn key(&self, reference: KeyReference) -> &str {
        match reference {
            KeyReference::Id => &self.key_id,
            KeyReference::Arn => &self.key_arn,
            KeyReference::AliasName => &self.alias_name,
            KeyReference::AliasArn => &self.alias_arn,
            KeyReference::Unresolvable => UNRESOLVABLE_KEY,
        }
    }

    /// The key the sentence NAMES for that reference: the key KMS resolves it to, and the
    /// reference itself where KMS resolves nothing for it.
    fn named_key(&self, reference: KeyReference) -> &str {
        match reference {
            KeyReference::Unresolvable => UNRESOLVABLE_KEY,
            _ => &self.key_arn,
        }
    }

    /// The `IamRoleArn` a row passes.
    fn role(&self, reference: RoleReference) -> String {
        match reference {
            RoleReference::TheExportRolesBareName => self.export_role_name.clone(),
            RoleReference::NoArnAtAll => "emu-unresolved/not-an-arn".to_string(),
            RoleReference::AnotherEntityOfThisAccount => {
                format!("arn:aws:iam::{ACCOUNT}:user/{}", self.export_role_name)
            }
            RoleReference::RoleArnWithARegion => {
                format!("arn:aws:iam:{REGION}:{ACCOUNT}:role/{}", self.export_role_name)
            }
            RoleReference::AnotherServicesArn => format!(
                "arn:aws:sts::{ACCOUNT}:assumed-role/{}/emu-session",
                self.export_role_name
            ),
            RoleReference::OfAnotherAccount => {
                format!("arn:aws:iam::{OTHER_ACCOUNT}:role/emu-unresolved-export-role")
            }
            RoleReference::AbsentHere => {
                format!("arn:aws:iam::{ACCOUNT}:role/emu-unresolved-no-such-role")
            }
            RoleReference::TheExportRole => self.export_role_arn.clone(),
        }
    }

    /// The parameters of one `StartExportTask` call.
    fn request(
        &self,
        identifier: &str,
        role_arn: &str,
        key_id: &str,
    ) -> [(&'static str, String); 5] {
        params![
            "ExportTaskIdentifier" => identifier,
            "SourceArn" => self.snapshot_arn,
            "S3BucketName" => self.bucket,
            "IamRoleArn" => role_arn,
            "KmsKeyId" => key_id,
        ]
    }
}

/// Stand the fixture up: an available DB instance, an available snapshot of it, the
/// destination bucket, the encryption key and the export role that trusts the export
/// principal and may write that bucket.
async fn fixture() -> Fixture {
    let (router, _engine) = emulator();
    let instance = "emu-unresolved-source";
    available_instance(&router, instance, "mysql").await;

    let snapshot = "emu-unresolved-snapshot";
    expect_ok(
        "snapshot the source",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    // The snapshot is `creating` when the call answers and an export reads one that is
    // available: the suite waits for that CONDITION rather than for a span.
    let described = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        "snapshot the source",
        "the DB snapshot",
        || call(&router, "DescribeDBSnapshots", &described),
        |status, body| status == 200 && body.contains("<Status>available</Status>"),
    )
    .await;

    let bucket = "emu-unresolved-destination";
    create_bucket(&router, bucket, REGION).await;
    let created = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key");
    let metadata = created
        .key_metadata()
        .expect("the created key reports its metadata");
    // The other two spellings `KmsKeyId` accepts name the key through an ALIAS, which is a
    // record KMS holds and resolves to the same key.
    let alias_name = "alias/emu-unresolved-export-key";
    kms(&router)
        .create_alias()
        .alias_name(alias_name)
        .target_key_id(metadata.key_id())
        .send()
        .await
        .expect("the emulated KMS points the alias at the export key");
    let export_role_name = "emu-unresolved-export-role";
    let export_role_arn = role(
        &router,
        export_role_name,
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(bucket)),
    )
    .await;

    Fixture {
        bucket: bucket.to_string(),
        snapshot_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}"),
        key_id: metadata.key_id().to_string(),
        key_arn: metadata
            .arn()
            .expect("the created key reports its ARN")
            .to_string(),
        alias_name: alias_name.to_string(),
        alias_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:{alias_name}"),
        export_role_name: export_role_name.to_string(),
        export_role_arn,
        router,
    }
}

/// Every row: a value that resolves to no role of this account is the modelled key fault
/// with the generic sentence naming the key the request passed, and nothing of the refused
/// export is recorded.
#[tokio::test]
async fn an_iam_role_arn_that_resolves_to_no_role_is_the_failed_grant() {
    let fixture = fixture().await;

    for row in ROWS {
        let case = row.case;
        let key = fixture.key(row.key).to_string();
        let answered = call(
            &fixture.router,
            "StartExportTask",
            &fixture.request(row.identifier, &fixture.role(row.role), &key),
        )
        .await;

        let body = expect_error(case, answered, 400, "KMSKeyNotAccessibleFault");
        let message = text(&body, "Message");
        // Whichever spelling the row passed, it names the ONE key this fixture created, so
        // the sentence names that key's ARN; a reference KMS resolves nothing for leaves the
        // sentence naming the reference itself.
        let expected = grant_not_created(fixture.named_key(row.key));
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?} in {body}"
        );
        let kind = text(&body, "Type");
        assert_eq!(
            kind, "Sender",
            "case {case:?}: expected a Sender fault, got {kind:?} in {body}"
        );
        // A refused export records no task: the region holds nothing under the identifier it
        // named.
        let reported = expect_ok(
            case,
            call(
                &fixture.router,
                "DescribeExportTasks",
                &params!["ExportTaskIdentifier" => row.identifier],
            )
            .await,
        );
        assert!(
            texts(&reported, "ExportTaskIdentifier").is_empty(),
            "case {case:?}: a refused export records no task, got {reported}"
        );
    }
}

/// The first control: the same request under the role's own ARN — a principal IAM resolves —
/// is accepted, so the table above cannot pass against a handler that refuses every request.
#[tokio::test]
async fn the_resolved_export_role_is_accepted() {
    let fixture = fixture().await;
    let identifier = "emu-unresolved-accepted";
    let body = expect_ok(
        "the resolved export role",
        call(
            &fixture.router,
            "StartExportTask",
            &fixture.request(
                identifier,
                &fixture.role(RoleReference::TheExportRole),
                fixture.key(KeyReference::Arn),
            ),
        )
        .await,
    );
    assert_eq!(
        text(&body, "ExportTaskIdentifier"),
        identifier,
        "the accepted export reports the identifier it was started under, got {body}"
    );
    // And it is the stored task that reports it back, through the reporting operation.
    let reported = expect_ok(
        "the accepted export is recorded",
        call(
            &fixture.router,
            "DescribeExportTasks",
            &params!["ExportTaskIdentifier" => identifier],
        )
        .await,
    );
    assert_eq!(
        texts(&reported, "ExportTaskIdentifier"),
        vec![identifier.to_string()],
        "the accepted export is reported by DescribeExportTasks, got {reported}"
    );
}

/// The second control: once the principal IS resolved, the key's own policy decides again —
/// a key that authorizes the export role for nothing is refused as the key's own
/// inaccessibility, in the other sentence that fault carries.
#[tokio::test]
async fn a_resolved_role_leaves_the_keys_own_policy_to_decide() {
    let fixture = fixture().await;
    // A second role, so the key's one statement decides about a principal that is NOT the
    // export's.
    let other_role = role(
        &fixture.router,
        "emu-unresolved-key-owner",
        st::EXPORT_SERVICE_PRINCIPAL,
        None,
    )
    .await;
    let closed_key = kms(&fixture.router)
        .create_key()
        .description("a key whose policy decides only about another role")
        .policy(key_policy_allowing(&other_role))
        // A policy naming a principal other than this account's own calls for the lockout
        // check to be bypassed, exactly as it does against the real service.
        .bypass_policy_lockout_safety_check(true)
        .send()
        .await
        .expect("the emulated KMS creates the key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");

    let body = expect_error(
        "a key whose policy authorizes the export role for nothing",
        call(
            &fixture.router,
            "StartExportTask",
            &fixture.request(
                "emu-unresolved-closed-key",
                &fixture.role(RoleReference::TheExportRole),
                &closed_key,
            ),
        )
        .await,
        400,
        "KMSKeyNotAccessibleFault",
    );
    let message = text(&body, "Message");
    let expected = key_not_accessible(&closed_key);
    assert_eq!(
        message, expected,
        "a key whose policy authorizes the export role for nothing keeps its own sentence: \
         expected {expected:?}, got {message:?} in {body}"
    );
}
