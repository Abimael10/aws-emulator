// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `PutBucketCors` JUDGES the `CORSConfiguration` it is given:
//! which documents it refuses, what each refusal says, and that a refused document changes
//! nothing about the bucket.
//!
//! The rule these cases assert: **a cors configuration S3 will not apply is refused with the
//! error code and the SENTENCE S3 publishes for that class of refusal — fixed text, carrying
//! a value from the request only where the published sentence quotes the offending entry, and
//! never the member path the document arrived on nor a count taken from it — and the bucket
//! keeps whatever configuration it already carried; a configuration S3 does apply is stored
//! whole and reported back by `GetBucketCors` unchanged.**
//!
//! Where each limit comes from:
//!
//! * the methods a rule may allow are the catalog `CORSRule$AllowedMethods` documents ("Valid
//!   values are `GET`, `PUT`, `HEAD`, `POST`, and `DELETE`",
//!   `smithy_query(query="shape",shape="CORSRule")`), so the rows below allow methods outside
//!   it — including a published method in a case the documentation does not publish;
//! * a configuration carries at most the 100 rules `CORSConfiguration$CORSRules` documents
//!   ("You can add up to 100 rules to the configuration") and a rule an identifier of at most
//!   the 255 characters `CORSRule$ID` documents ("The value cannot be longer than 255
//!   characters"), each asserted at the published bound and past it;
//! * an `AllowedOrigin` and an `AllowedHeader` are patterns matched by what stands before and
//!   after a single `*`, so one wildcard is all either carries, and an `ExposeHeader` names a
//!   response header a browser may read by name, so it carries none. Neither bound is
//!   modelled — both members are plain string lists — so the API is what enforces them.
//!
//! The rows differ along what the rule generalises over: WHICH published limit the document
//! breaks, which member carries the offending entry, how far past the limit the value sits
//! (one past each documented bound and far past it), and — for the sentences that quote the
//! entry — which value is quoted, so a sentence built from anything but the request's own
//! entry shows up. The recorded calls that filed the issue are one row each.

use service_com_amazonaws_s3_amazons3::state::{MAX_CORS_RULES, MAX_CORS_RULE_ID_CHARS};
use smithy_server_runtime::MultiServiceRouter;

use crate::bucket_lifecycle::{create_bucket, router, Answer};
use crate::bucket_website::{content, reported};
// The payload writer and the rule builders of the suite this module belongs to, so what a
// row sends is spelled the way that suite spells a cors document.
use super::{entries, get_cors, payload, put_cors, rule, simple_rule};

/// The code S3 answers a document that satisfies the published schema but breaks a limit the
/// API enforces with. Spelled here rather than imported, so a wording regression fails this
/// suite.
const REFUSAL_CODE: &str = "InvalidRequest";

/// The HTTP status that code is rendered with.
const REFUSAL_STATUS: u16 = 400;

/// One refused configuration: the case, the rules the document carries, and the `<Message>`
/// the refusal must carry, byte for byte.
struct Refusal {
    case: String,
    configuration: String,
    message: String,
}

/// A rule allowing `GET` from one origin, with `inner` added to it — so a row breaks exactly
/// one limit and nothing else.
fn rule_with(inner: &str) -> String {
    rule(&format!(
        "{}{}{inner}",
        entries("AllowedMethod", &["GET"]),
        entries("AllowedOrigin", &["https://app.example.com"])
    ))
}

