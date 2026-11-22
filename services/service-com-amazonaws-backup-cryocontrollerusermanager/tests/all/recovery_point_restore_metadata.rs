// SPDX-License-Identifier: CC0-1.0

//! `GetRecoveryPointRestoreMetadata`: the restore metadata a STORED recovery point carries.
//!
//! The rules under test:
//!
//! * the restore metadata of a recovery point is the ORIGINAL CONFIGURATION of the resource
//!   the backup captured, read from the service that OWNS that resource when the backup was
//!   taken and stored on the point — so every pair the read reports is one that service's own
//!   description of the resource gives, and a resource changed or deleted since leaves what
//!   the read reports alone;
//! * beside that configuration a point reports the keys the restore of its resource type
//!   reads, filled with the captured resource's own name, and the backup request that
//!   captured it — so the metadata handed back is metadata a restore from that point
//!   requires: a `StartRestoreJob` carrying it verbatim is accepted, and the same start with
//!   the key it named dropped is refused;
//! * a resource type whose owning service this emulator cannot ask captures no
//!   configuration, so its point reports the pairs it can answer from its own stored members
//!   rather than a configuration nobody read;
//! * the read is a READ: two reads of one point report the same metadata, and the point is
//!   unchanged by them;
//! * every way the pair can fail to name a stored recovery point is the one not-found
//!   refusal, a `RecoveryPointArn` that is not an ARN is a bad parameter value instead, and a
//!   vault outside the caller's visibility — or a `RecoveryPointArn` of another account — is
//!   refused 403 `AccessDeniedException` before existence is considered at all.
//!
//! The tables span what the rules generalise over: the SHAPE of the captured table (billing
//! mode, a sort key or none, secondary indexes or none, a stream on or off, encryption by a
//! key or by the owning service's default) and the resource TYPE of the point, since a type
//! this emulator cannot ask captures nothing.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire behaviour
//! a `backup` SDK client sees. The harness is the shared one in `vault_and_backup_job.rs`,
//! the Backup-and-DynamoDB emulator and the DynamoDB wire are `restore_job.rs`'s, and the
//! settled-point setup is `recovery_point_deletion.rs`'s, so this suite drives the same wire
//! paths rather than growing copies of them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::recovery_point_deletion::{completed_point, start_point};
use crate::restore_job::{drop_table, dynamodb, emulator, source_table};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, string_at, vault_arn, ACCOUNT, REGION,
};
use crate::vault_read_visibility::DENIED_MESSAGE;

/// The vault every recovery point of this suite is backed up into.
const VAULT: &str = "restore-metadata-read";

/// A second vault, so a case can address a point one vault holds through another.
const OTHER_VAULT: &str = "restore-metadata-elsewhere";

/// The role every backup and restore of this suite runs as.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// `GET /backup-vaults/{BackupVaultName}/recovery-points/{RecoveryPointArn}/restore-metadata`,
/// as the model binds this operation, with the documented `backupVaultAccountId` query
/// parameter when the case names an account.
async fn restore_metadata(
    router: &MultiServiceRouter,
    vault: &str,
    arn: &str,
    account: Option<&str>,
) -> (u16, Value) {
    let path = match account {
        Some(account) => format!(
            "/backup-vaults/{vault}/recovery-points/{arn}/restore-metadata\
             ?backupVaultAccountId={account}"
        ),
        None => format!("/backup-vaults/{vault}/recovery-points/{arn}/restore-metadata"),
    };
    call(router, "GET", &path, None).await
}

/// The `RestoreMetadata` of a read, as a JSON object; `Value::Null` when the response omits
/// the member.
fn reported_metadata(body: &Value) -> Value {
    body.get("RestoreMetadata").cloned().unwrap_or(Value::Null)
}

/// `PUT /restore-jobs`, as the model binds `StartRestoreJob`: a start from
/// `recovery_point_arn` of a `resource_type` resource carrying `metadata`.
async fn start_restore(
    router: &MultiServiceRouter,
    recovery_point_arn: &str,
    resource_type: &str,
    metadata: &Value,
) -> (u16, Value) {
    let body = json!({
        "RecoveryPointArn": recovery_point_arn,
        "ResourceType": resource_type,
        "Metadata": metadata,
        "IamRoleArn": role_arn(),
    })
    .to_string();
    call(router, "PUT", "/restore-jobs", Some(&body)).await
}

