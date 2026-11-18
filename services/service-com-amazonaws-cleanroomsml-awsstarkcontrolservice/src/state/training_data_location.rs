// SPDX-License-Identifier: CC0-1.0

//! Whether the TRAINING DATA a create points at is there to be read.
//!
//! ## The rule this module implements
//!
//! A training dataset points at a Glue table, and Clean Rooms ML reads the records of that
//! table out of the Amazon S3 location the table stores its data at. A create whose table
//! stores its data at a location holding NO non-zero-sized object is refused — there is
//! nothing there to train on — with the sentence Clean Rooms ML words for that condition
//! ([`no_non_zero_sized_objects`]) under the service's own `ValidationException`, which is
//! HTTP 400: the request named a location that holds no data, which is what that error
//! reports about an incorrect request parameter.
//!
//! The condition is the LOCATION's contents, so it is judged for every dataset the create
//! lists, whichever table each one names.
//!
//! ## What it does NOT judge
//!
//! Only what it can read. A create whose Glue table this emulator's Data Catalog does not
//! hold, whose table states no location, whose location is not an Amazon S3 one, or that is
//! made on an engine carrying no Data Catalog or no object store at all, is left to the rest
//! of the create: there is no evidence about the data either way, and a refusal invented
//! from a store that is not there would refuse a dataset AWS accepts. A location that IS
//! readable and holds nothing is the one case this refuses.
//!
//! ## Reading the two owning services
//!
//! The Glue table and the objects are each read through the OWNING service's own state
//! accessor on the shared engine, rather than through a second copy of either: the catalog
//! entry is the one `glue GetTable` reports and the objects are the ones `s3 ListObjectsV2`
//! lists, so what this judges is what a client of those services sees.
//!
//! LOCK ORDER: each store's lock is taken, read and released here, one at a time, and this
//! is called with NO guard of this service's own store held — which is what keeps the
//! cross-service access one-directional and a lock cycle unreachable. The work done under
//! either lock is bounded by this module's own constants ([`MAX_JUDGED_DATASETS`],
//! [`MAX_SCANNED_OBJECTS`]) rather than by anything a request chooses, and nothing is awaited
//! while a lock is held.

use ::service_com_amazonaws_glue_awsglue::state as glue;
use ::service_com_amazonaws_s3_amazons3::state as objects;

use crate::ops::types::{Dataset, GlueDataSource};

/// How many of a create's datasets this judges.
///
/// `CreateTrainingDatasetRequest$trainingData` is a `DatasetList` modelled
/// `@length(min: 1, max: 1)` (`smithy_query(query="operation",
/// operation="CreateTrainingDataset")`), so one request names ONE dataset and that list's own
/// validation refuses a longer one before this runs. The bound is stated here rather than
/// assumed, so the work one request can ask of the two stores below is capped by a constant
/// times [`MAX_SCANNED_OBJECTS`] whatever reaches this function.
const MAX_JUDGED_DATASETS: usize = 1;

/// How many objects of ONE location this looks at before it stops asking.
///
/// The object store's own per-bucket quota is well below this, so a bucket that service holds is
/// always read to the end and the bound refuses nothing; it is here so the work one create can
/// ask for is a constant of this module rather than another crate's quota. A location this could
/// not finish reading yields no evidence either way ([`holds_training_data`] answers `None`),
/// never a refusal.
const MAX_SCANNED_OBJECTS: usize = 10_000;

/// Why a create cannot read the training data its first unreadable dataset points at, or
/// `None` where every dataset it lists has data to read (and where there is nothing to judge
/// — see the module docs).
///
/// The datasets are walked in the order the request lists them, bounded by
/// [`MAX_JUDGED_DATASETS`], so a create naming more than one is refused for the first one that
/// has no data, which is the one a caller fixes first.
pub fn unreadable_training_data<Eng: Send + Sync + 'static>(
    engine: &Eng,
    training_data: &[Dataset],
) -> Option<String> {
    training_data
        .iter()
        .take(MAX_JUDGED_DATASETS)
        .find_map(|dataset| unreadable_dataset(engine, dataset))
}

/// Why ONE dataset's training data cannot be read, or `None` where it can be (or where
/// nothing about it can be judged).
fn unreadable_dataset<Eng: Send + Sync + 'static>(
    engine: &Eng,
    dataset: &Dataset,
) -> Option<String> {
    let location = table_location(engine, &dataset.input_config.data_source.glue_data_source)?;
    let (bucket, prefix) = s3_location(&location)?;
    match holds_training_data(engine, &bucket, &prefix) {
        // Readable, and there is something to train on — or there is no object store to read,
        // which says nothing about the data.
        Some(true) | None => None,
        Some(false) => Some(no_non_zero_sized_objects(&location)),
    }
}

/// The sentence Clean Rooms ML words for a location that holds no data: fixed text, with the
/// location after it EXACTLY as the create's own Glue table states it — the variable part of the
/// message is that stored value, so nothing here rewrites it.
fn no_non_zero_sized_objects(location: &str) -> String {
    format!("Failed to find any non-zero sized objects in this S3 location {location}.")
}

