// SPDX-License-Identifier: CC0-1.0

//! The OpsMetadata objects Application Manager holds for this account: the metadata a
//! create persists, the ARN it is reached by, and the two ways an ARN can name none.
//!
//! "Systems Manager calls this type of data OpsData" — an OpsMetadata object is the
//! metadata Application Manager keeps ABOUT one application: `CreateOpsMetadata` registers
//! one for the application a `ResourceId` names, `GetOpsMetadata` reports what it holds and
//! `DeleteOpsMetadata` removes it. All three reach the one store below
//! ([`super::ComAmazonawsSsmAmazonssmState::ops_metadata`]), so whatever the create
//! persisted is what a later read reports.
//!
//! An object is addressed by its `OpsMetadataArn` — the only thing the reads take — so the
//! store is keyed by `(region, arn)`: Systems Manager is regional, the ARN carries that
//! Region, and the ARN is MINTED by the create ([`ops_metadata_arn`]) rather than chosen by
//! a caller. An ARN no create minted therefore names no object, which is what
//! [`OPS_METADATA_NOT_FOUND`] reports.
//!
//! The two ways an ARN names nothing are DIFFERENT refusals, and the order between them is
//! what a caller reads:
//!
//! 1. an ARN naming another ACCOUNT is one whose resource is not this requester's to read
//!    at all, which is answered before the store is looked at
//!    ([`OPS_METADATA_NOT_THE_OWNER`], the modelled
//!    `OpsMetadataInvalidArgumentException`);
//! 2. an ARN of this account that names no object held here is the not-found refusal
//!    (`OpsMetadataNotFoundException`).
//!
//! So a caller cannot learn from a not-found whether another account holds the object.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::MetadataValue;

/// The resource prefix an `OpsMetadata` ARN carries, from the modelled `OpsMetadataArn`
/// pattern `^arn:(aws[a-zA-Z-]*)?:ssm:…:opsmetadata\/([a-zA-Z0-9-_\.\/]*)$`.
pub const ARN_OPS_METADATA_RESOURCE_PREFIX: &str = "opsmetadata/";

/// The path Application Manager mints an OpsMetadata object's ARN under, between the
/// resource prefix and the application's own name: the objects are the ones Systems Manager
/// itself keeps for an application, so the path names the owner of the namespace.
pub const ARN_OPS_METADATA_PATH: &str = "aws/ssm/";

/// The last segment of an OpsMetadata object's ARN, after the application's name.
pub const ARN_OPS_METADATA_SUFFIX: &str = "/opsmetadata";

/// How this service reports an `OpsMetadataArn` of this account that names no object it
/// holds, through `OpsMetadataNotFoundException` ("The OpsMetadata object doesn't exist").
///
/// Fixed text for this error class: it states that the object is not there and interpolates
/// nothing from the request, so the ARN a caller submitted does not appear in it. Both reads
/// report it, so a get and a delete of the same absent object are worded alike.
pub const OPS_METADATA_NOT_FOUND: &str =
    "OpsMetadata object not found. Verify the Amazon Resource Name (ARN) and try again.";

/// How this service reports an `OpsMetadataArn` naming an account other than the
/// requester's, through `OpsMetadataInvalidArgumentException` ("One of the arguments passed
/// is invalid"): the ARN is well formed and the object it names, if any, is not this
/// requester's.
pub const OPS_METADATA_NOT_THE_OWNER: &str = "Invalid OpsMetadataArn. Requester is not the \
                                              owner of the resource. Verify the Amazon \
                                              Resource Name (ARN) and try again.";

/// How this service reports a `ResourceId` that names no application an OpsMetadata object
/// can be minted for: the id's own last segment is what the ARN carries
/// ([`application_name`]), and a segment the modelled `OpsMetadataArn` could not hold names
/// no application. Answered through `OpsMetadataInvalidArgumentException`.
pub const OPS_METADATA_RESOURCE_ID_INVALID: &str =
    "Invalid ResourceId. Specify the ID of an Application Manager application and try again.";

