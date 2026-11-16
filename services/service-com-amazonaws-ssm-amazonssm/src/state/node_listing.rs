// SPDX-License-Identifier: CC0-1.0

//! The node listing of the Systems Manager INTEGRATED EXPERIENCE: whether an account has
//! it at all, which Regions' nodes one listing reports, and the `Node` a stored managed
//! node is projected as.
//!
//! `ListNodes` "takes in filters and returns a list of managed nodes matching the filter
//! criteria", and the nodes it lists are the ones the node store already holds
//! ([`super::ManagedNodeRecord`], keyed by `(region, node id)`) — the same records
//! `DescribeInstanceInformation` and `DescribeInstanceProperties` report and
//! [`super::inventory`] projects as a node's `AWS:InstanceInformation` item. This module is
//! the projection and the selection, so a node listing reports what registering persisted
//! rather than a second copy of it.
//!
//! ## Whether the account has the listing at all
//!
//! The listing belongs to the integrated experience — the Systems Manager experience an
//! account OPTS INTO — and an account that has not enabled it is answered
//! `UnsupportedOperationException`: "This operation is not supported for the current
//! account. You must first enable the Systems Manager integrated experience in your
//! account" ([`NOT_ENABLED_MESSAGE`]). Enabling it is what puts the Amazon Web Services
//! managed node resource data sync in the account: "the name of the supported resource data
//! sync is AWS-QuickSetup-ManagedNode" (`ListNodesRequest$SyncName`,
//! [`MANAGED_NODE_SYNC_NAME`]). So the decision is taken from that sync
//! ([`super::ComAmazonawsSsmAmazonssmState::has_integrated_experience`]): a fresh account,
//! which holds no sync at all, has no node listing, and an account that creates the managed
//! node sync has it at the endpoint the sync was created through — a sync is a REGIONAL
//! record like every other of this service ([`super::resource_data_syncs`]), so the decision
//! is per Region.
//!
//! ## Which nodes one listing reports
//!
//! `SyncName` is "the name of the Amazon Web Services managed resource data sync to retrieve
//! information about. For cross-account/cross-Region configurations, this parameter is
//! required … For single account/single-Region configurations, the parameter is not
//! required" — so a listing that names no sync reports the nodes of the Region it was called
//! at, and one that names the managed node sync reports the nodes of the Regions that sync
//! aggregates ([`ComAmazonawsSsmAmazonssmState::node_listing_regions`], the sync's own
//! `SyncSource$SourceRegions`). A name other than the supported one names no sync a node
//! listing can read, which is what `ResourceDataSyncNotFoundException` reports.
//!
//! The account column of a node is this emulator's one account
//! ([`::engine::emulator_account_id`]) and no organization record stands behind it, so a
//! node's `Owner` carries that account and neither organizational unit member — the keys
//! that select by one ([`filter_values`]) therefore select no node, rather than a node being
//! reported under an organizational unit nothing placed it in.

use crate::ops::types::{InstanceInfo, Node, NodeFilter, NodeOwnerInfo, NodeType};

use super::{ComAmazonawsSsmAmazonssmState, ManagedNodeRecord};

/// The name of the resource data sync the Systems Manager integrated experience puts in an
/// account: "For cross-account/cross-Region configurations, this parameter is required, and
/// the name of the supported resource data sync is AWS-QuickSetup-ManagedNode"
/// (`ListNodesRequest$SyncName`, and the same text on `ListNodesSummaryRequest$SyncName`).
///
/// It is the one sync name a node listing knows: the sync is Amazon Web Services' own rather
/// than one a caller invents a name for.
pub const MANAGED_NODE_SYNC_NAME: &str = "AWS-QuickSetup-ManagedNode";

