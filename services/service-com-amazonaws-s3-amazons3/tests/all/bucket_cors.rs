// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a general purpose bucket's CORS configuration: `PutBucketCors`
//! stores it and `GetBucketCors` reports it back.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one
//! the bucket-lifecycle suite in this crate already owns), so what is pinned is the wire
//! behaviour — status, body, error code — rather than any handler's internals.
//!
//! The rules under test, with their sources:
//!
//! * `PutBucketCors` "sets the cors configuration for your bucket. If the configuration
//!   exists, Amazon S3 replaces it" (the operation's own documentation): the
//!   `CORSConfiguration` payload becomes the bucket's cors configuration IN FULL — a
//!   replacement, not a merge — and the call answers the modelled 200 with an empty body.
//! * `GetBucketCors` reports exactly those rules back, every member and every rule
//!   unchanged and in the order they were given, and a bucket with no cors configuration
//!   answers `NoSuchCORSConfiguration` with HTTP 404 rather than an empty document
//!   (`CORSRules` is the whole output document, so an empty 200 would report a
//!   configuration allowing no cross-origin access instead of the absence of one).
//! * A configuration breaking a limit the API publishes — a method outside the `GET`, `PUT`,
//!   `HEAD`, `POST`, `DELETE` set `CORSRule$AllowedMethods` documents, an `ID` longer than
//!   the documented 255 characters, a negative `MaxAgeSeconds`, more than the documented 100
//!   rules, or a document past the documented 64 KB — is refused with HTTP 400 and leaves
//!   the stored configuration untouched.
//! * A cors call naming a bucket this emulator does not hold answers the service's
//!   `NoSuchBucket` (HTTP 404), the shape both handlers widen in.
//! * The configuration is what a CORS PREFLIGHT — the `OPTIONS` a browser sends before a
//!   cross-origin call — is answered from: a rule that admits the origin, the intended method
//!   and every intended header makes it HTTP 200 carrying that rule's `Access-Control-*`
//!   headers, and a combination no rule admits is refused 403 `AccessForbidden`. A bucket
//!   carrying no cors configuration at all (and one whose configuration `DeleteBucketCors` has
//!   removed) shares nothing cross-origin, which is the same code with the sentence S3 states
//!   for exactly that condition.
//!
//! What a round-trip row asserts is the ROUND TRIP, not one hand-written document: the
//! request payload and the reported document are compared leaf by leaf (the website suite's
//! [`content`], which is sensitive to the order of repeated siblings), so a row says "this
//! configuration comes back exactly as it went in" without restating it.

use service_com_amazonaws_s3_amazons3::state::{
    CORS_ALLOWED_METHODS, MAX_CORS_CONFIGURATION_BYTES, MAX_CORS_RULES, MAX_CORS_RULE_ID_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, send_bytes, Answer};
use super::bucket_website::{content, reported};

/// How `PutBucketCors` JUDGES the configuration it is given — which documents it refuses,
/// what each refusal says, and that a refused document changes nothing.
///
/// Declared here rather than in `tests/all.rs`, which declares the suites directly under
/// `tests/all/`: a file deeper than that is declared by its own parent module, which this
/// suite is — and this is the module whose payload writer and rule builders it spells its
/// configurations with. The `#[path]` is relative to this file's own directory, and is
/// needed because `tests/all.rs` pulled this file in with a `#[path]` of its own.
#[path = "bucket_cors/rule_validation.rs"]
mod rule_validation;

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The whole `CORSConfiguration` document for the rules `inner` spells.
///
/// `pub(crate)`, like [`put_cors`], [`get_cors`], [`simple_rule`] and [`round_trip_rows`]
/// below it: the cors-delete suite
/// ([`super::bucket_cors_delete`]) drives the same subresource through these helpers rather
/// than spelling a second copy of the document and the two requests.
pub(crate) fn payload(inner: &str) -> String {
    format!("<CORSConfiguration xmlns=\"{XMLNS}\">{inner}</CORSConfiguration>")
}

/// `PUT /<bucket>?cors` — `PutBucketCors`.
pub(crate) async fn put_cors(router: &MultiServiceRouter, bucket: &str, inner: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?cors"),
        &[],
        &payload(inner),
    )
    .await
}

/// `GET /<bucket>?cors` — `GetBucketCors`.
pub(crate) async fn get_cors(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "GET", &format!("/{bucket}?cors"), &[], "").await
}

