// SPDX-License-Identifier: CC0-1.0

//! What an `HTTP_PROXY` route of a deployed HTTP api stage ANSWERS: the message the gateway
//! FORWARDS to the upstream its integration resolved, and the answer that sends the caller
//! there for an upstream this emulator serves nothing at.
//!
//! `Integration$IntegrationType` gives `HTTP_PROXY` as "for integrating the route or method
//! request with an HTTP endpoint, with the client request passed through as-is", so a route
//! of this type is a HOP: the gateway builds a request of its own at that endpoint, carrying
//! the client's method, body, query string and headers, and the endpoint's answer is what the
//! client reads.
//!
//! # The hop-identifying fields are the GATEWAY's, never the client's
//!
//! A forwarded message says which intermediary forwarded it, and that is a statement about
//! THIS hop rather than anything the client may claim about earlier ones:
//!
//! * `Via` is the gateway's own ([`GATEWAY_VIA`]), REPLACING whatever the client sent — "a
//!   proxy MUST send an appropriate Via header field … in each message that it forwards"
//!   (RFC 9110 §7.6.3), and the field names the hop that is being made;
//! * the client's `X-Forwarded-For` is NOT forwarded. A client can write any chain it likes
//!   into it, so a value it sent says nothing about the connection this gateway read the
//!   request from; the address a receiver reports is the one that receiver itself observed
//!   (`super::backend_headers`, which states the peer of the connection it accepted). So a
//!   backend behind the hop never reads a chain the client composed.
//!
//! Both are the difference between a backend that can tell which peer called it and one a
//! caller can lie to. Everything else the client sent travels on, which is what "passed
//! through as-is" means — except the fields [`HEADERS_NOT_FORWARDED`] withholds, whose values
//! belong to the connection the gateway terminated rather than to the one it opens, and except
//! what the integration's own `RequestParameters` transformed before the message was built
//! (`super::request_parameters`), which is the api's own configuration of this hop.
//!
//! The REQUEST a Lambda proxy integration makes is not such a hop: the function is handed an
//! EVENT rather than an HTTP message ([`super::proxy_event`]), so the client's own fields
//! travel into it as they arrived.
//!
//! # Which upstream is reached, and which caller is sent there
//!
//! The address is the integration's own `IntegrationUri` with the serving stage's variables
//! interpolated into it (`super::dispatch`), so a reference the stage carries no variable for
//! names no backend at all.
//!
//! A hop is performed only where THIS api plane itself serves that address
//! (`super::serves_this_plane`): the request is handed back to this plane's own serving
//! ([`super::served_hop`]) with the hop fields above written onto it, which opens no
//! connection, resolves no name and dials no address a stored configuration named. Every
//! OTHER upstream is a URI the CALLER can reach — including, as the member's own documentation
//! says, "a third party API" — and this emulator makes no request of its own to one: a
//! destination an api's configuration names is chosen by whoever wrote that configuration
//! rather than by this process, and the caller's own credential must not travel to one. The
//! caller is sent there instead ([`SENT_TO_THE_UPSTREAM`] with the resolved upstream in
//! [`LOCATION_HEADER`], the redirect that PRESERVES the request's method and body, RFC 7231
//! §6.4.7), so the client makes exactly the request the gateway would have forwarded and
//! nothing about that endpoint's answer — its status, its headers, its body — is invented
//! here.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

use super::super::data_plane::{json_response, UNPLACEABLE_STATUS};
use super::proxy_event::{self, ProxyRequest};
use super::INTERNAL_MESSAGE;
use crate::state::Integration;

/// The status a caller is sent to an upstream with: the redirect that preserves the request's
/// method and body (RFC 7231 §6.4.7), so the client repeats the request the gateway would
/// have forwarded.
const SENT_TO_THE_UPSTREAM: u16 = 307;

/// The header that redirect carries the upstream in (RFC 7231 §7.1.2).
const LOCATION_HEADER: &str = "location";

