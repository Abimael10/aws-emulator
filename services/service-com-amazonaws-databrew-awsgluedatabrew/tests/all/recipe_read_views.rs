// SPDX-License-Identifier: CC0-1.0

//! What a READ of one recipe version answers with: its tag set, and the description the
//! addressed view of the version reports.
//!
//! Two rules are pinned here.
//!
//! 1. A recipe read answers with the recipe's whole tag set: the `Tags` member is part of
//!    every answer, holding no entry when the recipe carries no tag, so a client that
//!    enumerates or diffs tags reads an empty map rather than a missing member. The tags
//!    belong to the recipe rather than to one of its versions, so every view of every
//!    version reports the same map.
//!
//! 2. A version a publish froze keeps the DEFINITION it was frozen from, its description
//!    included, and that is what a read addressing the version (by its own number, or the
//!    draft by its number or alias) answers with. The `Description` a publish carries
//!    describes the recipe AS PUBLISHED (model: `PublishRecipeRequest$Description` — "A
//!    description of the recipe to be published, for this version of the recipe"), so it
//!    is the recipe's latest-published view that reports it — the `LATEST_PUBLISHED`
//!    alias and the omitted identifier that asks for it (model:
//!    `DescribeRecipeRequest$RecipeVersion`) — falling back to the frozen definition's
//!    own description when that publish carried none.
//!
//! The tables span what the rules generalise over: how many tags a recipe carries, which
//! version identifier the read addresses, how many publishes the recipe has had, what
//! each publish carried (text of its own, nothing, an empty string), and whether the
//! draft was edited between publishes.
//!
//! Every case drives the real wire path, so what is asserted is the HTTP status and the
//! JSON body a client sees.

use serde_json::{json, Value};

use crate::databrew_harness::{label, Emulator, Reply};

/// The alias naming the most recent published version (model: `Recipe$RecipeVersion`).
const LATEST_PUBLISHED: &str = "LATEST_PUBLISHED";

/// The alias naming the version development continues on (model:
/// `Recipe$RecipeVersion`).
const LATEST_WORKING: &str = "LATEST_WORKING";

/// `CreateRecipe`: `POST /recipes`.
async fn create_recipe(
    emulator: &Emulator,
    name: &str,
    description: Option<&str>,
    tags: Option<&Value>,
) -> Reply {
    let steps = json!([{"Action": {"Operation": "UPPER_CASE", "Parameters": {"sourceColumn": "dept"}}}]);
    let mut body = json!({"Name": name, "Steps": steps});
    if let Some(description) = description {
        body["Description"] = json!(description);
    }
    if let Some(tags) = tags {
        body["Tags"] = tags.clone();
    }
    emulator.call("POST", "/recipes", Some(body)).await
}

/// `PublishRecipe`: `POST /recipes/{Name}/publishRecipe`, with the optional
/// `Description` in the body.
async fn publish_recipe(emulator: &Emulator, name: &str, description: Option<&str>) -> Reply {
    let body = match description {
        Some(description) => json!({"Description": description}),
        None => json!({}),
    };
    emulator
        .call(
            "POST",
            &format!("/recipes/{}/publishRecipe", label(name)),
            Some(body),
        )
        .await
}

