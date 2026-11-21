// SPDX-License-Identifier: CC0-1.0

//! The registry's replication configuration, end to end over the wire.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.
//!
//! The rule under test is the registry's own: `DescribeRegistry` reports this registry's
//! id together with the replication configuration exactly as the last
//! `PutReplicationConfiguration` persisted it — an empty rule list for a registry nobody
//! has configured — and a put is refused when a destination names the source registry
//! itself, that is this account's registry in the Region the request was made against.
//!
//! A configuration is an instruction about CONTENT, so the suite also holds the registry to
//! what the rules it stores DO: an image pushed into a repository a rule covers is copied
//! into each destination registry that rule names — the same-named repository is created
//! there and holds the identical image, manifest bytes, referenced blobs and tags included
//! — while a repository no rule covers is copied nowhere and stays absent there.
//!
//! The last two suites are about WHICH VALUES a rule's repository filter may carry: the
//! class ECR holds that value to is the expression its own validator publishes, which
//! admits the `*` wildcard and states the value's length as a look-ahead, so a value inside
//! that class is stored and reported back and one outside it is refused quoting the very
//! expression it failed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, call_in_region, call_until_in_region, emulator, fetch_url, given_layer_in_region,
    layer_digest, ok_in_region, REGION, REGISTRY_ID,
};

/// Another account's registry: a well-formed `RegistryId` (pattern `^[0-9]{12}$`) that is
/// not this registry, so a destination naming it is cross-account replication.
const FOREIGN_REGISTRY_ID: &str = "111122223333";

/// The modelled ceiling on `ReplicationConfiguration.rules`:
/// `ReplicationRuleList` is `@length(min: 0, max: 25)`
/// (`smithy_query(query="shape",shape="ReplicationRuleList")`).
const MAX_REPLICATION_RULES: usize = 25;

/// Region codes this suite replicates to, from `emulator_telemetry::policy::KNOWN_REGIONS`
/// ("every AWS region the wire may carry"), the catalog a replication destination's
/// Region must appear in. Enough of them to fill the modelled maximum rule count, one
/// Region per rule.
const DESTINATION_REGIONS: [&str; MAX_REPLICATION_RULES] = [
    "af-south-1",
    "ap-east-1",
    "ap-east-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-south-1",
    "ap-south-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-southeast-5",
    "ap-southeast-6",
    "ap-southeast-7",
    "ca-central-1",
    "ca-west-1",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "eu-west-2",
    "eu-west-3",
    "il-central-1",
];

/// The `replicationConfiguration` `DescribeRegistry` reports, having checked that the
/// registry it reports is this one.
async fn described_configuration(router: &MultiServiceRouter, case: &str, region: &str) -> Value {
    let body = ok_in_region(router, case, region, "DescribeRegistry", &json!({})).await;
    assert_eq!(
        body["registryId"],
        json!(REGISTRY_ID),
        "case {case:?}: DescribeRegistry must report this registry's id, got {body:?}"
    );
    body["replicationConfiguration"].clone()
}

/// One replication rule: the destinations it replicates to, and nothing else.
fn rule(destinations: Value) -> Value {
    json!({"destinations": destinations})
}

/// One destination: a registry id and the Region it is in.
fn destination(registry_id: &str, region: &str) -> Value {
    json!({"region": region, "registryId": registry_id})
}

