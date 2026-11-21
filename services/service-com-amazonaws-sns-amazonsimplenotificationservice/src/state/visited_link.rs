// SPDX-License-Identifier: CC0-1.0

//! The LINKS this service hands a subscriber, and the request following one is read as.
//!
//! Two documents SNS delivers to an endpoint carry a URL for its owner to FOLLOW rather than
//! an action for a client to call: "To confirm the subscription, visit the SubscribeURL
//! included in this message" (the sentence a `SubscriptionConfirmation` states), and the
//! `UnsubscribeURL` a delivered notification carries, which a final cancellation's own
//! `SubscribeURL` undoes. Each is an AWS Query API call written as a URL — the action and
//! its parameters in the QUERY STRING of a `GET` — because that is what a link can be:
//! "you can send query requests by HTTP GET as well as by POST" is the whole reason those
//! members are URLs at all.
//!
//! The framework resolves an awsQuery request's operation from the `Action` field of its
//! form-encoded BODY, which a `GET` has none of, so a visit to such a link names no
//! operation and would be answered as a request for nothing. This module is the one reading
//! of that visit: which of this service's links a request follows
//! ([`visits_a_link`]), and the request it is the same call as
//! ([`as_form_request`]) — the POST of the same action and the same parameters, which the
//! service then serves through exactly the path every other call of this API takes.
//!
//! Only the two actions this service hands OUT as links are read this way
//! ([`LINKED_ACTIONS`]): a link is served because this service minted it, so nothing else
//! becomes reachable by `GET`.

/// The actions this service hands a subscriber as a URL to follow: the one a
/// `SubscribeURL` presents (`crate::ops::publish::endpoint_delivery`) and the one an
/// `UnsubscribeURL` presents (`crate::ops::publish`).
pub const LINKED_ACTIONS: &[&str] = &["ConfirmSubscription", "Unsubscribe"];

/// The member every AWS Query API request names its operation in.
const ACTION_FIELD: &str = "Action";

/// The method a link is followed with, which is the only method read here: a client CALLING
/// this API posts, and a request that posts carries its action in the body already.
const VISIT_METHOD: &str = "GET";

/// The media type an AWS Query API request carries its form-encoded parameters in.
const FORM_MEDIA_TYPE: &str = ::service_common::query_wire_form::QUERY_CONTENT_TYPE;

/// Whether a request of `method` to `uri` FOLLOWS one of this service's links: a `GET` of
/// the root path whose query names one of [`LINKED_ACTIONS`].
///
/// Read from the address alone, so a request that follows no link of this service — every
/// call of this API, and every other service's traffic, which this runs for on the way to
/// `can_handle`'s answer — is answered before anything else is done for it.
#[must_use]
pub fn visits_a_link(method: &str, uri: &str) -> bool {
    linked_action(method, uri).is_some()
}

/// Whether a request of `method` to `uri` follows one of this service's links AND presents
/// no credential of any kind, `authorization` being its `Authorization` header.
///
/// The ONE reading of the request this service serves as a visit: the address names a link
/// this service minted, and the caller is the one such a link exists for — the endpoint's
/// owner, which holds no Amazon Web Services credential. A visit that PRESENTS one (in the
/// header, or in a presigned query, both read through
/// [`::service_common::sigv4_presentation::presents_a_credential`]) is a client calling this
/// API, and is claimed, verified and served as every other call of it is.
#[must_use]
pub fn visits_a_link_with_no_credential(
    method: &str,
    uri: &str,
    authorization: Option<&str>,
) -> bool {
    if !visits_a_link(method, uri) {
        return false;
    }
    let query = uri.split_once('?').map(|(_path, query)| query);
    !::service_common::sigv4_presentation::presents_a_credential(authorization, query)
}

