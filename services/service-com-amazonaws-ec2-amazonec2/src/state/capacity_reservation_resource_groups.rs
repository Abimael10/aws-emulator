// SPDX-License-Identifier: CC0-1.0

//! The resource groups a Capacity Reservation has been ADDED to, as
//! `GetGroupsForCapacityReservation` reports them.
//!
//! A resource group is NOT a resource of this service: `resource-groups:CreateGroup` creates
//! one, `resource-groups:GroupResources` adds a resource to it and
//! `resource-groups:UngroupResources` takes it out again. So the operation that reports the
//! groups a reservation belongs to keeps no copy of that membership and no second list of which
//! groups exist — it ASKS the service that owns them, over the emulator's in-process callback,
//! and reports exactly the groups that service answers with.
//!
//! The public API of Resource Groups has no reverse lookup, so the membership is read the way
//! a client would: the account's groups in the Region, then each group's own membership
//! listing, keeping the groups whose membership names the resource. Both walks are bounded.
//!
//! NOTHING here opens a socket or leaves the host: the transport is
//! `ctx.callback().http_client()`, the emulator's in-process channel into its own services,
//! and the endpoint is only the address that channel resolves Resource Groups by. The
//! credentials are the emulator's own, used to sign this read; they are never returned or
//! logged.
//!
//! READ ONLY: nothing here writes to Resource Groups, and nothing here caches its answer — two
//! reads ask that service twice, so a grouping made between them is seen.

use ::smithy_server_runtime::OperationContext;

/// The endpoint prefix the in-process channel resolves Resource Groups by, which is also the
/// service segment of every group ARN that service mints.
const ENDPOINT_PREFIX: &str = "resource-groups";

/// How many pages of the account's groups one read follows.
///
/// The listing is paged, and the pages are followed until Resource Groups stops handing back a
/// token — this is what bounds that walk, so a token that never runs out cannot loop. At the
/// page size that service answers with it covers far more groups than an account's quota
/// allows.
pub const MAX_RESOURCE_GROUP_PAGES: usize = 32;

/// How many pages of ONE group's membership one read follows, for the same reason.
pub const MAX_RESOURCE_GROUP_MEMBERSHIP_PAGES: usize = 32;

/// How many memberships one read REPORTS, whatever it found.
///
/// The walks above bound the work; this bounds the answer, so the collection a reporting
/// operation builds out of it is bounded by this crate and never by a count a caller could
/// drive up by creating groups. It is well above the groups one account holds in one Region.
pub const MAX_RESOURCE_GROUPS_REPORTED: usize = 1_000;

/// One resource group a resource belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceGroupMembership {
    /// The group's ARN, as Resource Groups minted it.
    pub group_arn: String,
    /// The account that owns the group, read out of that ARN — the account segment is what an
    /// ARN names the owner in — or `None` for an ARN that names none.
    pub owner_id: Option<String>,
}

/// Every resource group of `region` whose membership names `resource_arn`, in group-ARN order
/// and once each.
///
/// The groups are the account's own, listed through Resource Groups, and the membership is that
/// service's to report: a group's membership comes from its definition — a static grouping, or
/// a resource query it resolves — so it is ASKED for rather than guessed at here.
///
/// Failing to ask reports NO group, which is the same answer an account whose resource is in no
/// group gives: an emulator with no Resource Groups mounted knows of no grouping, and nothing
/// is invented in its place.
///
/// What one read costs is bounded before it starts: at most
/// [`MAX_RESOURCE_GROUP_PAGES`] pages of groups, at most
/// [`MAX_RESOURCE_GROUP_MEMBERSHIP_PAGES`] pages of each one's membership, and at most
/// [`MAX_RESOURCE_GROUPS_REPORTED`] memberships reported.
pub async fn groups_holding(
    ctx: &OperationContext<'_>,
    region: &str,
    resource_arn: &str,
) -> Vec<ResourceGroupMembership> {
    let client = client(ctx, region);
    let mut memberships: Vec<ResourceGroupMembership> = Vec::new();
    for group_arn in groups_of_account(&client).await {
        if memberships.len() >= MAX_RESOURCE_GROUPS_REPORTED {
            break;
        }
        if group_holds(&client, &group_arn, resource_arn).await {
            let owner_id = super::arn_field(&group_arn, 4)
                .filter(|account| !account.is_empty())
                .map(str::to_string);
            memberships.push(ResourceGroupMembership {
                group_arn,
                owner_id,
            });
        }
    }
    memberships.sort_by(|left, right| left.group_arn.cmp(&right.group_arn));
    memberships.dedup();
    memberships
}

