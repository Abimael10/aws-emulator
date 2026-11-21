// SPDX-License-Identifier: CC0-1.0

//! The account's subnets: a fact this service RESOLVES rather than holds.
//!
//! Every create in this API that places a resource in a client VPC takes the subnets the
//! elastic network interfaces go in (`BrokerNodeGroupInfo$ClientSubnets`,
//! `VpcConfig$SubnetIds`, `CreateVpcConnectionRequest$ClientSubnets`,
//! `KafkaClusterClientVpcConfig$SubnetIds`). A subnet is not a resource of this service:
//! it is Amazon EC2's, so what a request names is resolved against EC2, reached the way
//! any emulated service reaches another — through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate.
//!
//! Every refusal built here is grounded in what EC2 ANSWERED. An emulator with no EC2
//! mounted knows nothing about the account's subnets, and nothing is what it refuses on: a
//! refusal is never assumed from silence.
//!
//! What the CALLER is told, though, is Amazon MSK's own sentence for a placement it could
//! not resolve ([`CLIENT_SUBNETS_ABSENT_MESSAGE`]) — fixed text, with no subnet id of the
//! request and nothing of EC2's own answer in it. MSK answers for the request it was given
//! rather than reporting the call it made on the caller's behalf; the member at fault is
//! named by the refusal's `invalidParameter`, which is the handler's to pass.
//!
//! Whether those subnets exist is one question; WHICH VPC holds them is the other, and a
//! request that declares the VPC it is placed in (`CreateVpcConnectionRequest$VpcId`) is
//! answered about both: a placement's elastic network interfaces are created inside a subnet
//! and attached to security groups, and each of those belongs to exactly one VPC, so a
//! request naming one of another VPC describes a placement that cannot be made
//! ([`subnet_outside_vpc_refusal`], [`security_group_outside_vpc_refusal`]). Those are the
//! only EC2 facts this module reads: nothing here is stored, and a VPC the request declares
//! is compared against what EC2 reported rather than looked up on its own.

/// An AWS SDK client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service:
/// it routes in process, so no network client is configured here. `region` is the
/// request's own signing region.
///
/// This crate's ONE such seam, re-exported below for every module that resolves a fact
/// another service owns — the subnets here, the SCRAM secrets in
/// [`super::scram_secret_admission`] — so there is a single place that says how this
/// service calls a peer.
macro_rules! callback_client {
    ($sdk:ident, $ctx:expr, $endpoint:expr, $region:expr) => {{
        let config = ::$sdk::Config::builder()
            .behavior_version(::$sdk::config::BehaviorVersion::latest())
            .http_client($ctx.callback().http_client())
            .credentials_provider($ctx.callback().aws_credentials_provider())
            .region(::$sdk::config::Region::new($region.to_string()))
            .endpoint_url($endpoint)
            // The transport is in-process: there is nothing to retry, time out or watch
            // for a stalled stream, and each of those would need a sleep implementation
            // this client is not given. The same goes for caching the identity: the
            // credentials are handed over directly.
            .retry_config(::$sdk::config::retry::RetryConfig::disabled())
            .timeout_config(::$sdk::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::$sdk::config::IdentityCache::no_cache())
            .stalled_stream_protection(::$sdk::config::StalledStreamProtectionConfig::disabled())
            .build();
        ::$sdk::Client::from_conf(config)
    }};
}

pub(crate) use callback_client;

/// How many subnets ONE create may name as a placement.
///
/// CHOSEN as a process-safety backstop, not a published AWS quota (the model declares no
/// `length` on any of those lists): the list comes from the request, and it is both cloned
/// into the resolving call below and compared against what EC2 reports, so without a
/// ceiling one create would size that work. The clamp is applied BEFORE either, and it is
/// the same ceiling this service already puts on the subnets a serverless VPC
/// configuration may name — one answer to "how many subnets may a create name", sized well
/// above the handful of Availability Zones a cluster is really spread over.
pub const MAX_PLACEMENT_SUBNETS: usize = super::MAX_SERVERLESS_VPC_ENTRIES;

