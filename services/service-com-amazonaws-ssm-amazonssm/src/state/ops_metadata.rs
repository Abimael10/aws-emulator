// SPDX-License-Identifier: CC0-1.0

//! The OpsMetadata objects Application Manager holds for this account: the metadata a
//! create persists, the ARN it is reached by, and the two ways an ARN can name none.
//!
//! "Systems Manager calls this type of data OpsData" — an OpsMetadata object is the
//! metadata Application Manager keeps ABOUT one application: `CreateOpsMetadata` registers
//! one for the application a `ResourceId` names, `UpdateOpsMetadata` rewrites and drops its
//! entries, `GetOpsMetadata` reports what it holds, `ListOpsMetadata` reports the objects a
//! Region holds and `DeleteOpsMetadata` removes one.
//! All five reach the one store below
//! ([`super::ComAmazonawsSsmAmazonssmState::ops_metadata`]), so whatever the create
//! persisted is what a later read reports.
//!
//! An object is addressed by its `OpsMetadataArn` — the only thing the single-object reads
//! take — so the store is keyed by `(region, arn)`: Systems Manager is regional, the ARN
//! carries that Region, and the ARN is MINTED by the create ([`ops_metadata_arn`]) rather
//! than chosen by a caller. An ARN no create minted therefore names no object, which is what
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
//!
//! A LISTING takes no ARN at all: it reports the objects the endpoint's Region holds, in
//! ascending ARN order ([`super::ComAmazonawsSsmAmazonssmState::ops_metadata_in_region`]),
//! narrowed to the ones meeting every filter the request carries
//! ([`OpsMetadataRecord::matches_filter`]) and bounded by the page it asked for
//! ([`list_ops_metadata_page_size`]), with the ARN of the object left over past the page
//! handed back as the token the next page resumes at.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::aws_smithy_schema_server::constraints::ViolationKind;
use ::service_common::validation::{
    quoted_echo, rendered_list, rendered_map_in_key_order, rendered_record, unanchored,
    UNSET_MEMBER,
};
use ::service_common::validation_message::{
    element_set_requirement, envelope_over, map_key_refusal_quoting, requirement,
};

use crate::ops::types::{MetadataValue, OpsMetadata};

/// The least number of characters the modelled `MetadataKey` admits, as a refusal states
/// the bound (`smithy_query(query="shape",shape="MetadataMap")`: the key targets
/// `MetadataKey`, `length 1..256`).
pub const MIN_METADATA_KEY_CHARACTERS: i64 = 1;

/// See [`MIN_METADATA_KEY_CHARACTERS`].
pub const MAX_METADATA_KEY_CHARACTERS: i64 = 256;

/// The expression the modelled `MetadataKey` requires a key to satisfy: not empty, and not a
/// run of whitespace.
///
/// A key is held to it HERE rather than by the framework's own walk, for two reasons that
/// each make the walk admit a key the shape does not: a map's key is a path segment to that
/// walk rather than a constrained leaf, so none of the key shape's constraints are taken at
/// all, and the expression is a negative LOOKAHEAD, which the walk's regular-expression
/// engine cannot compile — and a pattern it cannot compile is read as satisfied, so even a
/// key carried as a list ELEMENT passes. So whatever member carries a key, the operation
/// taking it is what holds the key to the shape.
pub const METADATA_KEY_PATTERN: &str = r"^(?!\s*$).+$";

/// Whether `key` is a key the modelled `MetadataKey` admits: between
/// [`MIN_METADATA_KEY_CHARACTERS`] and [`MAX_METADATA_KEY_CHARACTERS`] characters, carrying
/// something other than whitespace ([`METADATA_KEY_PATTERN`]).
#[must_use]
pub fn metadata_key_is_well_formed(key: &str) -> bool {
    let characters = i64::try_from(key.chars().count()).unwrap_or(i64::MAX);
    (MIN_METADATA_KEY_CHARACTERS..=MAX_METADATA_KEY_CHARACTERS).contains(&characters)
        && !key.trim().is_empty()
}

