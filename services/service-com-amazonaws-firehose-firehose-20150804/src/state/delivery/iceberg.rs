// SPDX-License-Identifier: CC0-1.0

//! The delivery of a stream's buffer to an APACHE ICEBERG table, and the commit that makes
//! the delivered rows part of that table.
//!
//! "Configure Apache Iceberg Tables destination" (`IcebergDestinationConfiguration`): the
//! destination is a table of a Data Catalog, not an Amazon S3 prefix, so delivering a buffer
//! to it is three writes and a catalog advance rather than one object:
//!
//! * the ROWS — the records of the buffer, read against the destination table's declared
//!   columns and written as one Parquet data object under the table's own `data/` directory;
//! * the MANIFEST and the MANIFEST LIST — the two Avro objects an Iceberg snapshot records
//!   its data files in, under the table's `metadata/` directory;
//! * the METADATA FILE — a new `NNNNN-<id>.metadata.json` carrying the snapshot the commit
//!   added, whose summary counts the records it added;
//! * the CATALOG ADVANCE — the destination table's `metadata_location` parameter, moved to the
//!   metadata file just written, which is what makes the snapshot the table's current state
//!   for every reader that resolves the catalog pointer.
//!
//! A record the destination table's columns do not hold is NOT dropped: it is written under
//! the destination's error output with the Iceberg failure envelope
//! ([`ICEBERG_MALFORMED_FIELD`]), exactly as every other record a delivery could not place is,
//! so a consumer of that prefix can both diagnose and re-drive it.
//!
//! A destination table that is NOT an Iceberg table — one the catalog holds no
//! `metadata_location` for, including a table whose `table_type` parameter was set by a client
//! without an `OpenTableFormatInput` — has no metadata for a commit to advance, so a buffer
//! routed to it writes nothing at all and nothing is retro-fitted onto the catalog row: the
//! records are accepted by the stream and the table stays exactly as it was.
//!
//! ## Lock order
//!
//! The Data Catalog is read (and, for the advance, written) with ITS own lock, and the object
//! store with its own, each taken on its own; this service's store is not held while either is
//! ([`super::deliver_due`] takes the buffer before this runs). So no two of the three are ever
//! held at once.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::parquet::basic::{Compression, ConvertedType, Repetition, Type as PhysicalType};
use ::parquet::data_type::{
    BoolType, ByteArray, ByteArrayType, DoubleType, FloatType, Int32Type, Int64Type,
};
use ::parquet::file::properties::WriterProperties;
use ::parquet::file::writer::SerializedFileWriter;
use ::parquet::schema::types::Type as SchemaType;

use crate::ops::types::{BufferingHints, DestinationTableConfiguration};
use crate::state::delivery::destination_logging::DestinationLogging;
use crate::state::delivery::time_zone::DeliveryTimeZone;
use crate::state::{DestinationConfigKind, StoredRecord};

use super::{
    bucket_name, error_payload, expand_prefix, object_name, DeliveredObject, DeliveryFailure,
    ObjectDestination, PendingDelivery, RecordFault,
};

/// The failure of a record whose own data the destination TABLE does not hold: a value that is
/// not of the Iceberg type the column it stands in declares, or a record that is not one JSON
/// object at all.
///
/// Such a record is written under the destination's error output with this code, which is what
/// a consumer of that prefix tells a record the table refused from one it delivered.
pub(super) const ICEBERG_MALFORMED_FIELD: DeliveryFailure = DeliveryFailure {
    output_type: "iceberg-failed",
    error_code: "Iceberg.MalformedFieldWithinRecord",
    error_message: "this record carries a field the destination table does not hold",
};

/// The `content` value of a data file and of a manifest holding data files: Iceberg's own
/// enumeration, in which zero is DATA (as against the delete files the other values name).
const CONTENT_DATA: i32 = 0;

/// The `status` of an entry a commit ADDS, from Iceberg's manifest-entry enumeration.
const STATUS_ADDED: i32 = 1;

/// The file format of the data objects this writer appends, as Iceberg names it.
const DATA_FILE_FORMAT: &str = "PARQUET";

/// The snapshot operation a delivery commits: Iceberg's own name for a commit that only adds
/// data files.
const APPEND_OPERATION: &str = "append";

/// The branch an append commits onto, which Iceberg names in a table's refs.
const MAIN_BRANCH: &str = "main";

/// How many rows one row group of a committed data object holds. A CHOSEN bound, the same
/// shape as the one the data-format conversion writes with: the rows of a chunk are written and
/// dropped before the next chunk is read, so what this holds is one chunk's values and not the
/// whole buffer's.
const MAX_ROWS_PER_ROW_GROUP: usize = 500;

/// How many columns of a destination table one commit writes. A CHOSEN bound: the records of a
/// flush are bounded by the stream's own buffer budget, but the COLUMNS come from the Data
/// Catalog, so this is what keeps the leaves one commit writes bounded whatever a table
/// declares.
const MAX_TABLE_COLUMNS: usize = 1_000;

/// One Apache Iceberg destination of a stream's buffer, read off the destination configuration
/// in force at the moment of the flush.
#[derive(Debug, Clone)]
pub struct IcebergTarget {
    /// `IcebergDestinationConfiguration$RoleARN` — the role every write of this delivery is
    /// made under.
    pub role_arn: String,
    /// The Data Catalog the destination tables live in: the catalog `CatalogConfiguration`
    /// names, and `None` for a configuration naming none, which is this account's own
    /// catalog.
    pub catalog_id: Option<String>,
    /// The destination tables the configuration lists, in configuration order.
    pub tables: Vec<DestinationTable>,
    /// `BufferingHints`, which decide when this destination's buffer is due. The same two
    /// parameters as every other destination's, so one window rule serves them all
    /// ([`super::delivery_window`]).
    pub hints: Option<BufferingHints>,
    /// Where a delivery this destination did not take is reported.
    pub logging: Option<DestinationLogging>,
    /// The bucket of the configuration's required `S3Configuration`, where the records this
    /// destination could not place are written.
    pub error_bucket: String,
    /// That bucket's `BucketARN`, verbatim.
    pub error_bucket_arn: String,
    /// That configuration's `ErrorOutputPrefix`, verbatim; the expansion happens per object.
    pub error_output_prefix: String,
    /// The key an object written here is encrypted at rest with, from the `S3Configuration`'s
    /// `EncryptionConfiguration`.
    pub kms_key_arn: Option<String>,
}

/// One table of `DestinationTableConfigurationList`: "the name of the Apache Iceberg Table"
/// and the database it lives in, with the error output prefix that table's own failures are
/// written under.
#[derive(Debug, Clone)]
pub struct DestinationTable {
    /// `DestinationDatabaseName`.
    pub database: String,
    /// `DestinationTableName`.
    pub table: String,
    /// `S3ErrorOutputPrefix` — "the table specific S3 error output prefix" — and `None` when
    /// the table's configuration names none, in which case the destination's own
    /// `ErrorOutputPrefix` is where its failures go.
    pub error_output_prefix: Option<String>,
}

/// Every Apache Iceberg table a destination configuration delivers to — the tables an
/// `Iceberg` destination lists, and none for a destination of any other kind.
#[must_use]
pub fn iceberg_targets(config: &DestinationConfigKind) -> Vec<IcebergTarget> {
    let DestinationConfigKind::Iceberg(config) = config else {
        return Vec::new();
    };
    let tables: Vec<DestinationTable> = config
        .destination_table_configuration_list
        .iter()
        .flatten()
        .map(destination_table)
        .collect();
    if tables.is_empty() {
        return Vec::new();
    }
    let Some(error_bucket) = bucket_name(&config.s3_configuration.bucket_arn) else {
        return Vec::new();
    };
    vec![IcebergTarget {
        role_arn: config.role_arn.clone(),
        catalog_id: catalog_account(config.catalog_configuration.catalog_arn.as_deref()),
        tables,
        hints: config.buffering_hints.clone(),
        logging: DestinationLogging::of(config.cloud_watch_logging_options.as_ref()),
        error_bucket: error_bucket.to_string(),
        error_bucket_arn: config.s3_configuration.bucket_arn.clone(),
        error_output_prefix: config
            .s3_configuration
            .error_output_prefix
            .clone()
            .unwrap_or_default(),
        kms_key_arn: super::kms_key_arn(config.s3_configuration.encryption_configuration.as_ref()),
    }]
}