/// `DescribeRegistry` reports whatever the last put persisted, whatever shape that
/// configuration has: no rules, one rule, several, filters or none, up to the modelled
/// maximum — and the report is stable across reads.
#[tokio::test]
async fn describe_registry_reports_the_configuration_each_put_persisted() {
    let router = emulator();
    let region = "eu-west-1";

    // Before any put: a registry nobody has configured has an EMPTY rule list, not an
    // absent configuration.
    assert_eq!(
        described_configuration(&router, "unconfigured registry", region).await,
        json!({"rules": []}),
        "an unconfigured registry reports an empty rule list"
    );

    // The rows differ along what the rule generalises over: the number of rules, the
    // number of destinations in a rule, whether a rule carries repository filters, and
    // whether a destination is in this account or another.
    let many_rules: Vec<Value> = DESTINATION_REGIONS
        .iter()
        .map(|region| rule(json!([destination(REGISTRY_ID, region)])))
        .collect();

    let cases: Vec<(&str, Value)> = vec![
        (
            "one rule, one destination in this account",
            json!([rule(json!([destination(REGISTRY_ID, "us-east-2")]))]),
        ),
        (
            "one rule, two destinations",
            json!([rule(json!([
                destination(REGISTRY_ID, "us-east-2"),
                destination(FOREIGN_REGISTRY_ID, "ap-south-1"),
            ]))]),
        ),
        (
            "a cross-account destination in the source Region",
            json!([rule(json!([destination(FOREIGN_REGISTRY_ID, "eu-west-1")]))]),
        ),
        (
            "a rule with a prefix filter",
            json!([{
                "destinations": [destination(REGISTRY_ID, "us-west-2")],
                "repositoryFilters": [{"filter": "team-a", "filterType": "PREFIX_MATCH"}],
            }]),
        ),
        (
            "two rules, one filtered and one not",
            json!([
                {
                    "destinations": [destination(REGISTRY_ID, "us-west-2")],
                    "repositoryFilters": [
                        {"filter": "team-a", "filterType": "PREFIX_MATCH"},
                        {"filter": "team-b/service", "filterType": "PREFIX_MATCH"},
                    ],
                },
                rule(json!([destination(FOREIGN_REGISTRY_ID, "sa-east-1")])),
            ]),
        ),
        (
            "the modelled maximum of 25 rules",
            Value::Array(many_rules),
        ),
        // "Creates or updates": a put carrying no rules replaces the configuration, so
        // this is how replication is turned off again.
        ("back to no rules", json!([])),
    ];

    for (case, rules) in cases {
        let expected = json!({"rules": rules});
        let put = ok_in_region(
            &router,
            case,
            region,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": expected}),
        )
        .await;
        assert_eq!(
            put["replicationConfiguration"], expected,
            "case {case:?}: the put must answer with the configuration it stored, got {put:?}"
        );

        let described = described_configuration(&router, case, region).await;
        assert_eq!(
            described, expected,
            "case {case:?}: DescribeRegistry must report the stored configuration, got \
             {described:?}"
        );

        // A report is not a mutation: reading twice reports the same thing.
        let again = described_configuration(&router, case, region).await;
        assert_eq!(
            again, described,
            "case {case:?}: a second DescribeRegistry must report the same configuration, \
             got {again:?}"
        );
    }
}

/// A destination is another registry: this account's registry in a different Region
/// (cross-Region), or another account's registry in any Region (cross-account). A
/// destination naming this account's registry in the Region the call was made against
/// names the source itself, and is refused.
#[tokio::test]
async fn a_destination_naming_the_source_registry_is_refused() {
    let router = emulator();

    // (case, the Region the call is made against, the rules, whether it is accepted)
    let cases: Vec<(&str, &str, Value, bool)> = vec![
        (
            "this account's registry in another Region is cross-Region replication",
            "eu-west-1",
            json!([rule(json!([destination(REGISTRY_ID, "us-east-2")]))]),
            true,
        ),
        (
            "another account's registry in the call's Region is cross-account replication",
            "eu-west-1",
            json!([rule(json!([destination(FOREIGN_REGISTRY_ID, "eu-west-1")]))]),
            true,
        ),
        (
            "this account's registry in the call's Region is the source itself",
            "eu-west-1",
            json!([rule(json!([destination(REGISTRY_ID, "eu-west-1")]))]),
            false,
        ),
        (
            "the source appears in the second of two rules",
            "eu-west-1",
            json!([
                rule(json!([destination(REGISTRY_ID, "us-east-2")])),
                rule(json!([destination(REGISTRY_ID, "eu-west-1")])),
            ]),
            false,
        ),
        (
            "the source appears beside another destination in one rule",
            "eu-west-1",
            json!([rule(json!([
                destination(FOREIGN_REGISTRY_ID, "ap-south-1"),
                destination(REGISTRY_ID, "eu-west-1"),
            ]))]),
            false,
        ),
        (
            "the same destination is the source when the call is made against it",
            "us-east-1",
            json!([rule(json!([destination(REGISTRY_ID, "us-east-1")]))]),
            false,
        ),
        (
            "and is a destination when the call is made against another Region",
            "ap-northeast-1",
            json!([rule(json!([destination(REGISTRY_ID, "us-east-1")]))]),
            true,
        ),
    ];

    // What the registry holds right now: only an accepted put changes it.
    let mut stored = json!({"rules": []});

    for (case, region, rules, accepted) in cases {
        let configuration = json!({"rules": rules});
        let answer = call_in_region(
            &router,
            region,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": configuration}),
        )
        .await;

        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                answer.status, answer.body
            );
            stored = configuration;
        } else {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "InvalidParameterException",
                "case {case:?}: expected InvalidParameterException, got {:?}",
                answer.body
            );
        }

        // Whatever the verdict, the stored configuration is the last ACCEPTED one: a
        // refused put persists nothing.
        let described = described_configuration(&router, case, region).await;
        assert_eq!(
            described, stored,
            "case {case:?}: expected the registry to still hold {stored:?}, got {described:?}"
        );
    }
}

