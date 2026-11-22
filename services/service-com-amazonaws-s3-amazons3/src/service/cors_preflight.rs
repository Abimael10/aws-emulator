// SPDX-License-Identifier: CC0-1.0

//! The CORS PREFLIGHT: what this service answers an `OPTIONS` request addressed to a bucket
//! or to one of its objects.
//!
//! A browser that is about to send a cross-origin request a simple form could not send asks
//! the resource first: "the browser sends a preflight `OPTIONS` request" carrying the origin
//! it is acting for (`Origin`), the method it intends to use
//! (`Access-Control-Request-Method`) and the headers it intends to send
//! (`Access-Control-Request-Headers`). Amazon S3 answers that question from the bucket's own
//! cors configuration — the `CORSRule` set `PutBucketCors` wrote and `GetBucketCors` reports
//! — and nothing else about the request: the preflight is UNSIGNED (a browser cannot sign
//! it) and is not a modelled operation of this service's API at all, which is why no routing
//! key names it and this module answers it through
//! [`super::EmulatedService::override_routing_error`].
//!
//! The rules it implements, each from the published behaviour of that answer:
//!
//! * a rule of the bucket's configuration that admits the origin, the intended method AND
//!   every intended header makes the answer `200` carrying that rule as the
//!   `Access-Control-Allow-*` headers a browser then applies: the origin it may act for, the
//!   methods it may use, the headers it may send, how long it may cache this answer
//!   (`CORSRule$MaxAgeSeconds`) and the response headers the page may read
//!   (`CORSRule$ExposeHeaders`);
//! * a bucket whose configuration no rule of admits that combination is refused `403`, and so
//!   is one carrying NO cors configuration at all (and one whose configuration
//!   `DeleteBucketCors` has removed): a resource with no cors configuration shares nothing
//!   cross-origin, and the two refusals are told apart by their sentence
//!   ([`NOT_ALLOWED_MESSAGE`], [`NOT_ENABLED_MESSAGE`]) because the code is the same;
//! * a preflight that names no `Origin` asks nothing a cors rule could be matched against,
//!   which S3 answers `400` ([`MISSING_ORIGIN_MESSAGE`]).
//!
//! WHICH matching is published: a rule's `AllowedOrigin` and its `AllowedHeader` entries "can
//! optionally contain at most one wildcard character (*)", and `AllowedMethod` is one of the
//! methods S3 serves ([`crate::state::CORS_ALLOWED_METHODS`]); the rules are consulted in the
//! order the configuration states them and the FIRST one that admits the request is the one
//! whose values the answer carries.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::smithy_server_runtime::ServerHttpProtocol;

/// The method a CORS preflight is sent with.
const PREFLIGHT_METHOD: &str = "OPTIONS";

/// The request headers a preflight asks its question in.
const ORIGIN_HEADER: &str = "origin";
/// See [`ORIGIN_HEADER`].
const REQUEST_METHOD_HEADER: &str = "access-control-request-method";
/// See [`ORIGIN_HEADER`].
const REQUEST_HEADERS_HEADER: &str = "access-control-request-headers";

/// The response headers the answer carries the matched rule in.
const ALLOW_ORIGIN_HEADER: &str = "access-control-allow-origin";
/// See [`ALLOW_ORIGIN_HEADER`].
const ALLOW_METHODS_HEADER: &str = "access-control-allow-methods";
/// See [`ALLOW_ORIGIN_HEADER`].
const ALLOW_HEADERS_HEADER: &str = "access-control-allow-headers";
/// See [`ALLOW_ORIGIN_HEADER`].
const MAX_AGE_HEADER: &str = "access-control-max-age";
/// See [`ALLOW_ORIGIN_HEADER`].
const EXPOSE_HEADERS_HEADER: &str = "access-control-expose-headers";

/// The header saying a browser may send the request's credentials, and the value S3 answers a
/// matched preflight with: a cross-origin call a bucket's cors configuration admits may carry
/// the caller's own credential, which is what a presigned or signed browser upload does.
const ALLOW_CREDENTIALS_HEADER: &str = "access-control-allow-credentials";
/// See [`ALLOW_CREDENTIALS_HEADER`].
const ALLOW_CREDENTIALS: &str = "true";

