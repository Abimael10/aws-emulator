// SPDX-License-Identifier: CC0-1.0

//! Which registry a repository's published address NAMES, read out of the address itself.
//!
//! The rule under test: **an Amazon ECR registry is one account's in ONE Region, so the
//! identity a repository reports is the `(account, Region, name)` triple — and
//! `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") carries that whole triple: the owning
//! account, the two fixed registry labels and the holding registry's Region as the labels of
//! the authority a client dials, then the repository's own name as the path. Two registries
//! therefore never publish one URI for one name, and what the URI names agrees with the
//! identity members reported beside it.**
//!
//! Where the sibling suite `crate::repository_registry_uri` asserts the composed value
//! against the authority this harness expects, this suite READS the reported value the way a
//! consumer of the member does — splitting it at the first `/` and the authority into its
//! labels — and holds what it reads against the OTHER members of the same response: the
//! account `Repository$registryId` reports and the Region `Repository$repositoryArn` names
//! ("The ARN contains the `arn:aws:ecr` namespace, followed by the region of the repository,
//! Amazon Web Services account ID of the repository owner, repository namespace, and
//! repository name"). So a URI that dropped a label of the registry it belongs to — leaving
//! one name in two Regions published under one address — fails here however the authority is
//! spelled, since the Region label is then no longer the Region the ARN names and the grid's
//! addresses are no longer distinct.
//!
//! The class the rule generalises over is the Region the repository is held in crossed with
//! the repository name, read back through every operation that reports a `Repository`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::wire::{at, call_in_region, emulator, ok_in_region, REGISTRY_ID};

/// The Regions this grid spans: five, of four partitions, and in no sorted order — the
/// registry a repository belongs to is the one its call was signed for, and its Region is
/// part of the repository's identity rather than a value any one constant holds. None of them
/// is a Region the sibling suites drive, so a value taken from somewhere other than the
/// addressed Region cannot pass this grid either.
const REGIONS: &[&str] = &[
    "eu-central-1",
    "ap-northeast-3",
    "us-gov-west-1",
    "cn-northwest-1",
    "sa-east-1",
];

/// The repository names this grid spans — a short one, the `.`/`-` separated form, the
/// doubled `_` separator and a namespaced name ("can be specified with namespace levels"),
/// which are the forms `RepositoryName`'s pattern admits
/// (`^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`, with
/// `@length(min: 2, max: 256)`):
///
/// ```text
/// smithy_query(query="shape",shape="RepositoryName",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const NAMES: &[&str] = &["svc", "group-b.service-2", "a__b", "org/team/svc"];

/// The two labels every Amazon ECR registry authority carries between the owning account and
/// the registry's Region.
///
/// Source: the registry URL format the model states in `AuthorizationData$proxyEndpoint`,
/// "The Amazon ECR registry URL format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`".
const FIXED_REGISTRY_LABELS: [&str; 2] = ["dkr", "ecr"];

/// The error a create for a name the ADDRESSED registry already holds is refused with.
const ALREADY_EXISTS: &str = "RepositoryAlreadyExistsException";

/// What a published repository address names, read the way a consumer of the member reads
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishedAddress {
    /// The account the authority's first label names.
    registry_id: String,
    /// The Region the authority names: the label after the two fixed ones.
    region: String,
    /// Everything after the Region label — the zone the registry is served in, which says
    /// nothing about WHICH registry it is.
    zone: String,
    /// The repository the path after the first `/` asks for.
    repository: String,
}

