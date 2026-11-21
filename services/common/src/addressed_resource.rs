// SPDX-License-Identifier: CC0-1.0

//! The ONE pre-dispatch read of the string a request names the resource it acts on by, and
//! the gate that decides which request names none, for every service whose authorization of
//! a caller-supplied resource comes BEFORE the modelled walk.
//!
//! ## Why the read is here rather than in each service
//!
//! A service that authorizes a call against a resource the CALLER names has to make that
//! decision before the framework validates the member the name arrived on: the framework's
//! pre-handler walk has already chosen HTTP 400 by the time a service can reword its refusal
//! ([`crate::restated_refusal`] and the `override_refusal` seam choose the code and the
//! message; the protocol owns the status), and a permissions failure answered as a malformed
//! member is the wrong class to a client. `Service::override_operation` is the seam that sees
//! a whole request first, so the decision is taken there — which means the resource has to be
//! read out of the request body WITHOUT the modelled input shape, through a schema declaring
//! that one member. That read is the same read in every such service, so it lives here once:
//! two services that take the same decision must not disagree about what a request named.
//!
//! ## What a service supplies
//!
//! Its own two halves, which is all that differs between them: the TABLE of operations whose
//! input names a resource — keyed by operation, so an operation that takes none never has its
//! body read and a member of another operation that happens to share a name is not judged —
//! and the PREDICATE that says whether a supplied string names a resource of that service at
//! all. The refusal's own envelope stays with the service too, because it names that service's
//! modelled error shape and its own sentence.
//!
//! Nothing here reads a store or the clock: the verdict is a function of the request's own
//! bytes, so a refused request changes nothing a later caller sees.

use ::aws_smithy_runtime_api::http::Request as RtRequest;
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::smithy_server_runtime::ServerHttpProtocol;

/// The namespace the read-only schemas of this gate name themselves in.
///
/// They stand for the READ, not for a shape of any model — a model's own input shape carries
/// every other member of its operation, which this read deliberately does not look at — so
/// they are named after the gate. Public so a service declaring a member of its own (one
/// spelled otherwise than [`RESOURCE_ARN_MEMBER`]) names the same namespace rather than a
/// second one.
pub const AUTHORIZATION_READ_NAMESPACE: &str = "emulator.authorization";

/// The member name the tagging operations of every AWS service name their resource by.
pub const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The `resourceArn` member of a request, typed as the string it is on the wire.
static RESOURCE_ARN: Schema = Schema::new_member(
    ShapeId::from_static(
        "emulator.authorization#ResourceArnRequest$resourceArn",
        AUTHORIZATION_READ_NAMESPACE,
        RESOURCE_ARN_MEMBER,
    ),
    ShapeType::String,
    RESOURCE_ARN_MEMBER,
    0,
);

/// The body schema a `resourceArn` request is read through: a structure declaring THAT ONE
/// member.
///
/// A codec skips every member a schema does not declare, so this reads the resource string
/// and nothing else: a body that also misses another required member, or carries one outside
/// its constraints, still names the resource the decision is about.
pub static RESOURCE_ARN_REQUEST: Schema = Schema::new_struct(
    ShapeId::from_static(
        "emulator.authorization#ResourceArnRequest",
        AUTHORIZATION_READ_NAMESPACE,
        "ResourceArnRequest",
    ),
    ShapeType::Structure,
    &[&RESOURCE_ARN],
);

/// The operations of a service whose input names a resource, each with the one-member body
/// schema that operation's resource string is read through ([`RESOURCE_ARN_REQUEST`], or a
/// schema the service declares for a member it spells otherwise).
pub type ResourceMembers<'a> = &'a [(&'static str, &'static Schema)];

/// The operation `req` names and the resource string it carries, when that string names NO
/// resource of the service — the one read both a dispatch gate and the refusal it answers
/// with are taken from, so the two cannot disagree about which request is refused.
///
/// The operation is resolved first, so only the operations in `members` are judged at all;
/// then that operation's resource member is read out of the request
/// ([`supplied_resource`]) and `addresses_a_resource` is asked whether the string names a
/// resource of this service.
///
/// `None` — leave the request to the modelled walk, which refuses it on its own terms — for
/// an operation outside the table, for a string that DOES name a resource, for a request that
/// carries no such member at all (nothing was named, so the absent `@required` member is the
/// answer the request gets), and for a body this cannot read.
///
/// `protocol` is the codec the request is SERVED in, so the read is the one the rest of the
/// request uses: a service reached over several protocols takes the same decision on each of
/// them, because an authorization decision is taken from what a request states, never from
/// the encoding it stated it in.
pub fn unaddressed_resource(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    members: ResourceMembers<'_>,
    addresses_a_resource: impl FnOnce(&str) -> bool,
) -> Option<(&'static str, String)> {
    let routing_key = protocol.routing_key(req).ok()?;
    let (operation, request_schema) = members
        .iter()
        .find(|(candidate, _)| *candidate == routing_key)?;
    let supplied = supplied_resource(protocol, req, request_schema)?;
    (!addresses_a_resource(&supplied)).then_some((*operation, supplied))
}

/// The resource string `req` supplies, read through the codec the request is SERVED in and
/// against `request_schema`'s one member.
///
/// `None` when the body is not a structure in that codec, carries no such member, or carries
/// something other than a string there: then the request names no resource a gate can judge,
/// and the modelled walk answers it.
pub fn supplied_resource(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    request_schema: &Schema,
) -> Option<String> {
    let mut reader = protocol.deserialize_input(req, request_schema).ok()?;
    let mut supplied = None;
    reader
        .read_struct(request_schema, &mut |member, value| {
            supplied = Some(value.read_string(member)?);
            Ok(())
        })
        .ok()?;
    supplied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one-member schema declares exactly the member the tagging operations name their
    /// resource by, as a string, under this gate's own namespace — so a codec reading a body
    /// through it reads that member and nothing else.
    #[test]
    fn the_read_declares_one_string_member_named_for_the_wire() {
        let members = RESOURCE_ARN_REQUEST.members();
        let named: Vec<Option<&str>> = members
            .iter()
            .map(|member| member.member_name())
            .collect();
        assert_eq!(
            named,
            vec![Some(RESOURCE_ARN_MEMBER)],
            "case the request schema: expected the one resource member, got {named:?}"
        );
        assert_eq!(
            RESOURCE_ARN.shape_type(),
            ShapeType::String,
            "case the member: expected the string it is on the wire, got {:?}",
            RESOURCE_ARN.shape_type()
        );
        assert_eq!(
            RESOURCE_ARN_REQUEST.shape_id().namespace(),
            AUTHORIZATION_READ_NAMESPACE,
            "case the namespace: expected this gate's own, got {:?}",
            RESOURCE_ARN_REQUEST.shape_id().namespace()
        );
    }
}