/// How a node listing reports a call made against an account that has not enabled the
/// Systems Manager integrated experience, through `UnsupportedOperationException`.
///
/// The whole sentence is this error class's own fixed text and interpolates nothing from the
/// request, so neither the filters nor the sync name a caller submitted appear in it.
///
/// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, check
/// `ssm.list-nodes-reflects-managed-instances`): a `ListNodes` on an account without the
/// experience is answered 400 `UnsupportedOperationException` with exactly this sentence —
/// which is how the service words it, rather than how the error shape's own documentation
/// paraphrases it ("This operation is not supported for the current account. …").
pub const NOT_ENABLED_MESSAGE: &str = "This is an unsupported operation for this account. \
                                       You must first enable the Systems Manager integrated \
                                       experience in your account.";

/// The lower bound of the modelled `MaxResults` of a node listing (`range 1..50`).
pub const MIN_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member, which is also the modelled `NodeList` length
/// (`len 0..50`) and therefore the page a request naming no `MaxResults` is served up to
/// (see [`super::page_size`]).
pub const MAX_PAGE_SIZE: i32 = 50;

/// The `ManagedStatus` of a node Systems Manager manages, from the modelled `ManagedStatus`
/// enum (`crate::ops::types::MANAGED_STATUS_VALUES`). Every node the node store holds
/// registered with Systems Manager, so every node it reports is in this status.
pub const MANAGED_STATUS_MANAGED: &str = "Managed";

/// The `ManagedStatus` value of the same enum that covers managed and unmanaged nodes
/// alike, which is why every node answers a filter for it ([`filter_values`]).
pub const MANAGED_STATUS_ALL: &str = "All";

/// The character a page cursor puts between the Region a node is held under and the node's
/// own id. A Region name carries only `[A-Za-z0-9-]` ([`super::region_is_well_formed`]) and
/// a node id only `[-\w]` (the modelled `NodeId` pattern), so it splits a cursor exactly
/// once.
const CURSOR_SEPARATOR: char = '/';

/// How many Regions one node listing walks.
///
/// Emulator resource guard, NOT a published AWS quota: the Regions a listing aggregates come
/// from the named sync's `SourceRegions`, whose length the model bounds nowhere, so without a
/// ceiling one request could make this service settle registrations and walk the node store
/// once per entry of a list a caller chose. It is more Regions than AWS itself has — the same
/// reading [`super::service_settings::MAX_SERVICE_SETTING_REGIONS`] takes of a Region-keyed
/// store — so no sync naming Regions that exist is narrowed by it.
pub const MAX_LISTED_REGIONS: usize = 64;

/// The page size a node listing serves (see [`super::page_size`]).
pub fn page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(max_results, MIN_PAGE_SIZE, MAX_PAGE_SIZE)
}

/// The `NextToken` a page hands out to resume at the node `node_id` names in `region`.
///
/// A listing walks `(region, node id)` pairs in ascending order, so the position it resumes
/// at is that pair and not an offset into a list that may have changed between two pages.
pub fn page_token(region: &str, node_id: &str) -> String {
    format!("{region}{CURSOR_SEPARATOR}{node_id}")
}

/// The `(region, node id)` position a `NextToken` resumes at, or `None` for a value this
/// service did not hand out — which is what `InvalidNextToken` reports.
///
/// A token is one of ours when it is a Region name and a well-formed node id
/// ([`super::managed_node_id_is_well_formed`], the same shape every node read of this
/// service recognises its own cursor by).
pub fn page_cursor(token: &str) -> Option<(&str, &str)> {
    let (region, node_id) = token.split_once(CURSOR_SEPARATOR)?;
    (super::region_is_well_formed(region) && super::managed_node_id_is_well_formed(node_id))
        .then_some((region, node_id))
}