/// `UpdateRecipe`: `PUT /recipes/{Name}`, the operation that modifies the working
/// version.
async fn update_recipe(
    emulator: &Emulator,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> Reply {
    let mut body = json!({"Steps": steps.clone()});
    if let Some(description) = description {
        body["Description"] = json!(description);
    }
    emulator
        .call("PUT", &format!("/recipes/{}", label(name)), Some(body))
        .await
}

/// `DescribeRecipe`: `GET /recipes/{Name}`, with the optional `recipeVersion` query
/// parameter the model binds `RecipeVersion` to.
async fn describe_recipe(emulator: &Emulator, name: &str, version: Option<&str>) -> Reply {
    let uri = match version {
        Some(version) => format!("/recipes/{}?recipeVersion={version}", label(name)),
        None => format!("/recipes/{}", label(name)),
    };
    emulator.call("GET", &uri, None).await
}

/// Assert the answer reports `expected` for `Description`, absent when the row expects
/// none.
fn assert_description(described: &Reply, case: &str, addressed: &str, expected: Option<&str>) {
    match expected {
        Some(expected) => assert_eq!(
            described.text("Description"),
            expected,
            "case {case:?}: the read {addressed} must report {expected:?}, got {:?}",
            described.body["Description"]
        ),
        None => assert!(
            described.is_absent("Description"),
            "case {case:?}: the read {addressed} has no description to report, got {:?}",
            described.body["Description"]
        ),
    }
}

/// A recipe read answers with the recipe's whole tag set, for every number of tags a
/// recipe can carry and whichever of its versions is addressed.
#[tokio::test]
async fn a_recipe_read_reports_the_whole_tag_set() {
    // The rows differ along how many tags the recipe carries: none at all, one, a few,
    // and a map well past one entry. `TagMap` is a map of `TagKey` to `TagValue` (model:
    // `smithy_query(query="shape", shape="TagMap")`), so the count is the axis the rule
    // generalises over.
    let many: Value = Value::Object(
        (0..12)
            .map(|index| (format!("key-{index}"), json!(format!("value-{index}"))))
            .collect(),
    );
    let tag_cases: [(&str, Option<Value>); 4] = [
        ("no tags at all", None),
        ("one tag", Some(json!({"team": "data"}))),
        (
            "several tags",
            Some(json!({"team": "data", "env": "test", "tier": "gold"})),
        ),
        ("a dozen tags", Some(many)),
    ];

    let emulator = Emulator::new();
    for (index, (case, tags)) in tag_cases.into_iter().enumerate() {
        let name = format!("tagged-{index}");
        assert_eq!(
            create_recipe(&emulator, &name, Some("the draft body"), tags.as_ref())
                .await
                .status,
            200,
            "case {case:?}: the recipe under test must be created"
        );
        assert_eq!(
            publish_recipe(&emulator, &name, Some("published once"))
                .await
                .status,
            200,
            "case {case:?}: the publish under test must answer 200"
        );

        // The tags belong to the recipe, so every identifier that resolves a version of
        // it answers with the same map: the two aliases, each version's own number, and
        // the omitted identifier that asks for the latest published version.
        for (addressed, identifier) in [
            ("by the working alias", Some(LATEST_WORKING)),
            ("by the working version's number", Some("1.1")),
            ("by the published version's number", Some("1.0")),
            ("by the published alias", Some(LATEST_PUBLISHED)),
            ("with no identifier at all", None),
        ] {
            let described = describe_recipe(&emulator, &name, identifier).await;
            assert_eq!(
                described.status, 200,
                "case {case:?}: the read {addressed} must answer 200, got {} {:?}",
                described.status, described.body
            );
            // An untagged recipe reports the map it holds, which has no entry in it.
            let expected = tags.clone().unwrap_or_else(|| json!({}));
            assert_eq!(
                described.member("Tags"),
                expected,
                "case {case:?}: the read {addressed} must report {expected:?}, got {:?}",
                described.body["Tags"]
            );
        }
    }
}

/// One row of the publish table: the case name, the description the recipe was created
/// with, and the description each successive publish carried.
type PublishRun = (
    &'static str,
    Option<&'static str>,
    &'static [Option<&'static str>],
);

/// A frozen version reports the description of the definition it froze, while the
/// recipe's latest-published view reports what its publish said about the recipe.
#[tokio::test]
async fn a_frozen_version_reports_its_definition_and_the_published_view_its_publish() {
    // The rows differ along how many publishes the recipe has had and what each one
    // carried: a description of its own, none at all, or an empty one (`RecipeDescription`
    // is `len 0..1024`, so the empty string is a description a publish may carry) — and
    // whether the recipe was created with a description for a publish to fall back to.
    let runs: [PublishRun; 6] = [
        (
            "one publish carrying its own description",
            Some("the draft body"),
            &[Some("published for release")],
        ),
        (
            "one publish carrying no description",
            Some("the draft body"),
            &[None],
        ),
        (
            "one publish carrying an empty description",
            Some("the draft body"),
            &[Some("")],
        ),
        (
            "a recipe created without a description, published with one",
            None,
            &[Some("published for release")],
        ),
        (
            "a recipe created without a description, published without one",
            None,
            &[None],
        ),
        (
            "three publishes, the middle one carrying no description",
            Some("the draft body"),
            &[Some("first cut"), None, Some("third cut")],
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, created_with, publishes)) in runs.into_iter().enumerate() {
        let name = format!("published-{index}");
        assert_eq!(
            create_recipe(&emulator, &name, created_with, None)
                .await
                .status,
            200,
            "case {case:?}: the recipe under test must be created"
        );

        for (publish_index, carried) in publishes.iter().copied().enumerate() {
            assert_eq!(
                publish_recipe(&emulator, &name, carried).await.status,
                200,
                "case {case:?}: publish {publish_index} must answer 200"
            );
            // The next whole major number at minor 0 (model: `Recipe$RecipeVersion`).
            let version = format!("{}.0", publish_index + 1);

            // Addressed by its own number, the version reports the definition it froze:
            // the draft's description, which no publish rewrites.
            let frozen = describe_recipe(&emulator, &name, Some(&version)).await;
            assert_eq!(
                frozen.status, 200,
                "case {case:?}: version {version} must resolve, got {} {:?}",
                frozen.status, frozen.body
            );
            assert_description(
                &frozen,
                case,
                &format!("of version {version} by its number"),
                created_with,
            );

            // The latest-published view reports what this publish said about the recipe,
            // and the frozen definition's own description when it said nothing. Both the
            // alias and the omitted identifier address that view.
            for (addressed, identifier) in [
                ("by the published alias", Some(LATEST_PUBLISHED)),
                ("with no identifier at all", None),
            ] {
                let latest = describe_recipe(&emulator, &name, identifier).await;
                assert_eq!(
                    latest.text("RecipeVersion"),
                    version,
                    "case {case:?}: the read {addressed} must answer the newest published \
                     version, got {} {:?}",
                    latest.status,
                    latest.body
                );
                assert_description(&latest, case, addressed, carried.or(created_with));
            }
        }

        // Every earlier version is still addressable by its own number and still reports
        // the definition it froze, so a later publish rewrites none of them.
        for publish_index in 0..publishes.len() {
            let version = format!("{}.0", publish_index + 1);
            let held = describe_recipe(&emulator, &name, Some(&version)).await;
            assert_eq!(
                held.status, 200,
                "case {case:?}: version {version} must still resolve after {} publishes, \
                 got {} {:?}",
                publishes.len(),
                held.status,
                held.body
            );
            assert_description(
                &held,
                case,
                &format!("of version {version} after every publish"),
                created_with,
            );
        }
    }
}

