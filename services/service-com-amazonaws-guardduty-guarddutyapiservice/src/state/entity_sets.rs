// SPDX-License-Identifier: CC0-1.0

//! Entity sets: the trusted- and threat-entity lists a detector holds, their
//! statuses and the location they are published from.
//!
//! A trusted entity set is an uploaded list of IP addresses and domains, held in
//! S3 and taken into use by GuardDuty. The record here carries what the create
//! stored plus the status the set is in; the status moves only when the CLOCK
//! owes it, through [`EntitySetRecord::settle`].
//!
//! A set a delete has been served for is no longer one the service HOLDS: the
//! deletion drops the record ([`delete_entity_set`]), so the id it named resolves
//! to nothing from that instant on and every read of it is refused the way an id
//! no create ever minted is.

use super::*;

/// `TrustedEntitySetStatus`: the status a set the caller did NOT ask to activate
/// stays in. The enum is `INACTIVE | ACTIVATING | ACTIVE | DEACTIVATING | ERROR
/// | DELETE_PENDING | DELETED` (`smithy_query(shape="TrustedEntitySetStatus")`).
pub const ENTITY_SET_INACTIVE: &str = "INACTIVE";

/// The `TrustedEntitySetStatus` a set the caller asked to activate starts in: the
/// uploaded list is being taken into use. See [`ENTITY_SET_INACTIVE`] for the
/// enum.
pub const ENTITY_SET_ACTIVATING: &str = "ACTIVATING";

/// The `TrustedEntitySetStatus` an activating set reaches once the clock owes it:
/// GuardDuty is using the list. See [`ENTITY_SET_INACTIVE`] for the enum.
pub const ENTITY_SET_ACTIVE: &str = "ACTIVE";

/// The `TrustedEntitySetStatus` a set an update asked to STOP using is in until
/// the clock owes it [`ENTITY_SET_INACTIVE`]: the mirror of
/// [`ENTITY_SET_ACTIVATING`]. See [`ENTITY_SET_INACTIVE`] for the enum.
pub const ENTITY_SET_DEACTIVATING: &str = "DEACTIVATING";

/// How long a set the caller asked to activate stays [`ENTITY_SET_ACTIVATING`],
/// or a set an update asked to stop using stays [`ENTITY_SET_DEACTIVATING`],
/// before the clock owes it the settled status.
///
/// AWS documents no duration for taking an uploaded list into use, so this span is
/// CHOSEN — the same one [`INVESTIGATION_TRANSITION_MILLIS`] uses, and for the same
/// reason: non-zero, so a caller reading straight after the create still observes
/// the transient status, and short enough that nothing has to wait long for the
/// settled one.
///
/// A DELETION has no such span: an entity set's delete drops the record
/// ([`delete_entity_set`]), so there is no transitional status for it to move
/// through and nothing left for a read to report.
pub const ENTITY_SET_TRANSITION_MILLIS: i64 = 500;

/// The wire name of the `TrustedEntitySetId` member, as the request carries it in
/// its URI label and the create reports it
/// (`smithy_query(operation="GetTrustedEntitySet")` renders the `jsonName`
/// `trustedEntitySetId`). It is what a rejection names.
pub const TRUSTED_ENTITY_SET_ID_MEMBER: &str = "trustedEntitySetId";

/// The wire name of the `ThreatEntitySetId` member, as the request carries it in
/// its URI label and the create reports it
/// (`smithy_query(operation="GetThreatEntitySet")` renders the `jsonName`
/// `threatEntitySetId`). It is what a rejection names.
pub const THREAT_ENTITY_SET_ID_MEMBER: &str = "threatEntitySetId";

/// The wire name of the `Name` member an entity-set create carries
/// (`jsonName` `name`), for the same reason.
pub const ENTITY_SET_NAME_MEMBER: &str = "name";

/// The wire name of the `Location` member an entity-set create carries
/// (`jsonName` `location`), for the same reason.
pub const ENTITY_SET_LOCATION_MEMBER: &str = "location";

