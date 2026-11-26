// SPDX-License-Identifier: CC0-1.0

//! The handshake a container client makes with the URL `GetAuthorizationToken` publishes:
//! take `proxyEndpoint` VERBATIM, present the issued token as HTTP Basic credentials and read
//! the registry API version off the root of the Docker Registry v2 API.
//!
//! RULE under test: **the registry URL this service publishes for a `docker login`
//! (`AuthorizationData$proxyEndpoint`, "The registry URL to use for this authorization token
//! in a `docker login` command") is an address THIS emulator answers the Docker Registry v2
//! API on — a `host[:port]` behind a scheme, with no name of AWS's own to resolve — and a
//! `GET` of that API's root at it, carrying the token the same call issued as Basic
//! credentials, is answered `200` with the registry API version header, whatever endpoint the
//! caller reached this emulator on.**
//!
//! That is the whole discovery sequence: a client authenticates at the published URL, learns
//! from the root that the endpoint speaks registry V2, and only then pulls. So an address no
//! listener of this process answers on — AWS's own
//! `<account>.dkr.ecr.<region>.<dnsSuffix>` registry name, or that layout written over any
//! other zone, which is a NAME a client would have to resolve rather than an address it can
//! dial — breaks every pull and push built on the published value even though the control
//! plane answered.
//!
//! What this suite adds to its siblings is the ROOT of the registry API reached at the
//! PUBLISHED URL: `crate::published_registry_address` and
//! `crate::announced_address_is_an_endpoint_this_emulator_is_reached_at` vary the endpoints and
//! assert which address each member carries, and `crate::registry_v2_pull` asserts the root's
//! own answer at one fixed host. The rows here join the two ends — what was published and what
//! answers there — and assert the version header a client reads the API off, which is what
//! decides whether the published URL is usable at all.
//!
//! The rows differ along what the rule generalises over: the address this emulator is SERVED
//! at (a loopback socket, another port on it, an IPv6 socket, a host of this service, a host of
//! an operator's own) crossed with the endpoint the caller ARRIVED on (that socket, a loopback
//! name, the registry host AWS publishes, that registry layout written over the loopback zone
//! and over an operator's zone, this service's own endpoint host and another service's),
//! crossed with the Region the call was signed for and with both spellings of the API's root
//! path. Each row ends with the refusals that prove the `200` is backed by the issue this
//! registry recorded: no credential at all, and a token this registry never minted.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The zones AWS publishes its own endpoints in are a sibling suite's list, imported rather
// than re-spelled, so the suites cannot drift on what a published address may not name.
use crate::published_registry_address::AWS_ENDPOINT_DNS_SUFFIXES;
use crate::wire::{at, emulator_served_at};

/// The header a registry answers every request with to state which version of the registry
/// API it speaks, and the value for this one. Spelled out here rather than imported so a
/// wrong header in the implementation fails these rows; a client reads it to decide it is
/// talking to a V2 registry at all.
const API_VERSION_HEADER: &str = "docker-distribution-api-version";

/// See [`API_VERSION_HEADER`].
const API_VERSION: &str = "registry/2.0";

/// The media type the registry API's own documents carry — the root's body and every error
/// document.
const REGISTRY_JSON: &str = "application/json";

/// The error code the Docker Registry HTTP API V2 error-code table states for a request
/// presenting no credential the registry honours.
const UNAUTHORIZED_CODE: &str = "UNAUTHORIZED";

/// The label run every Amazon ECR registry NAME carries between the owning account's id and
/// the Region, as the registry URL format `AuthorizationData$proxyEndpoint` documents spells
/// it (`https://aws_account_id.dkr.ecr.region.amazonaws.com`). The URL a `docker login` is
/// pointed at is an ADDRESS rather than such a name, so it carries none of this run — spelled
/// out so a value that gained it fails these rows.
const REGISTRY_LABEL_RUN: &str = ".dkr.ecr.";

/// The two spellings of the ROOT of the registry API, which is the address a client makes its
/// version check at: the API defines its whole surface under `/v2/`, and a client writes the
/// root with or without the trailing slash.
const ROOT_PATHS: [&str; 2] = ["/v2/", "/v2"];

/// The Regions the calls are signed for: a commercial Region and one of a partition with a
/// DNS suffix of its own. Which registry a token speaks for is recorded with the issue rather
/// than spelled into the published URL, so every Region's token must be honoured at the URL
/// published to it.
const SIGNING_REGIONS: [&str; 2] = ["us-west-2", "cn-north-1"];

