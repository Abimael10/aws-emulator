// SPDX-License-Identifier: CC0-1.0

//! An `Action` naming no operation of this service's model, over every way a Query call
//! can state one.
//!
//! RULE: an AWS Query call states the operation it is for in its `Action` field and the
//! API version that operation is asked for in its `Version` field, and the service
//! resolves the PAIR from wherever the request carries its call — the form-encoded body,
//! or the query string where SigV4's query-parameter scheme put the whole call there. A
//! pair it cannot resolve is refused HTTP 400 with the awsQuery error document whose
//! `(Type, Code, Message)` is `('Sender', 'InvalidAction', 'Could not find operation
//! <Action> for version <Version>')`, the two variable parts being the request's own
//! values; a request that carries NO `Action` at all is a different diagnostic, because
//! there is no action to report as not found.
//!
//! The rows differ along what the rule generalises over: WHERE the call is carried (a
//! form body, a query string, both), which action the call names, which version it names,
//! whether it names one at all, and how the value is spelled on the wire.

use super::malformed_session_policy::decode_xml_text;
use super::sts_sessions::{call, element, emulator, member, STS_API_VERSION, STS_SIGNING_NAME};
use smithy_server_runtime::MultiServiceRouter;

/// The error code AWS reserves for an action/version pair it cannot resolve.
const INVALID_ACTION: &str = "InvalidAction";

/// The fault class a Query error names when the CALLER stated the action.
const SENDER: &str = "Sender";

/// One call made the way a client fetches a URL: a `GET` of the root path whose whole
/// call is in the query string, with no body and no credential — the shape an AppSync HTTP
/// data source, a presigned URL and `curl` all send.
async fn get_call(router: &MultiServiceRouter, query: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://{STS_SIGNING_NAME}.amazonaws.com/?{query}"
        ))
        .header("host", format!("{STS_SIGNING_NAME}.amazonaws.com"))
        .body(bytes::Bytes::new())
        .expect("a well-formed query-string call");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One call made as an SDK makes it: a `POST` of a form-encoded body, with `body` as that
/// body verbatim so a row can state a call that carries no `Action`.
async fn post_body(router: &MultiServiceRouter, body: &str) -> (u16, String) {
    post_body_at(router, body, "").await
}