/// The prefix of every EC2 error code that says a value names no subnet of the account:
/// `InvalidSubnetID.NotFound` for an id no subnet carries, `InvalidSubnetID.Malformed` for
/// a value that is not a subnet id at all. Both say the same thing about a placement.
const SUBNET_ID_REFUSAL_PREFIX: &str = "InvalidSubnetID.";

/// What Amazon MSK answers a create whose client subnets it could not resolve against the
/// account's network.
///
/// A CONSTANT of the service, not built from the request (the apostrophe is U+2019, as the
/// service writes it): the ids the request named are not echoed back, nor is the count of
/// them, nor anything of the answer EC2 gave the resolving call — one sentence for every
/// create that places a resource in a client VPC it cannot be placed in, whichever member
/// named the subnets. That member is reported in the refusal's `invalidParameter` by the
/// handler that knows it.
pub const CLIENT_SUBNETS_ABSENT_MESSAGE: &str =
    "One or more of the specified client subnets don\u{2019}t exist.";

/// Why the subnets `subnet_ids` names cannot be placed in, or `None` when they can — which
/// includes the case where this emulator has no EC2 to ask, because a refusal has to be
/// grounded in what EC2 answered rather than assumed from silence.
///
/// The REFUSING half of [`resolve_placement_subnets`], for every caller that places a
/// resource in the subnets a request names without caring which Availability Zones they
/// span.
pub async fn placement_subnet_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<String> {
    match resolve_placement_subnets(ctx, region, subnet_ids).await {
        PlacementSubnets::Refused(reason) => Some(reason),
        PlacementSubnets::Resolved(_) | PlacementSubnets::Unknown => None,
    }
}

/// How many security groups ONE create may attach a placement to.
///
/// CHOSEN as a process-safety backstop, not a published AWS quota (the model declares no
/// `length` on `CreateVpcConnectionRequest$SecurityGroups`), for the same reason
/// [`MAX_PLACEMENT_SUBNETS`] exists: the list comes from the request and is both cloned into
/// the resolving call below and walked against what EC2 answered, so without a ceiling one
/// create would size that work. The same ceiling, because it is the same question — how many
/// EC2 ids may one create name.
pub const MAX_ATTACHED_SECURITY_GROUPS: usize = MAX_PLACEMENT_SUBNETS;

/// The EC2 filter that selects subnets by id (`DescribeSubnetsRequest$Filters`, `subnet-id`).
///
/// The membership reading below narrows with a FILTER rather than with `SubnetIds`, because
/// it asks a different question from the existence gate: it needs what EC2 holds for the ids
/// it does resolve, and a filtered read answers that for those while simply not reporting
/// the rest, where naming an absent id makes EC2 refuse the whole read.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The EC2 filter that selects security groups by id
/// (`DescribeSecurityGroupsRequest$Filters`, `group-id`), used for the reason
/// [`SUBNET_ID_FILTER`] is.
const SECURITY_GROUP_ID_FILTER: &str = "group-id";

/// What Amazon MSK answers a VPC connection whose client subnet EC2 reports in a VPC other
/// than the one the request declares.
///
/// No recording of this refusal was available, so the sentence is built from the two members'
/// OWN documentation (`ClientSubnets` is "the list of client subnets", `VpcId` "the VPC ID of
/// VPC connection") rather than from another refusal's wording. Its two variable parts are
/// the caller's own request: the id it named and the VPC it declared.
#[must_use]
pub fn subnet_outside_vpc_message(subnet_id: &str, vpc_id: &str) -> String {
    format!("'{subnet_id}' is not a subnet of the VPC '{vpc_id}'.")
}

/// The same refusal for a SECURITY GROUP EC2 reports in another VPC
/// ([`subnet_outside_vpc_message`] for how the sentence is arrived at; `SecurityGroups` is
/// "the list of security groups").
#[must_use]
pub fn security_group_outside_vpc_message(group_id: &str, vpc_id: &str) -> String {
    format!("'{group_id}' is not a security group of the VPC '{vpc_id}'.")
}

