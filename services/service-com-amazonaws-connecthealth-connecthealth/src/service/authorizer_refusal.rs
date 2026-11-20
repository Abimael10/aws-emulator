// SPDX-License-Identifier: CC0-1.0

//! How this service answers a call addressed to a `domainId` that names no allocation it
//! ever made — the refusal its FRONT DOOR raises before any resource lookup runs.
//!
//! RECORDED behaviour: ConnectHealth resolves the `domainId` a request addresses to a
//! resource ARN at the edge, ahead of the component that owns the domain, and authorizes
//! that ARN. An id with no allocation history anywhere resolves to no ARN at all, so the
//! authorizer's OWN configuration failure is what answers the call, before the lookup that
//! would have told a caller whether the resource exists:
//!
//! * the error code is [`AUTHORIZER_CONFIGURATION_EXCEPTION`], which this service's model
//!   declares on NO shape
//!   (`smithy_query(query="shape",shape="AuthorizerConfigurationException",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`
//!   reports the shape is not found), so it is answered through the one channel a handler
//!   has for a code its model does not name ([`crate::scaffolded::Scaffolded::unmodeled`])
//!   rather than by renaming a modelled shape;
//! * it carries NO detail: the body's detail member is an explicit null, so a client
//!   reading it is given nothing rather than empty text. There is no sentence here to word
//!   — absence is the whole requirement — which is why this module holds no text
//!   constructor beside [`crate::service::error_detail`]'s.
//!
//! Why it is not a server fault: the condition is permanent and lies in the request's own
//! id, so answering it with a 5xx would have an SDK retry a call that can never succeed.
//! It is the AUTHORIZER that refused, ahead of the resource, so the status is the one an
//! authorization layer answers with — [`AUTHORIZER_CONFIGURATION_STATUS`] — and not this
//! service's modelled 404, which states something about the resource that the front door
//! never got far enough to know.

/// The wire error code this refusal carries (`__type`, and the `x-amzn-errortype` header).
pub const AUTHORIZER_CONFIGURATION_EXCEPTION: &str = "AuthorizerConfigurationException";

/// The HTTP status it is answered with: the authorization layer's own refusal status.
///
/// A client-side 4xx, never a 5xx — an SDK retry classifier treats a server fault as
/// retryable, and an id that was never allocated anywhere cannot start resolving.
pub const AUTHORIZER_CONFIGURATION_STATUS: u16 = 403;

/// The refusal a call addressed to a `domainId` with no allocation history earns, for
/// whichever operation's error channel answers it.
///
/// Generic over the operation's modelled error enum, so the ONE wording of this refusal is
/// shared rather than rebuilt per handler.
pub fn unresolvable_domain_id<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::unmodeled(
        AUTHORIZER_CONFIGURATION_EXCEPTION,
        AUTHORIZER_CONFIGURATION_STATUS,
        // No detail member value at all: see the module docs.
        None,
    )
}

#[cfg(test)]
mod tests {
    use smithy_server_runtime::OperationError;

    use super::*;
    use crate::scaffolded::Scaffolded;

    /// RULE: the refusal is the unmodelled authorizer code, at a client-side status, with
    /// no detail text — whatever the operation whose channel carries it.
    #[test]
    fn the_authorizer_refusal_carries_the_code_a_client_status_and_no_detail() {
        let refusal: Scaffolded<crate::ops::delete_domain::DeleteDomainErrorModeled> =
            unresolvable_domain_id();
        assert_eq!(
            refusal.shape_name(),
            AUTHORIZER_CONFIGURATION_EXCEPTION,
            "case \"the code\": expected {AUTHORIZER_CONFIGURATION_EXCEPTION:?}, got {:?}",
            refusal.shape_name()
        );
        let status = refusal.http_status();
        assert_eq!(
            status, AUTHORIZER_CONFIGURATION_STATUS,
            "case \"the status\": expected {AUTHORIZER_CONFIGURATION_STATUS}, got {status}"
        );
        assert!(
            (400..500).contains(&status),
            "case \"the status class\": a permanent refusal must not be retryable, got {status}"
        );
        let Scaffolded::Unmodeled(unmodeled) = &refusal else {
            panic!("case \"the channel\": expected the unmodelled channel, got {refusal}");
        };
        assert!(
            unmodeled.message.is_none(),
            "case \"the detail\": expected no detail at all, got {:?}",
            unmodeled.message
        );
    }
}