/// A `CORSRule` carrying exactly the members `inner` spells.
fn rule(inner: &str) -> String {
    format!("<CORSRule>{inner}</CORSRule>")
}

/// Every entry of a flattened list, each in an element named `name`.
///
/// The element names are the `xmlName`s the model puts on `CORSRule`'s members
/// (`smithy_query(query="shape",shape="CORSRule")`): `AllowedMethod`, `AllowedOrigin`,
/// `AllowedHeader` and `ExposeHeader`.
fn entries(name: &str, values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("<{name}>{value}</{name}>"))
        .collect()
}

/// The simplest rule that satisfies the two required members: one method and one origin.
pub(crate) fn simple_rule(method: &str, origin: &str) -> String {
    rule(&format!(
        "{}{}",
        entries("AllowedMethod", &[method]),
        entries("AllowedOrigin", &[origin])
    ))
}

/// The configuration the refusal rows leave in place, and the REPLACEMENT in the round-trip
/// rows: it shares no optional member with the rows below, so a merge rather than a
/// replacement is visible.
fn replacement() -> String {
    simple_rule("HEAD", "https://replacement.example.com")
}

/// The table of cors configurations every row of which must round-trip unchanged.
///
/// The rows differ along what the store-and-report rule generalises over: WHICH of
/// `CORSRule`'s optional members a rule names, which VALUES they carry (every method the
/// `AllowedMethods` documentation publishes, not the ones the recorded call sent, and each
/// published bound of the rule-identifier length), how MANY rules the configuration carries
/// (one, several and the documented ceiling), how many entries a list carries, the ORDER the
/// rules arrive in, and the order the members themselves are spelled in.
pub(crate) fn round_trip_rows() -> Vec<(String, String)> {
    let any_origin = entries("AllowedOrigin", &["*"]);
    let get = entries("AllowedMethod", &["GET"]);

    let mut rows: Vec<(String, String)> = vec![
        (
            "one method and one origin".to_string(),
            simple_rule("GET", "https://www.example.com"),
        ),
        (
            "an origin naming a wildcard".to_string(),
            simple_rule("GET", "*"),
        ),
        (
            "an origin naming a wildcard subdomain".to_string(),
            simple_rule("GET", "https://*.example.com"),
        ),
        (
            "an origin naming a port".to_string(),
            simple_rule("GET", "http://localhost:3000"),
        ),
        (
            "three origins keep the order they arrived in".to_string(),
            rule(&format!(
                "{get}{}",
                entries(
                    "AllowedOrigin",
                    &[
                        "https://first.example.com",
                        "https://second.example.com",
                        "https://third.example.com",
                    ],
                )
            )),
        ),
        (
            "every published method in one rule".to_string(),
            rule(&format!(
                "{}{any_origin}",
                entries("AllowedMethod", &CORS_ALLOWED_METHODS)
            )),
        ),
        (
            "an allowed header".to_string(),
            rule(&format!(
                "{get}{any_origin}{}",
                entries("AllowedHeader", &["x-amz-request-id"])
            )),
        ),
        (
            "a wildcard allowed header".to_string(),
            rule(&format!(
                "{get}{any_origin}{}",
                entries("AllowedHeader", &["*"])
            )),
        ),
        (
            "two exposed headers".to_string(),
            rule(&format!(
                "{get}{any_origin}{}",
                entries("ExposeHeader", &["ETag", "x-amz-version-id"])
            )),
        ),
        (
            "a preflight cache time of zero seconds".to_string(),
            rule(&format!("{get}{any_origin}<MaxAgeSeconds>0</MaxAgeSeconds>")),
        ),
        (
            "a preflight cache time of an hour".to_string(),
            rule(&format!(
                "{get}{any_origin}<MaxAgeSeconds>3600</MaxAgeSeconds>"
            )),
        ),
        (
            "every member of one rule at once".to_string(),
            rule(&format!(
                "<ID>everything</ID>{}{}{}{}<MaxAgeSeconds>1800</MaxAgeSeconds>",
                entries("AllowedHeader", &["*"]),
                entries("AllowedMethod", &["PUT", "POST"]),
                entries("AllowedOrigin", &["https://app.example.com"]),
                entries("ExposeHeader", &["ETag"]),
            )),
        ),
        (
            "the members spelled in the reverse of the model's order".to_string(),
            rule(&format!(
                "<MaxAgeSeconds>60</MaxAgeSeconds>{}{}{}{}<ID>reversed</ID>",
                entries("ExposeHeader", &["ETag"]),
                entries("AllowedOrigin", &["https://app.example.com"]),
                entries("AllowedMethod", &["DELETE"]),
                entries("AllowedHeader", &["content-type"]),
            )),
        ),
        (
            "three rules keep the order they arrived in".to_string(),
            [
                rule(&format!(
                    "<ID>first</ID>{}{}",
                    entries("AllowedMethod", &["GET"]),
                    entries("AllowedOrigin", &["https://first.example.com"])
                )),
                rule(&format!(
                    "<ID>second</ID>{}{}",
                    entries("AllowedMethod", &["PUT"]),
                    entries("AllowedOrigin", &["https://second.example.com"])
                )),
                rule(&format!(
                    "<ID>third</ID>{}{}",
                    entries("AllowedMethod", &["POST"]),
                    entries("AllowedOrigin", &["https://third.example.com"])
                )),
            ]
            .concat(),
        ),
        (
            format!(
                "a rule identifier of exactly the {MAX_CORS_RULE_ID_CHARS} characters AWS \
                 documents"
            ),
            rule(&format!(
                "<ID>{}</ID>{get}{any_origin}",
                "i".repeat(MAX_CORS_RULE_ID_CHARS)
            )),
        ),
        (
            format!("exactly the {MAX_CORS_RULES} rules per configuration AWS documents"),
            (0..MAX_CORS_RULES)
                .map(|index| {
                    rule(&format!(
                        "<ID>rule-{index}</ID>{get}{}",
                        entries(
                            "AllowedOrigin",
                            &[format!("https://host-{index}.example.com").as_str()],
                        )
                    ))
                })
                .collect::<Vec<String>>()
                .concat(),
        ),
    ];

    // One row per method the `AllowedMethods` documentation publishes — the whole catalog
    // rather than the one method a recorded call happened to send.
    for method in CORS_ALLOWED_METHODS {
        rows.push((
            format!("a rule allowing {method} alone"),
            simple_rule(method, "https://www.example.com"),
        ));
    }

    rows
}

