// SPDX-License-Identifier: CC0-1.0

//! What the address this registry announces is COMPOSED of — nothing.
//!
//! RULE under test: **every address this registry hands a caller to DIAL is one of the two
//! addresses this emulator is really reached at — the endpoint the CALLER arrived on, or the
//! address this emulator is SERVED at — and nothing is composed on top of either.**
//! `Repository$repositoryUri` is "The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations" and `AuthorizationData$proxyEndpoint` is "The
//! registry URL to use for this authorization token in a `docker login` command": both are
//! ADDRESSES a container client dials as it was handed them, so the authority has to be a
//! `host[:port]` that leads back to this process. A name built out of the facts the registry
//! knows — the owning account's id, the Region of the record, the two fixed labels of a
//! registry host, the zone the caller arrived in — is NOT such an address: AWS's own spelling
//! resolves at the real Amazon ECR or nowhere, and that same layout in the zone the call came
//! from resolves only where a resolver implements that zone, so a client that was handed one
//! can neither log in nor push nor pull. One endpoint answers for every registry this emulator
//! serves, so the announced address names no registry at all: WHICH registry holds the
//! repository is reported BESIDE it, by `registryId` and `repositoryArn`, off the stored
//! record.
//!
//! What this suite adds to its siblings is WHAT the value may be made of rather than which of
//! the two endpoints wins. `crate::published_registry_address` varies the endpoint a call
//! arrived on, `crate::repository_uri_is_dialable_wherever_served` the address the emulator is
//! served at, and `crate::repository_uri_is_served` crosses the two — each spelling the
//! expected value out per row. The rows here assert the derivation-free invariant those per-row
//! values are instances of: the authority is one of the two endpoints this process is reached
//! at, and never a third value assembled from the account, the Region, the registry labels or
//! the arrival zone — which is exactly what an announced
//! `<registryId>.dkr.ecr.<region>.<arrival zone>:<arrival port>` is, a value that carries the
//! caller's own port and zone and is still an address nothing answers on. Each row then does
//! what the value is FOR: a client holding ONLY the announced URI splits it on the first `/`,
//! dials the authority with the credential that same call issued and is served the manifest
//! the repository holds.
//!
//! The rows differ along what the rule generalises over: the address the emulator is served at
//! (a loopback socket, another port on it, an IPv6 socket, a host of this service, a host of an
//! operator's own) crossed with the endpoint the caller arrived on (that socket itself, a
//! loopback name, a host of another service, the registry host AWS publishes, and that registry
//! layout composed in the loopback zone, in an operator's zone, for another account and for
//! another partition's Region) — crossed with the Region whose registry HOLDS the repository
//! and with the repository-name forms `RepositoryName`'s pattern allows, because neither the
//! record nor the name is an address either.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{at, emulator_served_at, given_layer_in_region, registry_get, REGISTRY_ID};

// The zones AWS publishes its own endpoints in are a sibling suite's list, imported rather
// than re-spelled, so the suites cannot drift on what an announced address may not name.
use crate::published_registry_address::AWS_ENDPOINT_DNS_SUFFIXES;

/// The media type a Docker v2 manifest is pushed and served under.
const DOCKER_V2_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of the config blob a Docker v2 manifest references.
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// The label run every Amazon ECR registry HOST carries between the owning account's id and
/// the Region, as the registry URL format `AuthorizationData$proxyEndpoint` documents spells it
/// (`https://aws_account_id.dkr.ecr.region.amazonaws.com`). No address this emulator announces
/// carries it, in any zone: spelled out here so a value that slipped into that spelling fails
/// these rows.
const REGISTRY_LABEL_RUN: &str = ".dkr.ecr.";

/// The tag the image of every row is pushed under.
const TAG: &str = "v1";

/// One pairing: where this emulator is SERVED, and the endpoint a caller ARRIVED on.
///
/// Which of the two the announced address carries is the sibling suites' subject; what is
/// asserted here is that it carries ONE OF THEM.
struct Pairing {
    case: &'static str,
    /// The base URL the router is served at, which is what
    /// `smithy_server_runtime::OperationContext::endpoint` reports.
    served_base: &'static str,
    /// The `Host` every call of the row carries.
    reached: &'static str,
}