/// Every configuration that must be refused, with the sentence S3 refuses it with.
fn refusal_rows() -> Vec<Refusal> {
    let mut rows: Vec<Refusal> = Vec::new();

    // An origin pattern carrying more than one wildcard, in each shape one arrives in: the
    // sentence is fixed apart from the origin, which is quoted as the request spelled it.
    for origin in [
        "https://*.*.example.com",
        "https://*.example.*",
        "*.*",
        "***",
        "https://*.*.*.example.com",
    ] {
        rows.push(Refusal {
            case: format!("an AllowedOrigin carrying several wildcards: {origin:?}"),
            configuration: simple_rule("GET", origin),
            message: format!("AllowedOrigin \"{origin}\" can not have more than one wildcard."),
        });
    }

    // The same limit on the other pattern list, named by its own member.
    for header in ["x-amz-*-*", "**", "*-*-*"] {
        rows.push(Refusal {
            case: format!("an AllowedHeader carrying several wildcards: {header:?}"),
            configuration: rule_with(&entries("AllowedHeader", &[header])),
            message: format!("AllowedHeader \"{header}\" can not have more than one wildcard."),
        });
    }

    // An exposed header is a name a browser is told it may read, so it carries no wildcard
    // at all — not even the single one a pattern list admits.
    for header in ["*", "x-amz-*"] {
        rows.push(Refusal {
            case: format!("an ExposeHeader carrying a wildcard: {header:?}"),
            configuration: rule_with(&entries("ExposeHeader", &[header])),
            message: format!(
                "ExposeHeader \"{header}\" contains wildcard. We currently do not support \
                 wildcard for ExposeHeader."
            ),
        });
    }

    // A method outside the documented catalog: another HTTP method, a published method in a
    // case the documentation does not publish, and a method of no catalog at all.
    for method in ["PATCH", "OPTIONS", "TRACE", "CONNECT", "get", "Put", "FETCH"] {
        rows.push(Refusal {
            case: format!("an AllowedMethod outside the published catalog: {method:?}"),
            configuration: simple_rule(method, "https://app.example.com"),
            message: format!(
                "Found unsupported HTTP method in CORS config. Unsupported method is {method}"
            ),
        });
    }

    // One rule past the documented ceiling, and far past it: the limit the sentence states is
    // the DOCUMENTED one, so both rows are refused in the same words.
    for count in [MAX_CORS_RULES + 1, MAX_CORS_RULES + 37] {
        rows.push(Refusal {
            case: format!("{count} rules, past the {MAX_CORS_RULES} AWS documents"),
            configuration: (0..count)
                .map(|index| simple_rule("GET", &format!("https://host-{index}.example.com")))
                .collect::<Vec<String>>()
                .concat(),
            message: format!(
                "The number of CORS rules should not exceed allowed limit of {MAX_CORS_RULES} \
                 rules."
            ),
        });
    }

    // The same for a rule identifier one character past the documented length and far past
    // it: neither the identifier nor its length is part of the sentence.
    for length in [MAX_CORS_RULE_ID_CHARS + 1, MAX_CORS_RULE_ID_CHARS * 4] {
        rows.push(Refusal {
            case: format!(
                "a rule identifier of {length} characters, past the {MAX_CORS_RULE_ID_CHARS} \
                 AWS documents"
            ),
            configuration: rule(&format!(
                "<ID>{}</ID>{}{}",
                "i".repeat(length),
                entries("AllowedMethod", &["GET"]),
                entries("AllowedOrigin", &["https://app.example.com"])
            )),
            message: format!(
                "CORS rule ID exceeds the maximum length of {MAX_CORS_RULE_ID_CHARS} characters."
            ),
        });
    }

    rows
}

/// One accepted configuration: the case and the rules the document carries.
///
/// These are the rows on the OTHER side of each limit above — the published bound itself, and
/// the pattern each refused one is one wildcard past — so the refusals are shown to be about
/// the limit rather than about the member.
fn accepted_rows() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = vec![
        (
            "an origin that is the wildcard alone".to_string(),
            simple_rule("GET", "*"),
        ),
        (
            "an origin carrying exactly one wildcard".to_string(),
            simple_rule("GET", "https://*.example.com"),
        ),
        (
            "an allowed header carrying exactly one wildcard".to_string(),
            rule_with(&entries("AllowedHeader", &["x-amz-*"])),
        ),
        (
            "an allowed header that is the wildcard alone".to_string(),
            rule_with(&entries("AllowedHeader", &["*"])),
        ),
        (
            "exposed headers carrying no wildcard".to_string(),
            rule_with(&entries("ExposeHeader", &["ETag", "x-amz-version-id"])),
        ),
        (
            format!(
                "a rule identifier of exactly the {MAX_CORS_RULE_ID_CHARS} characters AWS \
                 documents"
            ),
            rule(&format!(
                "<ID>{}</ID>{}{}",
                "i".repeat(MAX_CORS_RULE_ID_CHARS),
                entries("AllowedMethod", &["GET"]),
                entries("AllowedOrigin", &["https://app.example.com"])
            )),
        ),
        (
            format!("exactly the {MAX_CORS_RULES} rules AWS documents"),
            (0..MAX_CORS_RULES)
                .map(|index| simple_rule("GET", &format!("https://host-{index}.example.com")))
                .collect::<Vec<String>>()
                .concat(),
        ),
    ];

    // One row per method the catalog publishes, beside the rows above that break it.
    for method in ["GET", "PUT", "HEAD", "POST", "DELETE"] {
        rows.push((
            format!("the published method {method}"),
            simple_rule(method, "https://app.example.com"),
        ));
    }

    rows
}