#[tokio::test]
async fn putting_a_cors_configuration_stores_it_and_reads_back_through_get() {
    let router = router();

    for (index, (case, configuration)) in round_trip_rows().into_iter().enumerate() {
        let bucket = format!("cors-round-trip-{index}");
        let created = create_bucket(&router, &bucket, None, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created first, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );

        let put = put_cors(&router, &bucket, &configuration).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: expected the modelled 200 from PutBucketCors, got {} with body \
             {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        assert!(
            put.body.is_empty(),
            "case {case:?}: PutBucketCors's output is Unit, so the body must be empty, got \
             {:?}",
            String::from_utf8_lossy(&put.body)
        );

        let read = get_cors(&router, &bucket).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetBucketCors on the bucket just \
             configured, got {} with body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            reported(&read),
            content(&payload(&configuration)),
            "case {case:?}: expected the stored configuration back unchanged, got body {:?}",
            String::from_utf8_lossy(&read.body)
        );

        // Reading twice changes nothing: the read reports, it does not configure.
        let again = get_cors(&router, &bucket).await;
        assert_eq!(
            reported(&again),
            reported(&read),
            "case {case:?}: a second read must report exactly what the first did, got body \
             {:?}",
            String::from_utf8_lossy(&again.body)
        );

        // The second put REPLACES: what comes back is the new configuration alone, with
        // nothing merged in from the one above — and the replacement shares no optional
        // member with it, so a merge would show.
        let replaced = put_cors(&router, &bucket, &replacement()).await;
        assert_eq!(
            replaced.status, 200,
            "case {case:?}: the replacing put must succeed, got {} with body {:?}",
            replaced.status,
            String::from_utf8_lossy(&replaced.body)
        );
        let reread = get_cors(&router, &bucket).await;
        assert_eq!(
            reported(&reread),
            content(&payload(&replacement())),
            "case {case:?}: the cors configuration must be replaced, not merged, got body \
             {:?}",
            String::from_utf8_lossy(&reread.body)
        );
    }
}

