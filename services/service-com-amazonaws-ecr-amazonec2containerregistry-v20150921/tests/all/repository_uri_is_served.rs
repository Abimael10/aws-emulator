// SPDX-License-Identifier: CC0-1.0

//! The address a repository is published at where BOTH endpoints move: the one a caller
//! REACHED this emulator on and the one this emulator is SERVED at.
//!
//! The rule under test: **`Repository$repositoryUri` is this registry's own host — the owning
//! account, the two fixed labels and the repository's Region — written over the zone this
//! emulator announces its own names in, which is the zone of the endpoint it answers THAT
//! caller's registry requests at (the endpoint the caller reached, whenever a `/v2` request
//! addressed there reaches this registry, and otherwise the address this emulator is served
//! at), followed by `/` and the repository's own name.** The member states what the value is
//! for ("The URI for the repository. You can use this URI for container image `push` and
//! `pull` operations"), so a container client splits it on the first `/` and DIALS what is
//! ahead of that `/`, while a client that reads the value recovers the registry from the same
//! labels: the authority has to name the registry AND to lead back to this process.
//!
//! What this suite adds to its siblings is the CROSS of the two axes they hold fixed, each on
//! its own: `crate::published_registry_address` drives the endpoint a CALL arrived on against
//! one served address, and `crate::repository_uri_is_dialable_wherever_served` drives the
//! SERVED address against one arrival host. Neither answers what happens when the two
//! disagree — a caller that reached an address it can dial while this process is served under
//! a name of an operator's own, or a caller that reached a host naming another service, or one
//! registry, while the served address answers for them all. Each row below pairs a served
//! address with an arrival host and states which of the two the published NAME's zone is taken
//! from, then reads that back through every operation that serializes a `Repository` — the
//! three the fix this suite guards covers — and DIALS the published authority with the
//! credential `GetAuthorizationToken` issued that same caller, so the published address is
//! shown to reach this registry rather than merely to carry the right shape.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{at, emulator_served_at, registry_get, REGISTRY_ID};

/// One pairing of the two endpoints: where this emulator is SERVED, the `Host` a caller
/// ARRIVED on, the endpoint a `docker login` of that caller is pointed at, and the zone every
/// registry name reported to it is written over.
struct Pairing {
    case: &'static str,
    /// The base URL the router is served at, which is what `OperationContext::endpoint`
    /// reports.
    served_base: &'static str,
    /// The `Host` the caller's own calls carry.
    reached: &'static str,
    /// The endpoint this emulator answers that caller's registry requests at: the arrival
    /// host where a registry request addressed there reaches this registry, and the served
    /// address where it does not. Spelled out per row rather than derived, so a value built on
    /// anything else fails these rows.
    published: &'static str,
    /// The zone the registry names reported to that caller are written over: the zone of
    /// `published` — the one RFC 6761 reserves for the loopback interface where that endpoint
    /// is an address literal, and the host itself where it is a name.
    zone: &'static str,
}

/// The pairings the rows drive. Each states WHY the value is the one it is, in the case text:
/// the arrival host wins when a registry request addressed there reaches this registry, and
/// the served address stands in for every arrival host that routes elsewhere — a host AWS
/// publishes its own endpoints at, a host naming ANOTHER service, and a host naming ONE
/// registry, which answers for that Region alone.
const PAIRINGS: &[Pairing] = &[
    Pairing {
        case: "a caller on this service's own AWS endpoint host, served on a loopback socket",
        served_base: "https://127.0.0.1:9297",
        reached: "api.ecr.us-west-2.amazonaws.com",
        published: "127.0.0.1:9297",
        zone: "localhost",
    },
    Pairing {
        case: "a caller on the emulator's own socket, served under a name of an operator's own",
        served_base: "https://registry.emulator.test:8443",
        reached: "127.0.0.1:8443",
        published: "127.0.0.1:8443",
        zone: "localhost",
    },
    Pairing {
        case: "a caller on the loopback name, served on an IPv6 socket",
        served_base: "https://[::1]:9443",
        reached: "localhost:9443",
        published: "localhost:9443",
        zone: "localhost",
    },
    Pairing {
        case: "a caller on ANOTHER service's host, served at a host of this service",
        served_base: "https://ecr.localhost:8443",
        reached: "dynamodb.localhost:8443",
        published: "ecr.localhost:8443",
        zone: "ecr.localhost",
    },
    Pairing {
        case: "a caller on a registry host of one Region, served on a loopback socket",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.us-west-2.localhost:9297",
        published: "127.0.0.1:9297",
        zone: "localhost",
    },
    Pairing {
        case: "a caller on the loopback name, served under an operator's name with no port",
        served_base: "https://registry.emulator.test",
        reached: "localhost",
        published: "localhost",
        zone: "localhost",
    },
];

