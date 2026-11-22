// SPDX-License-Identifier: CC0-1.0

//! The subnets a request names, as the service that OWNS subnets reports them.
//!
//! A subnet is not a resource of this service: it is Amazon EC2's. A client builds its
//! network with `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC EC2 materialises
//! for every Region of the account, and then names those subnets in
//! `CreateDBSubnetGroupMessage$SubnetIds` or `ModifyDBSubnetGroupMessage$SubnetIds`. So EC2
//! is the ONE answer to "is this a subnet of the account, and where is it", and an
//! identifier a request names is resolved there and nowhere else — reached the way any
//! emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! What a subnet group REPORTS about its subnets — the Availability Zone each one lies in
//! (`Subnet$SubnetAvailabilityZone`), the VPC that owns them (`DBSubnetGroup$VpcId`) and the
//! protocol families they are addressed in (`DBSubnetGroup$SupportedNetworkTypes`) — is read
//! off the records EC2 answered with, never built from the identifiers themselves: a
//! subnet's zone, VPC and address blocks are EC2's facts, and a client cross-checking
//! `DescribeDBSubnetGroups` against `ec2:DescribeSubnets` must see one answer, not two.
//!
//! An identifier EC2 ANSWERED it holds no subnet for names no subnet of the account, and a
//! request carrying one is refused: the subnets a subnet group holds have to be subnets, and
//! a later `ec2:DescribeSubnets` could agree with no group built on an identifier nobody
//! minted.
//!
//! A hop that could not be made at all (no EC2 in this process, an answer that could not be
//! read) is evidence of nothing: the subnets it would have answered for stay UNKNOWN, which
//! this service reports as no zone, no VPC and no network type rather than as values of its
//! own choosing, and which no refusal is assumed from either
//! (`services/common/src/network.rs` states which reading belongs to which kind of
//! service).
//!
//! Nothing is cached: a subnet created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use std::collections::{BTreeMap, BTreeSet};

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `SubnetIds`: an identifier the account has
/// no subnet for is then an absent row instead of a failure of the whole call, so every
/// identifier the request named is still resolved against what EC2 does hold.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The most subnet identifiers ONE hop asks about, as values of one `subnet-id` filter.
///
/// The owner evaluates a filter against every subnet the account stores, so the cost of a
/// hop is (identifiers asked x subnets stored): this caps the first factor, so one hop costs
/// a constant times what the account stores rather than something the request chooses. Both
/// callers hold the list to [`super::MAX_SUBNETS_PER_SUBNET_GROUP`] before they resolve any
/// of it, and that bound is below this one, so one request costs ONE hop whatever its body
/// carries.
const SUBNET_IDS_PER_HOP: usize = 64;

/// `DBSubnetGroup$SupportedNetworkTypes`: "Valid Values: `IPV4` | `DUAL`", the IPv4-only
/// member.
pub const NETWORK_TYPE_IPV4: &str = "IPV4";

/// `DBSubnetGroup$SupportedNetworkTypes`: the member for a group whose subnets carry both
/// families — "A `DBSubnetGroup` can support only the IPv4 protocol or the IPv4 and the IPv6
/// protocols (DUAL)."
pub const NETWORK_TYPE_DUAL: &str = "DUAL";

/// What a request naming subnets that cannot make up a subnet group — ones the account does
/// not hold — is refused with.
///
/// The variable part is the request's WHOLE `SubnetIds` list, in the order it named them and
/// including the identifiers that DID resolve: the sentence reports which subnets the
/// service was asked about and says that SOME of them are invalid, not which ones, so a
/// client that diffs the bracketed list against what it submitted reads its own list back.
/// The fixed text around them, whose spacing (the space before the colon, none after it) is
/// AWS's own, is left as AWS words it, and the sibling view of this same front door
/// (`services/service-com-amazonaws-rds-amazonrdsv19/src/state/ec2_subnets.rs`) fills
/// the same template the same way.
pub fn invalid_subnets_message(requested_subnet_ids: &[String]) -> String {
    format!(
        "Some input subnets in :[{}] are invalid.",
        requested_subnet_ids.join(", ")
    )
}

/// The Availability Zones the subnets of one subnet group have to add up to:
/// `CreateDBSubnetGroup` states it — "subnet groups must contain at least one subnet in at
/// least two Availability Zones in the Amazon Web Services Region" — and
/// `DBSubnetGroupDoesNotCoverEnoughAZs` is the fault for a list that covers fewer.
///
/// The requirement's own exception ("unless there is only one Availability Zone") never
/// applies here: every Region this emulator reports zones for offers at least three
/// ([`::service_common::aws_availability_zones`]), so there is no Region a one-zone group
/// could be the whole of.
pub const MIN_SUBNET_GROUP_AVAILABILITY_ZONES: usize = 2;

