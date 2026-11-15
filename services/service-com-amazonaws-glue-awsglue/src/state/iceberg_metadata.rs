// SPDX-License-Identifier: CC0-1.0

//! The Iceberg TABLE METADATA document an open-table-format table is read through.
//!
//! A `CreateTable` carrying an `OpenTableFormatInput$IcebergInput` records three parameters on
//! the stored table ([`super::IcebergTableMarker`]), one of which — `metadata_location` —
//! names an object in Amazon S3. That object is the table: an Iceberg reader resolves the
//! pointer and reads the table's format version, its identity, its schema and its snapshot
//! history out of the document it holds. This module is that document — minted here so the
//! catalog row and the object it points at are written from ONE description of the table —
//! and the Hive-to-Iceberg type reading the schema inside it is built from.
//!
//! What a freshly created table's document states, as the Iceberg table specification lays a
//! metadata file out: the `format-version` the create asked for, a `table-uuid` identifying
//! this table (a new one per create, so a delete followed by a re-create of the same name is a
//! NEW table), the table's own `location`, one schema whose fields are numbered from 1 in the
//! order the columns are declared, and an EMPTY snapshot list — a table nothing has committed
//! to yet holds no snapshot.
//!
//! The catalog keeps the GLUE type names on `StorageDescriptor$Columns`: the Iceberg spelling
//! lives only in this document, which is what a reader of the metadata file sees.

use std::collections::HashMap;
use std::time::SystemTime;

use super::{base_hive_type, mint_hex_id};

/// How many 64-bit words a `table-uuid` carries: two, i.e. the 128 bits a UUID is.
const TABLE_UUID_WORDS: usize = 2;

/// How many `table-uuid` values this process has minted. A nonce source for
/// [`new_table_uuid`], exactly as the counters beside it are for the other ids this crate
/// mints.
static MINTED_TABLE_UUIDS: ::std::sync::atomic::AtomicU64 = ::std::sync::atomic::AtomicU64::new(0);

/// The schema id of the one schema a created table declares, and the spec and sort-order ids
/// of its unpartitioned, unsorted layout: Iceberg numbers each of those from zero.
const FIRST_SCHEMA_ID: i64 = 0;

/// The partition field id Iceberg starts an unpartitioned table's field numbering below:
/// "the last assigned partition field ID", which for a table with no partition field at all is
/// the id one below the range Iceberg assigns partition fields from (1000).
const LAST_PARTITION_ID: i64 = 999;

/// `current-snapshot-id` of a table that has committed nothing: Iceberg's own "no current
/// snapshot" value.
const NO_CURRENT_SNAPSHOT: i64 = -1;

/// A fresh `table-uuid`: the identity of ONE Iceberg table, minted when the table is created.
///
/// 128 bits spelled the way a UUID is, so two tables created in the same nanosecond — and a
/// table re-created under a name a deleted one held — cannot be given the same identity.
///
/// This names a RESOURCE, not a credential: a credential would come from the credential store,
/// never from here.
#[must_use]
pub fn new_table_uuid() -> String {
    uuid_spelling(&mint_hex_id(&MINTED_TABLE_UUIDS, TABLE_UUID_WORDS))
}

/// 32 hexadecimal characters grouped 8-4-4-4-12, the way a UUID is spelled.
///
/// The ONE spelling of a UUID in this crate: the `table-uuid` above and the id of an Iceberg
/// metadata file ([`super::new_iceberg_metadata_location`]) are both minted through it.
///
/// Built by walking the characters rather than by slicing, so the spelling cannot depend on
/// the id's width.
pub(super) fn uuid_spelling(digits: &str) -> String {
    let mut id = String::with_capacity(digits.len() + 4);
    for (position, character) in digits.chars().enumerate() {
        if matches!(position, 8 | 12 | 16 | 20) {
            id.push('-');
        }
        id.push(character);
    }
    id
}

/// The Iceberg type a Glue column's declared type is written as inside the metadata document.
///
/// Iceberg and Hive take their primitive names from the same vocabulary but do not spell every
/// one of them the same way: Hive's `bigint` is Iceberg's `long`, Hive's `tinyint`,
/// `smallint`, `int` and `integer` are all Iceberg's 32-bit `int`, and Hive's `char(n)` and
/// `varchar(n)` are Iceberg's `string`. The names the two DO share (`string`, `float`,
/// `double`, `boolean`, `date`, `timestamp`, `binary`, `decimal(p, s)`) are carried through,
/// which is also what a declaration this reading has no mapping for is: the metadata document
/// then states the type the catalog was given rather than one nothing produced.
///
/// The parameter list of a parameterised declaration is kept, because `decimal(10,2)` names a
/// different Iceberg type from `decimal(38,0)`.
#[must_use]
pub fn iceberg_field_type(declared: &str) -> String {
    match base_hive_type(declared).as_str() {
        "bigint" => "long".to_string(),
        "tinyint" | "smallint" | "int" | "integer" => "int".to_string(),
        "char" | "varchar" => "string".to_string(),
        // Every other declaration keeps its own spelling, lowercased and with the parameter
        // list it carries: `decimal(10,2)` is spelled that way in both vocabularies.
        _ => declared.trim().to_ascii_lowercase(),
    }
}

