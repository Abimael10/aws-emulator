// SPDX-License-Identifier: CC0-1.0

//! Performing a restore: re-creating the protected resource a recovery point
//! was taken of, in the SERVICE that owns it.
//!
//! # The rule this module implements
//!
//! A restore is not a record of intent: it produces a resource, and the
//! resource belongs to another service. So the work here is a call to that
//! service — made the way every emulated service reaches another, with the AWS
//! SDK driven over the emulator's own in-process transport
//! ([`smithy_server_runtime::EmbeddedEmulator`]) — and what the restore reports
//! as its `CreatedResourceArn` is what THAT service answered with, never a
//! value composed here. A resource the owning service does not hold is
//! therefore never reported as created.
//!
//! One arm per resource type whose restore this emulator can perform; the first
//! is Amazon DynamoDB, which the restore of a table recovery point creates
//! through `CreateTable`. The restored table's SHAPE is the source table's, read
//! back from DynamoDB itself: its key schema, the attributes that schema and its
//! indexes are declared over, its billing mode and capacity, and its secondary
//! indexes, global and local alike — every structural member of the source's
//! description that `CreateTable` takes, so the table a restore creates offers
//! the query paths the protected resource offered. (A table setting AWS
//! documents a DynamoDB restore as NOT reproducing — its stream, its
//! time-to-live, its auto scaling and its point-in-time recovery — is left to
//! the restored table's own defaults.)
//!
//! Its CONTENTS are the source's too: the items the recovery point captured when
//! the backup was taken ([`super::CapturedContents`]) are written into the
//! created table through the owning service's own API, so a restore puts the
//! data back and the restored table answers reads for what was backed up.
//!
//! A type with no arm here, and a call the owning service refuses, both end the
//! restore in the modelled failure status carrying the reason
//! ([`RestoreOutcome::Failed`]): nothing is reported created for a restore that
//! created nothing.

use std::collections::BTreeMap;

use smithy_server_runtime::EmbeddedEmulator;

/// The `ResourceType` of an Amazon DynamoDB table, as this service spells it
/// ([`super::SUPPORTED_RESOURCE_TYPES`]).
pub const RESOURCE_TYPE_DYNAMODB: &str = "DynamoDB";

/// The `Metadata` key that names the table a DynamoDB restore creates.
///
/// "You might need to provide a new resource name if the original already
/// exists" (model doc for `StartRestoreJob$Metadata`); for a DynamoDB table
/// that name is carried under this key. Matched case-insensitively, because the
/// metadata of a recovery point is reported in the spelling the protected
/// resource's own service uses and a client passes it back as it received it.
pub const METADATA_TARGET_TABLE_NAME: &str = "targetTableName";

/// The `BillingMode` of a table that is billed per request, as DynamoDB spells
/// it: the mode a restored table is created in when the source table is in it.
const BILLING_MODE_PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";

/// The provisioned capacity a restored table is created with when the source
/// table is provisioned but reports no capacity of its own — DynamoDB requires
/// a positive value for each, and this is the smallest pair that satisfies it.
const DEFAULT_PROVISIONED_CAPACITY: i64 = 1;

/// How many item writes one `BatchWriteItem` carries at most.
///
/// SOURCE: "A single call to BatchWriteItem can transmit up to 16MB of data over the
/// network, consisting of up to 25 item put or delete operations" (the model's doc for
/// `BatchWriteItem`). The write-back below sends the captured items in runs of this
/// size, so it makes one call per 25 items rather than one per item.
const MAX_ITEMS_PER_BATCH_WRITE: usize = 25;

/// What performing a restore came to.
#[derive(Debug, Clone)]
pub enum RestoreOutcome {
    /// The resource was created, and this is the ARN the service that owns it
    /// answered with — so a read of that service reports the resource this
    /// restore says it created.
    Created(String),
    /// Nothing was created, and this says why. It becomes the restore job's
    /// `StatusMessage`, beside the modelled failure status.
    Failed(String),
}