#[tokio::test]
async fn reading_a_bucket_with_no_cors_configuration_is_not_found() {
    let router = router();

    // Two buckets that have never been configured, one of them in a non-default Region, so
    // the answer is about the missing subresource rather than about one bucket.
    for (case, bucket, constraint) in [
        (
            "a bucket created with no location constraint",
            "cors-absent-default",
            None,
        ),
        (
            "a bucket created in another Region",
            "cors-absent-regional",
            Some("eu-west-1"),
        ),
    ] {
        let created = create_bucket(&router, bucket, constraint, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created first, got {}",
            created.status
        );

        let read = get_cors(&router, bucket).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a bucket with no cors configuration answers the documented \
             not-found rather than an empty document, got {} with body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "NoSuchCORSConfiguration",
            "case {case:?}: expected the documented error code, got {:?}",
            read.error_code()
        );

        // Reading twice answers the same way: the read reports, it does not configure.
        let again = get_cors(&router, bucket).await;
        assert_eq!(
            again.status, 404,
            "case {case:?}: a second read must answer the same way, got {}",
            again.status
        );

        // And the bucket still takes a put afterwards, so the rows above failed on the
        // missing subresource rather than on a broken bucket.
        let put = put_cors(&router, bucket, &replacement()).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: the bucket must accept a cors configuration, got {} with body \
             {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        let read = get_cors(&router, bucket).await;
        assert_eq!(
            reported(&read),
            content(&payload(&replacement())),
            "case {case:?}: the configured bucket must report what it was given, got body \
             {:?}",
            String::from_utf8_lossy(&read.body)
        );
    }
}

