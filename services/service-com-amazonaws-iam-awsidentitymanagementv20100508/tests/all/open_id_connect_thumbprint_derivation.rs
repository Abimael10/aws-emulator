// SPDX-License-Identifier: CC0-1.0

//! What `CreateOpenIDConnectProvider` PINS when the request names no thumbprint, and how it
//! refuses a `Url` outside the published form, end to end over the awsQuery wire.
//!
//! RULE 1: "this parameter is optional. If it is not included, IAM will retrieve and use the
//! top intermediate certificate authority (CA) thumbprint of the OpenID Connect identity
//! provider server certificate" (`CreateOpenIDConnectProvider$ThumbprintList`), so a
//! provider is NEVER registered unpinned: a create naming no thumbprint leaves the provider
//! carrying exactly one, of the form `thumbprintType` publishes ("the hex-encoded SHA-1 hash
//! value of the X.509 certificate", length 40 —
//! `smithy_query(query="shape", shape="thumbprintType")`), derived from the provider the URL
//! names, so it is the same on every read and two providers never share it. A create that
//! DOES name thumbprints stores exactly those.
//!
//! RULE 2: a `Url` outside the form `$Url` publishes ("the URL must begin with https:// …
//! path components are allowed but query parameters are not") is refused with `InvalidInput`
//! (HTTP 400) carrying NO `Message` member at all — recorded behaviour for that class — and
//! nothing is written.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: whether the
//! request named thumbprints and how many, the shape of the URL (bare host, one path
//! segment, a nested path, a mixed-case host, a host with digits and hyphens), and which
//! other members the request carried. No single request is the specification.
//!
//! The awsQuery harness (`call`, `element`, `optional`, `error_code`, `param`, `borrowed`)
//! is the one `tests/all/user_lifecycle.rs` and `tests/all/role_lifecycle.rs` own and every
//! suite in this crate shares; the provider helpers (`expected_arn`, `issuer_of`,
//! `list_members`, `thumbprint`) are `tests/all/open_id_connect_providers.rs`'s.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::open_id_connect_providers::{expected_arn, issuer_of, list_members, thumbprint};
use crate::role_lifecycle::{borrowed, optional, param};
use crate::user_lifecycle::{call, element, error_code, router};

/// The length `thumbprintType` publishes, which is also the length of a hex-encoded SHA-1
/// digest (`smithy_query(query="shape", shape="thumbprintType")`: len 40..40).
const THUMBPRINT_LENGTH: usize = 40;

/// Create a provider for `url`, with `thumbprints` and `client_ids` when the row carries
/// any, and answer the whole response body.
async fn create_provider(
    router: &MultiServiceRouter,
    url: &str,
    thumbprints: &[String],
    client_ids: &[&str],
) -> (u16, String) {
    let mut params = vec![param("Url", url)];
    for (index, value) in thumbprints.iter().enumerate() {
        params.push(param(&format!("ThumbprintList.member.{}", index + 1), value));
    }
    for (index, value) in client_ids.iter().enumerate() {
        params.push(param(&format!("ClientIDList.member.{}", index + 1), value));
    }
    call(router, "CreateOpenIDConnectProvider", &borrowed(&params)).await
}

/// `GetOpenIDConnectProvider` for the provider `url` names.
async fn read_provider(router: &MultiServiceRouter, url: &str) -> (u16, String) {
    let params = vec![param("OpenIDConnectProviderArn", &expected_arn(url))];
    call(router, "GetOpenIDConnectProvider", &borrowed(&params)).await
}

/// Assert `value` is of the form `thumbprintType` publishes: exactly 40 lower-case hex
/// characters, the spelling of a hex-encoded SHA-1 digest.
fn assert_published_thumbprint_form(case: &str, value: &str) {
    assert_eq!(
        value.chars().count(),
        THUMBPRINT_LENGTH,
        "case {case:?}: a thumbprint is {THUMBPRINT_LENGTH} characters, got {value:?}"
    );
    assert!(
        value
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
        "case {case:?}: a thumbprint is hex-encoded in lower case, got {value:?}"
    );
}

/// One row: the URL a provider is created for and the thumbprints the request names.
struct DerivationCase {
    case: &'static str,
    url: &'static str,
    requested: Vec<String>,
    client_ids: Vec<&'static str>,
}

/// The table: a request naming none, one, several and the published maximum of five
/// thumbprints, over URLs of every shape the model admits, with and without the other
/// optional list.
fn derivation_cases() -> Vec<DerivationCase> {
    vec![
        DerivationCase {
            case: "a bare host naming no thumbprint",
            url: "https://derive.example.org",
            requested: Vec::new(),
            client_ids: Vec::new(),
        },
        DerivationCase {
            case: "a one-segment path naming no thumbprint, with a client ID",
            url: "https://derive.example.com/oidc",
            requested: Vec::new(),
            client_ids: vec!["audience-0"],
        },
        DerivationCase {
            case: "a nested path naming no thumbprint",
            url: "https://derive-7.example-corp.net/auth/realms/dev-2",
            requested: Vec::new(),
            client_ids: Vec::new(),
        },
        DerivationCase {
            case: "a mixed-case host naming no thumbprint",
            url: "https://Derive.Example.COM/Realms/Main",
            requested: Vec::new(),
            client_ids: vec!["one.client", "another client"],
        },
        DerivationCase {
            case: "a host carrying digits naming no thumbprint",
            url: "https://oidc.eks.us-east-1.amazonaws.com/id/EXAMPLED539D4633E53DE1B716D3041E",
            requested: Vec::new(),
            client_ids: Vec::new(),
        },
        DerivationCase {
            case: "one thumbprint named by the request",
            url: "https://named-one.example.org",
            requested: vec![thumbprint(11)],
            client_ids: Vec::new(),
        },
        DerivationCase {
            case: "two thumbprints named by the request",
            url: "https://named-two.example.org/oidc",
            requested: vec![thumbprint(12), thumbprint(13)],
            client_ids: vec!["audience-1"],
        },
        DerivationCase {
            case: "the published maximum of five thumbprints",
            url: "https://named-five.example.org",
            requested: (20..25).map(thumbprint).collect(),
            client_ids: Vec::new(),
        },
    ]
}