/// Why the subnets `subnet_ids` names cannot carry a placement in the VPC `vpc_id` declares,
/// or `None` when every subnet EC2 reported for them is that VPC's.
///
/// A placement's elastic network interfaces live in ONE VPC: an interface is created inside a
/// subnet, and a subnet belongs to exactly one VPC, so a request that declares the VPC it is
/// placed in and names a subnet of another VPC describes a placement that cannot be made.
/// Which VPC holds a subnet is EC2's fact, like the subnet itself, so EC2 is asked
/// ([`resolve_placement_subnets`] asks the existence question of the same inventory) and the
/// answer decides WHETHER the create is refused, not what the refusal says.
///
/// `None` — refuse nothing — for everything that is not EC2 reporting a subnet of another
/// VPC: an id it reports no subnet for (that absence is the existence gate's to answer), a
/// subnet it reports no VPC for, an answer this emulator could not read, and an emulator with
/// no EC2 to ask. A refusal is grounded in what EC2 answered rather than assumed from
/// silence.
pub async fn subnet_outside_vpc_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
    subnet_ids: &[String],
) -> Option<String> {
    if vpc_id.is_empty() || subnet_ids.is_empty() {
        return None;
    }
    // The caller-sized list is clamped before it is cloned into the call below or walked
    // against what EC2 answered, exactly as the existence reading clamps it.
    if subnet_ids.len() > MAX_PLACEMENT_SUBNETS {
        return Some(too_many_ids("subnets", MAX_PLACEMENT_SUBNETS, subnet_ids.len()));
    }
    let endpoint = peer_endpoint("ec2", region)?;
    let answer = callback_client!(aws_sdk_ec2, ctx, endpoint, region)
        .describe_subnets()
        .filters(id_filter(SUBNET_ID_FILTER, subnet_ids))
        .send()
        .await;
    let output = match answer {
        Ok(output) => output,
        Err(error) => {
            warn_unresolved(&error, "the subnets");
            return None;
        }
    };
    first_outside_vpc(subnet_ids, vpc_id, |subnet_id| {
        output
            .subnets()
            .iter()
            .find(|subnet| subnet.subnet_id() == Some(subnet_id))
            .and_then(::aws_sdk_ec2::types::Subnet::vpc_id)
    })
    .map(|subnet_id| subnet_outside_vpc_message(subnet_id, vpc_id))
}

/// Why the security groups `group_ids` names cannot be attached to a placement in the VPC
/// `vpc_id` declares, or `None` when every group EC2 reported for them is that VPC's.
///
/// The same rule [`subnet_outside_vpc_refusal`] applies to the subnets, for the other EC2
/// resource a placement's network interfaces are created with: a security group belongs to
/// one VPC and can only be attached to an interface in that VPC, so a group of another VPC
/// describes a placement that cannot be made. The grounds and the non-answers are that
/// reading's too — only EC2 reporting a group of another VPC refuses anything.
pub async fn security_group_outside_vpc_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
    group_ids: &[String],
) -> Option<String> {
    if vpc_id.is_empty() || group_ids.is_empty() {
        return None;
    }
    if group_ids.len() > MAX_ATTACHED_SECURITY_GROUPS {
        return Some(too_many_ids(
            "security groups",
            MAX_ATTACHED_SECURITY_GROUPS,
            group_ids.len(),
        ));
    }
    let endpoint = peer_endpoint("ec2", region)?;
    let answer = callback_client!(aws_sdk_ec2, ctx, endpoint, region)
        .describe_security_groups()
        .filters(id_filter(SECURITY_GROUP_ID_FILTER, group_ids))
        .send()
        .await;
    let output = match answer {
        Ok(output) => output,
        Err(error) => {
            warn_unresolved(&error, "the security groups");
            return None;
        }
    };
    first_outside_vpc(group_ids, vpc_id, |group_id| {
        output
            .security_groups()
            .iter()
            .find(|group| group.group_id() == Some(group_id))
            .and_then(::aws_sdk_ec2::types::SecurityGroup::vpc_id)
    })
    .map(|group_id| security_group_outside_vpc_message(group_id, vpc_id))
}

