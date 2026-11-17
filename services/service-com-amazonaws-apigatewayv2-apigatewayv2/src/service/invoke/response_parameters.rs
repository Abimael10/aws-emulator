// SPDX-License-Identifier: CC0-1.0

//! How an HTTP api's integration TRANSFORMS the response its backend answered, before the
//! client reads it.
//!
//! "Supported only for HTTP APIs. You use response parameters to transform the HTTP response
//! from a backend integration before returning the response to clients. Specify a key-value
//! map from a selection key to response parameters. The selection key must be a valid HTTP
//! status code within the range of 200-599. Response parameters are a key-value map. The key
//! must match pattern `<action>:<header>.<location>` or `overwrite.statuscode`. The action
//! can be append, overwrite or remove. The value can be a static value, or map to response
//! data, stage variables, or context variables that are evaluated at runtime"
//! (`smithy_query(query="doc",shape="CreateIntegrationRequest",member="ResponseParameters")`).
//!
//! So one map of the integration's `ResponseParameters` is applied to one response: the one
//! the backend's own status SELECTS, and no other. What this module owns is the ORDER of that
//! sentence — which map the status selects, and what applying each of its entries leaves
//! behind. Both halves of the mapping LANGUAGE are read where this service owns each, so a
//! key or a value cannot mean one thing at the write and another here: the KEY grammar is
//! `crate::state::parameter_mapping` (`mapping_key`, `mapped_header`, `STATUS_CODE_OPERAND`),
//! the same reading that refuses a mapping on a header the gateway owns, and the VALUE
//! language is [`super::service_action::parameters::Mapping::of_response`].
//!
//! What this transforms is a response a BACKEND answered: a refusal the gateway itself
//! decided (an integration that produced no response, a throttled route) is the gateway's
//! answer and not the integration's, so the integration's mapping is not applied to it.
//!
//! Everything here is a PURE function of the response, the integration's stored mapping and
//! the call: nothing touches state, the clock or the socket, and a mapping this cannot carry
//! out leaves the message as the backend answered it rather than failing the request — a
//! parameter whose source the call has no value for maps nothing, which is what makes an
//! expression naming an absent header leave the response alone.

use std::collections::BTreeMap;

use aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;

use super::proxy_event::{Invocation, ProxyRequest};
use super::service_action::parameters::{Answered, Mapping};
use crate::state::parameter_mapping::{
    mapped_header, mapping_key, APPEND_ACTION, OVERWRITE_ACTION, REMOVE_ACTION,
    STATUS_CODE_OPERAND,
};

/// The bounds of a response status (RFC 9110 §15: three digits, 100 through 599), which is
/// what an `overwrite` of the status may name.
const FIRST_STATUS: u16 = 100;
const LAST_STATUS: u16 = 599;

/// How much this transform may ADD to the message it is applied to, across every entry of
/// the selected mapping: the total bytes of the header values it writes.
///
/// A mapped value can name a whole part of the call (`$response.body`, `$request.body`), and
/// how many entries a stored mapping holds is not bounded by the member's own shape, so the
/// work and the memory ONE response costs are bounded here instead of by what was stored —
/// the way every other renderer of caller-written text in this tree bounds its output. Past
/// it the remaining entries are not applied and the message keeps what the earlier ones left
/// (logged), rather than growing with the mapping. The size itself is this transform's own
/// ceiling and no rule of the service: a mapped header value is a field value, and a real
/// field is orders of magnitude smaller than this.
const MAX_MAPPED_BYTES: usize = 64 * 1024;

/// The response header that frames the body, re-stated for the message this leaves behind
/// rather than copied: the body is unchanged by any mapping here, so its length is too.
const CONTENT_LENGTH_HEADER: &str = "content-length";

/// What one response mapping is evaluated against: the call the response answers.
///
/// Both halves are what the gateway already read for the invocation — the request as the
/// backend saw it and the facts of the invocation — so a mapping expression resolves against
/// the same values the integration's own event carried.
pub struct Call<'c> {
    pub request: &'c ProxyRequest,
    pub invocation: &'c Invocation,
}

