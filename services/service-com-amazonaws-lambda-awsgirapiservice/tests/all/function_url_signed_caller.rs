// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the caller an `AWS_IAM` FUNCTION URL ADMITS: one whose SigV4
//! signature the emulator VERIFIED.
//!
//! RULE: a function URL whose `AuthType` is `AWS_IAM` is a data-plane endpoint of this
//! service, so a request carrying a SigV4 signature computed — under this service's own
//! signing name, in the endpoint's Region — over the very request that arrived is served by
//! the function the URL is attached to, with the caller that signature speaks for reported in
//! the event's `requestContext.authorizer.iam`. A signature-calculation refusal of a request
//! that was signed correctly is never one of its answers; which requests ARE refused is the
//! other half of the rule, driven by `function_url_signed_caller_admission` over the table
//! declared here.
//!
//! Every row drives a real request through a router that VERIFIES signatures, as the emulator
//! does when it is started with request authentication on, against a function whose deployment
//! package RUNS — so a served row is served by the run. The rows differ along what the rule
//! generalises over: which of the three addressings reaches the endpoint (the authority the
//! URL was published on, the reserved local-endpoint path, and the url id as the first path
//! segment), and what the request carries (the endpoint's root, a nested path, a query, another
//! method with a body).
//!
//! The suite beside these two drives the same rule over a real connection to the published
//! port, where only that one addressing exists
//! (`function_url_published_address_signature`); these are about the credential and the
//! addressing, over every address the endpoint answers at.

use std::sync::Arc;

use serde_json::Value;

use crate::function_url_data_plane::{
    addressed, given_configured_url, given_function, grant, published_keys, router_on, send,
    signature_scoped_for, Addressing, Endpoint,
};
use crate::function_url_unauthenticated_refusal::borrowed;

