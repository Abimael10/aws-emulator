// SPDX-License-Identifier: CC0-1.0

//! What the records a paginated ECR read serves contain, and in which order it serves
//! them. Two rules, each over a table of inputs:
//!
//! 1. `ImageDetail$imageSizeInBytes` is "the size, in bytes, of the image in the
//!    repository": the total over the DISTINCT blobs the image's manifest references of
//!    the byte count THIS REGISTRY recorded for each of them when its layer upload was
//!    completed. A descriptor's `size` is a claim made by whoever pushed the manifest, so
//!    it decides nothing where the registry has counted the bytes itself: two images over
//!    the same blobs report the same size however differently their manifests describe
//!    them, and neither reports the length of the manifest document.
//! 2. `DescribeRepositories` reports a registry's repositories in ascending order of the
//!    instant each was created — the repository created first comes first, which is the
//!    same rule `DescribeImages` reports a repository's images under (ascending push
//!    instant) — and pages over that same order, so a walk at any page size visits every
//!    repository exactly once in that order and a continuation page resumes strictly after
//!    the entry that closed the page before it.
//!
//! Every case goes through the router, so it covers routing, deserialization, the handler,
//! the persisted state and serialization — the same path a client takes.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state;

use crate::repository_image_lifecycle::{given_repository, manifest_digest};
use crate::wire::{at, call, emulator, emulator_with_engine, given_layer, ok};

/// The media type the OCI image-spec gives an image configuration blob.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";

/// The media type of a compressed filesystem layer.
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// An OCI image manifest's own media type.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// One descriptor a seeded manifest carries: the blob it names, and the `size` the
/// descriptor DECLARES for it (`None` for a descriptor that declares none at all).
type Descriptor<'a> = (&'a Blob, Option<i64>);

/// One blob this suite put into a repository: its digest, and the byte count the registry
/// recorded for it (which is simply how many bytes were uploaded).
struct Blob {
    digest: String,
    held: i64,
}

/// `len` bytes that no other blob of this suite equals — `name` is written at the front
/// and the rest padded — uploaded into `repository` through the documented layer-upload
/// sequence.
async fn given_blob(
    router: &smithy_server_runtime::MultiServiceRouter,
    repository: &str,
    name: &str,
    len: usize,
) -> Blob {
    assert!(
        name.len() <= len,
        "case {name:?}: a {len}-byte blob cannot carry its own name"
    );
    let mut content = name.as_bytes().to_vec();
    content.resize(len, b'.');
    Blob {
        digest: given_layer(router, name, repository, &content).await,
        held: i64::try_from(len).expect("a test blob fits in an i64"),
    }
}

/// Push a manifest whose `config` is `config` and whose `layers` are `layers`, each
/// descriptor declaring the size the row gave it, and answer the digest it is stored
/// under. `salt` keeps two rows over the same blobs from colliding on one digest.
async fn given_image(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    repository: &str,
    salt: &str,
    config: Option<Descriptor<'_>>,
    layers: &[Descriptor<'_>],
) -> String {
    let descriptor = |media_type: &str, (blob, declared): &Descriptor<'_>| -> Value {
        let mut descriptor = json!({"mediaType": media_type, "digest": blob.digest});
        if let Some(declared) = declared {
            descriptor["size"] = json!(declared);
        }
        descriptor
    };
    let mut document = json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "annotations": {"org.example.case": salt},
    });
    if let Some(config) = config.as_ref() {
        document["config"] = descriptor(CONFIG_MEDIA_TYPE, config);
    }
    document["layers"] = Value::Array(
        layers
            .iter()
            .map(|layer| descriptor(LAYER_MEDIA_TYPE, layer))
            .collect(),
    );
    let manifest = document.to_string();
    ok(
        router,
        case,
        "PutImage",
        &json!({"repositoryName": repository, "imageManifest": manifest}),
    )
    .await;
    manifest_digest(&manifest)
}

/// The `imageSizeInBytes` `DescribeImages` reports for one digest.
async fn reported_size(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    repository: &str,
    digest: &str,
) -> Value {
    let described = ok(
        router,
        case,
        "DescribeImages",
        &json!({"repositoryName": repository, "imageIds": [{"imageDigest": digest}]}),
    )
    .await;
    at(&described, "/imageDetails/0/imageSizeInBytes")
}

