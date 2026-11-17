// SPDX-License-Identifier: CC0-1.0

//! The account's resource groups: a fact this service RESOLVES rather than holds.
//!
//! An application is created OVER a resource group, and a resource group is not a
//! resource of this service — it belongs to Resource Groups. So the group a create
//! names is resolved against Resource Groups, reached the way any emulated service
//! reaches another: through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]).
//!
//! The answer is only ever one of three things, and a refusal is never assumed from
//! silence: an emulator with no Resource Groups mounted knows nothing about the group,
//! and nothing is what it refuses on ([`GroupLookup::Unknown`]). Only Resource Groups
//! ANSWERING that the account holds no such group ([`GroupLookup::Absent`]) is grounds
//! for refusing the create.
//!
//! The group's MEMBERSHIP is resolved the same way ([`group_resources`]): which
//! resources a group holds is computed from that group's own definition, which is
//! Resource Groups' to run, and it is what an application onboards its components
//! from.

/// What Resource Groups says about the group a request named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupLookup {
    /// Resource Groups reports the group.
    Present,
    /// Resource Groups reports that the account holds no group of that name in this
    /// region.
    Absent,
    /// Nothing is known: there is no Resource Groups to ask, or it answered something
    /// else.
    Unknown,
}

/// The Resource Groups client this service resolves a named group through.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — and each of those would need a sleep implementation this client is
/// not given. The same goes for caching the identity: the credentials the callback hands
/// over are used directly. `region` is the request's own signing region, because a
/// resource group lives in the region whose endpoint created it.
fn resource_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_resourcegroups::Client {
    let config = ::aws_sdk_resourcegroups::Config::builder()
        .behavior_version(::aws_sdk_resourcegroups::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_resourcegroups::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://resource-groups.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_resourcegroups::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_resourcegroups::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_resourcegroups::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_resourcegroups::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_resourcegroups::Client::from_conf(config)
}

/// One member of a resource group: the resource's ARN, and the resource type
/// Resource Groups reported for it (absent when it reported none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub resource_arn: String,
    pub resource_type: Option<String>,
}

/// How many pages of one group's membership are read.
///
/// The listing is paged, and the pages are followed until Resource Groups stops
/// handing back a token — this is what bounds that walk, so a token that never
/// runs out cannot loop. It covers far more resources than the components one
/// application holds ([`crate::state::COMPONENT_QUOTA`]).
const MAX_MEMBERSHIP_PAGES: usize = 32;

/// Every resource the group `name` resolves to in `region`, in the order
/// Resource Groups pages them.
///
/// A group's membership is Resource Groups' to compute, not this service's: a
/// group defined by a resource query resolves that query, and a group populated
/// by hand reports what was grouped into it. So this ASKS for the membership,
/// over the same in-process callback the group's existence is resolved through,
/// and reads it off `Resources` — the report the model's deprecated
/// `ResourceIdentifiers` was replaced by.
///
/// Failing to ask reports no member, exactly as a group holding no resource
/// does: an emulator with no Resource Groups mounted knows of no resource to
/// onboard, and nothing is invented in its place.
pub async fn group_resources(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Vec<GroupMember> {
    let client = resource_groups(ctx, region);
    let mut members: Vec<GroupMember> = Vec::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_MEMBERSHIP_PAGES {
        let answer = client
            .list_group_resources()
            .group(name)
            .set_next_token(next_token.take())
            .send()
            .await;
        let page = match answer {
            Ok(page) => page,
            Err(error) => {
                let reported = ::aws_smithy_types::error::display::DisplayErrorContext(&error);
                if error.as_service_error().is_some() {
                    // Resource Groups ANSWERED a failure: it holds the group's
                    // definition and could not report its membership, which is
                    // worth a developer's attention — the application onboards
                    // nothing rather than the resources the group holds.
                    ::tracing::warn!(
                        error = %reported,
                        "Resource Groups refused to list the resources of a request's resource group"
                    );
                } else {
                    // Nobody answered at all: an emulator with no Resource Groups
                    // mounted knows of no resource to onboard, which is not a
                    // failure of this request.
                    ::tracing::debug!(
                        error = %reported,
                        "no Resource Groups answered for the resources of a request's resource group"
                    );
                }
                break;
            }
        };
        let mut page_members: Vec<GroupMember> = page
            .resources()
            .iter()
            .filter_map(|item| item.identifier())
            .filter_map(member_of)
            .collect();
        members.append(&mut page_members);
        next_token = page
            .next_token()
            .map(str::to_string)
            .filter(|token| !token.is_empty());
        if next_token.is_none() {
            break;
        }
    }
    members
}

/// One listed identifier as a membership, or `None` for an entry that names no
/// resource.
fn member_of(
    identifier: &::aws_sdk_resourcegroups::types::ResourceIdentifier,
) -> Option<GroupMember> {
    let resource_arn = identifier.resource_arn()?;
    Some(GroupMember {
        resource_arn: resource_arn.to_string(),
        resource_type: identifier.resource_type().map(str::to_string),
    })
}

/// What Resource Groups says about the group `name` names in `region`.
///
/// Resource Groups answers its own not-found error for a name the account holds no
/// group for, which is the one answer that says the group does not exist. Any other
/// failure is this emulator failing to ask rather than Resource Groups reporting a
/// missing group, so it says nothing ([`GroupLookup::Unknown`]).
pub async fn resolve_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> GroupLookup {
    let answer = resource_groups(ctx, region)
        .get_group()
        .group(name)
        .send()
        .await;
    let error = match answer {
        Ok(_) => return GroupLookup::Present,
        Err(error) => error,
    };
    if error
        .as_service_error()
        .is_some_and(::aws_sdk_resourcegroups::operation::get_group::GetGroupError::is_not_found_exception)
    {
        return GroupLookup::Absent;
    }
    ::tracing::debug!(
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "the resource group a request named could not be resolved against Resource Groups"
    );
    GroupLookup::Unknown
}
