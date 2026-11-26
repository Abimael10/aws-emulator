// SPDX-License-Identifier: CC0-1.0

//! Route response keys: which `SelectionKey` a `RouteResponse` of a route may carry.
//!
//! A module of its own under [`crate::state`], because more than one operation writes one —
//! `CreateRouteResponse` and `UpdateRouteResponse` — so both hold it to one requirement and
//! state it in one sentence. [`crate::state`] re-exports every public item here, so a
//! handler reaches them the way it reaches the rest of this service's state helpers.
//!
//! A route response is what makes a WebSocket route TWO-WAY: "To define a two-way route,
//! create a route response", which is the one thing API Gateway reads a route response's key
//! for ([`crate::service::websocket::dispatch::is_two_way`] is this service's reader of that
//! fact). So the key is not a selector a caller composes: the service serves exactly one of
//! them ([`SUPPORTED_ROUTE_RESPONSE_KEY`]), and a route response under any other key would
//! select nothing and configure nothing.

use super::route_key::DEFAULT_ROUTE_KEY;

/// The one `RouteResponseKey` this service supports: the catch-all
/// ([`super::route_key::DEFAULT_ROUTE_KEY`]), whose presence on a route is what marks the
/// route two-way.
pub const SUPPORTED_ROUTE_RESPONSE_KEY: &str = DEFAULT_ROUTE_KEY;

/// The sentence API Gateway refuses a `RouteResponseKey` it does not serve with.
///
/// Every part of it is FIXED text — the only value-looking token is the key the service does
/// support, which the sentence NAMES rather than echoes from the request — so one wording
/// serves every key a request can send, and it says what the supported key means as well as
/// what it is.
pub fn unsupported_route_response_key_message() -> String {
    format!(
        "Currently, only {SUPPORTED_ROUTE_RESPONSE_KEY} is supported as a RouteResponseKey. \
         The creation of the RouteResponse with a key of {SUPPORTED_ROUTE_RESPONSE_KEY} will \
         indicate to API Gateway that this Route is intended to handle two-way \
         communication."
    )
}

/// `Ok(())`, or why `route_response_key` is no route response key this service serves — the
/// message for the operation's `BadRequestException`.
///
/// One rule: a route response carries the ONE key the service supports
/// ([`SUPPORTED_ROUTE_RESPONSE_KEY`]). Weighed by every writer of a route response key
/// before anything is persisted, so a key the service would never select cannot reach the
/// store through the create and cannot be patched onto a stored response either.
pub fn validate_route_response_key(route_response_key: &str) -> Result<(), String> {
    if route_response_key == SUPPORTED_ROUTE_RESPONSE_KEY {
        return Ok(());
    }
    Err(unsupported_route_response_key_message())
}

#[cfg(test)]
mod tests {
    use super::{
        unsupported_route_response_key_message, validate_route_response_key,
        SUPPORTED_ROUTE_RESPONSE_KEY,
    };

    #[test]
    fn the_one_supported_key_is_accepted_and_every_other_form_is_refused() {
        let accepted = validate_route_response_key(SUPPORTED_ROUTE_RESPONSE_KEY);
        assert_eq!(
            accepted,
            Ok(()),
            "case {SUPPORTED_ROUTE_RESPONSE_KEY:?}: expected the key accepted, got {accepted:?}"
        );
        // The rows span the forms a caller can write a selection key in: a bare word, a
        // route key of the api's own protocol, one of the other reserved `$` keys, a key
        // that only differs in case, one with the key as a prefix, and the empty string.
        let refused = [
            "custom",
            "echo",
            "sendMessage",
            "$connect",
            "$disconnect",
            "$DEFAULT",
            "$default-elsewhere",
            "GET /items",
            "",
        ];
        for route_response_key in refused {
            let weighed = validate_route_response_key(route_response_key);
            assert_eq!(
                weighed,
                Err(unsupported_route_response_key_message()),
                "case {route_response_key:?}: expected the unsupported-key refusal, got \
                 {weighed:?}"
            );
        }
    }

    #[test]
    fn the_refusal_names_the_key_the_service_serves_and_what_it_configures() {
        let message = unsupported_route_response_key_message();
        assert_eq!(
            message,
            "Currently, only $default is supported as a RouteResponseKey. The creation of \
             the RouteResponse with a key of $default will indicate to API Gateway that \
             this Route is intended to handle two-way communication.",
            "expected the fixed sentence for an unsupported route response key, got \
             {message:?}"
        );
    }
}
