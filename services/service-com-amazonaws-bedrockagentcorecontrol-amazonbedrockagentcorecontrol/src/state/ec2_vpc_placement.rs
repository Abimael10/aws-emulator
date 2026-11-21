// SPDX-License-Identifier: CC0-1.0

//! The EC2 network a VPC-mode resource of this control plane is placed in,
//! resolved against the service that OWNS it.
//!
//! A `VpcConfig` names "the subnets" and "the security groups associated with
//! the VPC configuration" (`smithy_query shape=VpcConfig`) — neither of which is
//! a resource of this service. A subnet and a security group are Amazon EC2's,
//! so whether one exists is EC2's to answer, and the EC2 service mounted in THIS
//! emulator is asked (`DescribeSubnets` and `DescribeSecurityGroups`, filtered by
//! id, over the in-process callback
//! [`smithy_server_runtime::OperationContext::callback`], so this crate names no
//! other service's crate) BEFORE a resource bound to that network is stored.
//!
//! The resource is PLACED in that network: a VPC-mode browser, code interpreter
//! or agent runtime reaches the network only through the elastic network
//! interfaces the service provisions in the named subnets under the named
//! groups, so an id that resolves to nothing names nowhere to place one and the
//! request is refused with the operation's modelled `ValidationException`,
//! persisting nothing — rather than settling a resource a client would believe
//! is usable.
//!
//! Nothing here reaches the network. The callback is the framework's in-process
//! dispatcher, so no socket is opened and no address out of the request is
//! contacted; the Region label the caller signed for selects which of this
//! emulator's own regional state answers, not a host.
//!
//! This service keeps nothing of the subnet or the group beyond the ids the
//! request carried, so it only GATES on existence: an emulator with no EC2 to
//! ask learns nothing to refuse a request on and refuses nothing — a refusal is
//! grounded in what EC2 ANSWERED, never assumed from silence — and an attempt
//! that answered nothing is reported at `warn`.
//!
//! Re-exported from [`crate::state`], so a handler names `crate::state::…` for
//! these exactly as it does for the record they are about.

use super::VALIDATION_REASON_FIELD_VALIDATION_FAILED;
use crate::ops::types::{ValidationException, ValidationExceptionField, VpcConfig};

/// The member of a `VpcConfig` that names the subnets, appended to the path of
/// the configuration itself so a refusal names the member the request carried
/// the offending value in.
const SUBNETS_MEMBER: &str = "subnets";

/// The member of a `VpcConfig` that names the security groups, for the reason
/// [`SUBNETS_MEMBER`] exists.
const SECURITY_GROUPS_MEMBER: &str = "securityGroups";

/// The member path `CreateAgentRuntime` and `UpdateAgentRuntime` carry a VPC
/// placement in: `NetworkConfiguration$networkModeConfig`, the runtime family's
/// own spelling of the configuration the browser and the code interpreter call
/// `vpcConfig` (`smithy_query shape=NetworkConfiguration`).
pub const RUNTIME_VPC_CONFIG_MEMBER: &str = "networkConfiguration.networkModeConfig";

/// The name of the EC2 filter that selects a subnet BY ID.
///
/// Filtering rather than naming the ids as `subnetIds` keeps an id EC2 holds no
/// subnet for on the SUCCESS path: the read answers the subnets it resolved and
/// the ones it did not come out of the diff, so the refusal names the very value
/// the request carried, in ONE read for the whole list.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The name of the EC2 filter that selects a security group by id, for the
/// reason [`SUBNET_ID_FILTER`] is used.
const GROUP_ID_FILTER: &str = "group-id";

/// The prefix of every EC2 error code that says a value names no subnet of the
/// account: `InvalidSubnetID.NotFound` for an id no subnet carries, and
/// `InvalidSubnetID.Malformed` for a value that is not a subnet id at all. Both
/// say the same thing about a placement.
const SUBNET_REFUSAL_PREFIX: &str = "InvalidSubnetID.";

