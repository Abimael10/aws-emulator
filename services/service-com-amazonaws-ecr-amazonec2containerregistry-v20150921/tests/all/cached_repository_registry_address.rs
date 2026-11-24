// SPDX-License-Identifier: CC0-1.0

//! The address a repository the REGISTRY ITSELF minted is published at — the mirror a pull
//! through cache rule creates on the first read under its prefix — driven end to end over the
//! wire.
//!
//! The rule under test: **`Repository$repositoryUri` is a registry HOST of the registry that
//! HOLDS the repository, followed by `/` and the repository's own name — the owning account,
//! the two fixed registry labels and that Region, in the zone and on the port this emulator
//! answers the reading caller's registry requests in.** "The URI for the repository. You can
//! use this URI for container image `push` and `pull` operations" is a value a client both
//! DIALS, by splitting it on the first `/`, and PARSES: the labels ahead of the zone are
//! where a client recovers the registry id and the Region from, which is how it builds the
//! `ecr:` resource of a grant, obtains a `docker login` for that registry, and writes the
//! image reference of a task definition or a function. The zone and the port are this
//! emulator's, because they are the half a client DIALS: a name in a zone AWS serves its own
//! registries in resolves at Amazon ECR rather than at the machine this emulator runs on.
//!
//! What this suite adds to its siblings — `crate::repository_registry_uri`, which reads the
//! rule across the Region and the repository name, and `crate::published_registry_address`,
//! which reads it across the endpoint a call arrived on for a repository a CLIENT created —
//! is the repository no `CreateRepository` call ever named: "a pull through cache rule
//! provides a way to cache images from an upstream registry source in your Amazon ECR private
//! registry" (`CreatePullThroughCacheRule`), so the mirror the registry creates on the first
//! covered read is published by the same rule as any other repository, and the path that
//! MINTED it changes nothing about its address.
//!
//! The class the rule generalises over is the Region the mirror is held in (and so the
//! partition), crossed with how deep the rule's prefix and the cached path are and the tag
//! the read asks for, and — in the second test — the endpoint the READ arrived on. The first
//! test then follows the value the way a container client does: it splits the reported URI,
//! dials the authority with the credential that Region's `docker login` token carries, and
//! asks for the manifest at the repository path, which must answer the very document the
//! import cached.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, emulator, ok_in_region, published_registry_authority_in, registry_get, registry_host_in,
    REGISTRY_ID,
};

/// The label run every Amazon ECR registry HOST carries between the owning account's id and
/// the Region, as the registry URL format `AuthorizationData$proxyEndpoint` documents spells
/// it (`https://aws_account_id.dkr.ecr.region.amazonaws.com`). Spelled out here rather than
/// imported so a published address that stopped naming a registry fails this suite.
const REGISTRY_LABEL_RUN: &str = ".dkr.ecr.";

/// The DNS suffixes AWS publishes its own endpoints under, spelled out here rather than
/// imported so a published address slipping into one of them fails these rows.
///
/// Source: `service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, the tree's one
/// list of them.
const AWS_ENDPOINT_DNS_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// The upstream these rules cache from, and the `UpstreamRegistry` that URL belongs to:
///
/// ```text
/// smithy_query(query="operation",operation="CreatePullThroughCacheRule",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const UPSTREAM_URL: &str = "public.ecr.aws";
const UPSTREAM_REGISTRY: &str = "ecr-public";

/// One mirror a cache rule creates: the Region the rule is registered and the first read is
/// made in — the registry that HOLDS the mirror — the rule's `ecrRepositoryPrefix`, the path
/// read under it, and the tag that read asks for.
#[derive(Clone, Copy)]
struct Mirror {
    case: &'static str,
    region: &'static str,
    prefix: &'static str,
    path: &'static str,
    tag: &'static str,
}

impl Mirror {
    /// The repository the covered read addresses, which is the rule's prefix followed by the
    /// path under it — a name carrying `/` separators of its own, which is the part of a URI
    /// a reader must get back unchanged.
    fn repository(&self) -> String {
        format!("{}/{}", self.prefix, self.path)
    }
}

/// The mirrors the rows drive: the Region (four partitions, and none of them the Region this
/// crate's harness signs for by default), crossed with a plain and a nested prefix, a one-,
/// two- and three-level cached path, and the tags a read asks for.
const MIRRORS: &[Mirror] = &[
    Mirror {
        case: "a three-level path under a plain prefix, in a commercial Region",
        region: "us-west-2",
        prefix: "ptc-mirror",
        path: "docker/library/busybox",
        tag: "latest",
    },
    Mirror {
        case: "a one-level path under a nested prefix, in another commercial Region",
        region: "ap-southeast-2",
        prefix: "team/mirror",
        path: "pause",
        tag: "3.9",
    },
    Mirror {
        case: "a two-level path in a GovCloud Region",
        region: "us-gov-east-1",
        prefix: "ptc-mirror",
        path: "library/alpine",
        tag: "3.20",
    },
    Mirror {
        case: "a two-level path in a partition with a DNS suffix of its own",
        region: "cn-north-1",
        prefix: "ptc-mirror",
        path: "library/hello-world",
        tag: "1.0.0",
    },
];