/// An image's reported size is the registry's own count of the blobs its manifest
/// references, whatever the manifest declares for them.
///
/// The rows differ along what the rule generalises over: whether a descriptor declares a
/// size at all, whether the size it declares is above or below the blob's real byte count,
/// how many blobs the manifest references, and whether one blob is named by more than one
/// descriptor.
#[tokio::test]
async fn an_image_s_size_is_the_registry_s_own_count_of_the_blobs_it_references() {
    let router = emulator();
    let repository = "counted";
    given_repository(&router, repository, json!({})).await;

    let config = given_blob(&router, repository, "config", 55).await;
    let layer = given_blob(&router, repository, "layer", 100).await;
    let second = given_blob(&router, repository, "second-layer", 20).await;
    let shared = given_blob(&router, repository, "shared", 64).await;

    // case, the config descriptor, the layer descriptors, the size the image must report
    let rows: Vec<(&str, Option<Descriptor<'_>>, Vec<Descriptor<'_>>, i64)> = vec![
        (
            "every descriptor declares the blob's real byte count",
            Some((&config, Some(config.held))),
            vec![(&layer, Some(layer.held))],
            155,
        ),
        (
            "a layer descriptor declares one byte more than the blob holds",
            Some((&config, Some(config.held))),
            vec![(&layer, Some(layer.held + 1))],
            155,
        ),
        (
            "a layer descriptor declares far fewer bytes than the blob holds",
            Some((&config, Some(config.held))),
            vec![(&layer, Some(7))],
            155,
        ),
        (
            "the config descriptor declares a size the blob does not have",
            Some((&config, Some(4096))),
            vec![(&layer, Some(layer.held))],
            155,
        ),
        (
            "no descriptor declares a size",
            Some((&config, None)),
            vec![(&layer, None)],
            155,
        ),
        (
            "three blobs, each declared wrongly",
            Some((&config, Some(1))),
            vec![(&layer, Some(2)), (&second, Some(3))],
            175,
        ),
        (
            // One blob, named twice: the repository holds it once, so it counts once.
            "one blob named as both the configuration and the only layer",
            Some((&shared, Some(999))),
            vec![(&shared, Some(1))],
            64,
        ),
        (
            "a manifest with no config descriptor at all",
            None,
            vec![(&layer, Some(0))],
            100,
        ),
    ];

    for (case, config_descriptor, layers, expected) in rows {
        let digest = given_image(
            &router,
            case,
            repository,
            case,
            config_descriptor,
            &layers,
        )
        .await;
        let reported = reported_size(&router, case, repository, &digest).await;
        assert_eq!(
            reported,
            json!(expected),
            "case {case:?}: expected imageSizeInBytes {expected}, got {reported:?}"
        );
    }
}

/// Two images over the same blobs report the same size, however differently their
/// manifests describe those blobs — and that size is not the length of either manifest
/// document.
#[tokio::test]
async fn images_over_the_same_blobs_report_the_same_size() {
    let router = emulator();
    let repository = "same-content";
    given_repository(&router, repository, json!({})).await;

    let config = given_blob(&router, repository, "config", 55).await;
    let layer = given_blob(&router, repository, "layer", 100).await;
    let content_total = config.held + layer.held;

    // Each row declares a different size for the very same layer blob, which is what
    // makes the three manifests three distinct images over one set of blobs.
    let mut digests = Vec::new();
    for (case, declared) in [
        ("the first image", layer.held),
        ("the second image", layer.held + 1),
        ("the third image", layer.held + 2),
    ] {
        digests.push((
            case,
            given_image(
                &router,
                case,
                repository,
                case,
                Some((&config, Some(config.held))),
                &[(&layer, Some(declared))],
            )
            .await,
        ));
    }
    let distinct: std::collections::BTreeSet<&String> =
        digests.iter().map(|(_, digest)| digest).collect();
    assert_eq!(
        distinct.len(),
        digests.len(),
        "the three manifests must be three distinct images, got {digests:?}"
    );

    for (case, digest) in &digests {
        let reported = reported_size(&router, case, repository, digest).await;
        assert_eq!(
            reported,
            json!(content_total),
            "case {case:?}: every image over these blobs reports {content_total}, \
             got {reported:?}"
        );
    }
}

