// SPDX-License-Identifier: CC0-1.0

//! The firewall endpoint a firewall subnet carries, instantiated in the service that
//! OWNS it.
//!
//! "In each configured subnet, Network Firewall instantiates a firewall endpoint to handle
//! network traffic" (`smithy_query(query="shape",shape="Attachment")`), and what that
//! endpoint IS is a Gateway Load Balancer VPC endpoint in the caller's own VPC: the id the
//! attachment reports is "the identifier of the firewall endpoint that Network Firewall has
//! instantiated in the subnet. You use this to identify the firewall endpoint in the VPC
//! route tables, when you redirect traffic to it" (`Attachment$EndpointId`). A VPC endpoint
//! is Amazon EC2's resource, and a route target has to be one EC2 holds — EC2 resolves
//! `CreateRoute`'s `VpcEndpointId` against its own endpoints — so this service does not mint
//! a firewall endpoint id of its own: it asks EC2 to create the endpoint and reports the id
//! EC2 answered, and it asks EC2 to delete that endpoint when the attachment holding it goes
//! away.
//!
//! The ask goes over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), the seam every service of this
//! emulator reaches another one through: this crate names no other service's crate, no
//! socket is opened, no host name is resolved and nothing leaves the process. The
//! credentials are the callback's own, which sign the hop and never appear in a response.
//!
//! Whether a placement EXISTS is EC2's answer too: a firewall endpoint can only be
//! instantiated in a VPC and subnets the calling account holds, so the ids a request carries
//! are resolved against EC2 ([`resolve`]) before anything is stored, and the operation that
//! asked refuses a request naming one this account cannot reach. Whether an endpoint is still
//! IN USE is EC2's answer for the same reason — the route tables that send traffic to it are
//! EC2's resource ([`still_routed`]) — so deleting a firewall whose endpoints a route table
//! still names is refused rather than leaving that route pointing at nothing.
//!
//! An ask EC2 does not answer at all is an ask this emulator could not make: there was no EC2
//! in this process (the assembled emulator mounts every service, so this is a process running
//! Network Firewall alone), or the call failed for a reason that is not "this account has no
//! such resource". Nothing is resolved then, and nothing is refused — a refusal of this
//! service's is grounded in what EC2 ANSWERED, and an ask that went unanswered grounds none.
//! An endpoint EC2 creates none for leaves its attachment carrying an id this service mints,
//! which no EC2 holds and no route can resolve, and the attempt is traced at `warn`.
//!
//! Nothing here holds this service's store: every function takes the values it needs and is
//! awaited with no guard held, so an endpoint can never be created or deleted behind a lock.

/// The `VpcEndpointType` a firewall endpoint is created as: a Gateway Load Balancer
/// endpoint, the one type a route can name as its `VpcEndpointId` target ("The ID of a VPC
/// endpoint. Supported for Gateway Load Balancer endpoints only",
/// `com.amazonaws.ec2#CreateRouteRequest$VpcEndpointId`).
const FIREWALL_ENDPOINT_TYPE: ::aws_sdk_ec2::types::VpcEndpointType =
    ::aws_sdk_ec2::types::VpcEndpointType::GatewayLoadBalancer;

/// The id prefix of an endpoint service of an account (`vpce-svc-…`), which is what the
/// endpoint service behind a firewall's endpoints is named by.
const ENDPOINT_SERVICE_ID_PREFIX: &str = "vpce-svc";

/// The endpoint service id behind ONE firewall's endpoints (`vpce-svc-` plus 17 hex digits,
/// the shape an endpoint service id takes).
///
/// Every firewall endpoint of a firewall is an endpoint TO the one Gateway Load Balancer
/// endpoint service that firewall is reached through, so the id is minted once per firewall
/// and kept on its record; `vpce-svc-` ids are minted the way every other server-generated
/// id of this service is ([`crate::state::new_endpoint_id`]).
pub fn new_endpoint_service_id() -> String {
    let digits = ::hex::encode(super::fresh_bytes());
    format!("{ENDPOINT_SERVICE_ID_PREFIX}-{}", &digits[..17])
}

/// The endpoint service NAME an endpoint to the service `service_id` names it by:
/// `com.amazonaws.vpce.<region>.<service-id>`, the form an endpoint service that Amazon Web
/// Services itself does not publish is named by (`com.amazonaws.ec2#ServiceDetail$ServiceName`
/// — "the name of the service", whose `vpce` label carries the owner's own service id).
pub fn endpoint_service_name(region: &str, service_id: &str) -> String {
    format!("com.amazonaws.vpce.{region}.{service_id}")
}