/// The expression a clause of this service quotes for [`METADATA_KEY_PATTERN`]: the model's
/// text without the whole-value pair it wraps the published expression in, which is how this
/// service states a `@pattern` (`crate::service::refusal`, rule 4) and the tree's one reading
/// of that pair ([`unanchored`]).
///
/// A clause a HANDLER states has to read like the one a member the framework's own walk
/// reported gets — that walk reports an element of `KeysToDelete` outside the key shape's
/// `@length`, and this service renders its expression that way — so both halves of the same
/// shape's refusal quote the same text.
#[must_use]
pub fn quoted_metadata_key_pattern() -> String {
    unanchored(METADATA_KEY_PATTERN)
}

/// The requirements the modelled `MetadataKey` states, in the order a clause lists a
/// constraint set: the bounds, longest first, and then the expression.
fn metadata_key_requirements() -> Vec<String> {
    vec![
        requirement(ViolationKind::MaxLength(MAX_METADATA_KEY_CHARACTERS)),
        requirement(ViolationKind::MinLength(MIN_METADATA_KEY_CHARACTERS)),
        requirement(ViolationKind::Pattern(quoted_metadata_key_pattern())),
    ]
}

/// The shape a metadata entry's value is printed as inside a quoted map, and the one member
/// it declares (`smithy_query(query="shape",shape="MetadataValue")`).
const METADATA_VALUE_SHAPE: &str = "MetadataValue";

/// See [`METADATA_VALUE_SHAPE`].
const METADATA_VALUE_MEMBER: &str = "Value";

/// `entries` as a clause quotes a map-valued member: each entry `<key>=<value>` in ascending
/// key order inside braces ([`rendered_map_in_key_order`]), with a value printed as the
/// structure it is ([`rendered_record`]) and a member the request left unset stated the way
/// the validating front end states one. `None` for a map too large for a clause to quote.
fn rendered_metadata_map(
    entries: &std::collections::HashMap<String, MetadataValue>,
) -> Option<String> {
    let rendered: Vec<(&str, String)> = entries
        .iter()
        .map(|(key, value)| {
            (
                key.as_str(),
                rendered_record(
                    METADATA_VALUE_SHAPE,
                    &[(
                        METADATA_VALUE_MEMBER,
                        value.value.as_deref().unwrap_or(UNSET_MEMBER),
                    )],
                ),
            )
        })
        .collect();
    rendered_map_in_key_order(&rendered)
}

/// The CLAUSE of `message`, a whole refusal a shared renderer wrote over ONE clause.
///
/// What the framework's envelope puts in front of one clause is what [`envelope_over`] puts
/// in front of one, so dropping that prefix leaves the clause itself — which is what lets the
/// clauses about two members go into ONE envelope, the way the framework's own walk states
/// two violations of one request. Nothing of the sentence is re-spelled here.
fn clause_of(message: String) -> String {
    let empty = String::new();
    let prefix = envelope_over(std::slice::from_ref(&empty)).unwrap_or_default();
    match message.strip_prefix(&prefix) {
        Some(clause) => clause.to_string(),
        None => message,
    }
}

/// The clause a MAP carrying a key outside the modelled `MetadataKey` is refused with, or
/// `None` when every key of `entries` is one the shape admits.
///
/// It states the map MEMBER `member_path` names, the map the request carried, and what a key
/// of it has to satisfy ([`::service_common::validation_message::map_key_refusal_quoting`],
/// the shared rendering of that clause, so a clause about a key cannot drift from the one a
/// reported member gets). ONE clause however many of the map's keys offend: the clause is
/// about that member's keys and the set they have to satisfy.
///
/// The clause QUOTES the map, the way a clause of this service stated at a COLLECTION member
/// quotes the collection the request submitted (`crate::service::refusal`, rule 5); a map
/// past the largest value a clause carries is stated without one.
#[must_use]
pub fn metadata_map_key_clause(
    member_path: &str,
    entries: &std::collections::HashMap<String, MetadataValue>,
) -> Option<String> {
    if entries.keys().all(|key| metadata_key_is_well_formed(key)) {
        return None;
    }
    let quoted = rendered_metadata_map(entries).map(|map| quoted_echo(&map).into_owned());
    Some(clause_of(map_key_refusal_quoting(
        member_path,
        quoted.as_deref(),
        MIN_METADATA_KEY_CHARACTERS,
        MAX_METADATA_KEY_CHARACTERS,
        Some(&quoted_metadata_key_pattern()),
    )))
}

