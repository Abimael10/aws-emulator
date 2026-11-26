// SPDX-License-Identifier: CC0-1.0

//! The refusal each GATEWAY operation publishes for a member it requires and the request
//! leaves out.
//!
//! The gateway surface does not word these from the member at fault. Each operation publishes
//! ONE refusal per required member — its own modelled error shape, the status that shape is
//! answered with, and a sentence that is entirely fixed text, naming no member path, no value
//! and no count. So they are decided here, where the routing key and the member are both in
//! hand, rather than by the framework's walk, which renders one clause from the member alone.
//!
//! The member is found the way the rest of [`super`] finds one: the request is read ONCE
//! through the same protocol and generated input schema the dispatch sequence will hand the
//! handler, and a member the shape requires and the body leaves out travels as that read's own
//! error naming it ([`absent_member`]). Where one request leaves out more than one, the read
//! names the first, which is the one the caller has to fill in first. A member the body DOES
//! carry is judged by the operation exactly as before: a value that spells nothing is a value,
//! and what it earns is the reading its own member's rule takes of it
//! ([`crate::state::requested_gateway_name_violation`], and the resolution a gateway reference
//! goes through).
//!
//! The refusals, and what each one is about:
//!
//! * **A request that carries no `gateway`.** Every operation that acts on ONE gateway
//!   identifies it by that member ("The name or Amazon Resource Name (ARN) of the gateway"),
//!   and a request that carries none hands it no identifier to act on: Amazon ECS states that
//!   as the identifier being invalid ([`GATEWAY_IDENTIFIER_INVALID_MESSAGE`]), the same
//!   sentence whichever of those operations required the member — which is why it is keyed on
//!   the member rather than on each of them.
//! * **A route read that carries no `gatewayRouteArn`**, which it addresses the route by,
//!   stated the same way about that ARN ([`GATEWAY_ROUTE_ARN_INVALID_MESSAGE`]).
//! * **An operation that associates a gateway WITH a cluster and carries no `cluster`.**
//!   Those operations act on the association of a gateway with a cluster, so a request naming
//!   only one of the pair names no association; Amazon ECS answers it with the one refusal it
//!   publishes for an association it will not act on, which deliberately conflates "you may
//!   not" with "there is none" ([`GATEWAY_ASSOCIATION_DENIED_MESSAGE`]) and so is an
//!   AUTHORIZATION refusal rather than a request-parameter one.
//! * **A `CreateGateway` that carries no `name`**, refused with the sentence that create
//!   publishes for the gateway's name ([`crate::state::GATEWAY_NAME_REQUIRED_MESSAGE`]) — the
//!   same one the handler answers a name that spells nothing with, so the two cannot drift
//!   apart.
//! * **A `CreateGateway` that carries no `infrastructureRoleArn`.** The role is what Amazon ECS
//!   manages the gateway's whole infrastructure as, and a create that hands over none is
//!   answered as a SERVER-side failure — `ServerException`, the retryable class
//!   ([`SERVICE_UNAVAILABLE_MESSAGE`]) — rather than as a bad parameter. RECORDED behaviour,
//!   and the half of it that matters to a caller is the class: an SDK retries it, where it
//!   would not retry a request-validation refusal. A role the request DOES carry and spells
//!   nothing in is the operation's own refusal, which it words itself
//!   ([`crate::state::INFRASTRUCTURE_ROLE_BLANK_MESSAGE`]).
//! * **A `CreateGateway` that carries no `configuration`.** The configuration determines the
//!   gateway type and so the infrastructure the role would be passed for; a create that names
//!   none is refused the pass of that role ([`CROSS_ACCOUNT_PASS_ROLE_MESSAGE`]), as an
//!   authorization refusal. RECORDED behaviour.
//!
//! A member absent from the table below keeps exactly the refusal it had: there is no AWS
//! sentence to put in its place.

use ::aws_smithy_runtime_api::http::Request as RtRequest;
use ::smithy_server_runtime::ServerHttpProtocol;

use crate::ops::types::*;
use crate::service::absent_member;

use super::WordedRefusal;

/// The member every operation that acts on ONE gateway identifies it by.
const GATEWAY_MEMBER: &str = "gateway";

/// The member a gateway ROUTE read addresses the route by.
const GATEWAY_ROUTE_ARN_MEMBER: &str = "gatewayRouteArn";

/// The member a create hands the role Amazon ECS manages the gateway's infrastructure as over
/// through.
const INFRASTRUCTURE_ROLE_ARN_MEMBER: &str = "infrastructureRoleArn";

/// The member a create names the type-specific configuration — and so the gateway type — in.
const CONFIGURATION_MEMBER: &str = "configuration";

/// `InvalidParameterException$message` for a request that carries no `gateway`: fixed
/// text about the identifier, carrying nothing of the request. RECORDED behaviour, with no
/// trailing period — this surface punctuates these two sentences without one.
const GATEWAY_IDENTIFIER_INVALID_MESSAGE: &str = "The gateway identifier is invalid";

/// `InvalidParameterException$message` for a route read that carries no `gatewayRouteArn`,
/// worded about that ARN as the sentence above is worded about the gateway's identifier.
/// RECORDED behaviour.
const GATEWAY_ROUTE_ARN_INVALID_MESSAGE: &str = "The gateway route ARN is invalid";

/// `AccessDeniedException$message` for an association of a gateway with a cluster that Amazon
/// ECS will not act on: ONE sentence covering both a caller that may not and a gateway that is
/// not there, so a refused call tells an unauthorized caller nothing about what the account
/// holds. RECORDED behaviour.
const GATEWAY_ASSOCIATION_DENIED_MESSAGE: &str = "You are not authorized to perform this \
                                                  operation on the specified gateway, or it \
                                                  does not exist.";