/// One emulator again, with Application Auto Scaling mounted beside Backup and DynamoDB: the
/// auto scaling settings in force on a table's capacity belong to that service, so without it
/// mounted there is nothing holding them for a capture to read.
fn emulator_with_auto_scaling() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_applicationautoscaling_anyscalefrontendservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Register a scalable target on `table`'s `dimension` through Application Auto Scaling's own
/// wire (`awsJson1_1`, target prefix `AnyScaleFrontendService`), so the settings a capture
/// reads are ones that service holds.
async fn register_scalable_target(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    dimension: &str,
    min_capacity: i64,
    max_capacity: i64,
) {
    let body = json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": format!("table/{table}"),
        "ScalableDimension": dimension,
        "MinCapacity": min_capacity,
        "MaxCapacity": max_capacity,
    });
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            "AnyScaleFrontendService.RegisterScalableTarget",
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/\
                 application-autoscaling/aws4_request, SignedHeaders=host;x-amz-target, \
                 Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    assert_eq!(
        status,
        200,
        "case {case:?}: setup RegisterScalableTarget on {dimension} should succeed, got \
         {status} {}",
        String::from_utf8_lossy(&payload)
    );
}

/// Create the vault a case backs its resource up into.
async fn given_vault(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, created) = create_vault(router, name, "{}").await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateBackupVault {name:?} should succeed, got {status} \
         {created:?}"
    );
}

/// The `BackupJobId` of the job that wrote the recovery point `arn`, as the backup-job
/// listing reports it — the id a point's restore metadata names as the backup request that
/// captured it.
async fn backup_request_of(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let (status, listed) = call(router, "GET", "/backup-jobs", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListBackupJobs 200, got {status} {listed:?}"
    );
    listed["BackupJobs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|job| string_at(job, "RecoveryPointArn") == arn)
        .map(|job| string_at(job, "BackupJobId"))
        .unwrap_or_else(|| panic!("case {case:?}: the job that wrote {arn} must be listed"))
}

/// The table `name`, as DynamoDB itself describes it.
async fn described_table(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, described) = dynamodb(router, "DescribeTable", json!({"TableName": name})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeTable 200 for {name:?}, got {status} {described:?}"
    );
    described["Table"].clone()
}

/// The restore metadata a point of the table DynamoDB describes as `table` must report,
/// computed HERE from that service's own description, so each pair is asserted to be the
/// owning service's answer rather than a value this service composed.
///
/// `request_id` is the backup job that captured the point, which no description of the table
/// carries: the Backup request is this service's own. `auto_scaling` is the settings
/// Application Auto Scaling holds for the table, which no description of it carries either —
/// the empty object for a table nothing is registered for, which is every table of a case
/// that registers none.
fn metadata_of(table: &Value, request_id: &str, auto_scaling: &str) -> Value {
    let key_attribute = |key_type: &str| -> String {
        table["KeySchema"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|element| element["KeyType"] == key_type)
            .map(|element| string_at(element, "AttributeName"))
            .unwrap_or_default()
    };
    let indices: Vec<String> = ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"]
        .iter()
        .flat_map(|member| table[*member].as_array().cloned().unwrap_or_default())
        .map(|index| string_at(&index, "IndexName"))
        .collect();
    let capacity = |member: &str| -> String {
        table["ProvisionedThroughput"][member]
            .as_i64()
            .unwrap_or_default()
            .to_string()
    };
    let stream = if table["StreamSpecification"]["StreamEnabled"]
        .as_bool()
        .unwrap_or_default()
    {
        string_at(&table["StreamSpecification"], "StreamViewType")
    } else {
        String::new()
    };
    json!({
        "originalTableName": string_at(table, "TableName"),
        "targetTableName": string_at(table, "TableName"),
        "primaryPartitionKey": key_attribute("HASH"),
        "sortKey": key_attribute("RANGE"),
        "secondaryIndices": Value::from(indices).to_string(),
        "provisionedReadCapacityUnits": capacity("ReadCapacityUnits"),
        "provisionedWriteCapacityUnits": capacity("WriteCapacityUnits"),
        // A table its service reports no encryption description for is encrypted by that
        // service's own default key, and names no key of the account.
        "encryptionType": table["SSEDescription"]["SSEType"].as_str().unwrap_or("DEFAULT"),
        "kmsMasterKeyArn": table["SSEDescription"]["KMSMasterKeyArn"].as_str().unwrap_or_default(),
        // No table of these cases has a scalable target registered for it, so the auto
        // scaling settings in force on its capacity are the empty set of them; the suite's
        // own auto-scaling case covers a table that has one.
        "autoScaling": auto_scaling,
        "stream": stream,
        "aws:backup:request-id": request_id,
    })
}