/// The modelled bound on the rule list is enforced: 25 rules are accepted, 26 are
/// refused at the boundary, and the refusal stores nothing.
#[tokio::test]
async fn a_rule_list_past_the_modelled_maximum_is_refused() {
    let router = emulator();
    let region = "eu-west-1";

    let rules = |count: usize| -> Value {
        Value::Array(
            (0..count)
                .map(|index| {
                    // Cycling the catalog, so a count past the number of Regions listed
                    // still produces that many rules: the bound under test is on the
                    // rule COUNT, and two rules may name one destination.
                    let region = DESTINATION_REGIONS[index % DESTINATION_REGIONS.len()];
                    rule(json!([destination(REGISTRY_ID, region)]))
                })
                .collect(),
        )
    };

    // (case, how many rules, whether the modelled `@length(max: 25)` admits it)
    for (case, count, accepted) in [
        ("one under the maximum", MAX_REPLICATION_RULES - 1, true),
        ("exactly the maximum", MAX_REPLICATION_RULES, true),
        ("one past the maximum", MAX_REPLICATION_RULES + 1, false),
    ] {
        let configuration = json!({"rules": rules(count)});
        let answer = call_in_region(
            &router,
            region,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": configuration}),
        )
        .await;

        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200 for {count} rules, got {} with {:?}",
                answer.status, answer.body
            );
            let described = described_configuration(&router, case, region).await;
            assert_eq!(
                described, configuration,
                "case {case:?}: expected the {count} stored rules back, got {described:?}"
            );
        } else {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400 for {count} rules, got {} with {:?}",
                answer.status, answer.body
            );
            // A refusal against a constraint the model declares is stated with the
            // client-input error this service models — `InvalidParameterException`,
            // which this operation declares — naming the member and the bound it
            // failed. See `override_refusal` in `src/service.rs`.
            assert_eq!(
                answer.error_code(),
                "InvalidParameterException",
                "case {case:?}: expected InvalidParameterException for {count} rules, got {:?}",
                answer.body
            );
            // The refused put left the last accepted configuration in place.
            let described = described_configuration(&router, case, region).await;
            assert_eq!(
                described["rules"].as_array().map(Vec::len),
                Some(MAX_REPLICATION_RULES),
                "case {case:?}: a refused put must not change the stored rules, got \
                 {described:?}"
            );
        }
    }
}

/// The Region every push below is made against: the source registry's. A destination is
/// another Region's registry of this account, which is cross-Region replication.
const SOURCE_REGION: &str = REGION;

/// The Regions a copy is delivered into below, neither of them [`SOURCE_REGION`].
const FIRST_DESTINATION: &str = "us-east-2";
/// See [`FIRST_DESTINATION`].
const SECOND_DESTINATION: &str = "ap-south-1";

/// A Region no rule below names, so nothing is ever copied into its registry.
const UNNAMED_REGION: &str = "ca-central-1";

/// The media type of the manifest the pushes below carry: the Docker image manifest V2
/// schema 2, which is what a `docker push` of a single-platform image sends.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type a Docker image manifest V2 schema 2 gives its filesystem layers.
const LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// The media type that schema gives the image configuration blob its `config` descriptor
/// names.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// One image a case pushes into the source registry.
struct Image<'a> {
    /// What the case is about, named in every assertion it makes.
    case: &'a str,
    /// The repository it is pushed into, whose name a copy shares in the destination.
    repository: &'a str,
    /// Its filesystem layers, in the order the manifest lists them.
    layers: Vec<Vec<u8>>,
    /// Its image configuration blob, for a manifest that carries a `config` descriptor, or
    /// `None` for one that carries none.
    config: Option<Vec<u8>>,
    /// The tag the push carries, or `None` for an untagged push.
    tag: Option<&'a str>,
}

/// What one push left in the source registry: the manifest BYTES as they were pushed and
/// the digest they hash to, which is the image's identity in every registry that holds it.
struct Pushed {
    manifest: String,
    digest: String,
}

/// Store `rules` as the registry's whole replication configuration.
async fn given_configuration(router: &MultiServiceRouter, case: &str, rules: Value) {
    ok_in_region(
        router,
        case,
        SOURCE_REGION,
        "PutReplicationConfiguration",
        &json!({"replicationConfiguration": {"rules": rules}}),
    )
    .await;
}

/// One `PREFIX_MATCH` repository filter: "the repository name prefix to configure
/// replication for" (`RepositoryFilter$filter`).
fn prefix(filter: &str) -> Value {
    json!({"filter": filter, "filterType": "PREFIX_MATCH"})
}