/// One refusal row: the case, the configuration, and the wire error code it must carry when
/// the rule being broken is one the handler itself judges.
///
/// A row whose code is `None` is one the request never reaches the handler with: both of
/// `CORSRule`'s required lists are `@required` in the model, so a document naming neither is
/// refused by the boundary validator — which answers its own code — before any rule of the
/// store is consulted. The row still pins the REFUSAL (HTTP 400, nothing stored), which is
/// what a caller sees.
type RefusalRow = (String, String, Option<&'static str>);

/// The table of cors configurations that must be refused, each with the code it goes out
/// with.
///
/// The rows differ along WHICH published limit is broken, and where the value sits relative
/// to it: one past each documented bound (the rule count, the identifier length, the
/// document size) and a value outside the documented method catalog in three different
/// shapes (a supported method in the wrong case, a method the catalog omits, and the
/// preflight method itself).
fn refusal_rows() -> Vec<RefusalRow> {
    let any_origin = entries("AllowedOrigin", &["*"]);
    let get = entries("AllowedMethod", &["GET"]);

    let mut rows: Vec<RefusalRow> = vec![
        ("no rule at all".to_string(), String::new(), None),
        (
            "a rule naming no method".to_string(),
            rule(&any_origin),
            None,
        ),
        (
            "a rule naming no origin".to_string(),
            rule(&get),
            None,
        ),
        (
            format!(
                "a rule identifier one character past the {MAX_CORS_RULE_ID_CHARS} AWS \
                 documents"
            ),
            rule(&format!(
                "<ID>{}</ID>{get}{any_origin}",
                "i".repeat(MAX_CORS_RULE_ID_CHARS + 1)
            )),
            Some("InvalidRequest"),
        ),
        (
            "a negative preflight cache time".to_string(),
            rule(&format!(
                "{get}{any_origin}<MaxAgeSeconds>-1</MaxAgeSeconds>"
            )),
            Some("InvalidRequest"),
        ),
        (
            format!("one rule past the {MAX_CORS_RULES} per configuration AWS documents"),
            (0..=MAX_CORS_RULES)
                .map(|index| {
                    rule(&format!(
                        "{get}{}",
                        entries(
                            "AllowedOrigin",
                            &[format!("https://host-{index}.example.com").as_str()],
                        )
                    ))
                })
                .collect::<Vec<String>>()
                .concat(),
            Some("InvalidRequest"),
        ),
        (
            format!(
                "a document past the {MAX_CORS_CONFIGURATION_BYTES} bytes a cors document \
                 may carry"
            ),
            // Few enough rules to stay under the rule ceiling, each carrying an origin long
            // enough that their text alone passes the documented document size.
            (0..MAX_CORS_RULES)
                .map(|index| {
                    rule(&format!(
                        "{get}{}",
                        entries(
                            "AllowedOrigin",
                            &[format!(
                                "https://{}-{index}.example.com",
                                "o".repeat(MAX_CORS_CONFIGURATION_BYTES / MAX_CORS_RULES)
                            )
                            .as_str()],
                        )
                    ))
                })
                .collect::<Vec<String>>()
                .concat(),
            Some("InvalidRequest"),
        ),
    ];

    // A method outside the published catalog, in the three shapes one arrives in.
    for method in ["get", "PATCH", "OPTIONS"] {
        rows.push((
            format!("a rule allowing the unsupported method {method:?}"),
            simple_rule(method, "*"),
            Some("InvalidRequest"),
        ));
    }

    rows
}

#[tokio::test]
async fn a_refused_cors_configuration_changes_nothing() {
    let router = router();
    let bucket = "cors-refusals";
    create_bucket(&router, bucket, None, None).await;

    // The configuration the bucket holds throughout: every refusal below must leave it
    // exactly as it is.
    let held = put_cors(&router, bucket, &replacement()).await;
    assert_eq!(
        held.status, 200,
        "the configuration the refusals must not disturb has to be stored first, got {} \
         with body {:?}",
        held.status,
        String::from_utf8_lossy(&held.body)
    );
    let before = reported(&get_cors(&router, bucket).await);

    for (case, configuration, code) in refusal_rows() {
        let refused = put_cors(&router, bucket, &configuration).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected the configuration to be refused with HTTP 400, got {} \
             with body {:?}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        if let Some(code) = code {
            assert_eq!(
                refused.error_code(),
                code,
                "case {case:?}: expected the published error code, got {:?} with body {:?}",
                refused.error_code(),
                String::from_utf8_lossy(&refused.body)
            );
        }
        assert!(
            !refused.error_message().trim().is_empty(),
            "case {case:?}: a refusal must say which rule the configuration breaks, got an \
             empty message in body {:?}",
            String::from_utf8_lossy(&refused.body)
        );

        let after = reported(&get_cors(&router, bucket).await);
        assert_eq!(
            after, before,
            "case {case:?}: a refused put must leave the stored cors configuration exactly \
             as it was"
        );
    }
}

#[tokio::test]
async fn a_cors_call_on_a_bucket_that_does_not_exist_is_no_such_bucket() {
    let router = router();

    // One bucket name that was never created, and one that exists only as another bucket's
    // neighbour, so the answer is about the named bucket rather than about an empty store.
    create_bucket(&router, "cors-neighbour", None, None).await;

    for (case, bucket) in [
        ("a name nothing was ever created under", "cors-missing"),
        ("a name close to an existing bucket's", "cors-neighbours"),
    ] {
        let put = put_cors(&router, bucket, &replacement()).await;
        assert_eq!(
            put.status, 404,
            "case {case:?}: PutBucketCors on a bucket the emulator does not hold answers \
             404, got {} with body {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        assert_eq!(
            put.error_code(),
            "NoSuchBucket",
            "case {case:?}: expected the service's NoSuchBucket from PutBucketCors, got {:?}",
            put.error_code()
        );

        let read = get_cors(&router, bucket).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: GetBucketCors on a bucket the emulator does not hold answers \
             404, got {} with body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "NoSuchBucket",
            "case {case:?}: expected the service's NoSuchBucket from GetBucketCors, got {:?}",
            read.error_code()
        );
    }
}

/// The endpoint a preflight below is addressed to, which carries no bucket — so the request
/// is PATH-STYLE, the form a browser sends for an endpoint addressed without the bucket.
const PREFLIGHT_HOST: &str = "s3.us-east-1.amazonaws.com";

/// The error code S3 refuses a cors request with, and the two sentences that tell its
/// refusals apart. Spelled here rather than imported, so a wording regression fails this
/// suite.
const CORS_REFUSAL_CODE: &str = "AccessForbidden";
/// See [`CORS_REFUSAL_CODE`] — the sentence S3 refuses a preflight no rule admits with,
/// which names each of the three things a rule is matched on.
const NOT_ALLOWED_MESSAGE: &str =
    "CORSResponse: This CORS request is not allowed. This is usually because the evalution of \
     Origin, request method / Access-Control-Request-Method or Access-Control-Request-Headers \
     are not whitelisted by the resource's CORS spec.";
