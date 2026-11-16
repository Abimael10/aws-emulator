// SPDX-License-Identifier: CC0-1.0

//! The managed VPC endpoint a workgroup is reached through, created in the service that OWNS
//! VPC endpoints and network interfaces.
//!
//! `Workgroup$endpoint` is "the endpoint that is created from the workgroup", and what that
//! endpoint IS inside a VPC is an interface VPC endpoint with one endpoint network interface
//! in each subnet the workgroup carries: `Endpoint$vpcEndpoints` is "an array of VpcEndpoint
//! objects", each one carrying "the connection endpoint ID for connecting to Amazon Redshift
//! Serverless" (`VpcEndpoint$vpcEndpointId`), "the VPC identifier that the endpoint is
//! associated with" (`VpcEndpoint$vpcId`) and "one or more network interfaces of the
//! endpoint" — each naming its subnet, that subnet's Availability Zone and "the IPv4 address
//! of the network interface within the subnet" (`NetworkInterface$privateIpAddress`).
//!
//! A subnet, a VPC endpoint and a network interface are all Amazon EC2's resources, so none of
//! the values the workgroup REPORTS is minted or derived here. This service ASKS EC2 which VPC
//! the workgroup's subnets belong to, asks it to create the endpoint in those subnets, and
//! reports the ids, the zones and the addresses EC2 answered: a client cross-checking
//! `GetWorkgroup` against `ec2:DescribeVpcEndpoints` / `ec2:DescribeNetworkInterfaces` sees one
//! account's network rather than two, and each interface's address is taken out of its own
//! subnet's range by the store that hands out every other address of that subnet.
//! `services/common/src/network.rs` states that contract for every service placing a resource
//! in a subnet.
//!
//! The one value this module does mint is the endpoint SERVICE the endpoint connects to
//! ([`new_endpoint_service_id`]), and that one is not the account's to hold: the service in
//! front of a workgroup's compute belongs to Amazon Redshift Serverless, so EC2 holds no
//! configuration for it and `ec2:DescribeVpcEndpointServices` — which reports the Region's
//! published services and THIS account's own configurations — would not list it whatever this
//! module did. That is why such a name is read for its form alone by the service that owns
//! endpoints, and `VpcEndpoint` carries no service name for a client to read it back by.
//!
//! The ask goes over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), the seam every service of this
//! emulator reaches another one through: this crate names no other service's crate, no socket
//! is opened, no host name is resolved and nothing leaves the process. The credentials are the
//! callback's own, which sign the hop and never appear in a response.
//!
//! An ask that answers no endpoint is one this emulator could not make — there is no EC2 in
//! this process — or one EC2 refused, because the account holds no subnet by the ids the
//! workgroup carries, or none of them is a subnet an endpoint can be placed in. Nothing of
//! this service is REFUSED on that answer: no operation here gates a workgroup on the subnets
//! it names, and the one it would gate — `CreateWorkgroup` — keeps serving a create naming
//! subnets the account does not hold, so an id EC2 reports no subnet for is read here only as
//! "there is no endpoint to report". That is the reading `services/common/src/network.rs`
//! gives a service which keeps nothing of a placement it did not resolve, as against one that
//! places a resource in a subnet and refuses a create it could not resolve; whether this
//! operation should refuse such an id at all is that operation's own gate and a change of its
//! own. Either way the workgroup reports NO managed endpoint rather than one in a VPC nobody
//! confirmed, and the attempt is traced at `warn`.
//!
//! The endpoint is created once, by the write that materialises the workgroup, and the ids,
//! zones and addresses EC2 answered are STORED on the record — so `GetWorkgroup` and
//! `ListWorkgroups` report them without asking again, and no read of a workgroup writes
//! anything.
//!
//! Nothing here holds this service's store: every function takes the values it needs and is
//! awaited with no guard held, so an endpoint is never created or deleted behind a lock.

use ::std::collections::{BTreeMap, BTreeSet};

use crate::ops::types::{NetworkInterface, VpcEndpoint};

/// The `VpcEndpointType` a workgroup's endpoint is created as: an INTERFACE endpoint, the type
/// that is reached through endpoint network interfaces in subnets — which is what the
/// interfaces `VpcEndpoint$networkInterfaces` reports are.
const WORKGROUP_ENDPOINT_TYPE: ::aws_sdk_ec2::types::VpcEndpointType =
    ::aws_sdk_ec2::types::VpcEndpointType::Interface;

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter asks the question rather than `SubnetIds`: an id the account has no subnet for is
/// then an absent row instead of a failure of the whole call, so every id the workgroup
/// carries is still resolved against what EC2 does hold.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The id prefix of an endpoint service of an account (`vpce-svc-…`), which is what the
/// endpoint service behind a workgroup's endpoint is named by.
const ENDPOINT_SERVICE_ID_PREFIX: &str = "vpce-svc";

