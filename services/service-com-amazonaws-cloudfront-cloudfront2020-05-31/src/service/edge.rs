// SPDX-License-Identifier: CC0-1.0

//! The distribution DATA plane: how a viewer request addressed to a distribution's own
//! domain name is recognised, decided and answered.
//!
//! A distribution is reached at its `Distribution$DomainName` — "the domain name that
//! CloudFront assigns when you create a distribution, such as
//! `d111111abcdef8.cloudfront.net`" — or at one of the alternate domain names its
//! `Aliases` carry. That host is NOT the control plane's
//! (`cloudfront.amazonaws.com`): its first label is the distribution's own generated
//! label, which names no service, so the emulator's front door can resolve no target
//! service from such a request and answers it `404 UnknownService` — for the hostname
//! this very service told the caller to use. The one thing that is never correct is for
//! the host to be unknown to the service.
//!
//! So this module is the second half of [`crate::service::EmulatedService`]: the claim
//! ([`claims_parts`], read by `Service::can_handle`) and the serving ([`serve`], reached
//! from `Service::override_operation`). The decisions it makes are CloudFront's OWN — the
//! ones a viewer sees before any origin is involved — and every one of them is computed
//! from the distribution's stored configuration:
//!
//! 1. the host (or the path, see below) names a distribution this service HOLDS, whose
//!    configuration is `Enabled`; a disabled distribution serves nothing;
//! 2. the distribution's `Restrictions.GeoRestriction` decides whether the viewer's
//!    COUNTRY is served at all. A viewer a `blacklist` names, or one a `whitelist` leaves
//!    out, is refused BY CLOUDFRONT with `403`, before the cache is read and before any
//!    origin request exists ([`geo_restricts`]);
//! 3. the CACHE BEHAVIOUR is selected by path pattern: `CacheBehaviors$Items` in the
//!    order the configuration lists them, first match wins, and the
//!    `DefaultCacheBehavior` when none matches — "If none of the path patterns match,
//!    CloudFront applies the default cache behavior";
//! 4. that behaviour's `ViewerProtocolPolicy` decides whether the SCHEME the viewer
//!    dialled is served at all: `redirect-to-https` answers a plaintext viewer `301` with
//!    the HTTPS URL of the same request, `https-only` answers it `403`, and `allow-all`
//!    serves it ([`viewer_protocol_plan`]);
//! 5. that behaviour's `AllowedMethods` decide whether the viewer's METHOD is served at
//!    all. A method the behaviour does not allow is refused BY CLOUDFRONT with `403` and
//!    `X-Cache: Error from cloudfront`, and no origin request is made;
//! 6. that behaviour's `TrustedKeyGroups` decide whether the viewer is AUTHORIZED at all:
//!    "when a cache behavior contains trusted key groups, CloudFront requires signed URLs or
//!    signed cookies for all requests that match the cache behavior", so a request carrying
//!    no signature, one whose key is not in those groups, or one whose policy's conditions do
//!    not hold is refused BY CLOUDFRONT with `403` under its own error code, before the cache
//!    is read and with no origin request made ([`signed_access`]);
//! 7. that behaviour's `CachedMethods` and cache policy decide whether the answer may
//!    come from the edge cache: a cacheable method whose TTL has not run out is answered
//!    `X-Cache: Hit from cloudfront` without touching the origin, and everything else
//!    goes to the origin and is answered `X-Cache: Miss from cloudfront`. An answer that
//!    came out of the cache also states HOW LONG it has been held, in the [`AGE`] every
//!    cache generates for a response it serves from storage, so a viewer can tell how
//!    stale the copy it was given is;
//! 8. only then is the ORIGIN the behaviour's `TargetOriginId` names fetched, in process,
//!    through the emulator this service is running within — over the scheme that origin's
//!    own `OriginProtocolPolicy` names ([`origin_uri`]), for the object the viewer's path
//!    names or, where that path is the distribution's ROOT, the one its `DefaultRootObject`
//!    names ([`origin_object_path`]), and as the distribution's own origin access control
//!    says that origin is to be addressed (see below);
//! 9. a viewer that already holds a copy of the object is told so rather than sent it again:
//!    the edge judges the `If-None-Match` and `If-Modified-Since` it sent against the
//!    validators of the representation it is about to serve, and answers `304` carrying none of
//!    its bytes when that copy is the current one ([`conditional`]);
//! 10. a viewer that asked for part of the object is answered that part of it and no more:
//!    the edge evaluates the `Range` it sent against the representation it is about to serve,
//!    whether the cache held it or the origin has just produced it, so a satisfiable window is
//!    answered `206` with those bytes alone and a range that selects nothing is refused `416`
//!    carrying none of them ([`byte_range`]). The origin is therefore always asked for the
//!    COMPLETE representation ([`HEADERS_NEVER_FORWARDED`] holds the viewer's `Range` and its
//!    conditional headers), so one viewer's window is never what the cache ends up holding for
//!    the object;
//! 11. an answer that is an ERROR is replaced by the one the distribution's
//!    `CustomErrorResponses` states for that status, where it states one: the viewer receives
//!    the configured `ResponseCode` and the page `ResponsePagePath` names, and the error itself
//!    is held for `ErrorCachingMinTTL` rather than for the behaviour's own TTL
//!    ([`custom_errors`]) — and whichever span applies, an answer whose ORIGIN asked that it not
//!    be stored is held only by the behaviour's minimum TTL ([`cache_directives`]);
//! 12. and whatever the answer turns out to be — the origin's, the cache's, or a page
//!    CloudFront generated itself — the header groups of the behaviour's
//!    `ResponseHeadersPolicyId` are applied to it: the CORS headers for a viewer whose
//!    `Origin` the policy grants, the security and custom headers it configures, and the
//!    headers its `RemoveHeadersConfig` takes off ([`response_headers`]).
//!
//! # What the origin fetch is, and what it is not
//!
//! The fetch is a real HTTP request to the origin's own `DomainName`, made through the
//! IN-PROCESS transport `OperationContext::callback` hands a handler: it is dispatched
//! back into this same emulator and opens no connection, so the only origin it can reach
//! is one the emulator itself hosts. An origin this emulator serves (an S3 bucket
//! endpoint, another emulated endpoint) answers it, and whatever it answers — its status
//! and its bytes — is what the edge serves and caches. An origin nothing in this emulator
//! serves answers nothing, and that is reported as CloudFront reports an origin it could
//! not reach: `502` with `X-Cache: Error from cloudfront`. Nothing is invented for an
//! origin that did not answer, and nothing off this machine is ever addressed.
//!
//! The SCHEME of that request is the origin's own, from its `OriginProtocolPolicy`, and an
//! origin addressed over a scheme the endpoint it names does not serve is the same `502`:
//! every AWS service endpoint this emulator serves answers over TLS alone, so an origin
//! whose policy addresses one of them over plaintext is an origin connection that cannot be
//! opened — while a bucket's WEBSITE endpoint is the other way round, serving plain HTTP and
//! no HTTPS at all, so an origin over one of those is reachable over plaintext and
//! unreachable over TLS ([`origin_accepts_scheme`]).
//!
//! One origin shape is worth naming because it is the one the Lambda-function-URL
//! topology uses: a function URL host is served by Lambda's own function-URL data plane
//! (`services/service-com-amazonaws-lambda-awsgirapiservice/src/service/function_url.rs`),
//! so whether such an origin answers at all — and what it answers — is that plane's
//! decision, and whatever it answers is what the edge serves and caches. Nothing is
//! invented for it here: an origin that answers nothing is the `502` above. The enforcement
//! in steps 1-6, which is CloudFront's own and needs no origin, is served in full whatever
//! the origin does.
//!
//! Two things the fetch will not do. A REQUEST LOOP is refused instead of followed: an
//! origin whose host the edge itself serves, and a request that has already been through
//! an edge (its `Via` names CloudFront), are answered with the same `502` rather than
//! handed back to the edge they came from. And the VIEWER'S OWN CREDENTIAL is never
//! relayed: the origin is named by the distribution's stored configuration, so an
//! `Authorization` header (or a session token) a viewer presents is dropped rather than
//! forwarded to a destination that viewer did not choose
//! ([`HEADERS_NEVER_FORWARDED`]) — every other viewer header the behaviour's policies name
//! travels to the origin carrying the value the viewer stated for it, wherever the request
//! carried that value ([`forwarding`], [`viewer_origin`]).
//!
//! An origin fetch is made ON BEHALF OF the distribution that configured it. The
//! `OriginAccessControl` the origin's `OriginAccessControlId` names decides whether that
//! fetch is SIGNED — `SigningBehavior` `always` "signs all origin requests" — and the
//! ORIGIN's own resource policy decides whether it is signed FOR: a grant to the
//! `cloudfront.amazonaws.com` service principal, scoped by `aws:SourceArn` to this
//! distribution. Both live in [`super::origin_access`], which this module calls once while
//! planning the fetch and once while making it; a fetch the origin's document does not grant
//! travels unsigned, so the origin answers it as it answers any caller presenting no
//! credential, and whatever it answers is what the edge serves.
//!
//! # Addressing
//!
//! A client on the machine this emulator runs on cannot reach `d<label>.cloudfront.net`
//! at all: nothing resolves it there, so it points at the emulator's own endpoint and
//! carries in the PATH what it cannot carry in the host. [`resolve`] reads three
//! addressings, and each one is checked against this service's own records, so one
//! request still has one claimant:
//!
//! * HOST-STYLE: the host is a distribution's `DomainName` or one of its `Aliases`, and
//!   the whole path is the viewer path;
//! * PREFIXED PATH-STYLE: `/_aws/cloudfront/{distribution-id}/{viewer-path}`, read on
//!   whatever host it arrives at — the reserved first segment is in no service's modelled
//!   path space;
//! * PATH-STYLE: the first path segment names a distribution this service holds — its id,
//!   or the domain name a viewer would have addressed in the host, the same name carried
//!   where this client can carry it — read only on a host that
//!   names NO SERVICE (the emulator's own endpoint) or names THIS service, so a first
//!   segment that happens to name a distribution cannot take a request away from the service
//!   whose endpoint the host names.
//!
//! A distribution is addressed by the name it PUBLISHES, in the host or in that first
//! segment, and by nothing else: what an origin happens to hold decides what a fetch
//! answers, never which distribution a request is for.
//!
//! The viewer's SCHEME is not carried on the request the router hands a service
//! (origin-form URI plus a `Host` header), so `ViewerProtocolPolicy` — which is about
//! HTTP versus HTTPS from the viewer — is not decided here.

/// The STANDARD LOGGING v2 access-log record one served viewer request produces, and the
/// CloudWatch Logs log groups the account's vended-log deliveries carry it into.
pub mod access_logs;
/// WHICH part of the object a viewer asking for a byte range is answered with: the window it
/// selected, or the refusal of a range that selects none of it.
pub mod byte_range;
/// What the ORIGIN said about storing its own answer, and how long the edge holds it because of
/// it.
pub mod cache_directives;
/// The content encodings the edge compresses an answer with, and the bytes they produce.
pub mod compression;
/// WHETHER a viewer that already holds a copy of the object is answered the object again, or
/// told that the copy it holds is still current.
pub mod conditional;
/// CONTINUOUS DEPLOYMENT: which of a continuous deployment's two distributions — the primary
/// the viewer addressed or the staging distribution its policy routes to — serves one viewer
/// request.
pub mod continuous_deployment;
/// The CUSTOM ERROR RESPONSES the distribution states: what a viewer receives when the answer
/// the edge reached for its request is an error, and how long that error is held.
pub mod custom_errors;
/// WHICH headers, cookies and query strings the behaviour's policies key on and forward.
pub mod forwarding;
/// The CLOUDFRONT FUNCTION the matched behaviour associates, run on the viewer event it
/// names.
pub mod functions;
/// The `CloudFront-*` request headers the edge generates itself, and the viewer-stated ones
/// it drops.
pub mod generated_headers;
/// The LAMBDA@EDGE function the matched behaviour associates, invoked on the viewer event it
/// names.
pub mod lambda_edge;
/// The real-time log RECORD one served viewer request produces, and the Kinesis data streams
/// the cache behaviour's real-time log configuration delivers it to.
pub mod realtime_logs;
/// The headers the cache behaviour's RESPONSE HEADERS POLICY puts on the answer, and the ones
/// it takes off.
pub mod response_headers;
/// SIGNED URLs and SIGNED COOKIES: whether a cache behaviour that restricts its content to
/// trusted signers serves one viewer request.
pub mod signed_access;
/// The STANDARD access-log FILE one served viewer request delivers into the Amazon S3 bucket
/// the distribution's own `Logging` configuration names.
pub mod standard_logs;

use aws_smithy_runtime_api::client::http::HttpConnector;
use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{http, OperationContext};
use std::time::{Duration, Instant, SystemTime};

use super::edge_metrics;
use super::origin_access::{self, OriginAccess};
use compression::Encoding;
use crate::ops::types::{
    AllowedMethods, CacheBehavior, CustomErrorResponse, DefaultCacheBehavior, ForwardedValues,
    FunctionAssociations, LambdaFunctionAssociations, Origin, ResponseHeadersPolicyConfig,
    TrustedKeyGroups, VpcOriginEndpointConfig,
};
use crate::state::{
    managed_cache_policy, matches_path_pattern, same_domain, CachedEdgeResponse,
    ComAmazonawsCloudfrontCloudfront20200531State, StoredDistribution, DEFAULT_ALLOWED_METHODS,
    DEFAULT_COMPRESS, DEFAULT_ORIGIN_READ_TIMEOUT, GEO_RESTRICTION_BLACKLIST,
    GEO_RESTRICTION_WHITELIST,
};

/// The reserved path segment that names a DATA plane on the one endpoint this emulator
/// serves its whole surface at, ahead of the distribution id:
/// `/_aws/cloudfront/{distribution-id}/{viewer-path}`.
///
/// It begins with `_`, which no modelled path segment of any service does, so the prefix
/// cannot collide with a control-plane route however a viewer path is spelled.
const EMULATOR_PATH_NAMESPACE: &str = "_aws";

/// The second segment of the prefixed path-style addressing: the data plane this module
/// serves.
const EDGE_PATH_LABEL: &str = "cloudfront";

/// The response header CloudFront reports a cache outcome in.
const X_CACHE: &str = "x-cache";

/// `X-Cache` for an answer the edge cache held.
const CACHE_HIT: &str = "Hit from cloudfront";

/// `X-Cache` for an answer the origin produced for this request.
const CACHE_MISS: &str = "Miss from cloudfront";

/// `X-Cache` for an answer CloudFront itself produced instead of the origin's.
const CACHE_ERROR: &str = "Error from cloudfront";

/// `X-Cache` for an answer a CloudFront FUNCTION generated: the function returned a response
/// object on the viewer-request event, so neither the cache nor an origin produced the answer
/// and CloudFront reports the function as what did ([`functions`]).
const CACHE_FUNCTION_GENERATED: &str = "FunctionGeneratedResponse from cloudfront";

/// `X-Cache` for the answer CloudFront produces when a function's RUN failed: the outcome
/// names the function's execution as what went wrong, which is what separates it from a
/// request the cache or an origin could not answer.
const CACHE_FUNCTION_EXECUTION_ERROR: &str = "FunctionExecutionError from cloudfront";

/// `X-Cache` for an answer a LAMBDA@EDGE function generated: the function returned a response
/// object on the viewer-request event, so neither the cache nor an origin produced the answer
/// and CloudFront reports the function as what did ([`lambda_edge`]).
const CACHE_LAMBDA_GENERATED: &str = "LambdaGeneratedResponse from cloudfront";

/// `X-Cache` for the answer CloudFront produces when a LAMBDA@EDGE function handed it no
/// object to continue from: the outcome names that function's execution as what went wrong,
/// which is what separates it from a request the cache or an origin could not answer.
const CACHE_LAMBDA_EXECUTION_ERROR: &str = "LambdaExecutionError from cloudfront";

/// The response (and origin-request) header carrying the identifier of the request the
/// edge served, from `X-Amz-Cf-Id`: "identifies the request that you're interested in".
const X_AMZ_CF_ID: &str = "x-amz-cf-id";

/// The response header a cache states the age of the response it is serving in: "the number
/// of seconds since the response was generated or successfully validated at the origin
/// server" (RFC 9111 §5.1), which every cache generates for a response it serves out of
/// storage (§4.2.3).
///
/// So an answer this edge served from its cache carries one, counted from the instant the
/// entry was filled, while an answer an origin produced for the request being served carries
/// only whatever the origin itself said about its own age.
const AGE: &str = "age";

/// The `Server` of a response CloudFront generated itself, and the name its `Via` carries
/// on an origin request — which is also how a request that has already been through an
/// edge is recognised ([`carries_cloudfront_via`]).
const SERVER: &str = "CloudFront";

/// The name CloudFront's own `Via` carries.
const VIA_CLOUDFRONT: &str = SERVER;

/// The `User-Agent` CloudFront sends an origin request with when the viewer's own is not
/// forwarded to the origin.
const ORIGIN_USER_AGENT: &str = "Amazon CloudFront";

/// The request header naming the client that made the request, which an origin request carries
/// the viewer's own value for exactly where the behaviour's policies name it
/// ([`ORIGIN_USER_AGENT`] is what it carries otherwise).
const USER_AGENT: &str = "user-agent";

/// The request header naming the chain of addresses a request has travelled.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The `X-Forwarded-For` ONE origin request carries.
///
/// "CloudFront appends the IP address of the viewer to the end of the header value, or adds the
/// header if the viewer didn't include one": every origin request states the address the request
/// came from, whatever the behaviour's policies forward, so an origin that decides anything by
/// the caller's address has one to read. The address is the ONE reading of it this plane has
/// ([`generated_headers::viewer_address`]), which is the LAST entry of the viewer's own chain
/// wherever it stated one that parses — so a chain this plane already read that address out of
/// travels as the viewer sent it, rather than ending with the same address twice.
fn forwarded_for(req: &RtRequest) -> String {
    let address = generated_headers::viewer_address(req);
    let chain = req
        .headers()
        .get(FORWARDED_FOR)
        .map(str::trim)
        .filter(|chain| !chain.is_empty());
    match chain {
        Some(chain)
            if chain
                .rsplit(',')
                .next()
                .map(str::trim)
                .is_some_and(|last| last == address) =>
        {
            chain.to_string()
        }
        Some(chain) => format!("{chain}, {address}"),
        None => address.to_string(),
    }
}

/// The status a viewer request gets when CloudFront refuses it at the edge.
const FORBIDDEN: u16 = 403;

/// The status a viewer request gets when the behaviour's `ViewerProtocolPolicy` is
/// `redirect-to-https` and the viewer dialled the plaintext scheme: "CloudFront returns an
/// HTTP status code of 301 (Moved Permanently) to the viewer along with the HTTPS URL".
const MOVED_PERMANENTLY: u16 = 301;

/// The response header that redirect carries the HTTPS URL in.
const LOCATION: &str = "location";

/// `ViewerProtocolPolicy` `redirect-to-https`: "If a viewer submits an HTTP request,
/// CloudFront returns an HTTP status code of 301 (Moved Permanently) to the viewer along
/// with the HTTPS URL. The viewer then resubmits the request using the new URL."
const REDIRECT_TO_HTTPS: &str = "redirect-to-https";

/// `ViewerProtocolPolicy` `https-only`: "If a viewer sends an HTTP request, CloudFront
/// returns an HTTP status code of 403 (Forbidden)."
const HTTPS_ONLY: &str = "https-only";

/// The scheme a viewer that dialled the plaintext half of an edge address used, which is
/// the only scheme either policy above acts on. The one spelling of it
/// ([`generated_headers`] reads the viewer's scheme off the request).
const PLAINTEXT_SCHEME: &str = generated_headers::PLAINTEXT_SCHEME;

/// The scheme a redirected viewer is sent to.
const SECURE_SCHEME: &str = generated_headers::DEFAULT_SCHEME;

/// `OriginProtocolPolicy` `http-only`: the origin is reached over HTTP alone.
const ORIGIN_HTTP_ONLY: &str = "http-only";

/// `OriginProtocolPolicy` `match-viewer`: the origin is reached over the scheme the VIEWER
/// dialled.
const ORIGIN_MATCH_VIEWER: &str = "match-viewer";

/// The default port of each scheme, which an origin URI leaves out.
const HTTP_DEFAULT_PORT: i32 = 80;
const HTTPS_DEFAULT_PORT: i32 = 443;

/// The status a viewer request gets when CloudFront could not get a response from the
/// origin.
const BAD_GATEWAY: u16 = 502;

/// The status a viewer request gets when the CloudFront function the matched behaviour
/// associates produced no event object for the edge to continue from — it raised, it did not
/// compile, it returned something that is no event object, or a bound of the runtime stopped
/// it. CloudFront answers a function failure `503`, having neither a request to fetch with
/// nor a response to serve ([`functions`]).
const FUNCTION_FAILED_STATUS: u16 = functions::FAILED_STATUS;

/// The detail line CloudFront's generated page carries for that failure, which names the
/// association rather than the request as the thing that could not be carried out.
const FUNCTION_FAILED_DETAIL: &str =
    "The CloudFront function associated with the CloudFront distribution is invalid or could \
     not be executed.";

/// The status a viewer request gets when the LAMBDA@EDGE function the matched behaviour
/// associates produced nothing for the edge to continue from ([`lambda_edge::FAILED_STATUS`]).
const LAMBDA_FAILED_STATUS: u16 = lambda_edge::FAILED_STATUS;

/// The detail line CloudFront's generated page carries for that failure, which names the
/// Lambda@Edge association as the thing that could not be carried out.
const LAMBDA_FAILED_DETAIL: &str =
    "The Lambda function associated with the CloudFront distribution is invalid or doesn't \
     have the required permissions.";

/// The detail line CloudFront's generated page carries for a request it refused.
const BAD_REQUEST_DETAIL: &str = "Bad request.";

/// The detail line CloudFront's generated page carries when it could not reach the
/// origin, from CloudFront's own troubleshooting of a 502.
const NO_ORIGIN_DETAIL: &str = "CloudFront wasn't able to connect to the origin.";

/// The detail line CloudFront's generated page carries for a viewer its geographic
/// restriction refuses.
const GEO_RESTRICTED_DETAIL: &str =
    "The Amazon CloudFront distribution is configured to block access from your country.";

/// The detail line CloudFront's generated page carries for a viewer method the matched cache
/// behaviour does not allow — the refusal `AllowedMethods` makes, which names the method as
/// the thing refused rather than the request.
const METHOD_NOT_ALLOWED_DETAIL: &str =
    "This distribution is not configured to allow the HTTP request method that was used for \
     this request. The distribution supports only cachable requests.";

