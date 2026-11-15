// SPDX-License-Identifier: CC0-1.0

//! The URI Amazon ECR reports for a repository, end to end over the wire.
//!
//! The rule under test: **`Repository$repositoryUri` names the registry that HOLDS the
//! repository — the account id of that registry, `dkr`, `ecr`, the Region that registry
//! serves and the DNS suffix of that Region's partition — followed by `/` and the
//! repository's own name.** The model writes that registry URL form down on
//! `AuthorizationData$proxyEndpoint` ("The Amazon ECR registry URL format is
//! `https://aws_account_id.dkr.ecr.region.amazonaws.com`"), and the account id and Region in
//! it are what the value is FOR: a client derives from them the registry it authenticates
//! to, the `image` of an ECS task definition, and whether an image reference belongs to its
//! own account and Region.
//!
//! An Amazon ECR registry is one account's IN ONE REGION, so the Region in the URI is the
//! repository's own — the Region its create was addressed to, which is also the Region that
//! keys the stored record. The class the rule generalises over is therefore the Region
//! crossed with the repository name, and the rows below span Regions of five partitions
//! (each with the DNS suffix that partition publishes) crossed with the name forms
//! `RepositoryName`'s pattern allows, read back through every operation that reports a
//! `Repository`.
//!
//! The last two tests are the invariants that make the value more than a well-shaped
//! string: two repositories of the same name in different Regions are reported with
//! different URIs (so the Region is read from the repository, not from one constant), and a
//! container client that splits the reported URI and pulls at it reaches the image that
//! repository holds (so the URI round-trips back into this registry).

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{at, emulator, given_layer_in_region, ok_in_region, REGISTRY_ID};

/// The media type a Docker v2 manifest is pushed and served under.
const DOCKER_V2_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of the config blob a Docker v2 manifest references.
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// One Region a repository can be created in, and the DNS suffix the registries of that
/// Region's partition are served under.
///
/// Source: the `dnsSuffix` each partition publishes in the AWS SDK partition metadata
/// (`endpoints.json`), which every regional endpoint of the partition is built from — `aws`
/// and `aws-us-gov` `amazonaws.com`, `aws-cn` `amazonaws.com.cn`, `aws-iso` `c2s.ic.gov`,
/// `aws-eusc` `amazonaws.eu`.
/// The rows are deliberately not only the commercial partition: the suffix is a property of
/// the Region, not a constant, and none of these Regions is the one the recorded case used.
const REGISTRY_REGIONS: &[(&str, &str)] = &[
    ("us-west-2", "amazonaws.com"),
    ("ap-southeast-2", "amazonaws.com"),
    ("us-gov-east-1", "amazonaws.com"),
    ("cn-north-1", "amazonaws.com.cn"),
    ("us-iso-east-1", "c2s.ic.gov"),
    ("eusc-de-east-1", "amazonaws.eu"),
];

/// The repository names these cases span, which are the forms `RepositoryName`'s pattern
/// allows — `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`
/// with `@length(min: 2, max: 256)`:
///
/// ```text
/// smithy_query(query="shape",shape="Repository",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: &[&str] = &["app", "ab", "team/app", "org/team/app", "a.b_c--d"];

/// The URI the rule says is reported for the repository `name` of this account's registry
/// in `region`, whose partition serves its registries under `dns_suffix`.
fn expected_uri(region: &str, dns_suffix: &str, name: &str) -> String {
    format!("{REGISTRY_ID}.dkr.ecr.{region}.{dns_suffix}/{name}")
}

/// The `repositoryUri` of the `Repository` at `pointer` in `body`, or a panic naming the
/// case and the body that carried none.
fn reported_uri(case: &str, body: &Value, pointer: &str) -> String {
    at(body, pointer)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a repositoryUri at {pointer}, got {body}"))
        .to_string()
}