/// The message a second `CreateTrustedEntitySet` is refused with. "At any given
/// time, you can have only one trusted entity set"
/// (`smithy_query(query="doc",shape="CreateTrustedEntitySet")`), and the set hangs
/// off the detector, which is the account/Region-level resource.
pub const TRUSTED_ENTITY_SET_ALREADY_EXISTS_MESSAGE: &str =
    "The request is rejected because a trusted entity set already exists for the input detectorId.";

/// How many threat entity sets one detector may hold.
///
/// Unlike a trusted entity set, of which "at any given time, you can have only
/// one" (`smithy_query(query="doc",shape="CreateTrustedEntitySet")`), AWS
/// documents no such rule for a threat entity set
/// (`smithy_query(query="doc",shape="CreateThreatEntitySet")`), so a detector may
/// hold several.
///
/// This ceiling is therefore EMULATOR-CHOSEN, not a published threat-entity-set
/// quota: it reuses the number AWS publishes for the account's threat lists in a
/// Region ([`THREAT_INTEL_SETS_PER_DETECTOR`]) because an entity set is the same
/// kind of uploaded list under a newer API, and a detector is the account's
/// single resource in a Region. Read it as "the emulator holds this many", never
/// as a documented AWS limit.
///
/// A ceiling of some kind is what BOUNDS this store: without it a caller varying
/// or omitting `ClientToken` would grow [`Detector::threat_entity_sets`] by one
/// entry per request, and the `@idempotencyToken` scan over it
/// ([`Detector::threat_entity_set_for_token`]) would grow with it.
pub const THREAT_ENTITY_SETS_PER_DETECTOR: usize = THREAT_INTEL_SETS_PER_DETECTOR;

/// The message a `CreateThreatEntitySet` past
/// [`THREAT_ENTITY_SETS_PER_DETECTOR`] is refused with. It names the limit, never
/// the caller's values.
pub fn threat_entity_set_limit_message() -> String {
    format!(
        "The request is rejected because the account has reached the limit of {THREAT_ENTITY_SETS_PER_DETECTOR} threat entity sets."
    )
}

/// Does `name` hold only the characters an entity-set `Name` may contain?
///
/// "The name of your list can include lowercase letters, uppercase letters,
/// numbers, dash (-), and underscore (_)"
/// (`smithy_query(query="doc",shape="CreateTrustedEntitySetRequest",member="Name")`)
/// — a documented constraint the model's `len 1..300` does not express, so it is
/// enforced here.
/// The alphabet is the shared one ([`is_list_name`]) WITHOUT the space, which
/// the entity-set documentation does not list.
pub fn is_entity_set_name(name: &str) -> bool {
    is_list_name(name, false)
}

/// Is `location` a valid Amazon S3 URL for an UPLOADED LIST?
///
/// "The format of the Location URL must be a valid Amazon S3 URL format. Invalid
/// URL formats will result in an error, regardless of whether you activate the
/// entity set or not"
/// (`smithy_query(query="doc",shape="CreateTrustedEntitySetRequest",member="Location")`).
///
/// The rule is the list FAMILY's, not the entity set's: every uploaded list's
/// `Location` is "the URI of the file that contains the …"
/// (`smithy_query(query="doc",shape="CreateIPSetRequest",member="Location")` and
/// its threat-list sibling) — the file GuardDuty fetches from Amazon S3 — so the
/// IPSet, the threat list and the entity sets are all held to this one syntax,
/// and a `Location` the service could never fetch is refused instead of stored.
/// The modelled constraint on each of those members is a `len 1..300` with no
/// pattern, so the handler is the enforcement point.
///
/// The accepted forms are the S3 URL forms, and nothing about the HOST or the PORT
/// of an object URL narrows them: an uploaded list is addressed either by the
/// `s3://bucket/key` scheme or by an object URL — an `http`/`https` URL that names
/// a bucket and a non-empty object key — resolved against whichever Amazon S3
/// endpoint that URL names. The bucket sits in the first path segment of a
/// path-style URL (`<s3 endpoint>/bucket/key`) and in the leading host label of a
/// virtual-hosted-style one (`bucket.s3<...>/key`).
///
/// The host is NOT a requirement: a public `amazonaws.com` S3 endpoint, a
/// Region-specific one and the endpoint the caller's own S3 client is configured
/// with are all S3 endpoints, and which of them is in force is environment-specific.
/// What stays refused is a value that is no S3 URL at all: free text, a bare path,
/// another scheme, or a URL that names no object (a bucket with no key).
///
/// It is the PREDICATE of [`s3_object_of_location`], the one reading of which object
/// such a value addresses, so the syntax a create is held to and the object a read
/// announces are decided by the same code.
pub fn is_s3_location(location: &str) -> bool {
    s3_object_of_location(location).is_some()
}

