// SPDX-License-Identifier: CC0-1.0

//! The URI Amazon ECR reports for a repository, end to end over the wire.
//!
//! The rule under test: **`Repository$repositoryUri` names the REGISTRY that holds the
//! repository, followed by `/` and the repository's own name.** An Amazon ECR registry is
//! one account's in ONE Region (`Repository$repositoryArn` is documented as carrying "the
//! region of the repository, Amazon Web Services account ID of the repository owner,
//! repository namespace, and repository name"), so the URI's authority carries that
//! registry's own account id and Region in the layout `AuthorizationData$proxyEndpoint`
//! documents for a registry URL (`https://aws_account_id.dkr.ecr.region.amazonaws.com`):
//! the 12-digit owning account, the labels `dkr` and `ecr`, the Region, then the zone the
//! registry is served in. Two Regions are two registries, so one repository name taken in
//! two Regions is two repositories and never one URI.
//!
//! The member's own doc says what the value is for ("The URI for the repository. You can use
//! this URI for container image `push` and `pull` operations"), so it is DIALLED as well as
//! read: AWS's own `amazonaws.com` zone resolves to nothing on the machine this emulator
//! runs on, and a URI naming it would send every push and pull out of the emulator, so the
//! registry's labels are announced in the zone this emulator answers in — the loopback zone,
//! which RFC 6761 reserves together with every name under it for that interface, on the port
//! the emulator is served on (`crate::wire::ANNOUNCED_REGISTRY_ZONE`).
//!
//! The class the rule generalises over is the Region the repository was created in crossed
//! with the repository name: the rows below span Regions of five partitions crossed with the
//! name forms `RepositoryName`'s pattern allows, read back through every operation that
//! reports a `Repository`.
//!
//! The tests that follow are the invariants that make the value more than a well-shaped
//! string: two repositories of the same name in two Regions report two different URIs, each
//! naming its own Region exactly as its ARN does; a container client that splits the
//! reported URI and pulls at it reaches the image THAT Region's repository holds; and the
//! name-collision walk — a create refused in the Region that holds the name and accepted in
//! another — leaves the two registries addressable apart.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, call_in_region, emulator, given_layer_in_region, ok_in_region, repository_uri_in,
    REGISTRY_ID,
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
            let expected = repository_uri_in(region, name);

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
            // The registry the authority names: this account's, in the repository's own
            // Region, which is what a client recovers from the URI it was handed.
            assert!(
                authority.starts_with(&format!("{REGISTRY_ID}.dkr.ecr.{region}.")),
                "case {case:?}: expected the authority of {created_uri:?} to name the \
                 registry {REGISTRY_ID:?} in {region:?}, got {authority:?}"
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
async fn one_name_in_two_registries_is_reported_at_its_own_registrys_uri() {
    // One name in several Regions is several repositories, so each is reported with the URI
    // of the registry that holds it: the URIs differ in the Region their authority names,
    // exactly as the ARNs do, and no two of them are the same value.
    let router = emulator();
    let name = "shared-name";
    let mut arns: Vec<String> = Vec::new();
    let mut uris: Vec<String> = Vec::new();
    for region in REGISTRY_REGIONS {
        let case = format!("{region}, repository {name}");
        let expected = repository_uri_in(region, name);
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
            "case {case:?}: expected the URI of the registry that holds the repository, got \
             {uri:?}"
        );
        assert!(
            !uris.contains(&uri),
            "case {case:?}: expected a URI no other Region's registry reports, but {uri:?} \
             was already reported for {uris:?}"
        );
        uris.push(uri);
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
            repository_uri_in(region, name),
            "case {case:?}: expected the URI of the registry that holds the repository, got \
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

/// The error code a create for a name the addressed registry already holds is refused with
/// (`CreateRepository` models `RepositoryAlreadyExistsException`).
const ALREADY_EXISTS: &str = "RepositoryAlreadyExistsException";

/// One pair of Regions and the repository name taken in both of them, for the
/// name-collision walk below.
struct RegionPair {
    case: &'static str,
    /// The Region the name is taken in first.
    home: &'static str,
    /// A second Region, whose registry the same name is free in.
    other: &'static str,
    /// The repository name, in one of the forms [`REPOSITORY_NAMES`] is read from.
    name: &'static str,
}

/// The pairs the collision walk drives: two commercial Regions, two Regions of different
/// partitions, and a GovCloud Region beside a commercial one, each with a different name
/// form — the Region pair and the name are what the rule generalises over.
const REGION_PAIRS: &[RegionPair] = &[
    RegionPair {
        case: "two commercial Regions",
        home: "us-east-1",
        other: "us-west-2",
        name: "shared",
    },
    RegionPair {
        case: "two commercial Regions, a namespaced name",
        home: "eu-west-1",
        other: "ap-southeast-2",
        name: "team/app",
    },
    RegionPair {
        case: "two Regions of different partitions",
        home: "us-west-2",
        other: "cn-north-1",
        name: "ab",
    },
    RegionPair {
        case: "a GovCloud Region beside a commercial one",
        home: "us-gov-east-1",
        other: "us-east-1",
        name: "a.b_c--d",
    },
];

#[tokio::test]
async fn a_name_one_registry_holds_is_free_in_another_and_the_two_report_two_uris() {
    // The walk a client makes when it hits the collision: the name is taken in one Region,
    // the same create there is refused, and the very same create in ANOTHER Region
    // succeeds — because that is a second registry. What the two repositories must then
    // report is their own registry's URI, so a tool keying on the URI cannot mistake one
    // for the other.
    for pair in REGION_PAIRS {
        let router = emulator();
        let case = format!("{}, repository {}", pair.case, pair.name);

        let created = ok_in_region(
            &router,
            &case,
            pair.home,
            "CreateRepository",
            &json!({"repositoryName": pair.name, "imageTagMutability": "IMMUTABLE"}),
        )
        .await;
        let home_uri = reported_uri(&case, &created, "/repository/repositoryUri");
        let home_arn = at(&created, "/repository/repositoryArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            home_uri,
            repository_uri_in(pair.home, pair.name),
            "case {case:?}: expected the URI of the registry in {:?}, got {home_uri:?}",
            pair.home
        );

        // The same name again in the SAME registry is the collision, refused with the code
        // and the sentence AWS words it with.
        let refused = call_in_region(
            &router,
            pair.home,
            "CreateRepository",
            &json!({"repositoryName": pair.name}),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for a name this registry holds, got {} with \
             {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            ALREADY_EXISTS,
            "case {case:?}: expected {ALREADY_EXISTS}, got {:?}",
            refused.body
        );
        let expected_message = format!(
            "The repository with name '{}' already exists in the registry with id \
             '{REGISTRY_ID}'",
            pair.name
        );
        assert_eq!(
            refused.message(),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {:?}",
            refused.message()
        );

        // The refused create changed nothing the home Region reports.
        let described = ok_in_region(
            &router,
            &case,
            pair.home,
            "DescribeRepositories",
            &json!({"repositoryNames": [pair.name]}),
        )
        .await;
        let held = at(&described, "/repositories")
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            held, 1,
            "case {case:?}: expected exactly one repository of this name in {:?}, got \
             {described}",
            pair.home
        );
        assert_eq!(
            reported_uri(&case, &described, "/repositories/0/repositoryUri"),
            home_uri,
            "case {case:?}: expected the read to report the URI the create did, got \
             {described}"
        );
        assert_eq!(
            at(&described, "/repositories/0/imageTagMutability")
                .as_str()
                .unwrap_or_default(),
            "IMMUTABLE",
            "case {case:?}: expected the refused create to leave the stored mutability \
             alone, got {described}"
        );

        // The second Region's registry does not hold the name, so the same create succeeds
        // there and reports its own registry's URI and its own ARN.
        let elsewhere = ok_in_region(
            &router,
            &case,
            pair.other,
            "CreateRepository",
            &json!({"repositoryName": pair.name}),
        )
        .await;
        let other_uri = reported_uri(&case, &elsewhere, "/repository/repositoryUri");
        let other_arn = at(&elsewhere, "/repository/repositoryArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            other_uri,
            repository_uri_in(pair.other, pair.name),
            "case {case:?}: expected the URI of the registry in {:?}, got {other_uri:?}",
            pair.other
        );
        assert_ne!(
            other_arn, home_arn,
            "case {case:?}: expected two Regions' repositories to carry two ARNs, got \
             {other_arn:?} for both"
        );
        assert_ne!(
            other_uri, home_uri,
            "case {case:?}: expected two Regions' repositories to carry two URIs, got \
             {other_uri:?} for both"
        );

        // Both are this account's registries: it is the Region that tells them apart, and
        // each URI's authority names its own.
        for (region, uri) in [(pair.home, &home_uri), (pair.other, &other_uri)] {
            let authority = uri
                .split_once('/')
                .map(|(authority, _)| authority)
                .unwrap_or_default();
            assert!(
                authority.starts_with(&format!("{REGISTRY_ID}.dkr.ecr.{region}.")),
                "case {case:?}: expected the authority of {uri:?} to name the registry \
                 {REGISTRY_ID:?} in {region:?}, got {authority:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_pull_at_each_regions_uri_reaches_that_regions_own_image() {
    // What telling the two apart is FOR: with one name in two registries, each reported URI
    // is followed the way a container client follows it — split at the first '/', dialled,
    // and pulled from with the credential that Region's token carries — and answers the
    // manifest THAT Region's repository holds. The two manifests differ, so a pull that
    // reached the other registry answers the wrong document rather than nothing.
    for pair in REGION_PAIRS {
        let router = emulator();
        let case = format!("{}, repository {}", pair.case, pair.name);
        let mut announced: Vec<(&str, String, String)> = Vec::new();

        for region in [pair.home, pair.other] {
            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": pair.name}),
            )
            .await;
            let uri = reported_uri(&case, &created, "/repository/repositoryUri");
            let config_bytes = format!("config of {region}").into_bytes();
            let config =
                given_layer_in_region(&router, &case, region, pair.name, &config_bytes).await;
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
                    "repositoryName": pair.name,
                    "imageManifest": manifest,
                    "imageTag": "v1",
                }),
            )
            .await;
            announced.push((region, uri, manifest));
        }

        for (region, uri, manifest) in &announced {
            let issued =
                ok_in_region(&router, &case, region, "GetAuthorizationToken", &json!({})).await;
            let token = at(&issued, "/authorizationData/0/authorizationToken")
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected an authorizationToken, got {issued}")
                })
                .to_string();
            let proxy_endpoint = at(&issued, "/authorizationData/0/proxyEndpoint")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let (authority, path) = uri
                .split_once('/')
                .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no repository path"));
            // A client logs in where it pushes: the registry URL is the URI's own authority
            // behind a scheme.
            assert_eq!(
                proxy_endpoint,
                format!("https://{authority}"),
                "case {case:?}: expected the {region:?} login URL to name the registry \
                 {authority:?} the repository is pushed to, got {proxy_endpoint:?}"
            );

            let pulled = pull_manifest(&router, authority, path, &token).await;
            assert_eq!(
                pulled.0, 200,
                "case {case:?}: a pull at the {region:?} URI {uri:?} must reach that \
                 Region's repository, got HTTP {} with {:?}",
                pulled.0, pulled.1
            );
            assert_eq!(
                pulled.1, *manifest,
                "case {case:?}: expected the manifest pushed in {region:?} from {uri:?}, \
                 got {:?}",
                pulled.1
            );
        }
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