/// The header that frames a response with no content of its own (RFC 9110 §8.6).
const CONTENT_LENGTH_HEADER: &str = "content-length";

/// The field a forwarded message names its intermediaries in (RFC 9110 §7.6.3).
const VIA_HEADER: &str = "via";

/// The one value API Gateway's own hop writes into [`VIA_HEADER`]: the protocol of the hop and
/// the name of the intermediary making it, fixed text that interpolates nothing about the
/// request, the api or the stage.
const GATEWAY_VIA: &str = "HTTP/1.1 AmazonAPIGateway";

/// The field whose value the hop does not forward, because what it states belongs to the
/// connection a receiver accepted rather than to anything a client may claim (see the module
/// docs).
const FORWARDED_FOR_HEADER: &str = proxy_event::FORWARDED_FOR_HEADER;

/// The verb that names no method of its own: `ANY` is the stand-in for every one of them, on
/// an integration exactly as on a route key, so an integration naming it forwards the
/// caller's own method.
const ANY_METHOD: &str = "ANY";

/// The headers a forwarded request does NOT carry over from the client's.
///
/// `host` addresses the endpoint, and the endpoint forwarded to is the one the integration's
/// `IntegrationUri` names rather than the one the client addressed; `content-length` frames
/// the body as this hop sends it. [`FORWARDED_FOR_HEADER`] is withheld for the reason the
/// module docs give.
///
/// The SigV4 authentication fields are withheld too, and that is the substantive one: a
/// caller's signature is computed over the endpoint, the path and the headers of the request
/// the CALLER made, so at the endpoint the request is forwarded to it authenticates nothing.
/// Carrying it would make a forwarded request look credentialed to a route whose
/// `AuthorizationType` is `AWS_IAM` on the strength of a signature over somebody else's
/// address.
const HEADERS_NOT_FORWARDED: &[&str] = &[
    "host",
    "content-length",
    FORWARDED_FOR_HEADER,
    "authorization",
    "x-amz-security-token",
    "x-amz-date",
    "x-amz-content-sha256",
];

/// The query parameters a PRESIGNED SigV4 credential is carried in — the query-string form of
/// the authentication headers [`HEADERS_NOT_FORWARDED`] withholds, for the same reason, and
/// the set AWS documents a presigned request carries. Compared lowercased; every other
/// parameter the client sent travels on, whatever it is named.
const SIGNED_QUERY_PARAMETERS: &[&str] = &[
    "x-amz-algorithm",
    "x-amz-credential",
    "x-amz-date",
    "x-amz-expires",
    "x-amz-signedheaders",
    "x-amz-signature",
    "x-amz-security-token",
    "x-amz-content-sha256",
];