/// An edit of the draft between two publishes is what makes the versions differ: each
/// frozen version reports the draft body it took, the draft reports the edit, and the
/// latest-published view reports the newest publish's own text.
#[tokio::test]
async fn an_edited_draft_leaves_each_frozen_version_with_the_body_it_took() {
    let emulator = Emulator::new();
    let name = "redrafted";
    let second_steps = json!([
        {"Action": {"Operation": "UPPER_CASE", "Parameters": {"sourceColumn": "dept"}}},
        {"Action": {"Operation": "RENAME", "Parameters": {"sourceColumn": "dept", "targetColumn": "DEPARTMENT"}}}
    ]);

    assert_eq!(
        create_recipe(&emulator, name, Some("the first draft"), None)
            .await
            .status,
        200,
        "case \"a redrafted recipe\": the recipe under test must be created"
    );
    assert_eq!(
        publish_recipe(&emulator, name, Some("published from the first draft"))
            .await
            .status,
        200,
        "case \"a redrafted recipe\": the first publish must answer 200"
    );
    assert_eq!(
        update_recipe(&emulator, name, Some("the second draft"), &second_steps)
            .await
            .status,
        200,
        "case \"a redrafted recipe\": the update of the draft must answer 200"
    );

    // The edit moved the draft on; the version frozen before it keeps the body it took,
    // and the latest-published view still reports the text its publish carried.
    let case = "a draft edited after one publish";
    assert_description(
        &describe_recipe(&emulator, name, Some("1.0")).await,
        case,
        "of version 1.0 by its number",
        Some("the first draft"),
    );
    assert_description(
        &describe_recipe(&emulator, name, Some(LATEST_PUBLISHED)).await,
        case,
        "by the published alias",
        Some("published from the first draft"),
    );
    for (addressed, identifier) in [
        ("by the working alias", LATEST_WORKING),
        ("by the working version's number", "1.1"),
    ] {
        let draft = describe_recipe(&emulator, name, Some(identifier)).await;
        assert_description(&draft, case, addressed, Some("the second draft"));
        assert_eq!(
            draft.member("Steps"),
            second_steps,
            "case {case:?}: the draft read {addressed} must hold what the update wrote, got {:?}",
            draft.body["Steps"]
        );
    }

    // A second publish, carrying nothing of its own, freezes the edited draft: the new
    // version reports the draft body it took, and so does the latest-published view,
    // which has no publish text to report.
    assert_eq!(
        publish_recipe(&emulator, name, None).await.status,
        200,
        "case \"a redrafted recipe\": the second publish must answer 200"
    );
    let case = "a second publish of the edited draft";
    for (addressed, identifier) in [
        ("of version 2.0 by its number", Some("2.0")),
        ("by the published alias", Some(LATEST_PUBLISHED)),
        ("with no identifier at all", None),
    ] {
        let read = describe_recipe(&emulator, name, identifier).await;
        assert_eq!(
            read.text("RecipeVersion"),
            "2.0",
            "case {case:?}: the read {addressed} must answer version 2.0, got {} {:?}",
            read.status,
            read.body
        );
        assert_description(&read, case, addressed, Some("the second draft"));
    }
    // The first version is untouched by either the edit or the second publish.
    assert_description(
        &describe_recipe(&emulator, name, Some("1.0")).await,
        case,
        "of version 1.0 by its number",
        Some("the first draft"),
    );
}
