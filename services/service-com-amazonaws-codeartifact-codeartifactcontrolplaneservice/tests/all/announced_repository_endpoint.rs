// SPDX-License-Identifier: CC0-1.0

//! The address `GetRepositoryEndpoint` ANNOUNCES, and that a client can actually dial it.
//!
//! The RULE under test: a URL this emulator hands a client to send later requests to is
//! rooted on the endpoint THIS emulator answers on — the address the request reached it at,
//! or the base it is served at — and the resource's own identity travels in the PATH
//! beneath it, under the reserved namespace segment a data plane served at the front door
//! sits beneath (`/_aws/<plane>/…`). For a repository endpoint that path carries what AWS's
//! own published host carries — the region, the `EndpointType` its suffix selects, and the
//! `<domain>-<domainOwner>` identity of the domain — followed by the two documented
//! segments `/<format>/<repository>/`. A URL on a hostname AWS publishes would resolve away
//! from this process, so nothing a client then sends to it — an npm publish, a pip install
//! — would ever arrive.
//!
//! Each case is a TABLE rather than one example, spanning what the rule generalises over:
//!
//! 1. the base the emulator is reachable at, the `EndpointType` asked for and every
//!    modelled `PackageFormat` — with no part of the announced URL naming a zone AWS
//!    publishes its own endpoints in;
//! 2. the address the REQUEST reached, which is what a client configured with one local
//!    endpoint URL resolves — against a request that addressed the service's own published
//!    host, which resolves nothing here and is answered the served base;
//! 3. the npm registry, driven end to end at the announced URL and nowhere else: ping,
//!    publish, packument and tarball, with the control plane describing what was stored;
//! 4. two domains holding a repository of the SAME name, whose announced endpoints stay
//!    distinct and each serve their own repository;
//! 5. the pypi index, served at its own announced URL;
//! 6. the documented flow end to end — `GetAuthorizationToken`, then a package manager
//!    pointed at the announced URL — for every authorization period a token may be minted
//!    for, and for the periods that mint none.

use serde_json::Value;
use smithy_server_runtime::{MultiServiceRouter, UNSERVED_URL_BASE};

// The enum catalogues come from this service's Smithy model
// (`smithy_query(query="shape", shape="PackageFormat,EndpointType")`), re-exported as the
// generated `*_VALUES` constants — so a model change moves these tables rather than leaving
// a hand-copied list behind.
use service_com_amazonaws_codeartifact_codeartifactcontrolplaneservice::ops::types::{
    ENDPOINT_TYPE_VALUES, PACKAGE_FORMAT_VALUES,
};

use crate::domain_repository_lifecycle::create_repository;
use crate::harness::{
    auth_for, call, create_domain, emulator_served_at, Answer, ACCOUNT, REGION,
};
use crate::npm_registry_endpoint::{publish_document, registry, tarball_bytes, token_for};

/// The base a served emulator in these cases reports, with the loopback address and port a
/// data port is published at.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The published minimum and maximum of the authorization period a token may be minted
/// for: "Valid values are 0 and any number between 900 (15 minutes) and 43200 (12 hours)"
/// (`GetAuthorizationTokenRequest$durationSeconds`, which also carries the modelled
/// `@range` ceiling 43200).
const MIN_DURATION_SECONDS: i64 = 900;
const MAX_DURATION_SECONDS: i64 = 43_200;

/// `GetAuthorizationToken` for `domain`, with the authorization period sent only when
/// named — the whole answer, because the cases below assert both a mint and a refusal.
async fn token_answer(
    router: &MultiServiceRouter,
    domain: &str,
    duration_seconds: Option<i64>,
) -> Answer {
    let mut uri = format!("/v1/authorization-token?domain={domain}");
    if let Some(duration) = duration_seconds {
        uri.push_str(&format!("&duration={duration}"));
    }
    call(router, "POST", &uri, None).await
}

