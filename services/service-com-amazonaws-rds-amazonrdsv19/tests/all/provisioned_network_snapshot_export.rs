// SPDX-License-Identifier: CC0-1.0

//! `StartExportTask` driven the way a CLIENT drives it, over a network the account
//! PROVISIONED itself, with every resource the export names held by the service that owns
//! it.
//!
//! The rules this suite pins, each over a table spanning what it generalises over:
//!
//! * A `StartExportTask` whose source, destination bucket, export role and encryption key
//!   all resolve is ACCEPTED (HTTP 200), reports `STARTING`, and the task it records then
//!   walks to the terminal `COMPLETE` with `PercentProgress` 100 while the export's objects
//!   appear in the destination bucket under `<S3Prefix>/<ExportTaskIdentifier>/` — and under
//!   `<ExportTaskIdentifier>/` for a request that named no prefix. The rows span an
//!   `S3Prefix` given and omitted, and the spellings `KmsKeyId` takes (a key id and a key
//!   ARN), because neither decides whether the export runs.
//! * A `KmsKeyId` whose key cannot encrypt the export is refused synchronously with the
//!   modelled `KMSKeyNotAccessibleFault` (HTTP 400) and the one sentence that fault carries,
//!   whose only variable part is the key reference the request spelled; nothing of the
//!   refused request survives — no export task is recorded and no object is written. The
//!   rows span the key reference's two spellings over a key whose own policy denies EVERY
//!   principal the two KMS operations an export performs on it.
//!
//! The source is a DB snapshot of a DB instance created in a DB subnet group over subnets
//! Amazon EC2 minted, the destination bucket and the objects are Amazon S3's, the export
//! role is IAM's — carrying the identity policy the Amazon RDS user guide's export role
//! carries, whose actions are WILDCARDS (`s3:PutObject*`) rather than the bare action names
//! — and the keys are KMS's. All five services are mounted on ONE engine, exactly as the
//! assembled router mounts them, so every answer here is one the owning service really
//! gave.

use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};
use tokio::time::sleep;

use crate::db_instances::{call, expect_error, expect_ok, subtrees, text, REGION};
use crate::rds_s3_integration::{create_bucket, kms, role, s3};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The master user the source DB instance is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The database the source instance carries.
const DATABASE: &str = "appdb";

/// The destination bucket every export below writes into.
const BUCKET: &str = "emu-provisioned-export-bucket";

/// The DB instance the exported snapshot is taken of, and the snapshot itself.
const INSTANCE: &str = "emu-provisioned-export-instance";
const SNAPSHOT: &str = "emu-provisioned-export-snapshot";

/// How long a poll loop here is given before it calls a transition unobservable: generous
/// next to the span this service owes one after, so a loaded machine cannot fail the suite.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// The gap between two polls of one loop.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The export role's identity policy as the Amazon RDS user guide writes it: the actions
/// `IamRoleArn` lists ("s3:PutObject* s3:GetObject* s3:ListBucket s3:DeleteObject*
/// s3:GetBucketLocation"), each a WILDCARD, over the two resource forms the same member
/// shows ("arn:aws:s3:::your-s3-bucket arn:aws:s3:::your-s3-bucket/*").
fn user_guide_role_policy(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:PutObject*","s3:GetObject*","s3:ListBucket","s3:DeleteObject*","s3:GetBucketLocation"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}}]}}"#
    )
}

/// A key policy that grants the account root everything — the default key policy's own
/// statement, which delegates to IAM — and then DENIES every principal the two KMS
/// operations an export performs on the key: "an explicit deny in any policy overrides any
/// allow", so the deny is what decides for the export's role as for anybody else.
fn key_policy_denying_every_principal() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"kms:*","Resource":"*"}},{{"Effect":"Deny","Principal":{{"AWS":"*"}},"Action":["kms:CreateGrant","kms:DescribeKey"],"Resource":"*"}}]}}"#
    )
}

/// Poll `read` until `settled` holds, answering the body it settled on; panics with the last
/// answer when [`POLL_DEADLINE`] passes first. The loop waits for the CONDITION, so a faster
/// or slower machine only changes how many times it looks.
async fn poll_until<Read, Fut>(
    case: &str,
    what: &str,
    read: Read,
    settled: impl Fn(&str) -> bool,
) -> String
where
    Read: Fn() -> Fut,
    Fut: std::future::Future<Output = (u16, String)>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (status, body) = read().await;
        if status == 200 && settled(&body) {
            return body;
        }
        last = format!("HTTP {status}: {body}");
        sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {what} never settled in {POLL_DEADLINE:?}; last answer: {last}")
}

