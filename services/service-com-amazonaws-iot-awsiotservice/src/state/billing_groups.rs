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

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{fresh_uuid, ComAmazonawsIotAwsiotserviceState};
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