/// The configuration the bucket carries while the refusals are tried: every optional member
/// of a rule at once, so anything a refused document left behind shows in the read.
fn baseline() -> String {
    rule(&format!(
        "<ID>baseline</ID>{}{}{}{}<MaxAgeSeconds>1800</MaxAgeSeconds>",
        entries("AllowedHeader", &["x-amz-*"]),
        entries("AllowedMethod", &["GET", "PUT"]),
        entries("AllowedOrigin", &["https://*.example.com"]),
        entries("ExposeHeader", &["ETag"]),
    ))
}

/// Create `bucket` and give it [`baseline`] as its cors configuration.
async fn configured_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = create_bucket(router, bucket, None, None).await;
    assert_eq!(
        created.status,
        200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        body_of(&created)
    );
    let put = put_cors(router, bucket, &baseline()).await;
    assert_eq!(
        put.status,
        200,
        "the baseline configuration must be accepted, got {} with body {:?}",
        put.status,
        body_of(&put)
    );
}

/// The body of `answer`, for an assertion that failed.
fn body_of(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

#[tokio::test]
async fn a_refused_cors_configuration_carries_s3_s_own_sentence_and_changes_nothing() {
    let router = router();
    let bucket = "cors-refusal-wording";
    configured_bucket(&router, bucket).await;
    let before = reported(&get_cors(&router, bucket).await);

    for row in refusal_rows() {
        let Refusal {
            case,
            configuration,
            message,
        } = row;

        let refused = put_cors(&router, bucket, &configuration).await;
        assert_eq!(
            refused.status,
            REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} with body {:?}",
            refused.status,
            body_of(&refused)
        );
        assert_eq!(
            refused.error_code(),
            REFUSAL_CODE,
            "case {case:?}: expected the code {REFUSAL_CODE}, got {:?} with body {:?}",
            refused.error_code(),
            body_of(&refused)
        );
        assert_eq!(
            refused.error_message(),
            message,
            "case {case:?}: expected the sentence S3 refuses this with, got {:?}",
            refused.error_message()
        );

        // Nothing of the refused document reached the bucket: the configuration it carried
        // before is the configuration it carries after.
        let after = reported(&get_cors(&router, bucket).await);
        assert_eq!(
            after, before,
            "case {case:?}: a refused put must leave the stored configuration alone, got \
             {after:?}"
        );
    }
}

#[tokio::test]
async fn a_cors_configuration_within_every_published_limit_is_stored_and_reported_back() {
    let router = router();

    for (index, (case, configuration)) in accepted_rows().into_iter().enumerate() {
        let bucket = format!("cors-accepted-{index}");
        let created = create_bucket(&router, &bucket, None, None).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the bucket must be created first, got {} with body {:?}",
            created.status,
            body_of(&created)
        );

        let put = put_cors(&router, &bucket, &configuration).await;
        assert_eq!(
            put.status,
            200,
            "case {case:?}: expected the modelled 200 from PutBucketCors, got {} with body {:?}",
            put.status,
            body_of(&put)
        );

        let read = get_cors(&router, &bucket).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetBucketCors, got {} with body {:?}",
            read.status,
            body_of(&read)
        );
        assert_eq!(
            reported(&read),
            content(&payload(&configuration)),
            "case {case:?}: expected the accepted configuration back unchanged, got body {:?}",
            body_of(&read)
        );
    }
}