/// Everything one export names, each held by the service that owns it.
struct Account {
    router: MultiServiceRouter,
    /// The ARN of the DB snapshot the exports read.
    source_arn: String,
    /// The export role's ARN.
    role_arn: String,
    /// A key the export can encrypt under: its key id and its key ARN.
    usable_key: (String, String),
    /// A key whose own policy denies every principal the export's KMS operations, in the
    /// same two spellings.
    denied_key: (String, String),
}

/// Stand up the whole account: the network, the destination, the keys and the source.
async fn provisioned_account() -> Account {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Verifying signatures is the emulator's concern, not this service's; these tests sign
    // nothing, so the check is off and the scope header is read for routing only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    let ec2 = ::aws_sdk_ec2::Client::from_conf(
        ::aws_sdk_ec2::Config::builder()
            .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build(),
    );

    // ---- the network the DB subnet group is over ----
    let zones: Vec<String> = ec2
        .describe_availability_zones()
        .send()
        .await
        .expect("EC2 reports the Region's Availability Zones")
        .availability_zones()
        .iter()
        .filter_map(|zone| zone.zone_name().map(str::to_string))
        .filter(|zone| zone.starts_with(REGION))
        .collect();
    let vpc = ec2
        .create_vpc()
        .cidr_block("10.77.0.0/16")
        .send()
        .await
        .expect("EC2 creates the VPC")
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("the created VPC reports its id")
        .to_string();
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().take(2).enumerate() {
        subnets.push(
            ec2.create_subnet()
                .vpc_id(&vpc)
                .cidr_block(format!("10.77.{index}.0/24"))
                .availability_zone(zone)
                .send()
                .await
                .unwrap_or_else(|error| panic!("EC2 creates the subnet in {zone}: {error}"))
                .subnet()
                .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
                .expect("the created subnet reports its id")
                .to_string(),
        );
    }
    assert_eq!(
        subnets.len(),
        2,
        "a DB subnet group is over two Availability Zones, and EC2 reported the zones \
         {zones:?}, so only {} subnet(s) could be created",
        subnets.len()
    );
    let security_group = ec2
        .create_security_group()
        .group_name("emu-provisioned-export-sg")
        .description("the export source security group")
        .vpc_id(&vpc)
        .send()
        .await
        .expect("EC2 creates the security group")
        .group_id
        .expect("the created security group reports its id");
    expect_ok(
        "the DB subnet group",
        call(
            &router,
            "CreateDBSubnetGroup",
            &params![
                "DBSubnetGroupName" => "emu-provisioned-export-subnets",
                "DBSubnetGroupDescription" => "the export source subnets",
                "SubnetIds.SubnetIdentifier.1" => subnets[0],
                "SubnetIds.SubnetIdentifier.2" => subnets[1],
            ],
        )
        .await,
    );

    // ---- the destination: the bucket, the role and the two keys ----
    create_bucket(&router, BUCKET, REGION).await;
    let role_arn = role(
        &router,
        "emu-provisioned-export-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(user_guide_role_policy(BUCKET)),
    )
    .await;
    let usable = kms(&router)
        .create_key()
        .description("the key the export encrypts under")
        .send()
        .await
        .expect("KMS creates the usable key")
        .key_metadata
        .expect("the created key reports its metadata");
    let denied = kms(&router)
        .create_key()
        .description("the key the export may not use")
        .policy(key_policy_denying_every_principal())
        // A policy that names a principal other than this account's own calls for the
        // lockout check to be bypassed, exactly as it does against the real service.
        .bypass_policy_lockout_safety_check(true)
        .send()
        .await
        .expect("KMS creates the denied key")
        .key_metadata
        .expect("the created key reports its metadata");

    // ---- the source: a DB instance in that subnet group, and a snapshot of it ----
    expect_ok(
        "the source DB instance",
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => INSTANCE,
                "Engine" => "mysql",
                "DBInstanceClass" => "db.t3.micro",
                "AllocatedStorage" => 20,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "DBName" => DATABASE,
                "DBSubnetGroupName" => "emu-provisioned-export-subnets",
                "VpcSecurityGroupIds.VpcSecurityGroupId.1" => security_group,
                "BackupRetentionPeriod" => 1,
                "StorageEncrypted" => "true",
            ],
        )
        .await,
    );
    let describe_instance = params!["DBInstanceIdentifier" => INSTANCE];
    poll_until(
        "the source DB instance",
        &format!("the DB instance {INSTANCE} reaching available"),
        || call(&router, "DescribeDBInstances", &describe_instance),
        |body| text(body, "DBInstanceStatus") == "available",
    )
    .await;
    expect_ok(
        "the source DB snapshot",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => INSTANCE,
                "DBSnapshotIdentifier" => SNAPSHOT,
            ],
        )
        .await,
    );
    let describe_snapshot = params!["DBSnapshotIdentifier" => SNAPSHOT];
    let snapshot = poll_until(
        "the source DB snapshot",
        &format!("the DB snapshot {SNAPSHOT} reaching available"),
        || call(&router, "DescribeDBSnapshots", &describe_snapshot),
        |body| text(body, "Status") == "available",
    )
    .await;

    Account {
        source_arn: text(&snapshot, "DBSnapshotArn"),
        role_arn,
        usable_key: (
            usable.key_id().to_string(),
            usable.arn().expect("the usable key's ARN").to_string(),
        ),
        denied_key: (
            denied.key_id().to_string(),
            denied.arn().expect("the denied key's ARN").to_string(),
        ),
        router,
    }
}

