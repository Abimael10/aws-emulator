// SPDX-License-Identifier: CC0-1.0

//! How an HTTP api's integration TRANSFORMS the request, before the backend it fronts
//! receives it.
//!
//! "For HTTP API integrations without a specified integrationSubtype request parameters are a
//! key-value map specifying how to transform HTTP requests before sending them to the backend.
//! The key should follow the pattern `<action>:<header|querystring|path>.<location>` where
//! action can be append, overwrite or remove. For values, you can provide static values, or
//! map request data, stage variables, or context variables that are evaluated at runtime"
//! (`smithy_query(query="doc",shape="CreateIntegrationRequest",member="RequestParameters")`).
//!
//! So each entry of such an integration's `RequestParameters` is one operation on the request
//! the backend is sent, and this module is that transform — the REQUEST-leg twin of
//! [`super::response_parameters`], which applies the same language to what the backend
//! answered. Both halves of the language are read where this service owns each, so a key or a
//! value cannot mean one thing at the write and another here: the KEY grammar is
//! `crate::state::parameter_mapping` (`mapping_key`, `mapped_header`,
//! `mapped_query_parameter`, `PATH_OPERAND`), the same reading that refuses a mapping on a
//! header the gateway owns, and the VALUE language is
//! [`super::service_action::parameters::Mapping`].
//!
//! # What is transformed, and when
//!
//! The request the GATEWAY resolved ([`ProxyRequest`]) is what every backend of this plane is
//! then addressed from — the event a Lambda proxy integration is handed
//! ([`super::proxy_event`]) and the message an `HTTP_PROXY` integration forwards
//! ([`super::http_proxy`]) — so the transform is applied to it once, where the route's
//! integration is performed ([`super::dispatch`]), and each leg states the request as the
//! mapping left it. The route was already matched, on the path the CLIENT sent, so no mapping
//! here can change which route serves the request.
//!
//! Every action the key grammar names, over the parts of the request its operands name:
//!
//! | key | what applying it leaves behind |
//! |---|---|
//! | `overwrite:header.<name>` | that header, holding the mapped value alone |
//! | `append:header.<name>` | the mapped value beside the ones the request carried |
//! | `remove:header.<name>` | that one header gone, every other one untouched |
//! | `overwrite:querystring.<name>` | that query parameter, holding the mapped value alone |
//! | `append:querystring.<name>` | the mapped value added to the query string |
//! | `remove:querystring.<name>` | that one query parameter gone |
//! | `overwrite:path` | the path the backend is addressed at |
//!
//! The path carries no `append` or `remove` of its own: a request has exactly one path, so
//! there is nothing to add one to and nothing left of it to send if it were removed.
//!
//! Everything here is a PURE function of the request, the integration's stored mapping and the
//! call: nothing touches state, the clock or the socket, and a mapping this cannot carry out
//! leaves the request as it arrived rather than failing it — a parameter whose source the call
//! has no value for maps nothing, which is what makes an expression naming an absent header
//! leave the request alone.

use std::collections::BTreeMap;

use ::percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use super::proxy_event::{Invocation, ProxyRequest};
use super::service_action::parameters::Mapping;
use crate::service::data_plane::FieldValues;
use crate::state::parameter_mapping::{
    mapped_header, mapped_query_parameter, mapping_key, APPEND_ACTION, OVERWRITE_ACTION,
    PATH_OPERAND, REMOVE_ACTION,
};

/// How much this transform may ADD to the request it is applied to, across every entry of the
/// mapping: the total bytes of the values it writes.
///
/// A mapped value can name a whole part of the call (`$request.body`), and how many entries a
/// stored mapping holds is not bounded by the member's own shape, so the work and the memory
/// ONE request costs are bounded here instead of by what was stored — the same ceiling the
/// response leg applies, for the same reason. Past it the remaining entries are not applied
/// and the request keeps what the earlier ones left (logged), rather than growing with the
/// mapping.
const MAX_MAPPED_BYTES: usize = 64 * 1024;

