// SPDX-License-Identifier: CC0-1.0

//! The account's BILLING GROUPS: the record one holds, and what every reader of one
//! reports.
//!
//! A billing group is a NAMED grouping of things "used to collect billing information"
//! for them, and the name is the group's identity — the model binds it as the
//! `@httpLabel` of `/billing-groups/{billingGroupName}` on every single-group operation
//! (`smithy_query(query="operation",operation="CreateBillingGroup,DescribeBillingGroup")`)
//! — which is why the store this module's record lives in
//! ([`ComAmazonawsIotAwsiotserviceState::billing_groups`]) is a `BTreeMap` keyed by it:
//! the map IS the registry. `BTreeMap` (not `HashMap`) because `ListBillingGroups` is
//! `@paginated` and a page boundary is only meaningful over a stable order.
//!
//! One record carries everything a reader hands back, so `DescribeBillingGroup` after
//! `CreateBillingGroup` reports the created group member for member: the reader clones
//! what the writer stored and the two cannot drift. `CreateThing` resolves its
//! `billingGroupName` against the same records ([`super::ComAmazonawsIotAwsiotserviceState::register_thing`]),
//! so a thing may only name a group this store holds.
//!
//! A LATER write of one record goes through the single registry write this module ends with
//! ([`ComAmazonawsIotAwsiotserviceState::update_billing_group`]), which applies the
//! properties the request carried and advances the record's `version` — so the reader
//! reports what the last writer left, not the creation-time snapshot.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    billing_group_version_conflict_message, fresh_uuid, missing_billing_group_message,
    ComAmazonawsIotAwsiotserviceState,
};
use crate::ops::types::{BillingGroupMetadata, BillingGroupProperties};

/// How many billing groups this emulator keeps for the account.
///
/// The store is one a CALLER grows — one record per `CreateBillingGroup` — so it is
/// bounded, for the same reason [`super::MAX_THING_TYPES`] is: the model carries no
/// figure, so this is the emulator's own ceiling on a store a request can grow, and a
/// create past it is refused with a modelled error rather than growing it without bound.
pub const MAX_BILLING_GROUPS: usize = 1_000;

/// How many tags one billing group may carry: the same documented IoT tagging limit
/// [`super::MAX_POLICY_TAGS`] records, for the same reason — `TagList` has no modelled
/// maximum, so without a ceiling one request would choose how large a single record is.
pub const MAX_BILLING_GROUP_TAGS: usize = super::MAX_POLICY_TAGS;

/// The `version` a freshly created billing group carries.
///
/// `DescribeBillingGroup$version` is `@required` ("The version of the billing group") and
/// is what a caller passes as `expectedVersion` to the operations that take one. A record
/// that exists has had one write (its create), so its version is 1 — the same rule
/// [`super::FIRST_THING_GROUP_VERSION`] states for a thing group.
pub const FIRST_BILLING_GROUP_VERSION: i64 = 1;

/// The ARN of a billing group:
/// `arn:aws:iot:<region>:<account>:billinggroup/<billingGroupName>`.
///
/// `billinggroup` (one word, lowercase) is the resource type AWS IoT's billing-group ARNs
/// use, beside `thinggroup/` for a thing group ([`super::thing_group_arn`]); the namespace
/// and account segments are the same ones [`super::thing_arn`] documents.
pub fn billing_group_arn(region: &str, billing_group_name: &str) -> String {
    ::emulator_server::emulator_arn("iot", region, &format!("billinggroup/{billing_group_name}"))
}

/// Everything about a billing group that is NOT its name, in the form the registry keeps
/// it and COMPARES it.
///
/// It is a type of its own for the same reason [`super::ThingGroupConfiguration`] is:
/// `CreateBillingGroup` is documented idempotent on it — "If this call is made multiple
/// times using the same billing group name and configuration, the call will succeed. If
/// this call is made with the same billing group name but different configuration a
/// ResourceAlreadyExistsException is thrown" — so deciding a repeat create is a single
/// `==`. `BillingGroupProperties` has exactly one member
/// (`smithy_query(query="shape",shape="BillingGroupProperties")`), so the description is
/// the whole configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BillingGroupConfiguration {
    /// `BillingGroupProperties$billingGroupDescription` — "The description of the billing
    /// group" — when the writer supplied one.
    pub description: Option<String>,
}