/// The headers that belong to ONE hop of an HTTP connection and are never forwarded to an
/// origin, as RFC 9110 lists them, plus the viewer's own `Authorization` and the headers
/// CloudFront itself puts on an origin request.
///
/// A viewer credential is never relayed to an origin by this emulator: the origin is named
/// by the distribution's stored configuration, so forwarding a caller's credential there
/// would hand it to a destination the caller of THIS request did not choose. Every other
/// viewer header is forwarded exactly as the behaviour's origin request policy says
/// ([`forwarding`]), carrying the value the viewer stated for it.
///
/// [`X_AMZ_CF_ID`] is here for the other reason: it is CloudFront's OWN identifier for the
/// request it is serving ([`stamp`] mints one per request), so an origin request carries the
/// edge's value and never a viewer's — a header a viewer sent under that name describes a
/// request the edge did not serve. The whole `CloudFront-*` family is dropped for that same
/// reason, through [`generated_headers::is_generated`].
///
/// The viewer's `Range` is here for a third reason: the edge evaluates a byte range ITSELF,
/// against the whole representation it holds ([`byte_range`]), so the origin is asked for that
/// whole representation and every window of it can be served from one cached object. The
/// viewer's CONDITIONAL headers are here for that same reason: the edge judges them itself,
/// against the representation it is about to serve ([`conditional`]), so one viewer's
/// revalidation never becomes what the cache holds for the object and every later viewer —
/// conditional or not — is answered from the one entry.
const HEADERS_NEVER_FORWARDED: [&str; 15] = [
    "authorization",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    byte_range::RANGE,
    conditional::IF_NONE_MATCH,
    conditional::IF_MODIFIED_SINCE,
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "x-amz-security-token",
    X_AMZ_CF_ID,
    // The reserved name the viewer's `Origin` reaches this data plane under when the request
    // came in on a distribution's own address ([`response_headers::VIEWER_ORIGIN`]): the NAME
    // is this emulator's own plumbing between one of its addresses and this service, so no
    // origin request carries it. The VALUE is the viewer's `Origin`, and it travels under that
    // header where the behaviour's policies name it ([`viewer_origin`]).
    response_headers::VIEWER_ORIGIN,
];

/// The response headers an answer the edge serves does NOT carry over from the origin's own
/// answer.
///
/// CloudFront serves the origin's response headers to the viewer — that is what carries an
/// object's metadata, its validators and its cache directives through the edge — so this list
/// is the exceptions, and each is here for one of three reasons:
///
/// * a HOP-BY-HOP header (RFC 9110 names them) describes the connection the edge made to the
///   origin, not the object it fetched, so it cannot be relayed onto another connection;
/// * `Content-Length`, `Content-Type` and `Content-Encoding` describe the bytes the edge is
///   SERVING, which are not always the bytes the origin sent: an answer compressed for this
///   viewer ([`compression`]) is a different length under a different coding, so the two the
///   edge restates per request are held on [`OriginAnswer`] itself and the length is left to
///   the connection that writes the body;
/// * [`X_CACHE`] and [`X_AMZ_CF_ID`] are what the EDGE says about the request it just served
///   ([`stamp`]), so a header an origin sent under those names describes a different request.
const RESPONSE_HEADERS_NEVER_SERVED: [&str; 13] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    CONTENT_TYPE,
    CONTENT_ENCODING,
    X_CACHE,
    X_AMZ_CF_ID,
];

/// Whether the origin's own `name` response header travels on to the viewer
/// ([`RESPONSE_HEADERS_NEVER_SERVED`] is the list of the ones that do not).
///
/// A header name is case-insensitive (RFC 9110), so the comparison is.
fn is_served_from_origin(name: &str) -> bool {
    !RESPONSE_HEADERS_NEVER_SERVED
        .iter()
        .any(|never| name.eq_ignore_ascii_case(never))
}

/// The most bytes of the origin's OWN response headers one answer carries over.
///
/// A bound of this EMULATOR's, not a CloudFront one: an answer held in the edge cache keeps
/// these headers beside its bytes ([`MAX_CACHED_BODY_BYTES`] bounds those), so this is what
/// bounds the rest of one entry however large a message an origin answers with. 8 KiB is the
/// header budget a stock HTTP server allows a whole message, so an origin answering within what
/// a server would accept loses nothing. The SAME set is served and cached, so an answer out of
/// the cache carries exactly what the answer from the origin carried.
const MAX_ORIGIN_HEADER_BYTES: usize = 8 * 1024;

/// The origin's own response headers the edge carries over, lowercased and in the order the
/// origin stated them, up to [`MAX_ORIGIN_HEADER_BYTES`].
fn served_origin_headers(
    headers: &::aws_smithy_runtime_api::http::Headers,
    uri: &str,
) -> Vec<(String, String)> {
    let mut served: Vec<(String, String)> = Vec::new();
    let mut bytes = 0usize;
    for (name, value) in headers.iter().filter(|(name, _)| is_served_from_origin(name)) {
        bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
        if bytes > MAX_ORIGIN_HEADER_BYTES {
            ::tracing::warn!(
                uri = %uri,
                "this origin stated more response headers than the edge carries over, so the \
                 rest of them are not served"
            );
            break;
        }
        served.push((name.to_ascii_lowercase(), value.to_string()));
    }
    served
}

/// The header a viewer asks for a content coding with, and the one an answer states its
/// coding in: `Accept-Encoding` is normalized by the cache policy that enables gzip or
/// Brotli ([`forwarding::accept_encoding`]), and `Content-Encoding` says what decodes the
/// bytes the edge served.
const ACCEPT_ENCODING: &str = "accept-encoding";

/// The response header naming the coding the served bytes are under.
const CONTENT_ENCODING: &str = "content-encoding";

/// The header the viewer's cookies travel in, as one `name=value` list.
const COOKIE: &str = "cookie";

/// The response (and request) header naming the media type of a body.
const CONTENT_TYPE: &str = "content-type";

/// The request header naming the host the request is addressed to, which an origin request
/// carries as [`origin_request_host`] decides.
const HOST: &str = "host";

/// The request header a browser states the site it is acting for in, and the one a CORS
/// preflight's answer is decided from.
const ORIGIN: &str = "origin";

/// The `Origin` the VIEWER stated for this request, wherever the request carries it.
///
/// A viewer that reached a distribution's OWN address states it under the reserved name that
/// address forwards it as ([`response_headers::VIEWER_ORIGIN`]), because the leg from there
/// into this data plane is this emulator calling itself and a leg carrying a browser's own
/// headers is one the front door refuses; a request dispatched at this service's endpoint
/// carries the header itself. Either way the value is the one the VIEWER sent, so everything
/// that reads the viewer's `Origin` — the response headers policy's CORS group
/// ([`response_headers::ViewerCors`]), the cache key and the origin request — reads it from
/// here and reads the same value.
fn viewer_origin(req: &RtRequest) -> Option<String> {
    req.headers()
        .get(response_headers::VIEWER_ORIGIN)
        .or_else(|| req.headers().get(ORIGIN))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// What a viewer request addresses, as the wire carries it: the request's host — absent
/// on a request that carries neither a `Host` header nor an absolute URI — its path, and
/// its query.
///
/// Built from BOTH shapes the router hands this module, the buffered parts the claim
/// reads and the runtime-API request the serving seam reads, so one rule resolves both and
/// the claim cannot resolve a different distribution than the answer does.
#[derive(Debug)]
pub struct Addressed<'a> {
    host: Option<&'a str>,
    path: &'a str,
    query: &'a str,
}

impl<'a> Addressed<'a> {
    /// The addressing of the buffered request these parts came from.
    fn of_parts(parts: &'a http::request::Parts) -> Self {
        Self {
            host: request_host(parts),
            path: parts.uri.path(),
            query: parts.uri.query().unwrap_or_default(),
        }
    }

    /// The addressing of a runtime-API request, which carries its URI as a string.
    fn of_request(req: &'a RtRequest) -> Self {
        Self {
            host: ::service_common::data_plane_addressing::addressed_authority(
                req.uri(),
                req.headers().get("host"),
            ),
            path: path_of(req.uri()),
            query: query_of(req.uri()),
        }
    }
}

/// What a viewer request addresses, as this service's records resolve it: the
/// distribution, and the viewer path WITHIN its endpoint.
#[derive(Debug)]
struct Resolved<'s, 'r> {
    /// The distribution that SERVES this request — the one whose behaviours, cache and
    /// origins answer it. The one the viewer addressed, unless a continuous deployment policy
    /// routed the request to that distribution's staging distribution
    /// ([`continuous_deployment`]).
    distribution: &'s StoredDistribution,
    /// The distribution whose PUBLISHED name the viewer addressed: the same record as
    /// `distribution`, unless a continuous deployment policy routed this request elsewhere.
    /// It is what the names a viewer may be sent back to, or may have signed a URL for, come
    /// from — a viewer addresses one distribution however the request is then served.
    addressed: &'s StoredDistribution,
    viewer_path: &'r str,
}

/// What `addressed` addresses, or `None` when this data plane serves it not at all.
///
/// The three addressings are the module docs' own list, read in that order.
fn resolve<'s, 'r>(
    store: &'s ComAmazonawsCloudfrontCloudfront20200531State,
    addressed: &Addressed<'r>,
) -> Option<Resolved<'s, 'r>> {
    if let Some(host) = addressed.host {
        if let Some(distribution) = store
            .distributions
            .values()
            .find(|distribution| serves_host(distribution, host))
        {
            return Some(Resolved {
                distribution,
                addressed: distribution,
                viewer_path: addressed.path,
            });
        }
    }
    // PREFIXED PATH-STYLE: the path names this data plane before it names the
    // distribution, so it is read on whatever host it arrives at.
    if let Some((id, viewer_path)) = split_edge_prefix(addressed.path) {
        return store
            .distributions
            .get(id)
            .map(|distribution| Resolved {
                distribution,
                addressed: distribution,
                viewer_path,
            });
    }
    // PATH-STYLE, on the emulator's own endpoint or this service's: the first segment
    // names a distribution this service holds.
    let host_service = addressed.host.and_then(service_named_by_host);
    if host_service
        .as_deref()
        .is_some_and(|service| service != crate::ROUTE_KEY)
    {
        return None;
    }
    let (first, rest) = split_first_segment(addressed.path)?;
    // The segment is the distribution's id, or the domain name a viewer would have
    // addressed in the host — the same name, carried where this client can carry it, port
    // and all ([`serves_host`] reads both forms).
    store
        .distributions
        .get(first)
        .or_else(|| {
            store
                .distributions
                .values()
                .find(|distribution| serves_host(distribution, first))
        })
        .map(|distribution| Resolved {
            distribution,
            addressed: distribution,
            viewer_path: rest,
        })
}

/// Whether `host` is a domain name this distribution is served on: the `DomainName`
/// CloudFront assigned it, or one of the alternate domain names its `Aliases` carry.
///
/// DNS names are case-insensitive, so the comparison is ([`same_domain`]). A multi-tenant
/// distribution reports no domain of its own and so is served on none: its viewers reach
/// it through a connection group's routing endpoint.
///
/// The `DomainName` is compared against the host AS ADDRESSED first, because the name a
/// distribution publishes is the address of the listener its edge is served on
/// (`crate::state::EdgeEndpoint`) and that address names a PORT: two distributions differ in
/// exactly that port, so a comparison that dropped it would resolve every one of them to
/// whichever the store lists first. A published name that carries no port — the name
/// CloudFront's own zone would give it — matches the host without its port, which is what a
/// client that resolves such a name sends.
///
/// An alias is only read when it is a name a VIEWER could be sent to: the alternate domain
/// names are caller-supplied and unverifiable here, so one spelled like another service's
/// endpoint, like `localhost`, or like an IP literal would take over the address every
/// client on this machine dials ([`addressable_alias`]). The domain CloudFront itself
/// assigned is never such a name, and a distribution whose alias is one is still reachable
/// through the reserved data-plane path.
fn serves_host(distribution: &StoredDistribution, host: &str) -> bool {
    if same_domain(&distribution.domain_name, host) {
        return true;
    }
    let host = bare_host(host);
    if same_domain(&distribution.domain_name, host) {
        return true;
    }
    if !addressable_alias(host) {
        return false;
    }
    distribution
        .config
        .aliases
        .as_ref()
        .and_then(|aliases| aliases.items.as_ref())
        .is_some_and(|aliases| aliases.iter().any(|alias| same_domain(alias, host)))
}

/// Whether `host` is a name an alternate domain name may claim: a customer domain, rather
/// than an address this emulator — or AWS itself — is reached at.
///
/// An alternate domain name is a domain the customer owns and points at the distribution,
/// so none of these is one: the loopback names a client on this machine dials (`localhost`
/// and the names under it), any IP literal (bracketed IPv6 included), and a name in one of
/// the zones AWS serves its own endpoints in ([`AWS_ENDPOINT_SUFFIXES`]) — which is where
/// every other emulated service's endpoint host lives, and where CloudFront's own
/// `cloudfront.net` domains live. A stored alias naming one of them would take over an
/// address other callers reach this emulator at, and `Aliases` is caller-supplied and
/// unverifiable here.
fn addressable_alias(host: &str) -> bool {
    let host = bare_host(host);
    if host.is_empty() {
        return false;
    }
    let lowercase = host.to_ascii_lowercase();
    if lowercase == "localhost" || lowercase.ends_with(".localhost") {
        return false;
    }
    // An IP literal: a bracketed IPv6 address, or a name that parses as an address.
    if lowercase.starts_with('[') || lowercase.parse::<::std::net::IpAddr>().is_ok() {
        return false;
    }
    !AWS_ENDPOINT_SUFFIXES
        .iter()
        .any(|suffix| lowercase == suffix.trim_start_matches('.') || lowercase.ends_with(suffix))
}

/// The zones AWS serves its own endpoints in, which an alternate domain name is never in:
/// every emulated service's endpoint host is under one of them, and so is the
/// `cloudfront.net` domain CloudFront assigns a distribution itself.
const AWS_ENDPOINT_SUFFIXES: [&str; 5] = [
    ".amazonaws.com",
    ".amazonaws.com.cn",
    ".api.aws",
    ".on.aws",
    ".cloudfront.net",
];

/// The distribution id and the viewer path of a PREFIXED PATH-STYLE address, or `None`
/// when `path` does not carry that prefix.
///
/// The viewer path keeps the leading `/` the host-style addressing carries, so one path is
/// matched against a `PathPattern` however the request was addressed.
fn split_edge_prefix(path: &str) -> Option<(&str, &str)> {
    let (namespace, after_namespace) = split_first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (label, after_label) = split_first_segment(after_namespace)?;
    if label != EDGE_PATH_LABEL {
        return None;
    }
    split_first_segment(after_label)
}

/// The first non-empty segment of `path` and the rest of the path, which keeps a leading
/// `/` so it reads as a path of its own.
fn split_first_segment(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.trim_start_matches('/');
    let (first, rest) = match trimmed.find('/') {
        Some(mark) => (&trimmed[..mark], &trimmed[mark..]),
        None => (trimmed, "/"),
    };
    (!first.is_empty()).then_some((first, rest))
}

/// The service `host` names to the emulator's front door, or `None` when it names none.
///
/// The front door's OWN host reader decides it, rather than a second rule written here:
/// the request the path addressings are read for is exactly the one that reader can place
/// nowhere — a loopback address or `localhost`, the only endpoint a client on this machine
/// can reach.
fn service_named_by_host(host: &str) -> Option<String> {
    let mut headers = http::HeaderMap::new();
    let value = http::HeaderValue::from_str(host).ok()?;
    headers.insert(http::header::HOST, value);
    ::emulator_server::service_from_host(&headers)
}

/// A host without its userinfo or its port, as the front door's own host reader reads it.
///
/// A bracketed IPv6 literal (`[::1]:443`) keeps its brackets and all of its colons: only
/// a port after the closing bracket is removed.
fn bare_host(host: &str) -> &str {
    let without_userinfo = host.rsplit('@').next().unwrap_or(host);
    if let Some(closing) = without_userinfo.find(']') {
        return &without_userinfo[..=closing];
    }
    without_userinfo
        .split(':')
        .next()
        .unwrap_or(without_userinfo)
}

/// Whether an authority STATES a port of its own — `host:port`, a bracketed IPv6 literal
/// with a port after the closing bracket included.
///
/// Read with the same rule [`bare_host`] strips one by, so the two cannot disagree about
/// where a host ends and its port begins.
fn states_a_port(authority: &str) -> bool {
    let without_userinfo = authority.rsplit('@').next().unwrap_or(authority);
    without_userinfo.len() != bare_host(authority).len()
}

/// Whether THIS data plane serves the request these parts came from — the data-plane half
/// of `Service::can_handle`.
///
/// Narrowed to a distribution this service HOLDS, read from the one shared store, so
/// nothing is claimed that this service cannot answer about and exactly one service claims
/// a request.
pub fn claims_parts<Eng: Send + Sync + 'static>(engine: &Eng, parts: &http::request::Parts) -> bool {
    claims(engine, &Addressed::of_parts(parts))
}

/// [`claims_parts`] for the runtime-API request the serving seam is handed.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    claims(engine, &Addressed::of_request(req))
}

/// Whether `req` is addressed to the EDGE DOMAIN of a distribution this service holds —
/// the host-style addressing alone.
///
/// Which plane a request belongs to is decided by its HOST: a request whose host is a
/// distribution's own domain name is viewer traffic to that distribution, whatever its
/// path spells, and never a call to the control plane that happens to share a path with a
/// modelled route. The two PATH addressings are the other way round, because they arrive
/// on a host that names the control plane or nothing: there the modelled route wins and
/// the path addressings are read only for a path no modelled route claims.
pub fn claims_request_by_host<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    let addressed = Addressed::of_request(req);
    let Some(host) = addressed.host else {
        return false;
    };
    crate::state::service_state(engine).is_some_and(|state| {
        state
            .lock()
            .distributions
            .values()
            .any(|distribution| serves_host(distribution, host))
    })
}

/// Whether this data plane serves `addressed`.
fn claims<Eng: Send + Sync + 'static>(engine: &Eng, addressed: &Addressed<'_>) -> bool {
    crate::state::service_state(engine)
        .is_some_and(|state| resolve(&state.lock(), addressed).is_some())
}

/// The cache behaviour selected for one viewer request, flattened out of the two shapes a
/// behaviour is modelled in (`DefaultCacheBehavior` and `CacheBehavior` carry the same
/// members).
#[derive(Debug)]
struct Behavior {
    /// `PathPattern` of the ordered behaviour that matched, or `None` for the default
    /// behaviour, which matches by being last.
    path_pattern: Option<String>,
    /// `RealtimeLogConfigArn` — "the Amazon Resource Name (ARN) of the real-time log
    /// configuration that is attached to this cache behavior", which is what the edge
    /// delivers this request's log record through ([`realtime_logs`]).
    realtime_log_config_arn: Option<String>,
    target_origin_id: String,
    /// `ViewerProtocolPolicy` — "the protocol that viewers can use to access the files in
    /// the origin specified by `TargetOriginId`", which is a required member of both
    /// shapes.
    viewer_protocol_policy: String,
    allowed_methods: Option<AllowedMethods>,
    /// `TrustedKeyGroups` — "a list of key groups that CloudFront can use to validate signed
    /// URLs or signed cookies": a behaviour that enables one serves only a request carrying a
    /// signature one of those groups' public keys verifies ([`signed_access`]).
    trusted_key_groups: Option<TrustedKeyGroups>,
    cache_policy_id: Option<String>,
    origin_request_policy_id: Option<String>,
    /// `Compress` — "whether you want CloudFront to automatically compress certain files
    /// for this cache behavior", and [`DEFAULT_COMPRESS`] for a behaviour that states none.
    compress: bool,
    /// `MinTTL`, the legacy per-behaviour minimum, for a behaviour that names no cache policy:
    /// the one span that still holds content whose origin asked no cache to store it
    /// ([`cache_directives::held_for`]).
    min_ttl: Option<i64>,
    /// `DefaultTTL`, the legacy per-behaviour default, for a behaviour that names no cache
    /// policy: how long an answer the behaviour may cache is held ([`cache_ttl`]).
    default_ttl: Option<i64>,
    /// `MaxTTL`, the legacy per-behaviour maximum, read where the behaviour states no
    /// `DefaultTTL` ([`cache_ttl`]).
    max_ttl: Option<i64>,
    /// `ForwardedValues` — the legacy member that states what a behaviour naming no cache
    /// policy keys on and forwards: "a `CacheBehavior` must include either a
    /// `CachePolicyId` or `ForwardedValues`" ([`forwarding`] reads it).
    forwarded_values: Option<ForwardedValues>,
    /// `ResponseHeadersPolicyId` — the policy whose header groups the edge applies to the
    /// answer it serves this request ([`response_headers`]).
    response_headers_policy_id: Option<String>,
    /// `FunctionAssociations` — "a list of CloudFront functions that are associated with a
    /// cache behavior", each naming the viewer event its function runs on ([`functions`]).
    function_associations: Option<FunctionAssociations>,
    /// `LambdaFunctionAssociations` — "a complex type that contains zero or more
    /// `LambdaFunctionAssociation` items for this cache behavior", each naming the event its
    /// Lambda@Edge function is invoked on ([`lambda_edge`]).
    lambda_function_associations: Option<LambdaFunctionAssociations>,
}

impl Behavior {
    /// The ordered behaviour `behavior` describes.
    fn of_ordered(behavior: &CacheBehavior) -> Self {
        Self {
            path_pattern: Some(behavior.path_pattern.clone()),
            realtime_log_config_arn: behavior.realtime_log_config_arn.clone(),
            target_origin_id: behavior.target_origin_id.clone(),
            viewer_protocol_policy: behavior.viewer_protocol_policy.clone(),
            allowed_methods: behavior.allowed_methods.clone(),
            trusted_key_groups: behavior.trusted_key_groups.clone(),
            cache_policy_id: behavior.cache_policy_id.clone(),
            origin_request_policy_id: behavior.origin_request_policy_id.clone(),
            compress: behavior.compress.unwrap_or(DEFAULT_COMPRESS),
            min_ttl: behavior.min_ttl,
            default_ttl: behavior.default_ttl,
            max_ttl: behavior.max_ttl,
            forwarded_values: behavior.forwarded_values.clone(),
            response_headers_policy_id: behavior.response_headers_policy_id.clone(),
            function_associations: behavior.function_associations.clone(),
            lambda_function_associations: behavior.lambda_function_associations.clone(),
        }
    }

    /// The default behaviour `behavior` describes.
    fn of_default(behavior: &DefaultCacheBehavior) -> Self {
        Self {
            path_pattern: None,
            realtime_log_config_arn: behavior.realtime_log_config_arn.clone(),
            target_origin_id: behavior.target_origin_id.clone(),
            viewer_protocol_policy: behavior.viewer_protocol_policy.clone(),
            allowed_methods: behavior.allowed_methods.clone(),
            trusted_key_groups: behavior.trusted_key_groups.clone(),
            cache_policy_id: behavior.cache_policy_id.clone(),
            origin_request_policy_id: behavior.origin_request_policy_id.clone(),
            compress: behavior.compress.unwrap_or(DEFAULT_COMPRESS),
            min_ttl: behavior.min_ttl,
            default_ttl: behavior.default_ttl,
            max_ttl: behavior.max_ttl,
            forwarded_values: behavior.forwarded_values.clone(),
            response_headers_policy_id: behavior.response_headers_policy_id.clone(),
            function_associations: behavior.function_associations.clone(),
            lambda_function_associations: behavior.lambda_function_associations.clone(),
        }
    }

    /// The methods this behaviour serves at all: `AllowedMethods$Items`, or
    /// [`DEFAULT_ALLOWED_METHODS`] for a behaviour that names none.
    fn allows(&self, method: &str) -> bool {
        match &self.allowed_methods {
            Some(allowed) => allowed
                .items
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(method)),
            None => DEFAULT_ALLOWED_METHODS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(method)),
        }
    }

    /// Whether this behaviour caches the response to `method`: `CachedMethods$Items`,
    /// or [`DEFAULT_ALLOWED_METHODS`] when the behaviour names neither list.
    ///
    /// "The HTTP methods for which CloudFront caches responses": a method that is allowed
    /// but not cached is forwarded to the origin every time and never answered from the
    /// cache.
    fn caches(&self, method: &str) -> bool {
        match self
            .allowed_methods
            .as_ref()
            .and_then(|allowed| allowed.cached_methods.as_ref())
        {
            Some(cached) => cached
                .items
                .iter()
                .any(|cached| cached.eq_ignore_ascii_case(method)),
            None => DEFAULT_ALLOWED_METHODS
                .iter()
                .any(|cached| cached.eq_ignore_ascii_case(method)),
        }
    }
}