/// One listed destination table, read off its configuration.
fn destination_table(config: &DestinationTableConfiguration) -> DestinationTable {
    DestinationTable {
        database: config.destination_database_name.clone(),
        table: config.destination_table_name.clone(),
        error_output_prefix: config
            .s3_error_output_prefix
            .clone()
            .filter(|prefix| !prefix.is_empty()),
    }
}

/// The Data Catalog a `CatalogConfiguration$CatalogARN` names: the ACCOUNT of that ARN, which
/// is how a Data Catalog is addressed (`arn:<partition>:glue:<region>:<account>:catalog`).
pub(crate) fn catalog_account(catalog_arn: Option<&str>) -> Option<String> {
    let account = catalog_arn?.split(':').nth(4)?;
    (!account.is_empty()).then(|| account.to_string())
}

/// What one Iceberg destination's delivery writes: the objects, and the catalog pointers the
/// commit advances once they are written.
#[derive(Debug, Default)]
pub(super) struct IcebergCommit {
    pub(super) objects: Vec<DeliveredObject>,
    pub(super) advances: Vec<CatalogAdvance>,
}

/// One destination table's `metadata_location`, moved to the metadata file a commit wrote.
#[derive(Debug, Clone)]
pub(super) struct CatalogAdvance {
    catalog_id: Option<String>,
    database: String,
    table: String,
    metadata_location: String,
}

/// The destination table as the catalog holds it, cloned out of the Data Catalog before
/// anything is written so that store is not held while a commit is built.
#[derive(Debug, Clone)]
struct CatalogTable {
    /// The table's own location — `StorageDescriptor$Location` — without a trailing separator.
    location: String,
    /// The table's columns as `(name, declared Hive type)` in table order.
    columns: Vec<(String, String)>,
    /// Where the table's CURRENT metadata file lives, which the commit reads the table's
    /// snapshot history out of and then replaces.
    metadata_location: String,
}

/// Deliver `delivery`'s buffer to `target`'s Iceberg tables: the objects a commit writes and
/// the catalog advances it owes.
///
/// `None` when the delivery could not be worked out at all right now — a destination listing
/// more tables than one commit walks, a table declaring a column type this writer has no leaf
/// for, a Parquet or Avro writer that refused the body — so the caller puts the whole buffer
/// back rather than writing part of it or reporting a record failure the real service would
/// have committed.
///
/// WHAT THIS WALKS is bounded by the buffer and NOT by the product of the buffer and the
/// configuration: the records are routed ONCE for the whole delivery ([`routed_buffer`]) rather
/// than re-read per configured table, the tables one commit walks are capped
/// ([`MAX_DESTINATION_TABLES`]), and the loop hands the runtime back per table and per record,
/// so a cancellation or a shutdown can land while a long configuration is being committed.
pub(super) async fn commit_buffer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    delivery: &PendingDelivery,
    target: &IcebergTarget,
    now: SystemTime,
) -> Option<IcebergCommit> {
    if target.tables.len() > MAX_DESTINATION_TABLES {
        ::tracing::warn!(
            tables = target.tables.len(),
            "the destination lists more tables than one commit walks here, so this delivery \
             was left for a later one"
        );
        return None;
    }
    // WHERE each record goes, decided in one pass over the buffer: a record's own routing
    // metadata is read once, whatever the configuration lists.
    let routed = routed_buffer(delivery, target).await;
    let mut commit = IcebergCommit::default();
    for (index, binding) in target.tables.iter().enumerate() {
        ::tokio::task::yield_now().await;
        let records = routed.to_table(index);
        let misrouted = routed.misrouted(index);
        if records.is_empty() && misrouted.is_empty() {
            continue;
        }
        // A table the catalog holds no Iceberg metadata for has no snapshot history to
        // commit onto, so this buffer writes nothing for it — and nothing is written onto the
        // catalog row either.
        let Some(table) = catalog_table(engine, target, binding) else {
            ::tracing::debug!(
                database = %binding.database,
                table = %binding.table,
                "the destination table carries no Iceberg metadata, so this buffer committed \
                 nothing to it"
            );
            continue;
        };
        let columns = typed_columns(&table)?;
        // Each record read against the table's columns: the rows that are committed, and the
        // records whose own data the table does not hold.
        let mut rows = Vec::with_capacity(records.len());
        let mut failed: Vec<&StoredRecord> = Vec::new();
        let mut faults: BTreeMap<String, RecordFault> = BTreeMap::new();
        for record in misrouted {
            faults.insert(
                record.record_id.clone(),
                RecordFault {
                    error_code: ICEBERG_MALFORMED_FIELD.error_code,
                    message: "the routing metadata of this record names a table this \
                              destination does not deliver to"
                        .to_string(),
                },
            );
            failed.push(record);
        }
        for record in records {
            ::tokio::task::yield_now().await;
            match row_of(&record.data, &columns) {
                Ok(row) => rows.push(row),
                Err(fault) => {
                    faults.insert(record.record_id.clone(), fault);
                    failed.push(record);
                }
            }
        }
        if !failed.is_empty() {
            // An error output prefix this emulator cannot resolve is not a record it drops:
            // the whole delivery is owed again instead.
            let prefix = error_prefix(target, binding, now)?;
            commit.objects.push(DeliveredObject {
                bucket: target.error_bucket.clone(),
                key: format!(
                    "{prefix}{}",
                    object_name(delivery, now, DeliveryTimeZone::default())
                ),
                body: error_payload(
                    &failed,
                    ICEBERG_MALFORMED_FIELD,
                    now,
                    None,
                    // A row this table does not hold was judged by the commit and by no data
                    // transformation, so no function is named beside the failure.
                    None,
                    &faults,
                    None,
                ),
                content_encoding: None,
                kms_key_arn: target.kms_key_arn.clone(),
                destination: ObjectDestination {
                    bucket_arn: target.error_bucket_arn.clone(),
                    role_arn: target.role_arn.clone(),
                    logging: target.logging.clone(),
                },
            });
        }
        if rows.is_empty() {
            continue;
        }
        let added = rows.len();
        let data = parquet_object(&binding.table, &columns, &rows).await?;
        let snapshot = append_snapshot(engine, delivery, target, binding, &table, data, added, now)?;
        commit.objects.extend(snapshot.objects);
        commit.advances.push(snapshot.advance);
    }
    Some(commit)
}

/// The most destination tables one commit walks, and the most one catalog resolution does
/// ([`crate::state::iceberg_catalog`]).
///
/// A CHOSEN bound: `DestinationTableConfigurationList` is a caller-written list and the work a
/// commit does is per table, so this is what keeps a configuration from deciding how much one
/// delivery does — beside [`MAX_TABLE_COLUMNS`], which bounds the per-table half. Far above the
/// tables one Firehose stream delivers to, so a configuration a client would really write is
/// never refused by it.
pub(crate) const MAX_DESTINATION_TABLES: usize = 1_000;

/// Where every record of a buffer goes: the records each configured table takes, and the
/// records whose routing names a table the destination does not list.
///
/// Decided in ONE pass over the buffer, so a record's own bytes are read once however many
/// tables the destination lists: what this walks is the buffer, not the product of the buffer
/// and the configuration.
struct RoutedBuffer<'r> {
    /// One bucket per configured table, in configuration order.
    by_table: Vec<Vec<&'r StoredRecord>>,
    /// The records routed nowhere this destination delivers to, reported by the FIRST
    /// configured table.
    misrouted: Vec<&'r StoredRecord>,
}