impl From<Option<BillingGroupProperties>> for BillingGroupConfiguration {
    /// Properties the request left out, or properties carrying no description, are the
    /// configuration of a group that HAS no description, which is what the group then has.
    fn from(properties: Option<BillingGroupProperties>) -> Self {
        Self {
            description: properties.and_then(|properties| properties.billing_group_description),
        }
    }
}

/// One billing group in the registry, as this service persists it.
#[derive(Debug, Clone)]
pub struct RegisteredBillingGroup {
    /// The name the group was created under; also its key in the registry.
    pub billing_group_name: String,
    /// The id minted when the group was created — stable for its lifetime.
    pub billing_group_id: String,
    /// `arn:aws:iot:<region>:<account>:billinggroup/<billingGroupName>`
    /// ([`billing_group_arn`]), minted on the create.
    pub billing_group_arn: String,
    /// What the group is configured with; see [`BillingGroupConfiguration`].
    pub configuration: BillingGroupConfiguration,
    /// `Tag$Key` -> `Tag$Value` for the tags the create request carried, "Metadata which
    /// can be used to manage the billing group". Tags are NOT part of
    /// [`BillingGroupConfiguration`]: they manage the resource rather than describe the
    /// group, so they do not decide whether a repeat create is the same group. The tag
    /// plane reads and writes this same field ([`super::resource_tags`]), so the tags a
    /// create stored are the tags a read reports.
    pub tags: BTreeMap<String, Option<String>>,
    /// When the group was created — `BillingGroupMetadata$creationDate`, "The date the
    /// billing group was created".
    pub creation_date: SystemTime,
    /// The registry version of this record, at [`FIRST_BILLING_GROUP_VERSION`] until a
    /// later write moves it.
    pub version: i64,
}

impl RegisteredBillingGroup {
    /// A newly created billing group: the minted identity plus the requested configuration
    /// and tags, at [`FIRST_BILLING_GROUP_VERSION`].
    pub fn new(
        region: &str,
        billing_group_name: String,
        billing_group_id: String,
        configuration: BillingGroupConfiguration,
        tags: BTreeMap<String, Option<String>>,
        creation_date: SystemTime,
    ) -> Self {
        Self {
            billing_group_arn: billing_group_arn(region, &billing_group_name),
            billing_group_name,
            billing_group_id,
            configuration,
            tags,
            creation_date,
            version: FIRST_BILLING_GROUP_VERSION,
        }
    }

    /// The group's configuration as the `BillingGroupProperties` a reader reports: ONE
    /// function, so two readers cannot describe the same record differently.
    pub fn properties(&self) -> BillingGroupProperties {
        BillingGroupProperties {
            billing_group_description: self.configuration.description.clone(),
        }
    }

    /// The group's `BillingGroupMetadata` — "Additional information about the billing
    /// group", which is the date the record was created.
    pub fn metadata(&self) -> BillingGroupMetadata {
        BillingGroupMetadata {
            creation_date: Some(::aws_smithy_types::DateTime::from(self.creation_date)),
        }
    }
}

impl ComAmazonawsIotAwsiotserviceState {
    /// Mint a `billingGroupId` no billing group in the registry is using.
    ///
    /// `BillingGroupId` is modelled `length 1..128, pattern ^[a-zA-Z0-9\-]+$`
    /// (`smithy_query(query="operation",operation="CreateBillingGroup")`), which the
    /// canonical hyphenated form of a version-4 UUID satisfies — the same reading
    /// [`ComAmazonawsIotAwsiotserviceState::mint_thing_group_id`] makes of the thing-group
    /// id's identical constraints; the loop re-mints on the (astronomically unlikely)
    /// collision, so two `CreateBillingGroup` calls can never be handed the same id.
    pub fn mint_billing_group_id(&self) -> String {
        loop {
            let candidate = fresh_uuid();
            if !self
                .billing_groups
                .values()
                .any(|group| group.billing_group_id == candidate)
            {
                return candidate;
            }
        }
    }
}