/// `GetRepositoryEndpoint`, sent to `host` — the address the client reached this emulator
/// at, which is what the announced URL has to lead back to.
async fn get_endpoint_at(
    router: &MultiServiceRouter,
    host: &str,
    domain: &str,
    repository: &str,
    format: &str,
    endpoint_type: Option<&str>,
) -> Answer {
    let mut uri =
        format!("/v1/repository/endpoint?domain={domain}&repository={repository}&format={format}");
    if let Some(endpoint_type) = endpoint_type {
        uri.push_str(&format!("&endpointType={endpoint_type}"));
    }
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host)
        .header("authorization", auth_for(REGION))
        .header("content-type", "application/json")
        .body(bytes::Bytes::new())
        .expect("build request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    Answer {
        status,
        error_type,
        body,
    }
}

/// The `repositoryEndpoint` of an answer that must have succeeded.
pub(crate) fn announced(answer: &Answer, case: &str) -> String {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected GetRepositoryEndpoint 200, got {answer:?}"
    );
    answer
        .body
        .get("repositoryEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no repositoryEndpoint in {answer:?}"))
        .to_string()
}

/// `GetRepositoryEndpoint` through the shared harness call (which addresses this service's
/// own published host), reduced to the announced URL.
async fn announced_endpoint(
    router: &MultiServiceRouter,
    domain: &str,
    repository: &str,
    format: &str,
) -> String {
    let answer = call(
        router,
        "GET",
        &format!(
            "/v1/repository/endpoint?domain={domain}&repository={repository}&format={format}"
        ),
        None,
    )
    .await;
    announced(&answer, &format!("{domain}/{repository}/{format}"))
}

/// The rule, as one string: the base the emulator is reached at, then the reserved
/// namespace segment and this service's plane, then what AWS's published host carries — the
/// region, the `EndpointType` and the domain's `<domain>-<domainOwner>` identity — then the
/// two documented path segments.
fn expected_endpoint(
    base: &str,
    region: &str,
    domain: &str,
    owner: &str,
    format: &str,
    repository: &str,
    endpoint_type: &str,
) -> String {
    format!("{base}/_aws/codeartifact/{region}/{endpoint_type}/{domain}-{owner}/{format}/{repository}/")
}

/// The `(authority, path)` of an announced URL — what a client splits it into to dial it.
pub(crate) fn dialable(url: &str) -> (String, String) {
    let without_scheme = url
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or_else(|| panic!("the announced URL {url:?} carries no scheme"));
    match without_scheme.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    }
}

/// A domain with one repository in it, and a token for that domain.
async fn given_repository(router: &MultiServiceRouter, domain: &str, repository: &str) -> String {
    assert_eq!(
        create_domain(router, domain, None).await.status,
        200,
        "CreateDomain {domain:?} must succeed"
    );
    assert_eq!(
        create_repository(router, domain, repository, None, &[])
            .await
            .status,
        200,
        "CreateRepository {repository:?} must succeed"
    );
    token_for(router, domain).await
}

