// SPDX-License-Identifier: CC0-1.0

//! The stage this API's front door takes BEFORE the operation a request names is
//! dispatched: WHO is calling.
//!
//! # The rule
//!
//! Route 53 ARC's recovery-control API is reached through a REST front door that verifies
//! the credentials a request carries before anything reads what the call says, and a caller
//! it cannot verify — one that is unsigned, that carries a malformed `Authorization`
//! header, or that presents credentials this emulator does not hold — is refused there,
//! with that layer's own answer and never with one of this service's modelled errors:
//!
//! * HTTP 401;
//! * the one fixed sentence `Unauthorized`, under a LOWERCASE `message` member, and nothing
//!   else in the body;
//! * the wire error code `UnauthorizedException` in `x-amzn-errortype`, the header
//!   `restJson1` resolves an error shape from — a shape this model declares nowhere
//!   (`smithy_query(query="shape",shape="UnauthorizedException")` reports none), because the
//!   layer that states the refusal publishes no error shape for a client to resolve.
//!
//! That is the point of the class: the refusal happens ahead of the code that owns the
//! modelled errors, so it travels with its own status, code and envelope rather than
//! through an operation's error enum. A caller the layer DOES verify reaches the operation
//! it named and gets this service's own answers — the modelled `ValidationException` (400)
//! its handlers raise over the request's input, the `AccessDeniedException` (403) the shared
//! ARN gate raises for another account's resource, and the `ResourceNotFoundException` (404)
//! a lookup answers.
//!
//! # Where the rule lives
//!
//! In [`::service_common::front_door`], as the one statement of it for every service
//! reached through such a door: the status, the code, the sentence and the definition
//! wrapper that renders them are that module's (`UNAUTHORIZED_STATUS`,
//! `UNAUTHORIZED_CODE`, `UNAUTHORIZED_MESSAGE`, `unauthorized_response` and
//! `UnauthorizedDefinition`), and this module only names the wrapper for this service.
//!
//! The emulator verifies every request's signature centrally, and a service never checks
//! one itself: the authentication layer refuses the caller before any handler runs and
//! hands the service ONE structural refusal to word (the framework's
//! `ProtocolError::Unauthenticated`). The only seam handed that refusal is the service's own
//! definition, where `refusal_response` renders it — the same place
//! [`::service_common::authentication_refusal`] restates it for the services whose answer is
//! a modelled 403 shape. This service's answer is not a modelled shape and not a 403, so it
//! is rendered by that shared wrapper: the status and the envelope are part of what the
//! front door states, and the wording seam alone cannot replace them.
//!
//! Nothing here reads a credential, a secret, a request body or any state. The layer's
//! verdict is translated into the answer this endpoint states for it.

/// This service's model-derived half, with the authentication layer's refusal stated as the
/// front door states it (see the module docs) — the shared wrapper, under the name this
/// crate wires it in by.
pub(super) type FrontDoorDefinition<D> = ::service_common::front_door::UnauthorizedDefinition<D>;
