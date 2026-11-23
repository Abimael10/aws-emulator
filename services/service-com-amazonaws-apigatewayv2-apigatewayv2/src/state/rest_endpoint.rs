// SPDX-License-Identifier: CC0-1.0

//! The REST endpoint a `ProductRestEndpointPage` documents, as the services that OWN API
//! Gateway's APIs report it.
//!
//! An `IdentifierParts` names one endpoint by its `RestApiId` and one of that API's stages.
//! API Gateway keeps ONE api-id space for the account: the REST APIs its older control
//! plane creates (`apigateway:CreateRestApi`, `apigateway:CreateStage`) and the HTTP and
//! WebSocket APIs this service creates (`CreateApi`, `CreateStage`) are addressed by ids
//! drawn from the same space and served on the same `execute-api` hosts. So "does the
//! account have the api this identifier names, with the stage it names" is a question asked
//! of EVERY store those APIs live in, and the identifier resolves when ANY of them holds the
//! pair. A page's content is GENERATED from the endpoint — "By default, this is the
//! documentation of your REST API from API Gateway" (`DisplayContentOverrides$Body`) — so a
//! write naming a pair no store holds is refused.
//!
//! Another service's store is reached through its own `state` accessor on the SHARED engine,
//! the way [`crate::service::authorization`] reads the IAM and STS stores, and under the same
//! lock order: every store asked here is locked and released BEFORE this service's own store
//! lock is taken by the caller, so each caller resolves first and reaches its own store
//! after.
//!
//! Nothing is cached: a write's identifier is resolved by that write, so an API created
//! after it does not retroactively change what an already-stored page documents, and a read
//! of the page asks nothing of anybody.

use super::*;

/// The sentence this service refuses a `RestEndpointIdentifier` that names no API of the
/// account, or no stage of the API it names, with: the identifier identifies no endpoint to
/// document.
///
/// Held here once, so every writer of a page refuses the same fact with the same words. It
/// has no variable parts — API Gateway names neither the API nor the stage in it.
pub const UNRESOLVED_REST_ENDPOINT_MESSAGE: &str =
    "Invalid restEndpointIdentifier stage or restApiId";

/// `Some(the refusal message)` when the REST endpoint `parts` identify cannot be resolved,
/// and `None` when it resolves.
///
/// A page's content is GENERATED from the endpoint it documents, so a write naming one that
/// does not exist describes no page at all: the write is refused `BadRequestException`
/// rather than storing a page that points at nothing. The identifier resolves when ANY store
/// of the account's APIs holds the `RestApiId`/`stage` pair — see this module's
/// documentation for why there is more than one.
///
/// A HOP THAT CANNOT BE MADE IS EVIDENCE OF NOTHING: an engine carrying no API Gateway (v1)
/// store cannot be asked which REST APIs the account has, so an identifier none of the
/// stores that CAN be asked holds is still admitted — the pair may well name an API of the
/// store that is missing. That is the same reading [`super::ec2_subnets`] takes of silence
/// from the owner of subnets.
///
/// Call it BEFORE this service's own store lock is taken — see this module's documentation.
pub fn unresolved_rest_endpoint<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    parts: &wire::IdentifierParts,
) -> Option<String> {
    let answers = [
        rest_api_holds_stage(engine, parts),
        own_api_holds_stage(engine, parts),
    ];
    // One store holding the pair resolves the identifier, whichever store it is: the api-id
    // space is the account's, not one control plane's.
    if answers.contains(&Some(true)) {
        return ::std::option::Option::None;
    }
    // A store that could not be asked at all leaves "the account has no such api" unproven,
    // so nothing is refused on it.
    if answers.iter().any(::std::option::Option::is_none) {
        return ::std::option::Option::None;
    }
    Some(UNRESOLVED_REST_ENDPOINT_MESSAGE.to_string())
}

/// Whether the REST (API Gateway v1) control plane holds the api `parts` names with the
/// stage it names, or `None` when that service cannot be asked at all.
fn rest_api_holds_stage<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    parts: &wire::IdentifierParts,
) -> Option<bool> {
    let owner = ::service_com_amazonaws_apigateway_backplanecontrolservice::state::service_state(
        engine,
    )?;
    let rest_apis = owner.lock();
    Some(
        rest_apis
            .rest_apis
            .get(&parts.rest_api_id)
            // A page documents ONE endpoint of ONE stage, and it is the stage that is
            // deployed and invocable, so an API the account has under a name it has no such
            // stage for identifies nothing either.
            .is_some_and(|rest_api| rest_api.stages.contains_key(&parts.stage)),
    )
}

/// Whether THIS service holds the api `parts` names with the stage it names, or `None` when
/// its own store cannot be reached.
///
/// The APIs this service creates share the account's api-id space and are invoked on the
/// same `execute-api` endpoint one of its stages is announced on, so an identifier naming
/// one of them names an endpoint the account really has.
fn own_api_holds_stage<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    parts: &wire::IdentifierParts,
) -> Option<bool> {
    let own = super::service_state(engine)?;
    let apis = own.lock();
    Some(
        apis.apis
            .get(&parts.rest_api_id)
            .is_some_and(|api| api.stages.contains_key(&parts.stage)),
    )
}
