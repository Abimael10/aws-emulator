// SPDX-License-Identifier: CC0-1.0

//! The PARAMETER MAPPING an HTTP api's integration transforms a request and a response
//! with, and the headers API Gateway reserves from it.
//!
//! "For HTTP API integrations without a specified integrationSubtype request parameters are
//! a key-value map specifying how to transform HTTP requests before sending them to the
//! backend. The key should follow the pattern &lt;action&gt;:&lt;header|querystring|path&gt;
//! .&lt;location&gt; where action can be append, overwrite or remove"
//! (`smithy_query(query="doc",shape="CreateIntegrationRequest",member="RequestParameters")`).
//! `Integration$ResponseParameters` is the same language applied to what the backend
//! answered, keyed by the status code the mapping is selected for.
//!
//! A mapping whose operand is a header the GATEWAY itself owns is not a transformation the
//! gateway can perform, because the value it would leave on the message is the one the
//! gateway writes when it forwards it. API Gateway refuses such a mapping at the write
//! rather than storing an integration that claims a transformation it would not make, and
//! that refusal is this module: which headers are reserved ([`RESTRICTED_HEADERS`],
//! [`RESTRICTED_HEADER_PREFIXES`]), and the one sentence the refusal is worded with
//! ([`restricted_header_message`]).
//!
//! The mapping LANGUAGE is only this one where the model says it is: an integration of an
//! HTTP api that names no `IntegrationSubtype`. A subtype's `RequestParameters` are "a
//! key-value map specifying parameters that are passed to AWS_PROXY integrations" — keys
//! name the action's own parameters, not an operation on the message — and a WebSocket api's
//! are "an integration request parameter name" mapped from a method request parameter. So
//! neither carries an operation on a header at all, and neither is read here.
//!
//! WHICH ENTRIES a parameter map stores at all is one rule across all three of those
//! languages, because all three spell a mapping's VALUE as where the parameter's value comes
//! from ([`stored_parameters`]), so it is answered here once for every writer of such a map.
//!
//! What a mapping KEY names is read here for both of its readers: the write, which refuses a
//! mapping on a header the gateway owns, and the data plane, which carries the mapping out
//! ([`mapping_key`], [`mapped_header`], and the one operand that names the response's own
//! status, [`STATUS_CODE_OPERAND`]). The VALUE half of the language is
//! `crate::service::invoke::service_action::parameters`, the one reading of what an
//! expression selects.

use std::collections::BTreeMap;

