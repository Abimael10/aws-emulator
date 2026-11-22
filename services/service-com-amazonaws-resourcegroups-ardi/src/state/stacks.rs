// SPDX-License-Identifier: CC0-1.0

//! The account's CloudFormation stacks a `CLOUDFORMATION_STACK_1_0` query is
//! RESOLVED against.
//!
//! A group whose definition names a stack holds no membership of its own: the
//! resources it contains are the ones that STACK contains, which is why
//! `ListGroupResources` and `SearchResources` both document
//! `cloudformation:DescribeStacks` and `cloudformation:ListStackResources` among
//! their required permissions
//! (`smithy_query(query="doc",shape="ListGroupResources")`) — the membership of a
//! stack query is the stack's own resources, read out of the service that holds
//! the stack rather than out of this service's records.
//!
//! This module is that view, and the ONE place this crate reaches it: the
//! CloudFormation service records, per stack it holds, the resources it installed
//! and the identifiers the services that OWN those resources minted for them, so a
//! stack query runs over that record instead of a second copy assembled here. A
//! stack CloudFormation no longer holds is absent from the view, so a group naming
//! it resolves no member and reports the modelled query error instead.
//!
//! Reading the view locks the CloudFormation store, so a caller reads it BEFORE
//! taking this service's own store lock: the two are then never locked in two
//! orders. The whole region is read rather than one named stack, because WHICH
//! stack a listing resolves is known only from this service's store — the group's
//! own query — and resolving that first would mean locking this store, dropping it
//! and resolving the group a second time.
//!
//! What one request costs is bounded by the emulator's own state rather than by the
//! request: the view holds one row per stack of the region, each carrying the
//! resources that stack installed, both of which exist only because some
//! CloudFormation operation — itself bounded by that service's own quotas —
//! recorded them.

use std::collections::BTreeMap;

// The CloudFormation service, aliased to the role it plays here: the account's
// stacks, which is what a stack query selects out of.
use service_com_amazonaws_cloudformation_cloudformation as cloudformation;

/// The ARN prefix every Amazon Resource Name starts with, which is what
/// distinguishes a physical resource id that IS an ARN (a topic's) from one that is
/// not (a queue's url, a table's name).
const ARN_PREFIX: &str = "arn:";

/// One resource of a stack a query can select: the ARN the service that owns it
/// minted, and the CloudFormation-style type the stack records for it.
#[derive(Debug, Clone)]
pub struct StackResource {
    /// The resource's ARN, as the service that owns it minted it.
    pub resource_arn: String,
    /// The resource's type, as the stack records it — `AWS::SNS::Topic` and the
    /// rest of the vocabulary `ResourceIdentifier$ResourceType` is documented in
    /// ("such as AWS::EC2::Instance"), which is the same vocabulary the stack's own
    /// `StackResource$ResourceType` is written in.
    pub resource_type: String,
}

/// One stack of the account, as a stack query addresses it.
#[derive(Debug, Clone)]
pub struct StackRecord {
    /// The stack's own id, which is its ARN.
    pub stack_id: String,
    /// The name the stack was created under.
    pub stack_name: String,
    /// Whether the stack is still part of the LIVE set — the set a stack NAME
    /// resolves within. A stack whose deletion has completed is addressable by its
    /// id alone, exactly as CloudFormation addresses one.
    pub live: bool,
    /// The resources the stack holds right now, each with an ARN its owning service
    /// minted.
    pub resources: Vec<StackResource>,
}

/// Every stack `region` holds, as a stack query addresses them.
///
/// Empty for an engine that carries no CloudFormation store at all, which is the
/// same answer an account with no stack gives: a stack query then resolves no
/// stack.
pub fn stacks_in<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) -> Vec<StackRecord> {
    let Some(state) = cloudformation::state::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    store
        .stacks_in(region)
        .map(|stack| StackRecord {
            stack_id: stack.stack_id.clone(),
            stack_name: stack.stack_name.clone(),
            live: stack.is_live(),
            resources: held_resources(stack),
        })
        .collect()
}

/// The stack `identifier` names among `stacks`, or `None` when the account holds
/// none under it.
///
/// Addressed exactly as CloudFormation addresses a stack: an id (the stack's ARN)
/// names the stack whatever became of it, while a NAME names only a live one,
/// since a name is reusable once the stack holding it is gone.
pub fn resolve<'stacks>(
    stacks: &'stacks [StackRecord],
    identifier: &str,
) -> Option<&'stacks StackRecord> {
    stacks
        .iter()
        .find(|stack| stack.stack_id == identifier)
        .or_else(|| {
            stacks
                .iter()
                .find(|stack| stack.live && stack.stack_name == identifier)
        })
}

/// The resources `stack` HOLDS right now, each reported under the ARN its owning
/// service minted for it, in ARN order and once each.
///
/// A record the stack kept for a resource it has already disposed of is not a
/// resource the stack holds, and a resource no ARN can be read for is not one this
/// service can report at all ([`resource_arn`]) — the modelled
/// `ResourceIdentifier$ResourceArn` is an ARN, and an identifier nothing minted is
/// what a client must never be handed.
fn held_resources(stack: &cloudformation::state::StoredStack) -> Vec<StackResource> {
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    for deployed in &stack.resources {
        if deployed.status == cloudformation::state::resource_status::DELETE_COMPLETE {
            continue;
        }
        if let Some(arn) = resource_arn(&deployed.resource) {
            held.insert(arn, deployed.resource.resource_type.clone());
        }
    }
    held.into_iter()
        .map(|(resource_arn, resource_type)| StackResource {
            resource_arn,
            resource_type,
        })
        .collect()
}

/// The ARN of one resource a stack installed, or `None` where nothing the stack
/// recorded for it is one.
///
/// The physical id where that is the ARN — which it is for every type whose
/// published return value is an ARN (an `AWS::SNS::Topic`'s topic ARN) — else the
/// ARN the resource's own attributes carry: a stack records the values the owning
/// service publishes for the resource, and that service's ARN is published under
/// the attribute names ending in `Arn` or `ARN` (`Arn`, `<Type>Arn`, `ARN`,
/// `<Type>ARN`). The unqualified `Arn` is preferred, so a type publishing several
/// spellings of the one ARN is read through the one every such type carries.
fn resource_arn(resource: &cloudformation::state::StoredResource) -> Option<String> {
    if resource.physical_resource_id.starts_with(ARN_PREFIX) {
        return Some(resource.physical_resource_id.clone());
    }
    let published = |name: &str, value: &String| {
        (name.ends_with("Arn") || name.ends_with("ARN")) && value.starts_with(ARN_PREFIX)
    };
    resource
        .attributes
        .get("Arn")
        .filter(|value| value.starts_with(ARN_PREFIX))
        .or_else(|| {
            resource
                .attributes
                .iter()
                .find(|(name, value)| published(name, value))
                .map(|(_, value)| value)
        })
        .cloned()
}