/// How many hexadecimal digits an endpoint service id carries after its prefix, the shape such
/// an id takes.
const ENDPOINT_SERVICE_ID_DIGITS: usize = 17;

/// The most subnet ids ONE workgroup's endpoint is resolved and placed over.
///
/// `Workgroup$subnetIds` carries no `@length`, so the list is as long as a caller chooses:
/// this caps both the ids one `DescribeSubnets` hop asks about and the subnets one endpoint is
/// placed in, so the work a create does is a constant times what the account stores rather
/// than something the request sizes. Well past the subnets a workgroup can usefully have —
/// an endpoint takes one subnet per Availability Zone, and no Region publishes this many.
const MAX_ENDPOINT_SUBNETS: usize = 32;

/// The endpoint service the workgroup's endpoint is an endpoint TO, minted per endpoint:
/// `vpce-svc-` plus [`ENDPOINT_SERVICE_ID_DIGITS`] hexadecimal digits.
///
/// A workgroup is not reached through a service Amazon Web Services publishes in the Region —
/// it is reached through the endpoint service standing in front of that one workgroup's
/// compute, which belongs to Redshift Serverless rather than to the caller's account (see the
/// module docs on why no account-held configuration could name it). So the endpoint names a
/// service of the form another account's service takes, and the id is minted the way every
/// other server-generated id of this service is ([`crate::state::new_resource_id`], the same
/// random bytes rendered as this id's digits).
fn new_endpoint_service_id() -> String {
    let digits = ::uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{ENDPOINT_SERVICE_ID_PREFIX}-{}",
        &digits[..ENDPOINT_SERVICE_ID_DIGITS]
    )
}

/// The endpoint service NAME an endpoint to the service `service_id` names it by:
/// `com.amazonaws.vpce.<region>.<service-id>`, the form an endpoint service that Amazon Web
/// Services itself does not publish in the Region is named by
/// (`ServiceDetail$ServiceName` — "the name of the service", whose `vpce` label carries the
/// owner's own service id).
fn endpoint_service_name(region: &str, service_id: &str) -> String {
    format!("com.amazonaws.vpce.{region}.{service_id}")
}

/// Ask Amazon EC2 to create the managed VPC endpoint of a workgroup placed in `subnet_ids`,
/// and answer it as the workgroup reports it — or `None` where EC2 created none (see the
/// module docs).
///
/// The endpoint is placed in the subnets EC2 answered for, in the VPC EC2 reports them in,
/// and its interfaces are read back off EC2's own records. `VpcEndpoint` reports no security
/// groups and this service resolves none, so the endpoint is created without naming any: EC2
/// then applies the VPC's own default group, which is what it applies to an interface endpoint
/// created without groups.
pub async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<VpcEndpoint> {
    if subnet_ids.is_empty() {
        // A workgroup carrying no subnet asks for an interface in none, so there is no managed
        // endpoint to create and nothing was refused.
        return None;
    }
    let client = ec2_client(ctx, region)?;
    let placement = placement(&client, subnet_ids).await?;
    let service_id = new_endpoint_service_id();
    let answer = client
        .create_vpc_endpoint()
        .vpc_endpoint_type(WORKGROUP_ENDPOINT_TYPE)
        .vpc_id(&placement.vpc_id)
        .set_subnet_ids(Some(placement.subnet_ids.clone()))
        .service_name(endpoint_service_name(region, &service_id))
        // One endpoint per ask: the token is this endpoint's own service id, so an ask is
        // never read as a replay of another workgroup's create.
        .client_token(&service_id)
        .send()
        .await;
    let created = match answer {
        Ok(output) => output.vpc_endpoint,
        Err(error) => {
            unasked(
                &::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
            );
            return None;
        }
    };
    let Some(vpc_endpoint_id) = created
        .as_ref()
        .and_then(::aws_sdk_ec2::types::VpcEndpoint::vpc_endpoint_id)
    else {
        unasked("Amazon EC2 reported no endpoint for the create");
        return None;
    };
    let interface_ids: Vec<String> = created
        .as_ref()
        .map(|endpoint| endpoint.network_interface_ids().to_vec())
        .unwrap_or_default();
    Some(VpcEndpoint {
        vpc_endpoint_id: Some(vpc_endpoint_id.to_string()),
        vpc_id: Some(placement.vpc_id.clone()),
        network_interfaces: Some(
            interfaces(&client, &interface_ids, &placement.subnet_ids).await,
        ),
    })
}