/// The S3 OBJECT `location` addresses — the `(bucket, key)` of the uploaded list — or
/// `None` for a value that addresses no object at all, which is exactly what
/// [`is_s3_location`] refuses.
///
/// The ONE reading of an uploaded list's `Location`, so what a create and an update
/// VALIDATE and what a read REPORTS ([`stored_list_location`]) cannot drift: the
/// accepted spellings are the ones [`is_s3_location`] documents, and the bucket and the
/// key are read off whichever of them the caller wrote — the authority of an `s3://` URL
/// is the bucket and its path the key, a virtual-hosted-style object URL carries the
/// bucket in the leading host labels, and every other object URL is read path-style, its
/// first path segment being the bucket and the rest the key.
///
/// The KEY is taken out of the submitted value as the caller WROTE it
/// ([`raw_location_key`]) rather than off the parsed URL, which reports its path
/// percent-ENCODED: a key naming the object in any script is the caller's own and comes
/// back as it went in, encoded exactly as far as it was submitted encoded.
pub fn s3_object_of_location(location: &str) -> Option<(String, String)> {
    let url = ::url::Url::parse(location).ok()?;
    let host = url.host_str()?;
    if host.is_empty() {
        return None;
    }
    let key = raw_location_key(location);
    match url.scheme() {
        // `s3://bucket/key`: the authority IS the bucket and the path the key.
        "s3" => (!key.is_empty()).then(|| (host.to_string(), key.to_string())),
        // An object URL on an S3 endpoint. Virtual-hosted style carries the bucket
        // in the host and the key in the path; every other host is read path-style,
        // the first path segment being the bucket and the rest the key.
        "http" | "https" => match virtual_hosted_bucket(host) {
            Some(bucket) => (!bucket.is_empty() && !key.is_empty())
                .then(|| (bucket.to_string(), key.to_string())),
            None => key.split_once('/').and_then(|(bucket, object_key)| {
                (!bucket.is_empty() && !object_key.is_empty())
                    .then(|| (bucket.to_string(), object_key.to_string()))
            }),
        },
        _ => None,
    }
}

/// What `location` carries after its authority, with the leading `/` and any query or
/// fragment removed: the path as the caller wrote it, which is the key (or the
/// `bucket/key` of a path-style object URL).
///
/// The empty string for a value that carries no path at all, and for one this module's
/// reader has not established is a URL — such a value never reaches an announced location,
/// because [`s3_object_of_location`] answers `None` for it.
fn raw_location_key(location: &str) -> &str {
    let Some((_scheme, after_scheme)) = location.split_once("://") else {
        return "";
    };
    let path = match after_scheme.find('/') {
        Some(end) => after_scheme[end..].trim_start_matches('/'),
        None => return "",
    };
    path.split(['?', '#']).next().unwrap_or(path)
}

/// The bucket a virtual-hosted-style S3 `host` carries — the labels before its `s3`
/// label — or `None` when `host` names an S3 endpoint directly (so a URL on it is
/// path-style) or is no S3 endpoint host at all.
///
/// Every regional spelling is an S3 endpoint host, dotted
/// (`s3.us-east-1.amazonaws.com`) or dashed (`s3-us-east-1.amazonaws.com`), as is the
/// Region-less `s3.amazonaws.com` — and so is an endpoint that serves the S3 API
/// under any other domain, which is why the domain itself is not part of the test.
pub(super) fn virtual_hosted_bucket(host: &str) -> Option<&str> {
    let prefix = s3_endpoint_prefix(host)?;
    (!prefix.is_empty()).then_some(prefix)
}

