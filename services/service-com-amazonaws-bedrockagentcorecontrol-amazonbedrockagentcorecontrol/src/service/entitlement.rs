// SPDX-License-Identifier: CC0-1.0

//! Whether the account this request speaks for may use the AgentCore Registry at all — the
//! decision taken BEFORE the request's members are validated, which is why it lives in the
//! service layer and not in a handler.
//!
//! The registry plane is an ENTITLED feature. An account that does not hold the AgentCore
//! Registry entitlement is refused every registry call, synchronously and identically on
//! every attempt, and nothing is recorded for it: the refusal is a property of the ACCOUNT,
//! not of the request, so it precedes both the request's members and the resource it names.
//! Taking it here is what makes that order real — the framework's centralized walk validates
//! the model's constraints before any handler runs, so a handler cannot take a decision that
//! precedes it, and a caller of an unentitled account would otherwise be answered
//! `ValidationException` for a malformed name where AWS answers `AccessDeniedException`.
//!
//! The three pieces:
//!
//! 1. **which calls** — the registry plane is named by the ROUTE: every modelled registry
//!    operation is bound under `/registries` (`OPERATIONS.md`), the registry's own routes and
//!    the record routes nested under them, so membership is read off the request line rather
//!    than enumerated per operation;
//! 2. **whether the account may** — [`crate::state`]'s own entitlement record, which nothing
//!    a client can call grants (`crate::state::registry_entitlement`);
//! 3. **the wording** — [`authorization::not_authorized`], the sentence this endpoint states
//!    a denial in, naming the principal that signed and the action it asked for.
//!
//! The decision is scoped to the registry plane: a call to any other family of this endpoint
//! is not looked at here, so the same credentials keep reaching every other operation.
//!
//! It is taken with no guard of this service's store held across anything else, and it writes
//! nothing.

use smithy_server_runtime::{OperationContext, Refused};

use super::authorization;

/// The first path segment every modelled route of the registry plane is bound under:
/// `POST /registries`, `GET /registries`, the three routes on `/registries/{registryId}` and
/// the seven record routes nested under it (`OPERATIONS.md`).
const REGISTRIES_PREFIX: &str = "registries";

/// The refusal a request to `operation` gets when it addresses the registry plane and this
/// account holds no AgentCore Registry entitlement, and `None` for every other request —
/// every call outside that plane, and every call of an account that does hold one.
///
/// Answered as a [`Refused`] — the code a client branches on and the sentence it shows — so
/// the status and the envelope stay with the protocol the request is served in.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
) -> Option<Refused> {
    if !addresses_registry_plane(ctx.parts()) {
        return None;
    }
    if entitled(engine) {
        return None;
    }
    let action = authorization::action_for(operation);
    let principal = authorization::request_principal(engine, ctx);
    ::tracing::info!(
        operation = %operation,
        action = %action,
        principal = %principal,
        "this account holds no agentcore registry entitlement, so the registry plane refuses \
         this call"
    );
    Some(Refused {
        code: crate::state::ACCESS_DENIED_SHAPE.to_string(),
        message: authorization::not_authorized(&principal, &action),
    })
}

/// Whether this request is a call of the registry plane, read off the route it came in on
/// (see the module docs).
fn addresses_registry_plane(parts: &::http::request::Parts) -> bool {
    let uri = parts.uri.to_string();
    ::service_common::uri_path(&uri)
        .split('/')
        .find(|segment| !segment.is_empty())
        == Some(REGISTRIES_PREFIX)
}

/// Whether the account this engine holds the state of holds the entitlement.
///
/// An engine carrying no store of this service holds no entitlement record either, and an
/// entitlement nothing granted is one the account does not have.
fn entitled<Eng: Send + Sync + 'static>(engine: &Eng) -> bool {
    crate::state::service_state(engine)
        .is_some_and(|state| state.lock().holds_registry_entitlement())
}
