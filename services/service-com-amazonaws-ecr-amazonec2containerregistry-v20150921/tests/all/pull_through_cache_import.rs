// SPDX-License-Identifier: CC0-1.0

//! The on-demand upstream import a pull through cache rule makes, end to end over the wire.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.
//!
//! The FIRST rule asserted over the tables below: **a read of an image under a repository
//! name a registered pull through cache rule's `ecrRepositoryPrefix` covers is served by
//! CACHING the upstream image — creating the mirrored repository on that first read and
//! reporting the image it stored — rather than refused as an absent repository; a read of a
//! name no rule covers is still refused `RepositoryNotFoundException`, and one a rule covers
//! but whose upstream cannot be reached through that rule is refused
//! `UnableToGetUpstreamImageException`.** The mirrored repository exists from then on, so
//! every later read of it — `BatchGetImage`, `DescribeImages`, `DescribeRepositories` — and
//! a `docker pull` on the registry endpoint report the same cached image.
//!
//! The SECOND: **what the cached manifest references, this registry HOLDS — an image is
//! cached with its filesystem layers, so a cached image is as complete as a pushed one: its
//! layers are available and downloadable, and a basic scan of it is accepted and reads a
//! filesystem, exactly as for an image a client pushed.** `UnsupportedImageTypeException` is
//! reserved for a manifest a basic scan genuinely cannot examine (a manifest list, an index,
//! anything that is not a container image), and its sentence is AWS's, which ends at "is of
//! a type that cannot be scanned".
//!
//! The rows differ along what the rules generalise over: which upstream registry the rule
//! caches from and whether it authenticates its callers, how the prefix is written (plain,
//! with its assumed trailing slash, nested), how many namespace levels the cached path
//! carries under it, which tag is asked for, which upstream prefix the rule prepends, which
//! plane the read arrives on, which manifest form declares no filesystem layer, and how a
//! scan's request addresses the image (by tag, by digest, by both).

use serde_json::{json, Value};

use crate::registry_v2_pull::{fetch, given_credential};
use crate::wire::{
    at, call, emulator, fetch_url, layer_digest, ok, ok_until, refused, REGION, REGISTRY_ID,
};

/// A Secrets Manager secret ARN in the namespace `CredentialArn`'s modelled pattern states
/// (smithy_query operation=CreatePullThroughCacheRule, member credentialArn:
/// `^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`).
const SECRET: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/upstream-AbCdEf";

/// An IAM role ARN for `customRoleArn`, "the ARN of the IAM role associated with the pull
/// through cache rule"; the model constrains that member by length alone.
const ROLE: &str = "arn:aws:iam::000000000000:role/ecr-pull-through-cache";

/// The tag a client that names none asks for, which is the one a bare reference carries.
const DEFAULT_TAG: &str = "latest";

/// One row of the import table: the rule to register and the reference read through it.
///
/// The upstream registries and the URL syntax for each are the `upstreamRegistryUrl`
/// member's own catalog (smithy_query operation=CreatePullThroughCacheRule,
/// member=upstreamRegistryUrl), paired with the `UpstreamRegistry` values that member lists
/// them under (smithy_query shape=UpstreamRegistry).
struct Row<'a> {
    case: &'a str,
    /// `ecrRepositoryPrefix` as the create request carries it.
    prefix: &'a str,
    /// `upstreamRegistry`; `None` leaves the member out, so it is resolved from the URL.
    upstream_registry: Option<&'a str>,
    upstream_registry_url: &'a str,
    credential_arn: Option<&'a str>,
    custom_role_arn: Option<&'a str>,
    /// `upstreamRepositoryPrefix`; `None` leaves the member out, which ECR treats as `ROOT`.
    upstream_repository_prefix: Option<&'a str>,
    /// The path under the prefix the read addresses, with the prefix's assumed `/` between
    /// them: `<prefix>/<path>` is the repository the import caches into.
    path: &'a str,
    /// The tag the read asks for.
    tag: &'a str,
}

