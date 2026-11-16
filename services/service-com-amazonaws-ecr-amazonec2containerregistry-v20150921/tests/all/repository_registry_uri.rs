// SPDX-License-Identifier: CC0-1.0

//! The URI Amazon ECR reports for a repository, end to end over the wire.
//!
//! The rule under test: **`Repository$repositoryUri` is the ADDRESS a client pushes the
//! repository to and pulls it from — the endpoint this emulator serves that registry at,
//! followed by `/` and the repository's own name.** The member's own doc says what the value
//! is for ("The URI for the repository. You can use this URI for container image `push` and
//! `pull` operations"), so it is dialled rather than read: AWS's own
//! `<account>.dkr.ecr.<region>.amazonaws.com` host resolves to nothing on the machine this
//! emulator runs on, and a URI naming it would send every push and pull out of the emulator.
//! What a client reads such a URI FOR is reported beside it on the same response — the owning
//! account in `Repository$registryId` and the Region in `Repository$repositoryArn`.
//!
//! One endpoint serves every Region's registry here, so the class the rule generalises over
//! is the Region the repository was created in crossed with the repository name: the rows
//! below span Regions of five partitions crossed with the name forms `RepositoryName`'s
//! pattern allows, read back through every operation that reports a `Repository`.
//!
//! The last two tests are the invariants that make the value more than a well-shaped string:
//! two repositories of the same name in two Regions are reported at the one address this
//! emulator serves both registries at while their ARNs still name their own Regions, and a
//! container client that splits the reported URI and pulls at it reaches the image that
//! repository holds.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, emulator, given_layer_in_region, ok_in_region, repository_uri, REGISTRY_ID,
};

/// The media type a Docker v2 manifest is pushed and served under.
const DOCKER_V2_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of the config blob a Docker v2 manifest references.
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// One Region a repository can be created in.
///
/// Source: the Regions the `dnsSuffix` of five partitions in the AWS SDK partition metadata
/// (`endpoints.json`) covers — `aws`, `aws-us-gov`, `aws-cn`, `aws-iso` and `aws-eusc`. The
/// rows are deliberately not only the commercial partition, and none of them is the Region
/// the recorded case used: the Region keys the stored record and names its ARN, so a URI that
/// quietly took it from one constant would pass a one-Region table.
const REGISTRY_REGIONS: &[&str] = &[
    "us-west-2",
    "ap-southeast-2",
    "us-gov-east-1",
    "cn-north-1",
    "us-iso-east-1",
    "eusc-de-east-1",
];

/// The repository names these cases span, which are the forms `RepositoryName`'s pattern
/// allows — `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`
/// with `@length(min: 2, max: 256)`:
///
/// ```text
/// smithy_query(query="shape",shape="Repository",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: &[&str] = &["app", "ab", "team/app", "org/team/app", "a.b_c--d"];

/// The `repositoryUri` of the `Repository` at `pointer` in `body`, or a panic naming the
/// case and the body that carried none.
fn reported_uri(case: &str, body: &Value, pointer: &str) -> String {
    at(body, pointer)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a repositoryUri at {pointer}, got {body}"))
        .to_string()
}

#[tokio::test]
async fn every_operation_reports_the_address_the_repository_is_pushed_and_pulled_at() {
    let router = emulator();
    for region in REGISTRY_REGIONS {
        for name in REPOSITORY_NAMES {
            let case = format!("{region}, repository {name}");
            let expected = repository_uri(name);

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

            // The two parts a container client splits the value into: the address it dials,
            // which is never an AWS host this emulator does not answer on, and the path
            // after the first '/', which is exactly the repository's name.
            let (authority, path) = created_uri
                .split_once('/')
                .unwrap_or_else(|| panic!("case {case:?}: {created_uri:?} names no path"));
            assert!(
                !authority.contains(".amazonaws.com"),
                "case {case:?}: the authority {authority:?} of {created_uri:?} is an AWS host \
                 this emulator does not answer on"
            );
            assert_eq!(
                path, *name,
                "case {case:?}: expected the path of {created_uri:?} to be {name:?}, got \
                 {path:?}"
            );

            // What a client reads instead of parsing the address: the owning account and the
            // Region, on the very response that carries the URI.
            let registry_id = at(&created, "/repository/registryId")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                registry_id, REGISTRY_ID,
                "case {case:?}: expected the registryId of this account, got {registry_id:?}"
            );
            let arn = at(&created, "/repository/repositoryArn")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                arn.starts_with("arn:") && arn.contains(&format!(":{region}:")),
                "case {case:?}: expected the ARN to name the Region {region:?}, got {arn:?}"
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
async fn one_name_in_two_registries_is_reported_at_the_one_address_with_its_own_arn() {
    // The URI is an address and the ARN is a label, so the two answer differently: one
    // endpoint serves every Region's registry here, and that is the address each Region's
    // repository is reported at, while each ARN still names the Region its own record is
    // keyed by.
    let router = emulator();
    let name = "shared-name";
    let expected = repository_uri(name);
    let mut arns: Vec<String> = Vec::new();
    for region in REGISTRY_REGIONS {
        let case = format!("{region}, repository {name}");
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
            "case {case:?}: expected the address this emulator serves the registry at, got \
             {uri:?}"
        );
        let arn = at(&described, "/repositories/0/repositoryArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            arn.contains(&format!(":{region}:")),
            "case {case:?}: expected the ARN to name {region:?}, got {arn:?}"
        );
        assert!(
            !arns.contains(&arn),
            "case {case:?}: expected an ARN no other Region's registry reports, but {arn:?} \
             was already reported for {arns:?}"
        );
        arns.push(arn);
    }
}

#[tokio::test]
async fn a_pull_at_the_reported_uri_reaches_the_repository_it_names() {
    // The invariant the form is for: the reported URI round-trips back into this registry.
    // Each row pushes one image, then splits the URI into the host and the repository path
    // exactly as a container client does and pulls the manifest there with the credential
    // `GetAuthorizationToken` issued for that Region's registry.
    for region in REGISTRY_REGIONS {
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
            repository_uri(name),
            "case {case:?}: expected the address this emulator serves the registry at, got \
             {reported:?}"
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