#[tokio::test]
async fn every_operation_reports_the_repository_uri_of_the_registry_that_holds_it() {
    let router = emulator();
    for (region, dns_suffix) in REGISTRY_REGIONS {
        for name in REPOSITORY_NAMES {
            let case = format!("{region}, repository {name}");
            let expected = expected_uri(region, dns_suffix, name);

            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let created_uri = reported_uri(&case, &created, "/repository/repositoryUri");
            assert_eq!(
                created_uri, expected,
                "case {case:?}: expected CreateRepository to report {expected:?}, got \
                 {created_uri:?}"
            );

            // The authority's first label is the registry the same response reports, and
            // the path after the first '/' is exactly the repository's name: those are the
            // two parts a client parses the value for.
            let registry_id = at(&created, "/repository/registryId")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let (authority, path) = created_uri
                .split_once('/')
                .unwrap_or_else(|| panic!("case {case:?}: {created_uri:?} names no path"));
            assert_eq!(
                authority.split('.').next().unwrap_or_default(),
                registry_id,
                "case {case:?}: expected the first label of {authority:?} to be the \
                 registryId {registry_id:?}"
            );
            assert_eq!(
                path, *name,
                "case {case:?}: expected the path of {created_uri:?} to be {name:?}, got \
                 {path:?}"
            );

            let described = ok_in_region(
                &router,
                &case,
                region,
                "DescribeRepositories",
                &json!({"repositoryNames": [name]}),
            )
            .await;
            let described_uri =
                reported_uri(&case, &described, "/repositories/0/repositoryUri");
            assert_eq!(
                described_uri, expected,
                "case {case:?}: expected DescribeRepositories to report {expected:?}, got \
                 {described_uri:?}"
            );

            let deleted = ok_in_region(
                &router,
                &case,
                region,
                "DeleteRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let deleted_uri = reported_uri(&case, &deleted, "/repository/repositoryUri");
            assert_eq!(
                deleted_uri, expected,
                "case {case:?}: expected DeleteRepository to report {expected:?}, got \
                 {deleted_uri:?}"
            );
        }
    }
}

#[tokio::test]
async fn one_name_in_two_registries_is_reported_with_two_uris() {
    // The Region comes from the repository that was created, not from a constant: the same
    // name taken in each Region is reported with that Region's own registry host, and the
    // values are all distinct.
    let router = emulator();
    let name = "shared-name";
    let mut seen: Vec<String> = Vec::new();
    for (region, dns_suffix) in REGISTRY_REGIONS {
        let case = format!("{region}, repository {name}");
        let expected = expected_uri(region, dns_suffix, name);
        ok_in_region(
            &router,
            &case,
            region,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
        let described = ok_in_region(
            &router,
            &case,
            region,
            "DescribeRepositories",
            &json!({"repositoryNames": [name]}),
        )
        .await;
        let uri = reported_uri(&case, &described, "/repositories/0/repositoryUri");
        assert_eq!(
            uri, expected,
            "case {case:?}: expected the URI of the registry in {region:?}, got {uri:?}"
        );
        assert!(
            !seen.contains(&uri),
            "case {case:?}: expected a URI no other Region's registry reports, but \
             {uri:?} was already reported for {seen:?}"
        );
        seen.push(uri);
    }
}

#[tokio::test]
async fn a_pull_at_the_reported_uri_reaches_the_repository_it_names() {
    // The invariant the form is for: the reported URI round-trips back into this registry.
    // Each row pushes one image, then splits the URI into the host and the repository path
    // exactly as a container client does and pulls the manifest there with the credential
    // `GetAuthorizationToken` issued for that Region's registry.
    for (region, dns_suffix) in REGISTRY_REGIONS {
        let router = emulator();
        let name = "pulled/by-a-client";
        let case = format!("{region}, repository {name}");

        let created = ok_in_region(
            &router,
            &case,
            region,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
        let reported = reported_uri(&case, &created, "/repository/repositoryUri");
        assert_eq!(
            reported,
            expected_uri(region, dns_suffix, name),
            "case {case:?}: expected the URI of the registry in {region:?}, got {reported:?}"
        );
        let (reported_host, reported_path) = reported
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: {reported:?} names no repository path"));

        let config_bytes = b"reported-config".to_vec();
        let config = given_layer_in_region(&router, &case, region, name, &config_bytes).await;
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
        ok_in_region(
            &router,
            &case,
            region,
            "PutImage",
            &json!({
                "repositoryName": name,
                "imageManifest": manifest,
                "imageTag": "v1",
            }),
        )
        .await;

        // `docker login` presents the issued token as a Basic credential, which is what
        // the token decodes to (`AWS:<password>`).
        let issued =
            ok_in_region(&router, &case, region, "GetAuthorizationToken", &json!({})).await;
        let token = at(&issued, "/authorizationData/0/authorizationToken")
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an authorizationToken, got {issued}"))
            .to_string();

        let pulled = pull_manifest(&router, reported_host, reported_path, &token).await;
        assert_eq!(
            pulled.0, 200,
            "case {case:?}: a pull at the reported URI {reported:?} must reach the \
             repository it names, got HTTP {} with {:?}",
            pulled.0, pulled.1
        );
        assert_eq!(
            pulled.1, manifest,
            "case {case:?}: expected the pushed manifest from the reported URI, got {:?}",
            pulled.1
        );
    }
}

/// `GET /v2/<repository>/manifests/v1` at `host`, with `token` presented the way a
/// container client presents a `docker login` credential — the HTTP status and the body.
async fn pull_manifest(
    router: &MultiServiceRouter,
    host: &str,
    repository: &str,
    token: &str,
) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("/v2/{repository}/manifests/v1"))
        .header("host", host)
        .header("authorization", format!("Basic {token}"))
        .header("accept", DOCKER_V2_MANIFEST)
        .body(Bytes::new())
        .expect("the pull request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}