/// Stamp the stored creation instant of one repository, so a case decides the creation
/// order rather than inheriting whatever instants the creates happened to land on (the
/// store seam `tests/all/paginated_read_scoping.rs` uses for an image's push instant).
fn given_creation_instant(
    engine: &engine::InMemoryEngine,
    repository: &str,
    created_at: SystemTime,
) {
    let state = state::service_state(engine).expect("the ecr store is reachable");
    state
        .lock()
        .repository_mut(crate::wire::REGION, repository)
        .expect("the repository was created over the wire")
        .created_at = created_at;
}

/// The repository names `DescribeRepositories` reported, in the order it reported them.
fn reported_names(body: &Value) -> Vec<String> {
    body["repositories"]
        .as_array()
        .unwrap_or_else(|| panic!("DescribeRepositories must return a list: {body:?}"))
        .iter()
        .map(|repository| {
            repository["repositoryName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// `DescribeRepositories` reports the repositories in the order they were created, and
/// pages over that same order.
///
/// The repositories are created in an order that is NOT their name order, so a read
/// ordered by the key the store holds them under would answer a different sequence, and
/// the instants are stamped a minute apart so the expected sequence owes nothing to how
/// fast the creates ran. The cases differ along what the rule generalises over: the page
/// size a walk takes the set at, and a walk whose page size CHANGES between pages.
#[tokio::test]
async fn describe_repositories_reports_and_pages_in_creation_order() {
    let (router, engine) = emulator_with_engine();
    // Created in this order; by name they sort alpha, bravo, mike, quebec, zulu.
    let created = ["zulu", "mike", "alpha", "quebec", "bravo"];
    // A fixed instant, so the stamped order is the one this case decided.
    let first = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for (position, name) in created.iter().enumerate() {
        given_repository(&router, name, json!({})).await;
        let position = u64::try_from(position).expect("a handful of repositories");
        given_creation_instant(&engine, name, first + Duration::from_secs(60 * position));
    }
    let expected: Vec<String> = created.iter().map(ToString::to_string).collect();

    let every = ok(
        &router,
        "every repository",
        "DescribeRepositories",
        &json!({}),
    )
    .await;
    assert_eq!(
        reported_names(&every),
        expected,
        "case \"every repository\": expected creation order {expected:?}, got {:?}",
        reported_names(&every)
    );

    // A walk at every page size visits every repository exactly once, in that order.
    for page_size in 1..=created.len() + 1 {
        let case = format!("maxResults {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({"maxResults": page_size});
            if let Some(token) = token.as_ref() {
                request["nextToken"] = json!(token);
            }
            let page = ok(&router, &case, "DescribeRepositories", &request).await;
            let reported = reported_names(&page);
            assert!(
                reported.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} entries, got {reported:?}"
            );
            walked.extend(reported);
            token = page["nextToken"].as_str().map(ToString::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, expected,
            "case {case:?}: paging must walk every repository once in creation order, \
             got {walked:?}"
        );
    }

    // A continuation page resumes strictly after the entry that closed the page before
    // it, whatever page size each call asks for: the token names a point in the order,
    // not a count of pages.
    let case = "a walk whose page size changes between pages";
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for page_size in [1usize, 2, 1, 3] {
        let mut request = json!({"maxResults": page_size});
        if let Some(token) = token.as_ref() {
            request["nextToken"] = json!(token);
        }
        let page = ok(&router, case, "DescribeRepositories", &request).await;
        walked.extend(reported_names(&page));
        token = page["nextToken"].as_str().map(ToString::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        token, None,
        "case {case:?}: 1 + 2 + 1 + 3 pages cover the whole set, so the last page \
         carries no token"
    );
    assert_eq!(
        walked, expected,
        "case {case:?}: every repository exactly once, in creation order, got {walked:?}"
    );

    // A token this registry did not mint is refused rather than served.
    let bad = call(
        &router,
        "DescribeRepositories",
        &json!({"nextToken": "not-a-token"}),
    )
    .await;
    assert_eq!(
        bad.error_code(),
        "InvalidParameterException",
        "case \"an unreadable token\": expected InvalidParameterException, got {:?}",
        bad.body
    );
}
