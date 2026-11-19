// SPDX-License-Identifier: CC0-1.0

//! What a domain export RUNS: the objects it writes into the bucket its start named.
//!
//! `StartDomainExport` "initiates the export of a SimpleDB domain to an S3 bucket"
//! (`smithy_query(query="operation",operation="StartDomainExport")`), so the export IS the
//! write: the items the exported domain holds are serialized into data objects under the
//! `s3KeyPrefix` the request named, beside the manifest summary the finished export reports
//! as its `exportManifest` ("the name of the manifest summary file for the export"). A
//! client that lists that prefix finds them, because they are objects this emulator's own
//! Amazon S3 holds — written through the in-process transport every emulated service
//! reaches another over ([`::service_common::s3_client`]), which opens no socket and
//! reaches nothing outside this process.
//!
//! The run is performed by the start itself, with the serialized items read off the domain
//! while the store is locked and the write made after the guard is dropped. What the run
//! leaves behind is a fact about the object store, so the status the clock then owes the
//! export is read off it: an export whose data was written reaches `SUCCEEDED`, and one the
//! object store refused reaches `FAILED` carrying that refusal
//! ([`super::StoredExport::settle`]).
//!
//! # The serialized form
//!
//! The repo carries no model for the export's file format, so the form here is the API's
//! OWN vocabulary and nothing invented beside it: one document carrying the exported items,
//! each naming the item and carrying one entry per attribute NAME with the values stored
//! under it — an attribute is multi-valued, so a name holds a set of values
//! ([`super::simpledb_domains`]).

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use smithy_server_runtime::OperationContext;

use super::simpledb_domains::{Domain, ItemAttributes};
use super::StoredExport;

/// The media type the export's objects are stored under: each is a JSON document.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The directory, inside the export's own prefix, its data files are written under — so a
/// listing of the prefix tells the export's data apart from its manifest.
const DATA_DIRECTORY: &str = "data";

/// The name of the one data file an export of this emulator writes: every item of the
/// domain, in one object.
const DATA_FILE: &str = "items.json";

/// The version the manifest states for the form its data file is written in: the version
/// of the Amazon SimpleDB API whose items that file carries.
const EXPORT_FORMAT_VERSION: &str = crate::service::sdb_query_api::API_VERSION;

/// The `ServerSideEncryption` an `s3SseAlgorithm` of `KMS` asks Amazon S3 for, as S3's own
/// member spells it (the modelled values are `AES256` and `KMS`; S3's are `AES256` and
/// `aws:kms`).
const S3_SSE_KMS: &str = "aws:kms";

/// The modelled `s3SseAlgorithm` value that names AWS KMS.
const SSE_ALGORITHM_KMS: &str = "KMS";

/// One object an export writes: the key it is written to, and the document it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportObject {
    pub key: String,
    pub body: String,
}

/// Why an export could not write its data: the Amazon S3 answer the write was refused
/// with, as the export's own `failureCode` and `failureMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFailure {
    pub code: String,
    pub message: String,
}

impl ExportFailure {
    /// The failure for a bucket the object store answered that it does not hold.
    fn no_such_bucket(bucket: &str) -> Self {
        Self {
            code: "NoSuchBucket".to_string(),
            message: format!(
                "The export could not be written: the specified bucket {bucket} does not \
                 exist."
            ),
        }
    }