/// Read `uri` as a container client does: split it at the first `/` — the authority it dials
/// ahead of that `/`, the repository it asks for after it — and split the authority into the
/// account label, the two fixed registry labels, the Region label and the zone that follows
/// them.
fn published_address(case: &str, uri: &str) -> PublishedAddress {
    let (authority, repository) = uri.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected <authority>/<repository>, got {uri:?}")
    });
    assert!(
        !authority.contains(':'),
        "case {case:?}: expected an authority carrying no port, as AWS's own spelling does, \
         got {authority:?}"
    );
    let labels: Vec<&str> = authority.splitn(5, '.').collect();
    assert_eq!(
        labels.len(),
        5,
        "case {case:?}: expected the account, {FIXED_REGISTRY_LABELS:?}, the Region and a \
         zone in the authority, got {authority:?}"
    );
    assert_eq!(
        [labels[1], labels[2]],
        FIXED_REGISTRY_LABELS,
        "case {case:?}: expected the fixed registry labels, got {authority:?}"
    );
    PublishedAddress {
        registry_id: labels[0].to_string(),
        region: labels[3].to_string(),
        zone: labels[4].to_string(),
        repository: repository.to_string(),
    }
}

/// The Region a repository ARN names: its fourth field, as the member documents the layout
/// ("the `arn:aws:ecr` namespace, followed by the region of the repository, Amazon Web
/// Services account ID of the repository owner, repository namespace, and repository name").
fn arn_region(case: &str, arn: &str) -> String {
    arn.split(':')
        .nth(3)
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| panic!("case {case:?}: expected a Region in the ARN, got {arn:?}"))
        .to_string()
}

/// The address the `Repository` at `pointer` publishes, held against the identity members
/// reported beside it in the same response: the account `registryId` names, the Region the
/// ARN names and the repository `repositoryName` is.
fn reported_address(case: &str, body: &Value, pointer: &str) -> PublishedAddress {
    let uri = at(body, &format!("{pointer}/repositoryUri"))
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a repositoryUri, got {body}"))
        .to_string();
    let address = published_address(case, &uri);

    let registry_id = at(body, &format!("{pointer}/registryId"));
    assert_eq!(
        Value::from(address.registry_id.as_str()),
        registry_id,
        "case {case:?}: expected the address {uri:?} to name the account reported beside it, \
         got {registry_id:?}"
    );
    let arn = at(body, &format!("{pointer}/repositoryArn"));
    let arn = arn
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a repositoryArn, got {body}"));
    assert_eq!(
        address.region,
        arn_region(case, arn),
        "case {case:?}: expected the address {uri:?} to name the Region of the registry its \
         ARN {arn:?} names"
    );
    let name = at(body, &format!("{pointer}/repositoryName"));
    assert_eq!(
        Value::from(address.repository.as_str()),
        name,
        "case {case:?}: expected the path of {uri:?} to be the repository's own name, got \
         {name:?}"
    );
    address
}

/// RULE: the address a repository is published at names the registry that holds it — the
/// owning account and the Region — so across a grid of Regions crossed with repository names
/// every address is distinct, differs from the others only in the labels of that identity,
/// and is the same value whichever operation reports the repository.
#[tokio::test]
async fn one_name_in_many_registries_is_published_at_one_address_each() {
    let router = emulator();
    // Every address this grid produced, so a URI two registries share is caught however it
    // was spelled: the key is the address, the value the row that published it.
    let mut published: BTreeMap<String, String> = BTreeMap::new();

    for region in REGIONS {
        for name in NAMES {
            let case = format!("{name} in {region}");
            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let address = reported_address(&case, &created, "/repository");
            assert_eq!(
                (address.registry_id.as_str(), address.region.as_str()),
                (REGISTRY_ID, *region),
                "case {case:?}: expected the address to name this account's registry in the \
                 Region addressed, got {address:?}"
            );
            assert_eq!(
                address.repository, *name,
                "case {case:?}: expected the repository's own name as the path, got {address:?}"
            );

            // A read of the repository through its own Region reports the very same address.
            let described = ok_in_region(
                &router,
                &case,
                region,
                "DescribeRepositories",
                &json!({"repositoryNames": [name]}),
            )
            .await;
            assert_eq!(
                reported_address(&case, &described, "/repositories/0"),
                address,
                "case {case:?}: expected the read to report the address the create did, got \
                 {described}"
            );

            let uri = format!(
                "{}.{}.{}.{}.{}/{}",
                address.registry_id,
                FIXED_REGISTRY_LABELS[0],
                FIXED_REGISTRY_LABELS[1],
                address.region,
                address.zone,
                address.repository
            );
            if let Some(held_by) = published.insert(uri.clone(), case.clone()) {
                panic!(
                    "case {case:?}: expected an address of its own, got {uri:?}, already \
                     published for {held_by:?}"
                );
            }
        }
    }

    assert_eq!(
        published.len(),
        REGIONS.len() * NAMES.len(),
        "expected one address per Region and name, got {published:?}"
    );
    // Only the identity labels differ: every row is served in the one zone this emulator
    // announces its names in, so the distinctness above comes from the registry each address
    // names rather than from where the registries are served.
    let zones: std::collections::BTreeSet<String> = published
        .keys()
        .map(|uri| published_address("the published grid", uri).zone)
        .collect();
    assert_eq!(
        zones.len(),
        1,
        "expected every registry to be served in the one announced zone, got {zones:?}"
    );
}

