// SPDX-License-Identifier: CC0-1.0

//! Route keys: the grammar a route key is written in, which depends on the PROTOCOL of the
//! api the route belongs to, and the form the service stores an accepted one in.
//!
//! A module of its own under [`crate::state`], because the route key is the api's data-plane
//! MATCH key and more than one operation writes one — `CreateRoute`, `UpdateRoute`, and the
//! quick-create route `CreateApi`/`UpdateApi` name with their `RouteKey` member — so every
//! writer has to hold it to one grammar and store it in one form, or a key that reaches the
//! store matches no request and a duplicate of a stored key is not recognised as one.
//! [`crate::state`] re-exports every public item here, so a handler reaches them the way it
//! reaches the rest of this service's state helpers.

use super::{HTTP_PROTOCOL_TYPE, WEBSOCKET_PROTOCOL_TYPE};

/// The route key that "acts as a catch-all for any request made to your API"
/// (`CreateApi$RouteKey`): valid on an api of either protocol, and the key quick create
/// mints when the request names none.
pub const DEFAULT_ROUTE_KEY: &str = "$default";

/// The route key AWS documents as the one "API Gateway invokes when a client disconnects
/// from your API" — the counterpart of
/// [`crate::state::route_request_parameters::CONNECT_ROUTE_KEY`].
pub const DISCONNECT_ROUTE_KEY: &str = "$disconnect";

/// The route keys a WEBSOCKET api reserves: the two lifecycle routes a connection drives and
/// the catch-all. A key beginning with `$` that is none of these names no call the service
/// makes, so it is no route key at all.
pub const RESERVED_WEBSOCKET_ROUTE_KEYS: &[&str] = &[
    super::route_request_parameters::CONNECT_ROUTE_KEY,
    DISCONNECT_ROUTE_KEY,
    DEFAULT_ROUTE_KEY,
];

/// Whether `method` is a method a route key of this control plane may name.
///
/// The catalog is NOT restated here: it is read from the one source of the methods this
/// control plane serves a route for (`crate::ops::import_api::route_methods`, plus the
/// `ANY` catch-all that stands for all of them), the same source the CORS rules read, so the
/// grammar a route key is held to and the methods an import can produce one for cannot
/// drift. A method is upper case on the wire, so the comparison is case-insensitive and the
/// stored key carries the upper-cased form ([`normalized_route_key`]).
fn names_a_route_method(method: &str) -> bool {
    let method = method.to_ascii_uppercase();
    method == crate::ops::import_api::ANY_METHOD
        || crate::ops::import_api::route_methods().any(|served| served == method)
}

/// The clause API Gateway opens the refusal of a route key that is not written in the
/// grammar the api's protocol routes on with.
pub const ROUTE_KEY_NOT_FORMATTED_PROPERLY: &str = "The provided route key is not formatted properly";

/// How API Gateway words that refusal in full, for an api of `protocol_type`.
///
/// [`ROUTE_KEY_NOT_FORMATTED_PROPERLY`] is the whole sentence a WEBSOCKET api's refusal
/// carries: its route key is whatever its `RouteSelectionExpression` yields for a message, a
/// value the service states no shape for, so there is no format to spell out. An HTTP api's
/// key is the request LINE, so the sentence goes on to name the protocol it is a line of and
/// the format a key of it takes. Neither form carries a value from the request.
pub fn route_key_grammar_message(protocol_type: &str) -> String {
    if protocol_type == WEBSOCKET_PROTOCOL_TYPE {
        return ROUTE_KEY_NOT_FORMATTED_PROPERLY.to_string();
    }
    format!(
        "{ROUTE_KEY_NOT_FORMATTED_PROPERLY} for {protocol_type} protocol. Format should be \
         \"[HTTP METHOD] /[RESOURCE PATH]\" or \"$default\""
    )
}

/// How API Gateway words the refusal of a route key whose resource path carries a GREEDY
/// path variable anywhere but last.
///
/// Fixed text, naming nothing from the request: a greedy variable (`{name+}`) matches the
/// whole remainder of the path, so a segment after one could never be matched.
pub const GREEDY_VARIABLE_POSITION: &str =
    "Greedy variables may only be in last position of route key";