/// Create a DynamoDB table of the shape `case` needs, beyond the two shapes the restore
/// suite's own upload covers: one key only, a secondary index, a stream and server-side
/// encryption under a key.
async fn given_wide_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "sku", "KeyType": "HASH"}],
        "AttributeDefinitions": [
            {"AttributeName": "sku", "AttributeType": "S"},
            {"AttributeName": "vendor", "AttributeType": "S"},
        ],
        "BillingMode": "PAY_PER_REQUEST",
        "GlobalSecondaryIndexes": [{
            "IndexName": "by-vendor",
            "KeySchema": [{"AttributeName": "vendor", "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "ALL"},
        }],
        "StreamSpecification": {"StreamEnabled": true, "StreamViewType": "NEW_AND_OLD_IMAGES"},
        "SSESpecification": {"Enabled": true},
    });
    let (status, created) = dynamodb(router, "CreateTable", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateTable {name:?} should succeed, got {status} {created:?}"
    );
    string_at(&created["TableDescription"], "TableArn")
}

/// One row of the capture table: the table a backup is taken of, and how it was created.
struct Table {
    name: &'static str,
    /// The table name every case backs up, created through the shape this row names.
    shape: Shape,
}

/// Which shape of table a row creates.
enum Shape {
    /// The restore suite's own upload, provisioned at a named capacity, keyed by a partition
    /// key and a sort key.
    Provisioned,
    /// The same table billed per request, which provisions no capacity at all.
    PayPerRequest,
    /// One partition key, a secondary index, a stream and encryption under a key.
    Wide,
}

/// The table.
fn tables() -> Vec<Table> {
    vec![
        Table {
            name: "restore-metadata-provisioned",
            shape: Shape::Provisioned,
        },
        Table {
            name: "restore-metadata-on-demand",
            shape: Shape::PayPerRequest,
        },
        Table {
            name: "restore-metadata-wide",
            shape: Shape::Wide,
        },
    ]
}

/// RULE: the restore metadata of a recovery point is the configuration the backup captured
/// from the resource's own service, so every pair the read reports is that service's own
/// description of the captured table, beside the keys the restore reads and the backup
/// request that captured it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_read_reports_the_configuration_the_backup_captured() {
    for table in tables() {
        let router = emulator();
        given_vault(&router, table.name, VAULT).await;
        let resource = match table.shape {
            Shape::Provisioned => source_table(&router, table.name, true).await,
            Shape::PayPerRequest => source_table(&router, table.name, false).await,
            Shape::Wide => given_wide_table(&router, table.name, table.name).await,
        };
        let point = completed_point(&router, VAULT, &resource).await;
        let request_id = backup_request_of(&router, table.name, &point).await;

        let described = described_table(&router, table.name, table.name).await;
        let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
        assert_eq!(
            (
                status,
                string_at(&read, "RecoveryPointArn"),
                string_at(&read, "BackupVaultArn"),
                string_at(&read, "ResourceType"),
                reported_metadata(&read),
            ),
            (
                200,
                point.clone(),
                vault_arn(REGION, VAULT),
                "DynamoDB".to_string(),
                metadata_of(&described, &request_id, "{}"),
            ),
            "case {:?}: expected the captured configuration of the table, got {status} \
             {read:?}",
            table.name
        );

        // The read is a READ: a second one reports the same pairs, and the point is still
        // the one the recovery-point read reports.
        let (status, again) = restore_metadata(&router, VAULT, &point, None).await;
        assert_eq!(
            (status, reported_metadata(&again)),
            (200, reported_metadata(&read)),
            "case {:?}: two reads of one point must agree, got {status} {again:?}",
            table.name
        );
        let (status, point_read) = call(
            &router,
            "GET",
            &format!("/backup-vaults/{VAULT}/recovery-points/{point}"),
            None,
        )
        .await;
        assert_eq!(
            (
                status,
                string_at(&point_read, "Status"),
                string_at(&point_read, "ResourceArn"),
            ),
            (200, "COMPLETED".to_string(), resource.clone()),
            "case {:?}: the metadata reads must leave the point where it was, got {status} \
             {point_read:?}",
            table.name
        );
    }
}