/// How many OpsMetadata objects this store holds at once, across every Region.
///
/// The EMULATOR's own ceiling rather than a quota Systems Manager publishes: a create is the
/// only thing that admits an object, callers may run as many as they like, and the store is
/// held in memory for as long as the process runs — so without a ceiling the one map would
/// grow for as long as creates keep arriving. A create past it is refused through the
/// modelled `OpsMetadataLimitExceededException` and writes nothing.
pub const MAX_OPS_METADATA: usize = 100;

/// What a create refused by [`MAX_OPS_METADATA`] reports: the emulator's own ceiling, named
/// as the emulator's, with the figure it stands at.
#[must_use]
pub fn ops_metadata_ceiling_message() -> String {
    format!("This emulator holds at most {MAX_OPS_METADATA} OpsMetadata objects across every Region")
}

/// What a create for an application this Region already holds an object for reports, through
/// `OpsMetadataAlreadyExistsException`: the ARN that is already held, which is the one this
/// create would have minted.
#[must_use]
pub fn ops_metadata_already_exists_message(arn: &str) -> String {
    format!("An OpsMetadata object already exists for the specified resource: {arn}")
}

/// The published bounds of `GetOpsMetadataMaxResults` (range 1..10). The maximum is also the
/// page a request naming no `MaxResults` is served: "the maximum number of items to return
/// for this call" bounds a page the caller asked to be bounded.
pub const MIN_OPS_METADATA_PAGE_SIZE: i32 = 1;

/// See [`MIN_OPS_METADATA_PAGE_SIZE`].
pub const MAX_OPS_METADATA_PAGE_SIZE: i32 = 10;

/// The page of metadata entries a read serves, or the refusal message for a `MaxResults`
/// outside the modelled range — which the boundary validator already refuses, so this is a
/// backstop rather than the first line of defence.
pub fn ops_metadata_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_OPS_METADATA_PAGE_SIZE,
        MAX_OPS_METADATA_PAGE_SIZE,
    )
}

/// The name the ARN of the object registered for `resource_id` carries: the id's own LAST
/// segment, which is the application it names.
///
/// A `ResourceId` is "a resource ID for a new Application Manager application"
/// (`CreateOpsMetadataRequest$ResourceId`), which is usually itself an ARN — and an
/// `OpsMetadataArn` admits only `[a-zA-Z0-9-_./]` after its resource prefix, which no ARN
/// with its colons could be spelled inside. So the name is the segment after the last `/`
/// or `:`, and `None` when that segment is empty or carries a character the ARN could not
/// hold ([`OPS_METADATA_RESOURCE_ID_INVALID`]).
#[must_use]
pub fn application_name(resource_id: &str) -> Option<&str> {
    let name = resource_id
        .rsplit(['/', ':'])
        .next()?;
    let admitted = |character: char| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
    };
    (!name.is_empty() && name.chars().all(admitted)).then_some(name)
}

/// The ARN of the OpsMetadata object registered for `application` in `region`:
/// `arn:aws:ssm:<region>:<account>:opsmetadata/aws/ssm/<application>/opsmetadata`.
#[must_use]
pub fn ops_metadata_arn(region: &str, application: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!(
            "{ARN_OPS_METADATA_RESOURCE_PREFIX}{ARN_OPS_METADATA_PATH}{application}\
             {ARN_OPS_METADATA_SUFFIX}"
        ),
    )
}

/// Whether `arn` names the account this endpoint serves — the question asked before the
/// store is looked at, so a caller is never told what another account holds.
///
/// An ARN whose account field is EMPTY names no other account either (the spelling AWS
/// writes an account-less resource in), so it is read as this account's; a value that is not
/// an ARN at all is left to the store, which holds no object under it. The modelled
/// `OpsMetadataArn` pattern is what admits a value here at all, so a malformed one never
/// reaches this.
#[must_use]
pub fn names_this_account(arn: &str) -> bool {
    ::service_common::arn::parse(arn).is_none_or(|parsed| {
        parsed.account.is_empty() || parsed.account == ::engine::emulator_account_id()
    })
}