/// The EC2 filter that selects the resources of one VPC (`vpc-id`), used for the reason
/// [`SUBNET_ID_FILTER`] is.
const VPC_ID_FILTER: &str = "vpc-id";

/// The EC2 filter that selects security groups by the name they carry
/// (`DescribeSecurityGroupsRequest$Filters`, `group-name`).
const SECURITY_GROUP_NAME_FILTER: &str = "group-name";

/// The name of the group every VPC comes with: "Your VPC automatically comes with a default
/// security group" (Amazon VPC User Guide, "Default security groups"), which is the group a
/// placement that attaches none of its own lands in.
const DEFAULT_SECURITY_GROUP_NAME: &str = "default";

/// Record, in `groups`, the security groups a placement in `subnet_ids` is ATTACHED to when
/// the request named none of its own — the VPC's default group
/// ([`default_placement_security_groups`]) — and leave a list the request did name exactly
/// as it named it.
///
/// The ONE application of that rule, so every create that places a resource in a client VPC
/// stores the attachment the same way: a member the request left out, and one it set to the
/// empty list, both name no group of their own, and an interface is never attached to
/// nothing. Applied to the configuration that is STORED, so every reader answers from the
/// one stored copy instead of re-deriving it.
pub async fn attach_default_security_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
    groups: &mut Option<Vec<String>>,
) {
    if groups.as_ref().is_some_and(|named| !named.is_empty()) {
        return;
    }
    if let Some(attached) = default_placement_security_groups(ctx, region, subnet_ids).await {
        *groups = Some(attached);
    }
}

/// The security groups a placement in `subnet_ids` is ATTACHED to when the request names
/// none of its own: the default security group of each VPC EC2 reports those subnets in, in
/// the order the request named the subnets — and `None` when nothing about them could be
/// resolved.
///
/// A placement's elastic network interfaces are created inside those subnets and are always
/// attached to at least one security group, so a request that names none is attached to the
/// VPC's default group — which is then what the resource REPORTS as its security groups. The
/// groups of the account are EC2's, like the subnets, so they are read off EC2 rather than
/// invented here: which VPC holds a subnet comes from the same `DescribeSubnets` the
/// existence gate asks, and that VPC's default group from `DescribeSecurityGroups`.
///
/// `None` — nothing to attach, so the member keeps whatever the request said — for
/// everything that is not EC2 reporting a default group: an emulator with no EC2 to ask, a
/// read it could not make, subnets it reports no VPC for, and a VPC it reports no default
/// group for. An attachment is grounded in what EC2 answered rather than assumed from
/// silence, exactly as a refusal is.
pub async fn default_placement_security_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<Vec<String>> {
    // The caller-sized list is clamped before it is cloned into either call below or walked
    // against what EC2 answered, exactly as the readings above clamp it.
    if subnet_ids.is_empty() || subnet_ids.len() > MAX_PLACEMENT_SUBNETS {
        return None;
    }
    let endpoint = peer_endpoint("ec2", region)?;
    let client = callback_client!(aws_sdk_ec2, ctx, endpoint, region);
    let subnets = match client
        .describe_subnets()
        .filters(id_filter(SUBNET_ID_FILTER, subnet_ids))
        .send()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            warn_unresolved(&error, "the subnets");
            return None;
        }
    };
    // The VPCs those subnets are in, each ONCE and in the order the request named them: a
    // configuration may name several subnets of one VPC, which is one network interface's
    // worth of groups, not one per subnet.
    let mut vpc_ids: Vec<String> = Vec::new();
    for subnet_id in subnet_ids {
        let held_by = subnets
            .subnets()
            .iter()
            .find(|subnet| subnet.subnet_id() == Some(subnet_id.as_str()))
            .and_then(::aws_sdk_ec2::types::Subnet::vpc_id);
        if let Some(vpc_id) = held_by {
            if !vpc_ids.iter().any(|held| held == vpc_id) {
                vpc_ids.push(vpc_id.to_string());
            }
        }
    }
    if vpc_ids.is_empty() {
        return None;
    }
    let groups = match client
        .describe_security_groups()
        .filters(id_filter(VPC_ID_FILTER, &vpc_ids))
        .filters(id_filter(
            SECURITY_GROUP_NAME_FILTER,
            &[DEFAULT_SECURITY_GROUP_NAME.to_string()],
        ))
        .send()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            warn_unresolved(&error, "the default security groups");
            return None;
        }
    };
    let attached: Vec<String> = vpc_ids
        .iter()
        .filter_map(|vpc_id| {
            groups
                .security_groups()
                .iter()
                .find(|group| {
                    group.vpc_id() == Some(vpc_id.as_str())
                        && group.group_name() == Some(DEFAULT_SECURITY_GROUP_NAME)
                })
                .and_then(::aws_sdk_ec2::types::SecurityGroup::group_id)
                .map(str::to_string)
        })
        .collect();
    (!attached.is_empty()).then_some(attached)
}