/// RULE, pinned on one known table: the pairs are reported in the forms a restore takes them
/// back in — the table's name, its key attributes, its capacity as decimal counts, its
/// indexes as a JSON array, and the empty value for what the captured table does not carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_captured_pairs_are_reported_in_the_forms_a_restore_takes_back() {
    let case = "a provisioned table keyed by a partition key and a sort key";
    let router = emulator();
    given_vault(&router, case, VAULT).await;
    // The restore suite's own upload: keys `pk`/`sk`, provisioned at 3 reads and 4 writes,
    // no secondary index, no stream, and no encryption of its own.
    let resource = source_table(&router, "restore-metadata-pinned", true).await;
    let point = completed_point(&router, VAULT, &resource).await;
    let request_id = backup_request_of(&router, case, &point).await;

    let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
    assert_eq!(
        (status, reported_metadata(&read)),
        (
            200,
            json!({
                "originalTableName": "restore-metadata-pinned",
                "targetTableName": "restore-metadata-pinned",
                "primaryPartitionKey": "pk",
                "sortKey": "sk",
                "secondaryIndices": "[]",
                "provisionedReadCapacityUnits": "3",
                "provisionedWriteCapacityUnits": "4",
                "encryptionType": "DEFAULT",
                "kmsMasterKeyArn": "",
                "autoScaling": "{}",
                "stream": "",
                "aws:backup:request-id": request_id,
            })
        ),
        "case {case:?}: expected the captured configuration in its reported forms, got \
         {status} {read:?}"
    );
}

/// RULE: a point describes the resource as it WAS when the backup was taken, so a resource
/// the owning service no longer holds leaves what the read reports unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metadata_describes_the_resource_as_the_backup_captured_it() {
    let case = "the captured table is dropped after the backup";
    let router = emulator();
    given_vault(&router, case, VAULT).await;
    let resource = source_table(&router, "restore-metadata-dropped", true).await;
    let point = completed_point(&router, VAULT, &resource).await;

    let (status, before) = restore_metadata(&router, VAULT, &point, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the metadata read to answer, got {status} {before:?}"
    );
    assert!(
        !reported_metadata(&before)["originalTableName"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "case {case:?}: the captured configuration must name the table, got {before:?}"
    );

    drop_table(&router, "restore-metadata-dropped").await;

    let (status, after) = restore_metadata(&router, VAULT, &point, None).await;
    assert_eq!(
        (status, reported_metadata(&after)),
        (200, reported_metadata(&before)),
        "case {case:?}: the captured configuration must survive the resource, got {status} \
         {after:?}"
    );
}

/// RULE: a resource type whose owning service this emulator cannot ask captures no
/// configuration, so its point reports only what it can answer from its own stored members —
/// the backup request that captured it, and the keys that type's restore reads, of which
/// these types have none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_type_this_emulator_cannot_ask_reports_the_backup_request_alone() {
    // The rows differ along the service that owns the protected resource and the form of its
    // ARN, including one that names neither Region nor account.
    let resources: Vec<(&str, String)> = vec![
        (
            "an EFS file system",
            format!("arn:aws:elasticfilesystem:{REGION}:{ACCOUNT}:file-system/fs-0abc1234"),
        ),
        (
            "an EBS volume",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:volume/vol-0123456789abcdef0"),
        ),
        (
            "an S3 bucket, whose ARN names neither Region nor account",
            "arn:aws:s3:::restore-metadata-bucket".to_string(),
        ),
    ];

    let router = emulator();
    given_vault(&router, "the uncapturable table", VAULT).await;
    for (case, resource) in resources {
        let point = completed_point(&router, VAULT, &resource).await;
        let request_id = backup_request_of(&router, case, &point).await;
        let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
        assert_eq!(
            (status, reported_metadata(&read)),
            (200, json!({"aws:backup:request-id": request_id})),
            "case {case:?}: expected only the backup request, got {status} {read:?}"
        );
    }
}

