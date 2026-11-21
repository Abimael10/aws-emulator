// SPDX-License-Identifier: CC0-1.0

//! The authorization this service takes on a caller-supplied resource ARN, ahead of the
//! modelled constraints on the member that carried it.
//!
//! ## The rule this module implements
//!
//! An operation whose only addressed resource is an ARN the CALLER supplies is authorized
//! against that ARN as supplied, before the member's modelled shape is checked: a policy
//! statement grants an action on resources it names by ARN, so a value that is not an ARN
//! at all names no resource any statement could have granted, and the request is refused as
//! an authorization failure — the modelled `AccessDeniedException`, HTTP 403, naming the
//! calling principal, the IAM action the call asks for and the value exactly as the request
//! carried it — rather than as the `ValidationException` (HTTP 400) a member outside its
//! modelled `@pattern` produces.
//!
//! The class is every operation of this service whose input declares a `@required`
//! `resourceArn` and nothing else that names a resource ([`RESOURCE_MEMBERS`]); the decision
//! is the same for each, and the action named is that operation's own. A value that IS an ARN
//! passes this gate whatever it names — a resource of another vendor, or one of this
//! service's own that the store does not hold — because deciding between those two is a
//! question about the request's content, which the handler answers once the request has been
//! authorized.
//!
//! ## What is this service's own, and what is shared
//!
//! The pre-dispatch READ of the member, and the gate that decides which request names no
//! resource, are the same in every service that authorizes a caller-supplied resource ahead
//! of the modelled walk, so both come from `service_common::addressed_resource`; this module
//! supplies the two halves that are this service's — the operation table and what "names a
//! resource" means here — and the refusal's own envelope, which names this service's modelled
//! error shape.
//!
//! ## Why it is an operation rather than a refusal wording
//!
//! The refusal has to come BEFORE the framework's modelled-constraint walk, which has
//! already chosen 400 by the time a service can reword it ([`crate::service`]'s
//! `override_refusal` chooses the code and the message; the protocol owns the status).
//! `Service::override_operation` is the seam that sees a whole request first, so the denial
//! is an [`ErasedOperation`] that answers that one request. The router still authenticates
//! the request against the modelled operation's auth schemes before running it.
//!
//! Nothing here reads the store or the clock: the verdict is a function of the request's own
//! value and of the identity the account holds, so it is the same on every call and a refused
//! request changes nothing a later caller sees.

use service_common::addressed_resource::{ResourceMembers, RESOURCE_ARN_REQUEST};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, OperationContext, ServerHttpProtocol, Service,
};

/// The error code and HTTP status a call naming a resource it cannot be authorized against
/// is answered with.
///
/// `AccessDeniedException` is this service's own modelled error for it, `@error("client")`
/// at `@httpError(403)`
/// (`smithy_query(query="shape",shape="AccessDeniedException")`). The tag family does not
/// LIST it, which is why the refusal is serialized here rather than returned from a
/// handler's modelled error channel.
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The operations whose input declares a caller-supplied `resourceArn` and nothing else that
/// addresses a resource — the tag family
/// (`smithy_query(query="operation",operation="TagResource")` and its siblings) — each with
/// the one-member body schema its resource string is read through.
///
/// The table is keyed by OPERATION, so a member of another operation that happens to be
/// spelled the same way is not judged by this gate until it is listed, and an operation that
/// takes no resource never has its body read here.
const RESOURCE_MEMBERS: [(&str, &::aws_smithy_schema::Schema); 3] = [
    ("TagResource", &RESOURCE_ARN_REQUEST),
    ("UntagResource", &RESOURCE_ARN_REQUEST),
    ("ListTagsForResource", &RESOURCE_ARN_REQUEST),
];

/// The sentence the modelled error states about itself, answered only in the unreachable
/// case where this operation is reached for a request whose resource cannot be read back.
const INSUFFICIENT_PERMISSION: &str =
    "You don't have permission to perform this operation. Check your IAM permissions and try \
     again.";

/// Answers one request `AccessDeniedException` (HTTP 403), with the denial built from that
/// request.
///
/// Holds nothing: every variable part of the sentence — the calling principal, the action the
/// call asks for and the value it named the resource by — is read off the request and the
/// account's own identity, so there is no state to carry.
pub(crate) struct DenyUnnamedResource;