/// The behaviour that applies to `viewer_path`: the first ordered behaviour whose
/// `PathPattern` matches it, and otherwise the default behaviour.
///
/// "CloudFront compares the path in the viewer request with the path patterns in the cache
/// behaviors ... in the order in which the cache behaviors are listed", and "if none of
/// the path patterns match, CloudFront applies the default cache behavior".
fn select_behavior(distribution: &StoredDistribution, viewer_path: &str) -> Behavior {
    let ordered = distribution
        .config
        .cache_behaviors
        .as_ref()
        .and_then(|behaviors| behaviors.items.as_ref())
        .into_iter()
        .flatten()
        .find(|behavior| matches_path_pattern(&behavior.path_pattern, viewer_path));
    match ordered {
        Some(behavior) => Behavior::of_ordered(behavior),
        None => Behavior::of_default(&distribution.config.default_cache_behavior),
    }
}

/// What the matched behaviour's `ViewerProtocolPolicy` does with a viewer that dialled the
/// PLAINTEXT scheme, or `None` when this viewer's scheme is served as it arrived.
///
/// The member is "the protocol that viewers can use to access the files in the origin
/// specified by `TargetOriginId`", and each value states its own answer: `allow-all` —
/// "viewers can use HTTP or HTTPS" — serves the request as it arrived; `redirect-to-https`
/// answers "an HTTP status code of 301 (Moved Permanently) to the viewer along with the
/// HTTPS URL", which the viewer "then resubmits the request using"; and `https-only`
/// answers "an HTTP status code of 403 (Forbidden)". A viewer that already dialled `https`
/// is served by every one of them, so the decision reads the scheme first.
///
/// Decided from the configuration and the connection alone: it needs no origin, no cache
/// and no method, so it is made before any of them.
fn viewer_protocol_plan(
    behavior: &Behavior,
    addressed: &Addressed<'_>,
    resolved: &Resolved<'_, '_>,
    req: &RtRequest,
) -> Option<Plan> {
    if generated_headers::viewer_scheme(req.uri()) != PLAINTEXT_SCHEME {
        return None;
    }
    match behavior.viewer_protocol_policy.as_str() {
        REDIRECT_TO_HTTPS => Some(Plan::Redirect {
            location: secure_url(
                viewer_host(addressed, resolved),
                resolved.viewer_path,
                addressed.query,
            ),
        }),
        HTTPS_ONLY => Some(Plan::Generated {
            status: FORBIDDEN,
            detail: BAD_REQUEST_DETAIL,
        }),
        // `allow-all`, and a policy value this emulator does not know: the request is
        // served over the scheme it arrived on.
        _ => None,
    }
}

/// The host a redirected viewer is sent back to: the one it ADDRESSED, when that host is a
/// domain name this distribution is served on, and otherwise the `DomainName` the
/// distribution publishes.
///
/// A viewer reaches a distribution at a name the distribution itself published — its
/// `DomainName` or one of its `Aliases` — so a redirect keeps the viewer on the name it
/// chose. A request that arrived through the reserved data-plane path carries this
/// service's own host instead of a name of the distribution's, and the name to send such a
/// viewer to is the one the distribution publishes. The names read are the ADDRESSED
/// distribution's, which is the one a viewer can be sent back to whichever of a continuous
/// deployment's two distributions serves the request ([`continuous_deployment`]).
fn viewer_host<'a>(addressed: &Addressed<'a>, resolved: &'a Resolved<'_, '_>) -> &'a str {
    addressed
        .host
        .filter(|host| serves_host(resolved.addressed, host))
        .unwrap_or(&resolved.addressed.domain_name)
}

/// The `https` URL of the same request: the host the viewer addressed, the viewer path and
/// the viewer query, which is the "HTTPS URL" a `redirect-to-https` answer carries.
fn secure_url(host: &str, viewer_path: &str, query: &str) -> String {
    let path = if viewer_path.starts_with('/') {
        viewer_path.to_string()
    } else {
        format!("/{viewer_path}")
    };
    if query.is_empty() {
        format!("{SECURE_SCHEME}://{host}{path}")
    } else {
        format!("{SECURE_SCHEME}://{host}{path}?{query}")
    }
}

/// The country a viewer request comes from, as the edge resolves it
/// ([`generated_headers::viewer_country`]): the ONE value this request's country has, read
/// by the geographic restriction below and by the header the origin receives alike.
fn viewer_country(req: &RtRequest) -> String {
    generated_headers::viewer_country(req)
}

/// Whether this distribution's GEOGRAPHIC RESTRICTION refuses a viewer in `country`,
/// computed from `Restrictions.GeoRestriction` alone.
///
/// `RestrictionType` decides how the `Items` are read. `blacklist`: the locations "specify
/// the countries in which you don't want CloudFront to distribute your content", so a
/// viewer in one of them is refused and every other viewer is served. `whitelist`: they
/// "specify the countries in which you want CloudFront to distribute your content", so a
/// viewer anywhere else is refused. `none`: "No geo restriction is enabled, meaning access
/// to content is not restricted by client geo location", so nobody is refused — which is
/// also what a configuration carrying no `Restrictions` at all means
/// ([`crate::state::GEO_RESTRICTION_NONE`]).
///
/// A `Location` is "a two-letter, uppercase country code", so the country the edge resolved
/// is matched without regard to its case.
fn geo_restricts(distribution: &StoredDistribution, country: &str) -> bool {
    let Some(restrictions) = distribution.config.restrictions.as_ref() else {
        return false;
    };
    let geo = &restrictions.geo_restriction;
    let listed = geo
        .items
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|location| location.eq_ignore_ascii_case(country));
    match geo.restriction_type.as_str() {
        GEO_RESTRICTION_BLACKLIST => listed,
        GEO_RESTRICTION_WHITELIST => !listed,
        // `none`, and a restriction type this emulator does not know: nothing is
        // restricted, which is what an absent `Restrictions` member means too.
        _ => false,
    }
}

/// The longest an object is held in the edge cache, whatever the configuration asks for:
/// a year in seconds, the `MaxTTL` every AWS-managed cache policy that caches at all
/// publishes ([`crate::state::MANAGED_CACHE_POLICIES`]). A stored TTL is an `i64` taken
/// from a request, so clamping it here is what keeps an expiry instant computable.
const MAX_CACHE_TTL_SECONDS: u64 = 31_536_000;

/// How long a response the behaviour caches may be served from the edge cache, or `None`
/// when the behaviour caches nothing at all.
///
/// The cache policy the behaviour names decides it — its `DefaultTTL` is "the default
/// amount of time ... that you want objects to stay in the CloudFront cache" — and a
/// policy whose TTLs are all zero (`Managed-CachingDisabled`) caches nothing, so such a
/// behaviour forwards every request. A behaviour that names no cache policy carries the
/// legacy per-behaviour `DefaultTTL` instead.
fn cache_ttl(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    behavior: &Behavior,
) -> Option<Duration> {
    held_span(
        store,
        behavior,
        |policy| policy.default_ttl.unwrap_or(policy.min_ttl),
        |managed| managed.default_ttl,
        |behavior| behavior.default_ttl.or(behavior.max_ttl),
    )
}

/// The behaviour's MINIMUM TTL, or `None` for a behaviour whose minimum is no span at all.
///
/// `MinTTL` is "the minimum amount of time that you want objects to stay in CloudFront caches
/// before CloudFront forwards another request to your origin", and it is read from the same two
/// places [`cache_ttl`] reads the span an answer is held for ([`held_span`]). It is what still
/// holds content whose origin asked no cache to store it ([`cache_directives::held_for`]).
fn cache_min_ttl(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    behavior: &Behavior,
) -> Option<Duration> {
    held_span(
        store,
        behavior,
        |policy| policy.min_ttl,
        |managed| managed.min_ttl,
        |behavior| behavior.min_ttl,
    )
}

/// One span a cache behaviour states, read from the one place that behaviour states it: the
/// cache policy the behaviour names (`stored` for a policy this account holds, `managed` for one
/// of CloudFront's own), and the legacy per-behaviour member (`legacy`) for a behaviour that
/// names no policy.
///
/// The reading is shared so every span this module takes off a behaviour is clamped the same way
/// and answers `None` for a span that is no time at all. A behaviour naming a cache policy this
/// account does not hold cannot be created, so a policy that can be read nowhere states no span.
fn held_span(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    behavior: &Behavior,
    stored: impl Fn(&crate::ops::types::CachePolicyConfig) -> i64,
    managed: impl Fn(&crate::state::ManagedCachePolicy) -> i64,
    legacy: impl Fn(&Behavior) -> Option<i64>,
) -> Option<Duration> {
    let seconds = match &behavior.cache_policy_id {
        Some(id) => match store.cache_policies.get(id) {
            Some(policy) => stored(&policy.config),
            None => managed_cache_policy(id).map_or(0, managed),
        },
        None => legacy(behavior).unwrap_or_default(),
    };
    let seconds = u64::try_from(seconds).unwrap_or_default();
    // Clamped to the longest TTL CloudFront's own cache policies carry, so a stored TTL
    // is a span of time rather than an instant the clock cannot reach: every published
    // managed policy's `MaxTTL` is this value ([`crate::state::MANAGED_CACHE_POLICIES`]),
    // and a configuration asking for more is held for this long.
    let seconds = seconds.min(MAX_CACHE_TTL_SECONDS);
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// What the edge does with one viewer request, decided from the stored configuration
/// alone.
///
/// Owned rather than borrowed: the decision is made under the store's lock and the origin
/// fetch happens after it is released.
#[derive(Debug)]
enum Plan {
    /// CloudFront answers this itself: a status it generated and the detail line its
    /// error page carries.
    Generated { status: u16, detail: &'static str },
    /// CloudFront refuses this itself because the behaviour restricts its content to
    /// trusted signers and this request's signed access did not hold ([`signed_access`]).
    Refused { refusal: signed_access::Refusal },
    /// CloudFront sends this viewer to the HTTPS url of what it asked for, the matched
    /// behaviour's `ViewerProtocolPolicy` being `redirect-to-https`.
    Redirect {
        /// The absolute `https` URL of the same request, which the redirect's `Location`
        /// carries.
        location: String,
    },
    /// The edge cache holds the answer.
    Hit {
        /// The ORIGIN's own answer, as the entry holds it.
        answer: OriginAnswer,
        /// The coding the edge compresses it with, when the behaviour asks for compression
        /// and this viewer accepts one ([`compression`]).
        encode: Option<Encoding>,
    },
    /// The origin must answer it.
    Fetch(Box<OriginFetch>),
}

/// One answer an ORIGIN produced, as the edge holds it and serves it: the status, the bytes,
/// and the headers the origin stated about them — the two the edge may have to restate per
/// request held on their own members, and the rest as the origin sent them.
///
/// What the edge serves FROM it — the content coding one viewer negotiated — is computed per
/// request rather than held ([`served_origin_answer`]), so one viewer's negotiation is never
/// served to another.
#[derive(Debug)]
struct OriginAnswer {
    status: u16,
    body: Vec<u8>,
    /// `Content-Type` as the origin stated it, or `None` when it stated none.
    content_type: Option<String>,
    /// `Content-Encoding` as the origin stated it — the coding its own bytes are under — or
    /// `None` for bytes the origin did not encode.
    content_encoding: Option<String>,
    /// Every OTHER response header the origin stated, lowercased and in the order it stated
    /// them: the edge serves the origin's own headers to the viewer, except the ones
    /// [`RESPONSE_HEADERS_NEVER_SERVED`] names.
    headers: Vec<(String, String)>,
    /// How long the edge cache has HELD this answer, for one it is serving out of that cache,
    /// and `None` for one the origin produced for the request being served — which is what
    /// decides whether the answer carries an [`AGE`] of this cache's own
    /// ([`served_origin_answer`]).
    age: Option<Duration>,
}

/// One origin fetch the edge is to make, and what to do with what comes back.
#[derive(Debug)]
struct OriginFetch {
    /// The distribution whose cache a cacheable answer is held in.
    distribution_id: String,
    /// The absolute URI of the origin request.
    uri: String,
    /// What the origin's ORIGIN ACCESS CONTROL makes of this fetch, or `None` for an origin
    /// whose configuration signs no request (see [`origin_access`]).
    access: Option<OriginAccess>,
    /// The headers the origin request carries beside its body's own.
    headers: Vec<(String, String)>,
    /// How long the edge waits for the origin, from `OriginReadTimeout`.
    read_timeout: Duration,
    /// The cache key and TTL a cacheable answer is held under, or `None` when this
    /// request's answer is not cached at all.
    cache: Option<CacheDirective>,
    /// The coding the edge compresses the origin's answer with, when the behaviour asks for
    /// compression and this viewer accepts one ([`compression`]).
    encode: Option<Encoding>,
    /// The viewer path, which an invalidation is matched against.
    viewer_path: String,
    /// `DistributionConfig$CustomErrorResponses`, which decide how long an answer that is an
    /// ERROR is held: `ErrorCachingMinTTL` rather than the behaviour's own TTL
    /// ([`custom_errors::caching_ttl`]).
    custom_error_responses: Vec<CustomErrorResponse>,
}

/// Where a cacheable answer is held, and for how long.
#[derive(Debug)]
struct CacheDirective {
    key: String,
    /// How long an answer the edge MAY cache is held, from the behaviour's cache policy, or
    /// `None` for a policy that holds no such answer at all — which still leaves an ERROR the
    /// distribution's custom error responses name to be held for its own span
    /// ([`custom_errors::caching_ttl`]).
    ttl: Option<Duration>,
    /// The behaviour's MINIMUM TTL ([`cache_min_ttl`]), which is the one span that holds content
    /// whose origin asked no cache to store it ([`cache_directives::held_for`]).
    min_ttl: Option<Duration>,
}

/// Serve one viewer request addressed to a distribution's edge.
///
/// The one serving path: the claim ([`claims_request`]) and this function read the same
/// addressing, so a request this service claimed is a request this function resolves.
pub async fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    let now = SystemTime::now();
    // When the edge RECEIVED this request, on the monotonic clock: the instant the origin
    // latency of an answer an origin produces is measured from ([`edge_metrics`]).
    let received = Instant::now();
    let Some(state) = crate::state::service_state(engine) else {
        return generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, "");
    };
    // WHICH functions this request runs, read in the same pass that plans it: a CloudFront
    // function runs on a request the edge is going to its cache or the origin for, so the
    // association and its `LIVE` code are read here and the code runs further down, once the
    // enforcement in front of the cache has had its say ([`functions`]).
    let cf_id;
    let edge_functions;
    let served_for;
    let mut plan;
    let realtime_log;
    let server_timing;
    // WHICH of a continuous deployment's two distributions serves this request, decided ONCE
    // here so everything below — the behaviour, its functions, the web ACL, the origin and the
    // series the answer is counted into — is the same distribution's
    // ([`continuous_deployment`]). Held across the re-plan a viewer-request function's rewrite
    // makes, so a rewrite cannot move a request from one distribution to the other.
    let staging;
    {
        let mut store = state.lock();
        // The edge reads the distribution as every other read does: through the clock,
        // which owes a just-created configuration its deployment.
        store.settle(now);
        cf_id = store.mint_edge_request_id();
        staging = continuous_deployment::staging_distribution(&mut store, req, now);
        edge_functions = associated_functions(&store, req, staging.as_deref());
        (served_for, plan) = plan_request(&store, req, &cf_id, now, staging.as_deref());
        // WHETHER this answer carries the response headers policy's `Server-Timing` header is
        // that policy's `SamplingRate` to decide, and it is decided HERE, over the sequence of
        // the responses the policy is applied to
        // (`crate::state::StoredResponseHeadersPolicy::adds_server_timing_to_next_response`) —
        // the header's own metrics are measured as the request is served, further down.
        server_timing = served_for
            .as_ref()
            .is_some_and(|served| store.adds_server_timing(&served.response_headers_policy_id));
        // WHETHER this request's log data is delivered at all is the matched behaviour's
        // real-time log configuration to decide, and it is decided HERE, over the sequence of
        // the requests that configuration covers ([`realtime_logs::sampled`]) — the record
        // itself is built from the answer, which is not served yet.
        realtime_log = served_for.as_ref().and_then(|served| {
            realtime_logs::sampled(&mut store, served.realtime_log_config_arn.as_deref())
        });
    }
    let method = req.method().to_ascii_uppercase();
    let request_bytes = req.body().bytes().map_or(0, <[u8]>::len);
    // The WAF web ACL the distribution's configuration puts in front of it decides the
    // request before anything the plan would do: "CloudFront responds to requests either
    // with the requested content or with an HTTP 403 status code (Forbidden)", so a request
    // it does not let through is answered here — before the edge cache is read and with no
    // origin request made — while the answer is still counted into the distribution's own
    // series, the edge having served it. Asked with THIS service's store released, since
    // the decision locks the WAFv2 service's own ([`super::web_acl`]).
    let (edge_refusal, logged) = match served_for.as_ref() {
        Some(served) => super::web_acl::refusal(
            engine,
            &served.web_acl_id,
            req,
            &served.viewer_path,
            &served.query,
        ),
        None => (None, None),
    };
    // What that web ACL LOGS about the request it has just decided, delivered to the
    // destination its own `LoggingConfiguration` names: WAF records the request it
    // inspected, so the record is delivered for a request it refused at the edge as much
    // as for one it let through to the cache.
    if let Some(logged) = logged.as_ref() {
        ::service_com_amazonaws_wafv2_awswaf_20190729::service::logs::deliver(ctx, logged).await;
    }
    // The viewer-REQUEST function, where the matched behaviour associates one, runs HERE: on a
    // request the edge is about to read its cache or ask the origin for, and so only on one
    // that got through everything CloudFront decides for itself in front of them — the
    // distribution being enabled, its geographic restriction, the behaviour's
    // `ViewerProtocolPolicy`, its `AllowedMethods`, its `TrustedKeyGroups` and the web ACL.
    // A refusal any of those already decided is what the viewer receives, and no function of
    // this distribution runs on it.
    //
    // What the function returns is what the edge continues with: the request it came back with
    // is re-planned, so the cache key and the origin fetch are that request's; a response it
    // generated itself is served with no origin request made at all; and a run that produced
    // neither leaves the edge nothing to continue from ([`functions`]).
    let mut rewritten = None;
    let mut function_answer = None;
    let running = edge_functions
        .as_ref()
        .filter(|_| edge_refusal.is_none() && matches!(plan, Plan::Hit { .. } | Plan::Fetch(_)))
        .and_then(|running| {
            running
                .associations
                .viewer_request
                .as_ref()
                .map(|associated| (running, associated))
        });
    if let Some((running, associated)) = running {
        let event = function_event(running, req, &cf_id, &running.viewer_path, &running.query);
        match functions::run_viewer_request(associated, &event).await {
            functions::Rewritten::Request(request) => {
                match rewritten_request(req, &running.viewer_path, &request) {
                    Some(request) => {
                        // Planned again, for the request the function returned: what the edge
                        // keys on and asks the origin for is that request, and the enforcement
                        // in front of the cache is applied to it as it was to the viewer's own.
                        let store = state.lock();
                        (_, plan) = plan_request(&store, &request, &cf_id, now, staging.as_deref());
                        drop(store);
                        rewritten = Some(request);
                    }
                    None => function_answer = Some(refused_by_function(&cf_id)),
                }
            }
            functions::Rewritten::Response(response) => {
                function_answer = Some(answer_of_function(response, &cf_id));
            }
            functions::Rewritten::Failed => {
                function_answer = Some(refused_by_function(&cf_id));
            }
        }
    }
    // The LAMBDA@EDGE function the matched behaviour associates on the viewer-request event is
    // INVOKED here, in the place its event is raised: the same point of the request the
    // CloudFront function's own event is, and under the same enforcement in front of it. What
    // the function returns is what the edge continues with, by the same reading: the request it
    // came back with is re-planned, a response it generated is served with no origin request
    // made at all, and a run that produced neither leaves the edge nothing to continue from
    // ([`lambda_edge`]).
    //
    // One event of one behaviour runs ONE function: a cache behaviour cannot associate both a
    // CloudFront function and a Lambda@Edge function on the same event type, so where a stored
    // configuration carries both, what the CloudFront function above already decided for this
    // event stands.
    let running = edge_functions
        .as_ref()
        .filter(|_| {
            edge_refusal.is_none()
                && function_answer.is_none()
                && rewritten.is_none()
                && matches!(plan, Plan::Hit { .. } | Plan::Fetch(_))
        })
        .and_then(|running| {
            running
                .lambda
                .viewer_request
                .as_ref()
                .map(|associated| (running, associated))
        });
    if let Some((running, associated)) = running {
        let event = function_event(running, req, &cf_id, &running.viewer_path, &running.query);
        let body = req.body().bytes().map(<[u8]>::to_vec).unwrap_or_default();
        match lambda_edge::run_viewer_request(ctx, associated, &event, &body).await {
            functions::Rewritten::Request(request) => {
                match rewritten_request(req, &running.viewer_path, &request) {
                    Some(request) => {
                        // Planned again, for the request the function returned, exactly as a
                        // CloudFront function's rewrite is.
                        let store = state.lock();
                        (_, plan) = plan_request(&store, &request, &cf_id, now, staging.as_deref());
                        drop(store);
                        rewritten = Some(request);
                    }
                    None => function_answer = Some(refused_by_edge_function(&cf_id)),
                }
            }
            functions::Rewritten::Response(response) => {
                function_answer = Some(answer_of_edge_function(response, &cf_id));
            }
            functions::Rewritten::Failed => {
                function_answer = Some(refused_by_edge_function(&cf_id));
            }
        }
    }
    // From here on the request is the one that function returned, where it returned one: the
    // cache key, the origin fetch and the log record are all that request's.
    let req = rewritten.as_ref().unwrap_or(req);
    let (mut response, served_from, origin_latency) = match (edge_refusal, function_answer) {
        (Some(refusal), _) => (
            refused_by_web_acl(refusal, &cf_id),
            edge_metrics::ServedFrom::CloudFront,
            None,
        ),
        // The answer a viewer-request function produced, or the page CloudFront generated
        // because that function produced nothing: CloudFront's own either way.
        (None, Some(answer)) => (answer, edge_metrics::ServedFrom::CloudFront, None),
        (None, None) => match plan {
            Plan::Generated { status, detail } => (
                generated(status, detail, &cf_id),
                edge_metrics::ServedFrom::CloudFront,
                None,
            ),
            Plan::Refused { refusal } => (
                refused_signed_access(refusal, &cf_id),
                edge_metrics::ServedFrom::CloudFront,
                None,
            ),
            Plan::Redirect { location } => (
                redirected(&location, &cf_id),
                edge_metrics::ServedFrom::CloudFront,
                None,
            ),
            Plan::Hit { answer, encode } => (
                served_origin_answer(
                    &answer,
                    encode,
                    cache_outcome(answer.status, CACHE_HIT),
                    &cf_id,
                ),
                edge_metrics::ServedFrom::EdgeCache,
                None,
            ),
            Plan::Fetch(fetch) => {
                let fetched = fetch_origin(engine, ctx, *fetch, &cf_id, req, received).await;
                // An origin that answered is what the answer came FROM; one that answered
                // nothing leaves CloudFront to produce the answer itself, which is no
                // origin's and has no origin latency.
                let served_from = if fetched.origin_latency.is_some() {
                    edge_metrics::ServedFrom::Origin
                } else {
                    edge_metrics::ServedFrom::CloudFront
                };
                (fetched.response, served_from, fetched.origin_latency)
            }
        },
    };
    // The viewer's CONDITIONAL headers, judged by the edge over the representation it is about
    // to serve — the entry its cache held, or the answer the origin has just produced: a viewer
    // whose copy is still the current one is told so and sent none of the object's bytes
    // ([`conditional`]). Judged BEFORE the range, which a viewer holding the current copy has no
    // need of, and only on an answer that IS the object, so a page CloudFront generated itself
    // is left as it is.
    if matches!(
        served_from,
        edge_metrics::ServedFrom::Origin | edge_metrics::ServedFrom::EdgeCache
    ) {
        conditional::apply(req, &method, now, &mut response);
    }
    // The viewer's byte RANGE, evaluated by the edge over the representation it is about to
    // serve — the entry its cache held, or the answer the origin has just produced: the window
    // the viewer selected is answered with those bytes alone, and a range that selects none of
    // the object is refused rather than widened into the whole object ([`byte_range`]). Only an
    // answer that IS the object has a window to take, so a page CloudFront generated itself is
    // left as it is.
    if matches!(
        served_from,
        edge_metrics::ServedFrom::Origin | edge_metrics::ServedFrom::EdgeCache
    ) {
        byte_range::apply(req.headers().get(byte_range::RANGE), &method, &mut response);
    }
    // The CUSTOM ERROR RESPONSE the distribution states for the status this answer carries,
    // where it states one: the viewer receives the configured `ResponseCode` and the page
    // `ResponsePagePath` names instead of the error ([`custom_errors`]). Applied to an answer
    // the edge reached on an ORIGIN's behalf — the origin's own, or the same answer out of the
    // cache — because the status it is keyed on is the origin's; a page CloudFront generated
    // itself for a request it never passed to an origin is its own refusal of this viewer.
    if let Some(served) = served_for.as_ref().filter(|_| {
        matches!(
            served_from,
            edge_metrics::ServedFrom::Origin | edge_metrics::ServedFrom::EdgeCache
        )
    }) {
        if let Some(configured) =
            custom_errors::configured(&served.custom_error_responses, response.status().as_u16())
        {
            custom_errors::apply(
                engine,
                ctx,
                &custom_errors::Serving {
                    req,
                    cf_id: &cf_id,
                    viewer_path: &served.viewer_path,
                    now,
                    received,
                    staging: staging.as_deref(),
                },
                &configured,
                &mut response,
            )
            .await;
        }
    }
    // The header groups of the response headers policy the matched behaviour attaches, put on
    // the answer CloudFront is about to serve — whoever produced it, since the policy is the
    // distribution's statement about the RESPONSE rather than about the origin
    // ([`response_headers`]). Applied before the request is counted and logged, so what those
    // report is the answer the viewer actually gets.
    if let Some(policy) = served_for
        .as_ref()
        .and_then(|served| served.response_headers_policy.as_ref())
    {
        response_headers::apply(
            policy,
            response_headers::ViewerCors::of_request(req),
            &mut response,
        );
    }
    // And the `Server-Timing` group of that policy, for a response its sampling selected: the
    // metrics are what the edge MEASURED serving this request — which layer answered it,
    // whether the cache held the object, the origin's first-byte latency and the edge's own —
    // so they are read off the serving path here rather than stated by the configuration
    // ([`response_headers::ServerTiming`]).
    if server_timing {
        response_headers::apply_server_timing(
            &response_headers::ServerTiming {
                request_id: &cf_id,
                cache_hit: match served_from {
                    edge_metrics::ServedFrom::EdgeCache => Some(true),
                    edge_metrics::ServedFrom::Origin => Some(false),
                    // A page CloudFront generated itself is no object of an origin's, so the
                    // edge cache neither held it nor missed it.
                    edge_metrics::ServedFrom::CloudFront => None,
                },
                origin_latency,
                taken: received.elapsed(),
            },
            &mut response,
        );
    }
    // The viewer-RESPONSE function, where the matched behaviour associates one: it is handed
    // the answer the edge is about to serve and the answer it returns is the one the viewer
    // gets, so it runs last — after everything else that touches the response — which is what
    // the viewer-response event is. It runs on an answer an ORIGIN produced or the cache held,
    // including an origin's own error: a page CloudFront generated itself is not an answer
    // this event is raised for, and neither is a response a viewer-request function generated.
    if let Some(associated) = edge_functions
        .as_ref()
        .filter(|_| {
            matches!(
                served_from,
                edge_metrics::ServedFrom::Origin | edge_metrics::ServedFrom::EdgeCache
            )
        })
        .and_then(|running| {
            running
                .associations
                .viewer_response
                .as_ref()
                .map(|associated| (running, associated))
        })
    {
        let (running, associated) = associated;
        let (viewer_path, query) = match served_for.as_ref() {
            Some(served) => (served.viewer_path.as_str(), served.query.as_str()),
            None => (running.viewer_path.as_str(), running.query.as_str()),
        };
        let event = function_event(running, req, &cf_id, viewer_path, query);
        response = match functions::run_viewer_response(associated, &event, &response).await {
            Some(answered) => answered,
            None => refused_by_function(&cf_id),
        };
    }
    // The LAMBDA@EDGE function the matched behaviour associates on the viewer-response event,
    // invoked where that event is raised: on the answer the edge is about to serve, last of
    // everything that touches it, and only on an answer an ORIGIN produced or the cache held —
    // a page CloudFront generated itself and a response a viewer-request function generated
    // are not answers this event is raised for. One event runs one function, for the reason
    // the viewer-request event above states.
    if let Some(associated) = edge_functions
        .as_ref()
        .filter(|running| {
            running.associations.viewer_response.is_none()
                && matches!(
                    served_from,
                    edge_metrics::ServedFrom::Origin | edge_metrics::ServedFrom::EdgeCache
                )
        })
        .and_then(|running| {
            running
                .lambda
                .viewer_response
                .as_ref()
                .map(|associated| (running, associated))
        })
    {
        let (running, associated) = associated;
        let (viewer_path, query) = match served_for.as_ref() {
            Some(served) => (served.viewer_path.as_str(), served.query.as_str()),
            None => (running.viewer_path.as_str(), running.query.as_str()),
        };
        let event = function_event(running, req, &cf_id, viewer_path, query);
        response = match lambda_edge::run_viewer_response(ctx, associated, &event, &response).await
        {
            Some(answered) => answered,
            None => refused_by_edge_function(&cf_id),
        };
    }
    // What the edge DID with this request is reported as the distribution's own CloudWatch
    // series, whoever produced the answer: the counters and error rates a client watches a
    // distribution with are the viewer requests it served ([`edge_metrics`]). The series the
    // distribution's monitoring subscription adds to those are published beside them where
    // that subscription enables them, and for no other distribution.
    if let Some(served) = served_for {
        edge_metrics::count_viewer_request(
            engine,
            &served.id,
            now,
            &edge_metrics::ServedRequest {
                method: &method,
                status: response.status().as_u16(),
                response_bytes: response.body().bytes().map_or(0, <[u8]>::len),
                request_bytes,
                served_from,
                origin_latency,
            },
            served.additional_metrics,
        );
        // And what the edge did with it is ALSO what the LOG RECORD of this request states, on
        // either log plane: one request the edge served, read once ([`realtime_logs`]) so the
        // real-time record and the standard access-log record of it cannot disagree.
        //
        // The span the edge spent on the request, measured once, and the instant it finished
        // responding read from the SAME pair of clock readings the rest of this request is
        // reported by: `now` is when the edge received it, so the two values a record carries
        // cannot describe different requests.
        let taken = received.elapsed();
        let logged = realtime_logs::LoggedRequest {
            req,
            at: now + taken,
            taken,
            origin_latency,
            status: response.status().as_u16(),
            response_bytes: response.body().bytes().map_or(0, <[u8]>::len),
            content_type: response
                .headers()
                .get(CONTENT_TYPE),
            distribution_domain: &served.domain_name,
            viewer_host: &served.viewer_host,
            viewer_path: &served.viewer_path,
            query: &served.query,
            country: &served.country,
            request_id: &cf_id,
            path_pattern: served.path_pattern.as_deref(),
            primary: served
                .primary
                .as_ref()
                .map(|(id, domain_name)| realtime_logs::PrimaryDistribution {
                    id,
                    domain_name,
                }),
            result_type: realtime_logs::ResultType::of_answer(
                served_from,
                response.status().as_u16(),
            ),
        };
        // The REAL-TIME record goes to the data streams of the matched cache behaviour's
        // real-time log configuration, for a request that configuration covers ([`realtime_logs`]).
        // Delivered with this service's store released, since the hop locks the stores of the
        // services it asks.
        if let Some(configured) = realtime_log {
            let record = logged.record(&configured.fields);
            realtime_logs::deliver(ctx, &configured, &record, &cf_id).await;
        }
        // The STANDARD LOGGING v2 record goes to the log group each vended-log delivery of this
        // distribution's access logs names ([`access_logs`]), whichever cache behaviour matched:
        // standard logging is configured on the distribution as a whole, in CloudWatch Logs.
        access_logs::deliver(engine, ctx, &served.id, &logged).await;
        // And the STANDARD access-log FILE goes into the Amazon S3 bucket the distribution's own
        // `Logging` configuration names, for a distribution that runs under one
        // ([`standard_logs`]) — the other way a client has a distribution's access logs
        // delivered, configured on CloudFront itself. Delivered with this service's store
        // released, since the write locks the object store's.
        if let Some(destination) = &served.standard_logging {
            standard_logs::deliver(ctx, &served.id, destination, &logged).await;
        }
    }
    response
}

