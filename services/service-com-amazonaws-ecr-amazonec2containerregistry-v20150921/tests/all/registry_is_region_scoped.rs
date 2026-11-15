// SPDX-License-Identifier: CC0-1.0

//! The REGISTRY a repository belongs to, end to end over the wire.
//!
//! The rule under test: **an Amazon ECR registry is one account's IN ONE REGION, so a
//! repository's identity is the `(account, Region, name)` triple its `repositoryArn` names
//! — a name taken in one Region is free in every other, and a call addressed to one
//! Region's endpoint reaches only that Region's repositories.**
//!
//! `CreateRepository`'s own `RepositoryAlreadyExistsException` ("the specified repository
//! already exists in the specified registry") is therefore judged per registry: the
//! duplicate create in the SAME Region is refused, and the same name in another Region is
//! created and reported with an ARN naming that Region. Every repository-addressed call
//! follows from the same rule, so the reads and writes below are asserted per Region too.
//!
//! Each case goes through the router with its own SigV4 credential scope
//! ([`crate::wire::call_in_region`]), which is what says which Region a call is made
//! against.

use serde_json::{json, Value};

use crate::wire::{
    at, call_in_region, emulator, ok_in_region, REGISTRY_ID,
};

/// The error `CreateRepository` refuses a name its registry already holds with.
const ALREADY_EXISTS: &str = "RepositoryAlreadyExistsException";

/// The error every repository-addressed call answers for a repository the addressed
/// registry does not hold.
const NOT_FOUND: &str = "RepositoryNotFoundException";

/// The Regions these cases span: four, so the rule is asserted over more than a pair, and
/// deliberately in no sorted order — the registry a call reaches is the one its credential
/// scope names, whatever order the Regions were used in.
const REGIONS: &[&str] = &["us-west-2", "us-east-1", "eu-west-1", "ap-southeast-2"];

/// The repository names these cases span: a plain one, one with the `.`/`_`/`-`
/// separators `RepositoryName`'s pattern admits, and a namespaced one (a name "can be
/// specified with namespace levels").
///
/// Source: `smithy_query(query="shape", shape="RepositoryName")`.
const NAMES: &[&str] = &["app", "team_a.service-1", "team-a/nested/app"];

/// The message AWS refuses a duplicate create with, built from the request and this
/// registry's own id.
fn already_exists_message(name: &str) -> String {
    format!("The repository with name '{name}' already exists in the registry with id '{REGISTRY_ID}'")
}

/// Assert a call made against `region` is refused with the wire code `code`, and answer the
/// refusal's body.
async fn refused_in_region(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
    code: &str,
) -> Value {
    let answer = call_in_region(router, region, operation, body).await;
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 from {operation} in {region}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected {code} from {operation} in {region}, got {:?}",
        answer.body
    );
    answer.body
}

