// SPDX-License-Identifier: CC0-1.0

//! The account's resources a resource query is RESOLVED against.
//!
//! A group whose definition is a `ResourceQuery` holds no membership of its own:
//! the resources it contains are the ones the query selects out of the account's
//! resources, which is why `ListGroupResources` documents
//! `tag:GetResources` among its required permissions
//! (`smithy_query(query="doc",shape="ListGroupResources")`) — the membership of a
//! tag query is the tagged resources of the account, resolved through the
//! resource-tagging view of them rather than out of this service's own records.
//!
//! This module is that view, and the ONE place this crate reaches it: the
//! resource-tagging service already projects every tagged resource of a region
//! out of the service that OWNS it (a queue out of Amazon SQS, a key out of AWS
//! KMS, …) plus the resources tagged through its own API, so a query runs over
//! that projection instead of a second copy assembled here. A resource an owning
//! service no longer holds is absent from the projection, so it leaves a group's
//! membership with nothing to keep in step.
//!
//! Reading the projection locks the owning services' stores, so a caller reads it
//! BEFORE taking this service's own store lock: the two are then never locked in
//! two orders.
//!
//! What one request costs is bounded by the emulator's own state rather than by the
//! request: the projection holds one row per TAGGED resource the services of this
//! region hold, each of which exists only because some service's own create — itself
//! bounded by that service's limits — recorded it, and the query matched against it
//! is decoded once by `serde_json` ([`super::query_document`], which returns the
//! refusal message rather than panicking) out of a `Query` the model bounds to 4096
//! characters, with the entries one request may carry bounded before any of this
//! runs ([`super::check_filters`]). No caller can make one resolution walk more than
//! the account already holds.

use std::collections::BTreeMap;

// The resource-tagging service, aliased to the role it plays here: the account's
// view of its tagged resources, which is what a tag query selects out of.
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging;

/// One resource of the account a query can select: its ARN and the tags it
/// carries.
///
/// The type a membership reports is NOT carried here: it is derived from the ARN
/// in the one place this service derives it ([`super::resource_type_of_arn`]), so
/// a resource reported from a query and one reported from a recorded membership
/// are named in the same vocabulary.
#[derive(Debug, Clone)]
pub struct TaggedResource {
    /// The resource's ARN, as the service that owns it minted it.
    pub resource_arn: String,
    /// The resource's tags, key -> value. Ordered, so one read of the projection
    /// is stable.
    pub tags: BTreeMap<String, String>,
}

/// Every tagged resource `region` holds, as a query selects over them.
///
/// Empty for an engine that carries no such projection at all, which is the same
/// answer an account with no tagged resource gives: a query then selects among
/// this service's own records alone.
pub fn tagged_resources<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
) -> Vec<TaggedResource> {
    // What the owning services themselves hold tags for, read BEFORE the tagging
    // store is locked, exactly as that service's own readers read it.
    let owned = tagging::state::owners::tagged_resources(engine, region);
    let Some(state) = tagging::state::service_state(engine) else {
        return Vec::new();
    };
    // The resources of this region: the owners' own tag sets merged with the ones
    // recorded against an ARN no owning service answers for, each resource once.
    let view = state.lock();
    view.resources_in_region(owned, region)
        .into_iter()
        .map(|entry| TaggedResource {
            resource_arn: entry.arn,
            tags: entry.tags,
        })
        .collect()
}