/// Register the mirror's rule, then make the covered read that MINTS the repository, and
/// answer the manifest the import cached — the document a pull at the published URI must
/// serve.
async fn given_mirror(router: &MultiServiceRouter, mirror: &Mirror) -> String {
    ok_in_region(
        router,
        mirror.case,
        mirror.region,
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": mirror.prefix,
            "upstreamRegistry": UPSTREAM_REGISTRY,
            "upstreamRegistryUrl": UPSTREAM_URL,
        }),
    )
    .await;
    let read = ok_in_region(
        router,
        mirror.case,
        mirror.region,
        "BatchGetImage",
        &json!({
            "repositoryName": mirror.repository(),
            "imageIds": [{"imageTag": mirror.tag}],
        }),
    )
    .await;
    assert_eq!(
        at(&read, "/failures"),
        json!([]),
        "case {:?}: expected the covered reference to be served by caching it, got {read}",
        mirror.case
    );
    at(&read, "/images/0/imageManifest")
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the cached image's manifest, got {read}",
                mirror.case
            )
        })
        .to_string()
}

/// The `repositoryUri` of the first repository `DescribeRepositories` reported, or a panic
/// naming the case and the body that carried none.
fn reported_uri(case: &str, described: &Value) -> String {
    at(described, "/repositories/0/repositoryUri")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a repositoryUri, got {described}"))
        .to_string()
}

/// The authorization token this registry issues for `region`, as a `docker login` of a caller
/// in that Region is handed it.
async fn token_for(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let issued = ok_in_region(router, case, region, "GetAuthorizationToken", &json!({})).await;
    at(&issued, "/authorizationData/0/authorizationToken")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an authorizationToken, got {issued}"))
        .to_string()
}

#[tokio::test]
async fn a_cached_repositorys_published_uri_names_the_registry_that_holds_it() {
    for mirror in MIRRORS {
        let router = emulator();
        let case = mirror.case;
        let repository = mirror.repository();
        let cached_manifest = given_mirror(&router, mirror).await;

        let described = ok_in_region(
            &router,
            case,
            mirror.region,
            "DescribeRepositories",
            &json!({"repositoryNames": [repository]}),
        )
        .await;
        let uri = reported_uri(case, &described);
        let expected = format!(
            "{}/{repository}",
            published_registry_authority_in(mirror.region)
        );
        assert_eq!(
            uri, expected,
            "case {case:?}: expected the mirror to be published at {expected:?}, got {uri:?}"
        );

        // The two parts a container client splits the value into: the authority it dials and
        // parses, and the path after the first `/`, which is exactly the repository's own
        // name — the rule's prefix and the cached path, however many segments each carries.
        let (authority, path) = uri
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no repository path"));
        assert_eq!(
            path, repository,
            "case {case:?}: expected the path of {uri:?} to be the repository name \
             {repository:?}, got {path:?}"
        );

        // What a client PARSES out of the authority: the registry holding the repository,
        // which must be the identity the same response reports beside the address.
        let reported_registry = at(&described, "/repositories/0/registryId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let reported_arn = at(&described, "/repositories/0/repositoryArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (reported_registry.as_str(), reported_arn.as_str()),
            (
                REGISTRY_ID,
                format!(
                    "arn:aws:ecr:{}:{REGISTRY_ID}:repository/{repository}",
                    mirror.region
                )
                .as_str()
            ),
            "case {case:?}: expected the owning account and this Region's ARN beside {uri:?}, \
             got {reported_registry:?} and {reported_arn:?}"
        );
        let region_label = authority
            .strip_prefix(&format!("{reported_registry}{REGISTRY_LABEL_RUN}"))
            .and_then(|rest| rest.split_once('.'))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected {authority:?} to be a registry host of the \
                     account {reported_registry:?}"
                )
            })
            .0;
        assert_eq!(
            region_label, mirror.region,
            "case {case:?}: expected the Region label of {authority:?} to be the Region the \
             mirror is held in, got {region_label:?}"
        );

        // The zone and the port are this emulator's, which is what makes the address one a
        // client on this machine can dial: never the host AWS serves this registry at, and
        // never any other name in a zone AWS publishes its own endpoints in.
        assert_ne!(
            authority,
            registry_host_in(mirror.region),
            "case {case:?}: {uri:?} names the host AWS publishes for this registry, which \
             resolves nowhere near this process"
        );
        for suffix in AWS_ENDPOINT_DNS_SUFFIXES {
            assert!(
                !authority.to_ascii_lowercase().contains(suffix),
                "case {case:?}: {uri:?} carries a name under {suffix:?} — a zone AWS \
                 publishes its own endpoints in, which this emulator does not answer in"
            );
        }

        // What the value is FOR: the reference a client writes from it (`<uri>:<tag>`),
        // dialled the way a container client dials one — the authority as the host, the
        // repository path and the tag as the manifest route — answers the very document the
        // import cached.
        let token = token_for(&router, case, mirror.region).await;
        let (status, served) = registry_get(
            &router,
            authority,
            &format!("/v2/{path}/manifests/{}", mirror.tag),
            &token,
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a pull of {uri:?}:{} must reach this registry, got HTTP {status} \
             with {served:?}",
            mirror.tag
        );
        assert_eq!(
            serde_json::from_str::<Value>(&served).unwrap_or(Value::Null),
            serde_json::from_str::<Value>(&cached_manifest).unwrap_or(Value::Null),
            "case {case:?}: expected the cached manifest from the published URI, got \
             {served:?}"
        );
    }
}