/// Ask Amazon EC2 to delete the managed endpoint `vpc_endpoint_id` names, as the workgroup
/// holding it goes away or is placed in other subnets.
///
/// The exact reverse of [`create`]: the interfaces carry that workgroup's traffic, so they do
/// not outlive it, and the addresses they held go back to the subnets they came out of. There
/// is nothing to report back — the workgroup's own record has moved on either way — so an
/// endpoint EC2 could not be asked about is traced at `warn` rather than failing the write
/// that owns it.
pub async fn remove(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_endpoint_id: &str,
) {
    let Some(client) = ec2_client(ctx, region) else {
        return;
    };
    let answer = client
        .delete_vpc_endpoints()
        .vpc_endpoint_ids(vpc_endpoint_id)
        .send()
        .await;
    match answer {
        Ok(output) if output.unsuccessful().is_empty() => {}
        Ok(_) => ::tracing::warn!(
            "Amazon EC2 did not delete the managed VPC endpoint of a workgroup"
        ),
        Err(error) => ::tracing::warn!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the managed VPC endpoint of a workgroup could not be deleted in Amazon EC2"
        ),
    }
}

/// Where a workgroup's endpoint is placed: the VPC its subnets belong to, and the subnets the
/// endpoint takes an interface in.
#[derive(Debug)]
struct Placement {
    /// The VPC EC2 reports those subnets in, which is the endpoint's own.
    vpc_id: String,
    /// The subnets the endpoint is placed in, in the order the workgroup names them.
    subnet_ids: Vec<String>,
}

/// Resolve the subnets a workgroup carries against EC2, and answer the placement they make up
/// — or `None` when EC2 holds a subnet for none of them.
///
/// One endpoint lives in ONE VPC, so the VPC is the one EC2 reports for the first id that
/// resolves, and an id EC2 reports in another VPC is not placed in. "You can specify only one
/// subnet per Availability Zone" for an interface endpoint, so of two subnets EC2 reports in
/// one zone the first the workgroup names is placed in and the second is not — an endpoint
/// takes one interface per zone either way, and asking for both is a refusal that would leave
/// the workgroup with no endpoint at all.
async fn placement(client: &::aws_sdk_ec2::Client, subnet_ids: &[String]) -> Option<Placement> {
    let asked = distinct(subnet_ids, MAX_ENDPOINT_SUBNETS);
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(SUBNET_ID_FILTER)
        .set_values(Some(asked.clone()))
        .build();
    let answer = client.describe_subnets().filters(by_id).send().await;
    let described = match answer {
        Ok(output) => output.subnets,
        Err(error) => {
            unasked(
                &::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
            );
            return None;
        }
    };
    let described = described.unwrap_or_default();
    let mut records: BTreeMap<&str, &::aws_sdk_ec2::types::Subnet> = BTreeMap::new();
    for subnet in &described {
        if let Some(subnet_id) = subnet.subnet_id() {
            records.insert(subnet_id, subnet);
        }
    }
    let Some(vpc_id) = asked
        .iter()
        .filter_map(|subnet_id| records.get(subnet_id.as_str()))
        .find_map(|subnet| subnet.vpc_id())
        .map(str::to_string)
    else {
        unasked("the account holds no subnet by the ids the workgroup carries");
        return None;
    };
    let mut zones: BTreeSet<&str> = BTreeSet::new();
    let mut placed: Vec<String> = Vec::new();
    for subnet_id in &asked {
        let Some(subnet) = records.get(subnet_id.as_str()) else {
            continue;
        };
        if subnet.vpc_id() != Some(vpc_id.as_str()) {
            continue;
        }
        if !zones.insert(subnet.availability_zone().unwrap_or_default()) {
            continue;
        }
        placed.push(subnet_id.clone());
    }
    (!placed.is_empty()).then_some(Placement {
        vpc_id,
        subnet_ids: placed,
    })
}

