// SPDX-License-Identifier: CC0-1.0

//! The NETWORK a Studio domain is placed in, as the service that OWNS it reports it.
//!
//! A VPC, a subnet and a security group are not resources of this service: they are Amazon
//! EC2's. A client builds its network with `ec2:CreateVpc`/`ec2:CreateSubnet`/
//! `ec2:CreateSecurityGroup` and then names those identifiers in `CreateDomain`, so EC2 is
//! the ONE answer to which VPC holds which subnet and which group
//! (`service_common::network` states that contract for this whole tree). It is asked the way
//! any emulated service reaches another — the AWS SDK over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! # What the placement has to satisfy
//!
//! A domain's traffic "is through the specified VPC and subnets", so the parts of the
//! placement have to name ONE VPC: `CreateDomain$SubnetIds` are "the VPC subnets that the
//! domain uses for communication" and `UserSettings$SecurityGroups` "the security groups for
//! the Amazon Virtual Private Cloud (VPC) that the domain uses for communication" — a subnet
//! of another VPC than the one `VpcId` declares, or a security group of another VPC than the
//! domain's, is a placement no network interface could be attached in.
//!
//! # The mismatch travels on the DOMAIN, not on the call
//!
//! The create itself is served: the request is well formed and SageMaker answers it with the
//! domain's id, ARN and Studio URL while the domain is still `Pending`. The placement is
//! resolved as the domain provisions, so an inconsistent one surfaces as the domain's own
//! terminal [`super::DOMAIN_STATUS_FAILED`] and the `FailureReason` beside it
//! ([`super::Domain::settle_status`]) rather than as a refusal of the call.
//!
//! # Only what EC2 ANSWERED is failed
//!
//! A hop that could not be made (no EC2 in this process, an answer that could not be read)
//! is evidence of nothing, and an identifier EC2 reports no record for says nothing about
//! which VPC holds it: neither fails a domain. What is failed is a membership EC2 STATED and
//! that disagrees with the domain's own VPC.

use std::collections::BTreeMap;

use crate::ops::types::{DefaultSpaceSettings, DomainSettings, UserSettings};

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter asks the question rather than `SubnetIds` does: an identifier the account has no
/// subnet for is then an absent row instead of a failure of the whole call, so every
/// identifier the request named is still resolved against what EC2 does hold.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The same for a security group identifier (`group-id`, from `DescribeSecurityGroups`' own
/// `Filters` documentation).
const GROUP_ID_FILTER: &str = "group-id";

/// The most identifiers ONE hop asks about. The owner evaluates a filter against every
/// record the account stores, so this caps the cost of a hop at a constant times what the
/// account holds rather than at something the request chooses. `SubnetIds` is modelled
/// `length 1..16`, so a create's subnets always fit in one hop.
const IDS_PER_HOP: usize = 64;

/// How a domain reports a subnet that belongs to another VPC than the one it uses for
/// communication: the subnet AS THE REQUEST NAMED IT and the VPC the domain is in.
///
/// One sentence for one fact, so the reason a caller reads off a failed domain names the
/// member to fix. Inside `FailureReason`'s modelled `length 0..1024` for any pair of
/// identifiers, which are themselves `length 0..32`.
#[must_use]
pub fn subnet_outside_domain_vpc(subnet_id: &str, vpc_id: &str) -> String {
    format!("The subnet '{subnet_id}' does not exist in the VPC '{vpc_id}'.")
}

/// How a domain reports a security group that belongs to another VPC than the one it uses
/// for communication, worded like [`subnet_outside_domain_vpc`] because it states the same
/// fact about the other half of the placement.
#[must_use]
pub fn security_group_outside_domain_vpc(group_id: &str, vpc_id: &str) -> String {
    format!("The security group '{group_id}' does not belong to the VPC '{vpc_id}'.")
}