/// The pairings the rows drive.
const PAIRINGS: &[Pairing] = &[
    Pairing {
        case: "a client of a local emulator, talking to the socket it is served on",
        served_base: "https://127.0.0.1:9297",
        reached: "127.0.0.1:9297",
    },
    Pairing {
        case: "a caller on the registry layout composed in the loopback zone",
        served_base: "https://127.0.0.1:36505",
        reached: "000000000000.dkr.ecr.us-west-2.localhost:36505",
    },
    Pairing {
        case: "a caller on the registry host AWS publishes for this account",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.eu-west-1.amazonaws.com",
    },
    Pairing {
        case: "a caller on this service's own endpoint host, served under an operator's name",
        served_base: "https://registry.emulator.test:8443",
        reached: "api.ecr.eu-west-1.amazonaws.com",
    },
    Pairing {
        case: "a caller on the loopback name, served on an IPv6 socket",
        served_base: "https://[::1]:9443",
        reached: "localhost:9443",
    },
    Pairing {
        case: "a caller on ANOTHER service's host, served at a host of this service",
        served_base: "https://ecr.localhost:8443",
        reached: "dynamodb.localhost:8443",
    },
    Pairing {
        case: "a caller on the registry layout composed in an operator's own zone",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.eu-west-1.registry.emulator.test:9297",
    },
    Pairing {
        case: "a caller on the registry layout of ANOTHER account",
        served_base: "https://127.0.0.1:9297",
        reached: "111122223333.dkr.ecr.eu-west-1.localhost:9297",
    },
    Pairing {
        case: "a caller on the registry layout of another partition's Region",
        served_base: "https://127.0.0.1:9297",
        reached: "000000000000.dkr.ecr.cn-north-1.amazonaws.com.cn",
    },
];

/// The Regions the rows hold repositories in: a commercial Region and one of a partition with a
/// DNS suffix of its own. The Region keys the stored record and names its ARN, so an announced
/// address that took the Region from the record shows up here.
const HOLDING_REGIONS: [&str; 2] = ["eu-west-1", "cn-north-1"];

/// The repository names the rows span — the shortest the pattern allows and one carrying a
/// namespace path, which is the part of a URI a client must get back unchanged:
/// `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$` with
/// `@length(min: 2, max: 256)`:
///
/// ```text
/// smithy_query(query="shape",shape="Repository",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: [&str; 2] = ["ab", "team/app"];

/// The authority a caller reaches this emulator at when it is served at `base`: the base URL
/// without its scheme, host and port together.
fn served_authority(base: &str) -> &str {
    base.split_once("://")
        .map_or(base, |(_scheme, authority)| authority)
        .trim_end_matches('/')
}

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