/// The header naming the request fields this answer depends on, so a cache does not serve one
/// origin's answer to another: the three a preflight asks its question in.
const VARY_HEADER: &str = "vary";
/// See [`VARY_HEADER`].
const VARY_VALUE: &str = "Origin, Access-Control-Request-Headers, Access-Control-Request-Method";

/// The value an `AllowedOrigin` or an `AllowedHeader` admits everything with, and the
/// character either may carry "at most one" of (`CORSRule$AllowedOrigins`).
///
/// The store's own spelling, which is also what `PutBucketCors` counts a pattern's wildcards
/// against ([`crate::state::CORS_WILDCARD`]): the matcher below and the validation that
/// admits a pattern into the configuration at all read the same character.
const WILDCARD: &str = crate::state::CORS_WILDCARD;

/// The most entries of a preflight's `Access-Control-Request-Headers` one answer is matched
/// against.
///
/// A bound, not a fidelity choice: the work one preflight asks for is the PRODUCT of the
/// entries it names and the headers the bucket's configuration allows (the configuration's
/// half is bounded by [`crate::state::MAX_CORS_RULES`] and
/// [`crate::state::MAX_CORS_CONFIGURATION_BYTES`]), so without a cap a request would choose
/// one factor of it. A browser announces a handful — a list longer than this is not one a cors
/// rule could allow, so it earns the same answer a header no rule allows does. The same
/// construction, for the same reason, bounds the signed-header list a canonical request is
/// built from ([`super::sigv4`]).
const MAX_REQUESTED_HEADERS: usize = 32;

/// The error code S3 refuses a cors request with, for both of the refusals below.
const CORS_REFUSAL_CODE: &str = "AccessForbidden";
/// The status that code is answered with.
const CORS_REFUSAL_STATUS: u16 = 403;

/// The sentence S3 refuses a preflight no rule of the bucket's cors configuration admits
/// with: it names each of the three things a rule is matched on, because any one of them can
/// be what no rule admitted.
const NOT_ALLOWED_MESSAGE: &str =
    "CORSResponse: This CORS request is not allowed. This is usually because the evalution of \
     Origin, request method / Access-Control-Request-Method or Access-Control-Request-Headers \
     are not whitelisted by the resource's CORS spec.";
/// The sentence S3 refuses a preflight against a bucket carrying NO cors configuration with
/// — the answer a bucket that was never configured, and one whose configuration was deleted,
/// both earn.
const NOT_ENABLED_MESSAGE: &str = "CORSResponse: CORS is not enabled for this bucket.";

/// The `<ResourceType>` a cors refusal names: the BUCKET whose configuration shares nothing
/// at all, and the resource the request ADDRESSED for a request its configuration does not
/// admit — the first decision is taken about the bucket's configuration before the address is
/// resolved any further, the second about the resource that configuration was consulted for.
const BUCKET_RESOURCE: &str = "BUCKET";
/// See [`BUCKET_RESOURCE`].
const OBJECT_RESOURCE: &str = "OBJECT";

/// The code and sentence S3 answers a preflight that names no `Origin`: without one there is
/// nothing for a rule to be matched against.
const MISSING_ORIGIN_CODE: &str = "BadRequest";
/// See [`MISSING_ORIGIN_CODE`].
const MISSING_ORIGIN_STATUS: u16 = 400;
/// See [`MISSING_ORIGIN_CODE`].
const MISSING_ORIGIN_MESSAGE: &str = "Insufficient information. Origin request header needed.";

/// The status S3 answers [`crate::state::NO_SUCH_BUCKET_CODE`] with: 404, the one every call
/// on a bucket this emulator holds none under earns.
const NO_SUCH_BUCKET_STATUS: u16 = 404;

/// What the addressed bucket's cors configuration is, as this surface reads it.
enum Configured {
    /// This emulator holds no bucket under that name at all.
    NoBucket,
    /// The bucket is there and carries no cors configuration — it shares nothing
    /// cross-origin.
    NoConfiguration,
    /// The rules the bucket's configuration states, in the order it states them.
    Rules(Vec<crate::state::BucketCorsRule>),
}