/// The interfaces of the endpoint EC2 created, as EC2's own record of each one reports it: its
/// id, the subnet it sits in, that subnet's Availability Zone and the private address it
/// holds, in the order of the subnets the endpoint was placed in.
///
/// An interface EC2 reports no subnet for is left out: the workgroup reports an interface by
/// the subnet it is in, so one that names none is nothing a client could place.
async fn interfaces(
    client: &::aws_sdk_ec2::Client,
    interface_ids: &[String],
    subnet_ids: &[String],
) -> Vec<NetworkInterface> {
    if interface_ids.is_empty() {
        return Vec::new();
    }
    let answer = client
        .describe_network_interfaces()
        .set_network_interface_ids(Some(interface_ids.to_vec()))
        .send()
        .await;
    let described = match answer {
        Ok(output) => output.network_interfaces.unwrap_or_default(),
        Err(error) => {
            unasked(
                &::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
            );
            return Vec::new();
        }
    };
    let mut by_subnet: BTreeMap<String, NetworkInterface> = BTreeMap::new();
    for interface in described {
        let Some(subnet_id) = interface.subnet_id() else {
            continue;
        };
        by_subnet.insert(
            subnet_id.to_string(),
            NetworkInterface {
                network_interface_id: interface.network_interface_id().map(str::to_string),
                subnet_id: Some(subnet_id.to_string()),
                private_ip_address: interface.private_ip_address().map(str::to_string),
                availability_zone: interface.availability_zone().map(str::to_string),
                // "The IPv6 address of the network interface within the subnet", for an
                // interface EC2 gave one: an endpoint in an IPv4-only subnet has none.
                ipv6_address: interface
                    .ipv6_addresses()
                    .first()
                    .and_then(::aws_sdk_ec2::types::NetworkInterfaceIpv6Address::ipv6_address)
                    .map(str::to_string),
            },
        );
    }
    subnet_ids
        .iter()
        .filter_map(|subnet_id| by_subnet.remove(subnet_id))
        .collect()
}

/// Whether an endpoint placed in `placed` has to MOVE to serve a workgroup carrying
/// `requested`.
///
/// The endpoint holds one interface per DISTINCT subnet, so what decides the placement is the
/// SET of subnets the workgroup carries: naming the same ones in another order, or naming one
/// of them twice, asks for the same interfaces in the same subnets and leaves the endpoint
/// where it is.
pub fn placement_moves(requested: &[String], placed: &[String]) -> bool {
    fn subnets(ids: &[String]) -> BTreeSet<&str> {
        ids.iter().map(String::as_str).collect()
    }
    subnets(requested) != subnets(placed)
}

/// The first `limit` DISTINCT entries of `ids`, in the order they first appear: a subnet is one
/// subnet however many times a list names it.
fn distinct(ids: &[String], limit: usize) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    ids.iter()
        .filter(|id| seen.insert(id.as_str()))
        .take(limit)
        .cloned()
        .collect()
}

/// Trace a managed endpoint Amazon EC2 did not create, which leaves the workgroup reporting no
/// managed VPC endpoint (see the module docs).
///
/// At `warn`: a deployment where this ask does not answer is one where no workgroup reports the
/// private connectivity it has, which is worth seeing, and `why` is what EC2 or the SDK
/// reported. No caller-supplied value is interpolated into the line.
fn unasked(why: &str) {
    ::tracing::warn!(
        why = %why,
        "the managed VPC endpoint of a workgroup was not created in Amazon EC2"
    );
}

/// An AWS SDK client for Amazon EC2 over the in-process callback into this same emulator, or
/// `None` when `region` is not a Region LABEL.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches IN PROCESS to a service registered in this very emulator, so no socket is opened
/// and nothing leaves the host. The endpoint URL is only the address the SDK needs to name the
/// service and the Region it is addressing — the callback's HTTP client never resolves or
/// connects to it — which is also why there is nothing to retry, time out, watch for a stalled
/// stream or cache an identity for.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in that
/// URL's authority, so it is read as a Region label (`service_common::arn::region_label`, the
/// one place this tree states that grammar) instead of being interpolated as given: a value
/// carrying a host or path delimiter would name an authority the caller chose rather than the
/// peer service this emulator asks.
fn ec2_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let Some(region) = ::service_common::arn::region_label(region) else {
        unasked("the request names no Region label, so Amazon EC2 cannot be asked");
        return None;
    };
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

#[cfg(test)]
mod tests {
    use super::{
        distinct, endpoint_service_name, new_endpoint_service_id, placement_moves,
        ENDPOINT_SERVICE_ID_DIGITS, ENDPOINT_SERVICE_ID_PREFIX, MAX_ENDPOINT_SUBNETS,
    };