/// `response` as this integration's `ResponseParameters` leave it.
///
/// The map the response's own status selects is the one applied; a response no selection key
/// names, and an integration carrying no mapping at all, is answered exactly as the backend
/// answered it.
pub fn transformed(
    response: RtResponse,
    parameters: &BTreeMap<String, BTreeMap<String, String>>,
    call: &Call<'_>,
) -> RtResponse {
    let status = response.status().as_u16();
    let Some(selected) = selected_mapping(parameters, status) else {
        return response;
    };
    // The body is read as the bytes it is; a response whose body is NOT in memory is one
    // this cannot carry across a transform without losing it, so it is answered exactly as
    // the backend answered it rather than re-framed around an empty body.
    let Some(body) = response.body().bytes().map(<[u8]>::to_vec) else {
        ::tracing::debug!(
            "this integration's answer carries a body that is not in memory, so its response \
             mapping is not applied to it"
        );
        return response;
    };
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
        .collect();
    let mut mapped_status = status;
    // How many bytes the applied entries have written so far ([`MAX_MAPPED_BYTES`]).
    let mut mapped_bytes = 0_usize;
    // What a value resolves against is the BACKEND's own answer, as it stands before any of
    // these mappings is applied: `$response.header.x` names what the backend answered, not
    // what an earlier entry of the same map left behind. ONE mapping is held across the
    // entries, so each source it reads — the answer's document, the request's, the call's
    // context — is computed at most once however many entries name it.
    let answered_headers = headers.clone();
    let mut mapping = Mapping::of_response(
        call.request,
        call.invocation,
        Answered {
            headers: &answered_headers,
            body: &body,
        },
    );
    for (key, value) in selected {
        let Some((action, operand)) = mapping_key(key) else {
            ::tracing::debug!(
                mapping = %key,
                "this integration's response mapping names no operation on the response, so \
                 nothing is mapped for it"
            );
            continue;
        };
        if operand.eq_ignore_ascii_case(STATUS_CODE_OPERAND) {
            if !action.eq_ignore_ascii_case(OVERWRITE_ACTION) {
                continue;
            }
            if let Some(named) = mapping
                .value_of(value)
                .and_then(|named| named.trim().parse::<u16>().ok())
                .filter(|named| (FIRST_STATUS..=LAST_STATUS).contains(named))
            {
                mapped_status = named;
            }
            continue;
        }
        let Some(header) = mapped_header(operand) else {
            continue;
        };
        let lowered = header.to_ascii_lowercase();
        if action.eq_ignore_ascii_case(REMOVE_ACTION) {
            headers.retain(|(name, _)| name != &lowered);
            continue;
        }
        let Some(resolved) = mapping.value_of(value) else {
            continue;
        };
        // What this transform ADDS is bounded, however many entries the mapping holds and
        // whatever part of the call each one names ([`MAX_MAPPED_BYTES`]).
        let written = mapped_bytes.saturating_add(lowered.len()).saturating_add(resolved.len());
        if written > MAX_MAPPED_BYTES {
            ::tracing::debug!(
                mapping = %key,
                "this integration's response mapping writes more than the {MAX_MAPPED_BYTES} \
                 bytes one answer carries, so this parameter and the ones after it are not \
                 applied"
            );
            break;
        }
        if action.eq_ignore_ascii_case(OVERWRITE_ACTION) {
            headers.retain(|(name, _)| name != &lowered);
        } else if !action.eq_ignore_ascii_case(APPEND_ACTION) {
            continue;
        }
        mapped_bytes = written;
        headers.push((lowered, resolved));
    }
    rendered(mapped_status, headers, body)
}