/// The import table.
fn rows() -> Vec<Row<'static>> {
    vec![
        Row {
            case: "amazon ecr public, a three-level path under the prefix",
            prefix: "ptc-public",
            upstream_registry: Some("ecr-public"),
            upstream_registry_url: "public.ecr.aws",
            credential_arn: None,
            custom_role_arn: None,
            upstream_repository_prefix: None,
            path: "docker/library/hello-world",
            tag: DEFAULT_TAG,
        },
        Row {
            case: "kubernetes, resolved from its URL, one path segment and a version tag",
            prefix: "k8s-mirror",
            upstream_registry: None,
            upstream_registry_url: "registry.k8s.io",
            credential_arn: None,
            custom_role_arn: None,
            upstream_repository_prefix: None,
            path: "pause",
            tag: "3.9",
        },
        Row {
            case: "docker hub through its secret, the prefix written with its assumed slash",
            prefix: "hub/",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: Some(SECRET),
            custom_role_arn: None,
            upstream_repository_prefix: None,
            path: "library/busybox",
            tag: "1.36.1",
        },
        Row {
            case: "quay through a custom role, under a nested prefix",
            prefix: "team/mirror",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            credential_arn: None,
            custom_role_arn: Some(ROLE),
            upstream_repository_prefix: None,
            path: "prometheus/node-exporter",
            tag: "v1.8.2",
        },
        Row {
            case: "github container registry, with an upstream prefix the rule prepends",
            prefix: "ghcr",
            upstream_registry: Some("github-container-registry"),
            upstream_registry_url: "ghcr.io",
            credential_arn: Some(SECRET),
            custom_role_arn: None,
            upstream_repository_prefix: Some("mirror"),
            path: "actions/runner",
            tag: "2.321.0",
        },
    ]
}

/// The repository name a row's read addresses: the prefix without the trailing slash ECR
/// assumes, then the path under it.
fn cached_repository(row: &Row<'_>) -> String {
    format!(
        "{}/{}",
        row.prefix.strip_suffix('/').unwrap_or(row.prefix),
        row.path
    )
}

/// Register the row's rule through `CreatePullThroughCacheRule`.
async fn given_rule(router: &smithy_server_runtime::MultiServiceRouter, row: &Row<'_>) {
    let mut request = json!({
        "ecrRepositoryPrefix": row.prefix,
        "upstreamRegistryUrl": row.upstream_registry_url,
    });
    for (member, value) in [
        ("upstreamRegistry", row.upstream_registry),
        ("credentialArn", row.credential_arn),
        ("customRoleArn", row.custom_role_arn),
        ("upstreamRepositoryPrefix", row.upstream_repository_prefix),
    ] {
        if let Some(value) = value {
            request[member] = json!(value);
        }
    }
    ok(router, row.case, "CreatePullThroughCacheRule", &request).await;
}

/// One `BatchGetImage` read of `repository` asking for `tag`.
async fn read_by_tag(
    router: &smithy_server_runtime::MultiServiceRouter,
    repository: &str,
    tag: &str,
) -> crate::wire::Answer {
    call(
        router,
        "BatchGetImage",
        &json!({"repositoryName": repository, "imageIds": [{"imageTag": tag}]}),
    )
    .await
}

/// The one image a `BatchGetImage` answer reports, or a panic naming the case.
fn single_image(case: &str, answer: &Value) -> Value {
    let failures = answer["failures"].as_array().cloned().unwrap_or_default();
    assert!(
        failures.is_empty(),
        "case {case:?}: expected no failures, got {failures:?}"
    );
    let images = answer["images"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        images.len(),
        1,
        "case {case:?}: expected exactly one image, got {images:?}"
    );
    images[0].clone()
}