/// Where the Glue table a dataset names stores its data, as this emulator's Data Catalog
/// holds it; `None` where that catalog, the database, the table or the table's own location
/// is absent.
///
/// The catalog a dataset leaves unnamed is the calling account's own, which is the default
/// AWS Glue documents for a `CatalogId` and the one the record of a created dataset carries
/// ([`crate::state::stored_training_data`]).
fn table_location<Eng: Send + Sync + 'static>(
    engine: &Eng,
    source: &GlueDataSource,
) -> Option<String> {
    let catalog = glue::service_state(engine)?;
    let catalog_id = glue::resolve_catalog_id(source.catalog_id.as_ref());
    let catalog = catalog.lock();
    let table = catalog
        .database(&catalog_id, &source.database_name)?
        .tables
        // The catalog keys a table by its folded name and folds a requested name the same
        // way, so the table is reached by the casing the dataset names.
        .get(&glue::fold_name(&source.table_name))?;
    table
        .storage_descriptor
        .as_ref()?
        .location
        .clone()
        .filter(|location| !location.is_empty())
}

/// Whether the object store holds at least one NON-ZERO-SIZED object under `prefix` of
/// `bucket`; `None` where there is no object store on this engine to read at all, or where the
/// location holds more objects than [`MAX_SCANNED_OBJECTS`] — neither says anything about the
/// data.
///
/// An object of zero bytes carries no records, which is exactly the condition this answers —
/// and it is what a "folder" marker is, so a location holding only those holds no data. The
/// boundary of the prefix is honoured as a DIRECTORY: an object at the prefix itself, or under
/// it, is the table's; a sibling key that merely starts with the same characters is not.
///
/// The walk is bounded by [`MAX_SCANNED_OBJECTS`] and stops at the first object that carries
/// data, so the work ONE create asks of this store is at most that many key comparisons per
/// dataset it names ([`MAX_JUDGED_DATASETS`] of them) — a product that is a constant of this
/// module rather than anything a request chooses. The store's lock is held for exactly that
/// walk and nothing is awaited while it is.
fn holds_training_data<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bucket: &str,
    prefix: &str,
) -> Option<bool> {
    let state = objects::service_state(engine)?;
    let store = state.lock();
    let Some(held) = store.bucket(bucket) else {
        // The bucket is not there, so the location holds nothing — which the caller reports
        // as the location holding no object rather than as nothing being known.
        return Some(false);
    };
    for (scanned, (key, object)) in held.objects().enumerate() {
        if scanned >= MAX_SCANNED_OBJECTS {
            // The location was not read to the end, so nothing is known about what it holds.
            return None;
        }
        if !object.body.is_empty() && selects(key, prefix) {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether `key` is one of the objects `prefix` names: the object AT that prefix, or an object
/// under it as a directory.
fn selects(key: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    match key.strip_prefix(prefix) {
        None => false,
        Some(rest) => rest.is_empty() || prefix.ends_with('/') || rest.starts_with('/'),
    }
}

/// The `(bucket, key)` an `s3://bucket/key` location names, or `None` for a value that names
/// no Amazon S3 location. The key is empty for a location naming only a bucket.
///
/// Read through the workspace's ONE reading of an `s3://` URI
/// ([`::service_common::s3_uri`]) rather than split here: that reading is the maintained URL
/// parser plus Amazon S3's own bucket-naming rule, so a location this crate judges is the same
/// location every other reader of that spelling resolves. A URI naming a bucket and nothing
/// under it addresses the bucket's own root, which is the prefix every object in it is under.
fn s3_location(location: &str) -> Option<(String, String)> {
    ::service_common::s3_uri::object_address(location).or_else(|| {
        ::service_common::s3_uri::bucket_name(location).map(|bucket| (bucket, String::new()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A location is read as a bucket and a key however it spells them, and anything that is
    /// not an `s3://` location Amazon S3 could hold a bucket for names none.
    #[test]
    fn an_s3_location_is_read_as_its_bucket_and_prefix() {
        for (location, expected) in [
            ("s3://lake/data/", Some(("lake", "data/"))),
            ("s3://lake/data", Some(("lake", "data"))),
            ("s3://lake/a/b/c", Some(("lake", "a/b/c"))),
            ("s3://lake/", Some(("lake", ""))),
            ("s3://lake", Some(("lake", ""))),
            ("s3://", None),
            ("s3:///data", None),
            // A name Amazon S3 holds no bucket under: its naming rule admits no upper case
            // (`service_common::s3_bucket_name`).
            ("s3://Lake/data", None),
            ("hdfs://lake/data", None),
            ("", None),
        ] {
            let read = s3_location(location);
            let read = read
                .as_ref()
                .map(|(bucket, prefix)| (bucket.as_str(), prefix.as_str()));
            assert_eq!(
                read, expected,
                "case {location:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// A prefix selects what lives at it and under it as a directory, and never a sibling key
    /// that merely begins with the same characters.
    #[test]
    fn a_prefix_selects_its_own_objects_only() {
        for (key, prefix, expected) in [
            ("data/part-0", "data/", true),
            ("data/nested/part-0", "data/", true),
            ("data", "data", true),
            ("data/part-0", "data", true),
            ("database/part-0", "data", false),
            ("other/part-0", "data/", false),
            ("anything", "", true),
        ] {
            let selected = selects(key, prefix);
            assert_eq!(
                selected, expected,
                "case {key:?} under {prefix:?}: expected {expected}, got {selected}"
            );
        }
    }

    /// The refusal names the location the table states, character for character: the sentence
    /// is fixed text and the location is the stored value, so nothing rewrites it.
    #[test]
    fn a_refusal_names_the_location_the_table_states() {
        for location in [
            "s3://lake/data/",
            "s3://lake/data",
            "s3://lake/events.csv",
            "s3://lake",
        ] {
            let reported = no_non_zero_sized_objects(location);
            assert_eq!(
                reported,
                format!(
                    "Failed to find any non-zero sized objects in this S3 location {location}."
                ),
                "case {location:?}: expected the location verbatim, got {reported:?}"
            );
        }
    }
}