/// The URLs one viewer request addresses, which a signed URL's policy `Resource` is matched
/// against ([`signed_access`]).
///
/// "The base URL, including the query string, if any" — built from the names the distribution
/// answers at: its own `DomainName`, "the domain name that CloudFront assigns when you create
/// a distribution", and each of its `Aliases`, which are "the CNAMEs, if any, for this
/// distribution". A policy may be written for any name a viewer reaches the distribution by,
/// and the viewer's own `Host` is not among the candidates because the request has already
/// been resolved to THIS distribution: whichever of its names was dialled, the policy has to
/// name one of them.
///
/// The CloudFront signature parameters are not part of the URL a policy was signed for — they
/// are what carries the signature — so they are dropped from the query string here.
fn resource_urls(
    distribution: &StoredDistribution,
    scheme: &str,
    viewer_path: &str,
    query: &str,
) -> Vec<String> {
    let signed_query = signed_access::resource_query(query);
    let suffix = if signed_query.is_empty() {
        viewer_path.to_string()
    } else {
        format!("{viewer_path}?{signed_query}")
    };
    ::std::iter::once(&distribution.domain_name)
        .chain(
            distribution
                .config
                .aliases
                .as_ref()
                .and_then(|aliases| aliases.items.as_ref())
                .into_iter()
                .flatten(),
        )
        .map(|host| format!("{scheme}://{host}{suffix}"))
        .collect()
}

/// The answer the edge serves for a viewer request whose SIGNED ACCESS did not hold: `403`
/// under the error code and sentence CloudFront states for that failure, carrying no origin
/// bytes and reported as an answer CloudFront produced itself.
fn refused_signed_access(refusal: signed_access::Refusal, cf_id: &str) -> RtResponse {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Error><Code>{}</Code><Message>{}</Message></Error>",
        refusal.code(),
        refusal.message()
    );
    let mut response = served(FORBIDDEN, body.into_bytes(), CACHE_ERROR, cf_id);
    let _ = response
        .headers_mut()
        .try_insert(CONTENT_TYPE, "text/xml".to_string());
    let _ = response
        .headers_mut()
        .try_insert("server", SERVER.to_string());
    response
}

/// The answer the edge serves for a viewer request the distribution's web ACL did not let
/// through: the status, headers and body WAF decided, under the edge's own request id and
/// reported as an answer CloudFront produced itself rather than one an origin did.
fn refused_by_web_acl(refusal: super::web_acl::Refusal, cf_id: &str) -> RtResponse {
    let mut response = served(
        refusal.status,
        refusal.body,
        CACHE_ERROR,
        cf_id,
    );
    if let Some(content_type) = refusal.content_type {
        let _ = response
            .headers_mut()
            .try_insert("content-type", content_type);
    }
    for (name, value) in refusal.headers {
        let _ = response.headers_mut().try_append(name, value);
    }
    response
}

/// The distribution one viewer request resolved to, and everything about it the edge still
/// needs once the store's lock is released: the id its series is counted under, whether its
/// monitoring subscription enables the additional series, the web ACL its configuration puts
/// in front of it, and the viewer path and query a rule inspects.
#[derive(Debug)]
struct ServedFor {
    id: String,
    /// What this distribution's monitoring subscription says about its ADDITIONAL CloudWatch
    /// series ([`edge_metrics::AdditionalMetrics`]).
    additional_metrics: edge_metrics::AdditionalMetrics,
    /// `DistributionConfig$WebACLId` as the stored configuration carries it — the empty
    /// string for a distribution that names none.
    web_acl_id: String,
    viewer_path: String,
    query: String,
    /// `Distribution$DomainName`, which is the name a record reports as the distribution's
    /// own ([`realtime_logs`]).
    domain_name: String,
    /// The PRIMARY distribution this request was addressed to — its id and its `DomainName` —
    /// when a continuous deployment policy routed it to that distribution's staging
    /// distribution, and `None` for a request the distribution it addressed served itself
    /// ([`continuous_deployment`]).
    primary: Option<(String, String)>,
    /// The host the VIEWER addressed this distribution by ([`viewer_host`]): a name the
    /// distribution is served on, which is what a record reports as the viewer's `Host`.
    viewer_host: String,
    /// The country the edge resolved for this viewer, read once and reported wherever a
    /// country is.
    country: String,
    /// The `PathPattern` of the cache behaviour that matched, or `None` for the default
    /// behaviour.
    path_pattern: Option<String>,
    /// `RealtimeLogConfigArn` of the behaviour that matched — the configuration this
    /// request's log record goes through, when the behaviour names one.
    realtime_log_config_arn: Option<String>,
    /// The configuration of the response headers policy the matched behaviour attaches, read
    /// under the store's lock, or `None` when the behaviour attaches none (or names one this
    /// account no longer holds). Its header groups are applied to whatever the edge serves
    /// ([`response_headers`]).
    response_headers_policy: Option<ResponseHeadersPolicyConfig>,
    /// `ResponseHeadersPolicyId` of the behaviour that matched — the empty string for a
    /// behaviour that attaches none. The policy's own `Server-Timing` sampling is counted
    /// against the RECORD it names, so the id travels beside the configuration read off it.
    response_headers_policy_id: String,
    /// `DistributionConfig$CustomErrorResponses` as the stored configuration lists them: what
    /// the viewer receives when the answer the edge reached is an error ([`custom_errors`]).
    /// Stated on the DISTRIBUTION rather than on a behaviour, so it covers every behaviour's
    /// answers.
    custom_error_responses: Vec<CustomErrorResponse>,
    /// WHERE this distribution's standard access logs are delivered, from the `Logging`
    /// configuration it runs under, or `None` for a distribution that enables none
    /// ([`standard_logs`]). Stated on the DISTRIBUTION, so every behaviour's answers are
    /// logged into the one bucket.
    standard_logging: Option<standard_logs::Destination>,
}

/// The distribution one viewer request resolved to and the FUNCTIONS its matched cache
/// behaviour associates — the CloudFront functions it runs itself and the Lambda@Edge
/// functions it invokes — read in one pass under the store's lock before the edge does
/// anything else with the request.
///
/// Held only for a request whose behaviour really associates a function: a behaviour that
/// associates none costs this nothing beyond the resolution the plan makes anyway.
#[derive(Debug)]
struct EdgeFunctions {
    /// `context.distributionId` of the event object.
    distribution_id: String,
    /// `context.distributionDomainName` of the event object — the name CloudFront assigned
    /// the distribution, whichever of its names the viewer dialled.
    domain_name: String,
    /// The viewer path WITHIN the distribution, which is the `request.uri` the event states.
    viewer_path: String,
    /// The query string as the viewer sent it, which the event states as `request.querystring`.
    query: String,
    /// The functions the behaviour associates, by the event each runs on.
    associations: functions::Associations,
    /// The LAMBDA@EDGE functions the behaviour associates, by the viewer event each is invoked
    /// on ([`lambda_edge`]).
    lambda: lambda_edge::Associations,
}

/// The functions the behaviour this request's path selects associates, or `None` when the
/// request resolves to no distribution of this account or its behaviour associates none.
///
/// Read off the distribution that SERVES the request, which `staging` names when a continuous
/// deployment policy routed it to a staging distribution: the functions a request runs are the
/// serving distribution's own ([`continuous_deployment`]).
fn associated_functions(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    req: &RtRequest,
    staging: Option<&str>,
) -> Option<EdgeFunctions> {
    let addressed = Addressed::of_request(req);
    let resolved = continuous_deployment::serving(store, resolve(store, &addressed)?, staging);
    // The SAME behaviour the plan is decided under: "CloudFront compares the path in the
    // viewer request with the path patterns in the cache behaviors", and the path it compares
    // is the one the viewer asked for — so which function runs is the viewer's own path to
    // decide, whatever that function then rewrites.
    let behavior = select_behavior(resolved.distribution, resolved.viewer_path);
    let associations = functions::associations(store, behavior.function_associations.as_ref());
    let lambda = lambda_edge::associations(behavior.lambda_function_associations.as_ref());
    (!associations.is_empty() || !lambda.is_empty()).then(|| EdgeFunctions {
        distribution_id: resolved.distribution.id.clone(),
        domain_name: resolved.distribution.domain_name.clone(),
        viewer_path: resolved.viewer_path.to_string(),
        query: addressed.query.to_string(),
        associations,
        lambda,
    })
}

/// The event object one of this request's functions is handed: what the edge resolved about
/// the request, under the identifier of the request it is serving.
fn function_event<'r>(
    running: &'r EdgeFunctions,
    req: &'r RtRequest,
    cf_id: &'r str,
    viewer_path: &'r str,
    query: &'r str,
) -> functions::Event<'r> {
    functions::Event::of_request(
        &running.distribution_id,
        &running.domain_name,
        cf_id,
        // The ONE address this request has, read where every other reader of it reads it
        // ([`generated_headers::viewer_address`]).
        generated_headers::viewer_address(req),
        req,
        viewer_path,
        query,
    )
}

/// The request the edge plans after a viewer-request function rewrote it: the same request,
/// under the URI, the query string and the headers that function returned.
///
/// The URI a function returns is the viewer path WITHIN the distribution, so it replaces
/// exactly that part of the request's own URI: the scheme and authority the viewer dialled and
/// the addressing that named the distribution belong to the connection rather than to the
/// function, and are kept as they arrived.
///
/// `None` is a rewrite that renders no HTTP request at all — a URI or a header name the wire
/// cannot carry — which leaves the edge no request to serve.
fn rewritten_request(
    req: &RtRequest,
    viewer_path: &str,
    rewrite: &functions::Request,
) -> Option<RtRequest> {
    let uri = req.uri();
    let addressing = path_of(uri).strip_suffix(viewer_path).unwrap_or_default();
    // "The request URI must begin with a forward slash": a URI a function returned without one
    // names the same object at the root of the distribution.
    let viewer_uri = if rewrite.uri.starts_with('/') {
        rewrite.uri.clone()
    } else {
        format!("/{}", rewrite.uri)
    };
    let mut rebuilt = format!("{}{addressing}{viewer_uri}", uri_prefix(uri));
    if !rewrite.query.is_empty() {
        rebuilt.push('?');
        rebuilt.push_str(&rewrite.query);
    }
    let body = req.body().bytes().map(<[u8]>::to_vec).unwrap_or_default();
    // The METHOD is the viewer's: a CloudFront function does not choose the method of the
    // request it was handed.
    let mut builder = ::http::Request::builder()
        .method(req.method())
        .uri(&rebuilt);
    for (name, value) in &rewrite.headers {
        builder = builder.header(name, value);
    }
    let built = builder
        .body(SdkBody::from(::bytes::Bytes::from(body)))
        .map_err(|error| error.to_string())
        .and_then(|request| RtRequest::try_from(request).map_err(|error| error.to_string()));
    match built {
        Ok(request) => Some(request),
        Err(error) => {
            ::tracing::warn!(
                uri = %rebuilt,
                %error,
                "the request this cache behaviour's viewer-request function returned does not \
                 render an HTTP request, so the edge has no request to serve"
            );
            None
        }
    }
}

/// The `scheme://authority` an ABSOLUTE request URI begins with, or the empty string for an
/// origin-form one — the part of a request's URI that belongs to the connection it arrived on.
fn uri_prefix(uri: &str) -> &str {
    match uri.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.find('/').unwrap_or(rest.len());
            &uri[..scheme.len() + "://".len() + authority]
        }
        None => "",
    }
}

/// The answer the edge serves for a response a viewer-request function GENERATED: the status,
/// the headers and the bytes that function stated, reported as an answer CloudFront produced
/// rather than one an origin did or the cache held.
fn answer_of_function(answer: functions::Generated, cf_id: &str) -> RtResponse {
    let mut response = served(
        answer.status,
        answer.body,
        CACHE_FUNCTION_GENERATED,
        cf_id,
    );
    for (name, value) in answer.headers {
        let _ = response.headers_mut().try_append(name, value);
    }
    response
}

/// The answer the edge serves for a response a LAMBDA@EDGE viewer-request function GENERATED:
/// the status, the headers and the bytes that function stated, reported under the cache
/// outcome that names the Lambda@Edge function as what produced it
/// ([`CACHE_LAMBDA_GENERATED`]).
fn answer_of_edge_function(answer: functions::Generated, cf_id: &str) -> RtResponse {
    let mut response = served(answer.status, answer.body, CACHE_LAMBDA_GENERATED, cf_id);
    for (name, value) in answer.headers {
        let _ = response.headers_mut().try_append(name, value);
    }
    response
}

/// Decide what the edge does with one viewer request, under the store's lock, and name the
/// distribution the request resolved to — the one whose series it is counted into.
///
/// `staging` is the staging distribution a continuous deployment policy routed this request
/// to, when one did: everything the plan reads — the behaviour, the cache, the origins and the
/// web ACL — is then that distribution's ([`continuous_deployment`]).
fn plan_request(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    req: &RtRequest,
    cf_id: &str,
    now: SystemTime,
    staging: Option<&str>,
) -> (Option<ServedFor>, Plan) {
    let addressed = Addressed::of_request(req);
    let Some(resolved) = resolve(store, &addressed) else {
        // The distribution was deleted between the claim and now: an endpoint that serves
        // no distribution of this account is refused, and counted into no distribution's
        // series, there being none to count it into.
        return (
            None,
            Plan::Generated {
                status: FORBIDDEN,
                detail: BAD_REQUEST_DETAIL,
            },
        );
    };
    let resolved = continuous_deployment::serving(store, resolved, staging);
    let behavior = select_behavior(resolved.distribution, resolved.viewer_path);
    let served_for = ServedFor {
        id: resolved.distribution.id.clone(),
        additional_metrics: edge_metrics::AdditionalMetrics::of_subscription(
            resolved.distribution.monitoring_subscription.as_ref(),
        ),
        web_acl_id: resolved
            .distribution
            .config
            .web_acl_id
            .clone()
            .unwrap_or_default(),
        viewer_path: resolved.viewer_path.to_string(),
        query: addressed.query.to_string(),
        domain_name: resolved.distribution.domain_name.clone(),
        // The distribution the viewer ADDRESSED, when that is not the one serving the request:
        // a continuous deployment policy routed this request to the staging distribution, and
        // the primary it was addressed to is what the record's primary-distribution fields
        // report ([`realtime_logs::PrimaryDistribution`]).
        primary: (resolved.addressed.id != resolved.distribution.id).then(|| {
            (
                resolved.addressed.id.clone(),
                resolved.addressed.domain_name.clone(),
            )
        }),
        // The name the viewer reached this distribution by, read ONCE here: a request that
        // arrived through the reserved data-plane path carries this service's own host rather
        // than a name of the distribution's, and the name such a viewer addressed the
        // distribution by is the one it publishes ([`viewer_host`]).
        viewer_host: viewer_host(&addressed, &resolved).to_string(),
        country: viewer_country(req),
        path_pattern: behavior.path_pattern.clone(),
        realtime_log_config_arn: behavior.realtime_log_config_arn.clone(),
        // Resolved HERE, under the lock the whole plan is decided under, so the policy
        // applied to the answer is the one the behaviour attached when the request was
        // planned — a read of a policy AWS published included
        // (`readable_response_headers_policy`).
        response_headers_policy: behavior
            .response_headers_policy_id
            .as_deref()
            .and_then(|id| store.readable_response_headers_policy(id))
            .map(|policy| policy.config.clone()),
        response_headers_policy_id: behavior
            .response_headers_policy_id
            .clone()
            .unwrap_or_default(),
        custom_error_responses: custom_errors::of_config(&resolved.distribution.config),
        standard_logging: standard_logs::Destination::of_logging(
            resolved.distribution.config.logging.as_ref(),
        ),
    };
    let plan = plan_resolved(store, &addressed, &resolved, req, cf_id, now, behavior);
    (Some(served_for), plan)
}

