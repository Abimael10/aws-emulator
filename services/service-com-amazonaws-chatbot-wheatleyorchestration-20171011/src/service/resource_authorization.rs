// SPDX-License-Identifier: CC0-1.0

//! The RESOURCE-SCOPED authorization decision on the `Resource` an association operation
//! addresses, taken BEFORE the modelled constraints on that member are judged.
//!
//! `AssociateToConfiguration` and `DisassociateFromConfiguration` are the two operations
//! whose input names a resource to LINK to a channel configuration (`Resource`, target
//! `ResourceIdentifier`, `@pattern` of a `custom-action/` ARN). AWS Chatbot decides whether
//! the caller's credentials are valid for that target FIRST: a value outside the namespace
//! this account's custom actions live in — including one that is not an ARN at all, which
//! the member's pattern would also reject — is refused with the modelled
//! `UnauthorizedException` (403, [`crate::state::UNAUTHORIZED_MESSAGE`]) rather than as a
//! request-constraint violation, because the request fails closed on a target the account's
//! namespace does not contain and nothing about the input is reported for it.
//!
//! The question asked here is exactly that NAMESPACE question
//! ([`crate::state::is_mintable_custom_action_arn`]: this partition, this service, the empty
//! region a custom action is account-scoped under, this account's id, a `custom-action/`
//! resource). What the ARN says about the action's own NAME is a property of the name rather
//! than of the namespace, so a value the namespace admits keeps the member's modelled
//! constraints and the refusal they earn.
//!
//! The framework judges every modelled constraint BEFORE a handler runs and answers a
//! violation in this service's input-constraint wording (400 `InvalidRequestException`,
//! `crate::service`), so a decision taken inside `handle` cannot come first — `handle` runs
//! after the validator, and the status a pre-dispatch refusal is served on is the
//! protocol's. That is why this is a seam: the gate below answers the authorization
//! question from the SAME reading the two handlers use
//! ([`crate::state::is_mintable_custom_action_arn`]) and renders the modelled error with the
//! status the model gives it, then hands every request it does not refuse to the generated
//! deserialize -> validate -> handle walk unchanged — so a malformed `ChatConfiguration`, an
//! absent member and every other constraint keep the refusal they had.

use std::sync::Arc;

use smithy_server_runtime::{BoxFuture, ErasedOperation, Operation, OperationError, Service};

use crate::ops::associate_to_configuration::{
    AssociateToConfigurationError as AssociateError,
    AssociateToConfigurationErrorModeled as AssociateModeled,
};
use crate::ops::disassociate_from_configuration::{
    DisassociateFromConfigurationError as DisassociateError,
    DisassociateFromConfigurationErrorModeled as DisassociateModeled,
};
use crate::ops::types::UnauthorizedException;

/// The member an association operation names the resource to link in: `Resource`
/// (`smithy_query(query="operation",operation="AssociateToConfiguration,
/// DisassociateFromConfiguration")`), a top-level member of both inputs, which this
/// service's `restJson1` wire carries under that name in the request document.
const RESOURCE_MEMBER: &str = "Resource";

/// Which of the two association operations a gate stands in front of.
///
/// They take the identical input and reach the identical decision, differing only in the
/// error channel the refusal travels through, so one gate type serves both.
#[derive(Debug, Clone, Copy)]
pub enum Association {
    /// `AssociateToConfiguration`, which LINKS the resource.
    ToConfiguration,
    /// `DisassociateFromConfiguration`, which unlinks it.
    FromConfiguration,
}

/// Serves one association operation with its resource-scoped authorization decision taken
/// BEFORE the modelled input constraints, then hands an authorized request to the generated
/// walk unchanged.
///
/// It is a pure READ — of the request's own `Resource` and of the ARN form this account
/// mints custom actions under — so it writes nothing and cannot change what a later caller
/// sees, and two identical requests get the identical answer.
///
/// Generic over the engine type for the same reason the rest of this crate is: the
/// operations it stands in front of are, so naming a concrete engine here would let this
/// seam drift from the service the router mounts.
pub struct ResourceAuthorizationGate<Eng> {
    /// The shared engine, so the generated walk this gate delegates to — and the handler
    /// behind it — reads THIS service's store rather than a private one.
    engine: Arc<Eng>,
    /// Which operation this gate serves.
    association: Association,
}