/// The one SCHEMA a created Iceberg table declares: a struct whose fields are the table's
/// columns, numbered from 1 in the order the columns are declared.
///
/// `columns` is the table's column information as `(name, declared Hive type)` in table order,
/// which is what every reader of a stored definition's columns already holds.
///
/// Every field is `required: false`: a Glue column holds no value wherever a record named
/// none, so the Iceberg field the column is written as is optional, and a create that declares
/// no required-ness per column cannot report one.
#[must_use]
pub fn table_schema(columns: &[(String, String)]) -> ::serde_json::Value {
    let fields: Vec<::serde_json::Value> = columns
        .iter()
        .enumerate()
        .map(|(index, (name, declared))| {
            ::serde_json::json!({
                "id": index + 1,
                "name": name,
                "required": false,
                "type": iceberg_field_type(declared),
            })
        })
        .collect();
    ::serde_json::json!({
        "type": "struct",
        "schema-id": FIRST_SCHEMA_ID,
        "fields": fields,
    })
}

/// The METADATA DOCUMENT of a freshly created Iceberg table: the table at the instant its
/// create minted it, before anything has been committed to it.
///
/// `location` is the table's own location, `format_version` the version the create asked for
/// (`IcebergInput$Version`, which defaults to 2) and `columns` the columns the stored
/// definition declares. `uuid` is the table's identity ([`new_table_uuid`]), passed in so the
/// caller can record the same value elsewhere.
#[must_use]
pub fn initial_table_metadata(
    location: &str,
    format_version: &str,
    uuid: &str,
    columns: &[(String, String)],
    created_at: SystemTime,
) -> ::serde_json::Value {
    let schema = table_schema(columns);
    let partition_spec = ::serde_json::json!({ "spec-id": FIRST_SCHEMA_ID, "fields": [] });
    let sort_order = ::serde_json::json!({ "order-id": FIRST_SCHEMA_ID, "fields": [] });
    let mut document = ::serde_json::json!({
        "format-version": format_version_number(format_version),
        "table-uuid": uuid,
        "location": location.trim_end_matches('/'),
        "last-sequence-number": 0,
        "last-updated-ms": epoch_millis(created_at),
        "last-column-id": columns.len(),
        "current-schema-id": FIRST_SCHEMA_ID,
        "schemas": [schema.clone()],
        "default-spec-id": FIRST_SCHEMA_ID,
        "partition-specs": [partition_spec.clone()],
        "last-partition-id": LAST_PARTITION_ID,
        "default-sort-order-id": FIRST_SCHEMA_ID,
        "sort-orders": [sort_order],
        "properties": ::serde_json::Map::new(),
        "current-snapshot-id": NO_CURRENT_SNAPSHOT,
        "refs": ::serde_json::Map::new(),
        // A table nothing has committed to holds no snapshot, and so no history of one
        // either.
        "snapshots": [],
        "snapshot-log": [],
        "metadata-log": [],
    });
    // Version 1 of the format states the current schema and partition spec in two singular
    // fields beside the lists, so a version-1 document carries them as well.
    if format_version_number(format_version) == 1 {
        if let Some(members) = document.as_object_mut() {
            members.insert("schema".to_string(), schema);
            members.insert("partition-spec".to_string(), partition_spec["fields"].clone());
        }
    }
    document
}