/// CASE 1: the announced URL is the base the emulator is reachable at, AWS's published host
/// in the path, and the documented segments — for every base, `EndpointType` and
/// `PackageFormat`.
#[tokio::test]
async fn the_announced_endpoint_is_rooted_on_the_base_the_emulator_is_reached_at() {
    // The rows are the bases `OperationContext::endpoint` reports: the unserved default, a
    // loopback data port, a bracketed IPv6 bind and a named host with a port.
    let bases = [
        UNSERVED_URL_BASE,
        SERVED_BASE,
        "https://[::1]:9297",
        "https://emulator.test.invalid:8443",
    ];
    // `None` is the "no endpointType supplied" row, which must resolve to the documented
    // `ipv4` default.
    let endpoint_types: Vec<Option<&str>> = std::iter::once(None)
        .chain(ENDPOINT_TYPE_VALUES.iter().copied().map(Some))
        .collect();
    let domain = "announced";
    let repository = "Releases.one";

    for base in bases {
        let router = emulator_served_at(base);
        given_repository(&router, domain, repository).await;

        for format in PACKAGE_FORMAT_VALUES {
            for endpoint_type in &endpoint_types {
                let case = format!("base {base} format {format} endpointType {endpoint_type:?}");
                let answer = call(
                    &router,
                    "GET",
                    &format!(
                        "/v1/repository/endpoint?domain={domain}&repository={repository}\
                         &format={format}{}",
                        match endpoint_type {
                            Some(kind) => format!("&endpointType={kind}"),
                            None => String::new(),
                        }
                    ),
                    None,
                )
                .await;
                let url = announced(&answer, &case);
                let expected = expected_endpoint(
                    base,
                    REGION,
                    domain,
                    ACCOUNT,
                    format,
                    repository,
                    endpoint_type.unwrap_or("ipv4"),
                );
                assert_eq!(
                    url, expected,
                    "case {case:?}: expected {expected:?}, got {url:?}"
                );

                // The AUTHORITY is the emulator's own, never the host AWS publishes: a
                // client dials what precedes the first path separator.
                let (authority, path) = dialable(&url);
                assert_eq!(
                    format!("https://{authority}"),
                    base,
                    "case {case:?}: expected the emulator's own authority, got {url:?}"
                );
                assert!(
                    path.ends_with(&format!("/{format}/{repository}/")),
                    "case {case:?}: expected the documented endpoint path, got {path:?}"
                );

                // And NO part of the URL names a host AWS publishes an endpoint at: the
                // domain's identity travels as a path segment, so neither the authority a
                // client dials nor anything beneath it is a name that resolves away from
                // this process. The zones are the shared list of AWS's own endpoint zones
                // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`).
                for zone in service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
                    assert!(
                        !url.contains(zone),
                        "case {case:?}: the announced URL names the AWS endpoint zone \
                         {zone:?}, which this emulator does not serve: {url:?}"
                    );
                }
            }
        }
    }
}

/// CASE 2: the announced URL leads back to the address the REQUEST reached — which is what
/// a client configured with one local endpoint URL can resolve — and a request that
/// addressed this service's own published host, resolving nothing here, is answered the
/// base the emulator is served at.
#[tokio::test]
async fn the_announced_endpoint_leads_back_to_the_address_the_request_reached() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "reached";
    let repository = "packages";
    given_repository(&router, domain, repository).await;

    // (case, the host the client addressed, the authority the answer must name)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a client on this machine, by address and port",
            "127.0.0.1:44035",
            "127.0.0.1:44035",
        ),
        ("a client by loopback name", "localhost:9297", "localhost:9297"),
        ("a client by IPv6 literal", "[::1]:9297", "[::1]:9297"),
        (
            "a client that resolves this service's own published host",
            "codeartifact.us-east-1.amazonaws.com",
            "127.0.0.1:9297",
        ),
    ];

    for (case, host, expected_authority) in rows {
        let answer = get_endpoint_at(&router, host, domain, repository, "npm", None).await;
        let url = announced(&answer, case);
        let (authority, path) = dialable(&url);
        assert_eq!(
            authority, *expected_authority,
            "case {case:?}: expected the authority {expected_authority:?}, got {url:?}"
        );
        assert_eq!(
            path,
            format!("/_aws/codeartifact/{REGION}/ipv4/{domain}-{ACCOUNT}/npm/{repository}/"),
            "case {case:?}: expected the documented endpoint path, got {url:?}"
        );
    }
}