#[tokio::test]
async fn a_first_read_under_a_rule_caches_the_upstream_image_and_creates_the_repository() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        given_rule(&router, &row).await;
        let repository = cached_repository(&row);

        // A rule the registry reports as valid is one an image can be pulled through, so
        // the verdict and the import cannot disagree.
        let validated = ok(
            &router,
            case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": row.prefix}),
        )
        .await;
        assert_eq!(
            at(&validated, "/isValid"),
            json!(true),
            "case {case:?}: expected the registered rule to validate, got {validated:?}"
        );

        // Nothing is cached under the prefix yet: the registry holds no such repository.
        let before = call(
            &router,
            "DescribeRepositories",
            &json!({"repositoryNames": [repository]}),
        )
        .await;
        assert_eq!(
            before.error_code().rsplit('#').next().unwrap_or_default(),
            "RepositoryNotFoundException",
            "case {case:?}: expected the cached repository to be absent before the first \
             read, got {:?}",
            before.body
        );

        // The first read IS the import: it answers the cached image rather than a refusal.
        let answer = read_by_tag(&router, &repository, row.tag).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from the first read of a covered repository, \
             got {} with {:?}",
            answer.status, answer.body
        );
        let image = single_image(case, &answer.body);
        assert_eq!(
            at(&image, "/imageId/imageTag"),
            json!(row.tag),
            "case {case:?}: expected the tag that was asked for, got {image:?}"
        );
        assert_eq!(
            at(&image, "/repositoryName"),
            json!(repository.clone()),
            "case {case:?}: expected the cached repository's name, got {image:?}"
        );
        assert_eq!(
            at(&image, "/registryId"),
            json!(REGISTRY_ID),
            "case {case:?}: expected this registry's id, got {image:?}"
        );
        let manifest = at(&image, "/imageManifest")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !manifest.is_empty(),
            "case {case:?}: expected a non-empty manifest, got {image:?}"
        );
        let document: Value = serde_json::from_str(&manifest).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the cached manifest to be JSON ({error}): {manifest}")
        });
        assert_eq!(
            at(&document, "/schemaVersion"),
            json!(2),
            "case {case:?}: expected an image manifest, got {manifest}"
        );
        let digest = at(&image, "/imageId/imageDigest")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            digest.starts_with("sha256:"),
            "case {case:?}: expected a content digest for the cached image, got {digest:?}"
        );

        // What the cached manifest REFERENCES is in the repository: the registry holds the
        // blob its configuration descriptor addresses, under the digest and the size the
        // descriptor states.
        let config_digest = at(&document, "/config/digest")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let availability = ok(
            &router,
            case,
            "BatchCheckLayerAvailability",
            &json!({"repositoryName": repository, "layerDigests": [config_digest.clone()]}),
        )
        .await;
        assert_eq!(
            at(&availability, "/layers/0/layerAvailability"),
            json!("AVAILABLE"),
            "case {case:?}: expected the repository to hold the blob the cached manifest \
             references ({config_digest}), got {availability:?}"
        );
        assert_eq!(
            at(&availability, "/layers/0/layerSize"),
            at(&document, "/config/size"),
            "case {case:?}: expected the stored blob's size to be the one the manifest \
             states, got {availability:?}"
        );

        // The repository exists from then on, and every other read of the registry reports
        // the cached image.
        let described = ok(
            &router,
            case,
            "DescribeRepositories",
            &json!({"repositoryNames": [repository]}),
        )
        .await;
        assert_eq!(
            at(&described, "/repositories/0/repositoryName"),
            json!(repository.clone()),
            "case {case:?}: expected the mirrored repository to be reported, got {described:?}"
        );
        assert_eq!(
            at(&described, "/repositories/0/repositoryArn"),
            json!(format!(
                "arn:aws:ecr:{REGION}:{REGISTRY_ID}:repository/{repository}"
            )),
            "case {case:?}: expected the cached repository's ARN in the Region the read was \
             made against, got {described:?}"
        );
        let images = ok(
            &router,
            case,
            "DescribeImages",
            &json!({"repositoryName": repository}),
        )
        .await;
        assert_eq!(
            at(&images, "/imageDetails/0/imageDigest"),
            json!(digest.clone()),
            "case {case:?}: expected the cached image to be reported by DescribeImages, got \
             {images:?}"
        );
        assert_eq!(
            at(&images, "/imageDetails/0/imageTags"),
            json!([row.tag]),
            "case {case:?}: expected the cached image to carry the tag it was cached under, \
             got {images:?}"
        );

        // A second read of the same reference answers the same image: the import happened
        // once, and reading it again mints nothing new.
        let again = read_by_tag(&router, &repository, row.tag).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected HTTP 200 from the second read, got {} with {:?}",
            again.status, again.body
        );
        assert_eq!(
            at(&single_image(case, &again.body), "/imageId/imageDigest"),
            json!(digest.clone()),
            "case {case:?}: expected the second read to answer the cached image, got {:?}",
            again.body
        );
        let still = ok(
            &router,
            case,
            "DescribeImages",
            &json!({"repositoryName": repository}),
        )
        .await;
        assert_eq!(
            still["imageDetails"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: expected the repository to hold the one cached image, got {still:?}"
        );
    }
}