/// What a `SubnetIds` list covering too few Availability Zones is refused with: AWS states
/// the requirement, then the coverage the request's own subnets add up to, then what to do
/// about it.
///
/// The variable part is the zones those subnets COVER — read off the records the owner of
/// subnets answered with, named once each and in zone order — so a list of two subnets of
/// one zone names that one zone, and a request whose subnets this service knows no zone for
/// names none. The sibling view of this same front door
/// (`services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs`) fills the same template
/// the same way.
pub fn az_coverage_message(covered: &BTreeSet<String>) -> String {
    let coverage = covered
        .iter()
        .map(String::as_str)
        .collect::<Vec<&str>>()
        .join(", ");
    format!(
        "The DB subnet group doesn't meet Availability Zone (AZ) coverage requirement. \
         Current AZ coverage: {coverage}. Add subnets to cover at least \
         {MIN_SUBNET_GROUP_AVAILABILITY_ZONES} AZs."
    )
}

/// What a `SubnetIds` list whose subnets do not all lie in ONE VPC is refused with: the
/// sentence the `InvalidSubnet` fault states for that condition itself, which carries no
/// identifier of its own — the condition is about the list, not about one subnet of it.
///
/// The sibling view of this same front door
/// (`services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs`) answers the one fault
/// with the fault's own sentence too, so a caller reading either view sees one wording.
pub const SUBNETS_NOT_IN_ONE_VPC_MESSAGE: &str =
    "The requested subnet is not valid, or multiple subnets were requested that are not all \
     in a common virtual private cloud (VPC).";

/// The network types a subnet group whose subnets are addressed in these families supports,
/// in the order `DBSubnetGroup$SupportedNetworkTypes` lists them ("Valid Values: `IPV4` |
/// `DUAL`").
///
/// `ipv4` and `ipv6` are about EVERY subnet of the group, because a cluster is placed ACROSS
/// the group's subnets: a family one subnet of the group is not addressed in is not one the
/// group can offer. A group whose subnets carry no IPv4 block at all supports neither value,
/// since both of the published ones include IPv4 — nothing is invented for it.
pub fn group_network_types(ipv4_everywhere: bool, ipv6_everywhere: bool) -> Vec<String> {
    let mut supported = Vec::with_capacity(2);
    if ipv4_everywhere {
        supported.push(NETWORK_TYPE_IPV4.to_string());
        if ipv6_everywhere {
            supported.push(NETWORK_TYPE_DUAL.to_string());
        }
    }
    supported
}

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `region` is the request's own signing region, so the question is asked in the Region the
/// caller addressed. The endpoint URL is only the address the callback's transport routes on
/// IN PROCESS: no socket is opened, no name is resolved and nothing leaves this process. The
/// credentials are the callback's, which sign this in-process hop and never appear in a
/// response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: `None` for a value that would name an authority
/// the caller chose rather than the peer service this emulator asks.
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
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials the callback
        // hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// One subnet of the account, as EC2 reported it: everything a subnet group reports about a
/// subnet it holds, and nothing derived here.
#[derive(Debug, Clone, Default)]
pub struct ResolvedSubnet {
    /// The `AvailabilityZone` EC2 reports the subnet lies in, absent where its record named
    /// none.
    pub availability_zone: Option<String>,
    /// The `VpcId` EC2 reports owns the subnet, absent for the same reason.
    pub vpc_id: Option<String>,
    /// Whether the subnet carries an IPv4 block (`CidrBlock`).
    pub has_ipv4_cidr: bool,
    /// Whether the subnet carries an IPv6 block (`Ipv6CidrBlockAssociationSet`).
    pub has_ipv6_cidr: bool,
}

/// What the owner of subnets answered about the identifiers a request named.
///
/// An identifier that is absent here is one the account has no subnet for, or one asked
/// about in a process EC2 is not mounted in: in the second case nothing is known about it at
/// all, which [`ResolvedSubnets::absent`] keeps apart from "the account holds no such
/// subnet".
#[derive(Debug, Clone, Default)]
pub struct ResolvedSubnets {
    /// `subnet-id` -> what EC2 answered for it.
    by_id: BTreeMap<String, ResolvedSubnet>,
    /// Whether the owner of subnets ANSWERED every question asked of it. `false` where no
    /// hop could be made at all, which is the one case an absent entry says nothing about
    /// whether the account holds the subnet — so it defaults to `false`, and nothing is
    /// refused on a hop that was never answered.
    answered: bool,
}

impl ResolvedSubnets {
    /// What EC2 answered for `subnet_id`, or `None` where it answered for no such subnet.
    #[must_use]
    pub fn get(&self, subnet_id: &str) -> Option<&ResolvedSubnet> {
        self.by_id.get(subnet_id)
    }