/// `ServerException$message` for a create that hands Amazon ECS no infrastructure role: the
/// fixed sentence of the retryable class. RECORDED behaviour.
const SERVICE_UNAVAILABLE_MESSAGE: &str = "Service Unavailable. Please try again later.";

/// `AccessDeniedException$message` for a create whose infrastructure role Amazon ECS is not
/// given a configuration to manage with: the pass of that role is refused. RECORDED behaviour,
/// with no trailing period.
const CROSS_ACCOUNT_PASS_ROLE_MESSAGE: &str = "Cross-account pass role is not allowed";

/// The status the gateway surface answers an `AccessDeniedException` of its own with.
///
/// RECORDED behaviour: these refusals are answered in the 400 class, and not at the 403 the
/// model pins on the shape (`smithy_query(query="operation",operation="CreateGateway")`),
/// which is the status this service's IAM gate answers an action no policy allows with.
const GATEWAY_ACCESS_DENIED_STATUS: u16 = 400;

/// The refusal the gateway operation `operation` publishes for a member it requires and `req`
/// leaves out, or `None` when the request carries every one of them (and for every operation
/// outside this surface).
pub(super) fn gateway_member_refusal(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<WordedRefusal> {
    let absent = absent_required_member(operation, protocol, req)?;
    published_refusal(operation, &absent)
}

/// The required member `req` leaves out, and `None` for an operation outside this surface.
///
/// Each read goes through the same protocol and generated input schema the dispatch sequence
/// will hand the handler, so the member named here is exactly the one the operation would have
/// been refused for; a body that cannot be read for any other reason is no refusal of this
/// layer's.
fn absent_required_member(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    /// The required member this operation's input shape finds absent.
    macro_rules! absent_of {
        ($input:ty, $schema:expr) => {
            absent_member::<$input>(protocol, $schema, req)
        };
    }
    match operation {
        // The create carries no identifier of an existing gateway: the members it requires are
        // the gateway's name, the role Amazon ECS manages it as, and the configuration that
        // settles its type.
        "CreateGateway" => absent_of!(CreateGatewayInput, CREATE_GATEWAY_INPUT_SCHEMA),
        "UpdateGateway" => absent_of!(UpdateGatewayInput, UPDATE_GATEWAY_INPUT_SCHEMA),
        "DescribeGateway" => absent_of!(DescribeGatewayInput, DESCRIBE_GATEWAY_INPUT_SCHEMA),
        "DescribeGatewayResources" => absent_of!(
            DescribeGatewayResourcesInput,
            DESCRIBE_GATEWAY_RESOURCES_INPUT_SCHEMA
        ),
        "DeleteGateway" => absent_of!(DeleteGatewayInput, DELETE_GATEWAY_INPUT_SCHEMA),
        "AssociateGatewayWithCluster" => absent_of!(
            AssociateGatewayWithClusterInput,
            ASSOCIATE_GATEWAY_WITH_CLUSTER_INPUT_SCHEMA
        ),
        "DisassociateGatewayFromCluster" => absent_of!(
            DisassociateGatewayFromClusterInput,
            DISASSOCIATE_GATEWAY_FROM_CLUSTER_INPUT_SCHEMA
        ),
        "DescribeGatewayRoute" => {
            absent_of!(DescribeGatewayRouteInput, DESCRIBE_GATEWAY_ROUTE_INPUT_SCHEMA)
        }
        // Every other operation of this service requires no gateway of its own: the listings
        // take the member as a FILTER, which a request may leave out.
        _ => None,
    }
}

/// The refusal `operation` publishes for the member it left out, or `None` when it publishes
/// none — which leaves the request to the framework's walk and to the operation exactly as
/// before.
///
/// Reached only for the operations [`absent_required_member`] reads, so the rules keyed on the
/// MEMBER alone — the gateway's identifier and the cluster of an association — cannot reach an
/// operation off this surface.
fn published_refusal(operation: &str, member: &str) -> Option<WordedRefusal> {
    match (operation, member) {
        ("CreateGateway", crate::state::GATEWAY_NAME_MEMBER) => Some(
            WordedRefusal::invalid_parameter(crate::state::GATEWAY_NAME_REQUIRED_MESSAGE),
        ),
        ("CreateGateway", INFRASTRUCTURE_ROLE_ARN_MEMBER) => {
            Some(WordedRefusal::server(SERVICE_UNAVAILABLE_MESSAGE))
        }
        ("CreateGateway", CONFIGURATION_MEMBER) => Some(WordedRefusal::access_denied(
            GATEWAY_ACCESS_DENIED_STATUS,
            CROSS_ACCOUNT_PASS_ROLE_MESSAGE,
        )),
        (_, GATEWAY_MEMBER) => Some(WordedRefusal::invalid_parameter(
            GATEWAY_IDENTIFIER_INVALID_MESSAGE,
        )),
        (_, GATEWAY_ROUTE_ARN_MEMBER) => Some(WordedRefusal::invalid_parameter(
            GATEWAY_ROUTE_ARN_INVALID_MESSAGE,
        )),
        (_, crate::state::CLUSTER_MEMBER) => Some(WordedRefusal::access_denied(
            GATEWAY_ACCESS_DENIED_STATUS,
            GATEWAY_ASSOCIATION_DENIED_MESSAGE,
        )),
        _ => None,
    }
}