/// The authority of the announced URI `uri` for the repository `name`, asserted to be an
/// address this emulator is reached at and composed of nothing else.
///
/// Four readings, each of them what makes the value dialable by whoever was handed it:
///
/// * it is `<authority>/<name>`, so a client that splits it on the first `/` dials the
///   authority and asks for the repository the path names;
/// * the authority is the endpoint the caller ARRIVED on or the address this emulator is
///   SERVED at — never a third value assembled from the account, the Region, the registry's
///   fixed labels or the arrival zone;
/// * a client on this machine can open a connection to it: it is the served address, or a name
///   the loopback interface this emulator's listeners are bound on answers for
///   (`service_common::data_plane_addressing::names_the_loopback_interface`, the tree's one
///   reading of that);
/// * it names no registry and no zone AWS publishes its own endpoints in, so nothing sends the
///   next push out of the emulated environment carrying the credential it is holding.
fn announced_authority(pairing: &Pairing, case: &str, uri: &str, name: &str) -> String {
    let (authority, path) = uri
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no repository path"));
    assert_eq!(
        path, name,
        "case {case:?}: expected the path of {uri:?} to be the repository name {name:?}, got \
         {path:?}"
    );
    let served = served_authority(pairing.served_base);
    assert!(
        authority == pairing.reached || authority == served,
        "case {case:?}: expected the authority of {uri:?} to be the endpoint the caller reached \
         ({:?}) or the address this emulator is served at ({served:?}) — a client dials it as it \
         was handed it, so nothing may be composed on top of either — got {authority:?}",
        pairing.reached
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
        "case {case:?}: expected an address this emulator answers on, got the registry host \
         spelling {authority:?}, which no resolver here answers for"
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
// RULE: every announced address is one of the two endpoints this emulator is reached at, and a
// client holding only the announced URI is served the repository's own image at it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_announced_address_is_an_endpoint_this_emulator_is_reached_at_and_nothing_composed() {
    for pairing in PAIRINGS {
        for region in HOLDING_REGIONS {
            let router = emulator_served_at(pairing.served_base);
            for name in REPOSITORY_NAMES {
                let case = format!("{}, {region}, repository {name}", pairing.case);

                let created = call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "CreateRepository",
                    &json!({"repositoryName": name}),
                )
                .await;
                let announced = announced_authority(
                    pairing,
                    &case,
                    &text(&case, &created, "/repository/repositoryUri"),
                    name,
                );

                // The identity the response reports BESIDE the address, off the stored record:
                // the address names no registry, so this is where a reader takes one from —
                // and it is the only place the account and the Region appear.
                let registry_id = text(&case, &created, "/repository/registryId");
                let arn = text(&case, &created, "/repository/repositoryArn");
                assert_eq!(
                    (registry_id.as_str(), arn.as_str()),
                    (
                        REGISTRY_ID,
                        format!("arn:aws:ecr:{region}:{REGISTRY_ID}:repository/{name}").as_str()
                    ),
                    "case {case:?}: expected the owning account and this Region's ARN beside the \
                     address, got {registry_id:?} and {arn:?}"
                );

                // The read a client discovers the push target through reports that same one
                // address.
                let described = call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "DescribeRepositories",
                    &json!({"repositoryNames": [name]}),
                )
                .await;
                let read_back = announced_authority(
                    pairing,
                    &case,
                    &text(&case, &described, "/repositories/0/repositoryUri"),
                    name,
                );
                assert_eq!(
                    read_back, announced,
                    "case {case:?}: expected the read to publish the repository at the address \
                     the create did ({announced:?}), got {read_back:?}"
                );

                // One image in that repository, pushed through the documented sequence, so the
                // pull below answers what the push stored rather than anything this suite made.
                let config_bytes = format!("config of {name} in {region}").into_bytes();
                let config =
                    given_layer_in_region(&router, &case, region, name, &config_bytes).await;
                let manifest = json!({
                    "schemaVersion": 2,
                    "mediaType": DOCKER_V2_MANIFEST,
                    "config": {
                        "mediaType": DOCKER_CONFIG_MEDIA_TYPE,
                        "digest": config,
                        "size": config_bytes.len(),
                    },
                    "layers": [],
                })
                .to_string();
                call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "PutImage",
                    &json!({
                        "repositoryName": name,
                        "imageManifest": manifest,
                        "imageTag": TAG,
                    }),
                )
                .await;

                // The URL a `docker login` of that same caller is pointed at is that one
                // address behind a scheme, so a client logs in and pushes at one authority.
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
                assert_eq!(
                    proxy_endpoint,
                    format!("https://{announced}"),
                    "case {case:?}: expected the login URL to carry the address the repository is \
                     published at ({announced:?}), got {proxy_endpoint:?}"
                );

                // What the address is FOR: a client holding only the announced URI dials the
                // authority with the credential this call issued and is served the manifest the
                // repository holds.
                let token = text(&case, &issued, "/authorizationData/0/authorizationToken");
                let (status, served_body) = registry_get(
                    &router,
                    &announced,
                    &format!("/v2/{name}/manifests/{TAG}"),
                    &token,
                    Some(DOCKER_V2_MANIFEST),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a pull at the announced address {announced:?} must reach this \
                     registry, got HTTP {status} with {served_body:?}"
                );
                assert_eq!(
                    served_body, manifest,
                    "case {case:?}: expected the manifest the push stored, got {served_body:?}"
                );

                // The echo a teardown reads carries that same one address too.
                let removed = call_at(
                    &router,
                    &case,
                    pairing.reached,
                    region,
                    "DeleteRepository",
                    &json!({"repositoryName": name, "force": true}),
                )
                .await;
                let on_delete = announced_authority(
                    pairing,
                    &case,
                    &text(&case, &removed, "/repository/repositoryUri"),
                    name,
                );
                assert_eq!(
                    on_delete, announced,
                    "case {case:?}: expected the delete to echo the repository at the address it \
                     was published at ({announced:?}), got {on_delete:?}"
                );
            }
        }
    }
}
