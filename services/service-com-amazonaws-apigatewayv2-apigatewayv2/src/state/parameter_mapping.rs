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
const MAPPING_ACTIONS: &[&str] = &["append", "overwrite", "remove"];

/// The operand prefix that makes a mapping key name a HEADER — the `header` of
/// `<action>:<header|querystring|path>.<location>`.
const HEADER_OPERAND: &str = "header.";

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
        if let Some(header) = mapped_header(key) {
            if header_is_restricted(header) {
                return Err(restricted_header_message(header));
            }
        }
    }
    Ok(())
}

/// The header a mapping key operates on — the `<location>` of `<action>:header.<location>` —
/// or `None` for a key that operates on something else (a query string, the path, the status
/// code) or that is no mapping expression at all.
///
/// A key this does not read as a header operation is not refused here: what a mapping key
/// may name beyond this is the mapping language's own question, and this module answers only
/// which headers are reserved.
fn mapped_header(key: &str) -> Option<&str> {
    let (action, operand) = key.split_once(':')?;
    if !MAPPING_ACTIONS
        .iter()
        .any(|known| action.trim().eq_ignore_ascii_case(known))
    {
        return None;
    }
    let operand = operand.trim();
    // Read with `str::get` rather than a slice expression, so an operand shorter than the
    // prefix — or one whose prefix length falls inside a multi-byte character — answers
    // `None` instead of being indexed.
    let prefix = operand.get(..HEADER_OPERAND.len())?;
    if !prefix.eq_ignore_ascii_case(HEADER_OPERAND) {
        return None;
    }
    let header = operand.get(HEADER_OPERAND.len()..)?;
    (!header.is_empty()).then_some(header)
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
        ];
        for (key, expected) in cases {
            let read = mapped_header(key);
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
