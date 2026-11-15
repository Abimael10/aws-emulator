// SPDX-License-Identifier: CC0-1.0

//! The subnets the ACCOUNT has, as the service that owns them reports them.
//!
//! A subnet is not a resource of this service: it is Amazon EC2's. A client builds its
//! network with `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC EC2
//! materialises for every Region of the account, and then names those subnets in
//! `CreateSubnetGroup$SubnetIds`. So EC2 is the ONE answer to "which subnets does the
//! account have", and an identifier a request names is resolved there and nowhere else —
//! reached the way any emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! The VPC a subnet group reports and the Availability Zone each of its subnets sits in
//! come from the record EC2 answered with, never from anything built here, so the
//! placement this service reports is the network the account really has. An identifier
//! EC2 reports no subnet for resolves to nothing, and that is the only case a create is
//! refused for.
//!
//! Nothing is cached: a subnet created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use std::collections::{HashMap, HashSet};

use super::SubnetRecord;

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `SubnetIds`: an identifier the account
/// has no subnet for is then an absent row instead of a failure of the whole call, so
/// every identifier the request named is still resolved against what EC2 does hold.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The most subnet identifiers ONE hop asks about, as values of one `subnet-id` filter.
///
/// The owner evaluates a filter against every subnet the account stores, so the cost of a
/// hop is (identifiers asked x subnets stored): this caps the first factor, so one hop
/// costs a constant times what the account stores rather than something the request
/// chooses. `CreateSubnetGroup` refuses a request naming more than
/// [`super::MAX_SUBNETS_PER_GROUP`] subnets BEFORE it resolves any of them, and that
/// bound is below this one, so one request costs ONE hop whatever its body carries.
const SUBNET_IDS_PER_HOP: usize = 64;

/// The most pages of one `DescribeSubnets` answer this reads.
///
/// EC2 pages its answer and reports a `NextToken` while subnets of the selection remain,
/// so a selection larger than one page is followed rather than silently truncated —
/// otherwise a subnet the account HAS would resolve to nothing because of where it
/// landed. The cap keeps the hop finite whatever the owner holds.
const SUBNET_PAGES_PER_HOP: usize = 8;

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `region` is the request's own signing region, so the question is asked in the Region
/// the caller addressed. The endpoint URL is only the address the callback's transport
/// routes on IN PROCESS: no socket is opened, no name is resolved and nothing leaves this
/// process. The credentials are the callback's, which sign this in-process hop and never
/// appear in a response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: `None` for a value that would name an
/// authority the caller chose rather than the peer service this emulator asks.
fn ec2(
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
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials the
        // callback hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// Every subnet of `subnet_ids` the account has in `region`, by identifier, as EC2
/// reports it — an identifier it holds no subnet for is simply absent from the map.
///
/// FAIL-CLOSED: a hop that could not be made at all contributes nothing, so the
/// identifiers it would have answered for resolve to nothing and the create is refused.
/// Nothing about a subnet is invented here, and a subnet that cannot be shown to be the
/// account's is not one a subnet group is built from: the group PUBLISHES each subnet's
/// VPC and Availability Zone, so with no record there is nothing to store or to report.
///
/// Bounded by [`SUBNET_IDS_PER_HOP`] identifiers per hop and [`SUBNET_PAGES_PER_HOP`]
/// pages per hop.
async fn held_subnets(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[&str],
) -> HashMap<String, SubnetRecord> {
    let mut held: HashMap<String, SubnetRecord> = HashMap::new();
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return held;
    };
    for chunk in subnet_ids.chunks(SUBNET_IDS_PER_HOP) {
        let mut next_token: Option<String> = None;
        for _ in 0..SUBNET_PAGES_PER_HOP {
            let by_id = ::aws_sdk_ec2::types::Filter::builder()
                .name(SUBNET_ID_FILTER)
                .set_values(Some(chunk.iter().map(|id| (*id).to_string()).collect()))
                .build();
            match client
                .describe_subnets()
                .filters(by_id)
                .set_next_token(next_token.take())
                .send()
                .await
            {
                Ok(output) => {
                    held.extend(output.subnets().iter().filter_map(|subnet| {
                        let subnet_id = subnet.subnet_id()?.to_string();
                        Some((
                            subnet_id.clone(),
                            SubnetRecord {
                                subnet_id,
                                // EC2 reports the VPC a subnet belongs to and the zone it
                                // sits in; a record missing either carries what this
                                // service can report of it, and nothing is invented here.
                                vpc_id: subnet.vpc_id().unwrap_or_default().to_string(),
                                availability_zone: subnet
                                    .availability_zone()
                                    .unwrap_or_default()
                                    .to_string(),
                            },
                        ))
                    }));
                    match output.next_token() {
                        Some(token) => next_token = Some(token.to_string()),
                        // The whole selection has been reported.
                        None => break,
                    }
                }
                Err(error) => {
                    // At `warn`: a group cannot be built when the owner of subnets cannot
                    // be asked, so a deployment where this hop fails is one where no
                    // subnet group can be created at all — worth seeing, and it IS why an
                    // identifier did not resolve.
                    ::tracing::warn!(
                        %region,
                        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                        "the subnets a create named could not be resolved against EC2"
                    );
                    break;
                }
            }
        }
    }
    held
}

/// The VPC every subnet of `subnets` belongs to, or `None` when they do not all belong to
/// one — "All of the subnets that you specify in a subnet group must exist in the same VPC"
/// (`CreateCluster$SubnetGroupName`), so a request spanning two of them names no single VPC
/// the group could report.
pub fn common_vpc(subnets: &[SubnetRecord]) -> Option<&str> {
    let first = subnets.first()?;
    subnets
        .iter()
        .all(|subnet| subnet.vpc_id == first.vpc_id)
        .then_some(first.vpc_id.as_str())
}

/// Every subnet identifier a request names, resolved against the subnets the account has
/// in `region`, in the order the request listed them.
///
/// `Err` carries the identifiers that name NO subnet of the account, in that same order
/// and without repetition, so the refusal names exactly what was wrong with the request
/// and nothing is stored.
///
/// ONE source — the service that owns subnets (see this module's own documentation) —
/// asked about every identifier the request named at once, deduplicated, so the work does
/// not grow with how many times one subnet is named.
pub async fn resolve_requested_subnets(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[&str],
) -> Result<Vec<SubnetRecord>, Vec<String>> {
    let mut distinct: Vec<&str> = Vec::with_capacity(subnet_ids.len());
    let mut seen: HashSet<&str> = HashSet::new();
    for subnet_id in subnet_ids {
        if seen.insert(subnet_id) {
            distinct.push(subnet_id);
        }
    }
    let held = held_subnets(ctx, region, &distinct).await;

    let mut resolved = Vec::with_capacity(subnet_ids.len());
    let mut unresolved: Vec<String> = Vec::new();
    for subnet_id in &distinct {
        if !held.contains_key(*subnet_id) {
            unresolved.push((*subnet_id).to_string());
        }
    }
    if !unresolved.is_empty() {
        return Err(unresolved);
    }
    for subnet_id in subnet_ids {
        if let Some(subnet) = held.get(*subnet_id) {
            resolved.push(subnet.clone());
        }
    }
    Ok(resolved)
}