/// The ARN of every resource group the account holds in the client's Region, in the order
/// Resource Groups pages them.
///
/// `GroupIdentifiers` is the report the model's deprecated `Groups` was replaced by, so the
/// groups are read off it, and a listed entry that names no ARN names no group to ask about.
async fn groups_of_account(client: &::aws_sdk_resourcegroups::Client) -> Vec<String> {
    let mut group_arns: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_RESOURCE_GROUP_PAGES {
        let answer = client
            .list_groups()
            .set_next_token(next_token.take())
            .send()
            .await;
        let page = match answer {
            Ok(page) => page,
            Err(error) => {
                report(&error, "the account's resource groups could not be listed");
                break;
            }
        };
        group_arns.extend(
            page.group_identifiers()
                .iter()
                .filter_map(|identifier| identifier.group_arn())
                .map(str::to_string),
        );
        next_token = page
            .next_token()
            .map(str::to_string)
            .filter(|token| !token.is_empty());
        if next_token.is_none() {
            break;
        }
    }
    group_arns
}

/// Whether the group `group_arn` names holds `resource_arn` as a member.
///
/// The membership is read off `Resources` — the report the model's deprecated
/// `ResourceIdentifiers` was replaced by — and the walk stops at the page the resource is found
/// on rather than reading the whole membership of a group that already answered.
///
/// A group that does not answer is reported as holding the resource in NO page of its
/// membership, and the read goes on to the next group rather than failing the request: the
/// listing is a walk over groups this read itself enumerated a moment earlier, so the answer
/// this case stands for is a group that was deleted, or emptied, between the two calls — a
/// group that no longer holds anything is exactly a group that does not hold this resource.
/// The failure is reported through [`report`] so a developer sees it.
async fn group_holds(
    client: &::aws_sdk_resourcegroups::Client,
    group_arn: &str,
    resource_arn: &str,
) -> bool {
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_RESOURCE_GROUP_MEMBERSHIP_PAGES {
        let answer = client
            .list_group_resources()
            .group(group_arn)
            .set_next_token(next_token.take())
            .send()
            .await;
        let page = match answer {
            Ok(page) => page,
            Err(error) => {
                report(&error, "a resource group's members could not be listed");
                return false;
            }
        };
        let holds = page
            .resources()
            .iter()
            .filter_map(|item| item.identifier())
            .filter_map(|identifier| identifier.resource_arn())
            .any(|member| member == resource_arn);
        if holds {
            return true;
        }
        next_token = page
            .next_token()
            .map(str::to_string)
            .filter(|token| !token.is_empty());
        if next_token.is_none() {
            return false;
        }
    }
    false
}

/// Report a Resource Groups failure without failing the request it was read for.
///
/// Resource Groups ANSWERING a failure is worth a developer's attention — it holds the groups
/// and could not report them, so the membership this read sees is short. Nobody answering at
/// all is an emulator with no Resource Groups mounted, which is not a failure of the request:
/// the account then has no grouping to report.
fn report<E, R>(
    error: &::aws_smithy_runtime_api::client::result::SdkError<E, R>,
    what: &'static str,
) where
    E: ::std::error::Error + 'static,
    R: ::std::fmt::Debug + 'static,
{
    let reported = ::aws_smithy_types::error::display::DisplayErrorContext(error);
    if error.as_service_error().is_some() {
        ::tracing::warn!(error = %reported, "{what}");
    } else {
        ::tracing::debug!(error = %reported, "{what}");
    }
}

/// The Resource Groups client this service reads that service through: the AWS SDK driven over
/// the emulator's own in-process transport, signing in the emulator's name.
fn client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_resourcegroups::Client {
    let config = ::aws_sdk_resourcegroups::Config::builder()
        .behavior_version(::aws_sdk_resourcegroups::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_resourcegroups::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials are handed over
        // directly.
        .retry_config(::aws_sdk_resourcegroups::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_resourcegroups::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_resourcegroups::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_resourcegroups::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_resourcegroups::Client::from_conf(config)
}