impl<'r> RoutedBuffer<'r> {
    /// The records the `index`-th configured table takes.
    fn to_table(&self, index: usize) -> &[&'r StoredRecord] {
        self.by_table.get(index).map_or(&[], Vec::as_slice)
    }

    /// The records the `index`-th configured table reports as placed nowhere: the first table's
    /// to report, so one error-output object carries them.
    fn misrouted(&self, index: usize) -> &[&'r StoredRecord] {
        if index == 0 {
            &self.misrouted
        } else {
            &[]
        }
    }
}

/// Route the buffer: each record to the table its own routing metadata names.
///
/// "Firehose will write data with insert if table specific configuration is not provided here"
/// (`DestinationTableConfigurationList`), so a record that says nothing about where it goes
/// goes to the destination the configuration names first — which for a destination listing ONE
/// table is the whole buffer, the only routing such a configuration can mean, read without
/// looking at a record at all. A record whose routing metadata names a table this destination
/// does NOT list is routed to no table: delivering it to a table the producer did not ask for
/// would be a wrong answer carrying a success, so it is reported under the error output
/// instead.
///
/// ASYNC because reading a long buffer's routing is CPU-bound: the loop hands the runtime back
/// between records, so a cancellation can land while a buffer is being routed.
async fn routed_buffer<'r>(
    delivery: &'r PendingDelivery,
    target: &IcebergTarget,
) -> RoutedBuffer<'r> {
    let mut by_table: Vec<Vec<&StoredRecord>> = vec![Vec::new(); target.tables.len().max(1)];
    let mut misrouted = Vec::new();
    if target.tables.len() <= 1 {
        if let Some(first) = by_table.first_mut() {
            first.extend(delivery.records.iter());
        }
        return RoutedBuffer {
            by_table,
            misrouted,
        };
    }
    for record in &delivery.records {
        ::tokio::task::yield_now().await;
        match routed_table(record, target) {
            Routing::Listed(index) => {
                if let Some(bucket) = by_table.get_mut(index) {
                    bucket.push(record);
                }
            }
            Routing::Unrouted => {
                if let Some(first) = by_table.first_mut() {
                    first.push(record);
                }
            }
            Routing::Unlisted => misrouted.push(record),
        }
    }
    RoutedBuffer {
        by_table,
        misrouted,
    }
}

/// Where a record's own routing metadata sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Routing {
    /// The record carries no routing metadata, so the configuration decides.
    Unrouted,
    /// The record names the `index`-th table this destination lists.
    Listed(usize),
    /// The record names a table this destination does not list, which is a record it cannot
    /// place anywhere.
    Unlisted,
}

/// Which configured table a record's own routing metadata names.
fn routed_table(record: &StoredRecord, target: &IcebergTarget) -> Routing {
    let Ok(document) = ::serde_json::from_slice::<::serde_json::Value>(&record.data) else {
        return Routing::Unrouted;
    };
    let named = document.get(RECORD_ROUTING_MEMBER).and_then(|routing| {
        let database = routing.get("destinationDatabaseName")?.as_str()?;
        let table = routing.get("destinationTableName")?.as_str()?;
        Some((database.to_string(), table.to_string()))
    });
    let Some((database, table)) = named else {
        return Routing::Unrouted;
    };
    target
        .tables
        .iter()
        .position(|binding| binding.database == database && binding.table == table)
        .map_or(Routing::Unlisted, Routing::Listed)
}

/// The member of a record that carries WHICH table it is delivered to, for a destination that
/// lists several: the routing metadata a producer puts on a record for an Iceberg destination.
const RECORD_ROUTING_MEMBER: &str = "otfMetadata";