/// One pairing: where this emulator is SERVED and the endpoint the caller ARRIVED on.
struct Arrival {
    case: &'static str,
    /// The base URL the router is served at, which is what
    /// `smithy_server_runtime::OperationContext::endpoint` reports.
    served_base: &'static str,
    /// The `Host` the call to `GetAuthorizationToken` carries.
    reached: &'static str,
}

/// The pairings the rows drive.
const ARRIVALS: &[Arrival] = &[
    Arrival {
        case: "a client talking to the socket this emulator is served on",
        served_base: "https://127.0.0.1:9297",
        reached: "127.0.0.1:9297",
    },
    Arrival {
        case: "a caller on the registry layout written over the loopback zone",
        served_base: "https://127.0.0.1:36619",
        reached: "000000000000.dkr.ecr.us-west-2.localhost:36619",
    },
    Arrival {
        case: "a caller on the registry host AWS publishes for this account",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.eu-west-1.amazonaws.com",
    },
    Arrival {
        case: "a caller on the registry layout written over an operator's own zone",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.eu-west-1.registry.emulator.test:9297",
    },
    Arrival {
        case: "a caller on this service's own endpoint host, served under an operator's name",
        served_base: "https://registry.emulator.test:8443",
        reached: "api.ecr.eu-west-1.amazonaws.com",
    },
    Arrival {
        case: "a caller on the loopback name, served on an IPv6 socket",
        served_base: "https://[::1]:9443",
        reached: "localhost:9443",
    },
    Arrival {
        case: "a caller on ANOTHER service's host, served at a host of this service",
        served_base: "https://ecr.localhost:8443",
        reached: "dynamodb.localhost:8443",
    },
];

/// The authority a caller reaches this emulator at when it is served at `base`: the base URL
/// without its scheme, host and port together.
fn served_authority(base: &str) -> &str {
    base.split_once("://")
        .map_or(base, |(_scheme, authority)| authority)
        .trim_end_matches('/')
}

/// What a registry answer carries: the status, the API version header a client reads the API
/// off, the content type and the body.
struct Answered {
    status: u16,
    api_version: String,
    content_type: String,
    body: String,
}

impl Answered {
    /// The error code the registry's error document states, or the empty string when the
    /// answer is not one.
    fn code(&self) -> String {
        serde_json::from_str::<Value>(&self.body)
            .ok()
            .and_then(|document| {
                document["errors"][0]["code"]
                    .as_str()
                    .map(ToString::to_string)
            })
            .unwrap_or_default()
    }
}

/// `GET <path>` at the registry address `authority`, presenting `credential` exactly as a
/// container client presents one — never a SigV4 signature, which is the whole point of this
/// plane — and reporting the headers an answer carries as well as its body.
///
/// This crate's shared `wire::registry_get` reports the status and the body only, and the
/// subject here is the API version header, so the request is made here.
async fn registry_request(
    router: &MultiServiceRouter,
    authority: &str,
    path: &str,
    credential: Option<&str>,
) -> Answered {
    let mut request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", authority);
    if let Some(credential) = credential {
        request = request.header("authorization", credential);
    }
    let request = request
        .body(Bytes::new())
        .expect("the registry request is well formed");
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
    let api_version = header(API_VERSION_HEADER);
    let content_type = header("content-type");
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Answered {
        status,
        api_version,
        content_type,
        body,
    }
}

/// `GetAuthorizationToken` as that caller makes it: signed for `region` and addressed to the
/// endpoint the pairing arrived on, answering the URL it published and the token it issued.
async fn issued_at(
    router: &MultiServiceRouter,
    case: &str,
    reached: &str,
    region: &str,
) -> (String, String) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", reached)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            "AmazonEC2ContainerRegistry_V20150921.GetAuthorizationToken",
        )
        .body(Bytes::from(json!({}).to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: GetAuthorizationToken answered a body that is not JSON ({error}): \
             {:?}",
            String::from_utf8_lossy(&bytes)
        )
    });
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAuthorizationToken, got {status} with {body}"
    );
    let member = |pointer: &str| {
        at(&body, pointer)
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a string at {pointer}, got {body}")
            })
            .to_string()
    };
    (
        member("/authorizationData/0/proxyEndpoint"),
        member("/authorizationData/0/authorizationToken"),
    )
}