/// CASE 3: an npm client pointed at the announced URL — and at no other address — reaches
/// the repository: it pings, publishes, reads the packument back and fetches the tarball the
/// packument names, and the control plane describes the version that publish stored.
#[tokio::test]
async fn an_npm_client_pointed_at_the_announced_endpoint_reaches_the_repository() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "npm-announced";
    let repository = "releases";
    let token = given_repository(&router, domain, repository).await;

    let url = announced_endpoint(&router, domain, repository, "npm").await;
    let (host, endpoint) = dialable(&url);

    // The write-authorization route an npm client checks before publishing — at the
    // endpoint announced for EVERY `EndpointType`, since each of them is announced on the
    // address this emulator answers at.
    for endpoint_type in std::iter::once(None).chain(ENDPOINT_TYPE_VALUES.iter().copied().map(Some))
    {
        let case = format!("ping at the endpoint announced for {endpoint_type:?}");
        let announced_for_type = announced(
            &get_endpoint_at(
                &router,
                "127.0.0.1:9297",
                domain,
                repository,
                "npm",
                endpoint_type,
            )
            .await,
            &case,
        );
        let (typed_host, typed_endpoint) = dialable(&announced_for_type);
        let ping = registry(
            &router,
            "GET",
            &typed_host,
            &format!("{typed_endpoint}-/ping?write=true"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(
            ping.status, 200,
            "case {case:?}: expected 200 at {announced_for_type:?}, got {ping:?}"
        );
    }

    // The rows span what a publish varies: a plain name and a scoped one under its
    // URL-encoded namespace, with one and with several versions.
    type Row = (
        &'static str,
        &'static str,
        Option<&'static str>,
        &'static str,
        &'static [&'static str],
    );
    let rows: [Row; 2] = [
        ("a plain package", "left-pad", None, "left-pad", &["1.0.0"]),
        (
            "a scoped package with two versions",
            "@acme%2fwidget",
            Some("acme"),
            "widget",
            &["2.1.0", "2.2.0"],
        ),
    ];

    for (case, url_path, namespace, name, versions) in rows {
        let display = match namespace {
            Some(scope) => format!("@{scope}/{name}"),
            None => name.to_string(),
        };
        let published = registry(
            &router,
            "PUT",
            &host,
            &format!("{endpoint}{url_path}"),
            Some(&token),
            Some(publish_document(&display, name, versions)),
        )
        .await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: expected the publish at the announced endpoint to answer 200, got \
             {published:?}"
        );

        let packument = registry(
            &router,
            "GET",
            &host,
            &format!("{endpoint}{url_path}"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(
            packument.status, 200,
            "case {case:?}: expected the packument to answer 200, got {packument:?}"
        );
        let document = packument.json();

        for version in versions {
            // The tarball URL the registry publishes is under the address the client
            // reached, so the same client fetches it.
            let tarball_url = document
                .get("versions")
                .and_then(|listed| listed.get(*version))
                .and_then(|entry| entry.get("dist"))
                .and_then(|dist| dist.get("tarball"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let (tarball_host, tarball_path) = dialable(&tarball_url);
            assert_eq!(
                tarball_host, host,
                "case {case:?} version {version:?}: expected a tarball URL at the address the \
                 client reached, got {tarball_url:?}"
            );
            let fetched = registry(&router, "GET", &host, &tarball_path, Some(&token), None).await;
            assert_eq!(
                fetched.status, 200,
                "case {case:?} version {version:?}: expected the tarball at {tarball_path:?} to \
                 answer 200, got {fetched:?}"
            );
            assert_eq!(
                fetched.body,
                tarball_bytes(name, version),
                "case {case:?} version {version:?}: expected the published bytes back"
            );

            // And the control plane holds the version that publish created.
            let mut uri = format!(
                "/v1/package/version?domain={domain}&repository={repository}&format=npm\
                 &package={name}&version={version}"
            );
            if let Some(scope) = namespace {
                uri.push_str(&format!("&namespace={scope}"));
            }
            let described = call(&router, "GET", &uri, None).await;
            assert_eq!(
                described.status, 200,
                "case {case:?} version {version:?}: expected DescribePackageVersion 200, got \
                 {described:?}"
            );
            assert_eq!(
                described.body["packageVersion"]["version"].as_str(),
                Some(*version),
                "case {case:?} version {version:?}: expected the stored version, got \
                 {described:?}"
            );
        }
    }
}

/// CASE 4: a repository name two domains both hold is announced as two DIFFERENT endpoints,
/// and each of them serves only its own domain's repository — the announced address carries
/// the domain, so neither publish can land in the other domain.
#[tokio::test]
async fn each_domain_announces_its_own_endpoint_for_a_shared_repository_name() {
    let router = emulator_served_at(SERVED_BASE);
    let repository = "shared-name";
    let first = given_repository(&router, "ca-first", repository).await;
    let second = given_repository(&router, "ca-second", repository).await;

    let first_url = announced_endpoint(&router, "ca-first", repository, "npm").await;
    let second_url = announced_endpoint(&router, "ca-second", repository, "npm").await;
    assert_ne!(
        first_url, second_url,
        "case \"two domains, one repository name\": expected distinct endpoints, got \
         {first_url:?} twice"
    );

    // (case, the domain, its token, its announced endpoint, the package published there)
    let rows = [
        ("the first domain", "ca-first", &first, &first_url, "alpha"),
        ("the second domain", "ca-second", &second, &second_url, "beta"),
    ];

    for (case, domain, token, url, package) in rows {
        let (host, endpoint) = dialable(url);
        let published = registry(
            &router,
            "PUT",
            &host,
            &format!("{endpoint}{package}"),
            Some(token),
            Some(publish_document(package, package, &["1.0.0"])),
        )
        .await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: expected the publish at its own endpoint to answer 200, got \
             {published:?}"
        );

        // It landed in THAT domain's repository, and in no other.
        for (other_case, other_domain, expected) in [
            ("its own domain", domain, 200),
            (
                "the other domain",
                if domain == "ca-first" {
                    "ca-second"
                } else {
                    "ca-first"
                },
                404,
            ),
        ] {
            let described = call(
                &router,
                "GET",
                &format!(
                    "/v1/package/version?domain={other_domain}&repository={repository}\
                     &format=npm&package={package}&version=1.0.0"
                ),
                None,
            )
            .await;
            assert_eq!(
                described.status, expected,
                "case {case:?} read through {other_case:?}: expected HTTP {expected}, got \
                 {described:?}"
            );
        }
    }
}

/// CASE 5: the other plane this service serves is reached at its own announced URL too —
/// the `pypi` endpoint's Simple Repository API index, which is the address `pip` is
/// configured with.
#[tokio::test]
async fn a_python_client_pointed_at_the_announced_endpoint_reads_the_index() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "pypi-announced";
    let repository = "wheels";
    let token = given_repository(&router, domain, repository).await;

    let url = announced_endpoint(&router, domain, repository, "pypi").await;
    let (host, endpoint) = dialable(&url);
    assert!(
        endpoint.ends_with(&format!("/pypi/{repository}/")),
        "expected the pypi endpoint path, got {url:?}"
    );

    // PEP 503: the index a client lands on is `<endpoint>simple/`.
    let index = registry(
        &router,
        "GET",
        &host,
        &format!("{endpoint}simple/"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        index.status, 200,
        "case \"the index at the announced endpoint\": expected 200, got {index:?}"
    );

    // Without the token it is a 401 from this plane, not an unrouted request: the address
    // is served, the credential is what is missing.
    let unauthenticated =
        registry(&router, "GET", &host, &format!("{endpoint}simple/"), None, None).await;
    assert_eq!(
        unauthenticated.status, 401,
        "case \"the index with no credential\": expected 401, got {unauthenticated:?}"
    );
}

/// CASE 6: the flow AWS documents for a package manager — `GetAuthorizationToken`, then
/// point npm at `repositoryEndpoint` — is walkable here, for every authorization period a
/// token may be minted for: the announced URL's authority is one this emulator answers on,
/// the token authenticates the registry's write-authorization route there, and a publish
/// carrying it lands in the repository the URL names. A period outside the accepted set
/// mints no token, so nothing is published with one.
///
/// The rows span what the rule generalises over: the authorization period asked for — none,
/// the caller-credential value, the published minimum, a value inside the interval and the
/// published maximum — and, on the refusal side, the smallest value there is and the one
/// just below the published minimum.
#[tokio::test]
async fn a_token_minted_for_any_accepted_period_authenticates_the_announced_endpoint() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "token-announced";
    let repository = "releases";
    given_repository(&router, domain, repository).await;

    let url = announced_endpoint(&router, domain, repository, "npm").await;
    let (host, endpoint) = dialable(&url);
    // The announced authority is one this emulator answers on, which is what makes the rest
    // of this flow reachable at all: a client handed a name AWS publishes never arrives.
    assert_eq!(
        host,
        SERVED_BASE.trim_start_matches("https://"),
        "case \"the announced authority\": expected the address this emulator is served at, \
         got {url:?}"
    );

    // (case, the authorization period asked for, the package published with its token)
    let rows: [(&str, Option<i64>, &str); 5] = [
        ("no durationSeconds", None, "default-period"),
        ("the caller-credential value", Some(0), "zero-period"),
        (
            "the published minimum",
            Some(MIN_DURATION_SECONDS),
            "minimum-period",
        ),
        ("an hour", Some(3_600), "one-hour-period"),
        (
            "the published maximum",
            Some(MAX_DURATION_SECONDS),
            "maximum-period",
        ),
    ];

    for (case, duration, package) in rows {
        let minted = token_answer(&router, domain, duration).await;
        assert_eq!(
            minted.status, 200,
            "case {case:?}: expected GetAuthorizationToken 200, got {minted:?}"
        );
        let token = minted
            .body
            .get("authorizationToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !token.is_empty(),
            "case {case:?}: expected a minted token, got {minted:?}"
        );

        // The write-authorization route an npm client checks before publishing, at the
        // announced URL and nowhere else.
        let ping = registry(
            &router,
            "GET",
            &host,
            &format!("{endpoint}-/ping?write=true"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(
            ping.status, 200,
            "case {case:?}: expected 200 from the write-authorization route at {url:?}, got \
             {ping:?}"
        );

        // And what that route reports is backed: the same token publishes, and the control
        // plane holds the version the publish stored.
        let published = registry(
            &router,
            "PUT",
            &host,
            &format!("{endpoint}{package}"),
            Some(&token),
            Some(publish_document(package, package, &["1.0.0"])),
        )
        .await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: expected the publish at {url:?} to answer 200, got {published:?}"
        );
        let described = call(
            &router,
            "GET",
            &format!(
                "/v1/package/version?domain={domain}&repository={repository}&format=npm\
                 &package={package}&version=1.0.0"
            ),
            None,
        )
        .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: expected DescribePackageVersion 200 for what the publish stored, \
             got {described:?}"
        );
    }

    // The other side of the rule: a period the accepted set does not hold mints no token at
    // all, in the service's own sentence, so no client ever reaches the endpoint with one.
    for duration in [1_i64, MIN_DURATION_SECONDS - 1] {
        let refused = token_answer(&router, domain, Some(duration)).await;
        assert_eq!(
            (refused.status, refused.code()),
            (400, "ValidationException".to_string()),
            "case refused {duration}: expected a 400 ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!(
                "Invalid durationSeconds value of {duration}: value must be 0 or between \
                 {MIN_DURATION_SECONDS} and {MAX_DURATION_SECONDS} (inclusive)."
            ),
            "case refused {duration}: expected the service's own sentence, got {refused:?}"
        );
        assert!(
            refused.body.get("authorizationToken").is_none(),
            "case refused {duration}: a refusal must carry no token, got {refused:?}"
        );
    }
}
