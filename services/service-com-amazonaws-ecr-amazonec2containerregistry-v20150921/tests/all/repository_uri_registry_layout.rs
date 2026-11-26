// SPDX-License-Identifier: CC0-1.0

//! The LAYOUT of the URI a repository is reported at, read off the response that carries it.
//!
//! The rule under test: **`Repository$repositoryUri` is the registry's own DNS authority
//! followed by the repository's name as its only path component — the owning account's id as
//! the first label, then the fixed labels `dkr` and `ecr`, then the Region of the registry
//! holding the repository, then the zone that registry is served in, and no port.** The
//! member says what the value is for ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations"), so a client splits it on the first `/`,
//! DIALS the authority and asks for the repository the path names — and reads the registry
//! that holds the repository out of those same leading labels, which is how a consumer
//! derives a task definition's `image`, a function's `ImageUri` or a push target from it, and
//! how a check that the image belongs to this account is made.
//!
//! The fixed part of the value is therefore the ORDER of those labels and the two literal
//! ones; what is variable comes from state and the request — the account id, the Region and
//! the repository's own name. The zone is this emulator's own rather than the one AWS serves
//! its registries in (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`): a
//! name there resolves at genuine Amazon ECR rather than at the machine this emulator runs
//! on, so a client handed one would leave the emulated environment carrying the credential
//! its `docker login` was issued.
//!
//! Nothing here composes the expected value from a constant: every assertion reads the
//! authority's labels and compares them against the identity the SAME response reports
//! (`registryId` and the Region its `repositoryArn` names), so a value that took its
//! identity, its label order or its path from anywhere else fails these rows. The class the
//! rule generalises over is the registry the repository is held in crossed with the
//! repository's name, so the rows span Regions of four partitions — none of them the Region
//! this crate's harness signs for by default — crossed with the name forms
//! `RepositoryName`'s pattern allows, through every operation that reports a `Repository`.
//!
//! Two invariants close it: the published authority reads BACK through this registry's own
//! reading of a registry host (`state::registry_pull::parse_registry_reference`) as naming
//! the very registry the response reports, so publishing and reading cannot drift; and a
//! container client that follows the value — dialling the authority it was handed with the
//! credential `GetAuthorizationToken` issued — reaches the image that repository holds.

use serde_json::{json, Value};

use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state as ecr_state;

use crate::wire::{at, emulator, given_layer_in_region, ok_in_region, registry_get};

/// The two fixed labels every Amazon ECR registry authority carries between the owning
/// account's id and the Region, as the registry URL format the model documents spells them:
/// "The Amazon ECR registry URL format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`"
///
/// ```text
/// smithy_query(query="doc",shape="AuthorizationData",member="proxyEndpoint",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
///
/// Spelled out here rather than imported so a value that spells them, or orders them, any
/// other way fails these rows.
const FIXED_LABELS: [&str; 2] = ["dkr", "ecr"];

/// The DNS suffixes AWS publishes its own endpoints under, spelled out here rather than
/// imported so a published zone slipping into one of them fails these rows.
///
/// Source: `service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, the tree's one
/// list of them.
const AWS_ENDPOINT_DNS_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// How many digits an account id — and so the first label of a registry authority — carries.
const ACCOUNT_ID_DIGITS: usize = 12;

/// The Regions the rows hold repositories in: one of four partitions, so a value that took
/// the Region from anywhere but the record, or wrote the partition's own DNS suffix as the
/// zone, shows up here. None is the Region this crate's harness signs for by default.
const HOLDING_REGIONS: [&str; 4] = ["us-west-2", "ap-southeast-2", "us-gov-east-1", "cn-north-1"];

/// The repository names the rows span, which are the forms `RepositoryName`'s pattern allows
/// — `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$` with
/// `@length(min: 2, max: 256)` — the shortest the pattern admits, a plain name, a namespaced
/// one, a twice-namespaced one, and one carrying the separators the pattern allows:
///
/// ```text
/// smithy_query(query="shape",shape="RepositoryName",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: [&str; 5] = ["ab", "app", "team/app", "org/team/app", "a.b_c--d"];