/// Decide what the edge does with one viewer request it has resolved to a distribution,
/// under the cache behaviour its path selected.
fn plan_resolved(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    addressed: &Addressed<'_>,
    resolved: &Resolved<'_, '_>,
    req: &RtRequest,
    cf_id: &str,
    now: SystemTime,
    behavior: Behavior,
) -> Plan {
    let distribution_id = resolved.distribution.id.clone();
    let viewer_path = resolved.viewer_path.to_string();
    if !resolved.distribution.config.enabled {
        // `DistributionConfig$Enabled`: "Whether the distribution is enabled to accept
        // user requests for content." A disabled distribution accepts none.
        return Plan::Generated {
            status: FORBIDDEN,
            detail: BAD_REQUEST_DETAIL,
        };
    }
    // Enforced HERE, by CloudFront, before the edge cache is read and before any origin
    // request exists: a viewer the distribution's geographic restriction refuses sees
    // CloudFront's own page and nothing of the origin — and nothing of a cached answer
    // either, so a restriction cannot be stepped around by asking for an object another
    // viewer already fetched.
    let country = viewer_country(req);
    if geo_restricts(resolved.distribution, &country) {
        ::tracing::debug!(
            distribution = %distribution_id,
            "this distribution's geographic restriction refuses this viewer's country; the \
             edge refuses the request and makes no origin request"
        );
        return Plan::Generated {
            status: FORBIDDEN,
            detail: GEO_RESTRICTED_DETAIL,
        };
    }
    // The behaviour the viewer's path selected, chosen once by the caller so the record this
    // request's log configuration is read off ([`realtime_logs`]) and the plan served here
    // are the SAME behaviour's.
    // Decided by CloudFront from the matched behaviour's `ViewerProtocolPolicy` alone,
    // before the cache is read and before any origin request exists: the SCHEME the viewer
    // dialled is a property of the connection, not of what it asked for.
    if let Some(plan) = viewer_protocol_plan(&behavior, addressed, resolved, req) {
        return plan;
    }
    let method = req.method().to_ascii_uppercase();
    if !behavior.allows(&method) {
        // Enforced HERE, by CloudFront, before any origin request exists: a method the
        // behaviour does not allow never reaches the origin.
        ::tracing::debug!(
            distribution = %distribution_id,
            path_pattern = ?behavior.path_pattern,
            %method,
            "the cache behavior does not allow this method; the edge refuses it and makes \
             no origin request"
        );
        return Plan::Generated {
            status: FORBIDDEN,
            detail: METHOD_NOT_ALLOWED_DETAIL,
        };
    }
    // Enforced HERE, by CloudFront, before the edge cache is read and before any origin
    // request exists: "when a cache behavior contains trusted key groups, CloudFront
    // requires signed URLs or signed cookies for all requests that match the cache
    // behavior". The groups and the public keys in them are read from the store on THIS
    // request, so a key an update has taken out of the group stops admitting the URLs it
    // signed; and because the decision is made before the cache, a signature that does not
    // hold cannot be answered with an object another viewer already fetched.
    if let Some(refusal) = signed_access::refusal(
        store,
        behavior.trusted_key_groups.as_ref(),
        &signed_access::SignedRequest {
            query: addressed.query,
            cookies: req.headers().get(COOKIE).unwrap_or_default(),
            resource_urls: resource_urls(
                resolved.addressed,
                generated_headers::viewer_scheme(req.uri()),
                &viewer_path,
                addressed.query,
            ),
            address: generated_headers::viewer_address(req),
        },
        now,
    ) {
        ::tracing::debug!(
            distribution = %distribution_id,
            path_pattern = ?behavior.path_pattern,
            code = refusal.code(),
            "this behaviour restricts its content to trusted signers and this request's \
             signed access did not hold; the edge refuses it and makes no origin request"
        );
        return Plan::Refused { refusal };
    }
    let ttl = cache_ttl(store, &behavior);
    let min_ttl = cache_min_ttl(store, &behavior);
    // What the behaviour's POLICIES make of this request: which of its parts take part in
    // the cache key, which of them reach the origin, and what the cache policy makes of the
    // viewer's `Accept-Encoding` ([`forwarding`]).
    let keyed = forwarding::cache_key_parameters(store, &behavior);
    let forwarded = forwarding::forwarded_parameters(store, &behavior);
    let accept_encoding =
        forwarding::accept_encoding(&keyed, req.headers().get(ACCEPT_ENCODING));
    // "Whether you want CloudFront to automatically compress certain files for this cache
    // behavior": the coding this viewer gets, or none when the behaviour asks for no
    // compression or the viewer accepts neither coding the cache policy enables.
    let encode = if behavior.compress {
        accept_encoding.preferred()
    } else {
        None
    };
    // The `CloudFront-*` headers the EDGE generates for this request: their values are the
    // edge's own wherever one of those headers is read, the cache key included
    // ([`keyed_values`]).
    let generated = generated_headers::Generated::of_request(req);
    let cache_key = cache_key(
        &method,
        &viewer_path,
        &keyed_values(&keyed, &accept_encoding, addressed.query, req, &generated),
    );
    // "The HTTP methods for which CloudFront caches responses": an answer to a cacheable
    // method may come out of the edge cache and may be held in it, and everything else goes to
    // the origin every time. WHETHER one is held, and for how long, is decided where the answer
    // is: the cache policy's TTL for an answer the edge may cache, and `ErrorCachingMinTTL` for
    // an ERROR the distribution's custom error responses name ([`custom_errors`]) — which a
    // policy holding nothing does not take away, so a cacheable method still reads the one
    // place such an error is held.
    let cacheable = behavior.caches(&method);
    if cacheable {
        if let Some(entry) = resolved.distribution.cached_edge_response(&cache_key, now) {
            return Plan::Hit {
                answer: OriginAnswer {
                    status: entry.status,
                    body: entry.body.clone(),
                    content_type: entry.content_type.clone(),
                    content_encoding: entry.content_encoding.clone(),
                    headers: entry.headers.clone(),
                    // How long this entry has been resident, from the instant the origin
                    // fetch filled it: what the answer's [`AGE`] is counted from. A clock
                    // that went backwards between the two leaves no span, which reads as an
                    // entry filled now.
                    age: Some(now.duration_since(entry.stored_at).unwrap_or_default()),
                },
                encode,
            };
        }
    }
    let Some(origin) = resolved
        .distribution
        .config
        .origins
        .items
        .iter()
        .find(|origin| origin.id == behavior.target_origin_id)
    else {
        // A behaviour whose `TargetOriginId` names no origin cannot be created, so this
        // is a configuration that lost its origin: there is nothing to fetch.
        return Plan::Generated {
            status: BAD_GATEWAY,
            detail: NO_ORIGIN_DETAIL,
        };
    };
    let headers = origin_request_headers(
        store,
        &behavior,
        origin,
        req,
        cf_id,
        // The host the VIEWER addressed, which is what a behaviour forwarding the viewer's
        // own `Host` sends the origin ([`origin_request_host`]).
        viewer_host(addressed, resolved),
    );
    // A REQUEST LOOP: the origin is a host the edge itself serves, or this request has
    // already been through an edge (its `Via` names CloudFront). Either way, fetching it
    // would hand the request back to the edge it came from, so the fetch is not made and
    // the request is answered the way an origin CloudFront cannot get a response from is.
    if serves_host(resolved.distribution, bare_host(&origin.domain_name))
        || carries_cloudfront_via(req)
    {
        ::tracing::warn!(
            distribution = %distribution_id,
            origin = %origin.domain_name,
            "this origin request would return to the edge it came from, so no origin \
             request is made"
        );
        return Plan::Generated {
            status: BAD_GATEWAY,
            detail: NO_ORIGIN_DETAIL,
        };
    }
    // The ORIGIN ACCESS CONTROL this origin carries, read from the same stored
    // configuration: what it says about this fetch is decided after the lock is released
    // (see [`origin_access`]), because the origin's own resource policy is read from the
    // origin's own service.
    //
    // A VPC ORIGIN names its endpoint declaration rather than carrying one: the protocol
    // policy and the ports its fetch is addressed under are the VPC origin resource's own
    // ([`vpc_origin_endpoint`]), read from this store while it is held.
    let vpc_endpoint = vpc_origin_endpoint(store, origin);
    let uri = origin_uri(
        origin,
        vpc_endpoint,
        generated_headers::viewer_scheme(req.uri()),
        // WHICH object the origin is asked for, which is the viewer's own path everywhere but
        // at the distribution's root ([`origin_object_path`]).
        &origin_object_path(resolved.distribution, &viewer_path),
        &origin_query(&keyed, &forwarded, addressed.query),
    );
    // An origin request the edge cannot OPEN at all is reported as an origin CloudFront
    // could not reach, and no request is sent: the configuration decides the scheme
    // ([`origin_uri`]), and the origin either answers on it or does not exist on it.
    if !origin_accepts_scheme(&uri) {
        ::tracing::warn!(
            distribution = %distribution_id,
            origin = %origin.domain_name,
            "this origin's protocol policy addresses it over a scheme it does not serve, so \
             the origin connection cannot be completed"
        );
        return Plan::Generated {
            status: BAD_GATEWAY,
            detail: NO_ORIGIN_DETAIL,
        };
    }
    let access = origin_access::of_fetch(
        store,
        resolved.distribution,
        origin,
        &method,
        path_of(&uri),
        req.headers().get("authorization").is_some(),
    );
    let fetch = OriginFetch {
        distribution_id,
        uri,
        access,
        headers,
        read_timeout: origin_read_timeout(origin),
        cache: cacheable.then_some(CacheDirective {
            key: cache_key,
            ttl,
            min_ttl,
        }),
        encode,
        viewer_path,
        custom_error_responses: custom_errors::of_config(&resolved.distribution.config),
    };
    Plan::Fetch(Box::new(fetch))
}

/// The query string the ORIGIN is asked for: the viewer's own parameters that either policy
/// names, in the order the viewer sent them.
///
/// "The headers, cookies, and query strings that are included in the cache key are also
/// included in requests that CloudFront sends to the origin", and the origin request policy
/// names what reaches the origin beside those — so the origin sees the UNION of the two, and
/// a parameter neither policy names is withheld from it.
fn origin_query(
    keyed: &forwarding::CacheKeyParameters,
    forwarded: &forwarding::ForwardedParameters,
    query: &str,
) -> String {
    forwarding::selected_query(query, |name| {
        keyed.query_strings.contains(name) || forwarded.query_strings.contains(name)
    })
}

/// The key one request's answer is cached under: the method, the path, and the values of the
/// parts of the request the behaviour's cache policy puts in the cache key
/// ([`keyed_values`]).
///
/// Nothing the cache policy does NOT name takes part in it — "the headers, cookies, and
/// query strings that are included in the cache key" are what separates one object from
/// another — so two requests the policy says are the same object share one entry however
/// they differ elsewhere. Every part the request contributes is rendered under its own LENGTH
/// ([`keyed_values`]), so no value a viewer chooses can reproduce the rendering of a
/// different request: the key reads as the list of parts it is rather than as text.
fn cache_key(method: &str, viewer_path: &str, keyed: &str) -> String {
    format!("{method} {}:{viewer_path}#{keyed}", viewer_path.len())
}

/// The values of the parts of the request the behaviour's cache policy keys on, rendered as
/// the part of the cache key they contribute.
///
/// "The headers, cookies, and query strings that are included in the cache key": two
/// requests that differ in any value the policy names therefore have different cache keys,
/// so one viewer's answer is never served to a request the policy says is a different one.
/// Each kind of part is rendered under a prefix of its own, so a header and a query string
/// of the same name are two values rather than one, and each kind is rendered in the order
/// that kind reaches the origin in: headers and cookies by name, the query string as the
/// viewer sent it — a query string is keyed on whole, so two requests that carry the same
/// parameters in a different order are two objects. The normalized `Accept-Encoding` is
/// keyed on last — "includes the normalized header in the cache key".
///
/// A header the EDGE generates contributes the EDGE'S OWN value ([`generated_headers`]),
/// never the one a viewer stated for it: such a header describes the viewer's CONNECTION, so
/// the value the origin request carries for it is the value that separates one object from
/// another — a policy keying on the forwarded scheme really does give the two schemes two
/// entries, and a viewer that states a `CloudFront-*` header of its own keys on nothing of its
/// own.
///
/// Each part is rendered under its own LENGTH, so a delimiter inside a value a viewer chose
/// cannot make one request's parts render as another's.
fn keyed_values(
    keyed: &forwarding::CacheKeyParameters,
    accept_encoding: &forwarding::AcceptEncoding,
    query: &str,
    req: &RtRequest,
    generated: &generated_headers::Generated,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (name, value) in req.headers().iter() {
        let name = name.to_ascii_lowercase();
        // A value the viewer stated for a header the edge generates is not that header's
        // value, so it takes no part in the key; the edge's own is added below.
        if generated_headers::is_generated(&name) {
            continue;
        }
        if keyed.headers.contains_header(&name) {
            parts.push(format!("h:{name}={value}"));
        }
    }
    for (name, value) in generated.headers() {
        if keyed.headers.contains_header(&name) {
            parts.push(format!("h:{name}={value}"));
        }
    }
    // The viewer's `Origin` is one of the viewer's own headers wherever the request carries it
    // ([`viewer_origin`]), so a policy keying on it really does give two sites two cache
    // entries: an answer the origin produced for one `Origin` is never served to another.
    if req.headers().get(ORIGIN).is_none() && keyed.headers.contains_header(ORIGIN) {
        if let Some(origin) = viewer_origin(req) {
            parts.push(format!("h:{ORIGIN}={origin}"));
        }
    }
    for name in keyed.headers.named() {
        // A `CloudFront-*` header is never one the VIEWER sent, so it is not keyed on as a
        // header the viewer did not send: the two the edge generates are keyed on by their
        // generated value above, and another member of the family contributes nothing to the
        // key at all, as it reaches no origin request either.
        if generated_headers::is_generated(name) {
            continue;
        }
        if name.eq_ignore_ascii_case(ORIGIN) && viewer_origin(req).is_some() {
            continue;
        }
        if req.headers().get(name).is_none() {
            parts.push(format!("h:{}", name.to_ascii_lowercase()));
        }
    }
    parts.sort();
    let cookie = req.headers().get(COOKIE).unwrap_or_default();
    parts.extend(
        forwarding::keyed_pairs(
            &forwarding::cookie_pairs(cookie),
            &keyed.cookies,
            forwarding::PairOrder::ByName,
        )
        .into_iter()
        .map(|pair| format!("c:{pair}")),
    );
    parts.extend(
        forwarding::keyed_pairs(
            &forwarding::query_pairs(query),
            &keyed.query_strings,
            forwarding::PairOrder::AsSent,
        )
        .into_iter()
        .map(|pair| format!("q:{pair}")),
    );
    if let Some(value) = accept_encoding.header_value() {
        parts.push(format!("{ACCEPT_ENCODING}={value}"));
    }
    // Each part under its own length, so a `;` (or a `#`, or an `=`) inside a value a viewer
    // chose cannot render as the boundary between two parts.
    parts
        .iter()
        .map(|part| format!("{}:{part}", part.len()))
        .collect::<Vec<_>>()
        .join(";")
}

/// Whether `req` has already been through a CloudFront edge: its `Via` names CloudFront,
/// which is the header an edge stamps on every origin request it makes
/// ([`origin_request_headers`]).
///
/// Read on the INBOUND path, which is what makes the stamp a loop signal rather than
/// decoration: a request that reaches the edge carrying it came from an edge.
fn carries_cloudfront_via(req: &RtRequest) -> bool {
    req.headers()
        .get("via")
        .is_some_and(|via| via.contains(VIA_CLOUDFRONT))
}

/// Whether the origin an origin request is addressed at SERVES the scheme that request is
/// addressed over, so the connection can be opened at all.
///
/// A plaintext origin request reaches only an origin that answers plaintext, and the AWS
/// SERVICE ENDPOINTS this emulator serves answer over TLS alone: every one of them is
/// reached at this emulator's own front door, which is ONE self-signed HTTPS listener and
/// refuses a plaintext request to that port (`vendor/server/src/lib.rs`, whose production
/// contract is a single TLS data listener). So an origin whose `OriginProtocolPolicy`
/// addresses one of them over `http` — an API Gateway stage's `execute-api` endpoint, a
/// bucket's own endpoint, any other service's — names an endpoint that answers nothing on
/// that scheme, which is "CloudFront can't connect to the origin" and the `502` an
/// unreachable origin is reported with. The divergence that follows is named rather than
/// hidden: AWS's own S3 REST endpoint does serve HTTP, so an `http-only` custom origin over
/// a bucket endpoint is served there and is this `502` here, because this emulator serves
/// that endpoint over TLS only.
///
/// The one AWS endpoint that is the other way round is an Amazon S3 WEBSITE ENDPOINT
/// ([`origin_access::names_an_s3_website_endpoint`]): a bucket's website surface serves plain
/// HTTP and no HTTPS at all — "Amazon S3 website endpoints do not support HTTPS" — which is
/// why CloudFront's own guidance for a website-endpoint origin is to address it with HTTP
/// Only. So such an origin is reachable over plaintext and is the unreachable-origin `502`
/// over TLS, whichever way round its protocol policy puts it.
///
/// Two kinds of host are NOT such an endpoint. A per-resource listener this emulator opens
/// negotiates its encryption in band, so it answers both schemes at one address — a
/// distribution's own edge domain among them ([`crate::state::edge_endpoint`]). And a host
/// of nobody's this emulator serves at all is reached over either scheme and answers on
/// neither, which the fetch itself reports.
fn origin_accepts_scheme(uri: &str) -> bool {
    let Some((scheme, _rest)) = uri.split_once("://") else {
        return true;
    };
    let Some(authority) = authority_of(uri) else {
        return true;
    };
    let host = bare_host(authority);
    let plaintext = scheme.eq_ignore_ascii_case(PLAINTEXT_SCHEME);
    // The website surface of a bucket, which serves plaintext and nothing else.
    if origin_access::names_an_s3_website_endpoint(host) {
        return plaintext;
    }
    if !plaintext {
        return true;
    }
    !names_a_service_endpoint(host)
}

/// Whether `host` is an AWS SERVICE ENDPOINT of this emulator: a host the front door
/// resolves a service from ([`service_named_by_host`]) in one of the zones AWS serves its
/// service endpoints in.
///
/// The edge zone is not one of them: `cloudfront.net` carries the domain names
/// DISTRIBUTIONS publish, which are addresses of this emulator's own per-distribution
/// listeners rather than of the front door, and those serve both schemes.
fn names_a_service_endpoint(host: &str) -> bool {
    if service_named_by_host(host).is_none() {
        return false;
    }
    let lowercase = host.to_ascii_lowercase();
    AWS_ENDPOINT_SUFFIXES
        .iter()
        .filter(|suffix| **suffix != EDGE_ZONE_SUFFIX)
        .any(|suffix| lowercase.ends_with(suffix))
}

/// The zone a distribution's own published domain name is in, which is the one zone of
/// [`AWS_ENDPOINT_SUFFIXES`] that names no service endpoint.
const EDGE_ZONE_SUFFIX: &str = ".cloudfront.net";

/// The PROTOCOL an origin is addressed under: the policy that chooses the scheme, and the
/// port each scheme is reached on.
///
/// Two kinds of origin state one: a `CustomOriginConfig` states it on the distribution's own
/// origin, and a VPC ORIGIN states it on the VPC origin resource the origin NAMES — its
/// `VpcOriginEndpointConfig` carries the very three members (`OriginProtocolPolicy`,
/// `HTTPPort`, `HTTPSPort`) a custom origin carries, because a VPC origin is the same
/// endpoint declaration held as a resource of its own. An origin stating neither is an Amazon
/// S3 origin, which is reached over HTTPS on that scheme's own port.
struct OriginProtocol<'a> {
    policy: &'a str,
    http_port: i32,
    https_port: i32,
}

impl<'a> OriginProtocol<'a> {
    /// The protocol `origin` is addressed under, reading the VPC origin's own endpoint
    /// configuration for an origin that names one, or `None` for an Amazon S3 origin.
    fn of(origin: &'a Origin, vpc_endpoint: Option<&'a VpcOriginEndpointConfig>) -> Option<Self> {
        if let Some(custom) = origin.custom_origin_config.as_ref() {
            return Some(Self {
                policy: custom.origin_protocol_policy.as_str(),
                http_port: custom.http_port,
                https_port: custom.https_port,
            });
        }
        vpc_endpoint.map(|vpc| Self {
            policy: vpc.origin_protocol_policy.as_str(),
            http_port: vpc.http_port,
            https_port: vpc.https_port,
        })
    }
}

/// The VPC ORIGIN ENDPOINT CONFIGURATION the origin names, for an origin that names one.
///
/// `Origin$VpcOriginConfig` carries only "the VPC origin ID", so everything about WHERE and
/// HOW that endpoint is reached — the protocol policy and the two ports — is held on the VPC
/// origin resource itself, read here from this service's own store. `None` for an origin
/// naming no VPC origin, and for one naming a VPC origin the store no longer holds.
fn vpc_origin_endpoint<'s>(
    store: &'s ComAmazonawsCloudfrontCloudfront20200531State,
    origin: &Origin,
) -> Option<&'s VpcOriginEndpointConfig> {
    let config = origin.vpc_origin_config.as_ref()?;
    store
        .vpc_origin(&config.vpc_origin_id)
        .map(|stored| &stored.endpoint_config)
}