/// The method and the resource path a route key of the `METHOD /path` form names, or `None`
/// for a key of any other form.
///
/// The two halves are separated by ONE space and the path begins with the path separator, so
/// a key carrying no space, one whose path is not absolute, and one carrying further
/// whitespace (which no request line has) name no method and no path at all.
fn method_and_path(route_key: &str) -> Option<(&str, &str)> {
    let (method, path) = route_key.split_once(' ')?;
    if !path.starts_with('/') || path.chars().any(char::is_whitespace) {
        return None;
    }
    Some((method, path))
}

/// Whether this route-key path segment declares a GREEDY path variable: `{proxy+}` matches
/// every remaining segment of the path, where `{id}` matches exactly one.
fn is_greedy_variable(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with("+}")
}

/// The form this service STORES `route_key` in, for a route of an api of `protocol_type`.
///
/// The method half of an HTTP api's route key is an HTTP method, and a method is upper case
/// on the wire, so the key a request writes in any other case names the same route: it is
/// stored upper-cased, which is what makes it the key the data plane matches a request's
/// method against and what makes a second route with the same key a duplicate. The resource
/// path is left exactly as written — a path is case-sensitive — and so is every key with no
/// method half: the catch-all, and a WEBSOCKET api's keys, which are matched against the
/// value its `RouteSelectionExpression` yields rather than against a request line.
pub fn normalized_route_key(protocol_type: &str, route_key: &str) -> String {
    if protocol_type != HTTP_PROTOCOL_TYPE {
        return route_key.to_string();
    }
    match method_and_path(route_key) {
        Some((method, path)) => format!("{} {path}", method.to_ascii_uppercase()),
        None => route_key.to_string(),
    }
}

/// `Ok(())`, or why `route_key` is no route key of an api of `protocol_type` — the message
/// for the operation's `BadRequestException`.
///
/// The catch-all [`DEFAULT_ROUTE_KEY`] is a route key of an api of either protocol. Beyond
/// it, one rule per protocol, each the grammar that protocol's data plane matches on:
///
/// * an HTTP api routes on the request LINE, so a key is `[HTTP METHOD] /[RESOURCE PATH]`:
///   a method this control plane serves a route for ([`names_a_route_method`], in any case,
///   see [`normalized_route_key`]) and an absolute path;
/// * a WEBSOCKET api routes on the value its `RouteSelectionExpression` yields for the
///   message, so a key is a value that selector can produce: one of
///   [`RESERVED_WEBSOCKET_ROUTE_KEYS`], or any non-empty value that does not begin with `$` —
///   the prefix those reserved keys are drawn from, so a key carrying it names a call the
///   service makes for no route.
///
/// And one rule over the PATH of every key written as a request line, whatever the api's
/// protocol, since that is the one form whose path the service parses: a GREEDY variable
/// stands for every segment that follows it, so it is only ever the LAST segment
/// ([`GREEDY_VARIABLE_POSITION`]).
///
/// Shared by every writer of a route key, so the create and the patch of a route, and the
/// quick-create key an api is minted with, cannot hold one to a different grammar. A key that
/// is WELL-FORMED but already taken is a different refusal, `ConflictException`
/// ([`crate::state::duplicate_identity::route_key_in_use_message`]).
pub fn validate_route_key(protocol_type: &str, route_key: &str) -> Result<(), String> {
    if route_key == DEFAULT_ROUTE_KEY {
        return Ok(());
    }
    // The one form whose resource path this service parses: a method it routes on and an
    // absolute path. A key of any other shape names no path at all.
    let request_line =
        method_and_path(route_key).filter(|(method, _)| names_a_route_method(method));
    if protocol_type == WEBSOCKET_PROTOCOL_TYPE && request_line.is_none() {
        let selects_a_message = RESERVED_WEBSOCKET_ROUTE_KEYS.contains(&route_key)
            || (!route_key.is_empty() && !route_key.starts_with('$'));
        return selects_a_message
            .then_some(())
            .ok_or_else(|| route_key_grammar_message(protocol_type));
    }
    let Some((_, path)) = request_line else {
        return Err(route_key_grammar_message(protocol_type));
    };
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let greedy_before_the_end = segments
        .iter()
        .enumerate()
        .any(|(position, segment)| is_greedy_variable(segment) && position + 1 < segments.len());
    if greedy_before_the_end {
        // A WEBSOCKET api states ONE sentence for every route key it will not take (see
        // [`route_key_grammar_message`]); an HTTP api, whose keys ARE the request lines its
        // data plane matches, names the rule that failed.
        if protocol_type == WEBSOCKET_PROTOCOL_TYPE {
            return Err(route_key_grammar_message(protocol_type));
        }
        return Err(GREEDY_VARIABLE_POSITION.to_string());
    }
    Ok(())
}