/// The message the gateway FORWARDS to `upstream` for this call, or `None` when the upstream
/// is not an address a request can be made at (so there is no hop to perform and no request
/// is built from guesswork).
///
/// It carries the client's body, the client's query string ([`forwarded_target`]) and every
/// header the client sent that [`HEADERS_NOT_FORWARDED`] does not withhold, with the hop
/// fields the module docs state written over them. Its verb is the integration's own —
/// "specifies the integration's HTTP method type" (`Integration$IntegrationMethod`), which for
/// this type is the backend's — and the client's where the integration names none, or names
/// the [`ANY_METHOD`] stand-in.
///
/// `request` is the request as the integration's own `RequestParameters` left it
/// (`super::request_parameters`), so a header, a query parameter or a path those mapped is
/// what the upstream reads; `path` is the one such an `overwrite:path` entry mapped, which
/// replaces the path the upstream's own URI names ([`addressed_at`]).
pub fn forwarded_request(
    request: &ProxyRequest,
    integration: &Integration,
    upstream: &str,
    path: Option<&str>,
) -> Option<RtRequest> {
    let host = ::service_common::data_plane_addressing::addressed_authority(upstream, None)?;
    let addressed = addressed_at(upstream, path);
    let mut forwarded = RtRequest::new(SdkBody::from(request.body.clone()));
    forwarded
        .set_uri(forwarded_target(&addressed, &request.raw_query_string))
        .ok()?;
    let method = integration
        .integration_method
        .as_deref()
        .map(str::trim)
        .filter(|declared| !declared.is_empty() && !declared.eq_ignore_ascii_case(ANY_METHOD))
        .unwrap_or(request.method.as_str());
    forwarded.set_method(method).ok()?;
    for (name, values) in &request.headers {
        if HEADERS_NOT_FORWARDED.contains(&name.as_str()) {
            continue;
        }
        for value in values {
            // `try_append`, not `insert`: a field the client sent more than once reaches the
            // upstream with every value it arrived with. Through the FALLIBLE form, because
            // these are bytes the client chose and one the HTTP layer cannot represent must
            // leave the field off rather than abort the hop.
            if let Err(refused) = forwarded
                .headers_mut()
                .try_append(name.clone(), value.clone())
            {
                ::tracing::debug!(
                    header = %name,
                    error = %refused,
                    "the client's header cannot be carried onward and is left off the request \
                     this integration forwards"
                );
            }
        }
    }
    // The hop's OWN fields, written after the client's so they REPLACE whatever it sent under
    // those names: the endpoint the request is addressed at, and the intermediary forwarding
    // it.
    forwarded
        .headers_mut()
        .try_insert("host", host.to_string())
        .ok()?;
    forwarded
        .headers_mut()
        .try_insert(VIA_HEADER, GATEWAY_VIA)
        .ok()?;
    Some(forwarded)
}

/// The address the hop is made at: the integration's resolved `upstream`, with its PATH
/// replaced by the one an `overwrite:path` entry of the integration's `RequestParameters`
/// mapped, for a mapping that states one.
///
/// Only the path is replaced: the endpoint — the scheme and the authority — is the api's own
/// configuration, which a mapping of the request does not move, and the upstream's own query
/// string travels on as it does for every other hop. The URL is read by the maintained parser
/// this crate already holds the caller-supplied URLs of this service to, rather than by a
/// second reading of RFC 3986 here; an upstream it cannot read is left exactly as stored,
/// which is the same answer as for an upstream naming no address a request is made at.
fn addressed_at(upstream: &str, path: Option<&str>) -> String {
    let Some(path) = path else {
        return upstream.to_string();
    };
    match ::url::Url::parse(upstream) {
        Ok(mut parsed) => {
            parsed.set_path(path);
            parsed.to_string()
        }
        Err(unreadable) => {
            ::tracing::debug!(
                error = %unreadable,
                "this route's HTTP_PROXY upstream is not a URL the mapped path can be written \
                 onto, so the hop is addressed where the integration names it"
            );
            upstream.to_string()
        }
    }
}

/// The request target the hop is made at: the resolved `upstream`, carrying the client's own
/// query string as well.
///
/// A proxy integration passes the request through and a request's query string is part of it,
/// so a backend that declares a query parameter sees the one the client sent. The upstream's
/// own pairs come FIRST — that is the api's configuration, which a client cannot displace —
/// and the client's follow; what does not travel is the credential a presigned client carries
/// in the query string ([`SIGNED_QUERY_PARAMETERS`]).
fn forwarded_target(upstream: &str, raw_query_string: &str) -> String {
    let sent: Vec<&str> = raw_query_string
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or(pair).to_ascii_lowercase();
            !SIGNED_QUERY_PARAMETERS.contains(&name.as_str())
        })
        .collect();
    if sent.is_empty() {
        return upstream.to_string();
    }
    let separator = if upstream.contains('?') { '&' } else { '?' };
    format!("{upstream}{separator}{}", sent.join("&"))
}