/// The clause a LIST carrying a key outside the modelled `MetadataKey` is refused with, or
/// `None` when every element of `keys` is one the shape admits.
///
/// The key shape's whole set, stated once at the collection member and quoting the list the
/// request carried, which is where this service states a constraint its elements broke
/// ([`::service_common::validation_message::element_set_requirement`]).
#[must_use]
pub fn metadata_key_list_clause(member_path: &str, keys: &[String]) -> Option<String> {
    if keys.iter().all(|key| metadata_key_is_well_formed(key)) {
        return None;
    }
    let elements: Vec<&str> = keys.iter().map(String::as_str).collect();
    let quoted = quoted_echo(&rendered_list(&elements)).into_owned();
    Some(format!(
        "Value '{quoted}' at '{member_path}' failed to satisfy constraint: {}",
        element_set_requirement(&metadata_key_requirements())
    ))
}

/// The whole refusal `clauses` make, in the framework's own envelope
/// ([`envelope_over`]) — `None` for a request that broke nothing, which is no refusal at all.
///
/// Every clause of one request goes in ONE envelope, so a request breaking the key shape at
/// two members is answered for both at once rather than one member at a time.
#[must_use]
pub fn metadata_key_refusal(clauses: &[String]) -> Option<String> {
    envelope_over(clauses)
}

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

/// How many metadata keys ONE OpsMetadata object holds at once.
///
/// The EMULATOR's own ceiling rather than a quota Systems Manager publishes: a create admits
/// at most the five entries `MetadataMap` carries, but an update may add five more each time
/// it runs, so without a ceiling one object's entries would grow for as long as updates keep
/// arriving. A write that would leave the object holding more than this is refused through
/// the modelled `OpsMetadataKeyLimitExceededException` and writes nothing.
pub const MAX_OPS_METADATA_KEYS: usize = 20;

/// What a write refused by [`MAX_OPS_METADATA_KEYS`] reports: the emulator's own ceiling,
/// named as the emulator's, with the figure it stands at.
#[must_use]
pub fn ops_metadata_key_ceiling_message() -> String {
    format!("This emulator holds at most {MAX_OPS_METADATA_KEYS} metadata keys per OpsMetadata object")
}

/// What an update carrying neither metadata to write nor keys to drop is refused with: such a
/// request names no change to make to the object, which is one of the arguments
/// `OpsMetadataInvalidArgumentException` is for.
pub const OPS_METADATA_UPDATE_IS_EMPTY: &str = "Invalid request. Specify MetadataToUpdate, \
                                                KeysToDelete, or both, and try again.";

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

/// The published bounds of `ListOpsMetadataMaxResults` (range 1..50): how many OBJECTS one
/// page of a listing carries, where [`MAX_OPS_METADATA_PAGE_SIZE`] bounds the metadata
/// ENTRIES of one object. The maximum is also the page a request naming no `MaxResults` is
/// served.
pub const MIN_LIST_OPS_METADATA_PAGE_SIZE: i32 = 1;

/// See [`MIN_LIST_OPS_METADATA_PAGE_SIZE`].
pub const MAX_LIST_OPS_METADATA_PAGE_SIZE: i32 = 50;