/// See [`CORS_REFUSAL_CODE`].
const NOT_ENABLED_MESSAGE: &str = "CORSResponse: CORS is not enabled for this bucket.";

/// Send one CORS PREFLIGHT: the `OPTIONS` a browser sends before a cross-origin call, which
/// carries NO credential at all — a browser cannot sign one — and asks its question in the
/// three `Origin` / `Access-Control-Request-*` headers `headers` spells.
async fn preflight(router: &MultiServiceRouter, uri: &str, headers: &[(&str, &str)]) -> Answer {
    super::bucket_lifecycle::send_bytes_signed(
        router,
        "",
        "OPTIONS",
        PREFLIGHT_HOST,
        uri,
        headers,
        b"",
    )
    .await
}

/// The cors configuration the preflight rows below are judged against: one rule for a named
/// origin and one for a wildcard origin, so a row can vary the origin as well as the method
/// and the headers it asks about.
fn preflight_configuration() -> String {
    let named = rule(&format!(
        "{}{}{}{}<MaxAgeSeconds>3000</MaxAgeSeconds>",
        entries("AllowedOrigin", &["https://app.example.com"]),
        entries("AllowedMethod", &["PUT", "GET"]),
        entries("AllowedHeader", &["content-type", "x-amz-meta-*"]),
        entries("ExposeHeader", &["ETag"]),
    ));
    let wildcard = rule(&format!(
        "{}{}",
        entries("AllowedOrigin", &["https://*.cdn.example.net"]),
        entries("AllowedMethod", &["HEAD"]),
    ));
    format!("{named}{wildcard}")
}

/// One preflight row: the case, the path addressed, the headers the preflight asks with, the
/// status it must earn, and the `(header, value)` pairs the answer must report — a header
/// named with `None` must be ABSENT.
type PreflightRow<'a> = (
    &'a str,
    &'a str,
    &'a [(&'a str, &'a str)],
    u16,
    &'a [(&'a str, Option<&'a str>)],
);