/// The error code Amazon EC2 answers a read naming a VPC id no VPC of the account has with
/// (`InvalidVpcID.NotFound`), which is what says the VPC is not one this account can reach
/// rather than that the ask failed.
const VPC_NOT_FOUND_CODE: &str = "InvalidVpcID.NotFound";

/// The same for a subnet id no subnet of the account has (`InvalidSubnetID.NotFound`).
const SUBNET_NOT_FOUND_CODE: &str = "InvalidSubnetID.NotFound";

/// What Amazon EC2 answered about the placement a firewall's endpoints would be instantiated
/// in — the VPC the firewall filters in and the subnets each endpoint sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// EC2 holds every resource the placement names for this account in this Region.
    Reachable,
    /// EC2 holds no VPC of this account with the id the request named.
    VpcUnreachable,
    /// EC2 holds no subnet of this account for at least one of the ids the request named.
    SubnetsUnreachable,
    /// EC2 answered nothing about the placement, so it is resolved neither way (see the
    /// module docs).
    Unanswered,
}

/// Resolve a placement against Amazon EC2: whether this account can reach `vpc_id` (when the
/// request named one) and every subnet of `subnet_ids`.
///
/// The VPC is resolved FIRST and answered on its own, because a subnet is a resource OF a VPC:
/// a request whose VPC this account cannot reach has named no subnet of its own either, so
/// what is wrong with it is the VPC. Only then are the subnets resolved, together in one read.
///
/// A read naming an id the account has no such resource for is REFUSED by EC2 rather than
/// given an absent row ([`VPC_NOT_FOUND_CODE`], [`SUBNET_NOT_FOUND_CODE`]), so that code is
/// what says "unreachable"; any other failure, and no EC2 to ask at all, is
/// [`Placement::Unanswered`] and resolves nothing. `subnet_ids` is bounded by the callers,
/// which hold a request to one subnet per Availability Zone before they reach here.
pub async fn resolve(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: Option<&str>,
    subnet_ids: &[String],
) -> Placement {
    let Some(client) = ec2_client(ctx, region) else {
        unresolved("there is no Amazon EC2 in this process to ask");
        return Placement::Unanswered;
    };
    if let Some(vpc_id) = vpc_id {
        match client.describe_vpcs().vpc_ids(vpc_id).send().await {
            // EC2 refuses a read naming a VPC it holds none of, so an answer with no row for
            // the id asked about is the same answer as that refusal.
            Ok(output) => {
                if !output.vpcs().iter().any(|vpc| vpc.vpc_id() == Some(vpc_id)) {
                    return Placement::VpcUnreachable;
                }
            }
            Err(error) => {
                return answered_not_found(&error, VPC_NOT_FOUND_CODE, Placement::VpcUnreachable)
            }
        }
    }
    if subnet_ids.is_empty() {
        return Placement::Reachable;
    }
    match client
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await
    {
        Ok(output) => {
            let held: ::std::collections::HashSet<&str> = output
                .subnets()
                .iter()
                .filter_map(::aws_sdk_ec2::types::Subnet::subnet_id)
                .collect();
            if subnet_ids.iter().any(|id| !held.contains(id.as_str())) {
                Placement::SubnetsUnreachable
            } else {
                Placement::Reachable
            }
        }
        Err(error) => {
            answered_not_found(&error, SUBNET_NOT_FOUND_CODE, Placement::SubnetsUnreachable)
        }
    }
}

/// Read one failed placement read: `unreachable` where EC2 answered `not_found_code` — the
/// code it refuses a read naming a resource this account does not have with — and
/// [`Placement::Unanswered`] for anything else, which resolves nothing and is traced.
fn answered_not_found<E: ::aws_sdk_ec2::error::ProvideErrorMetadata>(
    error: &E,
    not_found_code: &str,
    unreachable: Placement,
) -> Placement {
    if error.code() == Some(not_found_code) {
        return unreachable;
    }
    unresolved(&format!("Amazon EC2 answered {:?}", error.code()));
    Placement::Unanswered
}

/// Trace a placement Amazon EC2 answered nothing about, which is a placement this service
/// resolves neither way (see the module docs).
///
/// At `warn`: a deployment where this ask does not answer is one where no placement can be
/// resolved, which is worth seeing. `why` is this service's own text, never a caller value.
fn unresolved(why: &str) {
    ::tracing::warn!(
        why = %why,
        "the VPC and subnets a firewall's endpoints would be instantiated in were not resolved \
         in Amazon EC2"
    );
}