/// The action the link `uri` presents, when a request of `method` to it follows one of this
/// service's links, and `None` otherwise.
fn linked_action(method: &str, uri: &str) -> Option<&'static str> {
    if !method.eq_ignore_ascii_case(VISIT_METHOD) {
        return None;
    }
    let (path, query) = uri.split_once('?')?;
    // The root path, whichever address the link named: the authority is the endpoint, and an
    // AWS Query API call addresses the root of it.
    let path = path.split_once("://").map_or(path, |(_scheme, rest)| {
        rest.split_once('/').map_or("", |(_authority, rest)| rest)
    });
    if !path.trim_matches('/').is_empty() {
        return None;
    }
    let asked = ::service_common::query_parameter(query, ACTION_FIELD)?;
    LINKED_ACTIONS
        .iter()
        .copied()
        .find(|action| *action == asked)
}

/// The request a visit to one of this service's links IS: the POST of the same action with
/// the same parameters, which is the form every other call of this API arrives in.
///
/// The parameters need no rewriting — a query string and an
/// `application/x-www-form-urlencoded` body are the same encoding of the same pairs, which
/// is why one call can be written either way — so the query becomes the body pair by pair,
/// and nothing of the link's own values is parsed here.
///
/// What is left OUT is SigV4's own presentation ([`SIGNING_PARAMETER_PREFIX`]): where a
/// credential travels is not part of the call, so the parameters that carry one are not
/// members of the request this rebuilds. No member of this API is spelled that way, so
/// nothing a link states is dropped with them.
///
/// `None` when the request follows no link of this service, and when the rebuilt request
/// could not be formed at all, so a visit that cannot be served is answered rather than
/// panicking on the request path.
#[must_use]
pub fn as_form_request(
    req: &::aws_smithy_runtime_api::http::Request,
) -> Option<::aws_smithy_runtime_api::http::Request> {
    linked_action(req.method(), req.uri())?;
    let (_path, query) = req.uri().split_once('?')?;
    let body = form_body_of(query);
    let rebuilt = ::http::Request::builder()
        .method(::http::Method::POST)
        .uri("/")
        .header(::http::header::CONTENT_TYPE, FORM_MEDIA_TYPE)
        .body(::aws_smithy_types::body::SdkBody::from(body.into_bytes()))
        .ok()?;
    ::aws_smithy_runtime_api::http::Request::try_from(rebuilt).ok()
}

/// The prefix every parameter of SigV4's query presentation carries (`X-Amz-Credential`,
/// `X-Amz-Signature`, …), which is how those pairs are told from the call's own members.
///
/// No visit this service serves carries one — a request presenting a credential is not read
/// as a visit at all ([`visits_a_link_with_no_credential`]) — and leaving them out here is
/// what keeps that true of the REBUILT request whichever caller reaches this.
const SIGNING_PARAMETER_PREFIX: &str = "X-Amz-";