/// The Regions the rows hold repositories in: a commercial Region and one of a partition with
/// a DNS suffix of its own. The Region keys the stored record and names its ARN, so a
/// published address that took the Region from the record shows up here.
const HOLDING_REGIONS: [&str; 2] = ["us-west-2", "cn-north-1"];

/// The repository names the rows span: the shortest the pattern allows and one carrying a
/// namespace path, which is the part of a URI a reader must get back unchanged —
/// `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$` with
/// `@length(min: 2, max: 256)`:
///
/// ```text
/// smithy_query(query="shape",shape="Repository",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: [&str; 2] = ["ab", "team/app"];

/// Call one operation of this service in `region`, addressed to `host` — the pairing's own
/// arrival endpoint, which this crate's shared harness fixes and this suite varies.
async fn call_at(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
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
            format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let answered: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {operation} answered a body that is not JSON ({error}): {:?}",
            String::from_utf8_lossy(&bytes)
        )
    });
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {answered}"
    );
    answered
}

/// The string at `pointer`, or a panic naming the case and the body that carried none.
fn text(case: &str, body: &Value, pointer: &str) -> String {
    at(body, pointer)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {pointer}, got {body}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// RULE: every operation that serializes a Repository publishes it under its own registry's
// name — this account's registry in the repository's Region — written over the zone of the
// endpoint this emulator answers THAT caller's registry requests at (the arrival endpoint
// where a registry request addressed there reaches this registry, the served address
// otherwise), and that authority answers this registry's Docker Registry v2 requests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_published_address_pairs_the_reached_endpoint_with_the_served_one() {
    for pairing in PAIRINGS {
        let router = emulator_served_at(pairing.served_base);
        for region in HOLDING_REGIONS {
            for name in REPOSITORY_NAMES {
                let case = format!("{}, {region}, repository {name}", pairing.case);
                let authority =
                    format!("{REGISTRY_ID}.dkr.ecr.{region}.{}", pairing.zone);
                let expected = format!("{authority}/{name}");

                let created = call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "CreateRepository",
                    &json!({"repositoryName": name}),
                )
                .await;
                let created_uri = text(&case, &created, "/repository/repositoryUri");
                assert_eq!(
                    created_uri, expected,
                    "case {case:?}: expected CreateRepository to publish {expected:?}, got \
                     {created_uri:?}"
                );

                // The identity the response reports BESIDE the address, off the stored record:
                // the address names the registry too, and these are what a reader takes it
                // from without parsing anything.
                let registry_id = text(&case, &created, "/repository/registryId");
                let arn = text(&case, &created, "/repository/repositoryArn");
                assert_eq!(
                    (registry_id.as_str(), arn.as_str()),
                    (
                        REGISTRY_ID,
                        format!("arn:aws:ecr:{region}:{REGISTRY_ID}:repository/{name}").as_str()
                    ),
                    "case {case:?}: expected the owning account and this Region's ARN beside \
                     the address, got {registry_id:?} and {arn:?}"
                );

                // The read a client discovers the push target through, and the echoed
                // repository a teardown logs.
                for (operation, body, pointer) in [
                    (
                        "DescribeRepositories",
                        json!({"repositoryNames": [name]}),
                        "/repositories/0/repositoryUri",
                    ),
                    (
                        "DeleteRepository",
                        json!({"repositoryName": name}),
                        "/repository/repositoryUri",
                    ),
                ] {
                    let answered =
                        call_at(&router, &case, pairing.reached, region, operation, &body).await;
                    let reported = text(&case, &answered, pointer);
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {operation} to publish {expected:?}, got \
                         {reported:?}"
                    );
                }

                // The URL a `docker login` of that same caller is pointed at is dialled as
                // given, so it carries the endpoint this emulator answers that caller at —
                // the very endpoint whose zone the published name is written over.
                let issued = call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "GetAuthorizationToken",
                    &json!({}),
                )
                .await;
                let proxy_endpoint = text(&case, &issued, "/authorizationData/0/proxyEndpoint");
                let dialled = proxy_endpoint.strip_prefix("https://").unwrap_or_else(|| {
                    panic!("case {case:?}: expected an https registry URL, got {proxy_endpoint:?}")
                });
                assert_eq!(
                    dialled, pairing.published,
                    "case {case:?}: expected the login URL to carry the endpoint this emulator \
                     answers this caller at, got {proxy_endpoint:?}"
                );

                // What the address is FOR: dialled as it was handed over, with the credential
                // this call issued, this registry's `/v2` plane answers at the published name
                // and at the login URL's endpoint alike.
                let token = text(&case, &issued, "/authorizationData/0/authorizationToken");
                for dialable in [authority.as_str(), pairing.published] {
                    let (status, answered) =
                        registry_get(&router, dialable, "/v2/", &token, None).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the registry API must answer at the published address \
                         {dialable:?}, got HTTP {status} with {answered:?}"
                    );
                }
            }
        }
    }
}