/// The absolute URI of the origin request: the origin's own host, its `OriginPath`, the
/// viewer path and the viewer query.
///
/// `Origin$OriginPath` is "an optional path that CloudFront appends to the origin domain
/// name when CloudFront requests content". The scheme and port come from the protocol the
/// origin is addressed under ([`OriginProtocol`]): an Amazon S3 origin is reached over
/// HTTPS, and an origin stating a policy — a `CustomOriginConfig`, or the
/// `VpcOriginEndpointConfig` of the VPC origin it names — over the protocol that
/// `OriginProtocolPolicy` names, on the matching port: `http-only` is "CloudFront uses only
/// HTTP to access the origin", `https-only` only HTTPS, and `match-viewer` is CloudFront
/// "communicating with your origin using HTTP or HTTPS, depending on the protocol of the
/// viewer request", which is what `viewer_scheme` carries.
fn origin_uri(
    origin: &Origin,
    vpc_endpoint: Option<&VpcOriginEndpointConfig>,
    viewer_scheme: &str,
    viewer_path: &str,
    query: &str,
) -> String {
    let (scheme, port) = match OriginProtocol::of(origin, vpc_endpoint) {
        Some(protocol) => match protocol.policy {
            ORIGIN_HTTP_ONLY => (PLAINTEXT_SCHEME, protocol.http_port),
            ORIGIN_MATCH_VIEWER if viewer_scheme == PLAINTEXT_SCHEME => {
                (PLAINTEXT_SCHEME, protocol.http_port)
            }
            // `https-only`, `match-viewer` for a viewer that dialled HTTPS, and a policy
            // value this emulator does not know: the origin is reached over TLS.
            _ => (SECURE_SCHEME, protocol.https_port),
        },
        None => (SECURE_SCHEME, HTTPS_DEFAULT_PORT),
    };
    let authority = match (scheme, port) {
        (PLAINTEXT_SCHEME, HTTP_DEFAULT_PORT) | (SECURE_SCHEME, HTTPS_DEFAULT_PORT) => {
            origin.domain_name.clone()
        }
        // A domain name that already states a PORT is the whole address the endpoint is
        // served at — which is what the services of this emulator publish for a resource
        // whose listener answers on a port of its own — so the configured port is not written
        // after it: an authority states one port, and two would address nothing.
        _ if states_a_port(&origin.domain_name) => origin.domain_name.clone(),
        _ => format!("{}:{port}", origin.domain_name),
    };
    let origin_path = origin
        .origin_path
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    let path = if viewer_path.starts_with('/') {
        viewer_path.to_string()
    } else {
        format!("/{viewer_path}")
    };
    if query.is_empty() {
        format!("{scheme}://{authority}{origin_path}{path}")
    } else {
        format!("{scheme}://{authority}{origin_path}{path}?{query}")
    }
}

/// The viewer path WITHIN the distribution that is its ROOT: the one path a default root
/// object is requested for.
///
/// Both spellings of it, because a viewer path reaches this module from two addressings
/// ([`resolve`]): the root of a host-style request is `/`, and a prefixed path-style request
/// that names nothing below the distribution carries no viewer path at all.
const ROOT_VIEWER_PATHS: [&str; 2] = ["/", ""];

/// WHICH object the ORIGIN is asked for: the viewer's own path, except at the ROOT of the
/// distribution, where it is the object `DefaultRootObject` names.
///
/// `DistributionConfig$DefaultRootObject` is "the object that you want CloudFront to request
/// from your origin … when a viewer requests the root URL for your distribution", and "a
/// default root object avoids exposing the contents of your distribution": so the root URL is
/// an origin request for THAT object, and whatever an origin answers a request naming no
/// object with — a listing of everything it holds, or its own refusal — is never what the
/// viewer of a distribution that names a root object is served. The substitution is the root's
/// alone ("CloudFront doesn't return default root objects in subdirectories"), and a
/// distribution naming none asks the origin for the path the viewer spelled.
///
/// Only the ORIGIN request is substituted. The cache key, the cache behaviour the path
/// selected, the invalidation a batch matches and the event a function is handed are all the
/// VIEWER's request, which is the request that was made.
///
/// A stored value is used as it stands, with a leading separator removed: "do not add a `/`
/// before the object name", so one carrying it names the same object at the origin's root
/// rather than an empty first segment.
fn origin_object_path(distribution: &StoredDistribution, viewer_path: &str) -> String {
    let root_object = distribution
        .config
        .default_root_object
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches('/');
    if root_object.is_empty() || !ROOT_VIEWER_PATHS.contains(&viewer_path) {
        return viewer_path.to_string();
    }
    format!("/{root_object}")
}

/// The longest an origin fetch may wait, whatever the configuration asks for:
/// `OriginReadTimeout`'s own documented maximum ("the minimum timeout is 1 second, the
/// maximum is 120 seconds"), so one viewer request can never hold a worker longer than
/// the member admits.
const MAX_ORIGIN_READ_TIMEOUT_SECONDS: u64 = 120;

/// How long the edge waits for the origin's response.
///
/// `CustomOriginConfig$OriginReadTimeout` / `S3OriginConfig$OriginReadTimeout` /
/// `VpcOriginConfig$OriginReadTimeout`, in seconds — each the same "how long, in seconds,
/// CloudFront waits for a response from the origin" on the configuration the origin carries —
/// and [`DEFAULT_ORIGIN_READ_TIMEOUT`] for an origin that names none, clamped to the range
/// those members document.
fn origin_read_timeout(origin: &Origin) -> Duration {
    let seconds = origin
        .custom_origin_config
        .as_ref()
        .and_then(|custom| custom.origin_read_timeout)
        .or_else(|| {
            origin
                .s3_origin_config
                .as_ref()
                .and_then(|s3| s3.origin_read_timeout)
        })
        .or_else(|| {
            origin
                .vpc_origin_config
                .as_ref()
                .and_then(|vpc| vpc.origin_read_timeout)
        })
        .unwrap_or(DEFAULT_ORIGIN_READ_TIMEOUT);
    let seconds = u64::try_from(seconds).unwrap_or(0);
    Duration::from_secs(seconds.clamp(1, MAX_ORIGIN_READ_TIMEOUT_SECONDS))
}

/// The headers one origin request carries.
///
/// The origin's own `CustomHeaders` ("CloudFront adds these headers to every origin
/// request"), the `Host` the origin request carries ([`origin_request_host`]), the
/// `X-Amz-Cf-Id` of the request the origin is
/// being fetched for, a `Via` naming CloudFront, the `X-Forwarded-For` naming the address the
/// request came from ([`forwarded_for`]), the `User-Agent` CloudFront sends when
/// the viewer's is not forwarded, the viewer headers the behaviour's ORIGIN REQUEST POLICY
/// forwards, the ones its CACHE POLICY keys on — "the headers, cookies, and query strings
/// that are included in the cache key are also included in requests that CloudFront sends
/// to the origin" ([`forwarding`]) — and the headers the EDGE ITSELF generates that the
/// origin request policy names ([`generated_headers`]).
///
/// The cookies travel as the two policies select them, one `name=value` at a time, and the
/// `Accept-Encoding` travels normalized where the cache policy normalizes it. A header that
/// belongs to one hop of a connection, the viewer's own credential, and a value the viewer
/// stated for a header CloudFront generates itself are never forwarded
/// ([`HEADERS_NEVER_FORWARDED`], [`generated_headers::is_generated`]); every other viewer
/// header the policies name travels with the value the viewer stated for it, and the viewer's
/// `Origin` travels wherever the request carried it ([`viewer_origin`]).
fn origin_request_headers(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    behavior: &Behavior,
    origin: &Origin,
    req: &RtRequest,
    cf_id: &str,
    viewer_host: &str,
) -> Vec<(String, String)> {
    let keyed = forwarding::cache_key_parameters(store, behavior);
    let forwarded = forwarding::forwarded_parameters(store, behavior);
    let accept_encoding = forwarding::accept_encoding(&keyed, req.headers().get(ACCEPT_ENCODING));
    let generated = generated_headers::Generated::of_request(req);
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut forwarded_viewer_user_agent = false;
    for (name, value) in req.headers().iter() {
        let name = name.to_ascii_lowercase();
        // `Host` is not copied from the viewer's request here: WHICH host the origin request
        // carries is one decision, made once below ([`origin_request_host`]).
        if HEADERS_NEVER_FORWARDED.contains(&name.as_str()) || name == HOST {
            continue;
        }
        // A `CloudFront-*` header describes the viewer's CONNECTION, so the value the viewer
        // stated for one is dropped here: the edge's own value is added below, where the
        // policy names it.
        if generated_headers::is_generated(&name) {
            continue;
        }
        // The cookies and the negotiated content coding are selected and normalized below,
        // rather than forwarded as the viewer sent them — and so is the forwarded-for chain,
        // which every origin request carries whatever the policies name ([`forwarded_for`]).
        if name == COOKIE
            || (name == ACCEPT_ENCODING && accept_encoding.normalizes())
            || name == FORWARDED_FOR
        {
            continue;
        }
        // The body's own description travels with the body, whatever the policies say
        // about the viewer's other headers.
        let carries_body = name == CONTENT_TYPE || name == "content-length";
        if carries_body
            || keyed.headers.contains_header(&name)
            || forwarded.headers.contains_header(&name)
        {
            forwarded_viewer_user_agent |= name == USER_AGENT;
            headers.push((name, value.to_string()));
        }
    }
    // The viewer's `Origin`, where the behaviour's policies name that header and the request
    // carried it under the reserved name a distribution's own address forwards it as
    // ([`viewer_origin`]): the header the ORIGIN is sent is the one the viewer stated, so a
    // CORS request a browser made through a distribution reaches the origin as the browser
    // sent it — which is what an origin that answers a preflight from its own CORS
    // configuration is asked.
    if req.headers().get(ORIGIN).is_none()
        && (keyed.headers.contains_header(ORIGIN) || forwarded.headers.contains_header(ORIGIN))
    {
        if let Some(origin) = viewer_origin(req) {
            headers.push((ORIGIN.to_string(), origin));
        }
    }
    // The cookies the cache key carries, and the ones the origin request policy forwards
    // beside them, in the order the viewer sent them; a cookie neither policy names is
    // withheld from the origin, and a request whose cookies are all withheld carries no
    // `Cookie` header at all.
    if let Some(cookie) = req.headers().get(COOKIE) {
        let selected = forwarding::selected_cookies(cookie, |name| {
            keyed.cookies.contains(name) || forwarded.cookies.contains(name)
        });
        if let Some(selected) = selected {
            headers.push((COOKIE.to_string(), selected));
        }
    }
    // "CloudFront always includes the `Accept-Encoding` header in origin requests when the
    // value of this field is `true`" — as the normalized value, which is also what the cache
    // key carries.
    if let Some(value) = accept_encoding.header_value() {
        headers.push((ACCEPT_ENCODING.to_string(), value));
    }
    headers.push((
        HOST.to_string(),
        origin_request_host(origin, &keyed, &forwarded, viewer_host).to_string(),
    ));
    headers.push((X_AMZ_CF_ID.to_string(), cf_id.to_string()));
    headers.push((
        "via".to_string(),
        format!("1.1 {cf_id}.cloudfront.net ({VIA_CLOUDFRONT})"),
    ));
    // CloudFront's OWN `User-Agent`, for a request whose viewer `User-Agent` the policies do
    // not forward: the origin is then told who is calling it, which is CloudFront. A behaviour
    // that DOES forward the header has already put the viewer's own value on the request
    // above, and that one value is what the origin sees ([`ORIGIN_USER_AGENT`]).
    if !forwarded_viewer_user_agent {
        headers.push((USER_AGENT.to_string(), ORIGIN_USER_AGENT.to_string()));
    }
    // The chain of addresses the request has travelled, which EVERY origin request carries
    // whatever the behaviour's policies name ([`forwarded_for`]).
    headers.push((FORWARDED_FOR.to_string(), forwarded_for(req)));
    // The headers the edge generated for this request, where the origin request policy names
    // them: a behaviour with no origin request policy, or one whose policy does not list a
    // generated header, sends the origin none of them.
    for (name, value) in generated.headers() {
        if forwarded.generated_headers.contains_header(&name) {
            headers.push((name, value));
        }
    }
    for custom in origin
        .custom_headers
        .as_ref()
        .and_then(|custom| custom.items.as_ref())
        .into_iter()
        .flatten()
    {
        headers.push((
            custom.header_name.to_ascii_lowercase(),
            custom.header_value.clone(),
        ));
    }
    headers
}

/// The `Host` ONE ORIGIN REQUEST carries.
///
/// A CUSTOM origin is sent the host the VIEWER addressed — the distribution's own domain name
/// or the alternate domain name the viewer used — exactly where the matched behaviour's
/// policies carry the viewer's `Host` among the headers that reach the origin, and the
/// origin's own domain name everywhere else. That is what makes `Host` worth forwarding at
/// all: an origin behind several front doors tells them apart by the name the viewer
/// addressed, which is why the header behaviours spell out both answers — `allViewer` carries
/// every header the viewer sent, `allViewerExceptHostHeader` carries all of them but `Host`,
/// and a `whitelist` carries it exactly when it names it ([`forwarding::NameSet`], which
/// reads the legacy `ForwardedValues$Headers` list the same way).
///
/// An AMAZON S3 origin is always sent the origin's own domain name: a bucket is addressed by
/// its own host, so CloudFront sets that host whatever a behaviour forwards — which is the
/// reason `allViewerExceptHostHeader` exists for such an origin. An origin carrying no
/// `CustomOriginConfig` is read as one here, exactly as [`origin_uri`] reads it when it
/// chooses the scheme the origin is addressed over.
fn origin_request_host<'a>(
    origin: &'a Origin,
    keyed: &forwarding::CacheKeyParameters,
    forwarded: &forwarding::ForwardedParameters,
    viewer_host: &'a str,
) -> &'a str {
    let forwards_viewer_host =
        keyed.headers.contains_header(HOST) || forwarded.headers.contains_header(HOST);
    if forwards_viewer_host && origin.custom_origin_config.is_some() {
        return viewer_host;
    }
    &origin.domain_name
}

/// One answer the edge reached for the origin to produce, and what the metrics read it by:
/// the response the viewer gets, and — when an ORIGIN really answered — how long the edge had
/// been working on the request when that answer was ready, which is the span
/// [`edge_metrics::ORIGIN_LATENCY_METRIC`] measures. `origin_latency` is `None` for every path
/// where no origin answered, so an answer CloudFront produced for an origin it could not reach
/// is not reported as an origin's.
struct Fetched {
    response: RtResponse,
    origin_latency: Option<Duration>,
}

impl Fetched {
    /// An answer CloudFront produced itself, no origin having answered.
    fn of_cloudfront(response: RtResponse) -> Self {
        Self {
            response,
            origin_latency: None,
        }
    }
}

/// Fetch the origin, serve what it answered, and cache it when the behaviour caches it.
///
/// The fetch goes through the transport `OperationContext::callback` hands this handler,
/// so an origin this emulator serves answers it in process. An origin that answers
/// nothing at all is reported as CloudFront reports an origin it could not reach.
async fn fetch_origin<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    fetch: OriginFetch,
    cf_id: &str,
    req: &RtRequest,
    received: Instant,
) -> Fetched {
    let body = req.body().bytes().map(<[u8]>::to_vec).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(req.method())
        .uri(&fetch.uri);
    for (name, value) in &fetch.headers {
        builder = builder.header(name, value);
    }
    let built = builder.body(body).map_err(|error| error.to_string());
    let mut request = match built {
        Ok(request) => request,
        Err(error) => {
            ::tracing::warn!(
                uri = %fetch.uri,
                %error,
                "the configured origin does not render an HTTP request, so no origin \
                 request was made"
            );
            return Fetched::of_cloudfront(generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, cf_id));
        }
    };
    // The ORIGIN ACCESS CONTROL's own half of the fetch: the request is made AS the principal
    // that mechanism names, acting for this distribution, and the ORIGIN decides whether it
    // serves it — from its own resource policy, for that principal (see [`origin_access`]).
    //
    // The credential it is signed with is minted for this one request and recorded as that
    // call, so every decision the origin's service makes itself about it — its own policy, and
    // the key policy of a KMS key the object's bytes are encrypted under — is made for that
    // principal rather than for the account, whose own store would serve a private origin in
    // full. Both the credential and the record are destroyed when `_as_the_service_principal`
    // is dropped, which is every path out of this function.
    let mut _as_the_service_principal = None;
    if let Some(access) = &fetch.access {
        let principal = origin_access::acting_principal(ctx, access, fetch.read_timeout).await;
        match origin_access::sign_as_the_service_principal(
            engine,
            &mut request,
            access,
            &principal,
            ctx,
            fetch.read_timeout,
        )
        .await
        {
            Ok(in_flight) => _as_the_service_principal = in_flight,
            Err(error) => ::tracing::warn!(
                uri = %fetch.uri,
                %error,
                "the origin request could not be signed for this origin access control, \
                 so it is made as a request carrying no credential"
            ),
        }
    } else if let Some(region) = signed_as_this_emulator(&fetch.uri) {
        // An origin fronted by NO access mechanism is fetched as AWS fetches one: carrying no
        // credential. This emulator verifies every request it serves, though, so such a leg is
        // refused by its own front door before the origin's surface decides anything — and for
        // ONE origin surface that refusal is the whole answer: an Amazon S3 WEBSITE ENDPOINT
        // serves what the bucket makes PUBLIC and nothing else, to whoever asks
        // (`website_endpoint`, whose every read is judged by the bucket's anonymous-read rule),
        // so the identity on the leg decides nothing about what the edge may read. That leg
        // therefore travels with the signature every in-process call of this emulator carries
        // ([`::service_common::in_process_sigv4`]).
        //
        // No other origin is signed here. An origin whose service decides a read by the CALLER
        // — a bucket's REST endpoint among them — would serve a private object to the account's
        // own credential, so a distribution that was granted nothing must reach it exactly as
        // it does now: unsigned, and answered as the origin answers a caller presenting none.
        if let Err(error) = ::service_common::in_process_sigv4::sign_in_process_request(
            &mut request,
            S3_SIGNING_NAME,
            &region,
            ctx,
        )
        .await
        {
            ::tracing::warn!(
                uri = %fetch.uri,
                %error,
                "this origin request could not be signed as this emulator's own in-process \
                 call, so it is made carrying no credential"
            );
        }
    }
    let (parts, body) = request.into_parts();
    let built = RtRequest::try_from(::http::Request::from_parts(
        parts,
        SdkBody::from(::bytes::Bytes::from(body)),
    ))
    .map_err(|error| error.to_string());
    let request = match built {
        Ok(request) => request,
        Err(error) => {
            ::tracing::warn!(
                uri = %fetch.uri,
                %error,
                "the configured origin does not render an HTTP request, so no origin \
                 request was made"
            );
            return Fetched::of_cloudfront(generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, cf_id));
        }
    };
    let transport = ctx.callback().http_connector();
    let answered = ::tokio::time::timeout(fetch.read_timeout, transport.call(request)).await;
    let response = match answered {
        Err(_elapsed) => {
            ::tracing::warn!(
                uri = %fetch.uri,
                timeout_seconds = fetch.read_timeout.as_secs(),
                "the origin did not answer within its read timeout"
            );
            return Fetched::of_cloudfront(generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, cf_id));
        }
        Ok(Err(error)) => {
            ::tracing::warn!(
                uri = %fetch.uri,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the configured origin could not be reached"
            );
            return Fetched::of_cloudfront(generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, cf_id));
        }
        Ok(Ok(response)) => response,
    };
    // The origin has answered, so this is the span from the edge receiving the viewer request
    // to the answer being ready: what the origin latency of this request is.
    let origin_latency = received.elapsed();
    let status = response.status().as_u16();
    // What the ORIGIN said about its own bytes: the media type, which decides whether the
    // edge may compress them, and the coding they are already under, which means it must
    // not.
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(ToString::to_string);
    let content_encoding = response
        .headers()
        .get(CONTENT_ENCODING)
        .map(ToString::to_string);
    // And everything ELSE the origin stated about its answer, which the edge serves to the
    // viewer as the origin stated it ([`RESPONSE_HEADERS_NEVER_SERVED`] is what it does not).
    let headers = served_origin_headers(response.headers(), &fetch.uri);
    let body = response
        .into_body()
        .bytes()
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if is_transport_refusal(&body) {
        ::tracing::warn!(
            uri = %fetch.uri,
            "nothing in this emulator serves the configured origin, so no origin response \
             exists to serve"
        );
        return Fetched::of_cloudfront(generated(BAD_GATEWAY, NO_ORIGIN_DETAIL, cf_id));
    }
    let answer = OriginAnswer {
        status,
        body,
        content_type,
        content_encoding,
        headers,
        // The origin produced this answer for the request being served, so this cache has
        // held it for no time at all and states no age of its own about it.
        age: None,
    };
    if let Some(directive) = fetch.cache {
        // How long this answer is held: the behaviour's own TTL for an answer the edge may
        // cache at all, and `ErrorCachingMinTTL` for one the distribution's custom error
        // responses name — which is what holds an ERROR no cache policy would hold
        // ([`custom_errors`]) — unless the ORIGIN asked that its answer not be stored, which
        // only the behaviour's minimum TTL still holds ([`cache_directives`]).
        let ttl = cache_directives::held_for(&cache_directives::Residence {
            status,
            headers: &answer.headers,
            error_ttl: custom_errors::caching_ttl(&fetch.custom_error_responses, status),
            ttl: directive.ttl,
            min_ttl: directive.min_ttl,
        });
        // The size bound the entry has to stay within is [`cache`]'s own, stated where the
        // bytes are held.
        if let Some(ttl) = ttl {
            cache(
                engine,
                &fetch.distribution_id,
                &directive.key,
                ttl,
                &fetch.viewer_path,
                &answer,
            );
        }
    }
    // The origin answered, so the answer is the origin's — a MISS, since this request is
    // what went to the origin. An origin answer that is itself an error is reported as
    // one CloudFront did not produce from its cache either.
    let x_cache = cache_outcome(status, CACHE_MISS);
    Fetched {
        response: served_origin_answer(&answer, fetch.encode, x_cache, cf_id),
        origin_latency: Some(origin_latency),
    }
}

/// The signing name of the service an Amazon S3 surface is addressed under, which is what the
/// signature of an in-process leg to one names.
const S3_SIGNING_NAME: &str = "s3";

/// The REGION to sign an origin leg for, for the one origin surface whose answer the identity
/// on that leg cannot change: an Amazon S3 WEBSITE ENDPOINT
/// ([`origin_access::names_an_s3_website_endpoint`]), which serves what the bucket makes PUBLIC
/// and nothing else, whoever asks. `None` for every other origin, which is fetched as AWS
/// fetches one an access mechanism does not sign: carrying no credential.
///
/// Why the leg is signed HERE rather than the surface being served unsigned where that refusal
/// comes from: the front door asks the MODEL which schemes the route a request names is served
/// under, and the website surface is no modelled route of its own — a request to it names a
/// modelled operation of the REST API (a bucket or service listing, by its method and path), so
/// the scheme that operation's model declares is the one required however the surface itself
/// would be served. A door that declares no scheme of its own is therefore not reached for
/// such a request (verified against this emulator's own front door), which leaves the CALLER
/// of the leg as the layer that can state what the leg is: this emulator calling itself. A
/// client dialling the bucket's website host directly is in the same position and is not
/// repaired by this, which the S3 service's own anonymous-read work owns.
fn signed_as_this_emulator(uri: &str) -> Option<String> {
    let host = bare_host(authority_of(uri)?);
    if !origin_access::names_an_s3_website_endpoint(host) {
        return None;
    }
    origin_access::s3_endpoint_bucket(host).map(|(_bucket, region)| region)
}

/// The error types that are the in-process TRANSPORT's own refusal rather than an answer
/// from an origin: this emulator serves nothing at that host, or more than one of its
/// services claims it. Either way the fetch reached no origin, which is the same class as
/// an origin that never answered.
///
/// This is the shape the Lambda-function-URL origin takes here: no service of this
/// emulator claims a function URL host, so the fetch of such an origin reaches nothing.
const TRANSPORT_REFUSALS: [&str; 2] = ["UnknownService", "AmbiguousService"];

/// How much of an answer is read to tell the transport's own refusal from an origin's
/// response. The refusal envelope is two short fields; anything larger is a response from
/// something that served the request, so it is not read at all.
const MAX_REFUSAL_ENVELOPE_BYTES: usize = 4 * 1024;