/// The characters a value this transform writes into the query string keeps as written: RFC
/// 3986 §2.3's unreserved set. Everything else is percent-encoded by the maintained encoder
/// this crate already reads a request target with, so a mapped value carrying a `&`, an `=` or
/// a space is one parameter rather than several.
const QUERY_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// What the pairs of a query string are separated by, and what separates a pair's name from
/// its value (RFC 3986 §3.4, as `application/x-www-form-urlencoded` spells it).
const PAIR_SEPARATOR: char = '&';
const VALUE_SEPARATOR: char = '=';

/// Apply this integration's `RequestParameters` to `request`, and answer the path an
/// `overwrite:path` entry put on it — `None` when the mapping states none, which leaves the
/// backend addressed exactly where the integration names.
///
/// The path is answered rather than only written onto the request because the two backends of
/// this plane are addressed differently: a Lambda proxy integration is handed the request's
/// own path in its event, while an `HTTP_PROXY` integration forwards a message to the endpoint
/// its `IntegrationUri` names, whose path is that URI's.
pub fn transformed(
    request: &mut ProxyRequest,
    parameters: &BTreeMap<String, String>,
    invocation: &Invocation,
) -> Option<String> {
    if parameters.is_empty() {
        return None;
    }
    // What a value resolves against is the request AS IT ARRIVED: `$request.header.x` names
    // what the client sent, not what an earlier entry of the same map left behind. ONE mapping
    // is held across the entries, so each source it reads — the request's document, the call's
    // context — is computed at most once however many entries name it.
    let arrived = request.clone();
    let mut mapping = Mapping::of(&arrived, invocation);
    let mut overwritten_path = None;
    // How many bytes the applied entries have written so far ([`MAX_MAPPED_BYTES`]).
    let mut mapped_bytes = 0_usize;
    for (key, value) in parameters {
        let Some((action, operand)) = mapping_key(key) else {
            ::tracing::debug!(
                mapping = %key,
                "this integration's request mapping names no operation on the request, so \
                 nothing is mapped for it"
            );
            continue;
        };
        let Some(target) = Target::of(operand) else {
            ::tracing::debug!(
                mapping = %key,
                "this integration's request mapping names no part of the request, so nothing \
                 is mapped for it"
            );
            continue;
        };
        if action.eq_ignore_ascii_case(REMOVE_ACTION) {
            if target.is_path() {
                continue;
            }
            removed(request, &target);
            continue;
        }
        let appending = action.eq_ignore_ascii_case(APPEND_ACTION);
        if !appending && !action.eq_ignore_ascii_case(OVERWRITE_ACTION) {
            continue;
        }
        if appending && target.is_path() {
            continue;
        }
        let Some(resolved) = mapping.value_of(value) else {
            continue;
        };
        // What this transform ADDS is bounded, however many entries the mapping holds and
        // whatever part of the call each one names ([`MAX_MAPPED_BYTES`]).
        let written = mapped_bytes
            .saturating_add(target.named().len())
            .saturating_add(resolved.len());
        if written > MAX_MAPPED_BYTES {
            ::tracing::debug!(
                mapping = %key,
                "this integration's request mapping writes more than the {MAX_MAPPED_BYTES} \
                 bytes one request carries, so this parameter and the ones after it are not \
                 applied"
            );
            break;
        }
        mapped_bytes = written;
        match &target {
            Target::Header(name) => {
                written_field(&mut request.headers, name, appending, resolved);
            }
            Target::QueryParameter(name) => {
                if !appending {
                    request.raw_query_string = query_without(&request.raw_query_string, name);
                }
                request.raw_query_string =
                    query_with(&request.raw_query_string, name, &resolved);
                written_field(&mut request.query_parameters, name, appending, resolved);
            }
            Target::Path => {
                request.raw_path = resolved.clone();
                overwritten_path = Some(resolved);
            }
        }
    }
    overwritten_path
}

/// The part of the request one mapping key operates on.
enum Target {
    /// A header, by the lowercased name the request's own fields are keyed by.
    Header(String),
    /// A query-string parameter, by the name as the mapping spelled it: a parameter name is
    /// case-sensitive.
    QueryParameter(String),
    /// The path the backend is addressed at.
    Path,
}

