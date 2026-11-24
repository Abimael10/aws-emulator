// SPDX-License-Identifier: CC0-1.0

//! The VPC subnets a subnet group places a cluster in, resolved against the service that
//! OWNS them.
//!
//! A subnet group is "a collection of subnets (typically private) that you can designate
//! for your clusters running in an Amazon Virtual Private Cloud (VPC) environment", and
//! every one of its `SubnetIds` names an Amazon EC2 subnet — none of which is MemoryDB's.
//! So which VPC a group is in, which Availability Zone each of its subnets sits in and which
//! address families each is addressed in are EC2's to answer, and the EC2 service mounted in
//! THIS emulator is asked —
//! `DescribeSubnets` over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate — BEFORE any subnet group is written. That is why
//! `SubnetGroup$VpcId`, each `Subnet$AvailabilityZone` and each `SupportedNetworkTypes` are
//! values of this store at all:
//! they are what the owning service reported for the identifiers the request carried,
//! never values made up here.
//!
//! NOTHING here reaches the network. The callback is the framework's in-process dispatcher:
//! it hands the request to a service registered in this same process, so no socket is
//! opened, no address from the request is contacted, and the only destination involved is
//! this emulator itself. The Region label the caller signed for selects which of this
//! emulator's own regional state answers, not a host.
//!
//! An emulator with no EC2 mounted learns nothing to refuse a request on, and refuses
//! nothing: the group is stored with the identifiers it was given and reports no VPC, no
//! zone and no network types, because a refusal is grounded in what EC2 reported and never
//! assumed from silence.

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches IN PROCESS to a service registered in this very emulator, so no socket is
/// opened and nothing leaves the host. The endpoint URL below is only the address the SDK
/// needs to name the service and Region it is addressing — the callback's HTTP client never
/// resolves or connects to it — which is also why there is nothing to retry, time out, watch
/// for a stalled stream, or cache an identity for.
fn ec2_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_ec2::Client {
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
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// One subnet of a stored subnet group: the identifier the request listed, the
/// Availability Zone the service that owns it reports it in (`None` where no EC2 answered
/// about it), and the network types it supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSubnet {
    pub identifier: String,
    pub availability_zone: Option<String>,
    /// `Subnet$SupportedNetworkTypes`: the network types this subnet supports, derived from
    /// the address families Amazon EC2 reports blocks of for it
    /// ([`super::network_types::subnet_network_types`]). EMPTY where no EC2 answered about
    /// the subnet, since the families are that service's to report.
    pub supported_network_types: Vec<String>,
}

/// Where a subnet group places a cluster, as the owning service reports it: the VPC every
/// one of its subnets is in (`None` where no EC2 answered), and the subnets themselves in
/// the order the request listed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub vpc_id: Option<String>,
    pub subnets: Vec<StoredSubnet>,
}

/// What EC2 says about the subnets a request names: the placement to store, or why the
/// group cannot be written.
#[derive(Debug, Clone)]
pub enum SubnetPlacement {
    Resolved(Placement),
    /// The request names subnets that service holds none for, or subnets that are not all
    /// in one VPC. The caller maps each class onto the error this service answers it with.
    Refused(SubnetRefusal),
}

/// Why a subnet list cannot be stored, in the two classes this service answers differently:
/// identifiers that name no subnet of the account ([`SubnetRefusal::Absent`]), and subnets
/// this service will not place one group across ([`SubnetNotAllowedFault`]).
///
/// [`SubnetNotAllowedFault`]: crate::ops::types::SubnetNotAllowedFault
#[derive(Debug, Clone)]
pub enum SubnetRefusal {
    /// The identifiers Amazon EC2 holds no subnet for, in the order the request listed
    /// them. Answered with the modelled `InvalidParameterValueException` carrying
    /// [`super::absent_subnet_ids_message`].
    Absent(Vec<String>),
    /// The sentence the modelled `SubnetNotAllowedFault` carries.
    NotAllowed(String),
}

/// The sentence this service states for subnets that are not all in one VPC: a subnet group
/// designates subnets "for your clusters running in an Amazon Virtual Private Cloud (VPC)
/// environment" — one VPC — so a list spanning two is a placement no cluster can be made
/// in, which is what `SubnetNotAllowedFault` is modelled for.
fn mixed_vpc_message() -> String {
    "The requested subnets are not all in a common VPC.".to_string()
}

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// The filter is what asks the question rather than `SubnetIds`: an identifier the account
/// holds no subnet for is then an ABSENT ROW instead of a failure of the whole read, so one
/// read still resolves every identifier the request named and this service learns exactly
/// which of them name nothing — the difference between "EC2 says these do not exist" and
/// "EC2 could not be asked", which are answered differently below.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The most pages of one `DescribeSubnets` answer this follows.
///
/// EC2 pages its answer and reports a `NextToken` while subnets of the selection remain, so
/// a selection larger than one page is followed rather than truncated — otherwise a subnet
/// the account HAS would resolve to nothing because of where it landed. The cap keeps the
/// read finite whatever that service holds; at its default page size this is already well
/// past the number of subnets one group may name ([`super::MAX_SUBNETS_PER_GROUP`]). A read
/// that reaches the cap with a token still outstanding has learned only part of the answer,
/// so it reports NOTHING learned rather than a partial selection — a subnet in a page nobody
/// read must not be refused as one the account does not have.
const PAGES_PER_READ: usize = 8;