/// The mapping `status` selects, or `None` when the integration states none for it.
///
/// "The selection key must be a valid HTTP status code": a key is the status it selects, so
/// the response's own status is what names the map, and no other map applies to it.
fn selected_mapping(
    parameters: &BTreeMap<String, BTreeMap<String, String>>,
    status: u16,
) -> Option<&BTreeMap<String, String>> {
    parameters
        .iter()
        .find(|(key, mapping)| !mapping.is_empty() && key.trim().parse::<u16>() == Ok(status))
        .map(|(_, mapping)| mapping)
}

/// The transformed message as the response the caller receives: the mapped status, the
/// headers the mapping left, and the body the backend answered, unchanged.
///
/// A header value the HTTP layer refuses is left OFF rather than failing the whole response,
/// the way the backend's own answer is rendered: the mapping is the api's configuration and a
/// field that cannot be sent is named in the log instead.
fn rendered(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> RtResponse {
    let length = body.len().to_string();
    let mut response = RtResponse::new(
        StatusCode::try_from(status)
            .unwrap_or(StatusCode::try_from(FIRST_STATUS).expect("100 is a valid status code")),
        SdkBody::from(body),
    );
    let fields = response.headers_mut();
    for (name, value) in headers {
        if name == CONTENT_LENGTH_HEADER {
            continue;
        }
        if fields.try_append(name.clone(), value).is_err() {
            ::tracing::debug!(
                header = %name,
                "this integration's response mapping named a header value that cannot be \
                 sent, so it is left off the answer"
            );
        }
    }
    let _ = fields.try_insert(CONTENT_LENGTH_HEADER, length);
    response
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    /// The call every row below is mapped against.
    fn request() -> ProxyRequest {
        ProxyRequest {
            method: "GET".to_string(),
            raw_path: "/prod/orders".to_string(),
            route_path: "/orders".to_string(),
            raw_query_string: "page=2".to_string(),
            headers: BTreeMap::from([(
                "x-sent".to_string(),
                vec!["by-the-client".to_string()],
            )]),
            query_parameters: BTreeMap::from([("page".to_string(), vec!["2".to_string()])]),
            body: br#"{"wanted":"gold"}"#.to_vec(),
            route_key: "GET /orders".to_string(),
            api_id: "abc123".to_string(),
            stage_name: "prod".to_string(),
            stage_variables: BTreeMap::from([("tenant".to_string(), "acme".to_string())]),
            region: "us-east-1".to_string(),
            domain_name: "abc123.execute-api.us-east-1.amazonaws.com".to_string(),
        }
    }

    fn invocation() -> Invocation {
        Invocation {
            account_id: "000000000000".to_string(),
            request_id: "req-1".to_string(),
            received: SystemTime::now(),
            authorizer_context: None,
            authorizer_principal: None,
            admitted_jwt: None,
        }
    }

    /// A backend answer with `status`, two headers and a JSON body.
    fn answered(status: u16) -> RtResponse {
        let body = br#"{"origin":"lambda","note":"deliberate"}"#.to_vec();
        let mut response = RtResponse::new(
            StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        );
        let headers = response.headers_mut();
        let _ = headers.try_insert("content-type", "application/json");
        let _ = headers.try_insert("x-from-backend", "yes");
        response
    }

    /// The status, the headers and the body `mapping` leaves a `status` answer with.
    fn mapped(status: u16, mapping: &[(&str, &str, &str)]) -> (u16, Vec<(String, String)>, String) {
        let mut parameters: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (selection, key, value) in mapping {
            parameters
                .entry((*selection).to_string())
                .or_default()
                .insert((*key).to_string(), (*value).to_string());
        }
        let request = request();
        let invocation = invocation();
        let call = Call {
            request: &request,
            invocation: &invocation,
        };
        let response = transformed(answered(status), &parameters, &call);
        let body = response
            .body()
            .bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_default();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        (response.status().as_u16(), headers, body)
    }

    #[test]
    fn only_the_mapping_the_backends_status_selects_is_applied() {
        // Rows differ along which selection key the mapping states against the status the
        // backend answered: the status itself, another status, and a key that is no status.
        let cases: [(&str, u16, &str, u16); 4] = [
            ("the mapping keyed on the answered status", 500, "500", 403),
            ("a mapping keyed on another status", 500, "404", 500),
            ("the mapping keyed on a success", 201, "201", 403),
            ("a key that is no status at all", 500, "5XX", 500),
        ];
        for (case, answered_status, selection, expected) in cases {
            let (status, _, _) = mapped(
                answered_status,
                &[(selection, "overwrite:statuscode", "403")],
            );
            assert_eq!(
                status, expected,
                "case {case:?}: expected status {expected}, got {status}"
            );
        }
        let (status, _, _) = mapped(500, &[]);
        assert_eq!(
            status, 500,
            "case {:?}: expected the backend's own status, got {status}",
            "an integration carrying no response mapping"
        );
    }

    #[test]
    fn every_action_the_language_names_operates_on_the_response() {
        // One row per action, over a header the backend set and one it did not.
        let (_, headers, _) = mapped(
            200,
            &[
                ("200", "overwrite:header.x-from-backend", "mapped"),
                ("200", "append:header.x-added", "one"),
                ("200", "remove:header.content-type", ""),
            ],
        );
        let value = |name: &str| {
            headers
                .iter()
                .filter(|(held, _)| held == name)
                .map(|(_, value)| value.clone())
                .collect::<Vec<String>>()
        };
        assert_eq!(
            value("x-from-backend"),
            vec!["mapped".to_string()],
            "case {:?}: expected the overwritten value, got {headers:?}",
            "overwrite of a header the backend set"
        );
        assert_eq!(
            value("x-added"),
            vec!["one".to_string()],
            "case {:?}: expected the appended value, got {headers:?}",
            "append of a header the backend did not set"
        );
        assert!(
            value("content-type").is_empty(),
            "case {:?}: expected the header to be removed, got {headers:?}",
            "remove of a header the backend set"
        );
    }

    #[test]
    fn a_value_resolves_through_the_one_reading_of_the_mapping_language() {
        // Rows differ along the SOURCE a value names: each documented part of the call and of
        // the answer, a static value, and an expression this call has no value for. What each
        // expression resolves to is the mapping language's own rule
        // (`super::service_action::parameters`); these rows pin that the response path reads
        // values through it, including the two sources only this leg has.
        let cases: [(&str, &str, Option<&str>); 10] = [
            ("a static value", "mapped", Some("mapped")),
            (
                "a header of the response",
                "$response.header.x-from-backend",
                Some("yes"),
            ),
            (
                "a member of the response body",
                "$response.body.origin",
                Some("lambda"),
            ),
            (
                "a header of the request",
                "$request.header.x-sent",
                Some("by-the-client"),
            ),
            (
                "a query string parameter",
                "$request.querystring.page",
                Some("2"),
            ),
            (
                "a member of the request body",
                "$request.body.wanted",
                Some("gold"),
            ),
            ("the request path", "$request.path", Some("/orders")),
            ("a context variable", "$context.requestId", Some("req-1")),
            ("a stage variable", "$stageVariables.tenant", Some("acme")),
            (
                "an expression this call has no value for",
                "$request.header.x-absent",
                None,
            ),
        ];
        for (case, value, expected) in cases {
            let (_, headers, _) = mapped(200, &[("200", "overwrite:header.x-mapped", value)]);
            let mapped_value = headers
                .iter()
                .find(|(name, _)| name == "x-mapped")
                .map(|(_, value)| value.clone());
            assert_eq!(
                mapped_value.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {mapped_value:?}"
            );
        }
    }

    #[test]
    fn the_body_the_backend_answered_is_never_changed() {
        let (_, headers, body) = mapped(500, &[("500", "overwrite:statuscode", "403")]);
        assert_eq!(
            body, r#"{"origin":"lambda","note":"deliberate"}"#,
            "case {:?}: expected the backend's own body, got {body:?}",
            "a mapping that overwrites the status"
        );
        let length = headers
            .iter()
            .find(|(name, _)| name == CONTENT_LENGTH_HEADER)
            .map(|(_, value)| value.clone());
        assert_eq!(
            length.as_deref(),
            Some(body.len().to_string().as_str()),
            "case {:?}: expected the body's own length, got {length:?}",
            "the framing of a transformed response"
        );
    }

    #[test]
    fn a_status_outside_the_response_range_overwrites_nothing() {
        // Rows span the published bounds of the range, one past each, and a value that is no
        // status at all.
        let cases: [(&str, &str, u16); 6] = [
            ("the lowest response status", "100", 100),
            ("the highest response status", "599", 599),
            ("one below the range", "99", 500),
            ("one above the range", "600", 500),
            ("a value that is no number", "forbidden", 500),
            ("an expression naming nothing", "$context.nothing", 500),
        ];
        for (case, value, expected) in cases {
            let (status, _, _) = mapped(500, &[("500", "overwrite:statuscode", value)]);
            assert_eq!(
                status, expected,
                "case {case:?}: expected status {expected}, got {status}"
            );
        }
    }

    #[test]
    fn a_key_the_language_does_not_define_maps_nothing() {
        // Rows differ along the KEY: both spellings of the status key, a key naming no
        // operand, an unknown action, and a key that is no mapping expression at all.
        let cases: [(&str, &str, u16); 5] = [
            ("the documented status key", "overwrite.statuscode", 403),
            (
                "the status key written with a colon",
                "overwrite:statuscode",
                403,
            ),
            (
                "an action that is not one of the three",
                "rewrite:statuscode",
                500,
            ),
            (
                "an append of the status, which is not an operation on it",
                "append:statuscode",
                500,
            ),
            ("a key that is no mapping expression", "statuscode", 500),
        ];
        for (case, key, expected) in cases {
            let (status, _, _) = mapped(500, &[("500", key, "403")]);
            assert_eq!(
                status, expected,
                "case {case:?}: expected status {expected}, got {status}"
            );
        }
    }

    /// RULE: what the transform ADDS to one answer is bounded, however many entries the
    /// mapping holds and whatever part of the call each names — the entries up to the bound
    /// are applied and the rest are not, so neither the work nor the memory one response
    /// costs is sized by what was stored.
    #[test]
    fn the_bytes_one_answer_carries_bound_what_the_mapping_writes() {
        // Each entry maps the SAME large value, so the number of entries is what varies: one
        // well inside the bound, and enough of them to pass it several times over.
        let value = "x".repeat(1024);
        let within: Vec<(String, String)> = (0..4)
            .map(|index| (format!("append:header.x-mapped-{index}"), value.clone()))
            .collect();
        let past: Vec<(String, String)> = (0..256)
            .map(|index| (format!("append:header.x-mapped-{index}"), value.clone()))
            .collect();
        for (case, entries, all_applied) in [
            ("a mapping well inside the bound", &within, true),
            ("a mapping that writes past it", &past, false),
        ] {
            let rows: Vec<(&str, &str, &str)> = entries
                .iter()
                .map(|(key, value)| ("200", key.as_str(), value.as_str()))
                .collect();
            let (_, headers, _) = mapped(200, &rows);
            let written: usize = headers
                .iter()
                .filter(|(name, _)| name.starts_with("x-mapped-"))
                .map(|(name, value)| name.len() + value.len())
                .sum();
            let applied = headers
                .iter()
                .filter(|(name, _)| name.starts_with("x-mapped-"))
                .count();
            assert!(
                written <= MAX_MAPPED_BYTES,
                "case {case:?}: expected at most {MAX_MAPPED_BYTES} mapped bytes, got {written}"
            );
            assert_eq!(
                applied == entries.len(),
                all_applied,
                "case {case:?}: expected {} of {} entries applied, got {applied}",
                if all_applied { "all" } else { "not all" },
                entries.len()
            );
            assert!(
                applied > 0,
                "case {case:?}: expected the entries up to the bound to be applied, got none"
            );
        }
    }
}