/// What performing ONE restore needs, as the stored restore job and the recovery
/// point it reads carry it: grouped so the arms below take the restore rather
/// than a list of its parts.
#[derive(Debug, Clone, Copy)]
pub struct RestoreRequest<'r> {
    /// The Region the restore runs in, which the resource it creates is created in.
    pub region: &'r str,
    /// The Backup resource type being restored ([`super::resource_type_of`]), which
    /// selects the arm that performs it.
    pub resource_type: Option<&'r str>,
    /// The protected resource the recovery point was taken of.
    pub source_resource_arn: &'r str,
    /// The restore metadata the start was given, which names what to create.
    pub metadata: &'r BTreeMap<String, String>,
    /// The tags the recovery point carries, copied onto the created resource when
    /// `copy_source_tags` asks for it.
    pub source_tags: &'r BTreeMap<String, String>,
    pub copy_source_tags: bool,
    /// The contents the backup captured, which the restore writes back
    /// ([`super::CapturedContents`]).
    pub contents: &'r super::CapturedContents,
}

/// Re-create the protected resource `restore` names, as its restore metadata asks
/// for, holding the contents the recovery point captured, and answer what that
/// came to.
///
/// Called with NO state lock held: the hops below are awaits, and this
/// service's store is not held across one.
pub async fn perform_restore(
    callback: &EmbeddedEmulator,
    restore: RestoreRequest<'_>,
) -> RestoreOutcome {
    match restore.resource_type {
        Some(RESOURCE_TYPE_DYNAMODB) => restore_dynamodb_table(callback, &restore).await,
        // A type this emulator cannot re-create has no restore to perform, so
        // the job says so rather than reporting a resource nothing created.
        Some(other) => RestoreOutcome::Failed(format!(
            "this emulator performs no restore of a {other} recovery point, so no resource was \
             created"
        )),
        None => RestoreOutcome::Failed(
            "the restore names no resource type, so there is no resource to create".to_string(),
        ),
    }
}