/// A preflight is answered from the bucket's own cors rules: a rule that admits the origin,
/// the intended method and every intended header makes it a 200 carrying that rule, and a
/// combination no rule admits is refused 403.
///
/// The rows differ along what the rule generalises over: the ORIGIN (one the configuration
/// names, one it does not, one its wildcard covers), the intended METHOD (each of the ones a
/// rule states, and one it does not), the HEADERS asked about (none, several, one no rule
/// allows, one a wildcard allows), and whether the address names an object or the bucket
/// itself.
#[tokio::test]
async fn a_preflight_is_answered_from_the_buckets_cors_rules() {
    let router = router();
    create_bucket(&router, "cors-preflight", None, None).await;
    let configured = put_cors(&router, "cors-preflight", &preflight_configuration()).await;
    assert_eq!(
        configured.status, 200,
        "the bucket must take its cors configuration first, got {} with body {:?}",
        configured.status,
        String::from_utf8_lossy(&configured.body)
    );

    let rows: &[PreflightRow] = &[
        (
            "an allowed origin, an allowed method and the headers the rule allows",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "PUT"),
                ("access-control-request-headers", "content-type,x-amz-meta-note"),
            ],
            200,
            &[
                (
                    "access-control-allow-origin",
                    Some("https://app.example.com"),
                ),
                ("access-control-allow-methods", Some("PUT, GET")),
                (
                    "access-control-allow-headers",
                    Some("content-type, x-amz-meta-note"),
                ),
                ("access-control-max-age", Some("3000")),
                ("access-control-expose-headers", Some("ETag")),
            ],
        ),
        (
            "the other method the same rule allows, asking about no headers",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "GET"),
            ],
            200,
            &[
                (
                    "access-control-allow-origin",
                    Some("https://app.example.com"),
                ),
                ("access-control-allow-methods", Some("PUT, GET")),
                ("access-control-allow-headers", None),
            ],
        ),
        (
            "the BUCKET itself rather than an object under it",
            "/cors-preflight",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "GET"),
            ],
            200,
            &[(
                "access-control-allow-origin",
                Some("https://app.example.com"),
            )],
        ),
        (
            "an origin the wildcard rule covers",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://assets.cdn.example.net"),
                ("access-control-request-method", "HEAD"),
            ],
            200,
            &[
                (
                    "access-control-allow-origin",
                    Some("https://assets.cdn.example.net"),
                ),
                ("access-control-allow-methods", Some("HEAD")),
                ("access-control-max-age", None),
            ],
        ),
        (
            "a method no rule of the configuration allows",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "DELETE"),
            ],
            403,
            &[("access-control-allow-origin", None)],
        ),
        (
            "an origin no rule of the configuration names",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://attacker.example.org"),
                ("access-control-request-method", "PUT"),
            ],
            403,
            &[("access-control-allow-origin", None)],
        ),
        (
            "a header no rule of the configuration allows",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "PUT"),
                ("access-control-request-headers", "x-custom-field"),
            ],
            403,
            &[("access-control-allow-origin", None)],
        ),
        (
            "an origin outside the wildcard rule's suffix",
            "/cors-preflight/spa/assets/data.json",
            &[
                ("origin", "https://assets.cdn.example.org"),
                ("access-control-request-method", "HEAD"),
            ],
            403,
            &[("access-control-allow-origin", None)],
        ),
    ];

    for (case, uri, headers, status, reported) in rows {
        let answered = preflight(&router, uri, headers).await;
        assert_eq!(
            answered.status, *status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        for (header, value) in *reported {
            assert_eq!(
                answered.header(header).as_deref(),
                *value,
                "case {case:?}: expected {header} as {value:?}, got {:?}",
                answered.header(header)
            );
        }
        if *status == 403 {
            assert_eq!(
                answered.error_code(),
                CORS_REFUSAL_CODE,
                "case {case:?}: expected the {CORS_REFUSAL_CODE} code, got {:?}",
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                NOT_ALLOWED_MESSAGE,
                "case {case:?}: expected the sentence S3 refuses a disallowed cors request \
                 with, got {:?}",
                answered.error_message()
            );
            // The refusal names the method the preflight said it intended to use and the
            // kind of resource the configuration was consulted for — the only members
            // beside the sentence that say what was refused.
            let intended = headers
                .iter()
                .find(|(name, _)| *name == "access-control-request-method")
                .map(|(_, value)| value.to_string());
            assert_eq!(
                answered.element("Method"),
                intended,
                "case {case:?}: expected the refusal to name the intended method {intended:?}, \
                 got {:?}",
                answered.element("Method")
            );
            let addressed = if uri.trim_start_matches('/').contains('/') {
                "OBJECT"
            } else {
                "BUCKET"
            };
            assert_eq!(
                answered.element("ResourceType").as_deref(),
                Some(addressed),
                "case {case:?}: expected the refusal to name {addressed}, got {:?}",
                answered.element("ResourceType")
            );
        }
    }

    // A rule that shares with EVERY origin reports the wildcard back rather than the one
    // origin that asked, which is what lets a browser cache one answer for such a resource.
    let replaced = put_cors(&router, "cors-preflight", &simple_rule("GET", "*")).await;
    assert_eq!(
        replaced.status, 200,
        "the bucket must take the wildcard configuration, got {} with body {:?}",
        replaced.status,
        String::from_utf8_lossy(&replaced.body)
    );
    let answered = preflight(
        &router,
        "/cors-preflight/spa/assets/data.json",
        &[
            ("origin", "https://anything.example.org"),
            ("access-control-request-method", "GET"),
        ],
    )
    .await;
    assert_eq!(
        answered.header("access-control-allow-origin").as_deref(),
        Some("*"),
        "case \"a rule sharing with every origin\": expected the wildcard back, got {:?}",
        answered.header("access-control-allow-origin")
    );

    // And a preflight that names no origin asks nothing a rule could be matched against.
    let no_origin = preflight(
        &router,
        "/cors-preflight/spa/assets/data.json",
        &[("access-control-request-method", "GET")],
    )
    .await;
    assert_eq!(
        no_origin.status, 400,
        "case \"a preflight naming no origin\": expected HTTP 400, got {} with body {:?}",
        no_origin.status,
        String::from_utf8_lossy(&no_origin.body)
    );
    assert_eq!(
        no_origin.error_code(),
        "BadRequest",
        "case \"a preflight naming no origin\": expected the BadRequest code, got {:?}",
        no_origin.error_code()
    );
}