/// Ask Amazon EC2 to instantiate the firewall endpoint for `subnet_id`, and answer the id it
/// minted — or `None` where it created none (see the module docs).
///
/// `vpc_id` is the VPC the firewall filters in and `service_name` the endpoint service all of
/// that firewall's endpoints are endpoints to ([`endpoint_service_name`]). The endpoint is
/// created with the firewall's own service id and subnet as its client token, so an ask
/// repeated for one firewall subnet replays EC2's own create rather than standing up a second
/// endpoint.
pub async fn instantiate(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
    subnet_id: &str,
    service_name: &str,
) -> Option<String> {
    let Some(client) = ec2_client(ctx, region) else {
        unasked(subnet_id, "there is no Amazon EC2 in this process to ask");
        return None;
    };
    let answer = client
        .create_vpc_endpoint()
        .vpc_endpoint_type(FIREWALL_ENDPOINT_TYPE)
        .vpc_id(vpc_id)
        .subnet_ids(subnet_id)
        .service_name(service_name)
        .client_token(format!("{service_name}-{subnet_id}"))
        .send()
        .await;
    match answer {
        Ok(output) => {
            let created = output
                .vpc_endpoint()
                .and_then(::aws_sdk_ec2::types::VpcEndpoint::vpc_endpoint_id)
                .map(str::to_string);
            if created.is_none() {
                // EC2 answered the create but reported no endpoint, so there is no endpoint
                // id to report.
                unasked(subnet_id, "Amazon EC2 reported no endpoint for the create");
            }
            created
        }
        Err(error) => {
            unasked(
                subnet_id,
                &::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
            );
            None
        }
    }
}

/// Trace a firewall endpoint Amazon EC2 did not create, which leaves the attachment carrying
/// an id no EC2 holds (see the module docs).
///
/// At `warn`: a deployment where this ask does not answer is one where no firewall endpoint
/// can be routed to, which is worth seeing, and `why` is what the SDK reported. The subnet id
/// is the caller's own value, rendered in its escaped `Debug` form so a value carrying a
/// newline cannot forge a log line.
fn unasked(subnet_id: &str, why: &str) {
    ::tracing::warn!(
        subnet_id = ?subnet_id,
        why = %why,
        "the firewall endpoint for a firewall subnet was not instantiated in Amazon EC2"
    );
}

/// Instantiate the firewall endpoint of every subnet of `subnet_ids`, and answer the
/// endpoints EC2 created, keyed by the subnet each one serves. A subnet EC2 created none for
/// is absent from the answer, and its attachment falls back to a minted id.
///
/// `subnet_ids` is bounded HERE rather than by each caller: one endpoint is asked for per
/// entry, so a list longer than the number of Availability Zones a firewall spans ("you can
/// specify one subnet for each of the Availability Zones that the VPC spans",
/// `AssociateSubnets`) has no endpoint asked for at all — both callers hold the request to
/// that ceiling and refuse past it before reaching here, so this is the bound stated at the
/// growth site rather than a second refusal.
pub async fn instantiate_all(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
    service_name: &str,
    subnet_ids: &[String],
) -> ::std::collections::HashMap<String, String> {
    let mut created = ::std::collections::HashMap::new();
    if subnet_ids.len() > crate::state::MAX_AVAILABILITY_ZONES {
        return created;
    }
    for subnet_id in subnet_ids {
        if let Some(endpoint_id) = instantiate(ctx, region, vpc_id, subnet_id, service_name).await
        {
            created.insert(subnet_id.clone(), endpoint_id);
        }
    }
    created
}

/// The EC2 route-table filter that selects the tables holding a route whose target is one of
/// the named gateways (`route.gateway-id`).
///
/// A firewall endpoint is reported as a route's `GatewayId` — the modelled `Route` carries no
/// member of its own for a VPC endpoint — so this is the filter that answers "which route
/// tables still send traffic to these endpoints".
const ROUTE_GATEWAY_FILTER: &str = "route.gateway-id";