/// What precedes the `s3` label in an S3 endpoint `host`, if `host` is one: the
/// empty string for a path-style endpoint, and the bucket for a virtual-hosted
/// one. `None` when the host carries no `s3` label at all.
fn s3_endpoint_prefix(host: &str) -> Option<&str> {
    for (index, _) in host.match_indices("s3") {
        let (prefix, rest) = host.split_at(index);
        // The `s3` label must start a label of its own, and must be the whole
        // label or be followed by a `-<region>` suffix within it.
        if !(prefix.is_empty() || prefix.ends_with('.')) {
            continue;
        }
        let rest = &rest["s3".len()..];
        if rest.is_empty() || rest.starts_with('.') || rest.starts_with('-') {
            return Some(prefix.trim_end_matches('.'));
        }
    }
    None
}

/// One stored entity set. Timestamps are epoch milliseconds, the single source for
/// the instants the wire carries.
#[derive(Debug, Clone)]
pub struct EntitySetRecord {
    pub id: String,
    pub name: String,
    /// `TrustedEntitySetFormat`, stored as the create gave it.
    pub format: String,
    /// The S3 URL the list was uploaded to, stored verbatim.
    pub location: String,
    pub expected_bucket_owner: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// The `ClientToken` the create carried, so a replay returns this same set
    /// instead of minting a second one.
    pub client_token: Option<String>,
    /// `TrustedEntitySetStatus`.
    pub status: String,
    /// When the clock owes this set its next status, while one is owed. See
    /// [`ENTITY_SET_TRANSITION_MILLIS`].
    pub status_due_millis: Option<i64>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
    /// `ErrorDetails`: only an `ERROR` set carries one.
    pub error_details: Option<String>,
}

impl EntitySetRecord {
    /// An entity set created at `now`: the record BOTH entity-set creates store,
    /// so the trusted and the threat set cannot drift on what a create persists.
    /// It is the entity-set counterpart of [`IpSetRecord::created`].
    ///
    /// `Activate` is "a boolean value that indicates whether GuardDuty should
    /// start using the uploaded threat entity set to generate findings"
    /// (`smithy_query(query="doc",shape="CreateThreatEntitySetRequest",member="Activate")`):
    /// a set the caller asked to activate is not in use yet, so it starts in the
    /// transitional [`ENTITY_SET_ACTIVATING`] and the clock owes it
    /// [`ENTITY_SET_ACTIVE`] at [`ENTITY_SET_TRANSITION_MILLIS`], while one the
    /// caller did not ask to activate is stored unused — [`ENTITY_SET_INACTIVE`],
    /// which is settled, so nothing is owed on it until an update activates it.
    ///
    /// Nothing failed, so a created set carries no [`Self::error_details`].
    ///
    /// The bucket owner the record keeps is resolved through
    /// [`entity_set_bucket_owner`], so a set created without one still knows which
    /// account owns the location it was uploaded from.
    #[allow(clippy::too_many_arguments)]
    pub fn created(
        id: String,
        name: String,
        format: String,
        location: String,
        expected_bucket_owner: Option<String>,
        tags: BTreeMap<String, String>,
        activate: bool,
        client_token: Option<String>,
        now: i64,
    ) -> Self {
        let (status, status_due_millis) = if activate {
            (
                ENTITY_SET_ACTIVATING,
                Some(now.saturating_add(ENTITY_SET_TRANSITION_MILLIS)),
            )
        } else {
            (ENTITY_SET_INACTIVE, None)
        };
        Self {
            id,
            name,
            format,
            location,
            expected_bucket_owner: entity_set_bucket_owner(expected_bucket_owner.as_deref()),
            tags,
            client_token,
            status: status.to_string(),
            status_due_millis,
            created_at_millis: now,
            updated_at_millis: now,
            error_details: None,
        }
    }