/// RULE: the duplicate-name guard is per registry, so the create AWS refuses is the one
/// addressed to the Region that already holds the name — reported with the sentence AWS
/// words it in — while the same name in every other Region is a repository of its own,
/// published at its own registry's address.
#[tokio::test]
async fn a_name_one_registry_holds_is_published_elsewhere_under_its_own_registry() {
    for name in NAMES {
        let router = emulator();
        let (home, elsewhere) = REGIONS
            .split_first()
            .expect("the Region grid carries a home Region");
        let case = format!("{name} held in {home}");

        let created = ok_in_region(
            &router,
            &case,
            home,
            "CreateRepository",
            &json!({"repositoryName": name, "imageTagMutability": "IMMUTABLE"}),
        )
        .await;
        let home_address = reported_address(&case, &created, "/repository");

        // The same name against the SAME registry is the collision AWS refuses.
        let refused = call_in_region(
            &router,
            home,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for a name this registry holds, got {} with {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            ALREADY_EXISTS,
            "case {case:?}: expected {ALREADY_EXISTS}, got {:?}",
            refused.body
        );
        let expected_message = format!(
            "The repository with name '{name}' already exists in the registry with id \
             '{REGISTRY_ID}'"
        );
        assert_eq!(
            refused.message(),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {:?}",
            refused.message()
        );

        // Every OTHER Region's registry is free to hold the name, each publishing it at its
        // own address — and the delete that reports the repository back reports that address
        // too.
        for region in elsewhere {
            let case = format!("{name} also created in {region}");
            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let address = reported_address(&case, &created, "/repository");
            assert_eq!(
                address.region, **region,
                "case {case:?}: expected the address to name the Region holding it, got \
                 {address:?}"
            );
            assert_ne!(
                address, home_address,
                "case {case:?}: expected two registries holding one name to publish two \
                 addresses, got {address:?} for both"
            );

            let deleted = ok_in_region(
                &router,
                &case,
                region,
                "DeleteRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            assert_eq!(
                reported_address(&case, &deleted, "/repository"),
                address,
                "case {case:?}: expected the delete to report the address the create did, got \
                 {deleted}"
            );
        }

        // The home registry's repository is untouched by any of that, settings and all.
        let described = ok_in_region(
            &router,
            &case,
            home,
            "DescribeRepositories",
            &json!({"repositoryNames": [name]}),
        )
        .await;
        assert_eq!(
            reported_address(&case, &described, "/repositories/0"),
            home_address,
            "case {case:?}: expected the home registry to keep publishing its own address, \
             got {described}"
        );
        assert_eq!(
            at(&described, "/repositories/0/imageTagMutability"),
            json!("IMMUTABLE"),
            "case {case:?}: expected the refused create to leave the stored mutability alone, \
             got {described}"
        );
    }
}