/// Create `image`'s repository in the source registry and push the image into it, through
/// the documented push sequence: every blob first, then the manifest that references them.
async fn given_pushed_image(router: &MultiServiceRouter, image: &Image<'_>) -> Pushed {
    let case = image.case;
    ok_in_region(
        router,
        case,
        SOURCE_REGION,
        "CreateRepository",
        &json!({"repositoryName": image.repository}),
    )
    .await;
    let mut descriptors = Vec::new();
    for content in &image.layers {
        given_layer_in_region(router, case, SOURCE_REGION, image.repository, content).await;
        descriptors.push(json!({
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest(content),
            "size": content.len(),
        }));
    }
    let mut document = json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "layers": descriptors,
    });
    if let Some(config) = &image.config {
        given_layer_in_region(router, case, SOURCE_REGION, image.repository, config).await;
        document["config"] = json!({
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": layer_digest(config),
            "size": config.len(),
        });
    }
    let manifest = document.to_string();
    // Computed here, independently of the registry: a manifest is addressed by the digest
    // of its own bytes, so this is what both registries must report for the image.
    let digest = layer_digest(manifest.as_bytes());
    let mut request = json!({
        "repositoryName": image.repository,
        "imageManifest": manifest,
        "imageManifestMediaType": MANIFEST_MEDIA_TYPE,
    });
    if let Some(tag) = image.tag {
        request["imageTag"] = json!(tag);
    }
    let answer = ok_in_region(router, case, SOURCE_REGION, "PutImage", &request).await;
    assert_eq!(
        at(&answer, "/image/imageId/imageDigest"),
        json!(digest),
        "case {case:?}: the push must address the image by the digest of the manifest bytes \
         it stored, got {answer:?}"
    );
    Pushed { manifest, digest }
}

/// Assert that the registry of `destination` holds the copy of `image`: the same-named
/// repository, the identical manifest bytes under the source digest, the tags that pointed
/// at the source image, and every blob the manifest references with its bytes intact.
async fn assert_copy_landed(
    router: &MultiServiceRouter,
    image: &Image<'_>,
    pushed: &Pushed,
    destination: &str,
) {
    let case = image.case;
    // What a destination-Region client addresses the copy by: the tag the push carried, or
    // the digest for an untagged push.
    let image_id = match image.tag {
        Some(tag) => json!({"imageTag": tag}),
        None => json!({"imageDigest": pushed.digest}),
    };
    let read = json!({"repositoryName": image.repository, "imageIds": [image_id.clone()]});
    // A copy is made at the instant the clock owes it, so this waits for the CONDITION —
    // the destination read answering — rather than for a span of wall clock.
    let described = call_until_in_region(
        router,
        case,
        destination,
        "DescribeImages",
        &read,
        |answer| answer.status == 200,
    )
    .await;
    assert_eq!(
        at(&described.body, "/imageDetails/0/imageDigest"),
        json!(pushed.digest),
        "case {case:?}: the copy in {destination} must carry the source digest, got {:?}",
        described.body
    );
    assert_eq!(
        at(&described.body, "/imageDetails/0/repositoryName"),
        json!(image.repository),
        "case {case:?}: the copy is in the same-named repository, got {:?}",
        described.body
    );
    assert_eq!(
        at(&described.body, "/imageDetails/0/registryId"),
        json!(REGISTRY_ID),
        "case {case:?}: the destination registry is this account's, got {:?}",
        described.body
    );
    let expected_tags = match image.tag {
        Some(tag) => json!([tag]),
        // ECR omits `imageTags` for an untagged image rather than reporting [].
        None => Value::Null,
    };
    assert_eq!(
        at(&described.body, "/imageDetails/0/imageTags"),
        expected_tags,
        "case {case:?}: expected {expected_tags:?} on the copy in {destination}, got {:?}",
        described.body
    );

    // The destination holds the repository the replication created, in ITS own Region: a
    // repository's identity is its account, its Region and its name.
    let repositories = ok_in_region(
        router,
        case,
        destination,
        "DescribeRepositories",
        &json!({"repositoryNames": [image.repository]}),
    )
    .await;
    assert_eq!(
        at(&repositories, "/repositories/0/repositoryArn"),
        json!(format!(
            "arn:aws:ecr:{destination}:{REGISTRY_ID}:repository/{}",
            image.repository
        )),
        "case {case:?}: the created repository is the destination Region's, got \
         {repositories:?}"
    );

    // The manifest BYTES, which is what makes the copy the same image: a copy that
    // re-serialised the document would answer a different digest.
    let fetched = ok_in_region(
        router,
        case,
        destination,
        "BatchGetImage",
        &json!({"repositoryName": image.repository, "imageIds": [image_id]}),
    )
    .await;
    assert_eq!(
        at(&fetched, "/failures"),
        json!([]),
        "case {case:?}: the copy resolves in {destination}, got {fetched:?}"
    );
    assert_eq!(
        at(&fetched, "/images/0/imageManifest"),
        json!(pushed.manifest),
        "case {case:?}: expected the manifest bytes as pushed, got {fetched:?}"
    );
    assert_eq!(
        at(&fetched, "/images/0/imageId/imageDigest"),
        json!(pushed.digest),
        "case {case:?}: the manifest the copy serves hashes to the source digest, got \
         {fetched:?}"
    );

    // Every blob the manifest references — its layers and its image configuration — is in
    // the destination, and a download there serves the very bytes the source was pushed
    // with.
    for content in image.layers.iter().chain(image.config.iter()) {
        let digest = layer_digest(content);
        let answer = ok_in_region(
            router,
            case,
            destination,
            "GetDownloadUrlForLayer",
            &json!({"repositoryName": image.repository, "layerDigest": digest}),
        )
        .await;
        let url = answer["downloadUrl"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the answer in {destination} carries no downloadUrl: {answer:?}")
        });
        let (status, served) = fetch_url(router, case, url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the URL {destination} handed out must serve the layer, got \
             HTTP {status}"
        );
        assert_eq!(
            layer_digest(&served),
            digest,
            "case {case:?}: expected the bytes {digest} addresses, got {} bytes hashing to \
             {}",
            served.len(),
            layer_digest(&served)
        );
        assert_eq!(
            served.as_slice(),
            content.as_slice(),
            "case {case:?}: expected the {} bytes pushed at the source, got {}",
            content.len(),
            served.len()
        );
    }

    // A copy is not a push, so it starts no replication of its own: the copy is not copied
    // on from the destination.
    let statuses = ok_in_region(
        router,
        case,
        destination,
        "DescribeImageReplicationStatus",
        &json!({
            "repositoryName": image.repository,
            "imageId": {"imageDigest": pushed.digest},
        }),
    )
    .await;
    assert_eq!(
        at(&statuses, "/replicationStatuses"),
        json!([]),
        "case {case:?}: the copy in {destination} replicates nowhere, got {statuses:?}"
    );

    // Reading the destination again answers the same image: a read of a copy materialises
    // nothing of its own.
    let again = ok_in_region(router, case, destination, "DescribeImages", &read).await;
    assert_eq!(
        again, described.body,
        "case {case:?}: a second read in {destination} must answer the same image, got \
         {again:?}"
    );
}