/// One row of the uncovered-read table: what is read and why no rule covers it.
struct Uncovered<'a> {
    case: &'a str,
    /// The repository the read addresses.
    repository: &'a str,
    /// The `imageIds` entry it carries.
    identifier: Value,
}

#[tokio::test]
async fn a_read_no_rule_covers_is_still_refused_as_an_absent_repository() {
    let router = emulator();
    // One rule, so every row below is a read this registry has a rule to compare against.
    given_rule(
        &router,
        &Row {
            case: "the registered rule",
            prefix: "mirror",
            upstream_registry: Some("ecr-public"),
            upstream_registry_url: "public.ecr.aws",
            credential_arn: None,
            custom_role_arn: None,
            upstream_repository_prefix: None,
            path: "docker/library/hello-world",
            tag: DEFAULT_TAG,
        },
    )
    .await;

    let rows = vec![
        Uncovered {
            case: "a repository name under no prefix at all",
            repository: "standalone/app",
            identifier: json!({"imageTag": DEFAULT_TAG}),
        },
        Uncovered {
            case: "a name that only begins with the prefix's characters",
            repository: "mirrored/app",
            identifier: json!({"imageTag": DEFAULT_TAG}),
        },
        Uncovered {
            case: "the prefix itself, which names no path under the rule",
            repository: "mirror",
            identifier: json!({"imageTag": DEFAULT_TAG}),
        },
        Uncovered {
            // An import has no upstream manifest here to match a digest against, so a
            // reference that addresses content rather than a tag caches nothing.
            case: "a covered path addressed by a digest",
            repository: "mirror/docker/library/hello-world",
            identifier: json!({
                "imageDigest":
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            }),
        },
    ];

    for row in &rows {
        let case = row.case;
        let body = refused(
            &router,
            case,
            "BatchGetImage",
            &json!({"repositoryName": row.repository, "imageIds": [row.identifier]}),
            "RepositoryNotFoundException",
        )
        .await;
        assert_eq!(
            body["message"].as_str().unwrap_or_default(),
            format!(
                "The repository with name '{}' does not exist in the registry with id \
                 '{REGISTRY_ID}'",
                row.repository
            ),
            "case {case:?}: expected the registry's absent-repository wording, got {body:?}"
        );
        let described = call(
            &router,
            "DescribeRepositories",
            &json!({"repositoryNames": [row.repository]}),
        )
        .await;
        assert_eq!(
            described.error_code().rsplit('#').next().unwrap_or_default(),
            "RepositoryNotFoundException",
            "case {case:?}: expected the refused read to create no repository, got {:?}",
            described.body
        );
    }
}

/// One row of the unreachable-upstream table: the credential reference the rule stores that
/// no upstream credential can be read from.
struct Unreachable<'a> {
    case: &'a str,
    prefix: &'a str,
    credential_arn: Option<&'a str>,
    custom_role_arn: Option<&'a str>,
}

