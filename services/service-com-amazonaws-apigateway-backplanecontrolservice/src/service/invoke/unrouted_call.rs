// SPDX-License-Identifier: CC0-1.0

//! What a deployed stage answers a call NO deployed method serves.
//!
//! A stage resolves the method a request is served by from the path it addressed and the verb
//! it used. Where that resolution finds nothing, what the endpoint answers depends on what the
//! deployment holds at the path and on whether the request authenticated anybody:
//!
//! * a path the deployment holds NO resource for is a route the endpoint cannot place at all,
//!   and so is one carrying no credential: `403` `{"message":"Missing Authentication Token"}`,
//!   the refusal API Gateway answers an unplaceable request with;
//! * a path the deployment DOES hold a resource for, reached with a SigV4 credential, is
//!   AUTHORIZED before the method is resolved: `execute-api:Invoke` on the ARN built from the
//!   verb the request used and the path it addressed, decided from the caller's own policies
//!   and the api's document exactly as a served method's invoke is
//!   ([`super::execute_api_authorization`]). A caller that may not invoke that ARN is refused
//!   `403` with the authorization sentence and learns nothing about which verbs the resource
//!   defines; a caller that may is told the resource it asked for is not there — `404` through
//!   the `RESOURCE_NOT_FOUND` gateway response, naming the route and the method looked for
//!   ([`resource_not_found`]).
//!
//! So the answer to "is this verb defined here?" is only ever given to a caller allowed to ask,
//! which is the ordering AWS takes: the authorization decision is about the method ARN the
//! request names, and that ARN is built from the request rather than from the deployment — a
//! grant can name a verb no method serves, and a refusal is about the ask rather than about
//! what exists.
//!
//! Nothing here writes to this service's records; resolving the caller materialises the session
//! expiries the clock has already made due, as the gate beside it does.

use std::borrow::Cow;

use aws_smithy_runtime_api::http::Request as RtRequest;
use smithy_server_runtime::OperationContext;

use super::super::resource_policy;
use super::{
    access_denied, carries_sigv4_credential, execute_api_authorization, invalid_signature,
    most_specific_match, sigv4_access_key_id, sigv4_authentication, source_address_of,
    GatewayRefusal, MESSAGE_MEMBER, MISSING_AUTHENTICATION_TOKEN,
};
use crate::state::DeploymentEntry;

/// The deployment holds a resource at the path the request addressed, and the caller may
/// invoke it — there is simply no method for the verb it used: `404` through
/// `RESOURCE_NOT_FOUND`, whose own documented status is `404`
/// ([`crate::state::default_gateway_status_code`]).
///
/// The sentence NAMES what was looked for: the route — the path within the stage, with no
/// leading separator — and the HTTP method asked of it, which is what tells a developer
/// whether the path or the verb is the part the deployment does not carry. Its fixed text is
/// `No method found matching route ` / ` for http method ` and the closing full stop.
fn resource_not_found(route: &str, verb: &str) -> GatewayRefusal {
    GatewayRefusal {
        response_type: "RESOURCE_NOT_FOUND",
        message: Some(Cow::Owned(format!(
            "No method found matching route {route} for http method {verb}."
        ))),
        error_type: Some("NotFoundException"),
        validation_detail: Cow::Borrowed(""),
        message_member: MESSAGE_MEMBER,
    }
}

/// The resource path of the DEPLOYMENT that matches `request_path`, whether or not it carries
/// a method — the resource the request ADDRESSED, as against the one that SERVES it
/// ([`super::match_resource_path`], which reads only the paths a method is deployed under).
///
/// The selection itself is [`super::most_specific_match`], the one this plane resolves every
/// request with, so the two readers cannot drift on which path wins: what differs here is only
/// the candidates offered to it, the deployment's whole resource tree.
pub(super) fn addressed_resource_path<'d>(
    deployment: &'d DeploymentEntry,
    request_path: &str,
) -> Option<&'d str> {
    most_specific_match(
        deployment
            .resources
            .values()
            .map(|resource| resource.path.as_str()),
        request_path,
    )
}

/// One call no deployed method serves, as the stage resolved it.
pub(super) struct Unrouted<'a> {
    /// The api's own Region, which is the Region of the method ARN the decision is taken
    /// about.
    pub region: &'a str,
    /// The api the request resolved to.
    pub api_id: &'a str,
    /// The document the api carries, `None` for an api carrying none.
    pub policy: Option<&'a str>,
    /// The stage within that api.
    pub stage: &'a str,
    /// The path WITHIN the stage that the client addressed, leading `/` included.
    pub resource_path: &'a str,
    /// The deployed resource whose path matched it ([`addressed_resource_path`]), or `None`
    /// when the deployment holds none.
    pub addressed: Option<&'a str>,
    /// The interface VPC endpoint the request arrived through, when the endpoint it was
    /// addressed on names one.
    pub source_vpce: Option<&'a str>,
}

/// The refusal this stage answers `unrouted` with; see the module docs for the whole rule.
pub(super) fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
    unrouted: &Unrouted<'_>,
    now: ::std::time::SystemTime,
) -> GatewayRefusal {
    // A path the deployment holds no resource for is a route the endpoint cannot place, and a
    // request carrying no credential authenticated nobody to decide about: both are the
    // missing-token refusal, which is what API Gateway answers each with.
    let Some(addressed) = unrouted.addressed else {
        return MISSING_AUTHENTICATION_TOKEN;
    };
    if !carries_sigv4_credential(req) {
        return MISSING_AUTHENTICATION_TOKEN;
    }
    // The credential is AUTHENTICATED before anything is decided about the principal it
    // speaks for, exactly as it is for a method the stage does serve.
    if let Some(unauthenticated) = sigv4_authentication::refusal(req, ctx) {
        return invalid_signature(unauthenticated);
    }
    let caller = execute_api_authorization::signing_caller(
        engine,
        sigv4_access_key_id(req).unwrap_or_default().as_str(),
        ctx.credential_store(),
        now,
    );
    if let Some(message) = execute_api_authorization::expired_refusal(&caller) {
        return access_denied(message);
    }
    // The verb the REQUEST used, which is what the ARN a grant would have to name carries —
    // there is no deployed method to read one off.
    let verb = req.method().to_ascii_uppercase();
    // The address the request came from, read off the connection by the one reader this plane
    // keeps of it, so a clause over the network is decided here as it is for a served method.
    let source_ip = source_address_of(req);
    let call = resource_policy::Call {
        region: unrouted.region,
        api_id: unrouted.api_id,
        stage: unrouted.stage,
        verb: &verb,
        resource_path: addressed,
        request_path: unrouted.resource_path,
        source_vpce: unrouted.source_vpce,
        source_ip: Some(source_ip.as_str()),
        caller: resource_policy::Caller::Signed {
            named: &caller.arn,
            spellings: caller.spellings(),
        },
    };
    let stated = resource_policy::invoke_decision(unrouted.policy, &call);
    match execute_api_authorization::refusal(&caller, &call, stated) {
        Some(message) => access_denied(message),
        // The caller may invoke the ARN it named; the stage simply holds no method for it.
        None => resource_not_found(unrouted.resource_path.trim_start_matches('/'), &verb),
    }
}
