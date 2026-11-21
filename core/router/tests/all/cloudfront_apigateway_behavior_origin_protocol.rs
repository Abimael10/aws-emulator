// SPDX-License-Identifier: CC0-1.0

//! WHICH cache behaviour of a CloudFront distribution governs a viewer request, read over the
//! FORMS `CacheBehavior$PathPattern` is written in — and therefore which origin, and which
//! `CustomOriginConfig$OriginProtocolPolicy`, the edge fetches that request over.
//!
//! RULE: the behaviour that applies to a viewer request is the first ordered behaviour whose
//! `PathPattern` names the requested path — "when CloudFront receives a viewer request, the
//! requested path is compared with path patterns in the order in which cache behaviors are
//! listed", with the default behaviour applying when "the request for an object does not match
//! the path pattern for any cache behaviors" — and a pattern names a path through the forms
//! that member documents: a leading `/` is optional ("CloudFront behavior is the same with or
//! without the leading `/`"), `*` stands for any run of characters and `?` for exactly one.
//!
//! The rows span those forms: a pattern written WITH the optional leading slash, a pattern
//! whose `?` stands for the one character at its position, the same pattern against a path one
//! character longer, and a path no pattern names at all. Which of the two origins answered is
//! what makes the selection observable, so this suite is laid out as
//! [`super::cloudfront_per_behaviour_origin_protocol`] beside it is — one API Gateway stage
//! behind two custom origins, one addressed over TLS and one over plaintext — and reuses that
//! suite's harness rather than standing up a second one: its origins, its api, its
//! distribution builder and its reader of an answer. The ordered behaviours here name the
//! PLAINTEXT origin and the default behaviour the TLS one, so a path a pattern selects is the
//! origin connection CloudFront cannot complete (`502`/`504`, none of the stage's document)
//! and a path none of them selects is the stage's own answer — over paths the stage serves in
//! every case, so a leg that is not served is the selection and the origin connection and
//! nothing else.

use super::cloudfront_per_behaviour_origin_protocol::{
    check_leg, create_distribution, deployed_api_serving, Expected, PLAINTEXT_ORIGIN, PROBE,
    SECURE_ORIGIN, UNDER_BROKEN, UNDER_MIRROR,
};
use super::cloudfront_apigateway_origin_edge::{emulator, wait_until_deployed};

/// One more path the stage serves, whose last segment is one character LONGER than
/// [`UNDER_MIRROR`]'s: a pattern whose `?` stands for exactly one character does not name it,
/// so the DEFAULT behaviour is what applies to it.
const UNDER_MIRROR_LONGER: &str = "mirror/probes";

/// The ordered behaviours this suite's distribution carries, as `(PathPattern, TargetOriginId)`:
/// the plaintext origin behind two patterns written in two of the documented forms — one with
/// the optional leading `/` and a `*`, one with a `?`.
const PATTERN_FORMS: &[(&str, &str)] = &[
    ("/broken/*", PLAINTEXT_ORIGIN),
    ("mirror/pro?e", PLAINTEXT_ORIGIN),
];

/// One row: the path the viewer asks for, and what it is answered once a pattern — or none of
/// them — has selected the behaviour that names the origin.
struct Row {
    case: &'static str,
    path: &'static str,
    expected: Expected,
}

const ROWS: &[Row] = &[
    // A pattern written with the leading `/` selects its behaviour just as one without it
    // does, so this path is fetched from the plaintext origin and cannot be reached.
    Row {
        case: "a path pattern spelled with the optional leading slash",
        path: "/broken/probe",
        expected: Expected::OriginUnreachable,
    },
    // `?` stands for exactly one character, so it selects this path's behaviour.
    Row {
        case: "a path pattern whose ? stands for the one character at that position",
        path: "/mirror/probe",
        expected: Expected::OriginUnreachable,
    },
    // …and for no more than one, so a path one character longer matches neither pattern and is
    // the DEFAULT behaviour's — which names the origin the edge can reach.
    Row {
        case: "a path one character longer than the ? pattern names",
        path: "/mirror/probes",
        expected: Expected::Served(UNDER_MIRROR_LONGER),
    },
    // A path no ordered pattern names at all, which is the plainest reading of the same rule.
    Row {
        case: "a path no ordered pattern names",
        path: "/probe",
        expected: Expected::Served(PROBE),
    },
];

#[tokio::test]
async fn the_documented_path_pattern_forms_decide_which_behaviour_governs_the_fetch() {
    let router = emulator();
    let api = deployed_api_serving(
        &router,
        &[PROBE, UNDER_BROKEN, UNDER_MIRROR, UNDER_MIRROR_LONGER],
    )
    .await;

    let case = "a distribution whose ordered patterns are written in the documented forms";
    let (id, domain) = create_distribution(
        &router,
        case,
        "path-pattern-forms",
        &api,
        SECURE_ORIGIN,
        PATTERN_FORMS,
    )
    .await;
    wait_until_deployed(&router, case, &id).await;

    for row in ROWS {
        check_leg(&router, row.case, &domain, row.path, row.expected).await;
    }
}