/// RULE: the pairs the read hands back are what a restore from that point requires — a start
/// carrying them verbatim is accepted, and the same start with a key they named dropped is
/// refused, so the read and the start measure one contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metadata_handed_back_is_what_a_restore_from_that_point_requires() {
    // One captured table, and one resource whose configuration this emulator cannot capture:
    // the first names the key its restore requires, the second names none, and both are
    // accepted exactly as handed back.
    let router = emulator();
    given_vault(&router, "the round-trip table", VAULT).await;
    let table = source_table(&router, "restore-metadata-roundtrip", false).await;
    let cases: Vec<(&str, String)> = vec![
        ("a captured DynamoDB table", table),
        (
            "an EFS file system, whose restore reads no particular key",
            format!("arn:aws:elasticfilesystem:{REGION}:{ACCOUNT}:file-system/fs-0abc1234"),
        ),
    ];

    for (case, resource) in cases {
        let point = completed_point(&router, VAULT, &resource).await;
        let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the metadata read to answer, got {status} {read:?}"
        );
        let handed = reported_metadata(&read);
        let resource_type = string_at(&read, "ResourceType");

        let (status, started) = start_restore(&router, &point, &resource_type, &handed).await;
        assert_eq!(
            (status, error_code(&started).as_str()),
            (200, "<none>"),
            "case {case:?}: a start carrying the metadata it was handed must be accepted, got \
             {status} {started:?}"
        );
        assert!(
            !string_at(&started, "RestoreJobId").is_empty(),
            "case {case:?}: the accepted start must answer with the job it stored, got \
             {started:?}"
        );

        // The other half: a start that drops the key the restore of that type reads carries
        // less than it requires, and is refused as that member's value. The other keys the
        // read reported describe the captured resource and are not required of a start, so
        // dropping one of them is still accepted.
        let required = "targetTableName";
        if handed.get(required).is_some() {
            let mut without = handed.clone();
            without
                .as_object_mut()
                .expect("the metadata the read reported is a map")
                .remove(required);
            let (status, refused) = start_restore(&router, &point, &resource_type, &without).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "InvalidParameterValueException"),
                "case {case:?}: a start without {required:?} must be refused, got {status} \
                 {refused:?}"
            );
        }
    }
}

/// RULE: the auto scaling settings in force on a captured table's capacity belong to
/// Application Auto Scaling, so the pair reports the scalable targets THAT service holds for
/// the table — one entry per dimension it scales, carrying the window registered for it — and
/// the empty object for a table nothing is registered for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_captured_auto_scaling_is_what_the_owning_service_holds() {
    let case = "a table with scalable targets on both halves of its throughput";
    let router = emulator_with_auto_scaling();
    given_vault(&router, case, VAULT).await;
    // The table is provisioned at 3 reads and 4 writes, and each window below CONTAINS the
    // capacity in force, so registering moves neither half.
    let scaled = source_table(&router, "restore-metadata-scaled", true).await;
    let windows = [
        ("dynamodb:table:ReadCapacityUnits", 1, 10),
        ("dynamodb:table:WriteCapacityUnits", 2, 8),
    ];
    for (dimension, min_capacity, max_capacity) in windows {
        register_scalable_target(
            &router,
            case,
            "restore-metadata-scaled",
            dimension,
            min_capacity,
            max_capacity,
        )
        .await;
    }
    let point = completed_point(&router, VAULT, &scaled).await;

    let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
    let captured: Value = serde_json::from_str(
        reported_metadata(&read)["autoScaling"]
            .as_str()
            .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    assert_eq!(
        (status, captured),
        (
            200,
            json!({
                "dynamodb:table:ReadCapacityUnits": {"minCapacity": 1, "maxCapacity": 10},
                "dynamodb:table:WriteCapacityUnits": {"minCapacity": 2, "maxCapacity": 8},
            })
        ),
        "case {case:?}: expected the registered windows, got {status} {read:?}"
    );

    // The control: a table in the SAME emulator that nothing is registered for reports the
    // empty set of settings, so the pair follows the registrations rather than the service
    // being mounted.
    let unscaled = source_table(&router, "restore-metadata-unscaled", true).await;
    let other = completed_point(&router, VAULT, &unscaled).await;
    let (status, read) = restore_metadata(&router, VAULT, &other, None).await;
    assert_eq!(
        (status, reported_metadata(&read)["autoScaling"].clone()),
        (200, json!("{}")),
        "case a table nothing is registered for: expected the empty settings, got {status} \
         {read:?}"
    );
}

/// One row of the refusal table: the pair a request addresses, the account its query names,
/// and the refusal that condition carries.
struct Refused {
    name: &'static str,
    /// The `BackupVaultName` the request addresses.
    vault: &'static str,
    /// The `RecoveryPointArn` the request addresses.
    arn: String,
    /// The `backupVaultAccountId` the query names, when the row names one.
    account: Option<&'static str>,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE: every way the pair can fail to name a stored recovery point of a visible vault is
/// the refusal that condition carries — the not-found one for a pair that resolves to
/// nothing, the parameter-value one for an ARN that is not one, and the authorization one for
/// a vault or a point the caller cannot see, which is decided before existence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pair_that_names_no_visible_stored_point_is_refused() {
    let router = emulator();
    given_vault(&router, "the refusal table", VAULT).await;
    given_vault(&router, "the refusal table", OTHER_VAULT).await;
    let resource = source_table(&router, "restore-metadata-refusals", true).await;
    let point = completed_point(&router, VAULT, &resource).await;