impl<Eng: Send + Sync + 'static> ResourceAuthorizationGate<Eng> {
    /// Build the gate for `association` from the shared engine.
    pub fn new(engine: Arc<Eng>, association: Association) -> Self {
        Self {
            engine,
            association,
        }
    }

    /// The routing key the model names this gate's operation by, read off the operation
    /// itself rather than spelled out here.
    pub fn operation_name(&self) -> &'static str {
        match self.association {
            Association::ToConfiguration => {
                <crate::ops::associate_to_configuration::AssociateToConfiguration<Eng> as Operation>::NAME
            }
            Association::FromConfiguration => {
                <crate::ops::disassociate_from_configuration::DisassociateFromConfiguration<Eng> as Operation>::NAME
            }
        }
    }

    /// The resource-scoped refusal this operation states, in its own error channel: the
    /// modelled `UnauthorizedException` with the one fixed sentence this service words an
    /// authorization failure with.
    fn unauthorized(&self) -> Box<dyn OperationError> {
        let exception = UnauthorizedException {
            message: Some(crate::state::UNAUTHORIZED_MESSAGE.to_string()),
        };
        match self.association {
            Association::ToConfiguration => Box::new(AssociateError::Modeled(
                AssociateModeled::UnauthorizedException(exception),
            )),
            Association::FromConfiguration => Box::new(DisassociateError::Modeled(
                DisassociateModeled::UnauthorizedException(exception),
            )),
        }
    }

    /// IAM's own refusal, in this operation's error channel: the decision belongs to the
    /// layer in front of the service, which is why it travels through the same channel the
    /// handlers lift it into rather than through a modelled variant.
    fn access_denied(&self, denied: crate::scaffolded::AccessDeniedException) -> Box<dyn OperationError> {
        match self.association {
            Association::ToConfiguration => Box::new(AssociateError::AccessDenied(denied)),
            Association::FromConfiguration => Box::new(DisassociateError::AccessDenied(denied)),
        }
    }
}

/// The `Resource` the request names, as it carries it — read from the request document
/// rather than from a deserialized input, because this decision is taken before the input
/// is validated.
///
/// `None` when the request carries no readable document, or no textual `Resource` member:
/// there is then no resource to authorize, and the request keeps whatever the generated
/// walk answers it (the modelled refusal for an absent or non-textual required member).
fn named_resource(req: &aws_smithy_runtime_api::http::Request) -> Option<String> {
    let body = req.body().bytes()?;
    let document: ::serde_json::Value = ::serde_json::from_slice(body).ok()?;
    document
        .get(RESOURCE_MEMBER)?
        .as_str()
        .map(ToString::to_string)
}

impl<Eng: Send + Sync + 'static> ErasedOperation for ResourceAuthorizationGate<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r smithy_server_runtime::OperationContext<'r>,
    ) -> BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        Box::pin(async move {
            let refusal = named_resource(req)
                .filter(|resource| !crate::state::is_mintable_custom_action_arn(resource))
                .map(|resource| {
                    // IAM is asked first, exactly as the handlers ask it, so a principal
                    // the account's policies do not allow this action for is told that
                    // rather than told about the target.
                    match crate::service::authorization::authorize(
                        &*self.engine,
                        ctx,
                        self.operation_name(),
                        &resource,
                    ) {
                        Err(denied) => self.access_denied(denied),
                        Ok(()) => self.unauthorized(),
                    }
                });
            if let Some(refusal) = refusal {
                return svc
                    .definition()
                    .protocol()
                    .serialize_error(
                        refusal.as_serializable(),
                        refusal.variant_schema(),
                        refusal.shape_name(),
                        refusal.http_status(),
                    )
                    .unwrap_or_else(|error| {
                        svc.definition().framework_error_response(error, req)
                    });
            }
            // Authorized for the target it named: run the generated deserialize ->
            // validate -> handle -> serialize walk. Reached through the routing table
            // rather than through `override_operation`, which would route straight back
            // here.
            match svc.definition().routing_table().get(self.operation_name()) {
                Some(op) => op.run(svc, req, ctx).await,
                None => svc.definition().routing_error_response(req),
            }
        })
    }
}