    /// A subnet id of the published form, numbered so a case can ask for as many distinct ones
    /// as it likes.
    fn subnet(index: usize) -> String {
        format!("subnet-{index:017x}")
    }

    /// One endpoint is placed in one subnet per DISTINCT id the workgroup names, in the order
    /// it first names each, and in no more of them than the module's own ceiling.
    #[test]
    fn the_subnets_resolved_are_the_distinct_ones_named() {
        // (case, the ids the workgroup carries, the ids one hop asks about)
        let rows: [(&str, Vec<String>, Vec<String>); 5] = [
            ("no subnet at all", Vec::new(), Vec::new()),
            ("one subnet", vec![subnet(1)], vec![subnet(1)]),
            (
                "three subnets, in the order they were named",
                vec![subnet(3), subnet(1), subnet(2)],
                vec![subnet(3), subnet(1), subnet(2)],
            ),
            (
                "one subnet named twice: one subnet",
                vec![subnet(2), subnet(2)],
                vec![subnet(2)],
            ),
            (
                "one distinct subnet past the ceiling: the ceiling stands",
                (0..=MAX_ENDPOINT_SUBNETS).map(subnet).collect(),
                (0..MAX_ENDPOINT_SUBNETS).map(subnet).collect(),
            ),
        ];
        for (case, carried, expected) in rows {
            let asked = distinct(&carried, MAX_ENDPOINT_SUBNETS);
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// An endpoint moves only when the SET of subnets the workgroup carries changes: the rows
    /// differ in whether an id was added, dropped, reordered or repeated.
    #[test]
    fn only_a_different_set_of_subnets_moves_the_endpoint() {
        // (case, the subnets the request names, the subnets the endpoint is placed in, moves)
        let rows: [(&str, Vec<String>, Vec<String>, bool); 7] = [
            ("the same one subnet", vec![subnet(1)], vec![subnet(1)], false),
            (
                "the same two subnets in the same order",
                vec![subnet(1), subnet(2)],
                vec![subnet(1), subnet(2)],
                false,
            ),
            (
                "the same two subnets in the other order",
                vec![subnet(2), subnet(1)],
                vec![subnet(1), subnet(2)],
                false,
            ),
            (
                "one of them named twice",
                vec![subnet(1), subnet(2), subnet(1)],
                vec![subnet(1), subnet(2)],
                false,
            ),
            (
                "one subnet added",
                vec![subnet(1), subnet(2), subnet(3)],
                vec![subnet(1), subnet(2)],
                true,
            ),
            (
                "one subnet dropped",
                vec![subnet(1)],
                vec![subnet(1), subnet(2)],
                true,
            ),
            ("every subnet given up", Vec::new(), vec![subnet(1)], true),
        ];
        for (case, requested, placed, expected) in rows {
            let moves = placement_moves(&requested, &placed);
            assert_eq!(
                moves, expected,
                "case {case:?}: expected moves {expected}, got {moves}"
            );
        }
    }

    /// Every minted endpoint service id has the published shape, and two of them differ.
    #[test]
    fn a_minted_endpoint_service_id_has_the_published_shape() {
        let mut minted: Vec<String> = Vec::new();
        for case in 0..8 {
            let id = new_endpoint_service_id();
            let digits = id
                .strip_prefix(&format!("{ENDPOINT_SERVICE_ID_PREFIX}-"))
                .unwrap_or_default();
            assert_eq!(
                digits.len(),
                ENDPOINT_SERVICE_ID_DIGITS,
                "case {case}: expected {ENDPOINT_SERVICE_ID_DIGITS} digits, got {id:?}"
            );
            assert!(
                digits.chars().all(|digit| digit.is_ascii_hexdigit()),
                "case {case}: expected hexadecimal digits, got {id:?}"
            );
            assert!(
                !minted.contains(&id),
                "case {case}: expected a fresh id, got {id:?} again"
            );
            minted.push(id);
        }
    }

    /// An endpoint service of this account is named by the Region it is in and its own id.
    #[test]
    fn the_endpoint_service_name_carries_the_region_and_the_service_id() {
        for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
            let service_id = "vpce-svc-0123456789abcdef0";
            let expected = format!("com.amazonaws.vpce.{region}.{service_id}");
            let named = endpoint_service_name(region, service_id);
            assert_eq!(
                named, expected,
                "case {region:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}