/// The prefixes of every EC2 error code that says a value names no security
/// group of the account. Unlike the subnet codes, which share one prefix, EC2
/// words `InvalidGroup.NotFound` and `InvalidGroupId.Malformed` under two — and
/// both say the same thing about a placement.
const GROUP_REFUSAL_PREFIXES: &[&str] = &["InvalidGroup.", "InvalidGroupId."];

/// Why a request naming a subnet that is not there is refused, in the
/// `fieldList` entry the refusal carries: what is wrong with the member's value.
const SUBNET_ABSENT: &str = "names a subnet that does not exist in this account and Region";

/// Why a request naming a security group that is not there is refused, in the
/// `fieldList` entry the refusal carries.
const SECURITY_GROUP_ABSENT: &str =
    "names a security group that does not exist in this account and Region";

/// The modelled validation error for a VPC placement whose `subnets` entry EC2
/// holds no subnet for.
///
/// A reference to a resource of ANOTHER service that resolves to nothing is not
/// a complaint about the member's shape — the value parsed, it just names
/// nothing — so the message identifies the MISSING RESOURCE, quotes the
/// identifier the request supplied verbatim, and states the one remedy the
/// caller has. That is the form this crate already words a cross-service
/// reference that resolves to nothing in
/// ([`super::lambda_function_not_found`]), so a browser's absent subnet and a
/// gateway target's absent function cannot be worded two ways. The two sentences
/// around the id are fixed text; the id is the request's own value.
///
/// The member path stays where the shape declares a member complaint belongs —
/// the `fieldList` — and `reason` says a field failed validation.
fn subnet_not_found(member: &str, subnet_id: &str) -> ValidationException {
    absent_network_resource(
        format!("Subnet not found: {subnet_id}. Verify the subnet ID is correct and retry."),
        &format!("{member}.{SUBNETS_MEMBER}"),
        SUBNET_ABSENT,
    )
}

/// The modelled validation error for a VPC placement whose `securityGroups`
/// entry EC2 holds no security group for; the same form [`subnet_not_found`]
/// words for a subnet, about the other member.
fn security_group_not_found(member: &str, group_id: &str) -> ValidationException {
    absent_network_resource(
        format!(
            "Security group not found: {group_id}. Verify the security group ID is correct and \
             retry."
        ),
        &format!("{member}.{SECURITY_GROUPS_MEMBER}"),
        SECURITY_GROUP_ABSENT,
    )
}

/// The one shape both refusals above take, so the two cannot drift apart.
fn absent_network_resource(message: String, member: &str, why: &str) -> ValidationException {
    ValidationException {
        message,
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: Some(vec![ValidationExceptionField {
            name: member.to_string(),
            message: why.to_string(),
        }]),
    }
}

/// The refusal the VPC placement `vpc_config` earns, or `None` when EC2 resolved
/// every id it carried — and also when the request carries no placement at all,
/// or there is no EC2 to ask.
///
/// `member` is the path through the request to the configuration itself
/// (`networkConfiguration.vpcConfig` for a browser and a code interpreter,
/// [`RUNTIME_VPC_CONFIG_MEMBER`] for an agent runtime), so the `fieldList` names
/// the member the caller supplied the value in.
///
/// The SUBNETS are resolved first: a network interface is placed in a subnet and
/// then given its groups, so a request whose subnets name nothing is answered
/// about those rather than about the groups beside them.
pub async fn unresolved_vpc_placement(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    member: &str,
    vpc_config: Option<&VpcConfig>,
) -> Option<ValidationException> {
    let vpc_config = vpc_config?;
    if vpc_config.subnets.is_empty() && vpc_config.security_groups.is_empty() {
        return None;
    }
    let ec2 = ec2_client(ctx, region)?;
    if let Some(absent) = unresolved_subnet(&ec2, &vpc_config.subnets).await {
        return Some(subnet_not_found(member, &absent));
    }
    let absent = unresolved_security_group(&ec2, &vpc_config.security_groups).await?;
    Some(security_group_not_found(member, &absent))
}

