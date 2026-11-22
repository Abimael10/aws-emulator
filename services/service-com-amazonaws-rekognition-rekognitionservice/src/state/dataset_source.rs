// SPDX-License-Identifier: CC0-1.0

//! WHERE the entries of a dataset `CreateDataset` records come from, and why a source cannot
//! be read.
//!
//! ## The rule this module implements
//!
//! A `DatasetSource` names one of two sources, and never both: "to use an Amazon Sagemaker
//! format manifest file, specify the S3 bucket location in the `GroundTruthManifest` field. To
//! create a copy of an existing dataset, specify the Amazon Resource Name (ARN) of an existing
//! dataset in `DatasetArn`. You need to specify a value for `DatasetArn` or
//! `GroundTruthManifest`, but not both. if you supply both values, or if you don't specify any
//! values, an InvalidParameterException exception occurs"
//! (`smithy_query(query="shape",shape="DatasetSource")`). A create that states NO source at all
//! is a different thing from a source that states neither member: the member is optional on the
//! request, and such a create makes an EMPTY dataset.
//!
//! What a source contributes is [`DatasetContents`](super::datasets::DatasetContents), and it is
//! COUNTED off the source rather than assumed:
//!
//! * a MANIFEST is an object of this emulator's own Amazon S3, reached the way every other
//!   object this service reads is ([`super::image_source::s3_client`]), and it is JSON LINES —
//!   "a single update or addition is an entry (JSON Line) that provides information about a
//!   single image. To update an existing entry, you match the `source-ref` field of the update
//!   entry with the `source-ref` field of the entry that you want to update"
//!   (`smithy_query(query="shape",shape="DatasetChanges")`) — so each line is read as one entry
//!   and the dataset's counts are the counts of the entries that were really there
//!   ([`counted_manifest`]);
//! * a DATASET ARN is another dataset of this account, and a copy of one holds what it holds,
//!   so the contents are that dataset's.
//!
//! An address the object store does not serve is the modelled `InvalidS3ObjectException`, and
//! a `DatasetSource` that states both of its members or neither is
//! `InvalidParameterException`: both are [`UnreadableDatasetSource`] variants, each with
//! exactly one modelled answer, so the handler picks rather than classifies. (A DATASET ARN
//! naming nothing is the handler's own lookup to refuse, that source being another dataset of
//! this store rather than an object of the store this module reads.)

use std::collections::BTreeSet;

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::serde_json::Value;

use crate::ops::types::DatasetSource;

use super::datasets::DatasetContents;

/// How much of a manifest object this service reads into the process.
///
/// A manifest is read in full to be counted, so how large one may be is bounded — this
/// emulator holds its object store in memory and a manifest no larger than this is one it can
/// count without growing the process without limit. It is this emulator's own ceiling, not an
/// AWS-published one.
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

/// The member of one manifest entry that names the image the entry is about: "you match the
/// `source-ref` field of the update entry with the `source-ref` field of the entry that you
/// want to update" (`DatasetChanges$GroundTruth`), so an entry without it is about no image.
const SOURCE_REF_MEMBER: &str = "source-ref";

/// The suffix of the member that carries the METADATA of a label attribute: a Ground Truth
/// manifest states a label under an attribute name and its metadata under that name plus this
/// suffix, which is where the label's own NAME is written.
const METADATA_SUFFIX: &str = "-metadata";

/// The member a label attribute's metadata names an image-level label in.
const CLASS_NAME_MEMBER: &str = "class-name";

/// The member a label attribute's metadata names the labels of a localization attribute in: a
/// map from the class id each annotation carries to that class's own name.
const CLASS_MAP_MEMBER: &str = "class-map";

/// Why the source a `CreateDataset` named cannot be read. Each variant has exactly one
/// modelled answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnreadableDatasetSource {
    /// The `DatasetSource` states both of its members, or neither: the one case its own
    /// documentation names. Answered with `InvalidParameterException`.
    InvalidSource,
    /// The manifest names an object this emulator's Amazon S3 does not serve, names one
    /// incompletely, or holds more than [`MAX_MANIFEST_BYTES`]. Answered with
    /// `InvalidS3ObjectException`.
    UnreadableObject,
}

/// What ONE manifest object holds, as the lines of it that were read.
///
/// Separate from [`locate_manifest`] so the counting is pure: the S3 read is what can fail,
/// and the reading of what arrived is a function of the bytes alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The object's bucket, as the request named it.
    pub bucket: String,
    /// The object's key, as the request named it.
    pub name: String,
    /// The manifest's own bytes, as the object store served them.
    pub body: Vec<u8>,
}