    /// The failure for a write the object store REFUSED, or `None` when the refusal was
    /// not an answer about the write at all.
    ///
    /// Which is which is the one reading of an object store's answer this tree has
    /// ([`::service_common::s3_object_read::answered_refusal`]): the store saying it holds
    /// no such bucket, and the store saying the write is not permitted, are each a fact
    /// about the address the export was pointed at, so the export failed on it. Anything
    /// else — including a failure carrying no code, which is an emulator assembled with no
    /// Amazon S3 to write into — establishes nothing about that address, so no failure is
    /// recorded against the caller's export and the gap is warned about instead.
    fn refusing<E: ProvideErrorMetadata + ::std::fmt::Debug>(
        error: &E,
        bucket: &str,
        key: &str,
    ) -> Option<Self> {
        use ::service_common::s3_object_read::ObjectRead;

        let message = |answer: &str| format!("The export could not be written: {answer}");
        match ::service_common::s3_object_read::answered_refusal(error) {
            ObjectRead::Absent => Some(Self::no_such_bucket(bucket)),
            ObjectRead::Denied => Some(Self {
                code: "AccessDenied".to_string(),
                message: message(&format!(
                    "access to the object {key} of bucket {bucket} is denied."
                )),
            }),
            ObjectRead::Served(_) | ObjectRead::Unreadable | ObjectRead::Unanswered => {
                ::tracing::warn!(
                    bucket = %bucket,
                    key = %key,
                    error = ?error,
                    "this emulator's Amazon S3 answered an export's write with nothing \
                     about the address it named, so no object was written"
                );
                None
            }
        }
    }
}

/// Where one export writes, as its own record states it: the bucket, the account that must
/// own it, and the server-side encryption its objects are stored under.
#[derive(Debug, Clone)]
pub struct ExportDestination {
    bucket: String,
    bucket_owner: Option<String>,
    sse_algorithm: Option<String>,
    sse_kms_key_id: Option<String>,
}

impl ExportDestination {
    /// The destination `export` was started for.
    pub fn of(export: &StoredExport) -> Self {
        Self {
            bucket: export.s3_bucket.clone(),
            bucket_owner: export.s3_bucket_owner.clone(),
            sse_algorithm: export.s3_sse_algorithm.clone(),
            sse_kms_key_id: export.s3_sse_kms_key_id.clone(),
        }
    }
}

/// The objects the export `export` writes for `domain`: its data file, and the manifest
/// summary the finished export reports.
///
/// Both keys sit under the export's own `s3KeyPrefix`/`exportId` path, so two exports of
/// one domain into one prefix never overwrite each other, and the manifest key is the ONE
/// the record reports ([`StoredExport::manifest_key`]) rather than a second rendering of
/// it.
pub fn export_objects(export: &StoredExport, domain: &Domain) -> Vec<ExportObject> {
    let items: Vec<::serde_json::Value> = domain
        .items()
        .map(|(name, attributes)| item_document(name, attributes))
        .collect();
    let data_key = format!(
        "{prefix}{id}/{DATA_DIRECTORY}/{DATA_FILE}",
        prefix = export.s3_key_prefix.as_deref().unwrap_or(""),
        id = export.export_id,
    );
    // ONE document carrying the exported items in order, so a reader parses the data file
    // whole and walks the items it holds; a domain holding none is an empty list rather
    // than no file, because the export of an empty domain is still an export that ran.
    let data = ::serde_json::Value::Array(items.clone()).to_string();
    let manifest = ::serde_json::json!({
        "version": EXPORT_FORMAT_VERSION,
        "exportArn": export.export_arn,
        "exportId": export.export_id,
        "domainName": export.domain_name,
        "s3Bucket": export.s3_bucket,
        "s3KeyPrefix": export.s3_key_prefix,
        "itemCount": items.len(),
        "dataFileS3Keys": [data_key.clone()],
    });
    vec![
        ExportObject {
            key: data_key,
            body: data,
        },
        ExportObject {
            key: export.manifest_key(),
            body: manifest.to_string(),
        },
    ]
}

/// One item of the export's data: the item's own name, and one entry per attribute NAME
/// carrying the values stored under it.
///
/// An attribute is multi-valued — one name holds a set of values
/// ([`super::simpledb_domains`]) — so the values of one name travel together under that
/// name rather than as repeated single-valued entries.
fn item_document(item_name: &str, attributes: &ItemAttributes) -> ::serde_json::Value {
    let attributes: Vec<::serde_json::Value> = attributes
        .iter()
        .map(|(name, values)| {
            ::serde_json::json!({
                "name": name,
                "values": values.iter().collect::<Vec<&String>>(),
            })
        })
        .collect();
    ::serde_json::json!({ "itemName": item_name, "attributes": attributes })
}