/// The answer for the route's resolved `upstream`: [`SENT_TO_THE_UPSTREAM`] to it, with no
/// body of its own.
///
/// A stored `IntegrationUri` that resolves to something no header can carry names no
/// reachable upstream, so there is nowhere to send the caller: that is an integration that
/// produced no response, the class every other unperformable integration of this plane falls
/// into.
pub fn sent_to_the_upstream(upstream: &str) -> RtResponse {
    let mut response = RtResponse::new(
        StatusCode::try_from(SENT_TO_THE_UPSTREAM).unwrap_or(
            StatusCode::try_from(UNPLACEABLE_STATUS).expect("500 is a valid status code"),
        ),
        SdkBody::empty(),
    );
    let headers = response.headers_mut();
    if headers
        .try_insert(LOCATION_HEADER, upstream.to_string())
        .is_err()
    {
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    }
    let _ = headers.try_insert(CONTENT_LENGTH_HEADER, "0");
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_is_sent_to_the_upstream_the_integration_resolved() {
        // The rows differ along the upstream the integration resolved to: the two schemes an
        // HTTP endpoint is named with, one carrying a query string of its own, and a value no
        // header can carry — which names no reachable upstream at all.
        let cases: [(&str, &str, u16, Option<&str>); 4] = [
            (
                "an https endpoint",
                "https://upstream.example.test/items",
                SENT_TO_THE_UPSTREAM,
                Some("https://upstream.example.test/items"),
            ),
            (
                "an http endpoint",
                "http://upstream.example.test/items",
                SENT_TO_THE_UPSTREAM,
                Some("http://upstream.example.test/items"),
            ),
            (
                "an endpoint carrying its own query string",
                "https://upstream.example.test/items?tenant=a",
                SENT_TO_THE_UPSTREAM,
                Some("https://upstream.example.test/items?tenant=a"),
            ),
            (
                "a value no header can carry",
                "https://upstream.example.test/\u{7f}items",
                UNPLACEABLE_STATUS,
                None,
            ),
        ];
        for (case, upstream, status, location) in cases {
            let answer = sent_to_the_upstream(upstream);
            assert_eq!(
                answer.status().as_u16(),
                status,
                "case {case:?}: expected {status}, got {}",
                answer.status().as_u16()
            );
            let actual = answer.headers().get(LOCATION_HEADER);
            assert_eq!(
                actual, location,
                "case {case:?}: expected {LOCATION_HEADER} {location:?}, got {actual:?}"
            );
        }
    }

    /// One client request to forward, with the fields a row varies.
    fn client_request(method: &str, query: &str, headers: &[(&str, &str)]) -> ProxyRequest {
        let mut fields = crate::service::data_plane::FieldValues::new();
        for (name, value) in headers {
            fields
                .entry((*name).to_string())
                .or_default()
                .push((*value).to_string());
        }
        ProxyRequest {
            method: method.to_string(),
            raw_path: "/front".to_string(),
            route_path: "/front".to_string(),
            route_segments: vec!["front".to_string()],
            raw_query_string: query.to_string(),
            headers: fields,
            query_parameters: crate::service::data_plane::FieldValues::new(),
            body: b"sent".to_vec(),
            route_key: "GET /front".to_string(),
            api_id: "front".to_string(),
            stage_name: "$default".to_string(),
            stage_variables: ::std::collections::BTreeMap::new(),
            region: "us-east-1".to_string(),
            domain_name: "front.execute-api.us-east-1.amazonaws.com".to_string(),
        }
    }

    /// An `HTTP_PROXY` integration whose `IntegrationMethod` is `method`.
    fn http_proxy(method: Option<&str>) -> Integration {
        Integration {
            integration_type: crate::state::HTTP_PROXY_INTEGRATION_TYPE.to_string(),
            integration_method: method.map(str::to_string),
            ..Integration::default()
        }
    }

    #[test]
    fn the_forwarded_message_states_this_hop_and_not_the_clients_claims() {
        // The rows differ along what the CLIENT sent — the hop fields it claimed (both, one,
        // neither), a credential of its own, a header sent twice, a query string — and along
        // the method the integration states, each of which the forwarded message answers the
        // same way: the gateway's own hop fields, and nothing of the client's that belongs to
        // the connection it terminated.
        struct Row {
            case: &'static str,
            method: &'static str,
            integration_method: Option<&'static str>,
            query: &'static str,
            headers: &'static [(&'static str, &'static str)],
            /// The verb the forwarded message must carry.
            forwarded_method: &'static str,
            /// The target it must be addressed at.
            target: &'static str,
            /// The headers it must NOT carry at all.
            withheld: &'static [&'static str],
        }
        let upstream = "https://origin.example.test/echo";
        let cases: [Row; 5] = [
            Row {
                case: "a client claiming a Via and a forwarded chain of its own",
                method: "GET",
                integration_method: Some("GET"),
                query: "",
                headers: &[
                    ("via", "2.0 client-claimed-hop, 1.1 another-claimed-hop"),
                    ("x-forwarded-for", "203.0.113.9, 198.51.100.7"),
                    ("x-tenant", "acme"),
                ],
                forwarded_method: "GET",
                target: "https://origin.example.test/echo",
                withheld: &["x-forwarded-for"],
            },
            Row {
                case: "a client claiming neither",
                method: "POST",
                integration_method: Some("POST"),
                query: "",
                headers: &[("content-type", "application/json")],
                forwarded_method: "POST",
                target: "https://origin.example.test/echo",
                withheld: &["x-forwarded-for"],
            },
            Row {
                case: "an integration naming the catch-all verb, so the client's travels",
                method: "DELETE",
                integration_method: Some("ANY"),
                query: "",
                headers: &[("via", "1.1 claimed")],
                forwarded_method: "DELETE",
                target: "https://origin.example.test/echo",
                withheld: &["x-forwarded-for"],
            },
            Row {
                case: "an integration naming no verb at all",
                method: "PATCH",
                integration_method: None,
                query: "page=2&tenant=a",
                headers: &[],
                forwarded_method: "PATCH",
                target: "https://origin.example.test/echo?page=2&tenant=a",
                withheld: &["x-forwarded-for"],
            },
            Row {
                case: "a client presenting a credential of its own",
                method: "GET",
                integration_method: Some("GET"),
                query: "X-Amz-Credential=AKID%2F20261115&page=3",
                headers: &[
                    ("authorization", "AWS4-HMAC-SHA256 Credential=AKID/x"),
                    ("x-amz-date", "20261115T000000Z"),
                ],
                forwarded_method: "GET",
                target: "https://origin.example.test/echo?page=3",
                withheld: &[
                    "authorization",
                    "x-amz-date",
                    "x-forwarded-for",
                    "content-length",
                ],
            },
        ];
        for row in cases {
            let case = row.case;
            let request = client_request(row.method, row.query, row.headers);
            let forwarded = forwarded_request(&request, &http_proxy(row.integration_method), upstream, None)
                .unwrap_or_else(|| panic!("case {case:?}: the upstream is one a request is made at"));
            assert_eq!(
                forwarded.method(),
                row.forwarded_method,
                "case {case:?}: expected the verb {:?}, got {:?}",
                row.forwarded_method,
                forwarded.method()
            );
            assert_eq!(
                forwarded.uri(),
                row.target,
                "case {case:?}: expected the target {:?}, got {:?}",
                row.target,
                forwarded.uri()
            );
            let via = forwarded.headers().get(VIA_HEADER);
            assert_eq!(
                via,
                Some(GATEWAY_VIA),
                "case {case:?}: expected the gateway's own Via {GATEWAY_VIA:?}, got {via:?}"
            );
            assert_eq!(
                forwarded.headers().get("host"),
                Some("origin.example.test"),
                "case {case:?}: expected the upstream's own host, got {:?}",
                forwarded.headers().get("host")
            );
            for withheld in row.withheld {
                let carried = forwarded.headers().get(*withheld);
                assert_eq!(
                    carried, None,
                    "case {case:?}: {withheld} belongs to the connection the gateway \
                     terminated and must not be forwarded, got {carried:?}"
                );
            }
            // Everything else the client sent travels on.
            for (name, value) in row.headers {
                if row.withheld.contains(name) {
                    continue;
                }
                let carried = forwarded.headers().get(*name);
                let expected = if *name == VIA_HEADER {
                    Some(GATEWAY_VIA)
                } else {
                    Some(*value)
                };
                assert_eq!(
                    carried, expected,
                    "case {case:?}: expected {name} to reach the upstream as {expected:?}, got \
                     {carried:?}"
                );
            }
            assert_eq!(
                forwarded.body().bytes(),
                Some(&b"sent"[..]),
                "case {case:?}: the client's own body is what the hop carries"
            );
        }
    }

    #[test]
    fn an_upstream_no_request_can_be_made_at_is_no_hop() {
        // The rows differ along why the stored upstream names no address a request is made
        // at: no authority at all, and a value no request target can carry.
        let cases: [(&str, &str); 2] = [
            ("an upstream naming no endpoint", "/items"),
            (
                "an upstream no request target can carry",
                "https://origin.example.test/\u{7f}items",
            ),
        ];
        for (case, upstream) in cases {
            let request = client_request("GET", "", &[]);
            let forwarded = forwarded_request(&request, &http_proxy(Some("GET")), upstream, None);
            assert!(
                forwarded.is_none(),
                "case {case:?}: expected no hop to be built, got one addressed at {:?}",
                forwarded.map(|built| built.uri().to_string())
            );
        }
    }

    /// RULE: an `overwrite:path` entry of the integration's `RequestParameters` replaces the
    /// PATH the hop is addressed at and nothing else about the endpoint — the scheme, the
    /// authority and the upstream's own query string are the api's configuration.
    #[test]
    fn a_mapped_path_replaces_the_path_the_hop_is_addressed_at() {
        // The rows differ along what the upstream carries (a path, a path and a query string,
        // no path at all) and along how the mapped path is written (with and without its
        // leading separator), against the control of a mapping that states none.
        let cases: [(&str, &str, Option<&str>, &str); 6] = [
            (
                "an upstream with a path, and no mapping",
                "https://origin.example.test/echo",
                None,
                "https://origin.example.test/echo",
            ),
            (
                "an upstream with a path, mapped to another",
                "https://origin.example.test/echo",
                Some("/elsewhere"),
                "https://origin.example.test/elsewhere",
            ),
            (
                "a mapped path written with no leading separator",
                "https://origin.example.test/echo",
                Some("elsewhere"),
                "https://origin.example.test/elsewhere",
            ),
            (
                "an upstream carrying its own query string",
                "https://origin.example.test/echo?tenant=a",
                Some("/elsewhere"),
                "https://origin.example.test/elsewhere?tenant=a",
            ),
            (
                "an upstream naming no path at all",
                "https://origin.example.test",
                Some("/elsewhere"),
                "https://origin.example.test/elsewhere",
            ),
            (
                "a nested mapped path",
                "https://origin.example.test/echo",
                Some("/orders/7/items"),
                "https://origin.example.test/orders/7/items",
            ),
        ];
        for (case, upstream, path, expected) in cases {
            let request = client_request("GET", "", &[]);
            let forwarded =
                forwarded_request(&request, &http_proxy(Some("GET")), upstream, path)
                    .unwrap_or_else(|| panic!("case {case:?}: the upstream is one a request is made at"));
            assert_eq!(
                forwarded.uri(),
                expected,
                "case {case:?}: expected the hop addressed at {expected:?}, got {:?}",
                forwarded.uri()
            );
        }
    }
}
