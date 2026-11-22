// SPDX-License-Identifier: CC0-1.0

//! `CreateOpenIDConnectProvider` against a web identity issuer URL of the account's OWN
//! partition, end to end over the awsQuery wire.
//!
//! RULE: web identity federation hosted inside the partition is OUTBOUND only, so a URL
//! that addresses a web identity issuer this partition hosts — the issuer identifier the
//! outbound-federation operations report, and every address of this emulator whose whole
//! path is one segment of the form those issuers are minted in — cannot be registered as an
//! INBOUND OIDC trust anchor: the create is refused with `InvalidInput` (HTTP 400) and the
//! fixed sentence of that class, and nothing is written. Every OTHER https URL is still
//! registered, including one whose path segment has the issuer form but whose host is not
//! this emulator's.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: which
//! endpoint this emulator is served at (a bare host, a host with a port, an address of the
//! loopback interface), whose issuer the URL names (this account's, another account's in the
//! partition, one no account holds yet), how the URL spells the same address (a trailing
//! slash, the host in another case), and — for the accepted side — the segment's length,
//! alphabet, case and depth. No single request is the specification.
//!
//! The awsQuery harness (`call`, `element`, `error_code`, `param`, `borrowed`) is the one
//! `tests/all/user_lifecycle.rs` and `tests/all/role_lifecycle.rs` own and every suite in
//! this crate shares; the endpoint a row serves this emulator at is set per router here,
//! because the issuer URL is built on it (`MultiServiceRouter::set_url_base`).

use std::sync::Arc;

use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::open_id_connect_providers::{expected_arn, issuer_of};
use crate::role_lifecycle::{borrowed, param};
use crate::user_lifecycle::{account_id, call, element, error_code};

/// The awsQuery error code `InvalidInputException` carries
/// (`smithy_query(query="operation", operation="CreateOpenIDConnectProvider")` ->
/// `InvalidInputException` / `awsQueryError {code: InvalidInput, httpResponseCode: 400}`).
const INVALID_INPUT_CODE: &str = "InvalidInput";

/// The status that refusal carries, from the same `awsQueryError` trait.
const INVALID_INPUT_STATUS: u16 = 400;

/// The whole message this refusal class carries, spelled out here rather than read from the
/// service, so a change of wording fails this suite instead of following it. It interpolates
/// nothing from the request: the URL the caller submitted does not appear in it.
const PARTITION_ISSUER_MESSAGE: &str =
    "Creating an OIDC provider with an STS issuer URL from the same partition is not \
     supported. Within a partition, use AssumeRole instead of web identity federation.";

/// `EnableOutboundWebIdentityFederation`, which answers the account's `IssuerIdentifier` —
/// the in-partition STS web identity issuer a row names.
const ENABLE: &str = "EnableOutboundWebIdentityFederation";

/// The addresses a row serves this emulator at: the base a router carries when nothing is
/// served (`smithy_server_runtime::UNSERVED_URL_BASE`), a host with a port, and an address
/// of the loopback interface with a port — the three shapes an endpoint takes, because the
/// issuer URL is this endpoint followed by one opaque segment.
const ENDPOINTS: &[&str] = &[
    smithy_server_runtime::UNSERVED_URL_BASE,
    "https://emulator.test:8443",
    "https://127.0.0.1:41983",
];

/// A segment with the FORM an account's issuer is minted in — 32 upper-case hexadecimal
/// digits (`state::is_issuer_identifier_segment`, the service's own reading of that form) —
/// that no account of this emulator holds, so a row can name an in-partition issuer the
/// store has no record of.
const UNHELD_ISSUER_SEGMENT: &str = "0123456789ABCDEF0123456789ABCDEF";