/// Write `objects` into `destination` through this emulator's own Amazon S3, answering the
/// failure the export carries when the store refused the write and `None` when the data is
/// in the bucket.
///
/// The bucket is RESOLVED first, with the operation Amazon S3 documents for exactly that
/// question ([`::service_common::s3_client::bucket_is_absent`]): an export pointed at a
/// bucket the store does not hold has nowhere to write, and reporting its data written
/// would leave a caller believing in objects nothing ever wrote.
///
/// Call it with NO state guard held — it awaits.
pub async fn run_export(
    ctx: &OperationContext<'_>,
    region: &str,
    destination: &ExportDestination,
    objects: &[ExportObject],
) -> Option<ExportFailure> {
    if ::service_common::s3_client::bucket_is_absent(ctx, region, &destination.bucket).await {
        return Some(ExportFailure::no_such_bucket(&destination.bucket));
    }
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    for object in objects {
        let mut write = client
            .put_object()
            .bucket(&destination.bucket)
            .key(&object.key)
            .content_type(JSON_MEDIA_TYPE)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                object.body.clone().into_bytes(),
            ));
        // Every one of these is a parameter of the export's OWN record, so the objects are
        // stored under the encryption the start asked for, in the bucket the account the
        // start named owns — and a store that refuses any of that refuses the write, which
        // is the export's failure rather than a silent change of what it was asked for.
        if let Some(owner) = &destination.bucket_owner {
            write = write.expected_bucket_owner(owner);
        }
        if let Some(algorithm) = &destination.sse_algorithm {
            write = write.server_side_encryption(::aws_sdk_s3::types::ServerSideEncryption::from(
                s3_sse_algorithm(algorithm),
            ));
        }
        if let Some(key_id) = &destination.sse_kms_key_id {
            write = write.ssekms_key_id(key_id);
        }
        if let Err(error) = write.send().await {
            return ExportFailure::refusing(&error, &destination.bucket, &object.key);
        }
    }
    ::tracing::debug!(
        bucket = %destination.bucket,
        objects = objects.len(),
        "a domain export wrote its objects into the bucket it was started for"
    );
    None
}