/// The first id of `ids` that `held_by` reports in a VPC other than `vpc_id`, or `None` when
/// none of them is reported in another VPC.
///
/// The ONE statement of the membership rule both readings above take, so the two kinds of EC2
/// resource are judged the same way and only the answer they are read out of differs. Walked
/// over the ids the REQUEST named, so a refusal names the caller's own first offending entry
/// rather than whichever row EC2 happened to report first; an id `held_by` answers nothing for
/// establishes nothing, for the reason each reading's own documentation gives.
fn first_outside_vpc<'a>(
    ids: &'a [String],
    vpc_id: &str,
    held_by: impl Fn(&str) -> Option<&'a str>,
) -> Option<&'a String> {
    ids.iter()
        .find(|id| held_by(id).is_some_and(|holder| holder != vpc_id))
}

/// What a reading answers a list of EC2 ids longer than the ceiling it asks about
/// ([`MAX_PLACEMENT_SUBNETS`], [`MAX_ATTACHED_SECURITY_GROUPS`]) with, in the ONE wording every
/// one of them uses: `kind` names the resources as the sentence reads them, so the ceilings
/// cannot drift into different sentences for the same condition.
fn too_many_ids(kind: &str, limit: usize, named: usize) -> String {
    format!("A create may name at most {limit} {kind}, not {named}.")
}

/// The EC2 filter that selects the resources `values` names by the id filter `name`.
fn id_filter(name: &str, values: &[String]) -> ::aws_sdk_ec2::types::Filter {
    ::aws_sdk_ec2::types::Filter::builder()
        .name(name)
        .set_values(Some(values.to_vec()))
        .build()
}

/// Report an EC2 read that answered nothing about what a request named.
///
/// Worth a warning for the reason the existence reading gives: a deployment with no EC2 to
/// ask is one where the gate behind this read does not run, and a request is refused on what
/// EC2 answered rather than on a read that failed.
fn warn_unresolved<E>(
    error: &::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
    named: &str,
) where
    E: ::std::error::Error + ::std::fmt::Debug + 'static,
{
    ::tracing::warn!(
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "{named} a request named could not be resolved against EC2"
    );
}

/// What EC2 answered about the subnets a request named as a placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementSubnets {
    /// EC2 reported a subnet for every id the request named. The payload is the
    /// Availability Zones those subnets span — each zone once — when EC2 reported a zone
    /// for every one of them, and `None` when it did not: which zones a placement spans
    /// is then simply not known, and nothing can be refused on it.
    Resolved(Option<std::collections::BTreeSet<String>>),
    /// Amazon MSK refuses this placement, in its own sentence.
    Refused(String),
    /// EC2 could not be asked at all, so nothing about the placement is known — the same
    /// standing as a fact nobody answered, which grounds no refusal.
    Unknown,
}

