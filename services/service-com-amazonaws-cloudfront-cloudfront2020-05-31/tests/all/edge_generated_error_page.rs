// SPDX-License-Identifier: CC0-1.0

//! The PAGE CloudFront generates for a viewer request it answers itself, end to end over HTTP.
//!
//! RULE: an answer the edge generates — one no origin produced, reported under `X-Cache: Error
//! from cloudfront` — carries CloudFront's own error page, and that page is FIXED text apart
//! from three substituted values: the status in its heading, the detail line of the failure,
//! and the identifier of the request being answered (the same value the edge stamps on
//! `X-Amz-Cf-Id`). Every other line of it — the explanation of the refusal, the pointer to the
//! CloudFront documentation, the two line breaks between them, and the trailer naming
//! CloudFront as the generator of the page — is part of the answer, because a client, a log
//! scraper or an operator script recognises an edge-generated refusal by matching the page
//! around the request identifier.
//!
//! The rows differ along what the rule generalises over: WHICH refusal the viewer earned, and
//! so the status and the detail line the page carries — a request whose own framing the edge
//! refuses, a method the matched behaviour does not allow, a distribution that accepts no
//! requests, and an origin the edge could not reach — spanning both statuses a generated page
//! is served under here (`403` and `502`) and three different detail lines. The page is read as
//! a whole and compared to CloudFront's, with the request identifier taken from the answer's
//! own header rather than written into the expectation.
//!
//! The harness (the router and the `DistributionConfig` builder) is
//! [`crate::distribution_lifecycle`]'s; the origin a row needs an answer from is this service's
//! own control-plane host, as [`crate::edge_data_plane`] explains.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, API_VERSION, HOST,
};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose id is the one
/// `MANAGED_CACHE_POLICIES` publishes.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint, whose
/// `GET /2020-05-31/distribution` answers a `DistributionList`.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the served origin answers.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// An origin NOTHING in this emulator serves: the host shape of a Lambda function URL, which
/// no service of this emulator claims, so no origin response exists to fetch.
const UNSERVED_ORIGIN: &str = "b1c2d3e4f5a6b7c8.lambda-url.us-east-1.on.aws";

/// `X-Cache` for an answer CloudFront produced itself.
const ERROR: &str = "Error from cloudfront";

/// A member the served origin's own answer carries, so a page carrying it would be the
/// origin's bytes rather than CloudFront's page.
const ORIGIN_MEMBER: &str = "DistributionList";

/// The detail line CloudFront states for a request it refuses at the edge.
const BAD_REQUEST_DETAIL: &str = "Bad request.";

/// The detail line CloudFront states for a viewer method the matched cache behaviour does not
/// allow.
const METHOD_NOT_ALLOWED_DETAIL: &str = "This distribution is not configured to allow the \
     HTTP request method that was used for this request. The distribution supports only \
     cachable requests.";

/// The detail line CloudFront states when it could not reach the origin at all.
const NO_ORIGIN_DETAIL: &str = "CloudFront wasn't able to connect to the origin.";

/// The sentence CloudFront's generated page explains every refusal with.
const EXPLANATION: &str = "We can't connect to the server for this app or website at this \
     time. There might be too much traffic or a configuration error. Try again later, or \
     contact the app or website owner.";

/// The sentence CloudFront's generated page points the distribution's operator at the
/// documentation with.
const TROUBLESHOOTING: &str = "If you provide content to customers through CloudFront, you can \
     find steps to troubleshoot and help prevent this error by reviewing the CloudFront \
     documentation.";

/// CloudFront's own generated error page for `status`, the failure `detail` names and the
/// request `request_id` identifies.
///
/// Written out here as the edge-generated answer carries it, line by line: the page is a
/// data-plane response and so is in no Smithy shape, and these are the lines a client that
/// recognises a CloudFront refusal matches.
fn cloudfront_error_page(status: u16, detail: &str, request_id: &str) -> String {
    [
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \
         \"http://www.w3.org/TR/html4/loose.dtd\">"
            .to_string(),
        "<HTML><HEAD><META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; \
         charset=iso-8859-1\">"
            .to_string(),
        "<TITLE>ERROR: The request could not be satisfied</TITLE>".to_string(),
        "</HEAD><BODY>".to_string(),
        format!("<H1>{status} ERROR</H1>"),
        "<H2>The request could not be satisfied.</H2>".to_string(),
        "<HR noshade size=\"1px\">".to_string(),
        detail.to_string(),
        EXPLANATION.to_string(),
        "<BR clear=\"all\">".to_string(),
        TROUBLESHOOTING.to_string(),
        "<BR clear=\"all\">".to_string(),
        "<HR noshade size=\"1px\">".to_string(),
        "<PRE>".to_string(),
        "Generated by cloudfront (CloudFront)".to_string(),
        format!("Request ID: {request_id}"),
        "</PRE>".to_string(),
        "<ADDRESS>".to_string(),
        "</ADDRESS>".to_string(),
        "</BODY></HTML>".to_string(),
    ]
    .join("\n")
}

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    x_cache: String,
    request_id: String,
    body: String,
}