/// The media type a Docker v2 manifest is pushed and served under.
const DOCKER_V2_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of the config blob a Docker v2 manifest references.
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// The string at `pointer`, or a panic naming the case and the body that carried none.
fn text(case: &str, body: &Value, pointer: &str) -> String {
    at(body, pointer)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {pointer}, got {body}"))
        .to_string()
}

/// The Region an ECR repository ARN names: its fourth colon-separated field,
/// `arn:<partition>:ecr:<region>:<account>:repository/<name>`.
fn arn_region(case: &str, arn: &str) -> String {
    arn.split(':')
        .nth(3)
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| panic!("case {case:?}: {arn:?} names no Region"))
        .to_string()
}

/// Assert that the `Repository` at `pointer` in `body` reports `name` at the layout above,
/// and answer the authority it was published at.
///
/// The authority is read label by label and compared against the identity the SAME response
/// carries, so nothing of the expected value is composed here.
fn assert_registry_layout(case: &str, body: &Value, pointer: &str, name: &str) -> String {
    let uri = text(case, body, &format!("{pointer}/repositoryUri"));
    let registry_id = text(case, body, &format!("{pointer}/registryId"));
    let region = arn_region(case, &text(case, body, &format!("{pointer}/repositoryArn")));

    // A client reads the value by splitting it on the FIRST `/`: what is ahead of it is the
    // address it dials, and what follows is the repository it asks for.
    let (authority, path) = uri
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no repository path"));
    assert_eq!(
        path, name,
        "case {case:?}: expected the path of {uri:?} to be the repository name {name:?}, got \
         {path:?}"
    );
    // An authority with no port: the value AWS publishes carries none, and a container
    // client dials the registry on its own port.
    assert!(
        !authority.contains(':'),
        "case {case:?}: expected no port in the authority of {uri:?}, got {authority:?}"
    );

    // The four leading labels, in the one order the registry URL format states.
    let labels: Vec<&str> = authority.split('.').collect();
    let [dkr, ecr] = FIXED_LABELS;
    assert!(
        labels.len() >= 5,
        "case {case:?}: expected <account>.{dkr}.{ecr}.<region>.<zone> in {authority:?}, got \
         {} labels",
        labels.len()
    );
    assert_eq!(
        labels[0], registry_id,
        "case {case:?}: expected the first label of {authority:?} to be the registryId this \
         response reports, {registry_id:?}, got {:?}",
        labels[0]
    );
    assert!(
        registry_id.len() == ACCOUNT_ID_DIGITS
            && registry_id.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected a {ACCOUNT_ID_DIGITS}-digit account id as the first label of \
         {authority:?}, got {registry_id:?}"
    );
    assert_eq!(
        (labels[1], labels[2]),
        (dkr, ecr),
        "case {case:?}: expected the fixed labels {dkr:?} and {ecr:?} after the account in \
         {authority:?}, got {:?} and {:?}",
        labels[1],
        labels[2]
    );
    assert_eq!(
        labels[3], region,
        "case {case:?}: expected the Region of the registry holding the repository, {region:?}, \
         as the fourth label of {authority:?}, got {:?}",
        labels[3]
    );

    // The zone the registry is served in: at least one further non-empty label, and never
    // one AWS publishes its own endpoints in.
    let zone = labels[4..].join(".");
    assert!(
        labels[4..].iter().all(|label| !label.is_empty()),
        "case {case:?}: expected a served zone behind the Region in {authority:?}, got \
         {zone:?}"
    );
    assert!(
        !AWS_ENDPOINT_DNS_SUFFIXES
            .iter()
            .any(|suffix| zone == suffix.trim_start_matches('.') || zone.ends_with(suffix)),
        "case {case:?}: expected a zone this emulator answers in behind {region:?}, got \
         {zone:?}, which AWS publishes its own endpoints in"
    );

    // Publishing and reading are inverses: this registry's own reading of a registry host
    // takes the published authority back to the account and the Region the response named.
    let reference = ecr_state::registry_pull::parse_registry_reference(&uri);
    let (addressed_registry, read_back) = reference
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} reads back as no registry reference"));
    assert_eq!(
        (addressed_registry.as_str(), read_back.region.as_str()),
        (registry_id.as_str(), region.as_str()),
        "case {case:?}: expected {uri:?} to read back as this account's registry in \
         {region:?}, got {addressed_registry:?} in {:?}",
        read_back.region
    );
    assert_eq!(
        read_back.repository_name, name,
        "case {case:?}: expected {uri:?} to read back as the repository {name:?}, got {:?}",
        read_back.repository_name
    );

    authority.to_string()
}