/// Whether `body` is the in-process transport's OWN refusal of the origin request rather
/// than a response from an origin that served it.
///
/// The refusal is told by the error type its envelope carries ([`TRANSPORT_REFUSALS`]);
/// the bridge the request crosses keeps the body but not the headers, so the envelope is
/// where that type can be read at all. The same reading
/// `services/service-com-amazonaws-eventbridge-awsevents` makes of an api-destination
/// delivery nothing served.
fn is_transport_refusal(body: &[u8]) -> bool {
    if body.len() > MAX_REFUSAL_ENVELOPE_BYTES {
        return false;
    }
    let Ok(envelope) = ::serde_json::from_slice::<::serde_json::Value>(body) else {
        return false;
    };
    let Some(error_type) = envelope.get("__type").and_then(::serde_json::Value::as_str) else {
        return false;
    };
    // The type may carry a shape namespace; the code is its last segment.
    let code = error_type.rsplit('#').next().unwrap_or(error_type);
    TRANSPORT_REFUSALS.contains(&code)
}

/// The largest origin response this edge holds in its cache.
///
/// A cached answer is held in memory beside the distribution, so what one viewer request
/// can add to the process is bounded here as well as by the entry count: a response larger
/// than this is served to this viewer and not held, which a caller sees only as a cache
/// MISS. This emulator's own bound, not CloudFront's.
const MAX_CACHED_BODY_BYTES: usize = 1024 * 1024;

/// What [`X_CACHE`] reports for an answer the edge serves on an ORIGIN's behalf: the outcome
/// `produced` names — the origin produced this answer, or the cache held it — for an answer that
/// IS the object, and [`CACHE_ERROR`] for one that is an ERROR, which is what CloudFront reports
/// for an answer no object came out of however it reached it. The same reading on both paths, so
/// an error the distribution's configuration holds in the cache ([`custom_errors`]) does not read
/// as a hit the second time a viewer earns it.
fn cache_outcome(status: u16, produced: &'static str) -> &'static str {
    if is_cacheable_status(status) || status < 400 {
        produced
    } else {
        CACHE_ERROR
    }
}

/// Whether a status the origin answered is one the edge may hold in its cache.///
/// "CloudFront caches ... responses with the following status codes: 200, 203, 204, 206,
/// 300, 301, 302, 303, 304, 307, 308, 404, 405, 410, 414, and 501" — the successful,
/// redirect and documented error statuses; anything else is served to this viewer and not
/// held.
fn is_cacheable_status(status: u16) -> bool {
    const CACHEABLE: [u16; 15] = [
        200, 203, 204, 206, 300, 301, 302, 303, 304, 307, 308, 404, 405, 410, 414,
    ];
    CACHEABLE.contains(&status) || status == 501
}

/// Hold one origin answer in the distribution's edge cache.
///
/// The ORIGIN'S OWN bytes are held, under its own description of them: what the edge serves
/// from the entry is computed for each viewer request ([`served_origin_answer`]), so an
/// object cached for one viewer is not held under another viewer's content coding.
///
/// The size bound is enforced HERE, where the bytes are held, as well as where the plan asks
/// for them to be: an answer larger than [`MAX_CACHED_BODY_BYTES`] is served to this viewer
/// and not held, so what one viewer request can add to this process stays bounded whoever
/// calls this.
fn cache<Eng: Send + Sync + 'static>(
    engine: &Eng,
    distribution_id: &str,
    key: &str,
    ttl: Duration,
    viewer_path: &str,
    answer: &OriginAnswer,
) {
    if answer.body.len() > MAX_CACHED_BODY_BYTES {
        ::tracing::debug!(
            distribution = %distribution_id,
            bytes = answer.body.len(),
            "this origin answer is larger than the edge cache holds, so it is served to this \
             viewer and not held"
        );
        return;
    }
    let Some(state) = crate::state::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    // The expiry is computed BEFORE the store is locked and without arithmetic that can
    // fail: a TTL the clock cannot reach holds the entry for the longest span the clamp
    // admits rather than making the instant uncomputable.
    let Some(expires_at) = now.checked_add(ttl) else {
        ::tracing::warn!(
            distribution = %distribution_id,
            ttl_seconds = ttl.as_secs(),
            "this cache behavior's TTL runs past the clock, so the answer is served to \
             this viewer and not held"
        );
        return;
    };
    let mut store = state.lock();
    let Some(distribution) = store.distributions.get_mut(distribution_id) else {
        return;
    };
    distribution.store_edge_response(
        key.to_string(),
        CachedEdgeResponse {
            status: answer.status,
            body: answer.body.clone(),
            content_type: answer.content_type.clone(),
            content_encoding: answer.content_encoding.clone(),
            headers: answer.headers.clone(),
            path: viewer_path.to_string(),
            stored_at: now,
            expires_at,
        },
    );
}

/// One answer the edge serves on an ORIGIN's behalf, whether the origin produced it for this
/// request or the edge cache held it: the origin's bytes under the origin's own description
/// of them, compressed with `encode` where the behaviour asks for compression and these
/// bytes are compressible ([`compression`]).
///
/// An answer the origin already encoded keeps the origin's bytes and the origin's coding: it
/// is not encoded twice, and nothing is relabelled. An answer larger than the size
/// CloudFront's compression documents ([`compression::MAX_COMPRESSED_BYTES`]) is served as
/// the origin sent it, which also bounds the work one viewer request asks of this process.
fn served_origin_answer(
    answer: &OriginAnswer,
    encode: Option<Encoding>,
    x_cache: &str,
    cf_id: &str,
) -> RtResponse {
    let content_type = answer.content_type.as_deref();
    let mut body = answer.body.clone();
    let mut encoding = answer.content_encoding.clone();
    if encoding.is_none() {
        let compressible = compression::compressible(content_type)
            && body.len() <= compression::MAX_COMPRESSED_BYTES;
        if let Some(coding) = encode.filter(|_| compressible) {
            if let Some(compressed) = coding.compress(&body) {
                body = compressed;
                encoding = Some(coding.token().to_string());
            }
        }
    }
    let mut response = served(answer.status, body, x_cache, cf_id);
    // The origin's own headers, as the origin stated them: appended rather than set, so a
    // header the origin stated more than once (a `Set-Cookie` per cookie) reaches the viewer
    // as many times as the origin sent it.
    for (name, value) in &answer.headers {
        let _ = response
            .headers_mut()
            .try_append(name.clone(), value.clone());
    }
    // An answer served out of the cache states its AGE, as every cache states one for a
    // response it serves from storage: the seconds since the response was generated at the
    // origin, which is the span this entry has been resident plus whatever age the origin
    // itself stated about the bytes when they were fetched (a cache in front of the origin had
    // held them that long already). Stated after the origin's own headers, so what the viewer
    // reads is this cache's own count rather than the one the entry holds.
    if let Some(held) = answer.age {
        let at_origin = answer
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(AGE))
            .and_then(|(_, value)| value.trim().parse::<u64>().ok())
            .unwrap_or_default();
        let age = at_origin.saturating_add(held.as_secs());
        let _ = response.headers_mut().try_insert(AGE, age.to_string());
    }
    if let Some(content_type) = content_type {
        let _ = response
            .headers_mut()
            .try_insert(CONTENT_TYPE, content_type.to_string());
    }
    if let Some(encoding) = encoding {
        let _ = response.headers_mut().try_insert(CONTENT_ENCODING, encoding);
    }
    response
}

/// One answer the edge serves on behalf of an origin, or out of its cache.
fn served(status: u16, body: Vec<u8>, x_cache: &str, cf_id: &str) -> RtResponse {
    let mut response = RtResponse::new(status_code(status), SdkBody::from(body));
    stamp(&mut response, x_cache, cf_id);
    response
}

/// One answer CloudFront generated itself, as its own error page.
///
/// The page is the one CloudFront serves for a request it did not pass to an origin: the
/// status, the sentence it heads the page with, the detail line for that failure, and the
/// identifier of the request it answered, which is what a report to AWS Support quotes.
fn generated(status: u16, detail: &str, cf_id: &str) -> RtResponse {
    generated_page(status, detail, cf_id, CACHE_ERROR)
}

/// [`generated`] under the cache outcome `x_cache` states: the page is the same one, and what
/// differs is WHAT CloudFront reports as having produced it.
fn generated_page(status: u16, detail: &str, cf_id: &str, x_cache: &str) -> RtResponse {
    let page = format!(
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \
         \"http://www.w3.org/TR/html4/loose.dtd\">\n\
         <HTML><HEAD><META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; \
         charset=iso-8859-1\">\n\
         <TITLE>ERROR: The request could not be satisfied</TITLE>\n\
         </HEAD><BODY>\n\
         <H1>{status} ERROR</H1>\n\
         <H2>The request could not be satisfied.</H2>\n\
         <HR noshade size=\"1px\">\n\
         {detail}\n\
         <BR clear=\"all\">\n\
         <HR noshade size=\"1px\">\n\
         <PRE>Generated by cloudfront (CloudFront)\nRequest ID: {cf_id}\n</PRE>\n\
         <ADDRESS></ADDRESS>\n\
         </BODY></HTML>"
    );
    let mut response = RtResponse::new(status_code(status), SdkBody::from(page.into_bytes()));
    let _ = response
        .headers_mut()
        .try_insert("content-type", "text/html".to_string());
    let _ = response
        .headers_mut()
        .try_insert("server", SERVER.to_string());
    stamp(&mut response, x_cache, cf_id);
    response
}

/// The answer the edge serves when the CloudFront function the matched behaviour associates
/// produced no event object to continue from: CloudFront's own `503` page, reported under the
/// cache outcome that names the function's EXECUTION as what failed rather than the cache or
/// an origin ([`CACHE_FUNCTION_EXECUTION_ERROR`]).
fn refused_by_function(cf_id: &str) -> RtResponse {
    generated_page(
        FUNCTION_FAILED_STATUS,
        FUNCTION_FAILED_DETAIL,
        cf_id,
        CACHE_FUNCTION_EXECUTION_ERROR,
    )
}

/// The answer the edge serves when the LAMBDA@EDGE function the matched behaviour associates
/// produced nothing to continue from: CloudFront's own page for that failure, under the cache
/// outcome that names the Lambda@Edge function's execution as what went wrong
/// ([`CACHE_LAMBDA_EXECUTION_ERROR`]).
fn refused_by_edge_function(cf_id: &str) -> RtResponse {
    generated_page(
        LAMBDA_FAILED_STATUS,
        LAMBDA_FAILED_DETAIL,
        cf_id,
        CACHE_LAMBDA_EXECUTION_ERROR,
    )
}

/// The redirect CloudFront answers a viewer whose behaviour is `redirect-to-https`: the
/// status, the HTTPS URL to resubmit the request to, and the page CloudFront serves beside
/// it.
fn redirected(location: &str, cf_id: &str) -> RtResponse {
    let page = "<html>\n<head><title>301 Moved Permanently</title></head>\n<body>\n\
                <center><h1>301 Moved Permanently</h1></center>\n\
                <hr><center>CloudFront</center>\n</body>\n</html>\n";
    let mut response = RtResponse::new(
        status_code(MOVED_PERMANENTLY),
        SdkBody::from(page.as_bytes().to_vec()),
    );
    let _ = response
        .headers_mut()
        .try_insert(LOCATION, location.to_string());
    let _ = response
        .headers_mut()
        .try_insert(CONTENT_TYPE, "text/html".to_string());
    let _ = response
        .headers_mut()
        .try_insert("server", SERVER.to_string());
    stamp(&mut response, CACHE_ERROR, cf_id);
    response
}

/// Put the headers every edge answer carries on `response`: its cache outcome and the
/// identifier of the request it answers.
fn stamp(response: &mut RtResponse, x_cache: &str, cf_id: &str) {
    let _ = response
        .headers_mut()
        .try_insert(X_CACHE, x_cache.to_string());
    if !cf_id.is_empty() {
        let _ = response
            .headers_mut()
            .try_insert(X_AMZ_CF_ID, cf_id.to_string());
    }
}

/// `status` as a status code, falling back to the one CloudFront answers when it got no
/// origin response — the only way a status this module computed is not a status code is
/// if an origin answered one the wire cannot carry.
fn status_code(status: u16) -> StatusCode {
    StatusCode::try_from(status).unwrap_or_else(|_| {
        StatusCode::try_from(BAD_GATEWAY).expect("502 is a valid HTTP status code")
    })
}

/// The host a buffered request ADDRESSED, by the one reading of that question
/// (`service_common::data_plane_addressing::addressed_authority`): the authority of an
/// absolute-form request target when the request carries one, and the `Host` header
/// otherwise.
fn request_host(parts: &http::request::Parts) -> Option<&str> {
    match parts.uri.authority().map(http::uri::Authority::as_str) {
        Some(authority) => Some(authority),
        None => parts.headers.get("host").and_then(|host| host.to_str().ok()),
    }
}

/// The authority of an absolute request URI, or `None` when the URI is origin-form.
fn authority_of(uri: &str) -> Option<&str> {
    let (_scheme, rest) = uri.split_once("://")?;
    Some(match rest.find('/') {
        Some(path_start) => &rest[..path_start],
        None => rest,
    })
}

/// The path component of a request URI, whether it is origin-form or absolute.
fn path_of(uri: &str) -> &str {
    let after_authority = match uri.split_once("://") {
        Some((_scheme, rest)) => match rest.find('/') {
            Some(path_start) => &rest[path_start..],
            None => "/",
        },
        None => uri,
    };
    match after_authority.find('?') {
        Some(mark) => &after_authority[..mark],
        None => after_authority,
    }
}