/// A router serving only this service at `endpoint`, over a single shared engine.
///
/// The endpoint is what the account's issuer URL is built on, so it is the one knob this
/// suite turns beyond the request itself; the suite drives unsigned requests, exactly as the
/// shared harness does.
fn router_at(endpoint: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(endpoint)
            .unwrap_or_else(|| panic!("the suite's endpoint {endpoint:?} must parse")),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One `CreateOpenIDConnectProvider` call for `url`, with one audience so the request is the
/// one a client registering a trust anchor sends.
async fn create_provider(router: &MultiServiceRouter, url: &str) -> (u16, String) {
    let params = vec![
        param("Url", url),
        param(
            "ClientIDList.member.1",
            "https://example.com/webid-rejection-check",
        ),
    ];
    call(router, "CreateOpenIDConnectProvider", &borrowed(&params)).await
}

/// The account's issuer identifier, through the operation that reports it.
async fn enabled_issuer(router: &MultiServiceRouter) -> String {
    let (status, body) = call(router, ENABLE, &[]).await;
    assert_eq!(
        status, 200,
        "the fixture enable must succeed, got {status}: {body}"
    );
    element(&body, "IssuerIdentifier")
}

/// `url` with the letters of its host upper-cased: the same address, because a URL's host is
/// case-insensitive, spelled differently.
fn host_upper_cased(url: &str) -> String {
    let (scheme, authority_and_path) = url
        .split_once("://")
        .unwrap_or_else(|| panic!("expected an absolute URL, got {url:?}"));
    match authority_and_path.split_once('/') {
        Some((authority, path)) => format!("{scheme}://{}/{path}", authority.to_uppercase()),
        None => format!("{scheme}://{}", authority_and_path.to_uppercase()),
    }
}

/// Assert `url` was refused as an in-partition issuer, and that the refusal wrote nothing:
/// the provider it would have minted reads back as absent.
async fn assert_refused_and_unwritten(router: &MultiServiceRouter, case: &str, url: &str) {
    let (status, body) = create_provider(router, url).await;
    assert_eq!(
        status, INVALID_INPUT_STATUS,
        "case {case:?}: expected HTTP {INVALID_INPUT_STATUS} for {url}, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_INPUT_CODE,
        "case {case:?}: expected {INVALID_INPUT_CODE} for {url}, got {:?}: {body}",
        error_code(&body)
    );
    assert_eq!(
        element(&body, "Message"),
        PARTITION_ISSUER_MESSAGE,
        "case {case:?}: expected this refusal's fixed sentence, got {:?}",
        element(&body, "Message")
    );
    assert!(
        !element(&body, "Message").contains(url),
        "case {case:?}: the sentence interpolates nothing from the request, got {:?}",
        element(&body, "Message")
    );

    let read_params = vec![param("OpenIDConnectProviderArn", &expected_arn(url))];
    let (status, read) = call(router, "GetOpenIDConnectProvider", &borrowed(&read_params)).await;
    assert_eq!(
        status, 404,
        "case {case:?}: a refused create must persist nothing, got {status}: {read}"
    );
}

/// Assert `url` was registered and reads back under the ARN derived from it.
async fn assert_created_and_readable(router: &MultiServiceRouter, case: &str, url: &str) {
    let (status, body) = create_provider(router, url).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {url} to be registered, got {status}: {body}"
    );
    let arn = element(&body, "OpenIDConnectProviderArn");
    assert_eq!(
        arn,
        expected_arn(url),
        "case {case:?}: the ARN must be derived from the URL, got {arn:?}"
    );
    let read_params = vec![param("OpenIDConnectProviderArn", &arn)];
    let (status, read) = call(router, "GetOpenIDConnectProvider", &borrowed(&read_params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the created provider must read back, got {status}: {read}"
    );
    assert_eq!(
        element(&read, "Url"),
        issuer_of(url),
        "case {case:?}: the read must report the URL that was registered, got {:?}",
        element(&read, "Url")
    );
}

/// RULE: a Url naming a web identity issuer of this partition is refused with
/// `InvalidInput`, whichever endpoint this emulator is served at, whose issuer it is, and
/// however the same address is spelled — and the refused create writes nothing.
#[tokio::test]
async fn an_in_partition_web_identity_issuer_url_is_refused() {
    for endpoint in ENDPOINTS {
        let router = router_at(endpoint);
        let issuer = enabled_issuer(&router).await;

        // One row per way of naming an in-partition issuer: the account's own as reported,
        // that same address with a trailing slash and with its host in another case, the
        // issuer another account of this partition is given under this endpoint, and an
        // issuer-form address no account holds at all.
        let rows: Vec<(String, String)> = vec![
            ("the account's own issuer".to_string(), issuer.clone()),
            ("the issuer with a trailing slash".to_string(), format!("{issuer}/")),
            ("the issuer with its host upper-cased".to_string(), host_upper_cased(&issuer)),
            (
                "another account's issuer in the partition".to_string(),
                state::outbound_web_identity_issuer(endpoint, "123456789012"),
            ),
            (
                "an issuer-form address no account holds".to_string(),
                format!("{}/{UNHELD_ISSUER_SEGMENT}", endpoint.trim_end_matches('/')),
            ),
        ];
        for (name, url) in rows {
            let case = format!("{endpoint}: {name}");
            assert_refused_and_unwritten(&router, &case, &url).await;
        }
    }
}

/// RULE: the refusal is about the in-partition issuer ADDRESS, not about the account's
/// record — so it is answered before the account has ever enabled the feature, and the URL
/// the enable then reports is that same address.
#[tokio::test]
async fn the_refusal_does_not_wait_for_the_feature_to_be_enabled() {
    for endpoint in ENDPOINTS {
        let router = router_at(endpoint);
        // What the enable WOULD mint for this account under this endpoint
        // (`state::outbound_web_identity_issuer`), asked before any call has been served.
        let issuer = state::outbound_web_identity_issuer(endpoint, account_id());
        assert_refused_and_unwritten(&router, &format!("{endpoint}: before the enable"), &issuer)
            .await;

        let reported = enabled_issuer(&router).await;
        assert_eq!(
            reported, issuer,
            "case {endpoint:?}: the enable must report the address that was refused, got \
             {reported:?}"
        );
        assert_refused_and_unwritten(&router, &format!("{endpoint}: after the enable"), &issuer)
            .await;
    }
}

/// RULE: every other https Url is still registered — including one whose path segment has
/// the issuer form but whose host is not this emulator's, and one at this emulator's own
/// host whose path is not a single issuer-form segment.
#[tokio::test]
async fn a_url_outside_the_partitions_issuer_form_is_still_registered() {
    for endpoint in ENDPOINTS {
        let router = router_at(endpoint);
        // The account has the feature ON here, so these rows are registered beside an
        // issuer the account really holds rather than only in an account that holds none.
        let issuer = enabled_issuer(&router).await;
        let base = endpoint.trim_end_matches('/');

        // Rows: the issuer form under ANOTHER host; this emulator's host with a segment one
        // digit short, one digit long, in lower case, outside the hexadecimal alphabet, with
        // a further path segment, with an empty extra segment, and with no path at all.
        let rows: Vec<(&str, String)> = vec![
            (
                "the issuer form at another host",
                format!("https://oidc.example.org/{UNHELD_ISSUER_SEGMENT}"),
            ),
            (
                "a segment one digit short",
                format!("{base}/{}", &UNHELD_ISSUER_SEGMENT[1..]),
            ),
            (
                "a segment one digit long",
                format!("{base}/{UNHELD_ISSUER_SEGMENT}0"),
            ),
            (
                "a lower-case segment",
                format!("{base}/{}", UNHELD_ISSUER_SEGMENT.to_lowercase()),
            ),
            (
                "a segment outside the hexadecimal alphabet",
                format!("{base}/{}", "G".repeat(UNHELD_ISSUER_SEGMENT.len())),
            ),
            (
                "an issuer-form segment under a deeper path",
                format!("{base}/{UNHELD_ISSUER_SEGMENT}/callback"),
            ),
            ("this emulator's host alone", base.to_string()),
        ];
        for (name, url) in rows {
            let case = format!("{endpoint}: {name}");
            assert_ne!(
                url, issuer,
                "case {case:?}: this row must be a URL other than the account's issuer"
            );
            assert_created_and_readable(&router, &case, &url).await;
        }
    }
}

/// RULE: one URL is an in-partition issuer only for the partition that HOSTS it — the same
/// address is refused by an emulator served there and registered by one served elsewhere, so
/// the rule reads the endpoint rather than matching a shape.
#[tokio::test]
async fn the_same_url_is_an_issuer_only_where_it_is_hosted() {
    for endpoint in ENDPOINTS {
        let url = format!(
            "{}/{UNHELD_ISSUER_SEGMENT}",
            endpoint.trim_end_matches('/')
        );
        assert_refused_and_unwritten(&router_at(endpoint), &format!("hosted at {endpoint}"), &url)
            .await;

        for other in ENDPOINTS.iter().filter(|other| *other != endpoint) {
            assert_created_and_readable(
                &router_at(other),
                &format!("{url} served at {other}"),
                &url,
            )
            .await;
        }
    }
}