/// The authority of the published registry URL `url`, asserted to be an address a client on
/// this machine can DIAL as it was handed it: one of the two endpoints this emulator is
/// reached at, behind a scheme, carrying no path to strip and no name of AWS's own to resolve.
fn dialable_authority(arrival: &Arrival, case: &str, url: &str) -> String {
    let authority = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: expected an https registry URL, got {url:?}"));
    assert!(
        !authority.contains('/'),
        "case {case:?}: expected a bare host[:port] a client dials as given, got {url:?}"
    );
    let served = served_authority(arrival.served_base);
    assert!(
        authority == arrival.reached || authority == served,
        "case {case:?}: expected the authority of {url:?} to be the endpoint the caller reached \
         ({:?}) or the address this emulator is served at ({served:?}), got {authority:?}",
        arrival.reached
    );
    assert!(
        authority == served
            || ::service_common::data_plane_addressing::names_the_loopback_interface(authority),
        "case {case:?}: expected an address a client on this machine can dial — the served \
         address {served:?} or a name of the loopback interface this emulator's listeners are \
         bound on — got {authority:?}"
    );
    assert!(
        !authority.to_ascii_lowercase().contains(REGISTRY_LABEL_RUN),
        "case {case:?}: expected an endpoint of this emulator, got the registry name spelling \
         {authority:?}, which is a name to resolve rather than an address to dial"
    );
    for suffix in AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !authority.to_ascii_lowercase().contains(suffix),
            "case {case:?}: {authority:?} carries a name under {suffix:?} — a zone AWS publishes \
             its own endpoints in, which this emulator does not answer in"
        );
    }
    authority.to_string()
}

// ---------------------------------------------------------------------------
// RULE: the published login URL is an address this emulator serves the registry API on, and
// its root answers the API version to the token that same call issued.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_published_login_url_answers_the_registry_version_check_for_its_own_token() {
    for arrival in ARRIVALS {
        for region in SIGNING_REGIONS {
            let router = emulator_served_at(arrival.served_base);
            let case = format!("{}, signed for {region}", arrival.case);
            let (url, token) = issued_at(&router, &case, arrival.reached, region).await;
            let authority = dialable_authority(arrival, &case, &url);

            for path in ROOT_PATHS {
                let case = format!("{case}, root {path}");
                let answered = registry_request(
                    &router,
                    &authority,
                    path,
                    Some(&format!("Basic {token}")),
                )
                .await;
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: the registry API must answer at the published {url:?}, got \
                     HTTP {} with {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.api_version, API_VERSION,
                    "case {case:?}: expected the registry API version a client reads the API \
                     off, got {:?}",
                    answered.api_version
                );
                assert_eq!(
                    answered.body, "{}",
                    "case {case:?}: expected the empty JSON document the root answers, got {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.content_type, REGISTRY_JSON,
                    "case {case:?}: expected {REGISTRY_JSON}, got {:?}",
                    answered.content_type
                );
            }
        }
    }
}

#[tokio::test]
async fn the_published_login_url_refuses_a_credential_this_registry_did_not_mint() {
    // What makes the answer above a credential check rather than a blanket 200: the only
    // thing honoured at the published address is the issue this registry recorded.
    let unminted = ::aws_smithy_types::base64::encode("AWS:nothing-this-registry-ever-minted");

    for arrival in ARRIVALS {
        let router = emulator_served_at(arrival.served_base);
        let case = arrival.case.to_string();
        let (url, token) = issued_at(&router, &case, arrival.reached, SIGNING_REGIONS[0]).await;
        let authority = dialable_authority(arrival, &case, &url);

        // (case, the credential presented)
        let mut refused: Vec<(&str, Option<String>)> = vec![(
            "a token this registry never minted",
            Some(format!("Basic {unminted}")),
        )];
        // A request presenting NO credential is a registry client's only where the address it
        // was published at names no service: the front door reads the first label of a host as
        // a service name, and a credential-less request to such a host is that service's to
        // answer rather than this registry's (`service::registry_v2`'s own claim).
        if ::service_common::data_plane_addressing::service_named_by_host(&authority).is_none() {
            refused.push(("no credential at all", None));
        }
        for (presented, credential) in &refused {
            let case = format!("{case}, {presented}");
            let answered =
                registry_request(&router, &authority, ROOT_PATHS[0], credential.as_deref()).await;
            assert_eq!(
                answered.status, 401,
                "case {case:?}: expected the registry's challenge at {url:?}, got HTTP {} with \
                 {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.code(),
                UNAUTHORIZED_CODE,
                "case {case:?}: expected the registry API's own error code, got {:?}",
                answered.body
            );
            // A client discovers the API from a refusal too, so the version header is stated
            // on the challenge as well.
            assert_eq!(
                answered.api_version, API_VERSION,
                "case {case:?}: expected the registry API version on the challenge, got {:?}",
                answered.api_version
            );
        }

        // The credential that WAS minted is still honoured at the same address, so the
        // refusals above turned on the credential and nothing else.
        let answered =
            registry_request(&router, &authority, ROOT_PATHS[0], Some(&format!("Basic {token}")))
                .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the issued credential must still be honoured at {url:?}, got HTTP {} \
             with {:?}",
            answered.status, answered.body
        );
    }
}