/// The values of `node`, held in `region`, that a filter on `key` is matched against; EMPTY
/// for a key the record holds no value for, so a filter on it selects no node.
///
/// `NodeFilter$Key` is the modelled `NodeFilterKey` enum
/// (`crate::ops::types::NODE_FILTER_KEY_VALUES`), which the centralized request walk refuses
/// anything outside of before a handler runs, and every key of it names one attribute of a
/// node: the attributes of the stored record ([`super::ManagedNodeRecord::properties`]), the
/// Region the record is held under, or the account that owns it. The two the record holds
/// nothing for are stated here rather than guessed:
///
/// * `AgentType` — nothing records WHICH agent implementation reported, the same position
///   [`super::inventory`] takes of that attribute of the `AWS:InstanceInformation` item.
/// * `AvailabilityZoneId` — a node reports the zone it runs in by NAME
///   (`InstanceProperty$AvailabilityZone`, which is what Amazon EC2's own instance record
///   carries); nothing holds that zone's id.
///
/// `InstanceStatus` is the node's own state (`InstanceProperty$InstanceState`, "the current
/// state of the node"), the same attribute the inventory item reports under that name.
pub fn filter_values<'a>(
    node: &'a ManagedNodeRecord,
    region: &'a str,
    key: &str,
) -> Vec<&'a str> {
    fn one(value: &Option<String>) -> Vec<&str> {
        value.as_deref().into_iter().collect()
    }
    let held = &node.properties;
    match key {
        "AccountId" => vec![::engine::emulator_account_id()],
        "AgentType" => Vec::new(),
        "AgentVersion" => one(&held.agent_version),
        "AvailabilityZone" => one(&held.availability_zone),
        "AvailabilityZoneId" => Vec::new(),
        "ComputerName" => one(&held.computer_name),
        "InstanceId" => one(&held.instance_id),
        "InstanceStatus" => one(&held.instance_state),
        "IpAddress" => one(&held.ip_address),
        // Every node held here is managed, and `All` is the value that covers managed and
        // unmanaged nodes alike, so a node answers a filter for either.
        "ManagedStatus" => vec![MANAGED_STATUS_MANAGED, MANAGED_STATUS_ALL],
        "OrganizationalUnitId" | "OrganizationalUnitPath" => Vec::new(),
        "PlatformName" => one(&held.platform_name),
        "PlatformType" => one(&held.platform_type),
        "PlatformVersion" => one(&held.platform_version),
        "Region" => vec![region],
        "ResourceType" => one(&held.resource_type),
        "SourceId" => one(&held.source_id),
        "SourceLocation" => one(&held.source_location),
        "SourceType" => one(&held.source_type),
        // A key outside the modelled enum names no attribute of a node; the walk has
        // already refused one, so nothing reaches here to be matched.
        _ => Vec::new(),
    }
}

/// Whether `node`, held in `region`, satisfies one filter: any value it holds for the
/// filter's key against any of the filter's values, under the filter's operator.
///
/// A filter's values UNION and the operator is the modelled `NodeFilterOperatorType`
/// (`Equal`, `NotEqual`, `BeginWith`), compared through this service's one comparison of a
/// filter value against a held one ([`super::operator_matches`]); a filter that names no
/// operator compares for equality, the relation a key and a value state on their own
/// ([`super::OPERATOR_EQUAL`]).
pub fn matches_filter(node: &ManagedNodeRecord, region: &str, filter: &NodeFilter) -> bool {
    let operator = filter.r#type.as_deref().unwrap_or(super::OPERATOR_EQUAL);
    filter_values(node, region, &filter.key).iter().any(|held| {
        filter
            .values
            .iter()
            .any(|wanted| super::operator_matches(operator, held, wanted))
    })
}

/// When `node` last reported what it holds: the last time it pinged the service, and failing
/// that the day it registered.
///
/// It comes from the node record rather than from the clock, so two reads of one node report
/// the same instant. THE ONE reading of that across this service: the `CaptureTime` of a node
/// listing and the `CaptureTime` of the node's inventory item
/// ([`super::inventory::capture_time`], which renders this instant) are the same instant,
/// because both are when the node last said what it holds.
pub fn captured_at(node: &ManagedNodeRecord) -> Option<::aws_smithy_types::DateTime> {
    node.properties
        .last_ping_date_time
        .or(node.properties.registration_date)
}