    let refusals = vec![
        Refused {
            name: "a vault this account does not hold",
            vault: "no-such-vault",
            arn: point.clone(),
            account: None,
            status: 403,
            code: "AccessDeniedException",
            message: DENIED_MESSAGE.to_string(),
        },
        Refused {
            name: "another account's vault, named by the documented query parameter",
            vault: VAULT,
            arn: point.clone(),
            account: Some("111122223333"),
            status: 403,
            code: "AccessDeniedException",
            message: DENIED_MESSAGE.to_string(),
        },
        Refused {
            name: "a RecoveryPointArn of another account",
            vault: VAULT,
            arn: format!("arn:aws:backup:{REGION}:111122223333:recovery-point:1EB3B5E7"),
            account: None,
            status: 403,
            code: "AccessDeniedException",
            message: format!(
                "User: arn:aws:iam::{ACCOUNT}:user/emulator is not authorized to perform: \
                 backup:GetRecoveryPointRestoreMetadata on resource: \
                 arn:aws:backup:{REGION}:111122223333:recovery-point:1EB3B5E7 because no \
                 resource-based policy allows the backup:GetRecoveryPointRestoreMetadata action"
            ),
        },
        Refused {
            name: "a RecoveryPointArn that is not an ARN",
            vault: VAULT,
            arn: "not-an-arn".to_string(),
            account: None,
            status: 400,
            code: "InvalidParameterValueException",
            message: "Parameter RecoveryPointArn is invalid.".to_string(),
        },
        Refused {
            name: "an own-account ARN that names no stored point",
            vault: VAULT,
            arn: format!("arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:nothing-here"),
            account: None,
            status: 400,
            code: "ResourceNotFoundException",
            message: "Cannot find recovery point".to_string(),
        },
        Refused {
            name: "a point another vault of this account holds",
            vault: OTHER_VAULT,
            arn: point.clone(),
            account: None,
            status: 400,
            code: "ResourceNotFoundException",
            message: "Cannot find recovery point".to_string(),
        },
    ];

    for case in refusals {
        let (status, refused) =
            restore_metadata(&router, case.vault, &case.arn, case.account).await;
        assert_eq!(
            (
                status,
                error_code(&refused),
                string_at(&refused, "Message")
            ),
            (case.status, case.code.to_string(), case.message.clone()),
            "case {:?}: expected that condition's own refusal, got {status} {refused:?}",
            case.name
        );
        assert!(
            reported_metadata(&refused).is_null(),
            "case {:?}: a refusal must report no metadata, got {refused:?}",
            case.name
        );
    }

    // The control: the point the account holds in the vault that holds it is still answered,
    // so the refusals above are about the pair and not about the operation.
    let (status, read) = restore_metadata(&router, VAULT, &point, None).await;
    assert_eq!(
        (status, string_at(&read, "RecoveryPointArn")),
        (200, point.clone()),
        "case the control: the stored point must still be answered, got {status} {read:?}"
    );
}

/// RULE: a point whose backup has not finished is not addressable yet, so this read answers
/// the same not-found refusal every call that addresses a point by (vault, ARN) does — and no
/// number of reads brings the point forward.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_point_whose_backup_has_not_finished_has_no_metadata_yet() {
    let case = "a running backup";
    let router = emulator();
    given_vault(&router, case, VAULT).await;
    let resource = source_table(&router, "restore-metadata-running", true).await;
    let point = start_point(&router, VAULT, &resource, None).await;

    for attempt in 0..3 {
        let (status, refused) = restore_metadata(&router, VAULT, &point, None).await;
        assert_eq!(
            (
                status,
                error_code(&refused),
                string_at(&refused, "Message")
            ),
            (
                400,
                "ResourceNotFoundException".to_string(),
                "Cannot find recovery point".to_string()
            ),
            "case {case:?}, read {attempt}: expected the not-found refusal, got {status} \
             {refused:?}"
        );
    }

    // Once the clock has made a backup's completion due, the same read answers: the refusals
    // above were about the point's state, not about the pair.
    let settled = completed_point(&router, VAULT, &resource).await;
    let (status, read) = restore_metadata(&router, VAULT, &settled, None).await;
    assert_eq!(
        (status, string_at(&read, "RecoveryPointArn")),
        (200, settled.clone()),
        "case a settled backup: the point must be answered, got {status} {read:?}"
    );
}