/// The repository names `DescribeRepositories` reports to a call made against `region`.
async fn listed_in_region(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    region: &str,
) -> Vec<String> {
    let described = ok_in_region(router, case, region, "DescribeRepositories", &json!({})).await;
    described["repositories"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeRepositories reports a list"))
        .iter()
        .filter_map(|repository| repository["repositoryName"].as_str())
        .map(ToString::to_string)
        .collect()
}

/// RULE: one name is created once PER REGION. The duplicate create in the Region that
/// already holds the name is refused with `RepositoryAlreadyExistsException`, while the
/// same name in every other Region is created and reported with an ARN naming that Region.
#[tokio::test]
async fn a_repository_name_is_taken_in_one_region_only() {
    let router = emulator();

    for name in NAMES {
        for region in REGIONS {
            let case = format!("{name} in {region}");
            let created = ok_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            assert_eq!(
                at(&created, "/repository/repositoryArn"),
                json!(format!(
                    "arn:aws:ecr:{region}:{REGISTRY_ID}:repository/{name}"
                )),
                "case {case:?}: the created repository's ARN names the Region it was \
                 created in, got {created:?}"
            );

            // The SAME name against the SAME Region's endpoint is the duplicate AWS
            // refuses, whatever other Regions already hold it.
            let refusal = refused_in_region(
                &router,
                &case,
                region,
                "CreateRepository",
                &json!({"repositoryName": name}),
                ALREADY_EXISTS,
            )
            .await;
            assert_eq!(
                refusal["message"].as_str(),
                Some(already_exists_message(name).as_str()),
                "case {case:?}: expected the duplicate refusal to name the repository and \
                 the registry, got {refusal:?}"
            );
        }
    }

    // Every Region ends up holding every name, each in its own registry.
    for region in REGIONS {
        let listed = listed_in_region(&router, "every name per Region", region).await;
        let mut expected: Vec<String> = NAMES.iter().map(ToString::to_string).collect();
        expected.sort();
        let mut listed_sorted = listed.clone();
        listed_sorted.sort();
        assert_eq!(
            listed_sorted, expected,
            "case {region:?}: expected each Region's registry to hold every created name, \
             got {listed:?}"
        );
    }
}

/// RULE: a repository is reachable only through its OWN Region's endpoint — a read, a
/// write and a delete addressed to another Region address no repository of that registry,
/// so each answers `RepositoryNotFoundException` and changes nothing.
///
/// The rows differ along which operation is addressed: a read of the repository
/// (`DescribeRepositories` scoped to the name), a read of its images (`ListImages`), a
/// write of its configuration (`PutImageScanningConfiguration`) and a push (`PutImage`).
#[tokio::test]
async fn a_repository_is_reachable_only_in_its_own_region() {
    let router = emulator();
    let name = "scoped-app";
    let holder = "us-west-2";

    ok_in_region(
        &router,
        "the repository's own Region",
        holder,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;

    // (case, operation, request)
    let rows: &[(&str, &str, Value)] = &[
        (
            "a read of the repository",
            "DescribeRepositories",
            json!({"repositoryNames": [name]}),
        ),
        ("a read of its images", "ListImages", json!({"repositoryName": name})),
        (
            "a write of its scanning configuration",
            "PutImageScanningConfiguration",
            json!({
                "repositoryName": name,
                "imageScanningConfiguration": {"scanOnPush": true},
            }),
        ),
        (
            "a push into it",
            "PutImage",
            json!({
                "repositoryName": name,
                "imageManifest": "{\"schemaVersion\":2}",
                "imageTag": "v1",
            }),
        ),
        (
            "a delete of it",
            "DeleteRepository",
            // `force` because the push above left an image in it: "if true, deleting the
            // repository force deletes the contents of the repository".
            json!({"repositoryName": name, "force": true}),
        ),
    ];

    for (case, operation, body) in rows {
        for region in REGIONS.iter().filter(|region| **region != holder) {
            refused_in_region(&router, case, region, operation, body, NOT_FOUND).await;
        }
        // The same call against the repository's own Region reaches it.
        let answer = call_in_region(&router, holder, operation, body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the call against {holder} to reach the repository, got \
             {} with {:?}",
            answer.status, answer.body
        );
    }
}

/// RULE: the images of a repository belong to the repository, so two Regions holding a
/// repository of the SAME name hold two repositories: a push into one is not in the other,
/// and deleting one leaves the other with its images.
#[tokio::test]
async fn two_regions_holding_one_name_hold_two_repositories() {
    let router = emulator();
    let name = "shared-name";
    let first = "us-west-2";
    let second = "us-east-1";

    for region in [first, second] {
        ok_in_region(
            &router,
            "a repository of the same name per Region",
            region,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
    }

    // One image pushed into the FIRST Region's repository only.
    let pushed = ok_in_region(
        &router,
        "a push into the first Region",
        first,
        "PutImage",
        &json!({
            "repositoryName": name,
            "imageManifest": "{\"schemaVersion\":2,\"image\":\"first\"}",
            "imageTag": "v1",
        }),
    )
    .await;
    let digest = at(&pushed, "/image/imageId/imageDigest");

    let listed = ok_in_region(
        &router,
        "the first Region's images",
        first,
        "ListImages",
        &json!({"repositoryName": name}),
    )
    .await;
    assert_eq!(
        at(&listed, "/imageIds/0/imageDigest"),
        digest,
        "expected the pushed image in the Region it was pushed to, got {listed:?}"
    );

    let other = ok_in_region(
        &router,
        "the second Region's images",
        second,
        "ListImages",
        &json!({"repositoryName": name}),
    )
    .await;
    assert_eq!(
        other["imageIds"].as_array().map(Vec::len),
        Some(0),
        "expected the other Region's repository of the same name to hold no image, got \
         {other:?}"
    );

    // Deleting the EMPTY second Region's repository leaves the first Region's — images and
    // all — exactly as it was.
    ok_in_region(
        &router,
        "deleting the second Region's repository",
        second,
        "DeleteRepository",
        &json!({"repositoryName": name}),
    )
    .await;
    let still = ok_in_region(
        &router,
        "the first Region after the other was deleted",
        first,
        "ListImages",
        &json!({"repositoryName": name}),
    )
    .await;
    assert_eq!(
        at(&still, "/imageIds/0/imageDigest"),
        digest,
        "expected the first Region's repository to keep its image, got {still:?}"
    );
    let listed = listed_in_region(&router, "the second Region after its delete", second).await;
    assert!(
        listed.is_empty(),
        "expected the second Region's registry to hold nothing, got {listed:?}"
    );
}