/// The manifest object `source` names, once this emulator's Amazon S3 has served it.
///
/// `None` is a source that names no manifest at all, which is the DATASET-ARN source or no
/// source: those have nothing to read out of the object store.
pub async fn locate_manifest(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    source: &DatasetSource,
) -> Result<Option<Manifest>, UnreadableDatasetSource> {
    let manifest = match (source.ground_truth_manifest.as_ref(), source.dataset_arn.as_deref()) {
        // Both members, or neither: the one case the shape's own documentation refuses.
        (Some(_), Some(_)) | (None, None) => {
            return Err(UnreadableDatasetSource::InvalidSource)
        }
        (None, Some(_)) => return Ok(None),
        (Some(manifest), None) => manifest,
    };
    let Some(object) = manifest.s3_object.as_ref() else {
        return Err(UnreadableDatasetSource::UnreadableObject);
    };
    // An S3 object is identified by its bucket and its key: without both there is no object to
    // read, and nothing was read, so this is the same refusal as an address the store does not
    // serve — exactly as the image and the video paths beside this one classify it.
    let (Some(bucket), Some(name)) = (
        object.bucket.as_deref().filter(|value| !value.is_empty()),
        object.name.as_deref().filter(|value| !value.is_empty()),
    ) else {
        return Err(UnreadableDatasetSource::UnreadableObject);
    };
    let region = super::request_region(ctx.parts());
    let read = super::image_source::s3_client(ctx, &region)
        .get_object()
        .bucket(bucket)
        .key(name)
        .set_version_id(object.version.clone())
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => {
            if !super::image_source::store_denies_object(error.code()) {
                // Not the store reporting an object it does not serve but this emulator
                // failing to ask its own object store — worth a warning, because a deployment
                // with no Amazon S3 mounted to ask is one where no manifest is ever readable.
                // The caller's own values are rendered in their escaped `Debug` form, so a
                // bucket or key carrying a newline cannot forge a log line.
                ::tracing::warn!(
                    bucket = ?bucket,
                    key = ?name,
                    code = ?error.code(),
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "a dataset manifest could not be read from the emulator's Amazon S3"
                );
            }
            return Err(UnreadableDatasetSource::UnreadableObject);
        }
    };
    // The ceiling, against what the store SAYS the object holds, before any of it is read into
    // this process.
    if read
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .is_some_and(|declared| declared > MAX_MANIFEST_BYTES)
    {
        return Err(UnreadableDatasetSource::UnreadableObject);
    }
    let body = match read.body.collect().await {
        Ok(body) => body.into_bytes(),
        // The store answered, but the body did not arrive: nothing of the manifest is here.
        Err(error) => {
            ::tracing::warn!(
                bucket = ?bucket,
                key = ?name,
                error = %error,
                "a dataset manifest's bytes did not arrive from the emulator's Amazon S3"
            );
            return Err(UnreadableDatasetSource::UnreadableObject);
        }
    };
    if body.len() > MAX_MANIFEST_BYTES {
        return Err(UnreadableDatasetSource::UnreadableObject);
    }
    Ok(Some(Manifest {
        bucket: bucket.to_string(),
        name: name.to_string(),
        body: body.to_vec(),
    }))
}

/// What the entries of `manifest` add up to: one entry per JSON Line, counted.
///
/// * every non-blank line is one entry of the dataset, so `TotalEntries` is how many there
///   were;
/// * a line that is not a JSON object, or an object that names no image
///   ([`SOURCE_REF_MEMBER`]), is an entry that "contains at least one error", which is what
///   `ErrorEntries` counts;
/// * an entry is LABELED when it declares a label attribute — any member that is neither the
///   image reference nor another member's metadata — which is what `LabeledEntries` counts;
/// * the LABELS are the distinct names those attributes' metadata declares
///   ([`labels_of_entry`]), which is what `TotalLabels` counts.
///
/// Nothing here reads the IMAGE an entry names: an entry is a statement about an image, and
/// what this counts is the statements.
pub fn counted_manifest(manifest: &Manifest) -> DatasetContents {
    let text = String::from_utf8_lossy(&manifest.body);
    let mut contents = DatasetContents::default();
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        contents.total_entries = contents.total_entries.saturating_add(1);
        let Ok(Value::Object(entry)) = ::serde_json::from_str::<Value>(line) else {
            contents.error_entries = contents.error_entries.saturating_add(1);
            continue;
        };
        let names_an_image = entry
            .get(SOURCE_REF_MEMBER)
            .and_then(Value::as_str)
            .is_some_and(|reference| !reference.is_empty());
        if !names_an_image {
            contents.error_entries = contents.error_entries.saturating_add(1);
            continue;
        }
        let declared = labels_of_entry(&entry);
        if entry.keys().any(|member| is_label_attribute(member)) {
            contents.labeled_entries = contents.labeled_entries.saturating_add(1);
        }
        labels.extend(declared);
    }
    contents.labels = labels;
    contents
}

/// The label names ONE manifest entry declares.
///
/// A label attribute's own metadata member is where the name is written: an image-level label
/// states it as [`CLASS_NAME_MEMBER`], and a localization attribute states the names of its
/// classes as the [`CLASS_MAP_MEMBER`] from class id to name. An entry whose metadata declares
/// neither contributes no label — not an invented one.
fn labels_of_entry(entry: &::serde_json::Map<String, Value>) -> BTreeSet<String> {
    let mut labels = BTreeSet::new();
    for (member, value) in entry {
        if member == SOURCE_REF_MEMBER || !member.ends_with(METADATA_SUFFIX) {
            continue;
        }
        let Some(metadata) = value.as_object() else {
            continue;
        };
        if let Some(name) = metadata
            .get(CLASS_NAME_MEMBER)
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            labels.insert(name.to_string());
        }
        if let Some(classes) = metadata.get(CLASS_MAP_MEMBER).and_then(Value::as_object) {
            labels.extend(
                classes
                    .values()
                    .filter_map(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(ToString::to_string),
            );
        }
    }
    labels
}

/// Whether an entry declares a LABEL ATTRIBUTE under `member`: a member that is neither the
/// image reference nor the metadata of another attribute.
///
/// Public so the counting rule above and anything that reads an entry later agree on what a
/// label attribute is.
pub fn is_label_attribute(member: &str) -> bool {
    member != SOURCE_REF_MEMBER && !member.ends_with(METADATA_SUFFIX)
}