/// One endpoint a caller reads a mirror on: the `Host` it addressed, and the zone and the
/// port the address it is reported is published in — those of the endpoint this emulator
/// answers THAT caller's registry requests at, which for a caller on a host AWS publishes its
/// own endpoints at is the address this emulator is served at
/// (`crate::wire::ANNOUNCED_REGISTRY_ENDPOINT`).
struct ReadingEndpoint {
    case: &'static str,
    host: &'static str,
    zone: &'static str,
    port: &'static str,
}

impl ReadingEndpoint {
    /// The authority a repository of `region` is published at to a read that arrived here.
    fn published_authority_in(&self, region: &str) -> String {
        format!(
            "{REGISTRY_ID}{REGISTRY_LABEL_RUN}{region}.{}:{}",
            self.zone, self.port
        )
    }
}

/// The endpoints the second test reads each mirror on: this service's own regional endpoint
/// host (a name AWS publishes, so the served address stands in its place), the emulator's own
/// socket, a loopback name on another port, and a host of THIS service an operator serves the
/// emulator at.
const READING_ENDPOINTS: &[ReadingEndpoint] = &[
    ReadingEndpoint {
        case: "this service's own regional endpoint host",
        host: "api.ecr.us-west-2.amazonaws.com",
        zone: "localhost",
        port: "9297",
    },
    ReadingEndpoint {
        case: "the emulator's own socket",
        host: "127.0.0.1:9297",
        zone: "localhost",
        port: "9297",
    },
    ReadingEndpoint {
        case: "a loopback name on another port",
        host: "localhost:39355",
        zone: "localhost",
        port: "39355",
    },
    ReadingEndpoint {
        case: "a host of this service an operator serves the emulator at",
        host: "ecr.localhost:39355",
        zone: "ecr.localhost",
        port: "39355",
    },
];

/// `DescribeRepositories` for `repository` in `region`, addressed to `host` — the endpoint
/// the READ arrives on, which is what the second test varies.
async fn describe_at(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    repository: &str,
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
            "AmazonEC2ContainerRegistry_V20150921.DescribeRepositories",
        )
        .body(bytes::Bytes::from(
            json!({"repositoryNames": [repository]}).to_string(),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let body: Value = serde_json::from_slice(&answered).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: DescribeRepositories answered a body that is not JSON ({error}): \
             {:?}",
            String::from_utf8_lossy(&answered)
        )
    });
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeRepositories, got {status} with {body}"
    );
    body
}

#[tokio::test]
async fn a_cached_repositorys_registry_labels_do_not_move_with_the_endpoint_that_read_it() {
    // The two halves of the authority answer to two different things, and this is what keeps
    // them apart: the LABELS are the stored record's — this account's registry in the Region
    // holding the mirror — so no reader's endpoint moves them, while the ZONE and the PORT are
    // the endpoint this emulator answers that reader at, so every reader is handed an address
    // it can dial.
    for mirror in MIRRORS {
        let router = emulator();
        let repository = mirror.repository();
        given_mirror(&router, mirror).await;

        for endpoint in READING_ENDPOINTS {
            let case = format!("{}, read on {}", mirror.case, endpoint.case);
            let described =
                describe_at(&router, &case, endpoint.host, mirror.region, &repository).await;
            let uri = reported_uri(&case, &described);
            let expected = format!(
                "{}/{repository}",
                endpoint.published_authority_in(mirror.region)
            );
            assert_eq!(
                uri, expected,
                "case {case:?}: expected the mirror to be published at {expected:?}, got \
                 {uri:?}"
            );
            // The identity reported beside the address is the record's as well, so the two
            // readings of it cannot disagree whoever read them.
            assert_eq!(
                at(&described, "/repositories/0/repositoryArn"),
                json!(format!(
                    "arn:aws:ecr:{}:{REGISTRY_ID}:repository/{repository}",
                    mirror.region
                )),
                "case {case:?}: expected the stored ARN to be unchanged by the read's \
                 endpoint, got {described}"
            );
        }
    }
}