impl Target {
    /// What `operand` operates on, or `None` for an operand naming no part of a REQUEST (the
    /// response's own status among them).
    fn of(operand: &str) -> Option<Self> {
        if let Some(header) = mapped_header(operand) {
            return Some(Self::Header(header.to_ascii_lowercase()));
        }
        if let Some(parameter) = mapped_query_parameter(operand) {
            return Some(Self::QueryParameter(parameter.to_string()));
        }
        operand
            .eq_ignore_ascii_case(PATH_OPERAND)
            .then_some(Self::Path)
    }

    /// Whether this is the request's path, which only `overwrite` operates on.
    fn is_path(&self) -> bool {
        matches!(self, Self::Path)
    }

    /// The name this target is held under, for the bound on what the transform writes.
    fn named(&self) -> &str {
        match self {
            Self::Header(name) | Self::QueryParameter(name) => name,
            Self::Path => PATH_OPERAND,
        }
    }
}

/// Drop everything the request carried for `target`.
fn removed(request: &mut ProxyRequest, target: &Target) {
    match target {
        Target::Header(name) => {
            request.headers.remove(name);
        }
        Target::QueryParameter(name) => {
            request.query_parameters.remove(name);
            request.raw_query_string = query_without(&request.raw_query_string, name);
        }
        Target::Path => {}
    }
}

/// Write `resolved` into `values` under `name`: beside what is already held for an `append`,
/// and in place of it for an `overwrite`.
fn written_field(values: &mut FieldValues, name: &str, appending: bool, resolved: String) {
    let held = values.entry(name.to_string()).or_default();
    if !appending {
        held.clear();
    }
    held.push(resolved);
}

/// `raw` with every pair named `name` dropped.
///
/// A pair's name is read through the tree's one decoder of that grammar
/// ([`decoded_name`]), so `a%20b=1` is the parameter `a b` here exactly as it is in the
/// request's own decoded parameters.
fn query_without(raw: &str, name: &str) -> String {
    raw.split(PAIR_SEPARATOR)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| decoded_name(pair) != name)
        .collect::<Vec<&str>>()
        .join(&PAIR_SEPARATOR.to_string())
}

/// `raw` with one more pair, `name=value`, percent-encoded as a query component
/// ([`QUERY_COMPONENT`]).
fn query_with(raw: &str, name: &str, value: &str) -> String {
    let pair = format!(
        "{}{VALUE_SEPARATOR}{}",
        utf8_percent_encode(name, QUERY_COMPONENT),
        utf8_percent_encode(value, QUERY_COMPONENT)
    );
    if raw.is_empty() {
        return pair;
    }
    format!("{raw}{PAIR_SEPARATOR}{pair}")
}