/// The `ServerSideEncryption` Amazon S3 names for a modelled `s3SseAlgorithm` value.
fn s3_sse_algorithm(modelled: &str) -> &str {
    match modelled {
        SSE_ALGORITHM_KMS => S3_SSE_KMS,
        stated => stated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A record as `StartDomainExport` stores one, for the prefix `prefix`.
    fn stored(prefix: Option<&str>) -> StoredExport {
        let now = SystemTime::now();
        StoredExport {
            export_arn: "arn:aws:sdb:us-west-2:000000000000:domain/orders/export/e-1".to_string(),
            export_id: "e-1".to_string(),
            client_token: "token".to_string(),
            domain_name: "orders".to_string(),
            export_status: super::super::STATUS_PENDING.to_string(),
            requested_at: now,
            s3_bucket: "exports-bucket".to_string(),
            s3_key_prefix: prefix.map(str::to_string),
            s3_sse_algorithm: None,
            s3_sse_kms_key_id: None,
            s3_bucket_owner: None,
            export_manifest: None,
            items_count: None,
            export_data_cutoff_time: None,
            failure_code: None,
            failure_message: None,
            in_progress_due_at: now + Duration::from_millis(1),
            succeeded_due_at: now + Duration::from_millis(2),
        }
    }

    /// A domain holding `items`, each with its own pairs, written the way `PutAttributes`
    /// writes them.
    fn domain_holding(items: &[(&str, &[(&str, &str)])]) -> super::super::ComAmazonawsSimpledbv2Simpledbv2State {
        use super::super::simpledb_domains::AttributeWrite;

        let mut store = super::super::ComAmazonawsSimpledbv2Simpledbv2State::default();
        store.create_domain("orders");
        for (item, pairs) in items {
            let writes: Vec<AttributeWrite> = pairs
                .iter()
                .map(|(name, value)| AttributeWrite {
                    name: (*name).to_string(),
                    value: (*value).to_string(),
                    replace: false,
                })
                .collect();
            store
                .put_item_attributes("orders", item, &writes, SystemTime::now())
                .expect("the seeded write is inside every bound");
        }
        store
    }

    /// One row of the table below: the case, the `s3KeyPrefix` the export names, and the
    /// items the exported domain holds, each with its own attribute pairs.
    type ObjectsRow<'a> = (
        &'a str,
        Option<&'a str>,
        &'a [(&'a str, &'a [(&'a str, &'a str)])],
    );

    /// RULE: the objects an export writes are its data file and its manifest, keyed under
    /// the prefix the start named and the export's own id, and the data file carries one
    /// record per item of the domain with that item's own attributes.
    #[test]
    fn the_written_objects_are_the_domains_items_and_the_manifest() {
        let rows: &[ObjectsRow<'_>] = &[
            (
                "one item with one pair, under a prefix",
                Some("exports/"),
                &[("item-widget", &[("color", "blue")])],
            ),
            (
                "two items, one of them multi-valued, under no prefix at all",
                None,
                &[
                    ("item-a", &[("colour", "blue"), ("colour", "green")]),
                    ("item-b", &[("size", "large")]),
                ],
            ),
            ("a domain holding no items", Some("nested/prefix/"), &[]),
        ];
        for (case, prefix, items) in rows {
            let export = stored(*prefix);
            let store = domain_holding(items);
            let domain = store.domain("orders").expect("the seeded domain");
            let objects = export_objects(&export, domain);
            let keyed = |key: &str| objects.iter().find(|object| object.key == key).cloned();
            let prefix = prefix.unwrap_or("");
            let data_key = format!("{prefix}e-1/{DATA_DIRECTORY}/{DATA_FILE}");
            let data = keyed(&data_key).unwrap_or_else(|| {
                panic!("case {case:?}: the data object {data_key:?} must be written, got {objects:?}")
            });
            let lines: Vec<::serde_json::Value> = ::serde_json::from_str(&data.body)
                .unwrap_or_else(|error| panic!("case {case:?}: {:?}: {error}", data.body));
            assert_eq!(
                lines.len(),
                items.len(),
                "case {case:?}: one record per item of the domain, got {:?}",
                data.body
            );
            for (item, pairs) in *items {
                let document = lines
                    .iter()
                    .find(|document| document["itemName"] == *item)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the item {item:?} must be exported, got {lines:?}")
                    });
                for (name, value) in *pairs {
                    assert!(
                        document["attributes"].as_array().is_some_and(|written| written
                            .iter()
                            .any(|attribute| attribute["name"] == *name
                                && attribute["values"]
                                    .as_array()
                                    .is_some_and(|values| values.iter().any(|held| held == value)))),
                        "case {case:?}: the pair {name:?}={value:?} of {item:?} must be \
                         exported, got {document}"
                    );
                }
            }
            let manifest = keyed(&export.manifest_key()).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the manifest {:?} must be written, got {objects:?}",
                    export.manifest_key()
                )
            });
            let summary: ::serde_json::Value = ::serde_json::from_str(&manifest.body)
                .unwrap_or_else(|error| panic!("case {case:?}: {:?}: {error}", manifest.body));
            assert_eq!(
                summary["itemCount"],
                ::serde_json::json!(items.len()),
                "case {case:?}: the manifest counts the exported items, got {summary}"
            );
            assert_eq!(
                summary["dataFileS3Keys"],
                ::serde_json::json!([data_key]),
                "case {case:?}: the manifest names the data file it was written beside, \
                 got {summary}"
            );
        }
    }

    /// RULE: the encryption Amazon S3 is asked for is the one the modelled value names.
    #[test]
    fn the_requested_encryption_is_the_object_stores_own_spelling() {
        let rows: &[(&str, &str, &str)] = &[
            ("SSE-S3", "AES256", "AES256"),
            ("SSE-KMS", SSE_ALGORITHM_KMS, S3_SSE_KMS),
        ];
        for (case, modelled, expected) in rows {
            let actual = s3_sse_algorithm(modelled);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