/// The subnets Amazon EC2 reports for the identifiers `subnet_ids` names, keyed by
/// identifier, or `None` when that service could not be asked at all.
///
/// `None` is "nothing was learned" — no EC2 is mounted in this emulator, it refused the
/// read, or the answer did not fit the pages this follows — and is NOT evidence that a
/// subnet is absent; an identifier missing from a `Some` answer is.
async fn described(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<::std::collections::BTreeMap<String, ::aws_sdk_ec2::types::Subnet>> {
    let client = ec2_client(ctx, region);
    // The selection asked for, built once: every page of this read asks the same question.
    let filter = ::aws_sdk_ec2::types::Filter::builder()
        .name(SUBNET_ID_FILTER)
        .set_values(Some(subnet_ids.to_vec()))
        .build();
    let mut held = ::std::collections::BTreeMap::new();
    let mut next_token: Option<String> = None;
    for _ in 0..PAGES_PER_READ {
        let output = client
            .describe_subnets()
            .filters(filter.clone())
            .set_next_token(next_token.take())
            .send()
            .await
            .ok()?;
        for subnet in output.subnets() {
            if let Some(subnet_id) = subnet.subnet_id() {
                held.insert(subnet_id.to_string(), subnet.clone());
            }
        }
        match output.next_token() {
            Some(token) => next_token = Some(token.to_string()),
            // The whole selection has been reported.
            None => return Some(held),
        }
    }
    // The cap was reached with pages of the selection still unread.
    None
}

/// Resolve the subnets `subnet_ids` names against Amazon EC2, for a request signed for
/// `region`.
///
/// Every identifier has already been held to the subnet-id FORM by the caller
/// ([`super::subnet_id_rejection`]); this is the other half — whether the account really
/// holds those subnets, where they are, and which network types they are addressed to
/// carry. An empty list resolves to an empty placement: there is nothing to ask EC2 about.
pub async fn resolve_subnets(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> SubnetPlacement {
    if subnet_ids.is_empty() {
        return SubnetPlacement::Resolved(Placement {
            vpc_id: None,
            subnets: Vec::new(),
        });
    }
    let Some(reported) = described(ctx, region, subnet_ids).await else {
        // EC2 refused the read, or no EC2 is mounted to answer it at all. Neither says
        // anything about the subnets, so nothing is refused and the group is stored with
        // the identifiers the request listed and no placement read off them.
        return SubnetPlacement::Resolved(Placement {
            vpc_id: None,
            subnets: subnet_ids
                .iter()
                .map(|identifier| StoredSubnet {
                    identifier: identifier.clone(),
                    availability_zone: None,
                    supported_network_types: Vec::new(),
                })
                .collect(),
        });
    };
    // A read that resolved but reports no subnet for identifiers the request named says
    // those identifiers name no subnet of the account: every one of them is reported back,
    // in the order the request listed them, so the refusal names each offending value.
    let absent: Vec<String> = subnet_ids
        .iter()
        .filter(|subnet_id| !reported.contains_key(subnet_id.as_str()))
        .cloned()
        .collect();
    if !absent.is_empty() {
        return SubnetPlacement::Refused(SubnetRefusal::Absent(absent));
    }
    let mut subnets = Vec::with_capacity(subnet_ids.len());
    let mut vpc_ids: Vec<String> = Vec::new();
    for subnet_id in subnet_ids {
        // Every identifier resolved: the ones EC2 reported no subnet for were refused just
        // above, so this skips nothing a group would have been stored without.
        let Some(found) = reported.get(subnet_id.as_str()) else {
            continue;
        };
        if let Some(vpc_id) = found.vpc_id() {
            if !vpc_ids.iter().any(|held| held == vpc_id) {
                vpc_ids.push(vpc_id.to_string());
            }
        }
        subnets.push(StoredSubnet {
            identifier: subnet_id.clone(),
            availability_zone: found.availability_zone().map(str::to_string),
            // The address families EC2 reports blocks of for this subnet — its IPv4 block,
            // and any IPv6 block associated with it — are what say which network types a
            // node placed in it can be reached at (see [`super::network_types`]), so they
            // are read off the owner's record like the zone above.
            supported_network_types: super::network_types::subnet_network_types(
                found.cidr_block().is_some_and(|cidr| !cidr.is_empty()),
                found.ipv6_cidr_block_association_set().iter().any(
                    |association| {
                        association
                            .ipv6_cidr_block()
                            .is_some_and(|cidr| !cidr.is_empty())
                    },
                ),
            ),
        });
    }
    if vpc_ids.len() > 1 {
        return SubnetPlacement::Refused(SubnetRefusal::NotAllowed(mixed_vpc_message()));
    }
    SubnetPlacement::Resolved(Placement {
        vpc_id: vpc_ids.into_iter().next(),
        subnets,
    })
}