impl ErasedOperation for DenyUnnamedResource {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        // Nothing about this refusal is read from the request context: it is decided from the
        // request's own operation and body.
        let _ = ctx;
        // The answer is computed in one call that reads, uses and drops the service
        // definition: `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
        // await below would make this future non-`Send`.
        let response = refusal(svc, req);
        Box::pin(async move { response })
    }
}

/// The 403 for `req`, rendered through this service's own protocol — the same envelope
/// (`message`, and the same error-type header) a handler-raised `AccessDeniedException` goes
/// out in, so a refusal taken here and one taken in a handler are indistinguishable on the
/// wire.
fn refusal(
    svc: &dyn Service,
    req: &aws_smithy_runtime_api::http::Request,
) -> aws_smithy_runtime_api::http::Response {
    let definition = svc.definition();
    // The protocol THIS request is served in: the request is read, and the denial answered,
    // in the one codec the request arrived in.
    let wire = definition.wire_protocol(req);
    let Some((operation, resource)) = unauthorized_resource(wire.protocol(), req) else {
        // Unreachable: this operation is reached only for a request whose operation and
        // unauthorizable resource `Service::override_operation` has already read out of these
        // same bytes. A request this cannot read names no action and no resource, so the
        // refusal names neither, and falls back to the error's own sentence.
        return definition.framework_error_response(
            aws_smithy_schema_server::ProtocolError::Unauthenticated {
                code: ACCESS_DENIED_CODE,
                message: INSUFFICIENT_PERMISSION,
            },
            req,
        );
    };
    let denial = crate::ops::types::AccessDeniedException {
        message: ::service_common::iam_authorization::not_authorized_to_perform(
            &::service_common::caller_principal::ambient_user_arn(),
            &::service_common::iam_authorization::iam_action(
                definition.identity().signing_name(),
                operation,
            ),
            &resource,
        ),
    };
    wire.protocol()
        .serialize_error(
            &denial,
            &crate::ops::types::SCHEMA_ACCESS_DENIED_EXCEPTION,
            ACCESS_DENIED_CODE,
            ACCESS_DENIED_STATUS,
        )
        .unwrap_or_else(|error| definition.framework_error_response(error, req))
}

/// The operation `req` names and the value it named its resource by, when `req` is one of
/// [`RESOURCE_MEMBERS`] and that value is not an ARN — the one read both the dispatch gate
/// and the refusal it answers with are taken from.
///
/// The read and the gate are the shared ones
/// (`service_common::addressed_resource::unaddressed_resource`); what this adds is what
/// "names a resource" means for this service, which is the tree's one caller-supplied-ARN
/// parse: a value carrying the six fields of an ARN, with a partition, a vendor and a
/// resource in them, names a resource, and anything else names none.
pub(crate) fn unauthorized_resource(
    protocol: &dyn ServerHttpProtocol,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<(&'static str, String)> {
    let members: ResourceMembers<'_> = &RESOURCE_MEMBERS;
    ::service_common::addressed_resource::unaddressed_resource(protocol, req, members, |value| {
        ::service_common::arn::parse(value).is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence names the calling principal, the action and the resource as supplied, and
    /// nothing else. The rows are the operations of this service the gate covers, so the
    /// action half is read off the request rather than fixed to one call.
    #[test]
    fn the_denial_names_the_principal_the_action_and_the_value_as_supplied() {
        let principal = ::service_common::caller_principal::ambient_user_arn();
        // Values of several forms that name no resource: a bare word, a colonless path, an
        // ARN missing its resource field, and the empty string.
        let unnamed = [
            "not-an-arn",
            "global-resolver/gr-1",
            "arn:aws:route53globalresolver:us-east-2:000000000000:",
            "",
        ];
        for supplied in unnamed {
            for (operation, _) in RESOURCE_MEMBERS {
                let action = ::service_common::iam_authorization::iam_action(
                    crate::state::ARN_SERVICE,
                    operation,
                );
                let stated = ::service_common::iam_authorization::not_authorized_to_perform(
                    &principal, &action, supplied,
                );
                let expected = format!(
                    "User: {principal} is not authorized to perform: \
                     route53globalresolver:{operation} on resource: {supplied}"
                );
                assert_eq!(
                    stated, expected,
                    "case {operation:?} on {supplied:?}: expected {expected:?}, got {stated:?}"
                );
            }
        }
    }
}