/// The page of objects a listing serves, or the refusal message for a `MaxResults` outside
/// the modelled range — which the boundary validator already refuses, so this is a backstop.
pub fn list_ops_metadata_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_LIST_OPS_METADATA_PAGE_SIZE,
        MAX_LIST_OPS_METADATA_PAGE_SIZE,
    )
}

/// The `OpsMetadataFilter` key a listing narrows on the application an object was registered
/// for (`OpsMetadata$ResourceId`, "The ID of the Application Manager application").
pub const OPS_METADATA_FILTER_KEY_RESOURCE_ID: &str = "ResourceId";

/// The `OpsMetadataFilter` key a listing narrows on what provisioned the application: an
/// Application Manager property of the application rather than a member of the `OpsMetadata`
/// shape, so an object carries it among its metadata entries
/// ([`OpsMetadataRecord::matches_filter`]).
pub const OPS_METADATA_FILTER_KEY_PROVISIONED_BY: &str = "provisioned-by";

/// The `OpsMetadataFilter` key a listing narrows on the kind of application — "information
/// about the new application, including the application type" (`CreateOpsMetadata`) — another
/// property the object carries among its metadata entries.
pub const OPS_METADATA_FILTER_KEY_APPLICATION_TYPE: &str = "application-type";

/// Every filter key a listing searches on, in the order a refusal lists them back to the
/// caller. A key outside it names no property of an OpsMetadata object, which is one of the
/// arguments `OpsMetadataInvalidArgumentException` is for.
pub const OPS_METADATA_FILTER_KEYS: &[&str] = &[
    OPS_METADATA_FILTER_KEY_RESOURCE_ID,
    OPS_METADATA_FILTER_KEY_PROVISIONED_BY,
    OPS_METADATA_FILTER_KEY_APPLICATION_TYPE,
];

/// Whether `key` is a key a listing searches on ([`OPS_METADATA_FILTER_KEYS`]).
#[must_use]
pub fn is_ops_metadata_filter_key(key: &str) -> bool {
    OPS_METADATA_FILTER_KEYS.contains(&key)
}

/// What a `Filters` entry naming a key a listing does not search on is refused with: the key
/// the request named, and the keys an OpsMetadata listing accepts. The sentence names the
/// filter SHAPE the key belongs to, which is how this operation words it.
#[must_use]
pub fn invalid_ops_metadata_filter_key_message(key: &str) -> String {
    format!(
        "The following OpsMetadataFilter key is not valid: {key}. Valid filter keys include: \
         [{}].",
        OPS_METADATA_FILTER_KEYS.join(", ")
    )
}

