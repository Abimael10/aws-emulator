// SPDX-License-Identifier: CC0-1.0

//! The VPC security groups a request names, as the service that OWNS them reports them.
//!
//! A security group is not a resource of this service: it is Amazon EC2's. A client creates
//! one with `ec2:CreateSecurityGroup` (or uses the one EC2 materialises for a VPC) and then
//! names it in `VpcSecurityGroupIds`, so EC2 is the ONE answer to "does this group exist" —
//! reached the way any emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! A group a request names and the account does not hold is refused: the membership a
//! cluster or an instance would report for it is a reference to a resource that does not
//! exist, which no later `ec2:DescribeSecurityGroups` could agree with.
//!
//! A hop that could not be made at all (no EC2 in this process, an answer that could not be
//! read) is evidence of nothing, so NO group is reported unknown from it: a refusal has to
//! be grounded in what EC2 said.
//!
//! Nothing is cached: a group created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use std::collections::BTreeSet;

/// The `DescribeSecurityGroups` filter that selects groups BY ID (`group-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `GroupIds`: an identifier the account has
/// no group for is then an absent row instead of a failure of the whole call, so every
/// identifier the request named is still resolved against what EC2 does hold.
const GROUP_ID_FILTER: &str = "group-id";

/// The most group identifiers ONE hop asks about, for the same reason
/// [`super::ec2_subnets`] caps its own: the owner evaluates a filter against every group the
/// account stores, so one hop costs a constant times what the account stores rather than
/// something the request chooses.
const GROUP_IDS_PER_HOP: usize = 64;

/// What a request naming a security group the account does not hold is refused with.
///
/// Amazon RDS states the group three times over — as the subject of the sentence, then as
/// the id and the name it resolved to — and for an identifier it could not resolve the two
/// names are EMPTY, leaving the fixed text around the identifier the request carried. The
/// spacing is AWS's own.
pub fn invalid_security_group_message(group_id: &str) -> String {
    format!("Invalid security group , groupId= {group_id}, groupName=.")
}

/// The `DescribeSecurityGroups` filter that selects the groups of ONE VPC (`vpc-id`, listed
/// by that operation's own `Filters` documentation).
const VPC_ID_FILTER: &str = "vpc-id";

/// The `DescribeSecurityGroups` filter that selects groups by NAME (`group-name`, listed by
/// the same documentation).
const GROUP_NAME_FILTER: &str = "group-name";

/// The name EC2 gives the one security group it creates WITH a VPC: "your VPC automatically
/// comes with a default security group" (Amazon VPC User Guide, "Default security groups for
/// your VPCs"), named `default`, which is the group a resource placed in that VPC without
/// any named group belongs to.
const DEFAULT_SECURITY_GROUP_NAME: &str = "default";

/// The DEFAULT security group of `vpc_id`, as the service that owns security groups reports
/// it: the group named [`DEFAULT_SECURITY_GROUP_NAME`] in that VPC.
///
/// It is what a resource RDS places in a VPC belongs to when the request names no
/// `VpcSecurityGroupIds` of its own, so the identifier a resource then reports is the one EC2
/// minted for that group rather than a value this service chose — a client cross-checking it
/// against `ec2:DescribeSecurityGroups` reads one answer, and a group this service named
/// nobody minted would resolve to nothing.
///
/// `None` where the hop could not be made or EC2 answered with no such group: this service
/// then knows of no group to place the resource in, and reports none rather than one of its
/// own choosing.
pub async fn default_vpc_security_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
) -> Option<String> {
    let client = super::ec2_subnets::ec2(ctx, region)?;
    let of_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name(VPC_ID_FILTER)
        .values(vpc_id)
        .build();
    let named_default = ::aws_sdk_ec2::types::Filter::builder()
        .name(GROUP_NAME_FILTER)
        .values(DEFAULT_SECURITY_GROUP_NAME)
        .build();
    match client
        .describe_security_groups()
        .filters(of_vpc)
        .filters(named_default)
        .send()
        .await
    {
        Ok(output) => output
            .security_groups()
            .iter()
            .find_map(|group| group.group_id().map(str::to_string)),
        Err(error) => {
            // At `warn` for the same reason the other hops log one: a deployment where this
            // one fails is one where a resource placed in a VPC can name no group, and it IS
            // why the membership is empty.
            ::tracing::warn!(
                %region,
                %vpc_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the default security group of the VPC could not be resolved against EC2"
            );
            None
        }
    }
}

/// The identifiers among `group_ids` that EC2 ANSWERED it has no security group for, in
/// `region`, in the order the request named them.
///
/// Empty where the hop could not be made or EC2 answered with an error: this service then
/// knows nothing about the groups, and nothing is refused on no evidence.
pub async fn unresolved_vpc_security_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group_ids: &[String],
) -> Vec<String> {
    if group_ids.is_empty() {
        return Vec::new();
    }
    let Some(client) = super::ec2_subnets::ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return Vec::new();
    };
    let distinct: Vec<&str> = group_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    let mut held: BTreeSet<String> = BTreeSet::new();
    for chunk in distinct.chunks(GROUP_IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(GROUP_ID_FILTER)
            .set_values(Some(chunk.iter().map(|id| (*id).to_string()).collect()))
            .build();
        match client.describe_security_groups().filters(by_id).send().await {
            Ok(output) => {
                for group in output.security_groups() {
                    if let Some(group_id) = group.group_id() {
                        held.insert(group_id.to_string());
                    }
                }
            }
            Err(error) => {
                // At `warn` for the same reason the subnet hop logs one: a deployment where
                // this hop fails is one where no security group a request names can be
                // resolved, and it IS why nothing was refused.
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the security groups a request named could not be resolved against EC2"
                );
                return Vec::new();
            }
        }
    }
    group_ids
        .iter()
        .filter(|group_id| !held.contains(group_id.as_str()))
        .cloned()
        .collect()
}