/// Every security group identifier a `CreateDomain` request names, in the order it names
/// them and each one once.
///
/// A domain carries security groups in three places — the default user settings, the domain
/// settings and the default space settings — and each of them names groups "for the Amazon
/// Virtual Private Cloud (VPC) that the domain uses for communication", so the one rule
/// holds over all three and they are gathered here once rather than at each caller.
#[must_use]
pub fn domain_security_group_ids(
    default_user_settings: &UserSettings,
    domain_settings: Option<&DomainSettings>,
    default_space_settings: Option<&DefaultSpaceSettings>,
) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    let members = [
        default_user_settings.security_groups.as_ref(),
        domain_settings.and_then(|settings| settings.security_group_ids.as_ref()),
        default_space_settings.and_then(|settings| settings.security_groups.as_ref()),
    ];
    for group_ids in members.into_iter().flatten() {
        for group_id in group_ids {
            if !group_id.is_empty() && !named.iter().any(|seen| seen == group_id) {
                named.push(group_id.clone());
            }
        }
    }
    named
}

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `pub(super)` so the OTHER question this crate puts to EC2 — whether the account holds the
/// EBS volume a HyperPod node attachment names ([`super::ec2_volumes`]) — is asked over the
/// same one client construction rather than a second copy of it.
///
/// `region` is the request's own signing region, so the network is resolved in the Region the
/// caller addressed. The endpoint URL is only the address the callback's transport routes on
/// IN PROCESS: no socket is opened, no name is resolved and nothing leaves this process. The
/// credentials are the callback's, which sign this in-process hop and never appear in a
/// response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it would end up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar):
/// `None` for a value that would name an authority the caller chose rather than the peer
/// service this emulator asks.
///
/// There is nothing to retry, time out or watch for a stalled stream on an in-process
/// transport, and each of those would need a sleep implementation this client is not given;
/// the identity is likewise used directly rather than cached.
pub(super) fn ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// The VPC EC2 reports for each of `subnet_ids` it holds a subnet for, by identifier. An
/// identifier it reports nothing for, and one whose record carries no VPC, is simply absent
/// from the map — EC2 stated no membership for it, and no membership is what this answers.
async fn subnet_vpcs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[&str],
) -> BTreeMap<String, String> {
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return held;
    };
    for chunk in subnet_ids.chunks(IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(SUBNET_ID_FILTER)
            .set_values(Some(chunk.iter().map(|id| (*id).to_string()).collect()))
            .build();
        match client.describe_subnets().filters(by_id).send().await {
            Ok(output) => {
                for subnet in output.subnets() {
                    if let (Some(subnet_id), Some(vpc_id)) = (subnet.subnet_id(), subnet.vpc_id()) {
                        held.insert(subnet_id.to_string(), vpc_id.to_string());
                    }
                }
            }
            Err(error) => {
                // At `warn`: a deployment where this hop fails is one where no domain's
                // placement can be resolved at all, and it IS why nothing was failed.
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the subnets a domain named could not be resolved against EC2"
                );
            }
        }
    }
    held
}

/// [`subnet_vpcs`] for security groups: the VPC EC2 reports for each of `group_ids` it holds
/// a group for, read the same way and absent for the same reasons.
async fn security_group_vpcs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group_ids: &[&str],
) -> BTreeMap<String, String> {
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return held;
    };
    for chunk in group_ids.chunks(IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(GROUP_ID_FILTER)
            .set_values(Some(chunk.iter().map(|id| (*id).to_string()).collect()))
            .build();
        match client.describe_security_groups().filters(by_id).send().await {
            Ok(output) => {
                for group in output.security_groups() {
                    if let (Some(group_id), Some(vpc_id)) = (group.group_id(), group.vpc_id()) {
                        held.insert(group_id.to_string(), vpc_id.to_string());
                    }
                }
            }
            Err(error) => {
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the security groups a domain named could not be resolved against EC2"
                );
            }
        }
    }
    held
}