/// The destination table as the Data Catalog holds it, or `None` when the catalog holds no
/// such table, no location for it, or no Iceberg metadata pointer on it.
fn catalog_table<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target: &IcebergTarget,
    binding: &DestinationTable,
) -> Option<CatalogTable> {
    let catalog_id =
        ::service_com_amazonaws_glue_awsglue::state::resolve_catalog_id(target.catalog_id.as_ref());
    let state = ::service_com_amazonaws_glue_awsglue::state::service_state(engine)?;
    let store = state.lock();
    let entry = store
        .database(&catalog_id, &binding.database)?
        .tables
        .get(&::service_com_amazonaws_glue_awsglue::state::fold_name(
            &binding.table,
        ))?;
    let metadata_location =
        ::service_com_amazonaws_glue_awsglue::state::marked_metadata_location(
            entry.parameters.as_ref(),
        )?;
    let descriptor = entry.storage_descriptor.as_ref()?;
    let location = descriptor
        .location
        .as_deref()
        .filter(|location| !location.is_empty())?
        .trim_end_matches('/')
        .to_string();
    let columns: Vec<(String, String)> = descriptor
        .columns
        .iter()
        .flatten()
        .map(|column| (column.name.clone(), column.r#type.clone().unwrap_or_default()))
        .collect();
    (!columns.is_empty()).then_some(CatalogTable {
        location,
        columns,
        metadata_location,
    })
}

/// The expanded prefix a record this destination could not place is written under: the table's
/// own `S3ErrorOutputPrefix` where it names one, else the destination's `ErrorOutputPrefix`,
/// with the error output type as a segment of it.
///
/// "A prefix that Firehose evaluates and adds to failed records before writing them to S3"
/// (`ErrorOutputPrefix`): the Iceberg failures of a destination live under their OWN segment of
/// it, so a consumer reading that segment reads the records the table refused and nothing else.
/// A prefix that already names the type through the `firehose:error-output-type` namespace
/// carries it exactly where the prefix puts it; one that does not gets the segment appended,
/// which for a destination configuring no prefix at all is the whole prefix — the same form
/// every other destination's error output takes.
fn error_prefix(
    target: &IcebergTarget,
    binding: &DestinationTable,
    now: SystemTime,
) -> Option<String> {
    let configured = binding
        .error_output_prefix
        .clone()
        .unwrap_or_else(|| target.error_output_prefix.clone());
    let configured = if configured.contains(ERROR_OUTPUT_TYPE_NAMESPACE) {
        configured
    } else {
        format!("{configured}{ERROR_OUTPUT_TYPE_NAMESPACE}/")
    };
    expand_prefix(
        &configured,
        &BTreeMap::new(),
        Some(ICEBERG_MALFORMED_FIELD.output_type),
        now,
        DeliveryTimeZone::default(),
    )
    .ok()
}

/// The `ErrorOutputPrefix` namespace that expands to WHY a record is in the error output:
/// "You can use `!{firehose:error-output-type}` only in `ErrorOutputPrefix`."
const ERROR_OUTPUT_TYPE_NAMESPACE: &str = "!{firehose:error-output-type}";

/// What one commit writes beside the data object, and the pointer it advances.
struct Snapshot {
    objects: Vec<DeliveredObject>,
    advance: CatalogAdvance,
}

/// The objects one APPEND commit writes — the data object, the manifest of that data file, the
/// manifest list of the snapshot and the new metadata file — and the catalog advance that makes
/// the snapshot current.
///
/// The commit is built on the table's CURRENT state, read out of the object store at the
/// location the catalog points at: the metadata document for the schema, the snapshot history
/// and the totals, and the current snapshot's own MANIFEST LIST for the manifests the table
/// already holds. An Iceberg snapshot's manifest list enumerates every manifest the table
/// consists of, so the new snapshot's list carries the current one's entries and adds this
/// delivery's — a later delivery therefore leaves every earlier data file reachable from the
/// table. A table whose current metadata file is not there — the pointer names an object the
/// store does not hold — is committed onto a document built from the table's own definition,
/// which is the table as its create minted it; one whose current snapshot names a manifest list
/// the store does not hold is a table whose state cannot be read, so nothing is written and the
/// buffer is owed again rather than committed onto a state that would lose the earlier files.
#[allow(clippy::too_many_arguments)]
fn append_snapshot<Eng: Send + Sync + 'static>(
    engine: &Eng,
    delivery: &PendingDelivery,
    target: &IcebergTarget,
    binding: &DestinationTable,
    table: &CatalogTable,
    data: Vec<u8>,
    added: usize,
    now: SystemTime,
) -> Option<Snapshot> {
    let (bucket, _) =
        ::service_com_amazonaws_glue_awsglue::state::split_s3_location(&table.location)?;
    let bucket_arn = ::service_com_amazonaws_s3_amazons3::state::bucket_arn(&bucket);
    let destination = ObjectDestination {
        bucket_arn,
        role_arn: target.role_arn.clone(),
        logging: target.logging.clone(),
    };
    let current = current_metadata(engine, &table.metadata_location);
    let held = held_state(engine, current.as_ref())?;
    // The sequence number of this commit, after the table's last one. A stored document
    // carrying a value nothing can follow is a state this commit cannot be built on, so the
    // buffer is owed again rather than committed under a number that wrapped.
    let sequence_number = held.last_sequence_number.checked_add(1)?;
    let snapshot_id = new_snapshot_id();
    let data_name = format!(
        "00000-{}-{}.parquet",
        delivery.version_id,
        ::uuid::Uuid::new_v4()
    );
    let data_path = format!("{}/data/{data_name}", table.location);
    let data_size = i64::try_from(data.len()).ok()?;
    let added = i64::try_from(added).ok()?;
    let manifest_id = ::uuid::Uuid::new_v4();
    let manifest_path = format!("{}/metadata/{manifest_id}-m0.avro", table.location);
    let manifest = manifest_object(&data_path, data_size, added, snapshot_id, sequence_number)?;
    let manifest_list_path = format!(
        "{}/metadata/snap-{snapshot_id}-1-{manifest_id}.avro",
        table.location
    );
    let manifest_list = manifest_list_object(
        &held.manifests,
        &ManifestFile {
            path: manifest_path.clone(),
            length: i64::try_from(manifest.len()).ok()?,
            records: added,
            snapshot_id,
            sequence_number,
        },
    )?;
    let document = committed_metadata(
        table,
        current,
        &manifest_list_path,
        snapshot_id,
        sequence_number,
        Added {
            records: added,
            bytes: data_size,
        },
        &held,
        now,
    );
    let metadata_path = format!(
        "{}/metadata/{:05}-{}.metadata.json",
        table.location,
        next_metadata_number(&table.metadata_location),
        ::uuid::Uuid::new_v4()
    );
    let objects = vec![
        object_at(&data_path, data, target, &destination)?,
        object_at(&manifest_path, manifest, target, &destination)?,
        object_at(&manifest_list_path, manifest_list, target, &destination)?,
        object_at(
            &metadata_path,
            ::serde_json::to_vec(&document).ok()?,
            target,
            &destination,
        )?,
    ];
    Some(Snapshot {
        objects,
        advance: CatalogAdvance {
            catalog_id: target.catalog_id.clone(),
            database: binding.database.clone(),
            table: binding.table.clone(),
            metadata_location: metadata_path,
        },
    })
}

/// What one commit ADDS to the table, which its snapshot summary states.
struct Added {
    records: i64,
    bytes: i64,
}

/// What the table already HOLDS, as the commit reads it off the table's current state: the
/// manifests its current snapshot enumerates, the sequence number its last commit reached and
/// the totals its last snapshot summary stated.
struct Held {
    /// The entries of the current snapshot's manifest list, verbatim, so the new list carries
    /// the manifests the table consists of beside the one this delivery wrote.
    manifests: Vec<::apache_avro::types::Value>,
    last_sequence_number: i64,
    total_records: i64,
    total_data_files: i64,
    total_files_size: i64,
}

/// The table's current state, or `None` when its current snapshot names a manifest list the
/// object store does not hold — a state a commit cannot be built on.
///
/// A table with no current snapshot holds nothing, which is what a table its create just minted
/// is: no manifest, no record, no file.
fn held_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
    current: Option<&::serde_json::Value>,
) -> Option<Held> {
    let Some(document) = current else {
        return Some(Held {
            manifests: Vec::new(),
            last_sequence_number: 0,
            total_records: 0,
            total_data_files: 0,
            total_files_size: 0,
        });
    };
    let last_sequence_number = document["last-sequence-number"].as_i64().unwrap_or(0);
    let snapshot = current_snapshot(document);
    let summary_total = |name: &str| {
        snapshot
            .and_then(|snapshot| snapshot["summary"][name].as_str())
            .and_then(|total| total.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let manifests = match snapshot.and_then(|snapshot| snapshot["manifest-list"].as_str()) {
        // The manifests the table consists of right now. The list has to be READ: a commit
        // that could not read it would write a snapshot naming only its own manifest, which
        // is a table that has lost every earlier data file.
        Some(manifest_list) => manifest_entries(engine, manifest_list)?,
        None => Vec::new(),
    };
    Some(Held {
        manifests,
        last_sequence_number,
        total_records: summary_total("total-records"),
        total_data_files: summary_total("total-data-files"),
        total_files_size: summary_total("total-files-size"),
    })
}

/// The snapshot a metadata document reports as current, by its `current-snapshot-id`, and
/// `None` for a document that reports none.
fn current_snapshot(document: &::serde_json::Value) -> Option<&::serde_json::Value> {
    let current = document["current-snapshot-id"].as_i64()?;
    document["snapshots"]
        .as_array()?
        .iter()
        .find(|snapshot| snapshot["snapshot-id"].as_i64() == Some(current))
}

/// One object of a commit, addressed by its `s3://` path.
fn object_at(
    path: &str,
    body: Vec<u8>,
    target: &IcebergTarget,
    destination: &ObjectDestination,
) -> Option<DeliveredObject> {
    let (bucket, key) = ::service_com_amazonaws_glue_awsglue::state::split_s3_location(path)?;
    if key.is_empty() {
        return None;
    }
    Some(DeliveredObject {
        bucket,
        key,
        body,
        content_encoding: None,
        kms_key_arn: target.kms_key_arn.clone(),
        destination: destination.clone(),
    })
}

/// The table's CURRENT metadata document, read out of the object store at the location the
/// catalog points at, or `None` when the store holds no such object.
fn current_metadata<Eng: Send + Sync + 'static>(
    engine: &Eng,
    metadata_location: &str,
) -> Option<::serde_json::Value> {
    let (bucket, key) =
        ::service_com_amazonaws_glue_awsglue::state::split_s3_location(metadata_location)?;
    let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
    let store = state.lock();
    let body = &store.bucket(&bucket)?.object(&key)?.body;
    ::serde_json::from_slice(body).ok()
}

/// The metadata document the commit writes: the table's current one with the snapshot this
/// delivery appended, or — for a table whose current document could not be read — one built
/// from the table's own definition carrying that snapshot.
///
/// The snapshot's summary states what the commit ADDED and what the table then HOLDS, each
/// total being the one the table already held plus what this commit added — the counts a reader
/// of the snapshot sizes the table by.
#[allow(clippy::too_many_arguments)]
fn committed_metadata(
    table: &CatalogTable,
    current: Option<::serde_json::Value>,
    manifest_list: &str,
    snapshot_id: i64,
    sequence_number: i64,
    added: Added,
    held: &Held,
    now: SystemTime,
) -> ::serde_json::Value {
    let columns = table
        .columns
        .iter()
        .map(|(name, declared)| (name.clone(), declared.clone()))
        .collect::<Vec<_>>();
    let mut document = current.unwrap_or_else(|| {
        ::service_com_amazonaws_glue_awsglue::state::initial_table_metadata(
            &table.location,
            ::service_com_amazonaws_glue_awsglue::state::DEFAULT_ICEBERG_FORMAT_VERSION,
            &::service_com_amazonaws_glue_awsglue::state::new_table_uuid(),
            &columns,
            now,
        )
    });
    let timestamp = epoch_millis(now);
    let snapshot = ::serde_json::json!({
        "snapshot-id": snapshot_id,
        "sequence-number": sequence_number,
        "timestamp-ms": timestamp,
        "manifest-list": manifest_list,
        "schema-id": 0,
        "summary": {
            "operation": APPEND_OPERATION,
            // One data object per commit: this delivery's buffer is written as one file.
            "added-data-files": "1",
            "added-records": added.records.to_string(),
            "added-files-size": added.bytes.to_string(),
            "total-records": held.total_records.saturating_add(added.records).to_string(),
            "total-data-files": held.total_data_files.saturating_add(1).to_string(),
            "total-files-size": held.total_files_size.saturating_add(added.bytes).to_string(),
        },
    });
    let Some(members) = document.as_object_mut() else {
        return document;
    };
    if let Some(snapshots) = members
        .entry("snapshots")
        .or_insert_with(|| ::serde_json::json!([]))
        .as_array_mut()
    {
        snapshots.push(snapshot);
    }
    if let Some(log) = members
        .entry("snapshot-log")
        .or_insert_with(|| ::serde_json::json!([]))
        .as_array_mut()
    {
        log.push(::serde_json::json!({
            "timestamp-ms": timestamp,
            "snapshot-id": snapshot_id,
        }));
    }
    members.insert("last-sequence-number".to_string(), sequence_number.into());
    members.insert("last-updated-ms".to_string(), timestamp.into());
    members.insert("current-snapshot-id".to_string(), snapshot_id.into());
    members.insert(
        "refs".to_string(),
        ::serde_json::json!({
            MAIN_BRANCH: { "snapshot-id": snapshot_id, "type": "branch" }
        }),
    );
    document
}

/// The ordinal the next metadata file of a table carries: Iceberg numbers them from zero and
/// a commit writes the one after the current file's.
fn next_metadata_number(metadata_location: &str) -> u64 {
    metadata_location
        .rsplit('/')
        .next()
        .and_then(|name| name.split('-').next())
        .and_then(|ordinal| ordinal.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1)
}

/// A fresh snapshot id: the identity of ONE commit, which Iceberg states as a positive 64-bit
/// number.
///
/// This names a RESOURCE, not a credential: a credential would come from the credential store,
/// never from here.
fn new_snapshot_id() -> i64 {
    let bits = ::uuid::Uuid::new_v4().as_u128() as u64;
    i64::try_from(bits >> 1).unwrap_or(1).max(1)
}

/// `at` as epoch milliseconds, which is how Iceberg states an instant.
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis())
        .unwrap_or_default()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Move each advanced table's `metadata_location` to the metadata file the commit wrote.