/// The name one raw query-string pair states, decoded.
fn decoded_name(pair: &str) -> String {
    ::aws_smithy_schema_server::query::parse_form_urlencoded(pair.as_bytes())
        .into_iter()
        .next()
        .map(|(name, _value)| name)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    /// The call every row below is mapped against: a client request carrying two headers, two
    /// query parameters and a JSON body.
    fn request() -> ProxyRequest {
        ProxyRequest {
            method: "GET".to_string(),
            raw_path: "/orders".to_string(),
            route_path: "/orders".to_string(),
            route_segments: vec!["orders".to_string()],
            raw_query_string: "page=2&drop=yes".to_string(),
            headers: BTreeMap::from([
                ("x-client".to_string(), vec!["by-the-client".to_string()]),
                ("x-keep".to_string(), vec!["kept".to_string()]),
            ]),
            query_parameters: BTreeMap::from([
                ("page".to_string(), vec!["2".to_string()]),
                ("drop".to_string(), vec!["yes".to_string()]),
            ]),
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

    /// The request `mapping` leaves behind, and the path it overwrote.
    fn mapped(mapping: &[(&str, &str)]) -> (ProxyRequest, Option<String>) {
        let parameters: BTreeMap<String, String> = mapping
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let mut request = request();
        let path = transformed(&mut request, &parameters, &invocation());
        (request, path)
    }

    /// The ONE value the mapped request states for a field, or `None` for one it holds none
    /// of — the reading a backend of this plane states a field by.
    fn field(values: &FieldValues, name: &str) -> Option<String> {
        crate::service::data_plane::joined_field(values, name)
    }

    #[test]
    fn every_action_the_language_names_operates_on_the_request() {
        // One row per action over each operand the key grammar names, against a field the
        // client sent and one it did not.
        let (request, path) = mapped(&[
            ("overwrite:header.x-client", "replaced"),
            ("append:header.x-keep", "added"),
            ("overwrite:header.x-injected", "injected"),
            ("remove:header.x-dropped", "''"),
            ("overwrite:querystring.page", "9"),
            ("append:querystring.injected", "static-q"),
            ("remove:querystring.drop", "''"),
            ("overwrite:path", "/elsewhere"),
        ]);
        let cases: &[(&str, Option<&str>)] = &[
            ("x-client", Some("replaced")),
            ("x-keep", Some("kept,added")),
            ("x-injected", Some("injected")),
            ("x-dropped", None),
        ];
        for (name, expected) in cases {
            let held = field(&request.headers, name);
            assert_eq!(
                held.as_deref(),
                *expected,
                "case {name:?}: expected the backend to read {expected:?}, got {held:?}"
            );
        }
        let parameters: &[(&str, Option<&str>)] = &[
            ("page", Some("9")),
            ("injected", Some("static-q")),
            ("drop", None),
        ];
        for (name, expected) in parameters {
            let held = field(&request.query_parameters, name);
            assert_eq!(
                held.as_deref(),
                *expected,
                "case {name:?}: expected the backend to read the parameter {expected:?}, got \
                 {held:?}"
            );
        }
        assert_eq!(
            request.raw_query_string, "injected=static-q&page=9",
            "case {:?}: expected the mapped query string, got {:?}",
            "the query string the backend is sent", request.raw_query_string
        );
        assert_eq!(
            path.as_deref(),
            Some("/elsewhere"),
            "case {:?}: expected the overwritten path, got {path:?}",
            "an overwrite of the request's path"
        );
        assert_eq!(
            request.raw_path, "/elsewhere",
            "case {:?}: expected the request to carry the overwritten path, got {:?}",
            "an overwrite of the request's path", request.raw_path
        );
    }

    #[test]
    fn a_header_a_mapping_does_not_name_reaches_the_backend_as_it_arrived() {
        // A removal takes exactly the one header it names: every other field the client sent
        // travels on, which is what makes a mapping a transformation rather than a filter.
        let (request, path) = mapped(&[("remove:header.x-client", "''")]);
        assert_eq!(
            field(&request.headers, "x-client"),
            None,
            "case {:?}: expected the named header gone, got {:?}",
            "a removal of one header", request.headers
        );
        assert_eq!(
            field(&request.headers, "x-keep").as_deref(),
            Some("kept"),
            "case {:?}: expected every other header intact, got {:?}",
            "a removal of one header", request.headers
        );
        assert_eq!(
            request.raw_query_string, "page=2&drop=yes",
            "case {:?}: expected the client's own query string, got {:?}",
            "a mapping naming no query parameter", request.raw_query_string
        );
        assert_eq!(
            path, None,
            "case {:?}: expected the backend addressed where the integration names it, got \
             {path:?}",
            "a mapping naming no path"
        );
    }

    #[test]
    fn a_value_resolves_through_the_one_reading_of_the_mapping_language() {
        // Rows differ along the SOURCE a value names: each documented part of the request, a
        // static value, a context variable, a stage variable, and an expression this call has
        // no value for — which maps nothing, so the header stays absent.
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("a static value", "injected", Some("injected")),
            (
                "a header of the request",
                "$request.header.x-client",
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
            (
                "an expression naming the answer, which no request leg has",
                "$response.header.x-from-backend",
                None,
            ),
        ];
        for (case, value, expected) in cases {
            let (request, _) = mapped(&[("overwrite:header.x-mapped", value)]);
            let held = field(&request.headers, "x-mapped");
            assert_eq!(
                held.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {held:?}"
            );
        }
    }

    #[test]
    fn a_key_the_language_does_not_define_maps_nothing() {
        // Rows differ along the KEY: an action that is none of the three, the two actions the
        // path carries none of, an operand naming no part of the request, an operand with no
        // name behind it, and a key that is no mapping expression at all. The CONTROL is the
        // same mapping written the way the grammar defines it.
        let cases: &[(&str, &str)] = &[
            ("an action that is not one of the three", "rewrite:header.x-mapped"),
            ("an append of the path", "append:path"),
            ("a remove of the path", "remove:path"),
            ("an operand naming the response's status", "overwrite:statuscode"),
            ("an operand with no name behind it", "overwrite:header."),
            ("a key that is no mapping expression", "x-mapped"),
        ];
        let arrived = request();
        for (case, key) in cases {
            let (request, path) = mapped(&[(key, "mapped")]);
            assert_eq!(
                request.headers, arrived.headers,
                "case {case:?}: expected the client's own headers, got {:?}",
                request.headers
            );
            assert_eq!(
                request.raw_path, arrived.raw_path,
                "case {case:?}: expected the client's own path, got {:?}",
                request.raw_path
            );
            assert_eq!(
                path, None,
                "case {case:?}: expected no path overwritten, got {path:?}"
            );
        }
        let (control, _) = mapped(&[("overwrite:header.x-mapped", "mapped")]);
        assert_eq!(
            field(&control.headers, "x-mapped").as_deref(),
            Some("mapped"),
            "case {:?}: expected the mapping applied, got {:?}",
            "the same mapping written as the grammar defines it", control.headers
        );
    }

    #[test]
    fn a_mapped_query_value_is_one_parameter_however_it_is_written() {
        // Rows differ along what the mapped value carries: the separators of the grammar
        // itself, a space, a non-ASCII letter, and a plain value — each of which must reach
        // the backend as ONE parameter holding exactly that value.
        let cases: &[(&str, &str)] = &[
            ("a plain value", "plain"),
            ("a value carrying the pair separator", "a&b=c"),
            ("a value carrying a space", "two words"),
            ("a value carrying a non-ASCII letter", "caffè"),
        ];
        for (case, value) in cases {
            let (request, _) = mapped(&[("append:querystring.injected", value)]);
            let held = field(&request.query_parameters, "injected");
            assert_eq!(
                held.as_deref(),
                Some(*value),
                "case {case:?}: expected the parameter to hold {value:?}, got {held:?}"
            );
            // And the raw query string the hop carries decodes back to the same one value.
            let decoded: Vec<(String, String)> =
                ::aws_smithy_schema_server::query::parse_form_urlencoded(
                    request.raw_query_string.as_bytes(),
                )
                .into_iter()
                .filter(|(name, _)| name == "injected")
                .collect();
            assert_eq!(
                decoded,
                vec![("injected".to_string(), (*value).to_string())],
                "case {case:?}: expected one parameter in {:?}",
                request.raw_query_string
            );
        }
    }

    /// RULE: what the transform ADDS to one request is bounded, however many entries the
    /// mapping holds — the entries up to the bound are applied and the rest are not, so
    /// neither the work nor the memory one request costs is sized by what was stored.
    #[test]
    fn the_bytes_one_request_carries_bound_what_the_mapping_writes() {
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
            let rows: Vec<(&str, &str)> = entries
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let (request, _) = mapped(&rows);
            let mapped_headers: Vec<(&String, &Vec<String>)> = request
                .headers
                .iter()
                .filter(|(name, _)| name.starts_with("x-mapped-"))
                .collect();
            let written: usize = mapped_headers
                .iter()
                .map(|(name, held)| name.len() + held.iter().map(String::len).sum::<usize>())
                .sum();
            assert!(
                written <= MAX_MAPPED_BYTES,
                "case {case:?}: expected at most {MAX_MAPPED_BYTES} mapped bytes, got {written}"
            );
            assert_eq!(
                mapped_headers.len() == entries.len(),
                all_applied,
                "case {case:?}: expected {} of {} entries applied, got {}",
                if all_applied { "all" } else { "not all" },
                entries.len(),
                mapped_headers.len()
            );
            assert!(
                !mapped_headers.is_empty(),
                "case {case:?}: expected the entries up to the bound to be applied, got none"
            );
        }
    }
}