/// Why the network placement a `CreateDomain` asked for cannot be provisioned, or `None`
/// when every part of it EC2 answered about belongs to the domain's one VPC.
///
/// The domain's VPC is the one `VpcId` declares, and — for a request that declares none —
/// the VPC EC2 reports for the FIRST subnet it resolved, because that is the VPC the
/// domain's traffic would then go through. Every other part EC2 stated a membership for is
/// held to it: the subnets in the order the request lists them, then the security groups, so
/// the reason names the same member whichever other parts also disagree.
///
/// Resolved while the domain is `Pending` and reported on the domain, never as a refusal of
/// the call (see this module's own documentation). ONE hop for the subnets and one for the
/// groups, each asking about every identifier the request named at once, deduplicated, so
/// the work does not grow with how many times one identifier is named.
pub async fn domain_network_failure_reason(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: Option<&str>,
    subnet_ids: &[String],
    security_group_ids: &[String],
) -> Option<String> {
    let declared_vpc = vpc_id.filter(|vpc_id| !vpc_id.is_empty());
    if subnet_ids.is_empty() && security_group_ids.is_empty() {
        // Nothing is placed in a VPC, so there is no membership to hold to anything.
        return None;
    }

    let mut domain_vpc = declared_vpc.map(str::to_string);
    if !subnet_ids.is_empty() {
        let distinct = distinct(subnet_ids);
        let held = subnet_vpcs(ctx, region, &distinct).await;
        for subnet_id in subnet_ids {
            let Some(holder) = held.get(subnet_id.as_str()) else {
                continue;
            };
            match &domain_vpc {
                Some(vpc_id) if vpc_id != holder => {
                    return Some(subnet_outside_domain_vpc(subnet_id, vpc_id));
                }
                Some(_) => {}
                // A request that declared no VPC takes the first subnet's as the domain's.
                None => domain_vpc = Some(holder.clone()),
            }
        }
    }

    let vpc_id = domain_vpc?;
    if security_group_ids.is_empty() {
        return None;
    }
    let distinct = distinct(security_group_ids);
    let held = security_group_vpcs(ctx, region, &distinct).await;
    security_group_ids.iter().find_map(|group_id| {
        held.get(group_id.as_str())
            .filter(|holder| *holder != &vpc_id)
            .map(|_| security_group_outside_domain_vpc(group_id, &vpc_id))
    })
}

/// `ids` with each value once, in the order they first appear, so one hop asks about an
/// identifier a request repeats exactly once.
fn distinct(ids: &[String]) -> Vec<&str> {
    let mut distinct: Vec<&str> = Vec::with_capacity(ids.len());
    for id in ids {
        if !distinct.contains(&id.as_str()) {
            distinct.push(id);
        }
    }
    distinct
}

#[cfg(test)]
mod tests {
    use super::domain_security_group_ids;
    use crate::ops::types::{DefaultSpaceSettings, DomainSettings, UserSettings};

    /// RULE: every security group a create names, from whichever of the three settings
    /// members carries it, is gathered once in the order it was named.
    #[test]
    fn every_security_group_member_of_a_create_is_gathered_once() {
        let user = UserSettings {
            security_groups: Some(vec!["sg-1".to_string(), "sg-2".to_string()]),
            ..UserSettings::default()
        };
        let domain = DomainSettings {
            // The repeat and the empty value are what the gathering drops.
            security_group_ids: Some(vec![
                "sg-2".to_string(),
                String::new(),
                "sg-3".to_string(),
            ]),
            ..DomainSettings::default()
        };
        let space = DefaultSpaceSettings {
            security_groups: Some(vec!["sg-4".to_string()]),
            ..DefaultSpaceSettings::default()
        };

        for (case, settings, expected) in [
            (
                "no group at all",
                (
                    UserSettings::default(),
                    None,
                    None::<DefaultSpaceSettings>,
                ),
                Vec::<&str>::new(),
            ),
            (
                "the default user settings only",
                (user.clone(), None, None),
                vec!["sg-1", "sg-2"],
            ),
            (
                "every member, with a repeat and an empty value",
                (user.clone(), Some(domain.clone()), Some(space.clone())),
                vec!["sg-1", "sg-2", "sg-3", "sg-4"],
            ),
            (
                "the space settings only",
                (UserSettings::default(), None, Some(space.clone())),
                vec!["sg-4"],
            ),
        ] {
            let (user_settings, domain_settings, space_settings) = settings;
            let gathered = domain_security_group_ids(
                &user_settings,
                domain_settings.as_ref(),
                space_settings.as_ref(),
            );
            assert!(
                gathered == expected,
                "case {case:?}: expected {expected:?}, got {gathered:?}"
            );
        }
    }
}