    /// The identifiers among `subnet_ids` that EC2 ANSWERED it holds no subnet for, in the
    /// order the request named them and without repetition.
    ///
    /// EMPTY where the hop could not be made or the owner answered with an error: this
    /// service then knows nothing about the subnets, and a request is refused on what EC2
    /// said rather than on silence.
    #[must_use]
    pub fn absent(&self, subnet_ids: &[String]) -> Vec<String> {
        if !self.answered {
            return Vec::new();
        }
        let mut named: BTreeSet<&str> = BTreeSet::new();
        subnet_ids
            .iter()
            .filter(|subnet_id| !self.by_id.contains_key(subnet_id.as_str()))
            .filter(|subnet_id| named.insert(subnet_id.as_str()))
            .cloned()
            .collect()
    }

    /// The identifiers among `subnet_ids` EC2 reports in a VPC OTHER than `vpc_id`, in the
    /// order the request named them and without repetition.
    ///
    /// An identifier EC2 named no VPC for is not among them: a subnet this service knows no
    /// VPC for is no evidence of a subnet in the wrong one.
    #[must_use]
    pub fn outside_vpc(&self, subnet_ids: &[String], vpc_id: &str) -> Vec<String> {
        let mut named: BTreeSet<&str> = BTreeSet::new();
        subnet_ids
            .iter()
            .filter(|subnet_id| {
                self.by_id
                    .get(subnet_id.as_str())
                    .and_then(|subnet| subnet.vpc_id.as_deref())
                    .is_some_and(|owner| owner != vpc_id)
            })
            .filter(|subnet_id| named.insert(subnet_id.as_str()))
            .cloned()
            .collect()
    }

    /// The distinct VPCs EC2 reported for `subnet_ids`, in id order.
    #[must_use]
    pub fn vpcs(&self, subnet_ids: &[String]) -> BTreeSet<String> {
        subnet_ids
            .iter()
            .filter_map(|subnet_id| self.get(subnet_id)?.vpc_id.clone())
            .collect()
    }

    /// The VPC that owns `subnet_ids`: the one EC2 reported for every one of them.
    ///
    /// `None` where it reported none, or reported more than one — a group whose subnets lie
    /// in different VPCs has no VPC of its own, and neither has one asked about where the
    /// owner of subnets could not be reached.
    #[must_use]
    pub fn vpc_of(&self, subnet_ids: &[String]) -> Option<String> {
        let mut owners = self.vpcs(subnet_ids).into_iter();
        match (owners.next(), owners.next()) {
            (Some(vpc_id), None) => Some(vpc_id),
            _ => None,
        }
    }

    /// The Availability Zone of each of `subnet_ids` EC2 named one for, by identifier: what
    /// a group's `Subnets` report, read off EC2's records at the moment the group was
    /// written.
    #[must_use]
    pub fn zones_by_subnet(&self, subnet_ids: &[String]) -> BTreeMap<String, String> {
        subnet_ids
            .iter()
            .filter_map(|subnet_id| {
                let zone = self.get(subnet_id)?.availability_zone.clone()?;
                Some((subnet_id.clone(), zone))
            })
            .collect()
    }

    /// The zone EC2 reports the subnet `subnet_id` lies in, or `None` where it answered for
    /// no such subnet or named no zone for it.
    #[must_use]
    pub fn zone_of(&self, subnet_id: &str) -> Option<&str> {
        self.get(subnet_id)?.availability_zone.as_deref()
    }

    /// The zones `subnet_ids` COVER: the distinct zones of the subnets among them EC2 named
    /// a zone for, in zone order.
    ///
    /// A subnet lies in exactly ONE zone, so two subnets of one zone cover one zone and two
    /// subnets of two zones cover two — which is what makes this a count of coverage rather
    /// than of subnets.
    #[must_use]
    pub fn covered_zones(&self, subnet_ids: &[String]) -> BTreeSet<String> {
        subnet_ids
            .iter()
            .filter_map(|subnet_id| self.zone_of(subnet_id).map(str::to_string))
            .collect()
    }

    /// How many DISTINCT identifiers among `subnet_ids` this service knows no zone for,
    /// because EC2 answered for no such subnet or was never reached at all.
    #[must_use]
    pub fn zones_unknown(&self, subnet_ids: &[String]) -> usize {
        subnet_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<&str>>()
            .into_iter()
            .filter(|subnet_id| self.zone_of(subnet_id).is_none())
            .count()
    }