/// The DynamoDB arm: create the table the metadata names, shaped like the table
/// the recovery point was taken of, and holding the items that point captured.
async fn restore_dynamodb_table(
    callback: &EmbeddedEmulator,
    restore: &RestoreRequest<'_>,
) -> RestoreOutcome {
    let Some(target_table_name) = metadata_value(restore.metadata, METADATA_TARGET_TABLE_NAME)
    else {
        // A start whose metadata names no target table is refused when it is made
        // ([`super::rejected_restore_metadata`]), so no stored job reaches here
        // without one; a restore that has no target still creates nothing rather
        // than choosing a name of its own.
        return RestoreOutcome::Failed(format!(
            "the restore metadata names no {METADATA_TARGET_TABLE_NAME}, so there is no table to \
             create"
        ));
    };
    let Some(source_table_name) = table_name_of(restore.source_resource_arn) else {
        return RestoreOutcome::Failed(
            "the recovery point's resource is not a DynamoDB table, so there is no table to \
             create"
                .to_string(),
        );
    };

    let client = dynamodb_client(callback, restore.region);
    // The shape of the table to create is the SOURCE table's, read back from
    // the service that holds it rather than guessed here.
    let source = match client
        .describe_table()
        .table_name(source_table_name)
        .send()
        .await
    {
        Ok(answer) => answer,
        Err(error) => {
            return RestoreOutcome::Failed(format!(
                "the table the recovery point was taken of could not be read: {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            ))
        }
    };
    let Some(source) = source.table else {
        return RestoreOutcome::Failed(
            "the table the recovery point was taken of reports no description".to_string(),
        );
    };

    let mut create = client
        .create_table()
        .table_name(target_table_name)
        .set_key_schema(source.key_schema.clone())
        .set_attribute_definitions(source.attribute_definitions.clone());
    // One of the two is required by `CreateTable`, and which one is the source
    // table's own billing mode.
    let pay_per_request = source
        .billing_mode_summary
        .as_ref()
        .and_then(|summary| summary.billing_mode.as_ref())
        .is_some_and(|mode| mode.as_str() == BILLING_MODE_PAY_PER_REQUEST);
    if pay_per_request {
        create = create.billing_mode(::aws_sdk_dynamodb::types::BillingMode::PayPerRequest);
    } else {
        let read = source
            .provisioned_throughput
            .as_ref()
            .and_then(|throughput| throughput.read_capacity_units)
            .filter(|units| *units > 0)
            .unwrap_or(DEFAULT_PROVISIONED_CAPACITY);
        let write = source
            .provisioned_throughput
            .as_ref()
            .and_then(|throughput| throughput.write_capacity_units)
            .filter(|units| *units > 0)
            .unwrap_or(DEFAULT_PROVISIONED_CAPACITY);
        create = create.provisioned_throughput(
            ::aws_sdk_dynamodb::types::ProvisionedThroughput::builder()
                .read_capacity_units(read)
                .write_capacity_units(write)
                .build()
                .expect("a provisioned throughput with both capacities set"),
        );
    }
    // "If this equals True, tags included in the backup will be copied to the
    // restored resource" (model doc for
    // `StartRestoreJob$CopySourceTagsToRestoredResource`) — the tags included in
    // the backup are the ones the recovery point carries.
    if restore.copy_source_tags {
        for (key, value) in restore.source_tags {
            create = create.tags(
                ::aws_sdk_dynamodb::types::Tag::builder()
                    .key(key)
                    .value(value)
                    .build()
                    .expect("a tag with both members set"),
            );
        }
    }
    // The SECONDARY INDEXES the source table carries, global and local alike. They
    // are part of the shape the recovery point was taken of, so the restored table
    // has them: the index names, their own key schemas and their projections are
    // the source's, and a global index of a provisioned table is given capacity of
    // its own the way the table is. They are also what makes the forwarded
    // `AttributeDefinitions` whole — DynamoDB admits a definition only when some
    // key schema uses it, and an index-only attribute is used by no other.
    match declared_indexes(&source, pay_per_request) {
        Ok(declared) => {
            let global = (!declared.global.is_empty()).then_some(declared.global);
            let local = (!declared.local.is_empty()).then_some(declared.local);
            create = create
                .set_global_secondary_indexes(global)
                .set_local_secondary_indexes(local);
        }
        Err(index_name) => {
            return RestoreOutcome::Failed(format!(
                "the table the recovery point was taken of reports no complete description of \
                 its index {index_name}, so the table it was backed up as cannot be re-created"
            ))
        }
    }

    let created = match create.send().await {
        // The ARN reported is the one DynamoDB answered with, so the resource
        // this restore reports created is one that service holds.
        Ok(created) => created,
        Err(error) => {
            return RestoreOutcome::Failed(format!(
                "the restored table could not be created: {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            ))
        }
    };
    let Some(table_arn) = created
        .table_description
        .and_then(|description| description.table_arn)
    else {
        return RestoreOutcome::Failed(
            "the restored table was created but its own service reported no ARN for it"
                .to_string(),
        );
    };
    // THE DATA: the items the recovery point captured, written into the table this
    // restore just created, through the owning service's own API. A restore puts the
    // protected resource's contents back, so a restore that created the table but
    // could not write them reports the failure rather than a table that is missing
    // them. A table created an instant ago does not serve item writes yet, so the
    // write waits for the one it was created with to become usable.
    if !restore.contents.is_empty() {
        if let Err(reason) = wait_until_serving(&client, target_table_name).await {
            return RestoreOutcome::Failed(reason);
        }
        if let Err(reason) =
            write_captured_items(&client, target_table_name, restore.contents).await
        {
            return RestoreOutcome::Failed(reason);
        }
    }
    RestoreOutcome::Created(table_arn)
}

/// Wait until the table `table_name` names SERVES item writes, so the captured
/// contents can be written into it.
///
/// "You can perform read and write operations only on an ACTIVE table" (the model's
/// doc for `CreateTable`), and a table is created in a transient status for a span:
/// the restore of a table that holds data therefore takes at least as long as that
/// table takes to become usable, which is why the restore job reports the
/// non-terminal status until the data is in.
///
/// The wait is the owning service's OWN read: `DescribeTable` answers a poll once the
/// creation it still owes has fallen due, so the status it reports is the one in force
/// when it answers and nothing is waited for longer than that service takes. BOUNDED:
/// at most [`TABLE_READY_POLLS`] reads, each one in-process call, so a table that
/// never becomes usable ends the restore with a reason instead of waiting forever.
async fn wait_until_serving(
    client: &::aws_sdk_dynamodb::Client,
    table_name: &str,
) -> Result<(), String> {
    for _ in 0..TABLE_READY_POLLS {
        let described = client
            .describe_table()
            .table_name(table_name)
            .send()
            .await
            .map_err(|error| {
                format!(
                    "the restored table could not be read back: {}",
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                )
            })?;
        let serving = described
            .table
            .as_ref()
            .and_then(|table| table.table_status.as_ref())
            .is_some_and(|status| status == &::aws_sdk_dynamodb::types::TableStatus::Active);
        if serving {
            return Ok(());
        }
        ::tokio::time::sleep(TABLE_READY_POLL_WAIT).await;
    }
    Err(format!(
        "the restored table {table_name} did not become usable, so the items the recovery \
         point captured could not be written into it"
    ))
}

/// How many times the wait above looks, and how long it waits between looks: the read
/// it makes already waits out the span the owning service's clock owes a new table, so
/// these bound only a table that never becomes usable at all.
const TABLE_READY_POLLS: usize = 20;
const TABLE_READY_POLL_WAIT: ::std::time::Duration = ::std::time::Duration::from_millis(100);

/// The secondary indexes a restored table is created with, read off the source
/// table's description: [`DeclaredIndexes`], or the name of an index the source
/// describes too partially to re-create.
///
/// `pay_per_request` is the billing mode the restored table is created in, which
/// decides whether a global index may carry capacity at all: "Neither
/// ReadCapacityUnits nor WriteCapacityUnits can be specified when BillingMode is
/// PAY_PER_REQUEST" (DynamoDB's refusal for an index of such a table), while a
/// provisioned one requires both. A local index has no capacity of its own: it
/// shares the table's.
fn declared_indexes(
    source: &::aws_sdk_dynamodb::types::TableDescription,
    pay_per_request: bool,
) -> Result<DeclaredIndexes, String> {
    let mut declared = DeclaredIndexes::default();
    for index in source.global_secondary_indexes.iter().flatten() {
        let name = index.index_name.clone().unwrap_or_default();
        let mut builder = ::aws_sdk_dynamodb::types::GlobalSecondaryIndex::builder()
            .set_index_name(index.index_name.clone())
            .set_key_schema(index.key_schema.clone())
            .set_projection(index.projection.clone());
        if !pay_per_request {
            builder = builder.provisioned_throughput(index_throughput(
                index.provisioned_throughput.as_ref(),
            ));
        }
        let built = builder.build().map_err(|_| name)?;
        declared.global.push(built);
    }
    for index in source.local_secondary_indexes.iter().flatten() {
        let name = index.index_name.clone().unwrap_or_default();
        let built = ::aws_sdk_dynamodb::types::LocalSecondaryIndex::builder()
            .set_index_name(index.index_name.clone())
            .set_key_schema(index.key_schema.clone())
            .set_projection(index.projection.clone())
            .build()
            .map_err(|_| name)?;
        declared.local.push(built);
    }
    Ok(declared)
}

/// The two index lists a `CreateTable` takes, as the source table's description
/// gave them.
#[derive(Debug, Default)]
struct DeclaredIndexes {
    global: Vec<::aws_sdk_dynamodb::types::GlobalSecondaryIndex>,
    local: Vec<::aws_sdk_dynamodb::types::LocalSecondaryIndex>,
}

/// The capacity one global secondary index of a provisioned restored table is
/// created with: the source index's own, and the smallest DynamoDB admits for an
/// index the source reports no positive capacity for — the same reading the
/// table's own capacity is carried over by.
fn index_throughput(
    described: Option<&::aws_sdk_dynamodb::types::ProvisionedThroughputDescription>,
) -> ::aws_sdk_dynamodb::types::ProvisionedThroughput {
    let units = |units: Option<i64>| {
        units
            .filter(|units| *units > 0)
            .unwrap_or(DEFAULT_PROVISIONED_CAPACITY)
    };
    ::aws_sdk_dynamodb::types::ProvisionedThroughput::builder()
        .read_capacity_units(units(described.and_then(|described| described.read_capacity_units)))
        .write_capacity_units(units(
            described.and_then(|described| described.write_capacity_units),
        ))
        .build()
        .expect("a provisioned throughput with both capacities set")
}

/// Write the items the recovery point captured into the table `table_name` names,
/// or answer why they could not all be written.
///
/// BOUNDED by what the recovery point holds, which the capture itself bounds
/// ([`super::MAX_CAPTURED_CONTENT_BYTES`]): the items are sent in runs of
/// [`MAX_ITEMS_PER_BATCH_WRITE`], so the number of calls is a fixed fraction of the
/// number of items captured and there is no retry loop. An item the owning service
/// did not process is reported rather than dropped: a restore that reports the data
/// put back must have put all of it back.
async fn write_captured_items(
    client: &::aws_sdk_dynamodb::Client,
    table_name: &str,
    contents: &super::CapturedContents,
) -> Result<(), String> {
    for batch in contents.dynamodb_items.chunks(MAX_ITEMS_PER_BATCH_WRITE) {
        let mut writes = Vec::with_capacity(batch.len());
        for item in batch {
            let put = ::aws_sdk_dynamodb::types::PutRequest::builder()
                .set_item(Some(item.clone().into_iter().collect()))
                .build()
                .map_err(|error| {
                    format!("a captured item could not be written back: {error}")
                })?;
            writes.push(
                ::aws_sdk_dynamodb::types::WriteRequest::builder()
                    .put_request(put)
                    .build(),
            );
        }
        let answer = client
            .batch_write_item()
            .request_items(table_name.to_string(), writes)
            .send()
            .await
            .map_err(|error| {
                format!(
                    "the items the recovery point captured could not be written into the \
                     restored table: {}",
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                )
            })?;
        let unprocessed: usize = answer
            .unprocessed_items
            .iter()
            .flat_map(|items| items.values())
            .map(|writes| writes.len())
            .sum();
        if unprocessed > 0 {
            return Err(format!(
                "{unprocessed} of the items the recovery point captured were not written into \
                 the restored table"
            ));
        }
    }
    Ok(())
}

/// The value the restore metadata carries under `name`, matched
/// case-insensitively — see [`METADATA_TARGET_TABLE_NAME`].
///
/// `pub(crate)` because the SYNCHRONOUS refusal of metadata that names no such key reads it
/// through the same step the restore does ([`super::rejected_restore_metadata`]), so the
/// refusal and the restore can never disagree about which keys a request carries.
pub(crate) fn metadata_value<'m>(
    metadata: &'m BTreeMap<String, String>,
    name: &str,
) -> Option<&'m str> {
    metadata
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

/// The table name a DynamoDB table ARN names: the resource field is
/// `table/<name>`.
///
/// `pub(crate)` because asking DynamoDB whether it HOLDS that table reads the ARN through
/// the same step ([`super::protected_resource_answer`]).
pub(crate) fn table_name_of(resource_arn: &str) -> Option<&str> {
    let resource = resource_arn.splitn(6, ':').nth(5)?;
    resource.strip_prefix("table/").filter(|name| !name.is_empty())
}

/// The Amazon DynamoDB client a restore is performed through, and that the lookup of a
/// protected table asks ([`super::protected_resource_answer`]) — ONE construction of the
/// hop for both directions of the same question.
///
/// Configured as every other in-process client in this tree is
/// (`::service_common::eventbridge_client`): the callback routes IN PROCESS to
/// this emulator's own inbound listener, so the endpoint URL is addressing for
/// that transport rather than a host anything connects to; `region` is the
/// Region the restore runs in; and the credentials are the callback's, which
/// sign this hop and never appear in a response. There is nothing to retry,
/// time out or watch for a stalled stream over an in-process transport, and each
/// of those would need a sleep implementation this client is not given.
///
/// Built here rather than in `services/common` on purpose: that crate is a
/// dependency of EVERY service crate, so the DynamoDB SDK declared there would
/// be compiled into every one of them for the sake of this one constructor.
/// The sibling copy in the Redshift crate's zero-ETL source lookup is the other
/// service that asks DynamoDB something, and it is configured identically.
pub(crate) fn dynamodb_client(
    callback: &EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_dynamodb::Client {
    let config = ::aws_sdk_dynamodb::Config::builder()
        .behavior_version(::aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(::aws_sdk_dynamodb::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://dynamodb.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_dynamodb::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_dynamodb::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_dynamodb::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_dynamodb::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_dynamodb::Client::from_conf(config)
}