/// The modelled `Node` for the managed node `node`, held in `region` — the stored record
/// projected through the members a node listing answers with.
///
/// Every member is copied from what registering persisted, from the Region the record is
/// held under or from the account this emulator serves; the ones nothing holds a value for
/// are left unset rather than invented (see [`filter_values`]).
pub fn to_node(region: &str, node: &ManagedNodeRecord) -> Node {
    Node {
        capture_time: captured_at(node),
        id: node.properties.instance_id.clone(),
        owner: Some(NodeOwnerInfo {
            account_id: Some(::engine::emulator_account_id().to_string()),
            // No organization record stands behind this account, so the node is under no
            // organizational unit (see the module docs).
            organizational_unit_id: None,
            organizational_unit_path: None,
        }),
        region: Some(region.to_string()),
        // `NodeType` is a union whose one member is the instance a managed node IS.
        node_type: Some(NodeType::Instance(instance_info(node))),
    }
}

/// The `InstanceInfo` of one managed node: the attributes the stored record reports about
/// the instance behind it.
fn instance_info(node: &ManagedNodeRecord) -> InstanceInfo {
    let held = &node.properties;
    InstanceInfo {
        // Nothing records which agent implementation reported (see [`filter_values`]).
        agent_type: None,
        agent_version: held.agent_version.clone(),
        computer_name: held.computer_name.clone(),
        instance_status: held.instance_state.clone(),
        ip_address: held.ip_address.clone(),
        // Every node this store holds registered with Systems Manager.
        managed_status: Some(MANAGED_STATUS_MANAGED.to_string()),
        name: held.name.clone(),
        platform_type: held.platform_type.clone(),
        platform_name: held.platform_name.clone(),
        platform_version: held.platform_version.clone(),
        resource_type: held.resource_type.clone(),
        source_type: held.source_type.clone(),
        source_id: held.source_id.clone(),
        source_location: held.source_location.clone(),
        availability_zone: held.availability_zone.clone(),
        // Nothing holds the id of the zone a node runs in (see [`filter_values`]).
        availability_zone_id: None,
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// Whether the Systems Manager integrated experience is enabled at `region`'s endpoint:
    /// the account holds the Amazon Web Services managed node resource data sync there
    /// ([`MANAGED_NODE_SYNC_NAME`]).
    ///
    /// A node listing that answers `false` here is refused with [`NOT_ENABLED_MESSAGE`]
    /// before it looks at anything else the request names.
    pub fn has_integrated_experience(&self, region: &str) -> bool {
        self.resource_data_sync(region, MANAGED_NODE_SYNC_NAME)
            .is_some()
    }

    /// The Regions whose nodes one node listing reports, in ascending order, without
    /// repetition and at most [`MAX_LISTED_REGIONS`] of them.
    ///
    /// A listing that names no sync is a "single account/single-Region configuration"
    /// (`ListNodesRequest$SyncName`) and reports the nodes of `region`, the Region it was
    /// called at. One that names the managed node sync reports the nodes of the Regions that
    /// sync aggregates — "information about the data sources to synchronize"
    /// (`ResourceDataSyncSource$SourceRegions`) — and so of `region` alone for a sync that
    /// aggregates no Region of its own.
    pub fn node_listing_regions(&self, region: &str, sync_named: bool) -> Vec<String> {
        let mut regions: Vec<String> = if sync_named {
            self.resource_data_sync(region, MANAGED_NODE_SYNC_NAME)
                .and_then(|sync| sync.sync_source.as_ref())
                .map(|source| source.source_regions.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if regions.is_empty() {
            return vec![region.to_string()];
        }
        regions.sort();
        regions.dedup();
        regions.truncate(MAX_LISTED_REGIONS);
        regions
    }
}