/// The headers a parameter mapping may not operate on, lowercased because a header name is
/// case-insensitive (RFC 9110 §5.1).
///
/// Each is a header the gateway itself writes on the message it forwards, so no mapping of
/// one survives to the backend:
///
/// * `authorization` carries the credential the gateway's own authorization reads — an
///   `AWS_IAM` route's SigV4 signature and a JWT or `REQUEST` authorizer's identity source
///   are all taken from it, so the gateway owns it for the whole request;
/// * `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`,
///   `trailer` and `upgrade` are the connection-specific headers a proxy does not forward
///   (RFC 9110 §7.6.1), so they describe the hop and not the message;
/// * `content-length`, `transfer-encoding` and `host` are the message's own framing and its
///   target (RFC 9112 §6, RFC 9110 §7.2), which the gateway re-states for the hop it makes.
pub const RESTRICTED_HEADERS: &[&str] = &[
    "authorization",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The header-name prefixes a parameter mapping may not operate on, lowercased.
///
/// AWS reserves these for its own signalling between a client, the gateway and the backend
/// (`x-amz-` and `x-amzn-` name the headers the AWS protocols carry — a request id, a trace
/// header, a target — and `apigw-` the ones API Gateway adds itself), so a mapping cannot
/// take one over.
pub const RESTRICTED_HEADER_PREFIXES: &[&str] = &["apigw-", "x-amz-", "x-amzn-"];

/// The actions a mapping key may name: "action can be append, overwrite or remove"
/// (`CreateIntegration$RequestParameters`).
pub const APPEND_ACTION: &str = "append";
pub const OVERWRITE_ACTION: &str = "overwrite";
pub const REMOVE_ACTION: &str = "remove";
const MAPPING_ACTIONS: &[&str] = &[APPEND_ACTION, OVERWRITE_ACTION, REMOVE_ACTION];

/// The operand prefix that makes a mapping key name a HEADER — the `header` of
/// `<action>:<header|querystring|path>.<location>`.
const HEADER_OPERAND: &str = "header.";

/// The operand that makes a key name the RESPONSE's own status — the `statuscode` of
/// `overwrite.statuscode`, the one key of this language that operates on something other
/// than a header (`CreateIntegration$ResponseParameters`).
pub const STATUS_CODE_OPERAND: &str = "statuscode";

/// What separates a key's action from its operand. The member's own patterns spell the two
/// keys differently — `<action>:<header>.<location>` and `overwrite.statuscode` — so either
/// character ends the action and the rest of the key is the operand.
const ACTION_SEPARATORS: [char; 2] = [':', '.'];

/// How API Gateway words the refusal of a mapping that operates on a reserved header.
///
/// Every part of the sentence is fixed but the header name, which is the one the request's
/// own mapping key named, spelled as the request spelled it.
pub fn restricted_header_message(header: &str) -> String {
    format!(
        "Invalid mapping expression specified: Validation Result: warnings : [], errors : \
         [Operations on header {header} are restricted]"
    )
}

/// The entries of a parameter map a write STORES, out of the ones the request carried.
///
/// In every one of the parameter-mapping languages this service accepts, an entry's VALUE
/// names WHERE the parameter's value comes from: "the associated value is a method request
/// parameter value or static value" for a WebSocket api's integration, "you can provide
/// static values, or map request data, stage variables, or context variables" for a
/// subtype's parameters, and "for values, you can provide static values, or map request
/// data, …" for the `<action>:<header|querystring|path>.<location>` language read here
/// (`Integration$RequestParameters`). An entry carried with the EMPTY string behind it
/// therefore names no source at all — a static value is written quoted, so the empty one is
/// `''` — and it configures no mapping: the write keeps no key for it, so neither the
/// writing operation's answer nor any later read of the resource reports one. This is the
/// same reading of an empty value as the one a patch of an optional string member applies
/// ([`super::cleared_by_empty`]) and as the one the subtype gate applies when it weighs
/// whether an action's argument is defined ([`super::validate_subtype_request_parameters`]).
///
/// Read by every writer of such a map — the creates, the patches and the import — so the
/// entry a caller cleared cannot survive in one of them and be gone from another.
pub fn stored_parameters(
    carried: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    carried
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

/// The same rule over a map of parameter maps — `Integration$ResponseParameters`, "a
/// key-value map from a selection key to response parameters" — applied to each selection
/// key's own mapping ([`stored_parameters`]).
///
/// The SELECTION KEY's value is a map and not a mapping, so a key the request carried is
/// kept as carried, with whatever mappings of it named a source; what an empty value clears is
/// one parameter of that key's own map.
pub fn stored_selected_parameters<Parameters>(
    carried: impl IntoIterator<Item = (String, Parameters)>,
) -> BTreeMap<String, BTreeMap<String, String>>
where
    Parameters: IntoIterator<Item = (String, String)>,
{
    carried
        .into_iter()
        .map(|(selection, parameters)| (selection, stored_parameters(parameters)))
        .collect()
}

/// `Ok(())`, or why one of this integration's parameter mappings cannot be stored — the
/// message for the writing operation's `BadRequestException`.
///
/// Both maps are read, because both are the same mapping language: `RequestParameters`
/// transforms what the backend is sent and `ResponseParameters` what the caller is
/// answered, and a header the gateway owns is owned on either leg. The maps of a subtype
/// integration and of a WebSocket api's integration are a different language and are left
/// alone (see the module docs).
pub fn validate_integration_parameter_mappings(
    protocol_type: &str,
    integration_subtype: Option<&str>,
    request_parameters: &BTreeMap<String, String>,
    response_parameters: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), String> {
    if protocol_type != crate::state::HTTP_PROTOCOL_TYPE {
        return Ok(());
    }
    if integration_subtype.is_some_and(|subtype| !subtype.trim().is_empty()) {
        return Ok(());
    }
    let response_keys = response_parameters.values().flat_map(BTreeMap::keys);
    for key in request_parameters.keys().chain(response_keys) {
        if let Some(header) = keyed_header(key) {
            if header_is_restricted(header) {
                return Err(restricted_header_message(header));
            }
        }
    }
    Ok(())
}

/// The `(action, operand)` a mapping key names, or `None` for a key that names no operation
/// on the message at all — a key whose action is none of [`MAPPING_ACTIONS`], and a value
/// that is no mapping expression (a subtype parameter's name, a WebSocket integration's
/// parameter).
///
/// The action is compared case-insensitively and the operand carried through as written: what
/// the operand names — a header, the status — is [`mapped_header`]'s and the caller's to read.
pub fn mapping_key(key: &str) -> Option<(&str, &str)> {
    let (action, operand) = key.trim().split_once(ACTION_SEPARATORS)?;
    let action = action.trim();
    let operand = operand.trim();
    if !MAPPING_ACTIONS
        .iter()
        .any(|known| action.eq_ignore_ascii_case(known))
    {
        return None;
    }
    (!operand.is_empty()).then_some((action, operand))
}

/// The header an OPERAND operates on — the `<location>` of `header.<location>` — or `None`
/// for an operand that names something else (a query string, the path, the status code).
///
/// The prefix is compared case-insensitively, because a key may be written in any case, while
/// the header name is carried through as written: it is what a refusal echoes back, and the
/// message matches a field name case-insensitively itself.
pub fn mapped_header(operand: &str) -> Option<&str> {
    let prefix = operand.get(..HEADER_OPERAND.len())?;
    if !prefix.eq_ignore_ascii_case(HEADER_OPERAND) {
        return None;
    }
    operand
        .get(HEADER_OPERAND.len()..)
        .map(str::trim)
        .filter(|header| !header.is_empty())
}

/// The header a mapping KEY operates on, or `None` for a key that operates on something else.
///
/// A key this does not read as a header operation is not refused here: what a mapping key
/// may name beyond this is the mapping language's own question, and this module answers only
/// which headers are reserved.
fn keyed_header(key: &str) -> Option<&str> {
    let (_, operand) = mapping_key(key)?;
    mapped_header(operand)
}

/// Whether `header` is one of the headers a mapping may not operate on, read
/// case-insensitively against the two catalogs above.
fn header_is_restricted(header: &str) -> bool {
    let lowered = header.trim().to_ascii_lowercase();
    RESTRICTED_HEADERS.contains(&lowered.as_str())
        || RESTRICTED_HEADER_PREFIXES
            .iter()
            .any(|prefix| lowered.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_map(key: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(key.to_string(), "'a-value'".to_string())])
    }

    fn no_response_map() -> BTreeMap<String, BTreeMap<String, String>> {
        BTreeMap::new()
    }

    #[test]
    fn a_parameter_carried_with_nothing_behind_it_is_stored_under_no_key() {
        // Rows span what a request can carry for one entry of a parameter map: the empty
        // string, the empty STATIC value the language spells quoted, a value that is only
        // whitespace (text, so a value), a mapping expression, a static value, and a key of
        // each of the three mapping languages — the `<action>:<operand>` one, a subtype
        // parameter name and a WebSocket integration request parameter.
        let cases: &[(&str, &str, &str, bool)] = &[
            ("a mapping carried empty", "append:header.x-trace", "", false),
            (
                "the empty static value, which is a value",
                "append:header.x-trace",
                "''",
                true,
            ),
            (
                "a value that is only whitespace",
                "append:header.x-trace",
                " ",
                true,
            ),
            (
                "a mapping expression",
                "overwrite:header.x-key",
                "$context.requestId",
                true,
            ),
            ("a static value", "append:header.x-source", "'api'", true),
            (
                "a subtype parameter carried empty",
                "QueueUrl",
                "",
                false,
            ),
            (
                "a subtype parameter naming a source",
                "QueueUrl",
                "$request.header.queue",
                true,
            ),
            (
                "a WebSocket integration parameter carried empty",
                "integration.request.querystring.stage",
                "",
                false,
            ),
            (
                "a WebSocket integration parameter naming a source",
                "integration.request.querystring.stage",
                "method.request.querystring.stage",
                true,
            ),
        ];
        for (case, key, value, stored) in cases {
            let held = stored_parameters([((*key).to_string(), (*value).to_string())]);
            assert_eq!(
                held.contains_key(*key),
                *stored,
                "case {case:?}: expected the key stored {stored}, got {held:?}"
            );
            if *stored {
                assert_eq!(
                    held.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the value as written, got {held:?}"
                );
            }
        }
        // And the entries of ONE map are weighed one by one: the cleared one is gone and
        // every other is kept.
        let held = stored_parameters([
            ("append:header.x-trace".to_string(), String::new()),
            ("append:header.x-kept".to_string(), "'yes'".to_string()),
        ]);
        assert_eq!(
            held,
            BTreeMap::from([("append:header.x-kept".to_string(), "'yes'".to_string())]),
            "case {:?}: expected only the mapping that names a source, got {held:?}",
            "a map carrying one cleared entry beside one that names a source"
        );
    }

    #[test]
    fn a_selection_keys_own_mapping_is_weighed_by_the_same_rule() {
        let held = stored_selected_parameters([
            (
                "200".to_string(),
                BTreeMap::from([
                    ("append:header.x-kept".to_string(), "'yes'".to_string()),
                    ("append:header.x-cleared".to_string(), String::new()),
                ]),
            ),
            // A selection key whose every mapping was cleared keeps its own entry: the
            // key's value is a map and not a mapping.
            (
                "404".to_string(),
                BTreeMap::from([("append:header.x-cleared".to_string(), String::new())]),
            ),
        ]);
        let expected = BTreeMap::from([
            (
                "200".to_string(),
                BTreeMap::from([("append:header.x-kept".to_string(), "'yes'".to_string())]),
            ),
            ("404".to_string(), BTreeMap::new()),
        ]);
        assert_eq!(
            held, expected,
            "case {:?}: expected {expected:?}, got {held:?}",
            "a response mapping keyed on two statuses, one of them cleared outright"
        );
    }

    #[test]
    fn a_mapping_key_names_the_header_its_action_operates_on() {
        // The rows span the mapping grammar: each published action, the three operands, the
        // casing a key may be written in, and keys that are no mapping expression.
        let cases: &[(&str, Option<&str>)] = &[
            ("overwrite:header.authorization", Some("authorization")),
            ("append:header.x-trace", Some("x-trace")),
            ("remove:header.Authorization", Some("Authorization")),
            ("OVERWRITE:HEADER.Host", Some("Host")),
            // Operands that are not a header.
            ("overwrite:querystring.page", None),
            ("overwrite:path", None),
            ("overwrite:statuscode", None),
            // Not a mapping expression at all: a subtype parameter name, a WebSocket
            // integration's parameter, an unknown action, and an empty header name.
            ("QueueUrl", None),
            ("integration.request.header.authorization", None),
            ("rewrite:header.authorization", None),
            ("overwrite:header.", None),
            // The other separator the member's own patterns use (`overwrite.statuscode`),
            // which operates on the same header when the operand names one.
            ("overwrite.header.authorization", Some("authorization")),
            ("overwrite.statuscode", None),
        ];
        for (key, expected) in cases {
            let read = keyed_header(key);
            assert_eq!(
                read, *expected,
                "case {key:?}: expected the mapped header {expected:?}, got {read:?}"
            );
        }
    }

    #[test]
    fn a_mapping_on_a_reserved_header_is_refused_and_any_other_is_stored() {
        // One row per reserved header and per reserved prefix, plus headers a mapping may
        // operate on. `authorization` is the recorded case; every other row is a header the
        // issue did not cite.
        let mut cases: Vec<(String, bool)> = Vec::new();
        for header in RESTRICTED_HEADERS {
            cases.push(((*header).to_string(), true));
            // A header name is case-insensitive, so the same header in another casing is
            // the same header.
            cases.push((header.to_ascii_uppercase(), true));
        }
        for prefix in RESTRICTED_HEADER_PREFIXES {
            cases.push((format!("{prefix}request-id"), true));
        }
        for allowed in [
            "x-api-key",
            "content-type",
            "accept",
            "user-agent",
            "x-forwarded-for",
            "x-custom-header",
            // A name that merely STARTS with the characters of a reserved one is a
            // different header.
            "hostname",
            "connection-id",
            "x-amzcustom",
        ] {
            cases.push((allowed.to_string(), false));
        }

        for (header, restricted) in cases {
            for action in MAPPING_ACTIONS {
                let key = format!("{action}:header.{header}");
                let validated = validate_integration_parameter_mappings(
                    crate::state::HTTP_PROTOCOL_TYPE,
                    None,
                    &request_map(&key),
                    &no_response_map(),
                );
                assert_eq!(
                    validated.is_err(),
                    restricted,
                    "case {key:?}: expected refused {restricted}, got {validated:?}"
                );
                if restricted {
                    assert_eq!(
                        validated,
                        Err(restricted_header_message(&header)),
                        "case {key:?}: expected the refusal to name the header the request \
                         spelled, got {validated:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_response_mapping_is_held_to_the_same_reserved_headers() {
        let response = BTreeMap::from([(
            "200".to_string(),
            BTreeMap::from([("overwrite:header.Transfer-Encoding".to_string(), "'chunked'".to_string())]),
        )]);
        let validated = validate_integration_parameter_mappings(
            crate::state::HTTP_PROTOCOL_TYPE,
            None,
            &BTreeMap::new(),
            &response,
        );
        assert_eq!(
            validated,
            Err(restricted_header_message("Transfer-Encoding")),
            "a response mapping on a reserved header must be refused, got {validated:?}"
        );
    }

    #[test]
    fn the_other_two_mapping_languages_are_left_alone() {
        // A subtype integration's parameters name the action's own parameters, and a
        // WebSocket api's name a method request parameter: neither is an operation on a
        // header, so a key that merely READS like one configures nothing here.
        let key = "overwrite:header.authorization";
        let cases: &[(&str, Option<&str>)] = &[
            (crate::state::HTTP_PROTOCOL_TYPE, Some("SQS-SendMessage")),
            (crate::state::WEBSOCKET_PROTOCOL_TYPE, None),
            (crate::state::WEBSOCKET_PROTOCOL_TYPE, Some("")),
        ];
        for (protocol_type, subtype) in cases {
            let validated = validate_integration_parameter_mappings(
                protocol_type,
                *subtype,
                &request_map(key),
                &no_response_map(),
            );
            assert!(
                validated.is_ok(),
                "case ({protocol_type:?}, subtype {subtype:?}): expected the mapping to be \
                 left alone, got {validated:?}"
            );
        }
    }
}