///
/// Called only once the objects of the commit have been written, so the catalog never points at
/// a metadata file that is not there — the invariant the whole commit exists to keep. A table
/// the catalog no longer holds (dropped while the delivery was in flight) is left alone.
pub(super) fn advance_catalog<Eng: Send + Sync + 'static>(
    engine: &Eng,
    advances: &[CatalogAdvance],
    now: SystemTime,
) {
    if advances.is_empty() {
        return;
    }
    let Some(state) = ::service_com_amazonaws_glue_awsglue::state::service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    for advance in advances {
        let catalog_id = ::service_com_amazonaws_glue_awsglue::state::resolve_catalog_id(
            advance.catalog_id.as_ref(),
        );
        let folded = ::service_com_amazonaws_glue_awsglue::state::fold_name(&advance.table);
        let Some(entry) = store
            .database_mut(&catalog_id, &advance.database)
            .and_then(|database| database.tables.get_mut(&folded))
        else {
            continue;
        };
        let Some(parameters) = entry.parameters.as_mut() else {
            continue;
        };
        // Only a table that ALREADY carries the pointer is advanced: a commit moves a table's
        // metadata forward and never makes a table that was not an Iceberg table one.
        if !parameters.contains_key(
            ::service_com_amazonaws_glue_awsglue::state::ICEBERG_METADATA_LOCATION_PARAMETER,
        ) {
            continue;
        }
        parameters.insert(
            ::service_com_amazonaws_glue_awsglue::state::ICEBERG_METADATA_LOCATION_PARAMETER
                .to_string(),
            advance.metadata_location.clone(),
        );
        entry.update_time = now;
    }
}

// ---------------------------------------------------------------------------
// The rows: reading a record against the destination table's columns
// ---------------------------------------------------------------------------

/// The Iceberg type one column of the destination table holds, which decides both the leaf the
/// data object writes it as and the values the column admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldType {
    Boolean,
    /// Iceberg `int`: a 32-bit integer, which Hive's `tinyint`, `smallint` and `int` are
    /// written as.
    Int,
    /// Iceberg `long`, which Hive's `bigint` is written as.
    Long,
    Float,
    Double,
    /// Iceberg `string`, which Hive's `string`, `char(n)` and `varchar(n)` are written as.
    String,
    /// Iceberg `binary`, carried in JSON as base64 like every other blob on an AWS wire.
    Binary,
    /// Iceberg `date`: whole days since the epoch.
    Date,
    /// Iceberg `timestamp`: MICROSECONDS since the epoch, which is the unit Iceberg states a
    /// timestamp in.
    Timestamp,
}

/// One record's value for one column, read as that column's own type.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Null,
    Boolean(bool),
    Int32(i32),
    Int64(i64),
    Float(f32),
    Double(f64),
    Text(String),
    Bytes(Vec<u8>),
}

/// The destination table's columns with the Iceberg type each one holds.
///
/// `None` for a table declaring a type this writer has no leaf for: the real service writes
/// such a column, so reporting a record failure for it would mislabel every record of the
/// buffer — the delivery is owed again instead.
fn typed_columns(table: &CatalogTable) -> Option<Vec<(String, FieldType)>> {
    if table.columns.len() > MAX_TABLE_COLUMNS {
        ::tracing::warn!(
            columns = table.columns.len(),
            "the destination table declares more columns than one commit writes here, so this \
             delivery was left for a later one"
        );
        return None;
    }
    let mut typed = Vec::with_capacity(table.columns.len());
    for (name, declared) in &table.columns {
        match field_type(declared) {
            Some(field) => typed.push((name.clone(), field)),
            None => {
                ::tracing::warn!(
                    column = %name,
                    "the destination table declares a column type this emulator cannot commit, \
                     so this delivery was left for a later one"
                );
                return None;
            }
        }
    }
    Some(typed)
}

/// The Iceberg type a column's declared Glue type holds, read through the catalog's own
/// Hive-to-Iceberg mapping so the data object and the table's metadata document cannot disagree
/// about a column's type.
fn field_type(declared: &str) -> Option<FieldType> {
    match ::service_com_amazonaws_glue_awsglue::state::iceberg_field_type(declared).as_str() {
        "boolean" => Some(FieldType::Boolean),
        "int" => Some(FieldType::Int),
        "long" => Some(FieldType::Long),
        "float" => Some(FieldType::Float),
        "double" => Some(FieldType::Double),
        "string" => Some(FieldType::String),
        "binary" => Some(FieldType::Binary),
        "date" => Some(FieldType::Date),
        "timestamp" | "timestamptz" => Some(FieldType::Timestamp),
        _ => None,
    }
}