/// One OpsMetadata object, as the reads report it.
#[derive(Debug, Clone)]
pub struct OpsMetadataRecord {
    /// The ARN this record is stored under, minted by the create ([`ops_metadata_arn`]).
    pub arn: String,
    /// The Region whose endpoint created this object, and which its ARN carries.
    pub region: String,
    /// "The ID of the Application Manager application", exactly as the create supplied it:
    /// the ARN carries only the application's name, so the id itself is held here and is
    /// what a read reports.
    pub resource_id: String,
    /// "Metadata for a new Application Manager application", keyed by metadata key, as the
    /// create supplied it. A `BTreeMap` so a paged read walks a stable, total order and can
    /// resume from the key it stopped at rather than from an offset.
    pub metadata: BTreeMap<String, String>,
    /// The object's tags, keyed by tag key, as the create supplied them and the tag plane
    /// rewrites them ([`super::tagging`]).
    pub tags: BTreeMap<String, String>,
    /// When the create ran.
    pub created_at: SystemTime,
}

/// The entries a read reports, as the modelled `MetadataMap` carries them: one entry per
/// key of the object, each carrying the value the write persisted.
#[must_use]
pub fn to_metadata_map<'held>(
    entries: impl IntoIterator<Item = (&'held String, &'held String)>,
) -> std::collections::HashMap<String, MetadataValue> {
    entries
        .into_iter()
        .map(|(key, value)| {
            (
                key.clone(),
                MetadataValue {
                    value: Some(value.clone()),
                },
            )
        })
        .collect()
}

/// The metadata a write carries, as the store holds it: the modelled `MetadataMap` flattened
/// to its values, so a read reports what was written rather than a second copy of the shape.
///
/// A `MetadataValue` that carries no value at all is held as the empty string: the member is
/// optional in the model, and an entry a caller wrote is an entry a read reports.
#[must_use]
pub fn to_metadata_entries(
    metadata: Option<std::collections::HashMap<String, MetadataValue>>,
) -> BTreeMap<String, String> {
    metadata
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| (key, value.value.unwrap_or_default()))
        .collect()
}

impl super::ComAmazonawsSsmAmazonssmState {
    /// The OpsMetadata object `arn` names in `region`, or `None` when that Region holds
    /// none — which is what the not-found refusal reports.
    pub fn ops_metadata_object(&self, region: &str, arn: &str) -> Option<&OpsMetadataRecord> {
        self.ops_metadata.get(&(region.to_string(), arn.to_string()))
    }

    /// The same record, to rewrite its tags or its metadata.
    pub fn ops_metadata_object_mut(
        &mut self,
        region: &str,
        arn: &str,
    ) -> Option<&mut OpsMetadataRecord> {
        self.ops_metadata
            .get_mut(&(region.to_string(), arn.to_string()))
    }

    /// Store `record` as an OpsMetadata object of its own Region, keyed by its ARN.
    pub fn insert_ops_metadata(&mut self, record: OpsMetadataRecord) {
        let key = (record.region.clone(), record.arn.clone());
        self.ops_metadata.insert(key, record);
    }

    /// Drop the OpsMetadata object `arn` names in `region`, answering whether one was held.
    pub fn remove_ops_metadata(&mut self, region: &str, arn: &str) -> bool {
        self.ops_metadata
            .remove(&(region.to_string(), arn.to_string()))
            .is_some()
    }

    /// Whether this store can hold another OpsMetadata object ([`MAX_OPS_METADATA`]).
    pub fn can_hold_another_ops_metadata(&self) -> bool {
        self.ops_metadata.len() < MAX_OPS_METADATA
    }
}