/// The query component of a request URI: everything after the first `?`.
fn query_of(uri: &str) -> &str {
    match uri.find('?') {
        Some(mark) => &uri[mark + 1..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns that a wire test cannot vary cheaply: how a URI is
    //! split into an addressing, and how an origin request URI is built. The served
    //! behaviour is pinned end to end in `tests/all/edge_data_plane.rs`.

    use super::*;
    use crate::ops::types::CustomOriginConfig;

    #[test]
    fn the_origins_own_response_headers_are_served_except_the_listed_ones() {
        // Rows differ along WHY a header is or is not relayed: a header the origin stated
        // about the object, each class the exclusion list carries (a hop-by-hop header, the
        // three that describe the bytes being served, the two the edge states itself), the
        // same names spelled in another case, and a name that merely resembles one of them.
        let cases: [(&str, &str, bool); 13] = [
            ("the object's entity tag", "etag", true),
            ("a cache directive", "cache-control", true),
            ("user metadata", "x-amz-meta-note", true),
            ("a cookie the origin set", "set-cookie", true),
            ("a header name in upper case", "ETag", true),
            ("a hop-by-hop header", "connection", false),
            ("another hop-by-hop header", "transfer-encoding", false),
            ("a hop-by-hop header in mixed case", "Keep-Alive", false),
            ("the length of the bytes served", "content-length", false),
            ("the media type the edge restates", "content-type", false),
            ("the coding the edge restates", "content-encoding", false),
            ("the cache outcome the edge states", X_CACHE, false),
            ("a name that merely resembles one", "x-connection-note", true),
        ];
        for (case, name, expected) in cases {
            let actual = is_served_from_origin(name);
            assert_eq!(
                actual, expected,
                "case {case:?} ({name:?}): expected is_served_from_origin == {expected}, got \
                 {actual}"
            );
        }
    }

    #[test]
    fn a_rewritten_uri_keeps_the_addressing_the_viewer_dialled() {
        // Rows differ along the addressing the request arrived under (a distribution's own
        // host in origin form, the same absolute, the reserved data-plane path), along what
        // the function returned for the URI (a path, a path with no leading slash) and along
        // whether it returned a query string.
        // (case, the request's URI, the viewer path within the distribution, the URI the
        //  function returned, the query it returned, the URI the edge then serves)
        type Row = (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str);
        let cases: [Row; 5] = [
            (
                "a host-addressed request",
                "/original.txt",
                "/original.txt",
                "/rewritten.txt",
                "",
                "/rewritten.txt",
            ),
            (
                "an absolute request URI",
                "http://d111.cloudfront.net/original.txt",
                "/original.txt",
                "/rewritten.txt",
                "",
                "http://d111.cloudfront.net/rewritten.txt",
            ),
            (
                "the reserved data-plane path",
                "/_aws/cloudfront/E123/original.txt",
                "/original.txt",
                "/rewritten.txt",
                "",
                "/_aws/cloudfront/E123/rewritten.txt",
            ),
            (
                "a URI the function returned without a leading slash",
                "/original.txt",
                "/original.txt",
                "rewritten.txt",
                "",
                "/rewritten.txt",
            ),
            (
                "a query string the function returned",
                "/original.txt?a=1",
                "/original.txt",
                "/rewritten.txt",
                "b=2",
                "/rewritten.txt?b=2",
            ),
        ];
        for (case, uri, viewer_path, rewritten, query, expected) in cases {
            let mut req = RtRequest::new(SdkBody::empty());
            req.set_uri(uri.to_string()).expect("a valid URI");
            req.headers_mut()
                .insert("host".to_string(), "d111.cloudfront.net".to_string());
            let rewrite = functions::Request {
                uri: rewritten.to_string(),
                query: query.to_string(),
                headers: vec![("host".to_string(), "d111.cloudfront.net".to_string())],
            };
            let request =
                rewritten_request(&req, viewer_path, &rewrite).expect("the rewrite renders");
            assert_eq!(
                request.uri(),
                expected,
                "case {case:?}: expected the edge to serve {expected:?}, got {:?}",
                request.uri()
            );
        }
    }

    #[test]
    fn a_uri_is_split_into_its_path_and_its_query() {
        // Rows differ along the form of the URI (origin-form, absolute), whether it
        // carries a query, and whether it carries a path at all.
        let cases: [(&str, &str, &str); 6] = [
            ("/", "/", ""),
            ("/opt/thing", "/opt/thing", ""),
            ("/opt/thing?a=1&b=2", "/opt/thing", "a=1&b=2"),
            ("https://d1.cloudfront.net/opt", "/opt", ""),
            ("https://d1.cloudfront.net/opt?a=1", "/opt", "a=1"),
            ("https://d1.cloudfront.net", "/", ""),
        ];
        for (uri, path, query) in cases {
            let (actual_path, actual_query) = (path_of(uri), query_of(uri));
            assert_eq!(
                (actual_path, actual_query),
                (path, query),
                "case {uri:?}: expected the path {path:?} and the query {query:?}, got \
                 {actual_path:?} and {actual_query:?}"
            );
        }
    }

    #[test]
    fn the_data_plane_path_prefix_names_the_distribution_before_the_viewer_path() {
        // Rows differ along what follows the prefix (an id with a viewer path, an id
        // alone), along the spelling of the prefix itself, and along paths that carry no
        // prefix at all.
        let cases: [(&str, Option<(&str, &str)>); 6] = [
            ("/_aws/cloudfront/E123/opt/x", Some(("E123", "/opt/x"))),
            ("/_aws/cloudfront/E123", Some(("E123", "/"))),
            ("/_aws/cloudfront/E123/", Some(("E123", "/"))),
            ("/_aws/cloudfront", None),
            ("/_aws/execute-api/abc/stage", None),
            ("/2020-05-31/distribution", None),
        ];
        for (path, expected) in cases {
            let actual = split_edge_prefix(path);
            assert_eq!(
                actual, expected,
                "case {path:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn an_origin_request_uri_carries_the_origins_scheme_port_and_path() {
        // Rows differ along the origin protocol policy, the ports, the origin path and
        // the viewer query — everything the rule generalises over.
        let custom = |policy: &str, http_port: i32, https_port: i32| CustomOriginConfig {
            http_port,
            https_port,
            origin_protocol_policy: policy.to_string(),
            ..CustomOriginConfig::default()
        };
        let origin = |config: Option<CustomOriginConfig>, origin_path: Option<&str>| Origin {
            id: "primary".to_string(),
            domain_name: "origin.example.com".to_string(),
            origin_path: origin_path.map(str::to_string),
            custom_origin_config: config,
            ..Origin::default()
        };
        // (case, the origin, the scheme the viewer dialled, the viewer path, the query, the
        // origin URI)
        let cases: [(&str, Origin, &str, &str, &str, &str); 9] = [
            (
                "an https-only origin on the default port",
                origin(Some(custom("https-only", 80, 443)), None),
                "https",
                "/index.html",
                "",
                "https://origin.example.com/index.html",
            ),
            (
                "an http-only origin on the default port",
                origin(Some(custom("http-only", 80, 443)), None),
                "https",
                "/index.html",
                "",
                "http://origin.example.com/index.html",
            ),
            (
                "a match-viewer origin for a viewer that dialled plaintext",
                origin(Some(custom("match-viewer", 80, 443)), None),
                "http",
                "/index.html",
                "",
                "http://origin.example.com/index.html",
            ),
            (
                "a match-viewer origin for a viewer that dialled TLS",
                origin(Some(custom("match-viewer", 80, 443)), None),
                "https",
                "/index.html",
                "",
                "https://origin.example.com/index.html",
            ),
            (
                "a match-viewer origin on an http port of its own",
                origin(Some(custom("match-viewer", 8080, 443)), None),
                "http",
                "/index.html",
                "",
                "http://origin.example.com:8080/index.html",
            ),
            (
                "an https origin on a port of its own",
                origin(Some(custom("https-only", 80, 8443)), None),
                "https",
                "/index.html",
                "",
                "https://origin.example.com:8443/index.html",
            ),
            (
                "an origin path the viewer path is appended to",
                origin(Some(custom("https-only", 80, 443)), Some("/prefix")),
                "https",
                "/index.html",
                "",
                "https://origin.example.com/prefix/index.html",
            ),
            (
                "an S3 origin, over https, carrying the viewer query",
                origin(None, None),
                "http",
                "/key",
                "a=1",
                "https://origin.example.com/key?a=1",
            ),
            (
                // A domain name that already states a port is the whole address, so the
                // configured port is not written after it.
                "an origin whose domain name states the address it is served at",
                Origin {
                    domain_name: "127.0.0.1:34567".to_string(),
                    ..origin(Some(custom("http-only", 8080, 443)), None)
                },
                "https",
                "/index.html",
                "",
                "http://127.0.0.1:34567/index.html",
            ),
        ];
        for (case, origin, viewer_scheme, viewer_path, query, expected) in cases {
            let actual = origin_uri(&origin, None, viewer_scheme, viewer_path, query);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the origin URI {expected:?}, got {actual:?}"
            );
        }
    }

    /// A VPC ORIGIN states the protocol policy and the ports of its fetch on the VPC origin
    /// resource the origin NAMES, so an origin carrying no `CustomOriginConfig` at all is
    /// addressed by that endpoint configuration rather than as an Amazon S3 origin.
    #[test]
    fn a_vpc_origins_request_uri_is_addressed_by_its_endpoint_configuration() {
        let endpoint = |policy: &str, http_port: i32, https_port: i32| VpcOriginEndpointConfig {
            name: "endpoint".to_string(),
            arn: "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/app/edge/0"
                .to_string(),
            http_port,
            https_port,
            origin_protocol_policy: policy.to_string(),
            ..VpcOriginEndpointConfig::default()
        };
        let origin = |domain: &str| Origin {
            id: "vpc".to_string(),
            domain_name: domain.to_string(),
            vpc_origin_config: Some(crate::ops::types::VpcOriginConfig {
                vpc_origin_id: "vo_0123456789abcdef".to_string(),
                ..crate::ops::types::VpcOriginConfig::default()
            }),
            ..Origin::default()
        };
        // (case, the origin, the endpoint configuration of the VPC origin it names, the
        // scheme the viewer dialled, the origin URI)
        let cases: [(&str, Origin, VpcOriginEndpointConfig, &str, &str); 5] = [
            (
                "http-only on the default port, to a host",
                origin("origin.example.com"),
                endpoint("http-only", 80, 443),
                "https",
                "http://origin.example.com/index.html",
            ),
            (
                "https-only on a port of its own",
                origin("origin.example.com"),
                endpoint("https-only", 80, 8443),
                "http",
                "https://origin.example.com:8443/index.html",
            ),
            (
                "match-viewer for a viewer that dialled plaintext",
                origin("origin.example.com"),
                endpoint("match-viewer", 8080, 8443),
                "http",
                "http://origin.example.com:8080/index.html",
            ),
            (
                "match-viewer for a viewer that dialled TLS",
                origin("origin.example.com"),
                endpoint("match-viewer", 8080, 8443),
                "https",
                "https://origin.example.com:8443/index.html",
            ),
            (
                // The address the load balancer behind a VPC origin publishes names its own
                // port, and that address is where the fetch goes.
                "http-only, to the address the endpoint is served at",
                origin("127.0.0.1:34567"),
                endpoint("http-only", 80, 443),
                "https",
                "http://127.0.0.1:34567/index.html",
            ),
        ];
        for (case, origin, endpoint, viewer_scheme, expected) in cases {
            let actual = origin_uri(&origin, Some(&endpoint), viewer_scheme, "/index.html", "");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the origin URI {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn an_origin_addressed_over_a_scheme_it_does_not_serve_cannot_be_connected_to() {
        // Rows differ along the scheme the origin request is addressed over and along the
        // KIND of host it names: an AWS service endpoint this emulator serves (which is
        // served over TLS alone), a bucket's own virtual-hosted endpoint, a bucket's WEBSITE
        // endpoint (which is the other way round: plaintext and no TLS), a per-resource
        // listener of this emulator (which negotiates its encryption in band), and a host
        // nothing here serves.
        let cases: [(&str, &str, bool); 13] = [
            (
                "an api stage over TLS",
                "https://abcdef1234.execute-api.us-east-1.amazonaws.com/live/echo",
                true,
            ),
            (
                "an api stage over plaintext",
                "http://abcdef1234.execute-api.us-east-1.amazonaws.com/live/echo",
                false,
            ),
            (
                "a bucket endpoint over TLS",
                "https://shelf.s3.us-east-1.amazonaws.com/key",
                true,
            ),
            (
                "a bucket endpoint over plaintext",
                "http://shelf.s3.us-east-1.amazonaws.com/key",
                false,
            ),
            (
                "a bucket's website endpoint over plaintext",
                "http://shelf.s3-website-us-east-1.amazonaws.com/index.html",
                true,
            ),
            (
                "a bucket's website endpoint over TLS, which it does not serve",
                "https://shelf.s3-website-us-east-1.amazonaws.com/index.html",
                false,
            ),
            (
                "the dotted spelling of a website endpoint over plaintext",
                "http://shelf.s3-website.us-east-1.amazonaws.com/index.html",
                true,
            ),
            (
                "the dotted spelling of a website endpoint over TLS",
                "https://shelf.s3-website.us-east-1.amazonaws.com/index.html",
                false,
            ),
            (
                "a host whose label merely begins like the website endpoint, over plaintext",
                "http://shelf.s3-websitely.us-east-1.amazonaws.com/index.html",
                false,
            ),
            (
                "a per-resource listener over plaintext",
                "http://127.0.0.1:38211/index.html",
                true,
            ),
            (
                "a per-resource listener over TLS",
                "https://127.0.0.1:38211/index.html",
                true,
            ),
            (
                "a host nothing here serves, over plaintext",
                "http://origin.example.com/index.html",
                true,
            ),
            (
                "a distribution's own edge domain, over plaintext",
                "http://d111111abcdef8.cloudfront.net/index.html",
                true,
            ),
        ];
        for (case, uri, expected) in cases {
            let actual = origin_accepts_scheme(uri);
            assert_eq!(
                actual, expected,
                "case {case:?} ({uri:?}): expected origin_accepts_scheme == {expected}, got \
                 {actual}"
            );
        }
        // An origin-form URI names no scheme at all, so there is nothing to refuse.
        assert!(
            origin_accepts_scheme("/index.html"),
            "an origin-form URI names no scheme, so it is addressed over the one the request \
             is made on"
        );
    }

    #[test]
    fn an_origin_request_carries_no_viewer_credential_whatever_the_policy_forwards() {
        // The rows differ along the origin request policy's header behaviour — none (the
        // default), `allViewer` and `allViewerExceptHostHeader`, the AWS-managed policies
        // whose ids `MANAGED_ORIGIN_REQUEST_POLICIES` publishes — and along the viewer
        // header: a credential, a hop-by-hop header, and an ordinary one.
        const ALL_VIEWER: &str = "216adef6-5c7f-47e4-b989-5492eafa07d3";
        const ALL_VIEWER_EXCEPT_HOST: &str = "b689b0a8-53d0-40ab-baf2-68738e2966ac";
        let store = ComAmazonawsCloudfrontCloudfront20200531State::default();
        let origin = Origin {
            id: "primary".to_string(),
            domain_name: "origin.example.com".to_string(),
            ..Origin::default()
        };
        // (case, the policy the behaviour names, the viewer header, forwarded)
        let cases: [(&str, Option<&str>, &str, bool); 9] = [
            ("no policy, a credential", None, "authorization", false),
            ("no policy, an ordinary header", None, "accept-language", false),
            (
                "allViewer, a credential",
                Some(ALL_VIEWER),
                "authorization",
                false,
            ),
            (
                "allViewer, a session token",
                Some(ALL_VIEWER),
                "x-amz-security-token",
                false,
            ),
            (
                "allViewer, a hop-by-hop header",
                Some(ALL_VIEWER),
                "connection",
                false,
            ),
            (
                "allViewer, an ordinary header",
                Some(ALL_VIEWER),
                "accept-language",
                true,
            ),
            (
                "allViewerExceptHostHeader, a credential",
                Some(ALL_VIEWER_EXCEPT_HOST),
                "authorization",
                false,
            ),
            (
                "allViewerExceptHostHeader, an ordinary header",
                Some(ALL_VIEWER_EXCEPT_HOST),
                "accept-language",
                true,
            ),
            (
                "a policy this account does not hold",
                Some("00000000-0000-0000-0000-000000000000"),
                "accept-language",
                false,
            ),
        ];
        for (case, policy, header, forwarded) in cases {
            let behavior = Behavior {
                path_pattern: None,
                realtime_log_config_arn: None,
                target_origin_id: "primary".to_string(),
                viewer_protocol_policy: "allow-all".to_string(),
                allowed_methods: None,
                trusted_key_groups: None,
                cache_policy_id: None,
                origin_request_policy_id: policy.map(str::to_string),
                compress: false,
                min_ttl: None,
                default_ttl: None,
                max_ttl: None,
                forwarded_values: None,
                response_headers_policy_id: None,
                function_associations: None,
                lambda_function_associations: None,
            };
            let mut req = RtRequest::new(SdkBody::empty());
            req.set_uri("/index.html").expect("a valid URI");
            req.headers_mut().insert(header.to_string(), "v".to_string());
            let headers =
                origin_request_headers(&store, &behavior, &origin, &req, "cfid", VIEWER_HOST);
            let carried = headers.iter().any(|(name, _)| name == header);
            assert_eq!(
                carried, forwarded,
                "case {case:?}: expected the origin request to carry {header:?} == \
                 {forwarded}, got {carried} (headers {headers:?})"
            );
            // The origin here carries no `CustomOriginConfig`, so it is an Amazon S3 origin:
            // such an origin is addressed by its OWN host whatever a behaviour forwards
            // ([`origin_request_host`]), and the request names what it is being made for.
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "host" && value == &origin.domain_name)
                    && headers.iter().any(|(name, _)| name == X_AMZ_CF_ID),
                "case {case:?}: expected the origin's host and an X-Amz-Cf-Id, got \
                 {headers:?}"
            );
        }
    }

    /// The host the viewer addressed in the rows below: the name a distribution publishes.
    const VIEWER_HOST: &str = "d111111abcdef8.cloudfront.net";

    #[test]
    fn an_origin_request_carries_the_viewers_host_exactly_where_the_behaviour_forwards_it() {
        // Rows differ along what the rule generalises over: WHICH reading names the headers
        // that reach the origin (an AWS-managed origin request policy, the legacy
        // `ForwardedValues` whitelist, neither), the header BEHAVIOUR that reading carries
        // (`allViewer`, `allViewerExceptHostHeader`, a whitelist that names `Host`, one that
        // names another header, the list naming every header), and the KIND of origin (a
        // custom origin, an Amazon S3 one).
        const ALL_VIEWER: &str = "216adef6-5c7f-47e4-b989-5492eafa07d3";
        const ALL_VIEWER_EXCEPT_HOST: &str = "b689b0a8-53d0-40ab-baf2-68738e2966ac";
        const ORIGIN_HOST: &str = "origin.example.com";
        let store = ComAmazonawsCloudfrontCloudfront20200531State::default();
        let custom = Some(CustomOriginConfig {
            http_port: 80,
            https_port: 443,
            origin_protocol_policy: "https-only".to_string(),
            ..CustomOriginConfig::default()
        });
        let whitelist = |names: &[&str]| ForwardedValues {
            query_string: false,
            headers: Some(crate::ops::types::Headers {
                quantity: i32::try_from(names.len()).expect("a small list"),
                items: Some(names.iter().map(|name| (*name).to_string()).collect()),
            }),
            ..ForwardedValues::default()
        };
        // (case, the origin request policy, the legacy member, a custom origin, the host)
        type Row = (&'static str, Option<&'static str>, Option<ForwardedValues>, bool, &'static str);
        let cases: [Row; 8] = [
            ("no policy and no legacy member", None, None, true, ORIGIN_HOST),
            ("allViewer over a custom origin", Some(ALL_VIEWER), None, true, VIEWER_HOST),
            ("allViewer over an S3 origin", Some(ALL_VIEWER), None, false, ORIGIN_HOST),
            (
                "allViewerExceptHostHeader over a custom origin",
                Some(ALL_VIEWER_EXCEPT_HOST),
                None,
                true,
                ORIGIN_HOST,
            ),
            (
                "a legacy whitelist naming Host",
                None,
                Some(whitelist(&["Host"])),
                true,
                VIEWER_HOST,
            ),
            (
                "a legacy whitelist naming another header",
                None,
                Some(whitelist(&["Accept-Language"])),
                true,
                ORIGIN_HOST,
            ),
            (
                "a legacy whitelist naming every header",
                None,
                Some(whitelist(&["*"])),
                true,
                VIEWER_HOST,
            ),
            (
                "a legacy whitelist naming every header, over an S3 origin",
                None,
                Some(whitelist(&["*"])),
                false,
                ORIGIN_HOST,
            ),
        ];
        for (case, policy, forwarded_values, custom_origin, expected) in cases {
            let origin = Origin {
                id: "primary".to_string(),
                domain_name: ORIGIN_HOST.to_string(),
                custom_origin_config: custom_origin.then(|| custom.clone().expect("the config")),
                ..Origin::default()
            };
            let behavior = Behavior {
                path_pattern: None,
                realtime_log_config_arn: None,
                target_origin_id: "primary".to_string(),
                viewer_protocol_policy: "allow-all".to_string(),
                allowed_methods: None,
                trusted_key_groups: None,
                cache_policy_id: None,
                origin_request_policy_id: policy.map(str::to_string),
                compress: false,
                min_ttl: None,
                default_ttl: None,
                max_ttl: None,
                forwarded_values,
                response_headers_policy_id: None,
                function_associations: None,
                lambda_function_associations: None,
            };
            let mut req = RtRequest::new(SdkBody::empty());
            req.set_uri("/index.html").expect("a valid URI");
            req.headers_mut()
                .insert("host".to_string(), VIEWER_HOST.to_string());
            let headers =
                origin_request_headers(&store, &behavior, &origin, &req, "cfid", VIEWER_HOST);
            let carried: Vec<&String> = headers
                .iter()
                .filter(|(name, _)| name == "host")
                .map(|(_, value)| value)
                .collect();
            assert_eq!(
                carried,
                vec![&expected.to_string()],
                "case {case:?}: expected the origin request to carry the one host \
                 {expected:?}, got {carried:?} (headers {headers:?})"
            );
        }
    }

    #[test]
    fn an_alias_may_not_claim_an_address_this_emulator_is_reached_at() {
        // Rows differ along the shape of the stored alternate domain name: a customer
        // domain, the loopback names and IP literals a client on this machine dials, a
        // host that names a service endpoint, and the empty name.
        let cases: [(&str, &str, bool); 11] = [
            ("a customer domain", "cdn.example.com", true),
            ("a wildcard-free subdomain", "assets.cdn.example.com", true),
            ("the loopback name", "localhost", false),
            ("the loopback name with a port", "localhost:9297", false),
            ("a name under the loopback name", "edge.localhost", false),
            ("an IPv4 literal", "127.0.0.1", false),
            ("an IPv4 literal with a port", "127.0.0.1:9297", false),
            ("a bracketed IPv6 literal", "[::1]:9297", false),
            ("another service's endpoint", "s3.us-east-1.amazonaws.com", false),
            ("an edge domain CloudFront assigns", "d111.cloudfront.net", false),
            ("the empty name", "", false),
        ];
        for (case, alias, expected) in cases {
            let actual = addressable_alias(alias);
            assert_eq!(
                actual, expected,
                "case {case:?} ({alias:?}): expected addressable_alias == {expected}, got \
                 {actual}"
            );
        }
    }

    /// The policies one behaviour names, held in a store so the shaping below reads them
    /// exactly as a served request does: a cache policy that keys on one cookie and
    /// normalizes the content coding, and an origin request policy that forwards one other
    /// cookie, one viewer header and one header the EDGE generates.
    fn store_with_policies() -> ComAmazonawsCloudfrontCloudfront20200531State {
        use crate::ops::types::{
            CachePolicyConfig, CachePolicyCookiesConfig, CachePolicyHeadersConfig,
            CachePolicyQueryStringsConfig, CookieNames, Headers,
            OriginRequestPolicyConfig, OriginRequestPolicyCookiesConfig,
            OriginRequestPolicyHeadersConfig, OriginRequestPolicyQueryStringsConfig,
            ParametersInCacheKeyAndForwardedToOrigin, QueryStringNames,
        };
        let name_list = |names: &[&str]| {
            Some((
                i32::try_from(names.len()).unwrap_or_default(),
                Some(names.iter().map(|name| (*name).to_string()).collect()),
            ))
        };
        let mut store = ComAmazonawsCloudfrontCloudfront20200531State::default();
        let cookies = name_list(&["sim_key"]).expect("the list is not empty");
        store.cache_policies.insert(
            CACHE_POLICY.to_string(),
            crate::state::StoredCachePolicy {
                id: CACHE_POLICY.to_string(),
                config: CachePolicyConfig {
                    name: "keys-on-one-cookie".to_string(),
                    min_ttl: 0,
                    default_ttl: Some(60),
                    max_ttl: Some(60),
                    comment: None,
                    parameters_in_cache_key_and_forwarded_to_origin: Some(
                        ParametersInCacheKeyAndForwardedToOrigin {
                            enable_accept_encoding_gzip: true,
                            enable_accept_encoding_brotli: Some(true),
                            headers_config: CachePolicyHeadersConfig {
                                header_behavior: "none".to_string(),
                                headers: None,
                            },
                            cookies_config: CachePolicyCookiesConfig {
                                cookie_behavior: "whitelist".to_string(),
                                cookies: Some(CookieNames {
                                    quantity: cookies.0,
                                    items: cookies.1,
                                }),
                            },
                            query_strings_config: CachePolicyQueryStringsConfig {
                                query_string_behavior: "none".to_string(),
                                query_strings: None,
                            },
                        },
                    ),
                },
                last_modified_time: SystemTime::UNIX_EPOCH,
                etag: "E1".to_string(),
            },
        );
        let headers = name_list(&["x-fwd", "CloudFront-Viewer-Country"])
            .expect("the list is not empty");
        let forwarded_cookies = name_list(&["sim_fwd"]).expect("the list is not empty");
        let forwarded_queries = name_list(&["keep"]).expect("the list is not empty");
        store.origin_request_policies.insert(
            ORIGIN_REQUEST_POLICY.to_string(),
            crate::state::StoredOriginRequestPolicy {
                id: ORIGIN_REQUEST_POLICY.to_string(),
                config: OriginRequestPolicyConfig {
                    name: "forwards-what-the-key-excludes".to_string(),
                    comment: None,
                    headers_config: OriginRequestPolicyHeadersConfig {
                        header_behavior: "whitelist".to_string(),
                        headers: Some(Headers {
                            quantity: headers.0,
                            items: headers.1,
                        }),
                    },
                    cookies_config: OriginRequestPolicyCookiesConfig {
                        cookie_behavior: "whitelist".to_string(),
                        cookies: Some(CookieNames {
                            quantity: forwarded_cookies.0,
                            items: forwarded_cookies.1,
                        }),
                    },
                    query_strings_config: OriginRequestPolicyQueryStringsConfig {
                        query_string_behavior: "whitelist".to_string(),
                        query_strings: Some(QueryStringNames {
                            quantity: forwarded_queries.0,
                            items: forwarded_queries.1,
                        }),
                    },
                },
                last_modified_time: SystemTime::UNIX_EPOCH,
                etag: "E2".to_string(),
            },
        );
        store
    }

    /// The id the cache policy of [`store_with_policies`] is held under.
    const CACHE_POLICY: &str = "11111111-1111-1111-1111-111111111111";

    /// The id the origin request policy of [`store_with_policies`] is held under.
    const ORIGIN_REQUEST_POLICY: &str = "22222222-2222-2222-2222-222222222222";

    /// RULE: the origin request carries the UNION of what the cache key names and what the
    /// origin request policy forwards, and nothing else — and a header CloudFront generates
    /// itself carries the EDGE's value, never a value the viewer stated for it.
    ///
    /// The rows differ along what the viewer sent and what is asked about: a cookie the cache
    /// key names, a cookie only the origin request policy names, a cookie neither names, a
    /// header the policy names, a header it does not, a header the edge generates (both for a
    /// viewer that stated nothing and for one that stated a value of its own), and the
    /// content coding the cache policy normalizes.
    #[test]
    fn the_origin_request_carries_what_the_policies_name_and_the_edge_generated_values() {
        let store = store_with_policies();
        let origin = Origin {
            id: "primary".to_string(),
            domain_name: "origin.example.com".to_string(),
            ..Origin::default()
        };
        let behavior = behavior_under_test();
        // What one viewer sent: two cookies the policies name and one neither does, a header
        // the policy names and one it does not, a value spoofed for a header the edge
        // generates, and a content coding it accepts.
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_uri("/obj?keep=1&drop=2").expect("a valid URI");
        for (name, value) in [
            (COOKIE, "sim_fwd=c1; sim_unl=leak; sim_key=k1"),
            ("x-fwd", "forwarded"),
            ("x-unlisted", "withheld"),
            (generated_headers::VIEWER_COUNTRY, "ZZ"),
            (ACCEPT_ENCODING, "gzip, deflate, br"),
        ] {
            req.headers_mut().insert(name.to_string(), value.to_string());
        }
        let headers =
            origin_request_headers(&store, &behavior, &origin, &req, "cfid", VIEWER_HOST);
        let carried = |name: &str| {
            headers
                .iter()
                .find(|(carried, _)| carried == name)
                .map(|(_, value)| value.clone())
        };

        // (case, the header the origin request carries, the value it must carry)
        let cases: [(&str, &str, Option<&str>); 7] = [
            (
                "the cookies the two policies name, in name order, and no other",
                COOKIE,
                Some("sim_fwd=c1; sim_key=k1"),
            ),
            (
                "a viewer header the origin request policy names",
                "x-fwd",
                Some("forwarded"),
            ),
            (
                "a viewer header neither policy names",
                "x-unlisted",
                None,
            ),
            (
                "the country the EDGE resolved, not the one the viewer stated",
                generated_headers::VIEWER_COUNTRY,
                Some("US"),
            ),
            (
                "a generated header the policy does not name",
                generated_headers::FORWARDED_PROTO,
                None,
            ),
            (
                "the normalized content coding, which takes precedence for brotli",
                ACCEPT_ENCODING,
                Some("br"),
            ),
            (
                "the origin's own host, the policies naming no viewer Host beside it",
                "host",
                Some("origin.example.com"),
            ),
        ];
        for (case, header, expected) in cases {
            let actual = carried(header);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected the origin request to carry {header:?} == \
                 {expected:?}, got {actual:?} (headers {headers:?})"
            );
        }
    }

    #[test]
    fn the_origin_is_asked_for_the_query_strings_the_policies_name() {
        let store = store_with_policies();
        let keyed = forwarding::cache_key_parameters(&store, &behavior_under_test());
        let forwarded = forwarding::forwarded_parameters(&store, &behavior_under_test());
        // Rows differ along the viewer's query: a named parameter, an unnamed one, both, the
        // order they arrived in, and no query at all.
        let cases: [(&str, &str, &str); 5] = [
            ("a named parameter alone", "keep=1", "keep=1"),
            ("an unnamed parameter is withheld", "drop=2", ""),
            (
                "the named one travels and the unnamed one does not",
                "drop=2&keep=1",
                "keep=1",
            ),
            (
                "the viewer's order is kept",
                "keep=1&drop=2&keep=3",
                "keep=1&keep=3",
            ),
            ("no query at all", "", ""),
        ];
        for (case, query, expected) in cases {
            let actual = origin_query(&keyed, &forwarded, query);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the origin to be asked for {expected:?}, got \
                 {actual:?}"
            );
        }
    }

    /// The behaviour the two shaping tests above drive: it names both policies of
    /// [`store_with_policies`] and asks for automatic compression.
    fn behavior_under_test() -> Behavior {
        Behavior {
            path_pattern: None,
            realtime_log_config_arn: None,
            target_origin_id: "primary".to_string(),
            viewer_protocol_policy: "allow-all".to_string(),
            allowed_methods: None,
            trusted_key_groups: None,
            cache_policy_id: Some(CACHE_POLICY.to_string()),
            origin_request_policy_id: Some(ORIGIN_REQUEST_POLICY.to_string()),
            compress: true,
            min_ttl: None,
            default_ttl: None,
            max_ttl: None,
            forwarded_values: None,
            response_headers_policy_id: None,
            function_associations: None,
            lambda_function_associations: None,
        }
    }

    #[test]
    fn a_request_that_has_already_been_through_an_edge_is_recognised() {
        // Rows differ along the `Via` the request carries: one an edge of this emulator
        // stamped, one another proxy did, a chain ending in one, and none at all.
        let cases: [(&str, Option<&str>, bool); 4] = [
            ("no Via at all", None, false),
            (
                "a Via an edge stamped",
                Some("1.1 cfid.cloudfront.net (CloudFront)"),
                true,
            ),
            ("another proxy's Via", Some("1.1 proxy.internal"), false),
            (
                "a chain whose last hop is an edge",
                Some("1.1 proxy.internal, 1.1 cfid.cloudfront.net (CloudFront)"),
                true,
            ),
        ];
        for (case, via, expected) in cases {
            let mut req = RtRequest::new(SdkBody::empty());
            req.set_uri("/index.html").expect("a valid URI");
            if let Some(via) = via {
                req.headers_mut().insert("via".to_string(), via.to_string());
            }
            let actual = carries_cloudfront_via(&req);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected carries_cloudfront_via == {expected}, got {actual}"
            );
        }
    }
}