impl Account {
    /// One `StartExportTask` call, naming the key reference and prefix a row carries.
    async fn start_export(
        &self,
        identifier: &str,
        key_reference: &str,
        prefix: Option<&str>,
    ) -> (u16, String) {
        let mut request: Vec<(&str, String)> = params![
            "ExportTaskIdentifier" => identifier,
            "SourceArn" => self.source_arn,
            "S3BucketName" => BUCKET,
            "IamRoleArn" => self.role_arn,
            "KmsKeyId" => key_reference,
        ]
        .to_vec();
        if let Some(prefix) = prefix {
            request.push(("S3Prefix", prefix.to_string()));
        }
        call(&self.router, "StartExportTask", &request).await
    }

    /// The keys Amazon S3 holds in the destination bucket under `prefix`, through that
    /// service's own listing.
    async fn listed_keys(&self, prefix: &str) -> Vec<String> {
        s3(&self.router)
            .list_objects_v2()
            .bucket(BUCKET)
            .prefix(prefix)
            .send()
            .await
            .expect("Amazon S3 lists the destination bucket")
            .contents()
            .iter()
            .filter_map(|object| object.key().map(str::to_string))
            .collect()
    }

    /// The `ExportTaskIdentifier`s `DescribeExportTasks` reports for this account.
    async fn export_task_identifiers(&self) -> Vec<String> {
        let described = expect_ok(
            "the recorded export tasks",
            call(
                &self.router,
                "DescribeExportTasks",
                &params!["MaxRecords" => 20],
            )
            .await,
        );
        subtrees(&described, "ExportTask")
            .iter()
            .map(|task| text(task, "ExportTaskIdentifier"))
            .collect()
    }
}

/// One row of the accepted-export table: what the request names and where its objects land.
struct AcceptedRow {
    case: &'static str,
    identifier: &'static str,
    /// Whether the request spells `KmsKeyId` as the key ARN rather than the bare key id.
    by_arn: bool,
    /// `S3Prefix`, given or omitted.
    prefix: Option<&'static str>,
    /// Where the export's objects have to land: `<S3Prefix>/<ExportTaskIdentifier>/`, and
    /// `<ExportTaskIdentifier>/` for a request that named no prefix — written out here
    /// rather than computed, so the assertion is not the code under test restated.
    under: &'static str,
}

