// SPDX-License-Identifier: CC0-1.0

//! How this service states the refusal of a request the emulator's authentication layer
//! would not verify — the authorization pre-check that stands in front of the
//! ObservabilityAdmin API, ahead of any operation.
//!
//! # The rule
//!
//! Every request to this endpoint is authorized before it is dispatched: the layer in
//! front of the service verifies the SigV4 credentials it carries, and a caller it cannot
//! verify — one that is unsigned, that carries a malformed `Authorization` header, or that
//! presents credentials this emulator does not hold — is refused there, with that layer's
//! own answer and never with one of this service's modelled errors:
//!
//! * HTTP 401;
//! * the one fixed sentence `Unauthorized`, under a LOWERCASE `message` member — this
//!   service's own modelled errors (`AccessDeniedException`, `ValidationException`,
//!   `ResourceNotFoundException`) state theirs under `Message`, so which member carries the
//!   sentence is what tells a client which layer answered;
//! * the wire error code `UnauthorizedException` in `x-amzn-errortype`, the header
//!   `restJson1` resolves an error shape from, and NOTHING in the body beside the sentence:
//!   the layer that states this refusal publishes no error shape for a client to resolve.
//!
//! None of the three is in this service's model — `smithy_query(query="digest")` lists
//! `AccessDeniedException` (400), `InternalServerException` (500) and
//! `TooManyRequestsException` (429) as its service-common errors and no 401 shape at all —
//! which is the point: the refusal happens ahead of the code that owns the modelled errors,
//! so it travels with its own status, code and envelope rather than through an operation's
//! error enum. A caller the layer DOES verify reaches the operation it named and gets this
//! service's own answers, including the modelled `AccessDeniedException` an operation raises
//! itself.
//!
//! # Where the rule lives
//!
//! In [`::service_common::front_door`], as the one statement of it for every service
//! reached through such a door: the status, the code, the sentence and the definition
//! wrapper that renders them are that module's (`UNAUTHORIZED_STATUS`,
//! `UNAUTHORIZED_CODE`, `UNAUTHORIZED_MESSAGE`, `unauthorized_response` and
//! `UnauthorizedDefinition`), and this module only wires it in under the name this crate
//! names it by.
//!
//! The emulator verifies every request's signature centrally, and a service never checks
//! one itself: the authentication layer refuses the caller before any handler runs and
//! hands the service ONE structural refusal to word (the framework's
//! `ProtocolError::Unauthenticated`). The only seam handed that refusal is the service's
//! own definition, where `refusal_response` renders it — the same place
//! [`::service_common::authentication_refusal`] restates the refusal for the services whose
//! answer is a modelled 403 shape. This service's answer is not a modelled shape and not a
//! 403, so it is rendered by that shared wrapper: the status and the envelope are part of
//! what the front door states, and the wording seam alone cannot replace them.
//!
//! Nothing here reads a credential, a secret, a request body or any state. The layer's
//! verdict is translated into the answer this endpoint states for it.

/// This service's model-derived half, with the authentication layer's refusal stated as
/// the front door states it (see the module docs) — the shared wrapper, under the name
/// this crate wires it in by.
pub type FrontDoorDefinition<D> = ::service_common::front_door::UnauthorizedDefinition<D>;