#[tokio::test]
async fn a_read_a_rule_covers_whose_upstream_cannot_be_reached_is_refused_upstream() {
    let rows = vec![
        Unreachable {
            case: "a customRoleArn that is not an ARN at all",
            prefix: "role-plain",
            credential_arn: None,
            custom_role_arn: Some("ecr-pull-through-cache"),
        },
        Unreachable {
            case: "a customRoleArn naming an IAM resource that is no role",
            prefix: "role-type",
            credential_arn: Some(SECRET),
            custom_role_arn: Some("arn:aws:iam::000000000000:user/ecr-pull-through-cache"),
        },
        Unreachable {
            case: "a customRoleArn naming another service's resource",
            prefix: "role-vendor",
            credential_arn: None,
            custom_role_arn: Some("arn:aws:s3:::ecr-pull-through-cache"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let router = emulator();
        given_rule(
            &router,
            &Row {
                case,
                prefix: row.prefix,
                upstream_registry: Some("docker-hub"),
                upstream_registry_url: "registry-1.docker.io",
                credential_arn: row.credential_arn,
                custom_role_arn: row.custom_role_arn,
                upstream_repository_prefix: None,
                path: "library/busybox",
                tag: DEFAULT_TAG,
            },
        )
        .await;
        let repository = format!("{}/library/busybox", row.prefix);

        let body = refused(
            &router,
            case,
            "BatchGetImage",
            &json!({"repositoryName": repository, "imageIds": [{"imageTag": DEFAULT_TAG}]}),
            "UnableToGetUpstreamImageException",
        )
        .await;
        let message = body["message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            "The image or images were unable to be pulled using the pull through cache rule. \
             This is usually caused because of an issue with the Secrets Manager secret \
             containing the credentials for the upstream registry.",
            "case {case:?}: expected the error class's own wording, got {message:?}"
        );

        // The registry's own verdict on the same rule agrees, and nothing was cached.
        let validated = ok(
            &router,
            case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": row.prefix}),
        )
        .await;
        assert_eq!(
            at(&validated, "/isValid"),
            json!(false),
            "case {case:?}: expected the rule not to validate either, got {validated:?}"
        );
        let described = call(
            &router,
            "DescribeRepositories",
            &json!({"repositoryNames": [repository]}),
        )
        .await;
        assert_eq!(
            described.error_code().rsplit('#').next().unwrap_or_default(),
            "RepositoryNotFoundException",
            "case {case:?}: expected a refused import to create no repository, got {:?}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_docker_pull_of_a_covered_reference_caches_it_the_same_way() {
    let router = emulator();
    let row = Row {
        case: "a docker pull through the cache",
        prefix: "dkr-mirror",
        upstream_registry: Some("ecr-public"),
        upstream_registry_url: "public.ecr.aws",
        credential_arn: None,
        custom_role_arn: None,
        upstream_repository_prefix: None,
        path: "docker/library/hello-world",
        tag: DEFAULT_TAG,
    };
    let case = row.case;
    given_rule(&router, &row).await;
    let repository = cached_repository(&row);
    let credential = given_credential(&router).await;
    let path = format!("/v2/{repository}/manifests/{}", row.tag);

    // A `HEAD` is a client asking whether the manifest is there: it transfers no image, so
    // it caches nothing.
    let probed = fetch(&router, "HEAD", &path, Some(&credential), None).await;
    assert_eq!(
        probed.status, 404,
        "case {case:?}: expected an existence probe to find nothing cached yet, got {} with \
         {:?}",
        probed.status, probed.body
    );
    let absent = call(
        &router,
        "DescribeRepositories",
        &json!({"repositoryNames": [repository.clone()]}),
    )
    .await;
    assert_eq!(
        absent.error_code().rsplit('#').next().unwrap_or_default(),
        "RepositoryNotFoundException",
        "case {case:?}: expected the probe to create no repository, got {:?}",
        absent.body
    );

    // The `GET` is the pull, so it is what caches.
    let pulled = fetch(&router, "GET", &path, Some(&credential), None).await;
    assert_eq!(
        pulled.status, 200,
        "case {case:?}: expected the pull to be served from the cache it fills, got {} with \
         {:?}",
        pulled.status, pulled.body
    );
    let document: Value = serde_json::from_str(&pulled.body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the served manifest to be JSON ({error}): {:?}",
            pulled.body
        )
    });
    assert_eq!(
        at(&document, "/schemaVersion"),
        json!(2),
        "case {case:?}: expected an image manifest, got {:?}",
        pulled.body
    );

    // Both planes report ONE cached image: the API read answers the document the registry
    // endpoint served, under the digest that endpoint stated.
    let answer = read_by_tag(&router, &repository, row.tag).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the API read to answer the cached image, got {} with {:?}",
        answer.status, answer.body
    );
    let image = single_image(case, &answer.body);
    assert_eq!(
        at(&image, "/imageManifest"),
        json!(pulled.body.clone()),
        "case {case:?}: expected the API to answer the document the registry endpoint \
         served, got {image:?}"
    );
    assert_eq!(
        at(&image, "/imageId/imageDigest"),
        json!(pulled.content_digest.clone()),
        "case {case:?}: expected the digest the registry endpoint stated, got {image:?}"
    );
}

/// The status a start ADMITS a basic scan in, and the one the clock carries it into before
/// it ends, spelled out here rather than imported from the crate so a wrong spelling in the
/// implementation fails this suite (smithy_query shape=ScanStatus).
const SCAN_PENDING: &str = "PENDING";
/// See [`SCAN_PENDING`].
const SCAN_IN_PROGRESS: &str = "IN_PROGRESS";
/// See [`SCAN_PENDING`].
const SCAN_COMPLETE: &str = "COMPLETE";

/// The `description` a run that examined the image and finished reports beside
/// [`SCAN_COMPLETE`], spelled out for the same reason as [`SCAN_PENDING`].
const SCAN_COMPLETE_DESCRIPTION: &str = "The scan was completed successfully.";

/// Whether a scan read has left the two states a run that has not ended reports.
fn has_ended(answer: &Value) -> bool {
    ![SCAN_PENDING, SCAN_IN_PROGRESS]
        .iter()
        .any(|status| at(answer, "/imageScanStatus/status") == json!(status))
}

/// A cached image is as complete as a pushed one: the registry holds the filesystem layers
/// its manifest declares, so a basic scan of it is ACCEPTED and settles at `COMPLETE` with
/// its findings block — the documented mirror-then-scan workflow.
///
/// Every row of [`rows`] is exercised, so the rule is asserted for each upstream registry,
/// prefix spelling, path depth and tag the import table spans; the scan is started by the
/// TAG that was mirrored, the way a client that pulled a tag through the cache scans it.
#[tokio::test]
async fn a_basic_scan_of_a_cached_image_is_accepted_and_completes() {
    let router = emulator();
    // Every row is cached and started before any is read, so the runs the clock owes
    // proceed together rather than one row at a time.
    let mut started: Vec<(&str, String, Value)> = Vec::new();
    for row in rows() {
        let case = row.case;
        given_rule(&router, &row).await;
        let repository = cached_repository(&row);

        let answer = read_by_tag(&router, &repository, row.tag).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the covered read to cache the image, got {} with {:?}",
            answer.status, answer.body
        );
        let image = single_image(case, &answer.body);
        let manifest = at(&image, "/imageManifest")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let document: Value = serde_json::from_str(&manifest).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the cached manifest to be JSON ({error}): {manifest}")
        });

        // The cached image carries the filesystem layers of the image that was cached, and
        // the repository holds every one of them: available for download under the digest
        // and the size the manifest states, and served byte for byte.
        let layers = document["layers"].as_array().cloned().unwrap_or_default();
        assert!(
            !layers.is_empty(),
            "case {case:?}: expected the cached manifest to declare the image's filesystem \
             layers, got {manifest}"
        );
        for (index, descriptor) in layers.iter().enumerate() {
            let digest = descriptor["digest"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let availability = ok(
                &router,
                case,
                "BatchCheckLayerAvailability",
                &json!({"repositoryName": repository, "layerDigests": [digest.clone()]}),
            )
            .await;
            assert_eq!(
                at(&availability, "/layers/0/layerAvailability"),
                json!("AVAILABLE"),
                "case {case:?}: expected the repository to hold layer {index} of the cached \
                 image ({digest}), got {availability:?}"
            );
            assert_eq!(
                at(&availability, "/layers/0/layerSize"),
                descriptor["size"],
                "case {case:?}: expected layer {index}'s stored size to be the one the \
                 manifest states, got {availability:?}"
            );
            let handed_out = ok(
                &router,
                case,
                "GetDownloadUrlForLayer",
                &json!({"repositoryName": repository, "layerDigest": digest.clone()}),
            )
            .await;
            let url = at(&handed_out, "/downloadUrl")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let (status, bytes) = fetch_url(&router, case, &url).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected layer {index} of the cached image to be served, got \
                 HTTP {status}"
            );
            assert_eq!(
                layer_digest(&bytes),
                digest,
                "case {case:?}: expected the bytes served for layer {index} to be the content \
                 its digest addresses, got {} bytes",
                bytes.len()
            );
        }

        let image_id = json!({"imageTag": row.tag});
        let scan = ok(
            &router,
            case,
            "StartImageScan",
            &json!({"repositoryName": repository, "imageId": image_id}),
        )
        .await;
        assert_eq!(
            at(&scan, "/imageScanStatus/status"),
            json!(SCAN_PENDING),
            "case {case:?}: expected the cached image's scan to be admitted queued, got {scan:?}"
        );
        assert_eq!(
            at(&scan, "/repositoryName"),
            json!(repository.clone()),
            "case {case:?}: expected the start to report the cache repository, got {scan:?}"
        );
        assert_eq!(
            at(&scan, "/imageId/imageTag"),
            json!(row.tag),
            "case {case:?}: expected the start to report the tag it was asked for, got {scan:?}"
        );
        started.push((case, repository, image_id));
    }

    for (case, repository, image_id) in started {
        // The terminal status is the clock's to owe, so this looks again until the run has
        // ended rather than waiting out a span of wall clock.
        let ended = ok_until(
            &router,
            case,
            "DescribeImageScanFindings",
            &json!({"repositoryName": repository, "imageId": image_id}),
            has_ended,
        )
        .await;
        assert_eq!(
            at(&ended, "/imageScanStatus"),
            json!({"status": SCAN_COMPLETE, "description": SCAN_COMPLETE_DESCRIPTION}),
            "case {case:?}: expected a scan of the cached image to complete, got {:?}",
            at(&ended, "/imageScanStatus")
        );
        assert!(
            at(&ended, "/imageScanFindings/imageScanCompletedAt").is_number(),
            "case {case:?}: expected a completed run to report when it completed, got {:?}",
            at(&ended, "/imageScanFindings/imageScanCompletedAt")
        );
        let counts = at(&ended, "/imageScanFindings/findingSeverityCounts");
        assert!(
            counts
                .as_object()
                .is_some_and(|counts| counts.values().all(Value::is_i64)),
            "case {case:?}: expected an integer-valued severity map over the run's findings, \
             got {counts:?}"
        );
        // The counts are over the findings the run REPORTS, and a basic scan here consults
        // no vulnerability source, so the map is present and counts nothing.
        assert_eq!(
            counts,
            json!({}),
            "case {case:?}: expected the counts to be over the findings reported, got \
             {counts:?}"
        );
    }
}