/// [`post_body`] with `query` as the request's query string, so a row can put one call in
/// the body and another's fields beside it.
async fn post_body_at(router: &MultiServiceRouter, body: &str, query: &str) -> (u16, String) {
    let uri = if query.is_empty() {
        format!("https://{STS_SIGNING_NAME}.amazonaws.com/")
    } else {
        format!("https://{STS_SIGNING_NAME}.amazonaws.com/?{query}")
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("{STS_SIGNING_NAME}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The `(Type, Code, Message)` triple a client reads an awsQuery refusal as.
fn error_triple(case: &str, body: &str) -> (String, String, String) {
    (
        member(case, body, "Type"),
        member(case, body, "Code"),
        member(case, body, "Message"),
    )
}

/// RULE: an unresolvable pair is refused 400 `InvalidAction`, naming the action the
/// request asked for and the version it asked for it at — whichever way the call is
/// carried.
#[tokio::test]
async fn an_unresolvable_action_names_the_action_and_the_version_it_was_asked_for() {
    let (router, _engine) = emulator();

    // (case, how the call is carried, the message the refusal must state)
    enum Carried {
        /// The whole call in the query string of a `GET`.
        QueryString(&'static str),
        /// The call in a form-encoded `POST` body.
        FormBody(&'static str),
        /// The call in a form-encoded `POST` body, with another call's fields in the
        /// query string beside it.
        FormBodyBesideQueryString(&'static str, &'static str),
    }
    let rows: Vec<(&str, Carried, String)> = vec![
        (
            "a query string, the shape an HTTP client fetches",
            Carried::QueryString("Action=BogusAction&Version=2011-06-15"),
            "Could not find operation BogusAction for version 2011-06-15".to_string(),
        ),
        (
            "a form-encoded body, the shape an SDK posts",
            Carried::FormBody("Action=BogusAction&Version=2011-06-15"),
            "Could not find operation BogusAction for version 2011-06-15".to_string(),
        ),
        (
            "another action, so the name is read rather than fixed",
            Carried::QueryString("Action=NotAnStsAction&Version=2011-06-15"),
            "Could not find operation NotAnStsAction for version 2011-06-15".to_string(),
        ),
        (
            "an action of another AWS API",
            Carried::FormBody("Action=DescribeLoadBalancers&Version=2011-06-15"),
            "Could not find operation DescribeLoadBalancers for version 2011-06-15".to_string(),
        ),
        (
            "a version no model declares, so the version is read too",
            Carried::QueryString("Action=BogusAction&Version=2099-01-01"),
            "Could not find operation BogusAction for version 2099-01-01".to_string(),
        ),
        (
            "the fields in the other order",
            Carried::QueryString("Version=2011-06-15&Action=BogusAction"),
            "Could not find operation BogusAction for version 2011-06-15".to_string(),
        ),
        (
            // A call that states no version has stated nothing to disagree with, so the
            // action was not found in this front door's own API version.
            "no version at all",
            Carried::FormBody("Action=BogusAction"),
            format!("Could not find operation BogusAction for version {STS_API_VERSION}"),
        ),
        (
            // The value travels percent-encoded, so what is reported is what the client
            // spelled rather than the wire bytes.
            "an action spelled with an encoded character",
            Carried::QueryString("Action=Bogus%2DAction&Version=2011-06-15"),
            "Could not find operation Bogus-Action for version 2011-06-15".to_string(),
        ),
        (
            // The body is the call the signature covered, so a query string beside it
            // never out-ranks it.
            "a body-carried call, with another call's fields in the query string",
            Carried::FormBodyBesideQueryString(
                "Action=BogusAction&Version=2011-06-15",
                "Action=FromTheQuery&Version=2099-01-01",
            ),
            "Could not find operation BogusAction for version 2011-06-15".to_string(),
        ),
    ];

    for (case, carried, expected_message) in rows {
        let (status, body) = match carried {
            Carried::QueryString(query) => get_call(&router, query).await,
            Carried::FormBody(form) => post_body_at(&router, form, "").await,
            Carried::FormBodyBesideQueryString(form, query) => {
                post_body_at(&router, form, query).await
            }
        };
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        let stated = error_triple(case, &body);
        let expected = (
            SENDER.to_string(),
            INVALID_ACTION.to_string(),
            expected_message,
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?} for {body}"
        );
        assert!(
            element(&body, "RequestId").is_some(),
            "case {case:?}: the awsQuery error envelope must carry a RequestId, got {body}"
        );
    }
}

/// RULE: the missing-`Action` diagnostic is reserved for a call that truly carries none —
/// a request that DID state an action is never told the field is absent.
#[tokio::test]
async fn a_call_carrying_no_action_is_not_reported_as_an_action_that_was_not_found() {
    let (router, _engine) = emulator();

    // (case, the form-encoded body, whether the call states an action at all)
    let rows: &[(&str, &str, bool)] = &[
        ("a body stating only the version", "Version=2011-06-15", false),
        ("an empty body", "", false),
        ("a body of members with no action", "RoleArn=arn%3Aaws%3Aiam%3A%3A1%3Arole%2Fr", false),
        ("a body stating an action", "Action=BogusAction&Version=2011-06-15", true),
    ];

    for (case, body, states_an_action) in rows {
        let (status, answered) = post_body(&router, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {answered}"
        );
        assert_eq!(
            element(&answered, "Code").as_deref(),
            Some(INVALID_ACTION),
            "case {case:?}: expected the awsQuery {INVALID_ACTION} refusal, got {answered}"
        );
        let message = member(case, &answered, "Message");
        let names_an_unfound_operation = message.starts_with("Could not find operation");
        assert_eq!(
            names_an_unfound_operation, *states_an_action,
            "case {case:?}: expected an unfound-operation sentence only for a call that \
             states an action, got {message:?}"
        );
        if !states_an_action {
            assert!(
                message.contains("Action"),
                "case {case:?}: a call carrying no action must be told which field it \
                 omitted, got {message:?}"
            );
        }
    }
}

/// RULE: the action the refusal reports is the one the request spelled, and the envelope
/// stays a well-formed XML document whatever characters that spelling carries — the markup
/// characters arrive escaped as a text node escapes them, so a value a caller chose cannot
/// break out of the `<Message>` it is reported in.
#[tokio::test]
async fn an_action_carrying_markup_characters_is_reported_inside_the_envelope() {
    let (router, _engine) = emulator();

    // (case, the Action as the client percent-encoded it, the Action as the client spelled
    // it). The rows differ along which markup character the spelling carries: the two XML
    // requires escaped in a text node, the quote characters that are markup only inside an
    // attribute, and a whole element a value could try to inject.
    let rows: &[(&str, &str, &str)] = &[
        ("an action carrying a less-than", "A%3CB", "A<B"),
        ("an action carrying an ampersand", "A%26B", "A&B"),
        ("an action carrying both quote characters", "A%22B%27C", "A\"B'C"),
        (
            "an action spelling a whole element",
            "%3C%2FMessage%3E%3CCode%3EOops%3C%2FCode%3E",
            "</Message><Code>Oops</Code>",
        ),
    ];

    for (case, encoded, spelled) in rows {
        let (status, body) = get_call(
            &router,
            &format!("Action={encoded}&Version={STS_API_VERSION}"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        // The envelope still reports ONE code and ONE message: a spelling that tried to
        // close the element and open another did not become markup.
        let stated = (
            element(&body, "Type"),
            element(&body, "Code"),
            decode_xml_text(&member(case, &body, "Message")),
        );
        let expected = (
            Some(SENDER.to_string()),
            Some(INVALID_ACTION.to_string()),
            format!("Could not find operation {spelled} for version {STS_API_VERSION}"),
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?} for {body}"
        );
        // And the markup characters XML requires escaped in a text node are escaped on the
        // wire rather than written as themselves.
        let raw = member(case, &body, "Message");
        assert!(
            !raw.contains('<'),
            "case {case:?}: a text node must escape '<', got {raw:?}"
        );
        assert!(
            raw.split('&').skip(1).all(|after| {
                ["amp;", "lt;", "gt;", "quot;", "apos;", "#"]
                    .iter()
                    .any(|reference| after.starts_with(reference))
            }),
            "case {case:?}: every '&' of a text node must begin a reference, got {raw:?}"
        );
    }
}

/// RULE, the other side of it: an action the model DOES declare is still served, from the
/// query string as well as from the body — the refusal above reaches only a pair that does
/// not resolve.
#[tokio::test]
async fn an_action_the_model_declares_is_served_from_either_carrier() {
    let (router, _engine) = emulator();

    // The credential scope the query-parameter signing scheme carries, so the two
    // carriers speak for the SAME credential and their answers are comparable.
    let credential = format!(
        "{}%2F20260101%2Fus-east-1%2F{STS_SIGNING_NAME}%2Faws4_request",
        super::sts_sessions::LONG_TERM_KEY
    );
    let (status, from_query) = get_call(
        &router,
        &format!(
            "Action=GetCallerIdentity&Version={STS_API_VERSION}\
             &X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={credential}\
             &X-Amz-Date=20260101T000000Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=0123456789abcdef"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected a query-string GetCallerIdentity to be served, got {status} {from_query}"
    );
    let (status, from_body) = call(
        &router,
        super::sts_sessions::LONG_TERM_KEY,
        "GetCallerIdentity",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected a body-carried GetCallerIdentity to be served, got {status} {from_body}"
    );
    assert_eq!(
        element(&from_query, "Account"),
        element(&from_body, "Account"),
        "expected the two carriers to report one identity, got {from_query} and {from_body}"
    );
}