/// The firewall endpoints of `endpoint_ids` a VPC route table still routes traffic to, in the
/// order they were asked about.
///
/// "You can check whether a firewall is in use by reviewing the route tables for the
/// Availability Zones where you have firewall subnet mappings … When the route tables no
/// longer use the firewall endpoints, you can remove the firewall safely" (`DeleteFirewall`):
/// an endpoint a route table still points at is in use, and removing it would leave that route
/// pointing at nothing. The route tables are Amazon EC2's resource, so the question is asked
/// THERE, through the same in-process callback the rest of this module uses.
///
/// Empty where EC2 answered nothing (see the module docs) and where nothing was asked about:
/// a refusal of this service's is grounded in what EC2 answered, so an unanswered ask holds
/// nothing back. `endpoint_ids` is bounded HERE the way [`instantiate_all`] bounds its own
/// list — one endpoint per Availability Zone a firewall spans — so a longer list is asked
/// about not at all rather than turned into an unbounded filter.
pub async fn still_routed(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    endpoint_ids: &[String],
) -> Vec<String> {
    if endpoint_ids.is_empty() || endpoint_ids.len() > crate::state::MAX_AVAILABILITY_ZONES {
        return Vec::new();
    }
    let Some(client) = ec2_client(ctx, region) else {
        unrouted("there is no Amazon EC2 in this process to ask");
        return Vec::new();
    };
    let filter = ::aws_sdk_ec2::types::Filter::builder()
        .name(ROUTE_GATEWAY_FILTER)
        .set_values(Some(endpoint_ids.to_vec()))
        .build();
    let answer = client.describe_route_tables().filters(filter).send().await;
    let tables = match answer {
        Ok(output) => output.route_tables,
        Err(error) => {
            unrouted(&::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string());
            return Vec::new();
        }
    };
    let targeted: ::std::collections::HashSet<&str> = tables
        .iter()
        .flatten()
        .flat_map(|table| table.routes())
        .filter_map(::aws_sdk_ec2::types::Route::gateway_id)
        .collect();
    endpoint_ids
        .iter()
        .filter(|endpoint_id| targeted.contains(endpoint_id.as_str()))
        .cloned()
        .collect()
}

/// Trace route tables Amazon EC2 answered nothing about, which holds no firewall endpoint back
/// (see the module docs).
///
/// At `warn`: a deployment where this ask does not answer is one where no endpoint can be found
/// to be in use, which is worth seeing. `why` is this service's own text or what the SDK
/// reported, never a caller value.
fn unrouted(why: &str) {
    ::tracing::warn!(
        why = %why,
        "the route tables that still send traffic to a firewall's endpoints were not read in \
         Amazon EC2"
    );
}

/// The refusal message for a delete that would take away firewall endpoints a VPC route table
/// still routes traffic to, naming those endpoints.
///
/// The ids are the endpoints the request's own firewall carries and the route tables still
/// point at ([`still_routed`]), in the order they were asked about, so the caller is told which
/// routes to remove first.
pub fn still_routed_message(endpoint_ids: &[String]) -> String {
    format!(
        "Unable to fulfill request because the following related VPC endpoint(s) still exist \
         in route table(s): [{endpoints}]",
        endpoints = endpoint_ids.join(", "),
    )
}

/// Ask Amazon EC2 to delete the firewall endpoints `endpoint_ids` names, as the attachment
/// each one belongs to goes away: "this removes the firewall endpoints from the subnets"
/// (`DisassociateSubnets`), and a deleted firewall takes its own endpoints with it.
///
/// There is nothing to report back: the attachment is gone from this service's record either
/// way, and an endpoint EC2 could not be asked about is traced at `warn` rather than failing
/// the delete that owns it.
pub async fn remove(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    endpoint_ids: Vec<String>,
) {
    if endpoint_ids.is_empty() {
        return;
    }
    let Some(client) = ec2_client(ctx, region) else {
        ::tracing::warn!(
            "there is no Amazon EC2 in this process to delete a firewall endpoint in"
        );
        return;
    };
    let answer = client
        .delete_vpc_endpoints()
        .set_vpc_endpoint_ids(Some(endpoint_ids))
        .send()
        .await;
    match answer {
        Ok(output) if output.unsuccessful().is_empty() => {}
        Ok(output) => ::tracing::warn!(
            unsuccessful = output.unsuccessful().len(),
            "Amazon EC2 did not delete every firewall endpoint it was asked to"
        ),
        Err(error) => ::tracing::warn!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the firewall endpoints of a removed attachment could not be deleted in Amazon EC2"
        ),
    }
}

/// An AWS SDK client for Amazon EC2 over the in-process callback into this same emulator, or
/// `None` when `region` is not a Region LABEL.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches IN PROCESS to a service registered in this very emulator, so no socket is
/// opened and nothing leaves the host. The endpoint URL is only the address the SDK needs to
/// name the service and the Region it is addressing — the callback's HTTP client never
/// resolves or connects to it — which is also why there is nothing to retry, time out, watch
/// for a stalled stream or cache an identity for.
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