/// RULE: every operation that reports a `Repository` publishes it at its registry's own DNS
/// authority — `<registryId>.dkr.ecr.<region>.<zone>` — with the repository's name as the
/// path.
#[tokio::test]
async fn every_reported_repository_uri_carries_the_registry_authority_layout() {
    for region in HOLDING_REGIONS {
        let router = emulator();
        for name in REPOSITORY_NAMES {
            let case = format!("{region}, repository {name}");

            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let published = assert_registry_layout(&case, &created, "/repository", name);

            // The read a client discovers the push target through — both the form that names
            // the repositories and the one that enumerates the registry's own.
            let described = ok_in_region(
                &router,
                &case,
                region,
                "DescribeRepositories",
                &json!({"repositoryNames": [name]}),
            )
            .await;
            let read = assert_registry_layout(&case, &described, "/repositories/0", name);
            assert_eq!(
                read, published,
                "case {case:?}: expected the read to publish the authority the create did, \
                 got {read:?}"
            );

            let page = ok_in_region(
                &router,
                &case,
                region,
                "DescribeRepositories",
                &json!({"repositoryNames": [name, name]}),
            )
            .await;
            // A name listed twice is answered twice, and each entry carries the same address.
            for index in 0..2 {
                let entry =
                    assert_registry_layout(&case, &page, &format!("/repositories/{index}"), name);
                assert_eq!(
                    entry, published,
                    "case {case:?}: expected entry {index} of the projected read to publish \
                     {published:?}, got {entry:?}"
                );
            }

            let deleted = ok_in_region(
                &router,
                &case,
                region,
                "DeleteRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let echoed = assert_registry_layout(&case, &deleted, "/repository", name);
            assert_eq!(
                echoed, published,
                "case {case:?}: expected the delete to echo the authority the create \
                 published, got {echoed:?}"
            );
        }
    }
}

/// RULE: the authority a repository is published at is one this registry's own Docker
/// Registry v2 plane answers — so a client that splits the value it was handed, dials that
/// authority with the credential `GetAuthorizationToken` issued and asks for the path reaches
/// the image the repository holds.
#[tokio::test]
async fn a_pull_at_the_published_registry_authority_reaches_the_image() {
    for region in HOLDING_REGIONS {
        let router = emulator();
        let name = "followed/by-a-client";
        let case = format!("{region}, repository {name}");

        let created = ok_in_region(
            &router,
            &case,
            region,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
        let authority = assert_registry_layout(&case, &created, "/repository", name);

        // One image, pushed through the documented sequence, so the pull answers what the
        // push stored rather than anything this test composed.
        let config_bytes = format!("config of {region}").into_bytes();
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

        // `docker login` presents the issued token as a Basic credential, which is what the
        // token decodes to (`AWS:<password>`).
        let issued =
            ok_in_region(&router, &case, region, "GetAuthorizationToken", &json!({})).await;
        let token = text(&case, &issued, "/authorizationData/0/authorizationToken");

        let (status, served) = registry_get(
            &router,
            &authority,
            &format!("/v2/{name}/manifests/v1"),
            &token,
            Some(DOCKER_V2_MANIFEST),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a pull at the published authority {authority:?} must reach this \
             registry, got HTTP {status} with {served:?}"
        );
        assert_eq!(
            served, manifest,
            "case {case:?}: expected the pushed manifest from {authority:?}, got {served:?}"
        );
    }
}
