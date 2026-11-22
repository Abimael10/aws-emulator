// SPDX-License-Identifier: CC0-1.0

//! The restore metadata a backup CAPTURES from the protected resource it is taken of.
//!
//! # The rule this module implements
//!
//! `GetRecoveryPointRestoreMetadata` reports "the set of metadata key-value pairs that
//! describe the original configuration of the backed-up resource" (the model's doc for the
//! member), and that configuration belongs to ANOTHER service: only the service that owns
//! the resource knows how it was configured when the backup read it. So the pairs are read
//! from that service WHEN THE BACKUP IS TAKEN — over the emulator's own in-process transport
//! ([`smithy_server_runtime::EmbeddedEmulator`]), the hop the backup already asks whether the
//! resource exists at all through ([`super::protected_resource_answer`]) and the restore
//! re-creates it through ([`super::perform_restore`]) — and are stored on the recovery point
//! ([`super::RecoveryPoint::captured_restore_metadata`]), never read again: a recovery point
//! describes the resource as it WAS, so a later change to that resource, and its deletion,
//! leave what the point reports alone.
//!
//! One arm per resource type whose owning service this emulator can ask; the first is Amazon
//! DynamoDB, whose `DescribeTable` reports the table's name, key schema, capacity,
//! encryption, secondary indexes and stream, beside the auto scaling settings in force on
//! its capacity, which belong to Application Auto Scaling. A type with no arm here, and a
//! hop that answers nothing, capture NOTHING: the recovery point then reports only the pairs
//! it can answer from its own stored members ([`super::restore_metadata_of`]), rather than a
//! configuration nobody read.

use std::collections::BTreeMap;

use smithy_server_runtime::EmbeddedEmulator;

/// The keys the restore metadata of an Amazon DynamoDB recovery point carries for the
/// table's configuration.
///
/// SOURCE: the restore metadata AWS Backup reports for a DynamoDB recovery point and takes
/// back on a restore of one — the per-resource metadata AWS publishes for a DynamoDB restore,
/// which the model's doc for `StartRestoreJob$Metadata` links. The key that names the table
/// a restore CREATES is [`super::METADATA_TARGET_TABLE_NAME`], stated once beside the restore
/// that reads it; the keys below describe the table the backup was taken of.
const METADATA_ORIGINAL_TABLE_NAME: &str = "originalTableName";
/// The attribute the captured table is partitioned by.
const METADATA_PRIMARY_PARTITION_KEY: &str = "primaryPartitionKey";
/// The attribute it is sorted by, empty for a table that has none.
const METADATA_SORT_KEY: &str = "sortKey";
/// The secondary indexes it carries, as a JSON array of their names.
const METADATA_SECONDARY_INDICES: &str = "secondaryIndices";
/// The read and write capacity provisioned for it; both `0` for a table billed per request.
const METADATA_PROVISIONED_READ_CAPACITY_UNITS: &str = "provisionedReadCapacityUnits";
const METADATA_PROVISIONED_WRITE_CAPACITY_UNITS: &str = "provisionedWriteCapacityUnits";
/// How it is encrypted at rest, and the key that encrypts it.
const METADATA_ENCRYPTION_TYPE: &str = "encryptionType";
const METADATA_KMS_MASTER_KEY_ARN: &str = "kmsMasterKeyArn";
/// The application auto scaling settings in force on its capacity, as a JSON object of the
/// scalable targets registered for it ([`captured_auto_scaling`]).
const METADATA_AUTO_SCALING: &str = "autoScaling";
/// Its stream configuration: the view type the stream in force publishes, empty for a table
/// whose stream is off.
const METADATA_STREAM: &str = "stream";

/// The `encryptionType` of a table the owning service reports no server-side encryption
/// description for: it is encrypted under the Amazon Web Services owned key that service
/// applies by default, which is the default encryption of a table rather than a key of the
/// account.
const ENCRYPTION_TYPE_DEFAULT: &str = "DEFAULT";