/// What a `Filters` entry carrying a key but no value to search for is refused with, worded
/// as the sibling listings of this service word it (`crate::ops::list_cloud_connectors`).
#[must_use]
pub fn ops_metadata_filter_value_missing(key: &str) -> String {
    format!("Filter key '{key}' carries no value to search for")
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
    let name = resource_id.rsplit(['/', ':']).next()?;
    let admitted =
        |character: char| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.');
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

/// The resource an authorization decision about a call that addresses no ONE OpsMetadata
/// object is taken against: every `opsmetadata` resource of `region`, the object path spelled
/// with IAM's own wildcard —
/// `arn:aws:ssm:<region>:<account>:opsmetadata/*`.
///
/// A create is decided about this rather than about an object's own ARN because the ARN only
/// exists once the create has minted it ([`ops_metadata_arn`]), and a listing because it names
/// no object either; so a statement covering the Region's OpsMetadata objects allows such a
/// call while one scoped to a single object does not.
#[must_use]
pub fn ops_metadata_resources(region: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("{ARN_OPS_METADATA_RESOURCE_PREFIX}*"),
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
    /// "The user name who last updated the OpsMetadata object" — the principal the last
    /// write to this object spoke for, as `sts:GetCallerIdentity` reports it
    /// ([`super::identity_authorization::caller_arn`]). The create is that write until
    /// something else writes to the object, so it is the creator until then.
    pub last_modified_user: String,
    /// "The date the OpsMetadata object was last updated." Equal to
    /// [`created_at`](Self::created_at) until something updates the object.
    pub last_modified_at: SystemTime,
}

impl OpsMetadataRecord {
    /// Whether this object meets the filter `key` carrying `values`: it is reported when ANY
    /// value names what the key filters on, so one filter is one criterion however many
    /// values it carries.
    ///
    /// `ResourceId` is held against the application the create named, the one member of the
    /// `OpsMetadata` shape a listing searches on; the other keys name Application Manager
    /// properties of the application, which its metadata entries hold, so such a key is held
    /// against the entry stored under it. A key outside [`OPS_METADATA_FILTER_KEYS`] names
    /// neither, and a listing refuses one before it reaches this.
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        values.iter().any(|value| match key {
            OPS_METADATA_FILTER_KEY_RESOURCE_ID => self.resource_id == *value,
            OPS_METADATA_FILTER_KEY_PROVISIONED_BY | OPS_METADATA_FILTER_KEY_APPLICATION_TYPE => {
                self.metadata.get(key).is_some_and(|held| held == value)
            }
            _ => false,
        })
    }

    /// How many metadata keys this object would hold once the keys `to_delete` names are
    /// dropped from it and the entries `to_update` carries are written over it.
    ///
    /// Taken BEFORE anything is written, since a write past [`MAX_OPS_METADATA_KEYS`] is
    /// refused and leaves the object as it was. An entry whose key the object already holds
    /// REPLACES that entry rather than adding one, and a key named by both lists is held
    /// afterwards, so neither is counted twice.
    #[must_use]
    pub fn key_count_after_update(
        &self,
        to_update: &BTreeMap<String, String>,
        to_delete: &[String],
    ) -> usize {
        let mut keys: std::collections::BTreeSet<&str> =
            self.metadata.keys().map(String::as_str).collect();
        for key in to_delete {
            keys.remove(key.as_str());
        }
        keys.extend(to_update.keys().map(String::as_str));
        keys.len()
    }

    /// Write `to_update` over this object's entries, drop the ones `to_delete` names, and
    /// record that `user` wrote the object at `at`.
    ///
    /// The deletions are applied FIRST, so a key named by both lists is left holding the value
    /// this write carries: the update states what the entry is to be, where the deletion only
    /// states that what was there is to go. A key the object does not hold is nothing to drop.
    pub fn apply_update(
        &mut self,
        to_update: BTreeMap<String, String>,
        to_delete: &[String],
        user: String,
        at: SystemTime,
    ) {
        for key in to_delete {
            self.metadata.remove(key);
        }
        self.metadata.extend(to_update);
        self.last_modified_user = user;
        self.last_modified_at = at;
    }

    /// This object as a listing reports it: the `OpsMetadata` members, each read out of what
    /// the writes to this object persisted.
    #[must_use]
    pub fn to_ops_metadata(&self) -> OpsMetadata {
        OpsMetadata {
            resource_id: Some(self.resource_id.clone()),
            ops_metadata_arn: Some(self.arn.clone()),
            last_modified_date: Some(::aws_smithy_types::DateTime::from(self.last_modified_at)),
            last_modified_user: Some(self.last_modified_user.clone()),
            creation_date: Some(::aws_smithy_types::DateTime::from(self.created_at)),
        }
    }
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

    /// Every OpsMetadata object `region` holds, in ascending ARN order.
    ///
    /// That order is what makes a listing's resume cursor total: the token a page hands back
    /// is the ARN of the object the next page starts at, not an offset into a list that may
    /// have grown or shrunk between two requests.
    pub fn ops_metadata_in_region<'a>(
        &'a self,
        region: &'a str,
    ) -> impl Iterator<Item = &'a OpsMetadataRecord> + 'a {
        self.ops_metadata
            .iter()
            .filter(move |((held, _), _)| held == region)
            .map(|(_, record)| record)
    }
}