/// The columns of a stored table's definition as `(name, declared Hive type)` in table order,
/// which is what the metadata document's schema is built from.
#[must_use]
pub fn definition_columns(descriptor: Option<&super::StorageDescriptor>) -> Vec<(String, String)> {
    descriptor
        .and_then(|descriptor| descriptor.columns.as_ref())
        .into_iter()
        .flatten()
        .map(|column| {
            (
                column.name.clone(),
                column.r#type.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// Whether the parameters of a stored table mark it as an Iceberg table the catalog minted
/// metadata for: the `table_type` marker AND the `metadata_location` the metadata document
/// lives at.
///
/// The parameter-only route — a `table_type` of `ICEBERG` set by a client with no
/// `OpenTableFormatInput` — labels the row and mints no metadata, so it carries no
/// `metadata_location` and nothing reads a document for it.
#[must_use]
pub fn marked_metadata_location(parameters: Option<&HashMap<String, String>>) -> Option<String> {
    let parameters = parameters?;
    let marked = parameters
        .get(super::ICEBERG_TABLE_TYPE_PARAMETER)
        .is_some_and(|value| value.eq_ignore_ascii_case(super::ICEBERG_TABLE_TYPE));
    if !marked {
        return None;
    }
    parameters
        .get(super::ICEBERG_METADATA_LOCATION_PARAMETER)
        .filter(|location| !location.is_empty())
        .cloned()
}

/// Write `document` into Amazon S3 at `metadata_location`, which is where the table's
/// `metadata_location` parameter points.
///
/// The catalog row and the object it points at are written by the SAME create, because a
/// pointer to an object that does not exist is a table no Iceberg reader can open. The write
/// is made through the Amazon S3 client this crate reads objects with
/// ([`super::column_statistics_source::s3_client`]), over the in-process callback into this
/// same emulator, so the object lands in the one object store this emulator holds.
///
/// `Err` carries why the object could not be written — a location that names no bucket, or a
/// bucket the account does not hold — which the caller reports rather than failing the create
/// over: a create whose metadata could not be stored has still stored the catalog row, exactly
/// as a client observing both would see it.
pub async fn write_table_metadata(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    metadata_location: &str,
    document: &::serde_json::Value,
) -> Result<(), String> {
    let Some((bucket, key)) = super::split_s3_location(metadata_location) else {
        return Err(format!(
            "the metadata location {metadata_location} names no Amazon S3 bucket"
        ));
    };
    if key.is_empty() {
        return Err(format!(
            "the metadata location {metadata_location} names no object"
        ));
    }
    let body = ::serde_json::to_vec(document).map_err(|error| error.to_string())?;
    super::column_statistics_source::s3_client(ctx, region)
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_type(ICEBERG_METADATA_CONTENT_TYPE)
        .body(::aws_sdk_s3::primitives::ByteStream::from(body))
        .send()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The media type an Iceberg metadata file is stored with: the document is JSON, which is what
/// a reader handed the object has to parse it as.
const ICEBERG_METADATA_CONTENT_TYPE: &str = "application/json";

/// `IcebergInput$Version` as the number the metadata document states it as, and the documented
/// default for a value that is no version at all.
fn format_version_number(format_version: &str) -> u64 {
    format_version
        .trim()
        .parse()
        .unwrap_or_else(|_| super::DEFAULT_ICEBERG_FORMAT_VERSION.parse().unwrap_or(2))
}

/// `at` as epoch milliseconds, which is how Iceberg states an instant.
fn epoch_millis(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis())
        .unwrap_or_default()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Hive type a Glue column declares, with the Iceberg type the metadata document
    /// writes it as. The mapping is Iceberg's own primitive vocabulary beside Hive's.
    #[test]
    fn a_glue_column_type_maps_to_its_iceberg_type() {
        let cases: &[(&str, &str)] = &[
            ("string", "string"),
            ("STRING", "string"),
            ("char(3)", "string"),
            ("varchar(255)", "string"),
            ("int", "int"),
            ("integer", "int"),
            ("tinyint", "int"),
            ("smallint", "int"),
            ("bigint", "long"),
            ("float", "float"),
            ("double", "double"),
            ("boolean", "boolean"),
            ("date", "date"),
            ("timestamp", "timestamp"),
            ("binary", "binary"),
            ("decimal(10,2)", "decimal(10,2)"),
        ];
        for (declared, expected) in cases {
            let mapped = iceberg_field_type(declared);
            assert_eq!(
                mapped, *expected,
                "case {declared:?}: expected {expected}, got {mapped}"
            );
        }
    }

    /// A created table's document states the version asked for, its own identity, its columns
    /// numbered from 1 and no snapshot at all.
    #[test]
    fn a_created_table_declares_its_columns_and_no_snapshot() {
        let columns = vec![
            ("id".to_string(), "string".to_string()),
            ("count".to_string(), "bigint".to_string()),
        ];
        let document = initial_table_metadata(
            "s3://lake/events/",
            "2",
            "0f0f0f0f-0f0f-0f0f-0f0f-0f0f0f0f0f0f",
            &columns,
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(
            document["format-version"], 2,
            "case the default version: got {document}"
        );
        assert_eq!(
            document["location"], "s3://lake/events",
            "case the table location: got {document}"
        );
        assert_eq!(
            document["snapshots"],
            ::serde_json::json!([]),
            "case a table nothing committed to: got {document}"
        );
        let fields = &document["schemas"][0]["fields"];
        assert_eq!(
            fields[0],
            ::serde_json::json!({ "id": 1, "name": "id", "required": false, "type": "string" }),
            "case the first field: got {document}"
        );
        assert_eq!(
            fields[1],
            ::serde_json::json!({ "id": 2, "name": "count", "required": false, "type": "long" }),
            "case the second field: got {document}"
        );
    }
}