/// What an update asks the registry to change about one billing group.
///
/// `UpdateBillingGroup$billingGroupProperties` is `@required`
/// (`smithy_query(query="operation",operation="UpdateBillingGroup")`), so what the request
/// carries IS the group's properties afterwards: properties carrying no description leave
/// the group without one, the same reading [`super::ThingGroupUpdate`] makes of its own
/// `@required` properties member.
#[derive(Debug, Clone)]
pub struct BillingGroupUpdate {
    /// `BillingGroupProperties$billingGroupDescription`, as the request carried it.
    pub description: Option<String>,
    /// `UpdateBillingGroup$expectedVersion` — the version the caller believes the record is
    /// at. "If the version of the billing group does not match the expected version
    /// specified in the request, the UpdateBillingGroup request is rejected with a
    /// VersionConflictException."
    pub expected_version: Option<i64>,
}

/// Why a billing group record cannot take an update — what
/// [`ComAmazonawsIotAwsiotserviceState::update_billing_group`] answers instead of the
/// updated record.
///
/// It is the registry's reason, not a wire error: the handler maps it onto the error its
/// own model declares, and the wording lives with the family's other sentences so every
/// writer states one rule once — the shape [`super::ThingGroupUpdateRefusal`] takes.
#[derive(Debug, Clone)]
pub enum BillingGroupUpdateRefusal {
    /// `billingGroupName` names no billing group in the registry.
    MissingBillingGroup(String),
    /// `expectedVersion` does not match the version the record holds, so the caller is
    /// updating a record it has not seen.
    ///
    /// The two versions are carried for DIAGNOSTICS alone (a `Debug` rendering of why the
    /// write was refused): this family's refusal sentence reports neither of them, only the
    /// group the request addressed.
    VersionConflict {
        /// The group the request addressed.
        billing_group_name: String,
        /// The version the request expected.
        expected: i64,
        /// The version the registry holds.
        registered: i64,
    },
}

impl BillingGroupUpdateRefusal {
    /// What the refusal reports, in the wording this service states each of these in.
    pub fn message(&self) -> String {
        match self {
            Self::MissingBillingGroup(billing_group_name) => {
                missing_billing_group_message(billing_group_name)
            }
            Self::VersionConflict {
                billing_group_name, ..
            } => billing_group_version_conflict_message(billing_group_name),
        }
    }
}

impl ComAmazonawsIotAwsiotserviceState {
    /// Apply `update` to the billing group registered as `billing_group_name`: the ONE
    /// registry write that changes a billing group record, answering the record as it
    /// stands afterwards.
    ///
    /// Everything that can refuse the update is decided BEFORE the record is touched, so a
    /// refused update leaves the group — and its version — exactly as it was, which is the
    /// point of `expectedVersion`. A successful one advances the record's `version`, which
    /// is what a later reader reports and a later `expectedVersion` has to match.
    pub fn update_billing_group(
        &mut self,
        billing_group_name: &str,
        update: BillingGroupUpdate,
    ) -> Result<RegisteredBillingGroup, BillingGroupUpdateRefusal> {
        let group = self
            .billing_groups
            .get_mut(billing_group_name)
            .ok_or_else(|| {
                BillingGroupUpdateRefusal::MissingBillingGroup(billing_group_name.to_string())
            })?;
        if let Some(expected) = update.expected_version {
            if group.version != expected {
                return Err(BillingGroupUpdateRefusal::VersionConflict {
                    billing_group_name: group.billing_group_name.clone(),
                    expected,
                    registered: group.version,
                });
            }
        }

        // The group's identity — its name, the id and the ARN minted on the create — is not
        // part of an update: `UpdateBillingGroup` carries none of them, so the record keeps
        // what it was created with. Its tags are not either: they are managed by the tag
        // plane ([`RegisteredBillingGroup::tags`]).
        group.configuration.description = update.description;
        // One update is one write of the record, so the registry version it carries advances
        // by one — which is what a caller holding the old version then conflicts against.
        group.version += 1;
        Ok(group.clone())
    }
}