/// How API Gateway words the refusal of a deployment of an api one of whose routes names no
/// target.
///
/// A deployment freezes the api's routes for a stage to serve, and a route with no target has
/// nothing to serve the requests it matches, so the deployment names the route that cannot be
/// served — by the id the service minted for it and the key it carries, both read from the
/// api's own state.
pub fn undefined_route_target_message(route_id: &str, route_key: &str) -> String {
    format!(
        "A route target (typically an Integration) must be defined for route {route_id} with \
         key {route_key}"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_route_key, route_key_grammar_message, validate_route_key, DEFAULT_ROUTE_KEY,
        GREEDY_VARIABLE_POSITION,
    };
    use crate::state::{HTTP_PROTOCOL_TYPE, WEBSOCKET_PROTOCOL_TYPE};

    #[test]
    fn a_route_key_of_each_protocols_grammar_is_accepted() {
        // The rows span what each protocol routes on: every method the set names (upper and
        // lower case), a path with variables and a greedy one, the catch-all, the two
        // WebSocket lifecycle keys and a custom selector value.
        let rows = [
            (HTTP_PROTOCOL_TYPE, "GET /items"),
            (HTTP_PROTOCOL_TYPE, "get /items"),
            (HTTP_PROTOCOL_TYPE, "ANY /{proxy+}"),
            (HTTP_PROTOCOL_TYPE, "GET /items/{proxy+}"),
            (HTTP_PROTOCOL_TYPE, "DELETE /items/{id}"),
            (HTTP_PROTOCOL_TYPE, "POST /"),
            (HTTP_PROTOCOL_TYPE, DEFAULT_ROUTE_KEY),
            (WEBSOCKET_PROTOCOL_TYPE, "$connect"),
            (WEBSOCKET_PROTOCOL_TYPE, "$disconnect"),
            (WEBSOCKET_PROTOCOL_TYPE, DEFAULT_ROUTE_KEY),
            (WEBSOCKET_PROTOCOL_TYPE, "sendmessage"),
            (WEBSOCKET_PROTOCOL_TYPE, "sendMessage"),
            (WEBSOCKET_PROTOCOL_TYPE, "send.message-1_x"),
            // A key written as a request line is a route key on either protocol, and a value
            // that is no request line at all is whatever the api's selector yields for a
            // message — a value carrying a space among them.
            (WEBSOCKET_PROTOCOL_TYPE, "GET /items"),
            (WEBSOCKET_PROTOCOL_TYPE, "send message"),
            (WEBSOCKET_PROTOCOL_TYPE, "GET items"),
        ];
        for (protocol_type, route_key) in rows {
            let accepted = validate_route_key(protocol_type, route_key);
            assert_eq!(
                accepted,
                Ok(()),
                "case ({protocol_type:?}, {route_key:?}): expected the key accepted, got \
                 {accepted:?}"
            );
        }
    }

    #[test]
    fn a_route_key_outside_the_protocols_grammar_is_refused() {
        // The rows span the ways a key can fail the grammar: no method half, a word that is
        // no method, a relative path, whitespace inside the path, emptiness, and — on a
        // WEBSOCKET api — a reserved-prefix key the service makes no call for.
        let rows = [
            (HTTP_PROTOCOL_TYPE, "/items"),
            (HTTP_PROTOCOL_TYPE, "items"),
            (HTTP_PROTOCOL_TYPE, "FETCH /items"),
            (HTTP_PROTOCOL_TYPE, "GET items"),
            (HTTP_PROTOCOL_TYPE, "GET /items and more"),
            (HTTP_PROTOCOL_TYPE, "$connect"),
            (HTTP_PROTOCOL_TYPE, ""),
            (WEBSOCKET_PROTOCOL_TYPE, "$unknown"),
            (WEBSOCKET_PROTOCOL_TYPE, "$"),
            (WEBSOCKET_PROTOCOL_TYPE, ""),
        ];
        for (protocol_type, route_key) in rows {
            let refused = validate_route_key(protocol_type, route_key);
            assert_eq!(
                refused,
                Err(route_key_grammar_message(protocol_type)),
                "case ({protocol_type:?}, {route_key:?}): expected the grammar refusal, got \
                 {refused:?}"
            );
        }
    }

    #[test]
    fn a_greedy_path_variable_is_accepted_only_as_the_last_segment() {
        // The rows span where a greedy variable can sit: last (accepted, including the whole
        // path), and before a literal, before another variable and before a second greedy
        // one (each refused with the position's own sentence rather than the grammar's).
        let accepted = ["ANY /{proxy+}", "GET /items/{proxy+}", "GET /a/{id}/{rest+}"];
        for route_key in accepted {
            let weighed = validate_route_key(HTTP_PROTOCOL_TYPE, route_key);
            assert_eq!(
                weighed,
                Ok(()),
                "case {route_key:?}: expected a greedy variable in last position accepted, \
                 got {weighed:?}"
            );
        }
        let refused = [
            "GET /{proxy+}/items",
            "ANY /{proxy+}/{id}",
            "GET /{proxy+}/{rest+}",
            "GET /a/{rest+}/b/c",
        ];
        for route_key in refused {
            let weighed = validate_route_key(HTTP_PROTOCOL_TYPE, route_key);
            assert_eq!(
                weighed,
                Err(GREEDY_VARIABLE_POSITION.to_string()),
                "case {route_key:?}: expected the greedy-position refusal, got {weighed:?}"
            );
            // The same key on a WEBSOCKET api is refused too, in the one sentence that api
            // states for every route key it will not take.
            let weighed = validate_route_key(WEBSOCKET_PROTOCOL_TYPE, route_key);
            assert_eq!(
                weighed,
                Err(route_key_grammar_message(WEBSOCKET_PROTOCOL_TYPE)),
                "case {route_key:?}: expected a WEBSOCKET api's own refusal, got {weighed:?}"
            );
        }
    }

    #[test]
    fn the_stored_route_key_carries_the_method_upper_cased() {
        // (the api's protocol, the key the request wrote, the key the store holds)
        let rows = [
            (HTTP_PROTOCOL_TYPE, "get /x", "GET /x"),
            (HTTP_PROTOCOL_TYPE, "Get /x", "GET /x"),
            (HTTP_PROTOCOL_TYPE, "GET /x", "GET /x"),
            // The path is case-sensitive, so only the method half moves.
            (HTTP_PROTOCOL_TYPE, "patch /Items/{Id}", "PATCH /Items/{Id}"),
            (HTTP_PROTOCOL_TYPE, DEFAULT_ROUTE_KEY, DEFAULT_ROUTE_KEY),
            // A WebSocket key is a selector value, not a request line: it is stored as
            // written.
            (WEBSOCKET_PROTOCOL_TYPE, "sendMessage", "sendMessage"),
            (WEBSOCKET_PROTOCOL_TYPE, "$connect", "$connect"),
        ];
        for (protocol_type, requested, expected) in rows {
            let stored = normalized_route_key(protocol_type, requested);
            assert_eq!(
                stored, expected,
                "case ({protocol_type:?}, {requested:?}): expected the stored key \
                 {expected:?}, got {stored:?}"
            );
        }
    }
}