/// Assert that the registry of `region` holds no repository named `repository`: the refusal
/// a read of a repository that was never created is answered with, word for word.
async fn assert_absent(router: &MultiServiceRouter, case: &str, region: &str, repository: &str) {
    for (operation, body) in [
        (
            "DescribeRepositories",
            json!({"repositoryNames": [repository]}),
        ),
        ("DescribeImages", json!({"repositoryName": repository})),
    ] {
        let answer = call_in_region(router, region, operation, &body).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 from {operation} in {region}, got {} with \
             {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "RepositoryNotFoundException",
            "case {case:?}: expected RepositoryNotFoundException from {operation} in \
             {region}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            format!(
                "The repository with name '{repository}' does not exist in the registry \
                 with id '{REGISTRY_ID}'"
            ),
            "case {case:?}: unexpected refusal wording from {operation} in {region}"
        );
    }
}

/// An image pushed into a repository a replication rule covers is copied into each
/// destination registry that rule names, and into no other.
///
/// The rows differ along what the rule generalises over: whether the rule selects the
/// repository by a prefix filter or by carrying no filter at all, how many destinations the
/// configuration names, how many blobs the image is made of and whether it carries an image
/// configuration blob, whether the push carries a tag, and whether the repository name is a
/// plain name or a namespaced one. The reported case is the first row.
#[tokio::test]
async fn an_image_pushed_under_a_rule_is_copied_into_each_destination_registry() {
    // (the image pushed, the rules in force at that push, the destinations it reaches)
    let rows: Vec<(Image<'_>, Value, Vec<&str>)> = vec![
        (
            Image {
                case: "a prefix filter covering the repository",
                repository: "replicated-prod",
                layers: vec![b"replicated-prod-layer".to_vec()],
                config: None,
                tag: Some("replicated"),
            },
            json!([{
                "destinations": [destination(REGISTRY_ID, FIRST_DESTINATION)],
                "repositoryFilters": [prefix("replicated-")],
            }]),
            vec![FIRST_DESTINATION],
        ),
        (
            Image {
                case: "a rule with no filters, which covers every repository",
                repository: "unfiltered",
                layers: vec![b"first-layer".to_vec(), b"second-layer".to_vec()],
                config: Some(br#"{"architecture":"arm64","os":"linux"}"#.to_vec()),
                tag: Some("v1.4.0"),
            },
            json!([rule(json!([destination(REGISTRY_ID, FIRST_DESTINATION)]))]),
            vec![FIRST_DESTINATION],
        ),
        (
            Image {
                case: "two destination Regions in one rule, and a namespaced repository",
                repository: "team-a/service",
                layers: vec![b"team-a-service-layer".to_vec()],
                config: None,
                tag: Some("latest"),
            },
            json!([{
                "destinations": [
                    destination(REGISTRY_ID, FIRST_DESTINATION),
                    destination(REGISTRY_ID, SECOND_DESTINATION),
                ],
                "repositoryFilters": [prefix("team-a/")],
            }]),
            vec![FIRST_DESTINATION, SECOND_DESTINATION],
        ),
        (
            Image {
                case: "an untagged push, addressed by its digest",
                repository: "untagged",
                layers: vec![b"untagged-layer".to_vec()],
                config: None,
                tag: None,
            },
            json!([rule(json!([destination(REGISTRY_ID, SECOND_DESTINATION)]))]),
            vec![SECOND_DESTINATION],
        ),
        (
            Image {
                case: "an image of no layers at all, which still references its config",
                repository: "configonly",
                layers: Vec::new(),
                config: Some(br#"{"architecture":"amd64","os":"linux"}"#.to_vec()),
                tag: Some("config"),
            },
            json!([rule(json!([destination(REGISTRY_ID, FIRST_DESTINATION)]))]),
            vec![FIRST_DESTINATION],
        ),
    ];

    for (image, rules, destinations) in rows {
        let router = emulator();
        given_configuration(&router, image.case, rules).await;
        let pushed = given_pushed_image(&router, &image).await;

        for destination in &destinations {
            assert_copy_landed(&router, &image, &pushed, destination).await;
        }

        // The copy went where the rule pointed and nowhere else: a Region no destination
        // names holds no repository of that name at all. Asserted after the copies above
        // landed, so this is the registry's decision rather than a copy not made yet.
        assert_absent(&router, image.case, UNNAMED_REGION, image.repository).await;

        // The source still holds the image it was pushed into: a copy is a copy.
        let source = ok_in_region(
            &router,
            image.case,
            SOURCE_REGION,
            "BatchGetImage",
            &json!({
                "repositoryName": image.repository,
                "imageIds": [{"imageDigest": pushed.digest}],
            }),
        )
        .await;
        assert_eq!(
            at(&source, "/images/0/imageManifest"),
            json!(pushed.manifest),
            "case {:?}: the source must still hold the image it replicated, got {source:?}",
            image.case
        );
    }
}

/// A repository no replication rule covers is copied nowhere, so the destination registry
/// refuses a read of it exactly as it refuses a repository that was never created — while
/// the repository a rule does cover is copied into that very registry.
#[tokio::test]
async fn a_repository_no_rule_covers_is_absent_in_the_destination_registry() {
    let case = "a prefix filter decides what is replicated";
    let router = emulator();
    given_configuration(
        &router,
        case,
        json!([{
            "destinations": [destination(REGISTRY_ID, FIRST_DESTINATION)],
            "repositoryFilters": [prefix("replicated-")],
        }]),
    )
    .await;

    let covered = Image {
        case,
        repository: "replicated-prod",
        layers: vec![b"covered-layer".to_vec()],
        config: None,
        tag: Some("replicated"),
    };
    let control = Image {
        case,
        repository: "kept-local",
        layers: vec![b"control-layer".to_vec()],
        config: None,
        tag: Some("replicated"),
    };
    let covered_push = given_pushed_image(&router, &covered).await;
    let control_push = given_pushed_image(&router, &control).await;

    assert_copy_landed(&router, &covered, &covered_push, FIRST_DESTINATION).await;
    // The control matched no filter, so the destination holds nothing of it — and the copy
    // above has already landed in that same registry, so this is not a copy still in flight.
    assert_absent(&router, case, FIRST_DESTINATION, control.repository).await;

    // The source registry holds both, the unreplicated one included.
    for (image, pushed) in [(&covered, &covered_push), (&control, &control_push)] {
        let source = ok_in_region(
            &router,
            case,
            SOURCE_REGION,
            "BatchGetImage",
            &json!({
                "repositoryName": image.repository,
                "imageIds": [{"imageDigest": pushed.digest}],
            }),
        )
        .await;
        assert_eq!(
            at(&source, "/images/0/imageManifest"),
            json!(pushed.manifest),
            "case {case:?}: the source must still hold {:?}, got {source:?}",
            image.repository
        );
    }
}

/// A copy lands in a destination repository the client created itself, rather than only in
/// one the replication created: the destination repository is addressed by its name, so an
/// existing one receives the image, keeps the settings it was created with, and keeps what
/// it already held.
#[tokio::test]
async fn a_copy_lands_in_a_destination_repository_the_client_already_created() {
    let case = "a destination repository the client created";
    let router = emulator();
    let repository = "shared";

    // The client's own repository in the destination Region, created with a setting the
    // replication would not have chosen, so the copy is visibly delivered INTO it.
    let created = ok_in_region(
        &router,
        case,
        FIRST_DESTINATION,
        "CreateRepository",
        &json!({
            "repositoryName": repository,
            "imageScanningConfiguration": {"scanOnPush": true},
        }),
    )
    .await;
    assert_eq!(
        at(&created, "/repository/imageScanningConfiguration/scanOnPush"),
        json!(true),
        "case {case:?}: the client's own destination repository is created as asked, got \
         {created:?}"
    );

    given_configuration(
        &router,
        case,
        json!([rule(json!([destination(REGISTRY_ID, FIRST_DESTINATION)]))]),
    )
    .await;
    let image = Image {
        case,
        repository,
        layers: vec![b"shared-layer".to_vec()],
        config: None,
        tag: Some("shared"),
    };
    let pushed = given_pushed_image(&router, &image).await;

    assert_copy_landed(&router, &image, &pushed, FIRST_DESTINATION).await;

    // The repository is the one the client created: the copy did not replace it, so it still
    // reports the configuration and the creation instant the create answered with.
    let described = ok_in_region(
        &router,
        case,
        FIRST_DESTINATION,
        "DescribeRepositories",
        &json!({"repositoryNames": [repository]}),
    )
    .await;
    assert_eq!(
        at(&described, "/repositories/0/imageScanningConfiguration/scanOnPush"),
        json!(true),
        "case {case:?}: the copy keeps the destination repository's own settings, got \
         {described:?}"
    );
    assert_eq!(
        at(&described, "/repositories/0/createdAt"),
        at(&created, "/repository/createdAt"),
        "case {case:?}: the copy does not re-create the destination repository, got \
         {described:?}"
    );
}

// ---------------------------------------------------------------------------
// The value class a repository filter carries
// ---------------------------------------------------------------------------

/// The expression ECR's own validator HOLDS for the value a replication repository filter
/// carries — the one it quotes back in the constraint clause of its refusal — written out
/// literally here, because the point of the two suites below is that the service accepts
/// and quotes exactly this rule rather than another.
///
/// It admits the `*` wildcard in every character class and states the length the value may
/// run to as a leading look-ahead. The model spells the same class narrower —
/// `smithy_query(query="shape", shape="RepositoryFilterValue")` gives
/// `^(?:[a-z0-9]+(?:[._-][a-z0-9]*)*/)*[a-z0-9]*(?:[._-][a-z0-9]*)*$` with
/// `@length(2..256)` beside it — and what a caller is held to is the service's own
/// expression.
const PUBLISHED_FILTER_EXPRESSION: &str =
    "^(?=.{1,256}$)(?:[a-z0-9*]+(?:[._-][a-z0-9*]*)*/)*[a-z0-9*]*(?:[._-][a-z0-9*]*)*$";

/// The longest value [`PUBLISHED_FILTER_EXPRESSION`]'s look-ahead admits (`.{1,256}`).
const MAX_FILTER_LENGTH: usize = 256;

/// A Region that is not the one these calls are made against, so a destination in it is a
/// real destination and the configuration is judged on its filters alone.
const FILTER_DESTINATION: &str = "us-east-2";

/// One rule replicating to [`FILTER_DESTINATION`] and carrying `filters` as its repository
/// filters, each of them a `PREFIX_MATCH` filter.
fn filtered_rule(filters: &[&str]) -> Value {
    json!({
        "destinations": [destination(REGISTRY_ID, FILTER_DESTINATION)],
        "repositoryFilters": filters.iter().map(|filter| prefix(filter)).collect::<Vec<Value>>(),
    })
}

/// The whole clause a filter outside the class is refused with: the path the validating
/// walk descends to the `filter`-th filter of the `rule`-th rule, 1-based element steps
/// included, and the expression the service holds that value class to.
fn filter_clause(rule: usize, filter: usize) -> String {
    format!(
        "Invalid parameter at \
         'replicationConfiguration.rules.{rule}.member.repositoryFilters.{filter}.member.filter' \
         failed to satisfy constraint: 'Member must satisfy regular expression pattern: \
         {PUBLISHED_FILTER_EXPRESSION}'"
    )
}

/// A repository filter the published class ADMITS is stored and reported back, the `*`
/// wildcard included — the character an operator covers a namespace with, which the
/// model's own narrower `@pattern` does not admit.
///
/// The rows differ along what the rule generalises over: where the wildcard sits (the
/// whole value, the name after a namespace, the namespace itself, inside a name, after a
/// separator, several of them at once), whether there is a wildcard at all, and the length
/// of the value — the shortest and the longest the published look-ahead admits.
#[tokio::test]
async fn a_repository_filter_the_published_class_admits_is_stored_and_reported() {
    let longest = "a".repeat(MAX_FILTER_LENGTH);
    // (case, the filter value the rule carries)
    let rows: Vec<(&str, String)> = vec![
        (
            "the reported value: a wildcard covering a namespace",
            "team/*".to_string(),
        ),
        (
            "a wildcard on its own, which covers every repository",
            "*".to_string(),
        ),
        (
            "a wildcard as the namespace segment",
            "*/service".to_string(),
        ),
        ("a wildcard inside a name", "team-*-service".to_string()),
        (
            "a wildcard after a separator inside a namespaced name",
            "team/app.*".to_string(),
        ),
        ("several wildcards in one value", "*/*-*".to_string()),
        (
            "no wildcard at all, the form the model admits too",
            "team-a".to_string(),
        ),
        (
            // The published look-ahead admits one character where the model's `@length`
            // states two: the service's own expression is the rule for this class, the
            // same reason the wildcard is accepted.
            "the shortest value the published length admits",
            "a".to_string(),
        ),
        (
            "the longest value the published length admits",
            longest.clone(),
        ),
    ];

    let router = emulator();
    for (case, filter) in rows {
        let configuration = json!({"rules": [filtered_rule(&[filter.as_str()])]});
        let put = ok_in_region(
            &router,
            case,
            REGION,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": configuration}),
        )
        .await;
        assert_eq!(
            put["replicationConfiguration"], configuration,
            "case {case:?}: the put must answer with the configuration it stored, got {put:?}"
        );

        let described = described_configuration(&router, case, REGION).await;
        assert_eq!(
            described, configuration,
            "case {case:?}: DescribeRegistry must report the stored configuration, got \
             {described:?}"
        );
    }
}

/// A repository filter the published class does NOT admit is refused HTTP 400
/// `InvalidParameterException`, in the clause every refused parameter of this service is
/// stated in, quoting the expression the value was held to and naming the position the
/// filter sits at — and nothing of the refused configuration is stored.
///
/// The rows differ along what the rule generalises over: which half of the class the value
/// broke (a character of neither class, an empty namespace segment, the empty value and one
/// character past the published maximum), where the filter sits (the only one, a later
/// filter of a rule, a later rule's filter), and how many filters are at fault, since every
/// one of them is stated.
#[tokio::test]
async fn a_repository_filter_outside_the_published_class_is_refused_quoting_it() {
    let too_long = "a".repeat(MAX_FILTER_LENGTH + 1);
    // (case, the rules the put carries, the message the refusal states)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "an upper-case name, which no character class admits",
            json!([filtered_rule(&["Team/App"])]),
            filter_clause(1, 1),
        ),
        (
            "a character outside both classes",
            json!([filtered_rule(&["team#app"])]),
            filter_clause(1, 1),
        ),
        (
            "a space inside the value",
            json!([filtered_rule(&["team app"])]),
            filter_clause(1, 1),
        ),
        (
            "an empty namespace segment",
            json!([filtered_rule(&["team//app"])]),
            filter_clause(1, 1),
        ),
        (
            "the empty value, which the published length refuses",
            json!([filtered_rule(&[""])]),
            filter_clause(1, 1),
        ),
        (
            "one character past the published maximum length",
            json!([filtered_rule(&[too_long.as_str()])]),
            filter_clause(1, 1),
        ),
        (
            "the second filter of the second rule, so a position is not a name",
            json!([
                filtered_rule(&["team/*"]),
                filtered_rule(&["team/*", "Team/App"]),
            ]),
            filter_clause(2, 2),
        ),
        (
            "two filters at fault, each one stated",
            json!([filtered_rule(&["Team/App", "team-ok", "ALSO/BAD"])]),
            format!("{}; {}", filter_clause(1, 1), filter_clause(1, 3)),
        ),
    ];

    let router = emulator();
    for (case, rules, expected) in rows {
        let answer = call_in_region(
            &router,
            REGION,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": {"rules": rules}}),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected the clause {expected:?}, got {:?}",
            answer.message()
        );

        // A refused put stores nothing: no row above was ever accepted, so the registry
        // still reports the empty rule list it started with.
        let described = described_configuration(&router, case, REGION).await;
        assert_eq!(
            described,
            json!({"rules": []}),
            "case {case:?}: a refused put must store nothing, got {described:?}"
        );
    }
}