/// What this service answers the CORS preflight `req`, or `None` for a request that is not
/// one — which keeps whatever answer the caller would otherwise give.
///
/// `path_style` is the request's address in the PATH-STYLE form every route of this service
/// is bound in ([`super::EmulatedService::path_style_path`]), so a virtual-hosted-style
/// preflight — the form a browser sends for an endpoint addressed by bucket name — is
/// answered from the same bucket's configuration a path-style one is.
///
/// Read-only: the guard is taken, the bucket's cors configuration is copied out, and nothing
/// is written. A preflight is a QUESTION about the configuration.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    path_style: &str,
) -> Option<RtResponse> {
    if !req.method().eq_ignore_ascii_case(PREFLIGHT_METHOD) {
        return None;
    }
    // The bucket the preflight is about. An address that names none is no question about a
    // cors configuration, so it keeps the answer it would otherwise have earned.
    let bucket = super::addressed_bucket_name(path_style)?;
    let header = |name: &str| {
        req.headers()
            .get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let Some(origin) = header(ORIGIN_HEADER) else {
        return missing_origin(protocol);
    };
    // The bucket's own configuration, copied out under the guard — and whether the bucket is
    // THERE, which is a different answer: a name this emulator holds no bucket under is not a
    // bucket that shares nothing, it is no bucket at all.
    let configured = crate::state::service_state(engine)
        .map(|state| {
            let store = state.lock();
            match store.bucket(&bucket).map(crate::state::Bucket::cors) {
                None => Configured::NoBucket,
                Some(None) => Configured::NoConfiguration,
                Some(Some(rules)) => Configured::Rules(rules.to_vec()),
            }
        })
        .unwrap_or(Configured::NoBucket);
    let method = header(REQUEST_METHOD_HEADER).unwrap_or_default();
    let rules = match configured {
        Configured::NoBucket => return no_such_bucket(protocol, &bucket),
        Configured::NoConfiguration => {
            return refusal(protocol, NOT_ENABLED_MESSAGE, method, BUCKET_RESOURCE)
        }
        Configured::Rules(rules) => rules,
    };
    let requested_headers = requested_headers(header(REQUEST_HEADERS_HEADER));
    let Some(rule) = rules
        .iter()
        .find(|rule| admits(rule, origin, method, &requested_headers))
    else {
        // The resource the configuration was consulted FOR, which is what the request
        // addressed: a key under the bucket, or the bucket itself.
        let addressed = match super::addresses_object(path_style) {
            true => OBJECT_RESOURCE,
            false => BUCKET_RESOURCE,
        };
        return refusal(protocol, NOT_ALLOWED_MESSAGE, method, addressed);
    };
    Some(allowed(rule, origin, &requested_headers))
}

/// The headers a preflight says it intends to send, as the request spelled them:
/// `Access-Control-Request-Headers` is "a comma-delimited list of the HTTP headers" the
/// browser will send, so each entry is one header name and an empty list is a request that
/// will send none of its own.
fn requested_headers(header: Option<&str>) -> Vec<&str> {
    header
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Whether `rule` admits a preflight acting for `origin`, intending `method` and intending to
/// send `headers`.
///
/// All three have to hold: a rule states the origins it shares with, the methods it shares
/// through and the request headers it accepts, and a preflight that differs in any of them is
/// not the request the rule describes. A preflight naming no method at all names none a rule
/// can allow, so no rule admits it; neither is a header list longer than
/// [`MAX_REQUESTED_HEADERS`] one a rule allows.
fn admits(
    rule: &crate::state::BucketCorsRule,
    origin: &str,
    method: &str,
    headers: &[&str],
) -> bool {
    if headers.len() > MAX_REQUESTED_HEADERS {
        return false;
    }
    let origin_allowed = rule
        .allowed_origins
        .iter()
        .any(|allowed| matches_pattern(allowed, origin, true));
    let method_allowed = !method.is_empty()
        && rule
            .allowed_methods
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(method));
    let headers_allowed = headers.iter().all(|wanted| {
        rule.allowed_headers
            .iter()
            .any(|allowed| matches_pattern(allowed, wanted, false))
    });
    origin_allowed && method_allowed && headers_allowed
}

/// Whether `value` is what `pattern` names, where a pattern "can optionally contain at most
/// one wildcard character (*)" standing for any run of characters.
///
/// `case_sensitive` is what the compared thing is: an ORIGIN is a URI whose host and scheme a
/// browser sends in one spelling and which S3 compares as given, while a HEADER NAME is
/// case-insensitive by HTTP's own rules, so a rule allowing `Content-Type` allows the
/// `content-type` a browser announces.
///
/// Not `service_common::iam_authorization::wildcard_match`, which this crate already uses for
/// a policy element: that matcher implements IAM's wildcards, where `?` stands for exactly one
/// character too. A cors rule's `?` is a literal — `*` is the only wildcard it has, and at
/// most one of them — so a rule allowing `x-amz-?` would match a header name it does not name.
///
/// The ends are compared through `str::get`, which answers `None` for a byte index inside a
/// character rather than panicking: the offsets come from the PATTERN's lengths and the value
/// is a caller's header, so one need not divide the other on a character boundary — and a
/// value whose bytes do not divide there cannot be the text the pattern names.
fn matches_pattern(pattern: &str, value: &str, case_sensitive: bool) -> bool {
    let equal = |left: &str, right: &str| {
        if case_sensitive {
            left == right
        } else {
            left.eq_ignore_ascii_case(right)
        }
    };
    let Some((prefix, suffix)) = pattern.split_once(WILDCARD) else {
        return equal(pattern, value);
    };
    if value.len() < prefix.len() + suffix.len() {
        return false;
    }
    let head = value.get(..prefix.len());
    let tail = value.get(value.len() - suffix.len()..);
    match (head, tail) {
        (Some(head), Some(tail)) => equal(head, prefix) && equal(tail, suffix),
        _ => false,
    }
}

/// The `200` answer a preflight `rule` admits earns: the rule's own values, as the
/// `Access-Control-*` headers a browser applies.
///
/// The ORIGIN reported back is the request's own unless the rule shares with every origin, in
/// which case it is the wildcard the rule states — which is what lets a browser cache one
/// answer for a resource open to all. The HEADERS reported back are the ones the request said
/// it would send: the rule may allow more (or allow them by wildcard), and what the browser
/// needs told is that the ones it asked about are admitted.
fn allowed(rule: &crate::state::BucketCorsRule, origin: &str, headers: &[&str]) -> RtResponse {
    let shares_with_every_origin = rule
        .allowed_origins
        .iter()
        .any(|allowed| allowed == WILDCARD);
    let mut response = answered(200);
    insert_header(
        &mut response,
        ALLOW_ORIGIN_HEADER,
        if shares_with_every_origin {
            WILDCARD
        } else {
            origin
        },
    );
    insert_header(
        &mut response,
        ALLOW_METHODS_HEADER,
        &rule.allowed_methods.join(", "),
    );
    insert_header(&mut response, ALLOW_CREDENTIALS_HEADER, ALLOW_CREDENTIALS);
    if !headers.is_empty() {
        insert_header(&mut response, ALLOW_HEADERS_HEADER, &headers.join(", "));
    }
    if let Some(seconds) = rule.max_age_seconds {
        insert_header(&mut response, MAX_AGE_HEADER, &seconds.to_string());
    }
    if !rule.expose_headers.is_empty() {
        insert_header(
            &mut response,
            EXPOSE_HEADERS_HEADER,
            &rule.expose_headers.join(", "),
        );
    }
    response
}

/// One answer of this surface: the status, an empty body, the header naming what the answer
/// depends on, and the correlation pair every answer of this service carries
/// ([`crate::state::request_ids`]).
fn answered(status: u16) -> RtResponse {
    let status = StatusCode::try_from(status)
        .unwrap_or_else(|_| StatusCode::try_from(500).expect("500 is a status"));
    let mut response = RtResponse::new(status, SdkBody::empty());
    let correlation = crate::state::request_ids::answering()
        .unwrap_or_else(crate::state::request_ids::RequestCorrelation::mint);
    insert_header(&mut response, "x-amz-request-id", &correlation.request_id);
    insert_header(&mut response, "x-amz-id-2", &correlation.extended_request_id);
    insert_header(&mut response, VARY_HEADER, VARY_VALUE);
    insert_header(&mut response, "content-length", "0");
    response
}

/// `code`, `status` and `message` rendered through the SAME protocol every other answer of
/// this service is rendered through, so the envelope a client parses — the bare `<Error>` root
/// with its `<Code>`, `<Message>` and request-correlation elements — is the one every other S3
/// refusal carries.
///
/// A CORS refusal carries two members beside them: the `<Method>` the preflight said it
/// intended to use and the `<ResourceType>` the refusal is about
/// ([`crate::scaffolded::ErrorDetail::Method`], the pair S3 names a method refused against a
/// kind of resource by), because the code is the same for both refusals and the sentence is
/// the only other thing that tells them apart.
///
/// `None` on the serializer failure a fixed code and a fixed sentence cannot actually provoke,
/// which leaves the caller's own answer rather than panicking in the request path.
fn refusal(
    protocol: &dyn ServerHttpProtocol,
    message: &str,
    method: &str,
    resource_type: &str,
) -> Option<RtResponse> {
    let document = crate::scaffolded::UnmodeledWithDetail {
        message: message.to_string(),
        detail: crate::scaffolded::ErrorDetail::Method {
            method: method.to_string(),
            resource_type: resource_type.to_string(),
        },
    };
    rendered(
        protocol,
        &document,
        CORS_REFUSAL_CODE,
        CORS_REFUSAL_STATUS,
    )
}

/// The answer a preflight naming no `Origin` earns: nothing about it says which origin a rule
/// would have to admit, so there is no resource type and no method to name either.
fn missing_origin(protocol: &dyn ServerHttpProtocol) -> Option<RtResponse> {
    let document = crate::scaffolded::Unmodeled {
        code: MISSING_ORIGIN_CODE,
        status: MISSING_ORIGIN_STATUS,
        message: MISSING_ORIGIN_MESSAGE.to_string(),
    };
    rendered(
        protocol,
        &document,
        MISSING_ORIGIN_CODE,
        MISSING_ORIGIN_STATUS,
    )
}

/// The answer a preflight addressed to a bucket this emulator holds none under earns: there is
/// no configuration to consult, and no cors rule could be stated for a bucket that is not
/// there — so the answer is the one every call on an absent bucket earns, naming it in the
/// `<BucketName>` member S3's document carries it in.
fn no_such_bucket(protocol: &dyn ServerHttpProtocol, bucket: &str) -> Option<RtResponse> {
    let document = crate::scaffolded::UnmodeledWithDetail {
        message: crate::state::NO_SUCH_BUCKET_MESSAGE.to_string(),
        detail: crate::scaffolded::ErrorDetail::Bucket(bucket.to_string()),
    };
    rendered(
        protocol,
        &document,
        crate::state::NO_SUCH_BUCKET_CODE,
        NO_SUCH_BUCKET_STATUS,
    )
}

/// One refusal document, serialized in this service's envelope — the one place the two above
/// share, so both carry the same `<Error>` root and the same correlation pair.
fn rendered(
    protocol: &dyn ServerHttpProtocol,
    document: &dyn ::aws_smithy_schema::serde::SerializableStruct,
    code: &'static str,
    status: u16,
) -> Option<RtResponse> {
    protocol
        .serialize_error(document, &crate::scaffolded::UNMODELED_SCHEMA, code, status)
        .map_err(|error| {
            ::tracing::warn!(
                code,
                %error,
                "this refusal of a CORS preflight could not be rendered in this service's \
                 envelope"
            );
        })
        .ok()
}

/// Set one response header, dropping a value the wire cannot carry rather than turning an
/// answered preflight into a transport failure.
fn insert_header(response: &mut RtResponse, name: &'static str, value: &str) {
    if let Err(refused) = response.headers_mut().try_insert(name, value.to_string()) {
        ::tracing::warn!(
            header = name,
            error = %refused,
            "this header cannot be sent on the wire and is left off the preflight answer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rule allowing `origins`, `methods` and `headers`.
    fn rule(
        origins: &[&str],
        methods: &[&str],
        headers: &[&str],
    ) -> crate::state::BucketCorsRule {
        crate::state::BucketCorsRule {
            allowed_origins: origins.iter().map(|value| value.to_string()).collect(),
            allowed_methods: methods.iter().map(|value| value.to_string()).collect(),
            allowed_headers: headers.iter().map(|value| value.to_string()).collect(),
            ..crate::state::BucketCorsRule::default()
        }
    }

    /// One row of [`a_rule_admits_exactly_the_origins_methods_and_headers_it_states`]: the
    /// case, the rule it is judged against, the origin, the intended method, the headers
    /// asked about, and whether the rule admits that preflight.
    type AdmissionRow<'a> = (
        &'a str,
        &'a crate::state::BucketCorsRule,
        &'a str,
        &'a str,
        &'a [&'a str],
        bool,
    );

    /// One entry more than [`MAX_REQUESTED_HEADERS`], for the row that names more headers
    /// than one answer is matched against.
    const MANY_HEADERS: [&str; MAX_REQUESTED_HEADERS + 1] =
        ["x-anything"; MAX_REQUESTED_HEADERS + 1];

    /// Which preflights one rule admits: the rows differ along the origin, the method, the
    /// headers asked about and whether the rule states a wildcard for either list.
    #[test]
    fn a_rule_admits_exactly_the_origins_methods_and_headers_it_states() {
        let exact = rule(
            &["https://app.example.com"],
            &["GET", "PUT"],
            &["content-type"],
        );
        let wild = rule(&["https://*.example.com"], &["GET"], &["x-amz-*"]);
        let everything = rule(&[WILDCARD], &["GET", "PUT", "POST"], &[WILDCARD]);
        let rows: [AdmissionRow; 12] = [
            (
                "the stated origin and method",
                &exact,
                "https://app.example.com",
                "PUT",
                &[],
                true,
            ),
            (
                "another origin",
                &exact,
                "https://other.example.com",
                "PUT",
                &[],
                false,
            ),
            (
                "a method the rule does not state",
                &exact,
                "https://app.example.com",
                "DELETE",
                &[],
                false,
            ),
            (
                "no method at all",
                &exact,
                "https://app.example.com",
                "",
                &[],
                false,
            ),
            (
                "a header the rule states, in another case",
                &exact,
                "https://app.example.com",
                "GET",
                &["Content-Type"],
                true,
            ),
            (
                "a header the rule does not state",
                &exact,
                "https://app.example.com",
                "GET",
                &["content-type", "x-amz-meta-note"],
                false,
            ),
            (
                "an origin the wildcard rule covers",
                &wild,
                "https://spa.example.com",
                "GET",
                &[],
                true,
            ),
            (
                "an origin outside the wildcard rule",
                &wild,
                "https://spa.example.net",
                "GET",
                &[],
                false,
            ),
            (
                "a header the wildcard rule covers",
                &wild,
                "https://spa.example.com",
                "GET",
                &["x-amz-meta-note"],
                true,
            ),
            (
                "an origin whose bytes do not divide where the wildcard rule's do",
                &wild,
                "https://\u{e9}.example.com-padding",
                "GET",
                &[],
                false,
            ),
            (
                "every origin, method and header",
                &everything,
                "null",
                "POST",
                &["x-anything"],
                true,
            ),
            (
                "more headers than one answer is matched against",
                &everything,
                "null",
                "POST",
                &MANY_HEADERS,
                false,
            ),
        ];
        for (case, rule, origin, method, headers, expected) in rows {
            let admitted = admits(rule, origin, method, headers);
            assert_eq!(
                admitted, expected,
                "case {case:?}: expected the rule to admit the preflight: {expected}, got \
                 {admitted}"
            );
        }
    }
}