/// One row of the unscannable-image table: a document that declares no filesystem layer,
/// and how the scan request addresses the image it was pushed as.
struct Unscannable<'a> {
    case: &'a str,
    repository: &'a str,
    /// The document pushed, which declares no filesystem layer.
    manifest: Value,
    /// The tag it is pushed under.
    tag: &'a str,
    /// Whether the scan request carries the image's digest, and whether it carries the tag
    /// — a member it leaves out is rendered `null` in the refusal.
    by_digest: bool,
    by_tag: bool,
}

/// An image a basic scan genuinely cannot examine is refused `UnsupportedImageTypeException`
/// in AWS's own sentence, which names the identifier the request sent, the repository it
/// named and the registry that holds it, and ENDS at "is of a type that cannot be scanned".
///
/// The rows differ along what declares no filesystem layer — a manifest list and an OCI
/// image index (both reference other images), a manifest whose layer list is empty, and a
/// document that is no image manifest at all — and along how the request addresses the
/// image, since the identifier is echoed into the sentence.
#[tokio::test]
async fn an_image_no_basic_scan_can_examine_is_refused_in_the_documented_words() {
    let child = format!("sha256:{}", "b".repeat(64));
    let rows = vec![
        Unscannable {
            case: "a docker manifest list, addressed by its tag",
            repository: "unscannable/manifest-list",
            manifest: json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.docker.distribution.manifest.list.v2+json",
                "manifests": [{
                    "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                    "size": 7,
                    "digest": child,
                    "platform": {"architecture": "amd64", "os": "linux"},
                }],
            }),
            tag: "latest",
            by_digest: false,
            by_tag: true,
        },
        Unscannable {
            case: "an OCI image index, addressed by its digest",
            repository: "unscannable/oci-index",
            manifest: json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "manifests": [{
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "size": 11,
                    "digest": child,
                }],
            }),
            tag: "v2",
            by_digest: true,
            by_tag: false,
        },
        Unscannable {
            case: "an image manifest whose layer list is empty, addressed by both",
            repository: "unscannable/no-layers",
            manifest: json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "layers": [],
            }),
            tag: "3.1",
            by_digest: true,
            by_tag: true,
        },
        Unscannable {
            case: "a document that is no image manifest at all, addressed by its tag",
            repository: "unscannable/not-a-manifest",
            manifest: json!({"schemaVersion": 2, "image": "neither one thing nor the other"}),
            tag: "latest",
            by_digest: false,
            by_tag: true,
        },
    ];

    let router = emulator();
    for row in &rows {
        let case = row.case;
        ok(
            &router,
            case,
            "CreateRepository",
            &json!({"repositoryName": row.repository}),
        )
        .await;
        let manifest = serde_json::to_string(&row.manifest).expect("the test manifest serialises");
        ok(
            &router,
            case,
            "PutImage",
            &json!({
                "repositoryName": row.repository,
                "imageManifest": manifest.clone(),
                "imageTag": row.tag,
            }),
        )
        .await;
        let digest = layer_digest(manifest.as_bytes());

        let mut image_id = json!({});
        if row.by_digest {
            image_id["imageDigest"] = json!(digest.clone());
        }
        if row.by_tag {
            image_id["imageTag"] = json!(row.tag);
        }
        let body = refused(
            &router,
            case,
            "StartImageScan",
            &json!({"repositoryName": row.repository, "imageId": image_id}),
            "UnsupportedImageTypeException",
        )
        .await;
        // The identifier is rendered with BOTH its members, an absent one as `null`, as
        // every refusal of this registry renders the one the request sent.
        let expected = format!(
            "The image with 'imageId {{imageDigest:'{}', imageTag:'{}'}}' in the repository \
             with name '{}' in the registry with id '{REGISTRY_ID}' is of a type that cannot \
             be scanned",
            if row.by_digest { digest.as_str() } else { "null" },
            if row.by_tag { row.tag } else { "null" },
            row.repository,
        );
        assert_eq!(
            body["message"].as_str().unwrap_or_default(),
            expected,
            "case {case:?}: expected the error class's own sentence, got {body:?}"
        );
    }
}