/// Send one VIEWER request — no AWS credential, addressed to `host` — carrying `body` as its
/// content, which is what a request whose framing the edge refuses states.
async fn view(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    body: &str,
) -> Viewed {
    let request = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .body(Bytes::copy_from_slice(body.as_bytes()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (x_cache, request_id) = (header("x-cache"), header("x-amz-cf-id"));
    Viewed {
        status,
        x_cache,
        request_id,
        body: String::from_utf8_lossy(&response.into_body()).to_string(),
    }
}

/// A distribution over `origin` whose default behaviour allows only `GET` and `HEAD`, enabled
/// as `enabled` says.
fn config(caller_reference: &str, origin: &str, enabled: bool) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>generated error page</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>{enabled}</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one distribution over `origin` and wait out the propagation its create owes it:
/// reports the domain name CloudFront assigned it, which is what a viewer addresses.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    origin: &str,
    enabled: bool,
) -> String {
    let created = create_distribution(router, &config(caller_reference, origin, enabled)).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .expect("the created distribution reports an id")
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .expect("the created distribution reports a domain name")
        .to_string();
    awaited_deployment(router, case, &id).await;
    domain
}

/// One row: the refusal a viewer earns, and the page CloudFront answers it with.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The viewer method.
    method: &'static str,
    /// The content the viewer request carries, which is empty for every row but the one about
    /// a request whose own framing the edge refuses.
    body: &'static str,
    /// Whether the distribution this row addresses accepts requests at all.
    enabled: bool,
    /// Whether the distribution this row addresses names an origin this emulator serves.
    served_origin: bool,
    /// The status CloudFront answers under.
    status: u16,
    /// The detail line the page carries for this refusal.
    detail: &'static str,
}

/// Every refusal the edge generates a page for over a distribution with no function, no
/// signature requirement and no geographic restriction: the rows differ along the status and
/// the detail line, which are what the page substitutes.
const ROWS: &[Row] = &[
    Row {
        case: "a GET carrying request content",
        method: "GET",
        body: "a viewer body a GET defines no meaning for",
        enabled: true,
        served_origin: true,
        status: 403,
        detail: BAD_REQUEST_DETAIL,
    },
    Row {
        case: "a HEAD carrying request content",
        method: "HEAD",
        body: "a viewer body a HEAD defines no meaning for",
        enabled: true,
        served_origin: true,
        status: 403,
        detail: BAD_REQUEST_DETAIL,
    },
    Row {
        case: "a method the behavior does not allow",
        method: "POST",
        body: "",
        enabled: true,
        served_origin: true,
        status: 403,
        detail: METHOD_NOT_ALLOWED_DETAIL,
    },
    Row {
        case: "a distribution that accepts no requests",
        method: "GET",
        body: "",
        enabled: false,
        served_origin: true,
        status: 403,
        detail: BAD_REQUEST_DETAIL,
    },
    Row {
        case: "an origin nothing serves",
        method: "GET",
        body: "",
        enabled: true,
        served_origin: false,
        status: 502,
        detail: NO_ORIGIN_DETAIL,
    },
];

/// RULE: every answer the edge generates itself carries CloudFront's whole page, with only the
/// status, the detail line of the failure and the request identifier substituted into it.
#[tokio::test]
async fn an_edge_generated_answer_carries_cloudfronts_whole_page() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so a row cannot read a distribution another one left behind.
        let router = emulator();
        let origin = if row.served_origin {
            SERVED_ORIGIN
        } else {
            UNSERVED_ORIGIN
        };
        let domain = deployed(
            &router,
            case,
            &format!("ref-generated-page-{case}"),
            origin,
            row.enabled,
        )
        .await;

        let answer = view(&router, row.method, &domain, SERVED_PATH, row.body).await;
        assert_eq!(
            (answer.status, answer.x_cache.as_str()),
            (row.status, ERROR),
            "case {case}: expected CloudFront's own {} under X-Cache {ERROR:?}, got {} with \
             {:?} (body {:?})",
            row.status,
            answer.status,
            answer.x_cache,
            answer.body
        );
        assert!(
            !answer.request_id.is_empty(),
            "case {case}: expected the edge to stamp an X-Amz-Cf-Id on the answer, got none \
             (body {:?})",
            answer.body
        );
        // A HEAD answer carries no content at all, so what is read of the page is the status
        // and the headers; every other row reads the page whole.
        if row.method == "HEAD" {
            continue;
        }
        let expected = cloudfront_error_page(row.status, row.detail, &answer.request_id);
        assert_eq!(
            answer.body, expected,
            "case {case}: expected CloudFront's generated page, got {:?}",
            answer.body
        );
        assert!(
            !answer.body.contains(ORIGIN_MEMBER),
            "case {case}: the page carries origin bytes, so the request reached the origin: \
             {:?}",
            answer.body
        );
    }
}

/// RULE, stated as the invariant it is: the page is FIXED. Two viewer requests that earn the
/// same refusal are answered with pages that differ in the request identifier and in nothing
/// else — so a client matching the page around that identifier recognises both.
#[tokio::test]
async fn two_refusals_of_one_kind_differ_only_in_the_request_identifier() {
    let router = emulator();
    let case = "repeated refusal";
    let domain = deployed(&router, case, "ref-generated-page-repeat", SERVED_ORIGIN, true).await;

    let first = view(&router, "POST", &domain, SERVED_PATH, "").await;
    let second = view(&router, "POST", &domain, SERVED_PATH, "").await;
    assert_ne!(
        first.request_id, second.request_id,
        "case {case}: expected a request identifier minted per request, got {:?} twice",
        first.request_id
    );
    assert_ne!(
        first.body, second.body,
        "case {case}: expected each page to carry its own request identifier, got the same \
         page twice: {:?}",
        first.body
    );
    let (first_scrubbed, second_scrubbed) = (
        first.body.replace(&first.request_id, "<request-id>"),
        second.body.replace(&second.request_id, "<request-id>"),
    );
    assert_eq!(
        first_scrubbed, second_scrubbed,
        "case {case}: expected the two pages to differ only in the request identifier, got \
         {first_scrubbed:?} and {second_scrubbed:?}"
    );
}
