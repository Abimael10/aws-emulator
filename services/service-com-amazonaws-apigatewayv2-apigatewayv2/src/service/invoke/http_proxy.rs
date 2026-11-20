// SPDX-License-Identifier: CC0-1.0

//! What an `HTTP_PROXY` route of a deployed HTTP api stage ANSWERS: the upstream the
//! integration resolved, and the answer that sends the caller there.
//!
//! `Integration$IntegrationType` gives `HTTP_PROXY` as "for integrating the route or method
//! request with an HTTP endpoint, with the client request passed through as-is", so the
//! endpoint such a route fronts is a URI the CALLER can reach — including, as the member's
//! own documentation says, "a third party API".
//!
//! This emulator makes no request of its own to that endpoint. A destination an integration's
//! configuration names is chosen by whoever wrote the api's configuration rather than by this
//! process, so relaying a request to it would turn a stored string into a request this
//! emulator makes on a caller's behalf — and the caller's own credential must not travel to
//! one either. The caller is therefore sent there instead: [`SENT_TO_THE_UPSTREAM`] with the
//! resolved upstream in [`LOCATION_HEADER`], the redirect that PRESERVES the request's method
//! and body (RFC 7231 §6.4.7), so the client makes exactly the request the gateway would have
//! relayed and nothing about that endpoint's answer — its status, its headers, its body — is
//! invented here.
//!
//! The one thing the gateway still decides for such a route is the ADDRESS: the integration's
//! own `IntegrationUri` with the serving stage's variables interpolated into it
//! (`super::dispatch`), so a reference the stage carries no variable for names no backend at
//! all and is answered as an integration that produced no response rather than by sending the
//! caller to a URI that still carries the reference.

use ::aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

use super::super::data_plane::{json_response, UNPLACEABLE_STATUS};
use super::INTERNAL_MESSAGE;

/// The status a caller is sent to an upstream with: the redirect that preserves the request's
/// method and body (RFC 7231 §6.4.7), so the client repeats the request the gateway would
/// have relayed.
const SENT_TO_THE_UPSTREAM: u16 = 307;

/// The header that redirect carries the upstream in (RFC 7231 §7.1.2).
const LOCATION_HEADER: &str = "location";

/// The header that frames a response with no content of its own (RFC 9110 §8.6).
const CONTENT_LENGTH_HEADER: &str = "content-length";

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
}