/// Rule: an export whose source, destination bucket, role and key all resolve is accepted,
/// reports `STARTING`, and its task walks to the terminal `COMPLETE` with all of its
/// progress while the export's objects land under `<S3Prefix>/<ExportTaskIdentifier>/` — or
/// under `<ExportTaskIdentifier>/` where the request named no prefix.
#[tokio::test]
async fn an_export_whose_parameters_resolve_completes_and_leaves_its_objects_under_its_prefix() {
    let account = provisioned_account().await;
    let rows = [
        AcceptedRow {
            case: "a key id and a prefix",
            identifier: "emu-provisioned-export-prefixed",
            by_arn: false,
            prefix: Some("exports/nightly"),
            under: "exports/nightly/emu-provisioned-export-prefixed/",
        },
        AcceptedRow {
            case: "a key ARN and no prefix",
            identifier: "emu-provisioned-export-rooted",
            by_arn: true,
            prefix: None,
            under: "emu-provisioned-export-rooted/",
        },
    ];

    for row in &rows {
        let key_reference = if row.by_arn {
            &account.usable_key.1
        } else {
            &account.usable_key.0
        };
        let started = expect_ok(
            row.case,
            account
                .start_export(row.identifier, key_reference, row.prefix)
                .await,
        );
        // The values `ExportTask$Status` and `ExportTask$PercentProgress` publish are AWS's,
        // so they are written out here rather than read from this crate's own constants.
        let status = text(&started, "Status");
        assert_eq!(
            status, "STARTING",
            "case {:?}: expected an accepted export to report \"STARTING\", got {status:?} in \
             {started}",
            row.case
        );
        let echoed = text(&started, "KmsKeyId");
        assert_eq!(
            &echoed, key_reference,
            "case {:?}: expected the key reference {key_reference:?} echoed, got {echoed:?}",
            row.case
        );

        let request = params!["ExportTaskIdentifier" => row.identifier];
        let described = poll_until(
            row.case,
            "the export task reaching a terminal status",
            || call(&account.router, "DescribeExportTasks", &request),
            |body| ["COMPLETE", "FAILED", "CANCELED"].contains(&text(body, "Status").as_str()),
        )
        .await;
        let settled = text(&described, "Status");
        assert_eq!(
            settled, "COMPLETE",
            "case {:?}: expected the export to settle on \"COMPLETE\", got {settled:?} in \
             {described}",
            row.case
        );
        let progress = text(&described, "PercentProgress");
        assert_eq!(
            progress, "100",
            "case {:?}: expected PercentProgress 100 at COMPLETE, got {progress:?}",
            row.case
        );

        // The objects of this export, where the request's own prefix puts them: what a
        // caller listing that prefix in Amazon S3 finds.
        let under = row.under;
        let keys = account.listed_keys(under).await;
        assert!(
            !keys.is_empty(),
            "case {:?}: expected the export's objects under {under:?}, got {keys:?}",
            row.case
        );
        assert!(
            keys.iter().all(|key| key.starts_with(under)),
            "case {:?}: every object of the export sits under {under:?}, got {keys:?}",
            row.case
        );
    }
}

/// One row of the unusable-key table: the spelling `KmsKeyId` carries.
struct DeniedKeyRow {
    case: &'static str,
    identifier: &'static str,
    /// Whether the request spells the key as its ARN rather than its bare key id.
    by_arn: bool,
}

/// Rule: a `KmsKeyId` whose key cannot encrypt the export — here a key whose own policy
/// denies EVERY principal the two KMS operations an export performs on it — is refused
/// synchronously with the modelled `KMSKeyNotAccessibleFault` (HTTP 400) and the one
/// sentence that fault carries, naming the key reference exactly as the request spelled it;
/// the refusal records no export task and writes no object.
#[tokio::test]
async fn an_export_named_an_unusable_key_is_refused_and_leaves_nothing_behind() {
    let account = provisioned_account().await;
    let rows = [
        DeniedKeyRow {
            case: "the denied key by key id",
            identifier: "emu-provisioned-denied-by-id",
            by_arn: false,
        },
        DeniedKeyRow {
            case: "the denied key by key ARN",
            identifier: "emu-provisioned-denied-by-arn",
            by_arn: true,
        },
    ];

    for row in &rows {
        let key_reference = if row.by_arn {
            &account.denied_key.1
        } else {
            &account.denied_key.0
        };
        let body = expect_error(
            row.case,
            account
                .start_export(row.identifier, key_reference, Some("denied-attempt"))
                .await,
            400,
            "KMSKeyNotAccessibleFault",
        );
        let expected = format!("The KMS key {key_reference} doesn't exist or is disabled.");
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {:?}: expected the refusal sentence {expected:?}, got {message:?}",
            row.case
        );
    }

    let recorded = account.export_task_identifiers().await;
    assert!(
        recorded.is_empty(),
        "a refused export records no export task, got {recorded:?}"
    );
    let keys = account.listed_keys("").await;
    assert!(
        keys.is_empty(),
        "a refused export writes no object, got {keys:?}"
    );
}