/// RULE: the created provider reports a non-empty `ThumbprintList` whichever way the request
/// was written — the thumbprints it named, or the ONE derived for the provider when it named
/// none — and `GetOpenIDConnectProvider` reports the same list, in the same order.
#[tokio::test]
async fn a_create_naming_no_thumbprint_pins_one_and_reads_it_back() {
    let router = router();

    for case in derivation_cases() {
        let (status, created) =
            create_provider(&router, case.url, &case.requested, &case.client_ids).await;
        assert_eq!(
            status, 200,
            "case {:?}: the create must succeed, got {status}: {created}",
            case.case
        );

        let (status, read) = read_provider(&router, case.url).await;
        assert_eq!(
            status, 200,
            "case {:?}: the created provider must read back, got {status}: {read}",
            case.case
        );
        let reported = list_members(&read, "ThumbprintList");
        assert!(
            !reported.is_empty(),
            "case {:?}: a provider is never registered unpinned, got {reported:?}",
            case.case
        );

        if case.requested.is_empty() {
            assert_eq!(
                reported.len(),
                1,
                "case {:?}: a create naming none pins the one thumbprint IAM derives, got \
                 {reported:?}",
                case.case
            );
            assert_published_thumbprint_form(case.case, &reported[0]);
        } else {
            assert_eq!(
                reported, case.requested,
                "case {:?}: a create naming thumbprints stores exactly those, got \
                 {reported:?}",
                case.case
            );
        }

        // A second read reports the same list: the thumbprint belongs to the provider, so
        // reading it neither derives a new one nor changes what a later caller sees.
        let (_, again) = read_provider(&router, case.url).await;
        assert_eq!(
            list_members(&again, "ThumbprintList"),
            reported,
            "case {:?}: two reads must report the same thumbprints, got {again}",
            case.case
        );
    }
}

/// RULE: the derived thumbprint is a property of the PROVIDER, so two providers created in
/// one account — whatever their URLs' shapes — never share one, and none of them is a value
/// the request carried.
#[tokio::test]
async fn each_provider_is_pinned_to_its_own_derived_thumbprint() {
    let router = router();

    let mut derived: BTreeSet<String> = BTreeSet::new();
    let cases: Vec<DerivationCase> = derivation_cases()
        .into_iter()
        .filter(|case| case.requested.is_empty())
        .collect();
    assert!(
        cases.len() >= 2,
        "the table must carry at least two rows naming no thumbprint, got {}",
        cases.len()
    );

    for case in &cases {
        let (status, created) = create_provider(&router, case.url, &[], &case.client_ids).await;
        assert_eq!(
            status, 200,
            "case {:?}: the create must succeed, got {status}: {created}",
            case.case
        );
        let (_, read) = read_provider(&router, case.url).await;
        let reported = list_members(&read, "ThumbprintList");
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: expected one derived thumbprint, got {reported:?}",
            case.case
        );
        let value = reported[0].clone();
        assert!(
            !value.contains(issuer_of(case.url)),
            "case {:?}: a thumbprint is a digest, not the issuer echoed back, got {value:?}",
            case.case
        );
        assert!(
            derived.insert(value.clone()),
            "case {:?}: two providers must not share a thumbprint, got {value:?} twice",
            case.case
        );
    }
}

/// RULE: a `Url` outside the published form is refused with `InvalidInput` (HTTP 400)
/// carrying no `Message` member, and nothing is written — so a later read of the ARN that
/// URL would have minted answers 404.
#[tokio::test]
async fn a_url_outside_the_published_form_is_refused_without_a_message() {
    let router = router();

    // Rows: the other schemes a caller reaches for, a scheme-less host, a malformed scheme,
    // a query parameter on a bare host and on a path, and URLs naming no host at all.
    let cases = [
        "http://scheme.example.org",
        "https:/scheme.example.org",
        "scheme.example.org",
        "oidc://scheme.example.org",
        "https://query.example.org?audience=sts",
        "https://query.example.org/oidc?audience=sts",
        "https://",
        "https:///realms/main",
    ];

    for url in cases {
        let params = vec![param("Url", url)];
        let (status, body) = call(
            &router,
            "CreateOpenIDConnectProvider",
            &borrowed(&params),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {url:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {url:?}: expected InvalidInput, got {body}"
        );
        assert_eq!(
            optional(&body, "Message"),
            None,
            "case {url:?}: AWS's refusal for this class carries no Message, got {body}"
        );
        assert!(
            optional(&body, "OpenIDConnectProviderArn").is_none(),
            "case {url:?}: a refused create mints no ARN, got {body}"
        );
    }

    // A well-formed URL is still registered, so the refusals are about the value and not
    // about the account — and it is pinned, which is the other half of this suite.
    let (status, created) = create_provider(&router, "https://well-formed.example.org", &[], &[])
        .await;
    assert_eq!(
        status, 200,
        "a well-formed URL must still be accepted, got {status}: {created}"
    );
    let (_, read) = read_provider(&router, "https://well-formed.example.org").await;
    assert_eq!(
        element(&read, "Url"),
        "well-formed.example.org",
        "the read must report the stored issuer, got {read}"
    );
    assert_eq!(
        list_members(&read, "ThumbprintList").len(),
        1,
        "the accepted create must pin one derived thumbprint, got {read}"
    );
}