/// One record read against the destination table's columns, or the fault that keeps it out of
/// the table.
///
/// A record is one JSON object whose members are the table's columns, matched by name. A column
/// the record names no value for holds none, which is not a failure: the Iceberg fields a Glue
/// column is written as are optional.
fn row_of(data: &[u8], columns: &[(String, FieldType)]) -> Result<Vec<Cell>, RecordFault> {
    let document: ::serde_json::Value = ::serde_json::from_slice(data).map_err(|_| RecordFault {
        error_code: ICEBERG_MALFORMED_FIELD.error_code,
        message: "this record is not the JSON object the destination table's columns are read \
                  from"
            .to_string(),
    })?;
    let Some(members) = document.as_object() else {
        return Err(RecordFault {
            error_code: ICEBERG_MALFORMED_FIELD.error_code,
            message: "this record is not the JSON object the destination table's columns are \
                      read from"
                .to_string(),
        });
    };
    let mut row = Vec::with_capacity(columns.len());
    for (name, field) in columns {
        let value = members.get(name).unwrap_or(&::serde_json::Value::Null);
        row.push(cell_of(name, *field, value)?);
    }
    Ok(row)
}

/// One value read as the column's own Iceberg type, or the fault that keeps the record out of
/// the table.
fn cell_of(
    name: &str,
    field: FieldType,
    value: &::serde_json::Value,
) -> Result<Cell, RecordFault> {
    if value.is_null() {
        return Ok(Cell::Null);
    }
    let malformed = |carried: &str| RecordFault {
        error_code: ICEBERG_MALFORMED_FIELD.error_code,
        message: format!(
            "the column {name} of the destination table does not hold {carried}"
        ),
    };
    match field {
        FieldType::Boolean => value
            .as_bool()
            .map(Cell::Boolean)
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::Int => value
            .as_i64()
            .and_then(|number| i32::try_from(number).ok())
            .map(Cell::Int32)
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::Long => value
            .as_i64()
            .map(Cell::Int64)
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::Float => value
            .as_f64()
            .map(|number| Cell::Float(number as f32))
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::Double => value
            .as_f64()
            .map(Cell::Double)
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::String => value
            .as_str()
            .map(|text| Cell::Text(text.to_string()))
            .ok_or_else(|| malformed(&kind_of(value))),
        FieldType::Binary => value
            .as_str()
            .and_then(|text| ::aws_smithy_types::base64::decode(text).ok())
            .map(Cell::Bytes)
            .ok_or_else(|| malformed("a value that is not base64-encoded data")),
        FieldType::Date => value
            .as_str()
            .and_then(epoch_days)
            .map(Cell::Int32)
            .ok_or_else(|| malformed("a value that is not an ISO-8601 date")),
        FieldType::Timestamp => epoch_micros(value)
            .map(Cell::Int64)
            .ok_or_else(|| malformed("a value that is not an ISO-8601 timestamp")),
    }
}

/// What KIND of JSON value this is, for the part of a fault that is about the value's TYPE
/// rather than its content.
fn kind_of(value: &::serde_json::Value) -> String {
    match value {
        ::serde_json::Value::Null => "a null",
        ::serde_json::Value::Bool(_) => "a boolean",
        ::serde_json::Value::Number(_) => "a number",
        ::serde_json::Value::String(_) => "a string",
        ::serde_json::Value::Array(_) => "an array",
        ::serde_json::Value::Object(_) => "an object",
    }
    .to_string()
}

/// The whole days since the epoch an ISO-8601 calendar date names, or `None` for a value that
/// is no such date.
fn epoch_days(value: &str) -> Option<i32> {
    let date = ::chrono::NaiveDate::parse_from_str(value.trim(), ISO_DATE).ok()?;
    i32::try_from(
        date.signed_duration_since(::chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?)
            .num_days(),
    )
    .ok()
}

/// The MICROSECONDS since the epoch a timestamp value names, or `None` for a value that names
/// no instant.
///
/// An Iceberg `timestamp` literal is an ISO-8601 date and time — with a zone offset (which
/// names an instant directly) or without one (which names a local date and time, read as UTC
/// because an Iceberg `timestamp` carries no zone) — and a producer may also carry an instant
/// as epoch MILLISECONDS, the unit a JSON number states one in on an AWS wire. The
/// space-separated SQL spelling (`YYYY-MM-DD HH:MM:SS`) is not an ISO-8601 date and time, so a
/// record carrying it holds no timestamp this column can take.
fn epoch_micros(value: &::serde_json::Value) -> Option<i64> {
    if let Some(millis) = value.as_i64() {
        return millis.checked_mul(1_000);
    }
    let text = value.as_str()?.trim();
    if let Ok(offset) = ::chrono::DateTime::parse_from_rfc3339(text) {
        return offset.timestamp_micros().into();
    }
    for format in ISO_LOCAL_DATE_TIMES {
        if let Ok(local) = ::chrono::NaiveDateTime::parse_from_str(text, format) {
            return local.and_utc().timestamp_micros().into();
        }
    }
    None
}

/// How an Iceberg `date` value is spelled: the ISO-8601 calendar date.
const ISO_DATE: &str = "%Y-%m-%d";

/// How an Iceberg `timestamp` value with NO zone offset is spelled: the ISO-8601 date and time,
/// with or without a fraction of a second. The `T` is what separates the date from the time in
/// ISO-8601, which is why the space-separated SQL spelling is not one of these.
const ISO_LOCAL_DATE_TIMES: &[&str] = &["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"];

// ---------------------------------------------------------------------------
// The data object: one Parquet file holding the committed rows
// ---------------------------------------------------------------------------

/// The Parquet object the commit writes, holding `rows` as the table's columns.
///
/// ASYNC because serializing a buffer's rows is CPU-bound: the loop hands the runtime back
/// between row groups and between columns, so a cancellation can land while a long buffer is
/// being written instead of the whole run holding the worker.
async fn parquet_object(
    table: &str,
    columns: &[(String, FieldType)],
    rows: &[Vec<Cell>],
) -> Option<Vec<u8>> {
    let schema = message_type(table, columns)?;
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = match SerializedFileWriter::new(
        Vec::new(),
        ::std::sync::Arc::new(schema),
        ::std::sync::Arc::new(properties),
    ) {
        Ok(writer) => writer,
        Err(error) => {
            ::tracing::warn!(%error, "the Parquet writer refused this commit's data object");
            return None;
        }
    };
    for chunk in rows.chunks(MAX_ROWS_PER_ROW_GROUP) {
        ::tokio::task::yield_now().await;
        if let Err(error) = write_row_group(&mut writer, columns, chunk) {
            ::tracing::warn!(%error, "the Parquet writer refused this commit's row group");
            return None;
        }
    }
    match writer.into_inner() {
        Ok(body) => Some(body),
        Err(error) => {
            ::tracing::warn!(%error, "the Parquet writer refused this commit's data object");
            None
        }
    }
}

/// The Parquet message type the committed data object carries: one OPTIONAL leaf per column,
/// carrying the Iceberg field id of that column — the id the table's metadata document numbers
/// it with, which is how an Iceberg reader binds a column of the file to a field of the schema.
fn message_type(table: &str, columns: &[(String, FieldType)]) -> Option<SchemaType> {
    let mut fields = Vec::with_capacity(columns.len());
    for (index, (name, field)) in columns.iter().enumerate() {
        let (physical, converted) = leaf_of(*field);
        let leaf = SchemaType::primitive_type_builder(name, physical)
            .with_repetition(Repetition::OPTIONAL)
            .with_converted_type(converted)
            .with_id(Some(i32::try_from(index + 1).ok()?))
            .build()
            .ok()?;
        fields.push(::std::sync::Arc::new(leaf));
    }
    SchemaType::group_type_builder(if table.is_empty() { "table" } else { table })
        .with_fields(fields)
        .build()
        .ok()
}