/// The `ServiceNamespace` Application Auto Scaling holds a DynamoDB table's scalable targets
/// under, and the `ResourceId` prefix it identifies one by (`table/<name>`, which is also the
/// resource portion of a table's ARN — [`super::table_name_of`] reads the name back out of
/// it).
const AUTO_SCALING_NAMESPACE_DYNAMODB: &str = "dynamodb";
const AUTO_SCALING_TABLE_RESOURCE_PREFIX: &str = "table/";

/// The original configuration of the protected resource `resource_arn` names, as the service
/// that owns it reports it, for the recovery point a backup of it is about to store.
///
/// Asked with NO store guard held: it makes at most one in-process call and opens no socket.
/// An empty map is "nothing was captured" — see the module docs.
pub async fn captured_restore_metadata<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
) -> BTreeMap<String, String> {
    match super::resource_type_of(resource_arn).as_deref() {
        Some(super::RESOURCE_TYPE_DYNAMODB) => {
            captured_dynamodb_table(engine, callback, region, resource_arn).await
        }
        // A type whose owning service this emulator cannot ask for a configuration captures
        // nothing.
        _ => BTreeMap::new(),
    }
}

/// The Amazon DynamoDB arm: the table the ARN names, as DynamoDB itself describes it.
///
/// The client and the table-name reading are the ones the restore and the existence question
/// share ([`super::dynamodb_client`]), so every direction of a question about one table
/// reaches DynamoDB through one hop and reads one ARN form. Every pair below is read off that
/// description: a member the table does not carry is reported as the empty value its key
/// takes rather than guessed, and a description this emulator could not get captures nothing
/// at all.
async fn captured_dynamodb_table<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
) -> BTreeMap<String, String> {
    let Some(table_name) = super::table_name_of(resource_arn) else {
        return BTreeMap::new();
    };
    let described = super::dynamodb_client(callback, region)
        .describe_table()
        .table_name(table_name)
        .send()
        .await;
    let Some(table) = described.ok().and_then(|answer| answer.table) else {
        return BTreeMap::new();
    };

    let mut metadata = BTreeMap::new();
    // The name the table is held under, as its own service reports it.
    if let Some(name) = table.table_name.clone() {
        metadata.insert(METADATA_ORIGINAL_TABLE_NAME.to_string(), name.clone());
        // The restore's target defaults to the table the backup was taken of: "you might
        // need to provide a new resource name if the original already exists" (the model's
        // doc for `StartRestoreJob$Metadata`), so the name a client is handed is the
        // original's and a client that wants another replaces it.
        metadata.insert(super::METADATA_TARGET_TABLE_NAME.to_string(), name);
    }
    // The key schema: which attribute the table is partitioned by, and which it is sorted by
    // — the latter empty for a table with no sort key, which is a table of one key only.
    let key_attribute = |wanted: &::aws_sdk_dynamodb::types::KeyType| -> String {
        table
            .key_schema
            .iter()
            .flatten()
            .find(|element| element.key_type() == wanted)
            .map(|element| element.attribute_name().to_string())
            .unwrap_or_default()
    };
    metadata.insert(
        METADATA_PRIMARY_PARTITION_KEY.to_string(),
        key_attribute(&::aws_sdk_dynamodb::types::KeyType::Hash),
    );
    metadata.insert(
        METADATA_SORT_KEY.to_string(),
        key_attribute(&::aws_sdk_dynamodb::types::KeyType::Range),
    );
    // The indexes the table carries, global and local alike: the list is a JSON array, so a
    // table with none reports `[]` rather than an absent key.
    let indexes: Vec<String> = table
        .global_secondary_indexes
        .iter()
        .flatten()
        .filter_map(|index| index.index_name.clone())
        .chain(
            table
                .local_secondary_indexes
                .iter()
                .flatten()
                .filter_map(|index| index.index_name.clone()),
        )
        .collect();
    metadata.insert(
        METADATA_SECONDARY_INDICES.to_string(),
        ::serde_json::Value::from(indexes).to_string(),
    );
    // The capacity provisioned for it, as that service reports it: zeroes for a table billed
    // per request, which provisions none.
    let throughput = table.provisioned_throughput.as_ref();
    metadata.insert(
        METADATA_PROVISIONED_READ_CAPACITY_UNITS.to_string(),
        throughput
            .and_then(|throughput| throughput.read_capacity_units)
            .unwrap_or_default()
            .to_string(),
    );
    metadata.insert(
        METADATA_PROVISIONED_WRITE_CAPACITY_UNITS.to_string(),
        throughput
            .and_then(|throughput| throughput.write_capacity_units)
            .unwrap_or_default()
            .to_string(),
    );
    // How it is encrypted at rest, and under which key: a table its service reports no
    // encryption description for is encrypted by that service's default, and names no key of
    // the account.
    let encryption = table.sse_description.as_ref();
    metadata.insert(
        METADATA_ENCRYPTION_TYPE.to_string(),
        encryption
            .and_then(|encryption| encryption.sse_type.as_ref())
            .map_or_else(
                || ENCRYPTION_TYPE_DEFAULT.to_string(),
                |sse_type| sse_type.as_str().to_string(),
            ),
    );
    metadata.insert(
        METADATA_KMS_MASTER_KEY_ARN.to_string(),
        encryption
            .and_then(|encryption| encryption.kms_master_key_arn.clone())
            .unwrap_or_default(),
    );
    metadata.insert(
        METADATA_AUTO_SCALING.to_string(),
        captured_auto_scaling(engine, region, table_name),
    );
    // Its stream: the view type the stream in force publishes, and the empty value for a
    // table whose stream is off — which is what a table carrying no enabled stream
    // specification is.
    let stream = table
        .stream_specification
        .as_ref()
        .filter(|specification| specification.stream_enabled)
        .and_then(|specification| specification.stream_view_type.as_ref())
        .map(|view_type| view_type.as_str().to_string())
        .unwrap_or_default();
    metadata.insert(METADATA_STREAM.to_string(), stream);
    metadata
}