    /// The network types a group holding `subnet_ids` supports; see
    /// [`group_network_types`], which states the rule.
    ///
    /// A group holding a subnet EC2 answered nothing for supports nothing: its addressing is
    /// unknown, so no family can be said to cover every subnet of the group.
    #[must_use]
    pub fn network_types(&self, subnet_ids: &[String]) -> Vec<String> {
        if subnet_ids.is_empty() {
            return Vec::new();
        }
        let resolved: Vec<&ResolvedSubnet> = subnet_ids
            .iter()
            .filter_map(|subnet_id| self.get(subnet_id))
            .collect();
        if resolved.len() != subnet_ids.len() {
            return Vec::new();
        }
        group_network_types(
            resolved.iter().all(|subnet| subnet.has_ipv4_cidr),
            resolved.iter().all(|subnet| subnet.has_ipv6_cidr),
        )
    }
}

/// The refusal a `SubnetIds` list earns when its subnets cover fewer Availability Zones
/// than a subnet group must ([`MIN_SUBNET_GROUP_AVAILABILITY_ZONES`]), or `None` when they
/// cover enough — whichever operation carries the list.
///
/// A subnet lies in exactly ONE zone and WHICH zone is the owner of subnets' fact, so the
/// coverage is counted over the zones `resolved` carries for the request's own subnets: a
/// subnet named twice is one subnet and two subnets of one zone are one zone's worth of
/// coverage. The refusal NAMES the coverage those subnets add up to, which is the same
/// placement the group would have reported.
///
/// An identifier this service knows no zone for lies in a zone that may be any of the
/// Region's, which is not evidence that the list covers too few: it counts as a zone of its
/// own, so nothing is refused on an assumed placement and a deployment where the owner of
/// subnets cannot be asked at all refuses nothing on that silence.
///
/// Comes after the list has been held to naming subnets the account HOLDS, all of one VPC:
/// coverage is a statement about subnets that are there.
#[must_use]
pub fn zone_coverage_shortfall(
    subnet_ids: &[String],
    resolved: &ResolvedSubnets,
) -> Option<String> {
    let covered = resolved.covered_zones(subnet_ids);
    let unknown = resolved.zones_unknown(subnet_ids);
    if covered.len().saturating_add(unknown) >= MIN_SUBNET_GROUP_AVAILABILITY_ZONES {
        return None;
    }
    Some(az_coverage_message(&covered))
}

/// Every identifier in `subnet_ids` the account has a subnet for, in `region`, as EC2
/// reports it.
///
/// Identifiers are deduplicated, so the work does not grow with how many times one subnet is
/// named, and asked about in hops of at most [`SUBNET_IDS_PER_HOP`]. The CALLER has already
/// held the list to the published allowance for one subnet group
/// ([`super::MAX_SUBNETS_PER_SUBNET_GROUP`]), so the number of hops is bounded by that
/// allowance and not by the length of list a caller sent.
///
/// A hop that could not be made contributes nothing, and the caller treats those identifiers
/// as unknown: this service REFUSES a subnet it was told is not there and REPORTS what it
/// was told about one that is, and neither an assumed refusal nor an assumed placement is
/// grounded in anything EC2 said.
pub async fn resolve_subnets(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> ResolvedSubnets {
    let mut resolved = ResolvedSubnets::default();
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return resolved;
    };
    // Every hop below has to be answered for an absent entry to mean "the account holds no
    // such subnet"; one that fails takes this back off.
    resolved.answered = true;
    let distinct: Vec<&str> = subnet_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    for chunk in distinct.chunks(SUBNET_IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(SUBNET_ID_FILTER)
            .set_values(Some(chunk.iter().map(|id| (*id).to_string()).collect()))
            .build();
        match client.describe_subnets().filters(by_id).send().await {
            Ok(output) => {
                for subnet in output.subnets() {
                    let Some(subnet_id) = subnet.subnet_id() else {
                        continue;
                    };
                    resolved.by_id.insert(
                        subnet_id.to_string(),
                        ResolvedSubnet {
                            availability_zone: subnet
                                .availability_zone()
                                .map(str::to_string),
                            vpc_id: subnet.vpc_id().map(str::to_string),
                            // The address families the subnet is addressed in, as EC2 holds
                            // them: its IPv4 block, and any IPv6 block associated with it.
                            // They are what says which network types a group of this subnet
                            // supports, so they are read off the owner's record like
                            // everything else here.
                            has_ipv4_cidr: subnet
                                .cidr_block()
                                .is_some_and(|cidr| !cidr.is_empty()),
                            has_ipv6_cidr: subnet
                                .ipv6_cidr_block_association_set()
                                .iter()
                                .any(|association| {
                                    association
                                        .ipv6_cidr_block()
                                        .is_some_and(|cidr| !cidr.is_empty())
                                }),
                        },
                    );
                }
            }
            Err(error) => {
                // At `warn`: a group's placement is what a client reads to tell which zones
                // and which VPC it covers, so a deployment where this hop fails is one where
                // no group can report them — worth seeing, and it IS why an identifier
                // resolved to nothing.
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the subnets a request named could not be resolved against EC2"
                );
                resolved.answered = false;
            }
        }
    }
    resolved
}
