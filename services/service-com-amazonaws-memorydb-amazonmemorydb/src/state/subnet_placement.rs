// SPDX-License-Identifier: CC0-1.0

//! The VPC subnets a subnet group places a cluster in, resolved against the service that
//! OWNS them.
//!
//! A subnet group is "a collection of subnets (typically private) that you can designate
//! for your clusters running in an Amazon Virtual Private Cloud (VPC) environment", and
//! every one of its `SubnetIds` names an Amazon EC2 subnet — none of which is MemoryDB's.
//! So which VPC a group is in and which Availability Zone each of its subnets sits in are
//! EC2's to answer, and the EC2 service mounted in THIS emulator is asked —
//! `DescribeSubnets` over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate — BEFORE any subnet group is written. That is why
//! `SubnetGroup$VpcId` and each `Subnet$AvailabilityZone` are values of this store at all:
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
//! nothing: the group is stored with the identifiers it was given and reports no VPC and no
//! zone, because a refusal is grounded in what EC2 reported and never assumed from silence.

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

/// One subnet of a stored subnet group: the identifier the request listed, and the
/// Availability Zone the service that owns it reports it in (`None` where no EC2 answered
/// about it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSubnet {
    pub identifier: String,
    pub availability_zone: Option<String>,
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
    /// The request names a subnet that service holds none for, or subnets that are not all
    /// in one VPC. The caller maps the message onto its modelled `InvalidSubnet` /
    /// `SubnetNotAllowedFault`.
    Refused(SubnetRefusal),
}

/// Why a subnet list cannot be stored, in the two classes MemoryDB models separately: a
/// value that names no subnet of the account ([`InvalidSubnet`]), and subnets this service
/// will not place one group across ([`SubnetNotAllowedFault`]).
///
/// [`InvalidSubnet`]: crate::ops::types::InvalidSubnet
/// [`SubnetNotAllowedFault`]: crate::ops::types::SubnetNotAllowedFault
#[derive(Debug, Clone)]
pub enum SubnetRefusal {
    /// The sentence the modelled `InvalidSubnet` carries.
    Invalid(String),
    /// The sentence the modelled `SubnetNotAllowedFault` carries.
    NotAllowed(String),
}

/// The sentence this service states for a subnet identifier that names no subnet of the
/// account: the modelled `InvalidSubnet` wording about the identifier the request carried,
/// which is the SAME sentence a value of the wrong form is refused with
/// ([`super::invalid_subnet_message`]) — one wording for the one condition.
fn absent_subnet_message(subnet_id: &str) -> String {
    super::invalid_subnet_message(subnet_id)
}

/// The sentence this service states for subnets that are not all in one VPC: a subnet group
/// designates subnets "for your clusters running in an Amazon Virtual Private Cloud (VPC)
/// environment" — one VPC — so a list spanning two is a placement no cluster can be made
/// in, which is what `SubnetNotAllowedFault` is modelled for.
fn mixed_vpc_message() -> String {
    "The requested subnets are not all in a common VPC.".to_string()
}

/// Resolve the subnets `subnet_ids` names against Amazon EC2, for a request signed for
/// `region`.
///
/// Every identifier has already been held to the subnet-id FORM by the caller
/// ([`super::subnet_id_rejection`]); this is the other half — whether the account really
/// holds those subnets, and where they are. An empty list resolves to an empty placement:
/// there is nothing to ask EC2 about.
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
    let answered = ec2_client(ctx, region)
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await;
    let Ok(output) = answered else {
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
                })
                .collect(),
        });
    };
    let reported = output.subnets();
    let mut subnets = Vec::with_capacity(subnet_ids.len());
    let mut vpc_ids: Vec<String> = Vec::new();
    for subnet_id in subnet_ids {
        // A read that resolved but reports no subnet for an id the request named says the
        // same thing a refusal does, and about the same id.
        let Some(found) = reported
            .iter()
            .find(|subnet| subnet.subnet_id() == Some(subnet_id.as_str()))
        else {
            return SubnetPlacement::Refused(SubnetRefusal::Invalid(absent_subnet_message(
                subnet_id,
            )));
        };
        if let Some(vpc_id) = found.vpc_id() {
            if !vpc_ids.iter().any(|held| held == vpc_id) {
                vpc_ids.push(vpc_id.to_string());
            }
        }
        subnets.push(StoredSubnet {
            identifier: subnet_id.clone(),
            availability_zone: found.availability_zone().map(str::to_string),
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