/// Resolve the subnets `subnet_ids` names against the account's EC2 inventory, and report
/// both whether they can be placed in and which Availability Zones they span.
///
/// Two questions, in this order: whether the list is within [`MAX_PLACEMENT_SUBNETS`],
/// which is answered from the request alone and so bounds everything below it, and then
/// whether EC2 resolves every id it carries.
///
/// Amazon MSK creates the elastic network interfaces a cluster (or a VPC connection, or a
/// replicator's client) is reached through INSIDE the subnets a request names, so it
/// resolves them before it provisions anything. The subnets of the account are EC2's, so
/// EC2 is asked: it answers `InvalidSubnetID.NotFound` for an id no subnet of the account
/// carries, and an id it reports no subnet for says the same thing. Either way the caller
/// is answered MSK's own [`CLIENT_SUBNETS_ABSENT_MESSAGE`] — EC2's answer decides WHETHER
/// the create is refused, not what the refusal says.
///
/// The Availability Zone of a subnet is EC2's fact too, which is why it is read off the
/// SAME answer rather than guessed from the list: a cluster's brokers are spread over the
/// zones its client subnets are in, so an admission rule about those zones is grounded in
/// what EC2 reported or is not applied at all.
pub async fn resolve_placement_subnets(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> PlacementSubnets {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    if subnet_ids.is_empty() {
        return PlacementSubnets::Resolved(Some(std::collections::BTreeSet::new()));
    }
    // The caller-sized list is clamped here, before it is cloned into the call below or
    // walked against what EC2 answered, so neither is sized by the request.
    if subnet_ids.len() > MAX_PLACEMENT_SUBNETS {
        return PlacementSubnets::Refused(too_many_ids(
            "subnets",
            MAX_PLACEMENT_SUBNETS,
            subnet_ids.len(),
        ));
    }
    // No Region label, so no endpoint to ask EC2 at: the same answer an unreachable EC2
    // gets, which grounds no refusal.
    let Some(endpoint) = peer_endpoint("ec2", region) else {
        return PlacementSubnets::Unknown;
    };
    let answer = callback_client!(aws_sdk_ec2, ctx, endpoint, region)
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await;
    let error = match answer {
        Ok(output) => {
            // EC2 reports one subnet per id it resolved, so an id it reported nothing for
            // names no subnet of the account — the same thing the refusal below says.
            let mut zones = Some(std::collections::BTreeSet::new());
            for id in subnet_ids {
                let Some(subnet) = output
                    .subnets()
                    .iter()
                    .find(|subnet| subnet.subnet_id() == Some(id.as_str()))
                else {
                    return PlacementSubnets::Refused(CLIENT_SUBNETS_ABSENT_MESSAGE.to_string());
                };
                // A subnet EC2 reports no zone for leaves the SPAN of the placement
                // unknown; the placement itself still resolved.
                match (subnet.availability_zone(), zones.as_mut()) {
                    (Some(zone), Some(spanned)) => {
                        spanned.insert(zone.to_string());
                    }
                    (None, _) => zones = None,
                    (Some(_), None) => {}
                }
            }
            return PlacementSubnets::Resolved(zones);
        }
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default()
        .to_string();
    // Any other answer is this emulator failing to ask rather than EC2 reporting a subnet
    // the account does not have, so it grounds no refusal — and it is worth a warning,
    // because a deployment with no EC2 to ask is one where this gate does not run.
    if !code.starts_with(SUBNET_ID_REFUSAL_PREFIX) {
        ::tracing::warn!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the subnets a request named could not be resolved against EC2"
        );
        return PlacementSubnets::Unknown;
    }
    PlacementSubnets::Refused(CLIENT_SUBNETS_ABSENT_MESSAGE.to_string())
}

/// The endpoint the in-process callback reaches `service` in `region` at, or `None` when
/// `region` is not a Region LABEL.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in an
/// endpoint URL's authority rather than in a response, so it is read as a Region label
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: a value carrying a host or path delimiter would
/// name an authority the caller chose rather than the peer service this emulator asks.
pub(crate) fn peer_endpoint(service: &str, region: &str) -> Option<String> {
    let Some(region) = ::service_common::arn::region_label(region) else {
        ::tracing::warn!(
            peer = %service,
            "the request names no Region label, so {service} cannot be asked"
        );
        return None;
    };
    Some(format!("https://{service}.{region}.amazonaws.com"))
}