/// A bucket with NO cors configuration shares nothing cross-origin: every preflight against
/// it is refused with the sentence S3 states for exactly that condition — before the
/// configuration is put, and again once `DeleteBucketCors` has removed it.
#[tokio::test]
async fn a_preflight_against_a_bucket_sharing_nothing_is_refused() {
    let router = router();
    create_bucket(&router, "cors-unconfigured", None, None).await;

    let asked = &[
        ("origin", "https://app.example.com"),
        ("access-control-request-method", "GET"),
    ];
    let case = "before any cors configuration is put";
    let refused = preflight(&router, "/cors-unconfigured/spa/index.html", asked).await;
    assert_eq!(
        refused.status, 403,
        "case {case:?}: expected HTTP 403, got {} with body {:?}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(
        refused.error_code(),
        CORS_REFUSAL_CODE,
        "case {case:?}: expected the {CORS_REFUSAL_CODE} code, got {:?}",
        refused.error_code()
    );
    assert_eq!(
        refused.error_message(),
        NOT_ENABLED_MESSAGE,
        "case {case:?}: expected the sentence S3 refuses a bucket sharing nothing with, got \
         {:?}",
        refused.error_message()
    );
    // A bucket that shares nothing at all is refused ABOUT THE BUCKET, whatever the request
    // addressed under it: the decision is about the configuration the bucket does not carry.
    assert_eq!(
        refused.element("ResourceType").as_deref(),
        Some("BUCKET"),
        "case {case:?}: expected the refusal to name BUCKET, got {:?}",
        refused.element("ResourceType")
    );
    assert_eq!(
        refused.element("Method").as_deref(),
        Some("GET"),
        "case {case:?}: expected the refusal to name the intended method, got {:?}",
        refused.element("Method")
    );

    // With a configuration in place the same preflight is answered.
    let configured = put_cors(
        &router,
        "cors-unconfigured",
        &simple_rule("GET", "https://app.example.com"),
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "the bucket must take its cors configuration, got {} with body {:?}",
        configured.status,
        String::from_utf8_lossy(&configured.body)
    );
    let answered = preflight(&router, "/cors-unconfigured/spa/index.html", asked).await;
    assert_eq!(
        answered.status, 200,
        "case \"with a cors configuration in place\": expected HTTP 200, got {} with body {:?}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    assert_eq!(
        answered
            .header("access-control-allow-credentials")
            .as_deref(),
        Some("true"),
        "case \"with a cors configuration in place\": an admitted preflight says the call may \
         carry its credentials, got {:?}",
        answered.header("access-control-allow-credentials")
    );

    // And once the configuration is deleted the bucket shares nothing again.
    let deleted = send(&router, "DELETE", "/cors-unconfigured?cors", &[], "").await;
    assert_eq!(
        deleted.status, 204,
        "DeleteBucketCors must answer 204, got {} with body {:?}",
        deleted.status,
        String::from_utf8_lossy(&deleted.body)
    );
    let refused = preflight(&router, "/cors-unconfigured/spa/index.html", asked).await;
    assert_eq!(
        refused.status, 403,
        "case \"after DeleteBucketCors\": expected HTTP 403, got {} with body {:?}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(
        refused.error_message(),
        NOT_ENABLED_MESSAGE,
        "case \"after DeleteBucketCors\": expected the sentence S3 refuses a bucket sharing \
         nothing with, got {:?}",
        refused.error_message()
    );

    // The boundary of that answer: a name this emulator holds no bucket under is not a bucket
    // sharing nothing, it is no bucket at all, and the preflight says so. Such a request is
    // dispatched with a credential scope, because an anonymous request naming a bucket this
    // emulator does not hold is not this service's to claim in the first place.
    let case = "a bucket this emulator holds none under";
    let absent = send_bytes(&router, "OPTIONS", "/cors-absent/spa/index.html", asked, b"").await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: expected HTTP 404, got {} with body {:?}",
        absent.status,
        String::from_utf8_lossy(&absent.body)
    );
    assert_eq!(
        absent.error_code(),
        "NoSuchBucket",
        "case {case:?}: expected the NoSuchBucket code, got {:?}",
        absent.error_code()
    );
    assert_eq!(
        absent.element("BucketName").as_deref(),
        Some("cors-absent"),
        "case {case:?}: expected the refusal to name the bucket, got {:?}",
        absent.element("BucketName")
    );
}