    /// Apply the status change the CLOCK already made due, if any.
    ///
    /// The move out of [`ENTITY_SET_ACTIVATING`] or [`ENTITY_SET_DEACTIVATING`] was
    /// owed at [`Self::status_due_millis`] whether or not anybody called, which is
    /// why a reporting operation may apply it. `UpdatedAt` records the instant it was
    /// OWED, not the instant somebody looked, so two reads of a settled set agree.
    pub fn settle(&mut self, now: i64) {
        let Some(due) = self.status_due_millis else {
            return;
        };
        if now < due {
            return;
        }
        // Only a transitional status is owed a move, and each one settles to its
        // own status, so a status this function does not know stays where it is.
        self.status = match self.status.as_str() {
            ENTITY_SET_ACTIVATING => ENTITY_SET_ACTIVE.to_string(),
            ENTITY_SET_DEACTIVATING => ENTITY_SET_INACTIVE.to_string(),
            other => other.to_string(),
        };
        self.status_due_millis = None;
        self.updated_at_millis = due;
    }

    /// Start the transition the `Activate` an UPDATE carries asks for, from
    /// whatever status this set currently has: [`ENTITY_SET_ACTIVATING`] then
    /// [`ENTITY_SET_ACTIVE`] when the caller asks GuardDuty to start using the
    /// set, [`ENTITY_SET_DEACTIVATING`] then [`ENTITY_SET_INACTIVE`] when it asks
    /// it to stop. It is the entity-set counterpart of
    /// [`ThreatIntelSetRecord::set_activation`], shared by both entity-set
    /// updates so the trusted and the threat set cannot drift on what `Activate`
    /// does.
    ///
    /// Asking for the status the set has already settled on, or is already
    /// transitioning to, owes nothing and answers `false`, so a repeated request
    /// leaves the status and the instant the clock owes it exactly where they are
    /// rather than re-opening a transition. Asking for the other one replaces
    /// whatever was owed, so a request that reverses a transition still running
    /// turns it around instead of queueing behind it.
    ///
    /// The transition is never immediate: the clock owes the settled status
    /// [`ENTITY_SET_TRANSITION_MILLIS`] later and only [`Self::settle`]
    /// materialises it, so a caller reading straight afterwards still observes
    /// the transitional status.
    pub fn set_activation(&mut self, activate: bool, now: i64) -> bool {
        if !self.activation_changes(activate) {
            return false;
        }
        let transitional = if activate {
            ENTITY_SET_ACTIVATING
        } else {
            ENTITY_SET_DEACTIVATING
        };
        self.status = transitional.to_string();
        self.status_due_millis = Some(now.saturating_add(ENTITY_SET_TRANSITION_MILLIS));
        true
    }

    /// Would [`Self::set_activation`] move anything — does `activate` ask for a
    /// status this set has neither settled on nor already started towards?
    ///
    /// Answered WITHOUT writing, so a caller can decide whether its request
    /// changes this record at all BEFORE it reads the store's clock. The answer
    /// does not depend on whether [`Self::settle`] has run yet: settling only ever
    /// moves a status to the other member of the same pair.
    pub fn activation_changes(&self, activate: bool) -> bool {
        let (settled, transitional) = if activate {
            (ENTITY_SET_ACTIVE, ENTITY_SET_ACTIVATING)
        } else {
            (ENTITY_SET_INACTIVE, ENTITY_SET_DEACTIVATING)
        };
        self.status != settled && self.status != transitional
    }
}

/// Serve the deletion of the entity set `id` names within `sets`, one of a
/// detector's entity-set maps, reporting whether such a set was there to delete.
///
/// Shared by BOTH entity-set deletes, so the trusted and the threat set cannot
/// drift on what a delete does: the record is DROPPED, so from that instant the
/// detector holds no entity set under that id — its listing stops naming it, its
/// own read is refused as absent, a replayed `ClientToken` mints a new set rather
/// than handing this one back, and a second delete has nothing left to delete.
///
/// Dropping the record is also what BOUNDS this store under create-and-delete
/// churn: a deletion gives a slot back instead of leaving a record behind, so no
/// retention allowance is needed and no caller can grow the map by deleting.
/// It is the entity sets' counterpart of what a filter's and a publishing
/// destination's delete does, and it differs from the uploaded lists (an IPSet and
/// a threat list), whose record is retained so their own read can report the
/// terminal status of the status enum.
pub fn delete_entity_set(sets: &mut BTreeMap<String, EntitySetRecord>, id: &str) -> bool {
    sets.remove(id).is_some()
}