/// The form body `query` states: its pairs in the order they were written, without the ones
/// that carry SigV4's own presentation.
fn form_body_of(query: &str) -> String {
    let carried: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(name, _value)| name);
            !name
                .get(..SIGNING_PARAMETER_PREFIX.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SIGNING_PARAMETER_PREFIX))
        })
        .collect();
    carried.join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a request follows one of this service's links when it is a `GET` of the root
    /// path whose query names one of the actions this service hands out as a URL. The rows
    /// differ along the form of the address: both links, each spelling of the address a link
    /// may carry, another action, another path, and the POST every client call arrives as.
    #[test]
    fn only_a_visit_to_one_of_this_services_links_is_read_as_one() {
        // (case, method, uri, whether it follows a link)
        let rows: &[(&str, &str, &str, bool)] = &[
            (
                "the SubscribeURL of a confirmation request",
                "GET",
                "https://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&TopicArn=arn:aws:sns:us-east-1:000000000000:t&Token=abc",
                true,
            ),
            (
                "the UnsubscribeURL of a delivered notification",
                "GET",
                "https://sns.us-east-1.amazonaws.com/?Action=Unsubscribe&SubscriptionArn=arn:aws:sns:us-east-1:000000000000:t:0123",
                true,
            ),
            (
                "the same link followed at the emulator's own endpoint",
                "GET",
                "/?Action=Unsubscribe&SubscriptionArn=arn:aws:sns:us-east-1:000000000000:t:0123",
                true,
            ),
            (
                "an action this service hands out no link for",
                "GET",
                "https://sns.us-east-1.amazonaws.com/?Action=DeleteTopic&TopicArn=arn:aws:sns:us-east-1:000000000000:t",
                false,
            ),
            (
                "a path that is no AWS Query API address",
                "GET",
                "/SimpleNotificationService-0123.pem?Action=ConfirmSubscription",
                false,
            ),
            (
                "a request carrying no query at all",
                "GET",
                "/",
                false,
            ),
            (
                "the POST every client call of this API arrives as",
                "POST",
                "/?Action=ConfirmSubscription",
                false,
            ),
        ];
        for (case, method, uri, expected) in rows {
            let read = visits_a_link(method, uri);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?} for {uri:?}, got {read:?}"
            );
        }
    }

    /// RULE: the visit this service SERVES as one presents no credential — the caller such a
    /// link exists for — while one presenting a credential is a client calling this API. The
    /// rows differ along where a credential travels: nowhere, the `Authorization` header, and
    /// a presigned query.
    #[test]
    fn only_a_visit_presenting_no_credential_is_served_as_one() {
        let called = "/?Action=Unsubscribe&SubscriptionArn=arn:aws:sns:us-east-1:000000000000:t:1";
        // (case, the `Authorization` header, the uri, whether it is served as a visit)
        let rows: &[(&str, Option<&str>, String, bool)] = &[
            (
                "the link as its endpoint's owner follows it",
                None,
                called.to_string(),
                true,
            ),
            (
                "the same address with a signature",
                Some("AWS4-HMAC-SHA256 Credential=AKID/20260101/us-east-1/sns/aws4_request"),
                called.to_string(),
                false,
            ),
            (
                "the same address presigned",
                None,
                format!(
                    "{called}&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKID%2F20260101\
                     &X-Amz-Signature=0000"
                ),
                false,
            ),
        ];
        for (case, authorization, uri, expected) in rows {
            let read = visits_a_link_with_no_credential("GET", uri, *authorization);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?} for {uri:?}, got {read:?}"
            );
        }
    }

    /// RULE: the request a visit IS carries the link's own parameters as the form body of a
    /// POST of the root path, so the call the link writes and the call a client posts are one
    /// request — and SigV4's own presentation, which is not part of the call, is left out of
    /// it. The rows differ along what the query carries: the link's members alone, and the
    /// same link with a presigned credential's parameters beside them.
    #[test]
    fn a_visit_is_the_post_of_the_same_call() {
        let called = "Action=ConfirmSubscription&TopicArn=arn:aws:sns:us-east-1:000000000000:t&\
                      Token=abc";
        // (case, the link's query, the form body the rebuilt POST carries)
        let rows: &[(&str, String, &str)] = &[
            (
                "the link as this service minted it",
                called.to_string(),
                called,
            ),
            (
                "the same link presented with a presigned credential",
                format!(
                    "{called}&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKID%2F20260101\
                     &x-amz-signature=0000"
                ),
                called,
            ),
        ];
        for (case, query, expected) in rows {
            let visit = ::http::Request::builder()
                .method(::http::Method::GET)
                .uri(format!("https://sns.us-east-1.amazonaws.com/?{query}"))
                .body(::aws_smithy_types::body::SdkBody::empty())
                .expect("a well-formed visit");
            let visit = ::aws_smithy_runtime_api::http::Request::try_from(visit)
                .expect("a runtime-API request");
            let rebuilt =
                as_form_request(&visit).expect("the visit is one of this service's links");
            let body = rebuilt
                .body()
                .bytes()
                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                .unwrap_or_default();
            assert_eq!(
                (rebuilt.method(), rebuilt.uri(), body.as_str()),
                ("POST", "/", *expected),
                "case {case:?}: expected the POST of the same call, got {rebuilt:?} with \
                 {body:?}"
            );
            assert_eq!(
                rebuilt
                    .headers()
                    .get(::http::header::CONTENT_TYPE.as_str()),
                Some(FORM_MEDIA_TYPE),
                "case {case:?}: the rebuilt request declares the form body it carries"
            );
        }
    }
}