/// The signing name the emulator verifies a request to this service under: a function URL is a
/// data-plane endpoint of Lambda, so its callers sign for `lambda`.
pub(crate) const SIGNING_NAME: &str = "lambda";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`), which is the
/// account a verified caller of this emulator belongs to.
pub(crate) const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The header the handler sets on every answer, so an answer of the RUN is distinguishable
/// from one the endpoint worded itself.
pub(crate) const BACKEND_HEADER: &str = "x-echo-backend";
/// See [`BACKEND_HEADER`].
pub(crate) const BACKEND_MARKER: &str = "fn-url";

/// The handler every function of these two suites runs: it echoes what the endpoint turned the
/// request into, so every assertion about a served row is about the RUN's own view of the
/// request.
pub(crate) fn echo_handler() -> String {
    r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json", "x-echo-backend": "fn-url"},
        "body": json.dumps(
            {
                "path": event["rawPath"],
                "rawQueryString": event.get("rawQueryString", ""),
                "method": event["requestContext"]["http"]["method"],
                "body": event.get("body") or "",
                "authorizer": event["requestContext"].get("authorizer"),
            }
        ),
    }
"#
    .to_string()
}

/// How a row is credentialled: one SIGNATURE the emulator really verifies, and the three ways
/// one does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// A signature computed over the request that is sent.
    OverThisRequest,
    /// No `Authorization` header at all.
    None,
    /// A signature computed over OTHER bytes than the ones sent: the body was changed after
    /// signing.
    OverOtherBytes,
    /// A signature whose credential scope names another service.
    ScopedElsewhere,
}

/// One request of either table: what it addresses, what it carries, and how it is
/// credentialled.
pub(crate) struct Row {
    pub(crate) case: &'static str,
    pub(crate) addressing: Addressing,
    pub(crate) method: &'static str,
    /// The path WITHIN the function URL the request reaches, which is the event's `rawPath`.
    pub(crate) within: &'static str,
    pub(crate) query: &'static str,
    /// The body the request is SENT with.
    pub(crate) body: &'static str,
    /// The media type it is sent under; empty for a request carrying no body.
    pub(crate) media_type: &'static str,
    pub(crate) credential: Credential,
}

impl Row {
    /// The host and request target this row reaches: the path within the URL under the
    /// addressing's own base, with the query it carries.
    pub(crate) fn target(&self, endpoint: &Endpoint) -> (String, String) {
        let within = if self.query.is_empty() {
            self.within.to_string()
        } else {
            format!("{within}?{query}", within = self.within, query = self.query)
        };
        addressed(endpoint, self.addressing, &within)
    }

    /// The headers the request carries of its own, which are signed with it.
    pub(crate) fn carried(&self) -> Vec<(&'static str, &'static str)> {
        if self.media_type.is_empty() {
            Vec::new()
        } else {
            vec![("content-type", self.media_type)]
        }
    }

    /// The bytes the signature was computed over, which are the ones sent except for the row
    /// whose body was changed after signing.
    fn signed_bytes(&self) -> &'static str {
        match self.credential {
            // The same length, other content: what a signature covers is the payload's hash,
            // so this is a signature over bytes that did not arrive.
            Credential::OverOtherBytes => r#"{"item":"ONE"}"#,
            _ => self.body,
        }
    }

    /// The headers this row is credentialled with.
    pub(crate) async fn credentialled(&self, host: &str, target: &str) -> Vec<(String, String)> {
        let carried = self.carried();
        if self.credential == Credential::None {
            return carried
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
        }
        let signing_name = match self.credential {
            Credential::ScopedElsewhere => "s3",
            _ => SIGNING_NAME,
        };
        signature_scoped_for(
            signing_name,
            host,
            self.method,
            target,
            self.signed_bytes().as_bytes(),
            &carried,
        )
        .await
    }
}

/// An `AWS_IAM` function URL on a function that RUNS [`echo_handler`], with the account's own
/// grant of the endpoint's action on it.
pub(crate) async fn given_restricted_url(
    router: &::smithy_server_runtime::MultiServiceRouter,
    function: &str,
) -> Endpoint {
    given_function(router, function, &echo_handler()).await;
    let endpoint = given_configured_url(router, function, None, "AWS_IAM", &Value::Null).await;
    grant(
        router,
        function,
        None,
        &::serde_json::json!({
            "StatementId": "invoke-function-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": ACCOUNT,
            "FunctionUrlAuthType": "AWS_IAM",
        }),
    )
    .await;
    endpoint
}

/// RULE: a correctly signed caller of an `AWS_IAM` function URL is served BY THE FUNCTION,
/// whichever addressing reached the endpoint and whatever the request carries, and is reported
/// to the function as the caller its credential speaks for.
#[tokio::test]
async fn a_correctly_signed_caller_of_an_aws_iam_function_url_is_served_by_the_function() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let endpoint = given_restricted_url(&router, "url-signed").await;
    // From here on the SAME emulator verifies request signatures, as it does when it is
    // started with request authentication on.
    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);

    let rows: &[Row] = &[
        Row {
            case: "the endpoint's own root, on the authority the URL was published on",
            addressing: Addressing::PublishedAuthority,
            method: "GET",
            within: "/",
            query: "",
            body: "",
            media_type: "",
            credential: Credential::OverThisRequest,
        },
        Row {
            case: "a nested path carrying a query, on the published authority",
            addressing: Addressing::PublishedAuthority,
            method: "GET",
            within: "/items/one",
            query: "page=2&nonce=abc",
            body: "",
            media_type: "",
            credential: Credential::OverThisRequest,
        },
        Row {
            case: "another method carrying a body, on the published authority",
            addressing: Addressing::PublishedAuthority,
            method: "POST",
            within: "/orders",
            query: "",
            body: r#"{"item":"one"}"#,
            media_type: "application/json",
            credential: Credential::OverThisRequest,
        },
        Row {
            case: "a nested path carrying a query, on the reserved local-endpoint path",
            addressing: Addressing::LocalEndpointPath,
            method: "GET",
            within: "/items/one",
            query: "via=local",
            body: "",
            media_type: "",
            credential: Credential::OverThisRequest,
        },
        Row {
            case: "another method carrying a body, on the reserved local-endpoint path",
            addressing: Addressing::LocalEndpointPath,
            method: "PUT",
            within: "/orders/two",
            query: "",
            body: "two",
            media_type: "text/plain",
            credential: Credential::OverThisRequest,
        },
        Row {
            case: "a nested path, path-style on the emulator's own endpoint",
            addressing: Addressing::PathStyle,
            method: "GET",
            within: "/items/one",
            query: "",
            body: "",
            media_type: "",
            credential: Credential::OverThisRequest,
        },
    ];

    let (key_id, _secret) = published_keys();
    for row in rows {
        let case = row.case;
        let (host, target) = row.target(&endpoint);
        let credentialled = row.credentialled(&host, &target).await;
        let answer = send(
            &authenticating,
            row.method,
            &host,
            &target,
            row.body.as_bytes(),
            &borrowed(&credentialled),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a correctly signed caller must be served by the function, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.header(BACKEND_HEADER).as_deref(),
            Some(BACKEND_MARKER),
            "case {case:?}: expected the function's own {BACKEND_HEADER}, got {}",
            answer.summary()
        );
        let echoed = answer.json();
        assert_eq!(
            echoed.get("path").and_then(Value::as_str),
            Some(row.within),
            "case {case:?}: expected the handler to be told the path {:?}, got {}",
            row.within,
            answer.text()
        );
        assert_eq!(
            echoed.get("rawQueryString").and_then(Value::as_str),
            Some(row.query),
            "case {case:?}: expected the handler to be told the query {:?}, got {}",
            row.query,
            answer.text()
        );
        assert_eq!(
            echoed.get("method").and_then(Value::as_str),
            Some(row.method),
            "case {case:?}: expected the handler to be told the method {:?}, got {}",
            row.method,
            answer.text()
        );
        assert_eq!(
            echoed.get("body").and_then(Value::as_str),
            Some(row.body),
            "case {case:?}: expected the handler to be told the body {:?}, got {}",
            row.body,
            answer.text()
        );
        // The caller the VERIFIED signature speaks for, which is what an `AWS_IAM` endpoint
        // reports to the function.
        let iam = echoed
            .get("authorizer")
            .and_then(|authorizer| authorizer.get("iam").cloned())
            .unwrap_or(Value::Null);
        assert_eq!(
            iam.get("accessKey").and_then(Value::as_str),
            Some(key_id.as_str()),
            "case {case:?}: expected the verified caller's own access key, got {iam}"
        );
        assert_eq!(
            iam.get("accountId").and_then(Value::as_str),
            Some(ACCOUNT),
            "case {case:?}: expected the caller's account, got {iam}"
        );
    }
}