/// The auto scaling settings in force on `table_name`'s capacity, as a JSON object keyed by
/// the scalable dimension each one scales: the window (`minCapacity`, `maxCapacity`) the
/// scalable target registered for that dimension keeps the capacity inside.
///
/// A table's auto scaling belongs to Application Auto Scaling, not to DynamoDB, so it is read
/// from THAT service's store on the shared engine — the way this service resolves the managed
/// key a vault is encrypted under ([`super::default_encryption_key_arn`]) — rather than
/// composed here. A table nothing is registered for reports the empty object, which is the
/// serialisation of the empty set of targets, and so does an engine whose store cannot be
/// reached: nothing is then registered as far as this capture can tell.
///
/// LOCKING: this takes the Application Auto Scaling store's lock and no other, with this
/// service's own store NOT held (the capture runs before the job is stored), so the two can
/// never be taken in opposite orders.
fn captured_auto_scaling<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    table_name: &str,
) -> String {
    use ::service_com_amazonaws_applicationautoscaling_anyscalefrontendservice::state as auto_scaling_store;

    let resource_id = format!("{AUTO_SCALING_TABLE_RESOURCE_PREFIX}{table_name}");
    let registered = auto_scaling_store::service_state(engine)
        .map(|targets| {
            targets
                .lock()
                .targets
                .iter()
                .filter(|(target, _)| {
                    target.region == region
                        && target.service_namespace == AUTO_SCALING_NAMESPACE_DYNAMODB
                        && target.resource_id == resource_id
                })
                .map(|(target, registered)| {
                    (
                        target.scalable_dimension.clone(),
                        ::serde_json::json!({
                            "minCapacity": registered.min_capacity,
                            "maxCapacity": registered.max_capacity,
                        }),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    ::serde_json::Value::Object(registered).to_string()
}
