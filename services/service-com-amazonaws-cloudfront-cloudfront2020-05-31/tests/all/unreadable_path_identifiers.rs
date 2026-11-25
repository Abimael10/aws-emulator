// SPDX-License-Identifier: CC0-1.0

//! What CloudFront answers a request whose PATH it cannot read, end to end over HTTP.
//!
//! The rule under test: the front door answers a request whose method and path name NONE of
//! the actions this API serves with the unknown action it is: HTTP 404,
//! `<Code>InvalidAction</Code>`, and no `Message` at all — nothing read the request as an
//! action, so there is no operation, member or value for the refusal to name, and the
//! request's own path is never echoed back. An identifier carrying a `/` (an ARN where an id
//! belongs) is exactly such a request: it spills into the next path segment and matches no
//! route, while the operation itself is served for an identifier that fits one segment.
//!
//! The rows span what the rule generalises over: four resource families, both an unmatched
//! identifier and an unmatched resource word, a path longer than its route, a method no route
//! of the path serves and a path outside this API's version prefix, against matched routes as
//! the positive control.
//!
//! What an identifier a MATCHED route cannot resolve earns is each resource kind's own
//! contract, asserted in the suites that own it
//! (`distribution_identifier_reads_no_form`, `distribution_identifier_reads_its_alphabet`,
//! `origin_access_control_identifier_readability`,
//! `crossed_resource_identifiers`).
//!
//! Every assertion travels the router, so each case exercises routing, the restXml reader, the
//! handler, the persisted state and serialization. The request harness and the create helpers
//! are the crate's own.

use crate::distribution_lifecycle::{
    batch_xml, call, config, create_distribution, emulator, API_VERSION, ACCOUNT_ID,
};

/// The code the front door names an action it could not find.
const INVALID_ACTION: &str = "InvalidAction";

/// An invalidation batch every mutation row below submits: a well-formed document, so what the
/// call is answered with is decided by the identifier in its path and nothing else.
fn batch(reference: &str) -> String {
    batch_xml(reference, &["/*"])
}

// ---------------------------------------------------------------------------
// Rule 1 — a path that names no action of this API
// ---------------------------------------------------------------------------

/// One request whose path names no action, and the one it is contrasted with.
struct Unmatched {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<String>,
}

#[tokio::test]
async fn a_path_that_names_no_action_is_the_front_doors_unknown_action() {
    let router = emulator();
    let created = create_distribution(
        &router,
        &config("the distribution the matched routes address", "unknown-action").xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    // The ARNs of three resource families, each carrying the `/` that makes it span two path
    // segments — which is why a request naming one where an identifier belongs matches no
    // route at all.
    let distribution_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{id}");
    let function_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:function/unknown-action-fn");
    let origin_access_control_arn =
        format!("arn:aws:cloudfront::{ACCOUNT_ID}:origin-access-control/E1AAAAAAAAAAAA");

    let rows = vec![
        Unmatched {
            case: "an ARN where a distribution id belongs, on a read",
            method: "GET",
            uri: format!("/{API_VERSION}/distribution/{distribution_arn}/config"),
            body: None,
        },
        Unmatched {
            case: "an ARN where a function name belongs",
            method: "GET",
            uri: format!("/{API_VERSION}/function/{function_arn}/describe"),
            body: None,
        },
        Unmatched {
            case: "an ARN where a distribution id belongs, on a mutation",
            method: "POST",
            uri: format!("/{API_VERSION}/distribution/{distribution_arn}/invalidation"),
            body: Some(batch("unknown-action-arn")),
        },
        Unmatched {
            case: "an ARN where an origin access control id belongs",
            method: "GET",
            uri: format!("/{API_VERSION}/origin-access-control/{origin_access_control_arn}"),
            body: None,
        },
        Unmatched {
            case: "a segment past the end of a route",
            method: "GET",
            uri: format!("/{API_VERSION}/distribution/{id}/config/and-more"),
            body: None,
        },
        Unmatched {
            case: "a resource word no action of this API serves",
            method: "GET",
            uri: format!("/{API_VERSION}/not-a-cloudfront-resource/{id}"),
            body: None,
        },
        Unmatched {
            case: "a method no action of this path serves",
            method: "DELETE",
            uri: format!("/{API_VERSION}/distribution/{id}/config"),
            body: None,
        },
        Unmatched {
            case: "a path outside this API's version prefix",
            method: "GET",
            uri: format!("/not-an-api-version/distribution/{id}"),
            body: None,
        },
    ];

    for Unmatched {
        case,
        method,
        uri,
        body,
    } in &rows
    {
        let answer = call(&router, method, uri, body.as_deref(), None).await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            INVALID_ACTION,
            "case {case}: expected Code {INVALID_ACTION}, got {:?} with {}",
            answer.error_code(),
            answer.raw
        );
        // The refusal states its code and nothing else: no `Message`, and in particular
        // nothing taken from the request.
        assert_eq!(
            answer.root().find_text("Message"),
            None,
            "case {case}: expected no Message member, got {:?} with {}",
            answer.root().find_text("Message"),
            answer.raw
        );
        assert!(
            !answer.raw.contains(uri.as_str()),
            "case {case}: the refusal echoed the request path back in {}",
            answer.raw
        );
        // The envelope still names the request it answers, as every other refusal of this
        // service does.
        let request_id = answer.root().find_text("RequestId").unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {case}: expected the envelope to carry a RequestId, got {}",
            answer.raw
        );
    }

    // The positive control: a path that DOES name an action is served by that action — the
    // refusal above is about the action lookup, not about the identifier the path carries.
    let served = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        None,
        None,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case a matched route: expected HTTP 200, got {} with {}",
        served.status, served.raw
    );
    let absent = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/EABCDEFGH12345/config"),
        None,
        None,
    )
    .await;
    assert_eq!(
        absent.error_code(),
        "NoSuchDistribution",
        "case a matched route addressing nothing: expected the operation's own refusal, got \
         {:?} with {}",
        absent.error_code(),
        absent.raw
    );
}