/// Why a billing group record cannot be REMOVED — what
/// [`ComAmazonawsIotAwsiotserviceState::delete_billing_group`] answers instead of removing it.
///
/// There is exactly one reason, because the delete models no not-found error: a name the
/// registry does not hold is the state the caller asked for, so the only way the removal is
/// refused is the optimistic concurrency the versioned record is written under — the same
/// reason [`BillingGroupUpdateRefusal::VersionConflict`] states for a write of one.
#[derive(Debug, Clone)]
pub enum BillingGroupDeleteRefusal {
    /// `expectedVersion` does not match the version the record holds, so the caller is
    /// deleting a record it has not seen.
    ///
    /// The two versions are carried for DIAGNOSTICS alone, for the reason
    /// [`BillingGroupUpdateRefusal::VersionConflict`] carries them: this family's refusal
    /// sentence reports neither of them, only the group the request addressed.
    VersionConflict {
        /// The group the request addressed.
        billing_group_name: String,
        /// The version the request expected.
        expected: i64,
        /// The version the registry holds.
        registered: i64,
    },
}

impl BillingGroupDeleteRefusal {
    /// What the refusal reports, in the wording this service states it in — the versioned
    /// families' shared conflict sentence with this family's noun.
    pub fn message(&self) -> String {
        match self {
            Self::VersionConflict {
                billing_group_name, ..
            } => billing_group_version_conflict_message(billing_group_name),
        }
    }
}

impl ComAmazonawsIotAwsiotserviceState {
    /// Remove the billing group registered as `billing_group_name` from the registry: the ONE
    /// registry write that makes a billing group stop existing, answering the record it
    /// removed, or `None` when the registry held no group under that name.
    ///
    /// A name the registry does not hold removes nothing and is NOT a refusal: the delete
    /// models no not-found error at all
    /// (`smithy_query(query="operation",operation="DeleteBillingGroup")` lists
    /// InternalFailure, InvalidRequest, Throttling and VersionConflict), so it is idempotent
    /// and a repeat of it leaves the registry in the state the caller asked for.
    ///
    /// `expectedVersion` is read BEFORE anything is removed, so a refused delete leaves the
    /// group — and its version — exactly as it was, which is the point of the parameter.
    ///
    /// Membership is a relation that only exists while the group does: a thing's
    /// `billingGroupName` names a group this registry holds
    /// ([`ComAmazonawsIotAwsiotserviceState::register_thing`] refuses one it does not), so the
    /// removal also clears that name off every thing that carried it rather than leaving a
    /// reader of the thing reporting a group nothing holds. A thing's own registry version is
    /// NOT advanced by it: the write was to the group, not to the thing.
    pub fn delete_billing_group(
        &mut self,
        billing_group_name: &str,
        expected_version: Option<i64>,
    ) -> Result<Option<RegisteredBillingGroup>, BillingGroupDeleteRefusal> {
        let Some(group) = self.billing_groups.get(billing_group_name) else {
            return Ok(None);
        };
        if let Some(expected) = expected_version {
            if group.version != expected {
                return Err(BillingGroupDeleteRefusal::VersionConflict {
                    billing_group_name: group.billing_group_name.clone(),
                    expected,
                    registered: group.version,
                });
            }
        }

        // The name is the group's identity and its key in the registry, so removing that entry
        // is what makes every later reader — the describe path, the listings, and a
        // `CreateThing` resolving a `billingGroupName` — stop seeing it.
        let removed = self.billing_groups.remove(billing_group_name);
        for thing in self.things.values_mut() {
            if thing.configuration.billing_group_name.as_deref() == Some(billing_group_name) {
                thing.configuration.billing_group_name = None;
            }
        }
        Ok(removed)
    }
}