/// The first id of `subnet_ids` EC2 reports no subnet for, in ONE read filtered
/// by id, or `None` when it resolved every one of them (and when nothing could
/// be read about them at all).
///
/// The list is the one the framework's constraint walk has already held to
/// `Subnets`' own 1..16 bound, so neither the read nor the diff is sized by the
/// request.
async fn unresolved_subnet(ec2: &::aws_sdk_ec2::Client, subnet_ids: &[String]) -> Option<String> {
    if subnet_ids.is_empty() {
        return None;
    }
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(SUBNET_ID_FILTER)
        .set_values(Some(subnet_ids.to_vec()))
        .build();
    let error = match ec2.describe_subnets().filters(by_id).send().await {
        Ok(output) => {
            let reported: Vec<&str> = output
                .subnets()
                .iter()
                .filter_map(::aws_sdk_ec2::types::Subnet::subnet_id)
                .collect();
            // A read that resolved but reports no subnet for an id the request
            // named says the same thing a refusal does, and about the same id.
            return subnet_ids
                .iter()
                .find(|id| !reported.contains(&id.as_str()))
                .cloned();
        }
        Err(error) => error,
    };
    // A REFUSED read names no ONE id — a filtered read is not refused for an id
    // that resolves to nothing, so this is the malformed-value answer — and the
    // request is answered about the first id of the list it carried.
    let named = subnet_ids.first()?;
    grounds_a_refusal(&error, &[SUBNET_REFUSAL_PREFIX], "the subnets")
        .then(|| named.clone())
}

/// The first id of `security_group_ids` EC2 reports no security group for, by
/// the rule [`unresolved_subnet`] follows for the other member.
async fn unresolved_security_group(
    ec2: &::aws_sdk_ec2::Client,
    security_group_ids: &[String],
) -> Option<String> {
    if security_group_ids.is_empty() {
        return None;
    }
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(GROUP_ID_FILTER)
        .set_values(Some(security_group_ids.to_vec()))
        .build();
    let error = match ec2.describe_security_groups().filters(by_id).send().await {
        Ok(output) => {
            let reported: Vec<&str> = output
                .security_groups()
                .iter()
                .filter_map(::aws_sdk_ec2::types::SecurityGroup::group_id)
                .collect();
            return security_group_ids
                .iter()
                .find(|id| !reported.contains(&id.as_str()))
                .cloned();
        }
        Err(error) => error,
    };
    let named = security_group_ids.first()?;
    grounds_a_refusal(&error, GROUP_REFUSAL_PREFIXES, "the security groups")
        .then(|| named.clone())
}

/// Whether an EC2 read that FAILED grounds a refusal: its error code says one of
/// the ids names no such resource of the account.
///
/// Anything else is this emulator failing to ASK rather than EC2 reporting a bad
/// id — there is no EC2 mounted, or the answer could not be read — and nothing
/// is what a request is refused on then. It is worth a warning, because a
/// deployment with no EC2 to ask is one where this gate does not run.
fn grounds_a_refusal<
    E: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::error::Error + 'static,
>(
    error: &::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
    refusal_prefixes: &[&str],
    named: &str,
) -> bool {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    if error.code().is_some_and(|code| {
        refusal_prefixes
            .iter()
            .any(|prefix| code.starts_with(prefix))
    }) {
        return true;
    }
    ::tracing::warn!(
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "{named} a request named could not be resolved against EC2"
    );
    false
}

/// The EC2 client this service resolves a caller-named placement through: the
/// in-process callback into this same emulator, at the endpoint of the Region
/// the request was signed for — `None` when that value is not a Region LABEL.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends
/// up in the endpoint URL's authority, so it is read as a Region label
/// (`service_common::arn::region_label`) instead of being interpolated as given:
/// a value carrying a host or path delimiter would name an authority the caller
/// chose rather than the peer service this emulator asks.
fn ec2_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let Some(region) = ::service_common::arn::region_label(region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return None;
    };
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or
        // watch for a stalled stream, and each of those would need a sleep
        // implementation this client is not given. The same goes for caching the
        // identity: the credentials are handed over directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}