/// The Parquet leaf one Iceberg field type is written as: its physical type and the annotation
/// that says what those bytes mean.
fn leaf_of(field: FieldType) -> (PhysicalType, ConvertedType) {
    match field {
        FieldType::Boolean => (PhysicalType::BOOLEAN, ConvertedType::NONE),
        FieldType::Int => (PhysicalType::INT32, ConvertedType::INT_32),
        FieldType::Long => (PhysicalType::INT64, ConvertedType::INT_64),
        FieldType::Float => (PhysicalType::FLOAT, ConvertedType::NONE),
        FieldType::Double => (PhysicalType::DOUBLE, ConvertedType::NONE),
        FieldType::String => (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
        FieldType::Binary => (PhysicalType::BYTE_ARRAY, ConvertedType::NONE),
        FieldType::Date => (PhysicalType::INT32, ConvertedType::DATE),
        FieldType::Timestamp => (PhysicalType::INT64, ConvertedType::TIMESTAMP_MICROS),
    }
}

/// Write `rows` as ONE row group of the data object: each column's values as one batch, with
/// the definition level that carries the rows the column holds no value for.
fn write_row_group(
    writer: &mut SerializedFileWriter<Vec<u8>>,
    columns: &[(String, FieldType)],
    rows: &[Vec<Cell>],
) -> Result<(), String> {
    let mut row_group = writer.next_row_group().map_err(|error| error.to_string())?;
    for (index, (name, field)) in columns.iter().enumerate() {
        let mut column = row_group
            .next_column()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("the Parquet schema lost the column {name}"))?;
        let cells: Vec<&Cell> = rows
            .iter()
            .map(|row| row.get(index).unwrap_or(&Cell::Null))
            .collect();
        // One definition level per row: 1 where the column holds a value and 0 where it holds
        // none, which is how an OPTIONAL leaf carries a null.
        let definitions: Vec<i16> = cells
            .iter()
            .map(|cell| i16::from(**cell != Cell::Null))
            .collect();
        let levels = Some(definitions.as_slice());
        match field {
            FieldType::Boolean => {
                let present: Vec<bool> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Boolean(flag) => Some(*flag),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<BoolType>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
            FieldType::Int | FieldType::Date => {
                let present: Vec<i32> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Int32(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<Int32Type>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
            FieldType::Long | FieldType::Timestamp => {
                let present: Vec<i64> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Int64(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<Int64Type>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
            FieldType::Float => {
                let present: Vec<f32> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Float(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<FloatType>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
            FieldType::Double => {
                let present: Vec<f64> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Double(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<DoubleType>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
            FieldType::String | FieldType::Binary => {
                let present: Vec<ByteArray> = cells
                    .iter()
                    .filter_map(|cell| match cell {
                        Cell::Text(text) => Some(ByteArray::from(text.clone().into_bytes())),
                        Cell::Bytes(bytes) => Some(ByteArray::from(bytes.clone())),
                        _ => None,
                    })
                    .collect();
                column
                    .typed::<ByteArrayType>()
                    .write_batch(&present, levels, None)
                    .map_err(|error| error.to_string())?;
            }
        }
        column.close().map_err(|error| error.to_string())?;
    }
    row_group.close().map_err(|error| error.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The commit's Avro objects: the manifest and the manifest list
// ---------------------------------------------------------------------------

/// The Avro schema of a MANIFEST: one entry per data file a commit added, as Iceberg's
/// manifest-entry schema declares it (the entry's status and the snapshot and sequence number
/// it was added by, plus the data file itself).
const MANIFEST_ENTRY_SCHEMA: &str = r#"{
  "type": "record",
  "name": "manifest_entry",
  "fields": [
    { "name": "status", "type": "int", "field-id": 0 },
    { "name": "snapshot_id", "type": ["null", "long"], "default": null, "field-id": 1 },
    { "name": "sequence_number", "type": ["null", "long"], "default": null, "field-id": 3 },
    { "name": "file_sequence_number", "type": ["null", "long"], "default": null, "field-id": 4 },
    { "name": "data_file", "field-id": 2, "type": {
        "type": "record",
        "name": "r2",
        "fields": [
          { "name": "content", "type": "int", "field-id": 134 },
          { "name": "file_path", "type": "string", "field-id": 100 },
          { "name": "file_format", "type": "string", "field-id": 101 },
          { "name": "partition", "field-id": 102, "type": {
              "type": "record", "name": "r102", "fields": []
          } },
          { "name": "record_count", "type": "long", "field-id": 103 },
          { "name": "file_size_in_bytes", "type": "long", "field-id": 104 }
        ]
    } }
  ]
}"#;

/// The Avro schema of a MANIFEST LIST: one entry per manifest a snapshot holds, as Iceberg's
/// manifest-file schema declares it.
const MANIFEST_FILE_SCHEMA: &str = r#"{
  "type": "record",
  "name": "manifest_file",
  "fields": [
    { "name": "manifest_path", "type": "string", "field-id": 500 },
    { "name": "manifest_length", "type": "long", "field-id": 501 },
    { "name": "partition_spec_id", "type": "int", "field-id": 502 },
    { "name": "content", "type": "int", "field-id": 517 },
    { "name": "sequence_number", "type": "long", "field-id": 515 },
    { "name": "min_sequence_number", "type": "long", "field-id": 516 },
    { "name": "added_snapshot_id", "type": "long", "field-id": 503 },
    { "name": "added_files_count", "type": "int", "field-id": 504 },
    { "name": "existing_files_count", "type": "int", "field-id": 505 },
    { "name": "deleted_files_count", "type": "int", "field-id": 506 },
    { "name": "added_rows_count", "type": "long", "field-id": 512 },
    { "name": "existing_rows_count", "type": "long", "field-id": 513 },
    { "name": "deleted_rows_count", "type": "long", "field-id": 514 }
  ]
}"#;

/// The MANIFEST object of a commit: the one data file this delivery added, recorded as the
/// entry a reader of the snapshot resolves it through.
fn manifest_object(
    data_path: &str,
    data_size: i64,
    records: i64,
    snapshot_id: i64,
    sequence_number: i64,
) -> Option<Vec<u8>> {
    use ::apache_avro::types::Value;

    let schema = parse_schema(MANIFEST_ENTRY_SCHEMA)?;
    let mut writer = ::apache_avro::Writer::new(&schema, Vec::new());
    let data_file = Value::Record(vec![
        ("content".to_string(), Value::Int(CONTENT_DATA)),
        ("file_path".to_string(), Value::String(data_path.to_string())),
        (
            "file_format".to_string(),
            Value::String(DATA_FILE_FORMAT.to_string()),
        ),
        ("partition".to_string(), Value::Record(Vec::new())),
        ("record_count".to_string(), Value::Long(records)),
        ("file_size_in_bytes".to_string(), Value::Long(data_size)),
    ]);
    let entry = Value::Record(vec![
        ("status".to_string(), Value::Int(STATUS_ADDED)),
        (
            "snapshot_id".to_string(),
            Value::Union(1, Box::new(Value::Long(snapshot_id))),
        ),
        (
            "sequence_number".to_string(),
            Value::Union(1, Box::new(Value::Long(sequence_number))),
        ),
        (
            "file_sequence_number".to_string(),
            Value::Union(1, Box::new(Value::Long(sequence_number))),
        ),
        ("data_file".to_string(), data_file),
    ]);
    if let Err(error) = writer.append(entry) {
        ::tracing::warn!(%error, "the Avro writer refused this commit's manifest");
        return None;
    }
    writer
        .into_inner()
        .inspect_err(|error| {
            ::tracing::warn!(%error, "the Avro writer refused this commit's manifest");
        })
        .ok()
}

/// One entry of a manifest list: a manifest the snapshot holds, and what that manifest added.
struct ManifestFile {
    path: String,
    length: i64,
    records: i64,
    snapshot_id: i64,
    sequence_number: i64,
}

/// The MANIFEST LIST object of a commit: every manifest the table already held (`carried`,
/// verbatim as the current snapshot's list recorded them) plus the one this delivery wrote.
///
/// An Iceberg snapshot's manifest list is the whole of the table's state, not the delta of one
/// commit, so an entry is carried forward exactly as it was written: its own counts and
/// sequence numbers describe the manifest it names and are not this commit's to restate.
fn manifest_list_object(
    carried: &[::apache_avro::types::Value],
    added: &ManifestFile,
) -> Option<Vec<u8>> {
    use ::apache_avro::types::Value;

    let schema = parse_schema(MANIFEST_FILE_SCHEMA)?;
    let mut writer = ::apache_avro::Writer::new(&schema, Vec::new());
    let entry = Value::Record(vec![
        (
            "manifest_path".to_string(),
            Value::String(added.path.clone()),
        ),
        ("manifest_length".to_string(), Value::Long(added.length)),
        ("partition_spec_id".to_string(), Value::Int(0)),
        ("content".to_string(), Value::Int(CONTENT_DATA)),
        (
            "sequence_number".to_string(),
            Value::Long(added.sequence_number),
        ),
        (
            "min_sequence_number".to_string(),
            Value::Long(added.sequence_number),
        ),
        (
            "added_snapshot_id".to_string(),
            Value::Long(added.snapshot_id),
        ),
        ("added_files_count".to_string(), Value::Int(1)),
        ("existing_files_count".to_string(), Value::Int(0)),
        ("deleted_files_count".to_string(), Value::Int(0)),
        ("added_rows_count".to_string(), Value::Long(added.records)),
        ("existing_rows_count".to_string(), Value::Long(0)),
        ("deleted_rows_count".to_string(), Value::Long(0)),
    ]);
    for held in carried.iter().cloned().chain([entry]) {
        if let Err(error) = writer.append(held) {
            ::tracing::warn!(%error, "the Avro writer refused this commit's manifest list");
            return None;
        }
    }
    writer
        .into_inner()
        .inspect_err(|error| {
            ::tracing::warn!(%error, "the Avro writer refused this commit's manifest list");
        })
        .ok()
}

/// How many entries one manifest list this emulator reads back may hold.
///
/// A CHOSEN bound: the list is read out of the object store, where a caller can write one, so
/// this is what keeps a commit from being handed an unbounded number of entries to carry
/// forward. Far above the manifests a table reached by one Firehose stream accumulates, since
/// each one is one delivery.
const MAX_CARRIED_MANIFESTS: usize = 100_000;

/// The entries of the manifest list at `manifest_list`, read back out of the object store with
/// the Avro implementation, or `None` when the store holds no such object or its bytes are not
/// a manifest list this reader can read.
fn manifest_entries<Eng: Send + Sync + 'static>(
    engine: &Eng,
    manifest_list: &str,
) -> Option<Vec<::apache_avro::types::Value>> {
    let (bucket, key) =
        ::service_com_amazonaws_glue_awsglue::state::split_s3_location(manifest_list)?;
    let body = {
        let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
        let store = state.lock();
        store.bucket(&bucket)?.object(&key)?.body.clone()
    };
    let reader = ::apache_avro::Reader::new(&body[..])
        .inspect_err(|error| {
            ::tracing::warn!(%error, %manifest_list, "the table's manifest list did not read");
        })
        .ok()?;
    let mut entries = Vec::new();
    for held in reader {
        if entries.len() >= MAX_CARRIED_MANIFESTS {
            ::tracing::warn!(
                %manifest_list,
                entries = MAX_CARRIED_MANIFESTS,
                "the table's manifest list holds more entries than one commit carries here"
            );
            return None;
        }
        match held {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                ::tracing::warn!(%error, %manifest_list, "an entry of the table's manifest list did not read");
                return None;
            }
        }
    }
    Some(entries)
}

/// One of the two Avro schemas above, parsed by the Avro implementation itself.
fn parse_schema(document: &str) -> Option<::apache_avro::Schema> {
    ::apache_avro::Schema::parse_str(document)
        .inspect_err(|error| {
            ::tracing::warn!(%error, "an Iceberg manifest schema did not parse");
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Iceberg `timestamp` column takes the ISO-8601 spellings and epoch milliseconds, and
    /// nothing else — the space-separated SQL spelling included.
    #[test]
    fn a_timestamp_column_takes_only_the_iso_spellings() {
        // (the value a record carries, whether the column holds it)
        let cases: &[(::serde_json::Value, bool)] = &[
            (::serde_json::json!("2026-01-01T00:00:00Z"), true),
            (::serde_json::json!("2026-01-01T00:00:00+02:00"), true),
            (::serde_json::json!("2026-01-01T00:00:00"), true),
            (::serde_json::json!("2026-01-01T00:00:00.123456"), true),
            (::serde_json::json!(1_767_225_600_000_i64), true),
            (::serde_json::json!("2026-01-01 00:00:00"), false),
            (::serde_json::json!("2026-01-01"), false),
            (::serde_json::json!("not a timestamp"), false),
            (::serde_json::json!(true), false),
        ];
        for (value, held) in cases {
            let read = cell_of("ts", FieldType::Timestamp, value);
            assert_eq!(
                read.is_ok(),
                *held,
                "case {value}: expected held={held}, got {read:?}"
            );
        }
    }

    /// The two Avro objects of a commit are written by the Avro implementation, so a schema
    /// it would refuse is caught here rather than at a delivery — and the manifest list a
    /// commit writes carries the entries the table already held beside its own, which is read
    /// back through the same implementation.
    #[test]
    fn a_commit_writes_both_of_its_avro_objects() {
        let manifest = manifest_object("s3://lake/t/data/a.parquet", 10, 3, 7, 1)
            .expect("case the manifest: the Avro writer must take the entry");
        assert!(
            manifest.starts_with(b"Obj"),
            "case the manifest: an Avro container opens with its magic, got {:?}",
            manifest.get(..4)
        );
        let added = |path: &str, sequence_number: i64| ManifestFile {
            path: path.to_string(),
            length: 10,
            records: 3,
            snapshot_id: 7,
            sequence_number,
        };
        let first = manifest_list_object(&[], &added("s3://lake/t/metadata/a-m0.avro", 1))
            .expect("case the first list: the Avro writer must take the entry");
        assert!(
            first.starts_with(b"Obj"),
            "case the first list: an Avro container opens with its magic, got {:?}",
            first.get(..4)
        );
        // The entries of that list, read back the way a commit onto this table reads them.
        let held: Vec<::apache_avro::types::Value> = ::apache_avro::Reader::new(&first[..])
            .expect("case the first list: it must read back")
            .map(|entry| entry.expect("case the first list: its entry must read back"))
            .collect();
        assert_eq!(
            held.len(),
            1,
            "case the first list: expected one entry, got {held:?}"
        );
        let second = manifest_list_object(&held, &added("s3://lake/t/metadata/b-m0.avro", 2))
            .expect("case the second list: the Avro writer must take both entries");
        let paths: Vec<String> = ::apache_avro::Reader::new(&second[..])
            .expect("case the second list: it must read back")
            .filter_map(|entry| match entry {
                Ok(::apache_avro::types::Value::Record(members)) => members
                    .into_iter()
                    .find(|(member, _)| member == "manifest_path")
                    .and_then(|(_, value)| match value {
                        ::apache_avro::types::Value::String(path) => Some(path),
                        _ => None,
                    }),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                "s3://lake/t/metadata/a-m0.avro".to_string(),
                "s3://lake/t/metadata/b-m0.avro".to_string()
            ],
            "case the second list: the new list names the manifest the table held and the one \
             this commit wrote, got {paths:?}"
        );
    }

    /// The metadata file a commit writes is numbered after the current one, whatever the
    /// current pointer looks like.
    #[test]
    fn a_commit_numbers_its_metadata_file_after_the_current_one() {
        let cases: &[(&str, u64)] = &[
            ("s3://lake/t/metadata/00000-abc.metadata.json", 1),
            ("s3://lake/t/metadata/00007-abc.metadata.json", 8),
            ("s3://lake/t/metadata/v1.metadata.json", 1),
        ];
        for (current, expected) in cases {
            let next = next_metadata_number(current);
            assert_eq!(
                next, *expected,
                "case {current}: expected {expected}, got {next}"
            );
        }
    }
}
