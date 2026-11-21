// SPDX-License-Identifier: CC0-1.0

//! WHICH identifier a mutation names when the REST API it addresses is GONE, asserted on
//! the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the create
//! helpers are this crate's existing harness.
//!
//! The RULE under test: a call whose own target IS the REST API — including the create that
//! resolves `restApiId` as a precondition of its own, ahead of the parent it would hang the
//! new record off — refuses a `restApiId` that resolves to no stored API with 404
//! `NotFoundException` and the account-qualified identifier sentence
//! (`crate::rest_api_identifier_refusals::no_such_api`), however the API came to be absent
//! and whatever the rest of the request names; and a `parentId` is resolved INSIDE the API
//! the request names, so the fixed resource sentence answers only a parent that names no
//! resource of an API that DOES exist.
//!
//! `rest_api_identifier_refusals` and `nested_record_refusals` assert that rule over
//! identifiers no API of the account ever had — the latter's
//! `a_create_resource_reports_the_api_before_the_parent_it_cannot_resolve` over both halves
//! of it. What is NOT covered there, and is what the rows below add, is every row whose API
//! the account HELD and a `DeleteRestApi` removed — the teardown a reconciler drives — and
//! with it the parent forms only such an API can present: its own root resource id and the
//! id of a resource nested under it, both minted by the service and valid right up to the
//! delete, so a lookup that reached a resource outside its API would resolve them, where an
//! identifier never minted has no resource of its own to name. The never-minted rows are
//! kept beside them for ONE reason: they are the control the deleted rows are read against,
//! so the sentence is shown not to turn on how the API became absent. The other axes below —
//! the `pathPart` spread, the live parent of another stored API, and the other
//! API-addressed operations driven on the same gone identifier — are new here too.
//! The rows differ along what the rule generalises over: how the API went away (deleted
//! after being built out and deployed, deleted while flat, never created at all), the
//! identifier's own length, alphabet and case, the parent named beside it (a stale root, a
//! stale nested child, the live root of a DIFFERENT stored API, an unknown id), the
//! `pathPart` submitted with it (a plain segment, a path variable, a greedy one), and the
//! operation that resolved the identifier (the create the mutation runs through, plus — on
//! the DELETED rows, the ones this suite owns — the reads and patches of the API record and
//! the creates of a deployment and a stage, so the mutation and the reads of one removed API
//! are shown to agree).

use serde_json::{json, Value};

use crate::rest_api_identifier_refusals::no_such_api;
use crate::rest_api_lifecycle::{
    assert_failure, create_api, make_deployable, resource_paths, resource_under, string_member,
    Answer, Gateway,
};
use crate::stage_lifecycle::create_deployed_stage;

/// The sentence this service states for a resource address that resolves to no resource
/// (`state::NO_SUCH_RESOURCE`): fixed text in full, with neither the identifier nor the
/// account in it.
const NO_SUCH_RESOURCE: &str = "Invalid Resource identifier specified";

/// Identifiers no API of the account ever had, differing in length (one character, the ten
/// characters API Gateway mints, longer), alphabet (letters, digits, a mix, a hyphen) and
/// case. `restApiId` is a plain `String` with no `@pattern`
/// (`smithy_query(query="operation", operation="CreateResource")`), so every row reaches
/// the lookup.
const NEVER_CREATED: &[&str] = &["r", "zzzzzzzzzz", "0123456789", "Ab3Xy9Zq1W", "no-such-api-02"];

/// Parent identifiers no stored API has a resource under, in the same spread: a resource id
/// is a plain `String` label too.
const UNKNOWN_PARENTS: &[&str] = &["p", "yyyyyyyyyy", "9876543210", "Zq1WAb3Xy9", "no-such-res-02"];

/// `pathPart` values the grammar admits, so a row is refused for the API it names and not
/// for its body: a plain segment, one carrying the punctuation a segment may hold, a path
/// variable and a greedy one (`Resource$pathPart`, whose grammar
/// `rest_api_lifecycle` asserts).
const PATH_PARTS: &[&str] = &["hello", "post-delete", "{petId}", "{proxy+}"];

/// An API that is NOT in the store, with whatever identifiers were valid parents of it
/// while it was.
struct Absent {
    /// How this API came to be absent, named in every assertion of the row.
    how: String,
    /// The `restApiId` the rows send.
    api: String,
    /// Parents the service itself minted for this API, valid until it went away.
    stale_parents: Vec<String>,
    /// Whether the account HELD this API and a `DeleteRestApi` removed it. The rows that did
    /// are the ones this suite owns, so only they drive the other API-addressed operations:
    /// over an identifier never minted, `rest_api_identifier_refusals` already owns that
    /// matrix, and a second copy of it would have to be edited in two places.
    held: bool,
}

/// `DeleteRestApi` for `api`, asserting the removal was accepted.
async fn delete_api(gateway: &Gateway, api: &str) {
    let answer = gateway
        .call("DELETE", &format!("/restapis/{api}"), None)
        .await;
    assert_eq!(
        answer.status, 202,
        "case DeleteRestApi({api}): expected 202, got {} with {:?}",
        answer.status, answer.body
    );
}

/// `CreateResource` for `path_part` under `parent` of `api`, whatever it answers — unlike
/// `rest_api_lifecycle::create_resource`, which asserts the create was accepted, because every
/// row below drives a create that is expected to be REFUSED.
async fn attempt_create_resource(
    gateway: &Gateway,
    api: &str,
    parent: &str,
    path_part: &str,
) -> Answer {
    gateway
        .call(
            "POST",
            &format!("/restapis/{api}/resources/{parent}"),
            Some(json!({ "pathPart": path_part })),
        )
        .await
}

/// A patch body that is valid in itself, so a row reaches the identifier lookup instead of
/// being refused for its body.
fn patch(path: &str, value: &str) -> Value {
    json!({ "patchOperations": [{ "op": "replace", "path": path, "value": value }] })
}

/// Every other call of this service whose own target IS the API, with `api` the identifier
/// under test: the record's own read, its patch, two collections of it, and the two creates
/// that snapshot it. None of them needs a record of the API to exist, so each is refused for
/// the identifier alone.
fn api_targeted_requests(api: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        ("GetRestApi", "GET", format!("/restapis/{api}"), None),
        (
            "UpdateRestApi",
            "PATCH",
            format!("/restapis/{api}"),
            Some(patch("/description", "d")),
        ),
        (
            "GetResources",
            "GET",
            format!("/restapis/{api}/resources"),
            None,
        ),
        ("GetModels", "GET", format!("/restapis/{api}/models"), None),
        (
            "CreateDeployment",
            "POST",
            format!("/restapis/{api}/deployments"),
            Some(json!({})),
        ),
        (
            "CreateStage",
            "POST",
            format!("/restapis/{api}/stages"),
            Some(json!({ "stageName": "prod", "deploymentId": "zzzzzzzzzz" })),
        ),
        ("DeleteRestApi", "DELETE", format!("/restapis/{api}"), None),
    ]
}

/// Assert `answer` is a 404 `NotFoundException` whose message is exactly `message`.
fn assert_reports(answer: &Answer, case: &str, message: &str) {
    assert_failure(answer, 404, "NotFoundException", case);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

/// The absent APIs under test: two the account HELD and deleted — one built out into a tree
/// and deployed to a stage, the way a teardown leaves it, and one still flat — and the
/// identifiers no API ever had.
async fn absent_apis(gateway: &Gateway) -> Vec<Absent> {
    let mut rows = Vec::new();

    // Deleted after being built out: a nested tree and a deployed stage, so the delete had
    // records of every kind to take with it and the ids below were live parents.
    let (built, built_root) = create_api(gateway, "deleted-after-buildout").await;
    let pets = resource_under(gateway, &built, &built_root, "pets").await;
    let by_id = resource_under(gateway, &built, &pets, "{petId}").await;
    make_deployable(gateway, &built).await;
    create_deployed_stage(gateway, &built, "prod").await;
    delete_api(gateway, &built).await;
    rows.push(Absent {
        how: "an api deleted after it was built out and deployed".to_string(),
        api: built,
        stale_parents: vec![built_root, pets, by_id],
        held: true,
    });

    // Deleted while flat: the root resource every API is created with is the only parent it
    // ever had.
    let (flat, flat_root) = create_api(gateway, "deleted-while-flat").await;
    delete_api(gateway, &flat).await;
    rows.push(Absent {
        how: "an api deleted while it was still flat".to_string(),
        api: flat,
        stale_parents: vec![flat_root],
        held: true,
    });

    for never in NEVER_CREATED {
        rows.push(Absent {
            how: format!("an api identifier {never:?} the account never held"),
            api: (*never).to_string(),
            stale_parents: Vec::new(),
            held: false,
        });
    }
    rows
}

/// RULE: `CreateResource` resolves `restApiId` first, so an API that is not in the store —
/// deleted or never created — is refused as the API, with the account-qualified identifier
/// sentence, whatever parent and `pathPart` the request carries.
#[tokio::test]
async fn a_create_on_a_gone_rest_api_names_the_api_identifier() {
    let gateway = Gateway::new();
    // One surviving API supplies a LIVE parent, so a row can name the absent API beside a
    // resource id that does resolve somewhere in the account.
    let (stored, stored_root) = create_api(&gateway, "surviving-api").await;

    for absent in absent_apis(&gateway).await {
        // The parents a row names beside the absent API: every parent the service minted
        // for it, the live root of the surviving API, and a parent no API has.
        let mut parents: Vec<(String, String)> = absent
            .stale_parents
            .iter()
            .map(|parent| (format!("its own stale parent {parent}"), parent.clone()))
            .collect();
        parents.push((
            "the live root of another stored api".to_string(),
            stored_root.clone(),
        ));
        for unknown in UNKNOWN_PARENTS {
            parents.push((format!("the unknown parent {unknown:?}"), (*unknown).to_string()));
        }

        for (position, parent) in &parents {
            for path_part in PATH_PARTS {
                let case = format!(
                    "CreateResource(pathPart={path_part:?}) on {} with {position}",
                    absent.how
                );
                let answer =
                    attempt_create_resource(&gateway, &absent.api, parent, path_part).await;
                assert_reports(&answer, &case, &no_such_api(&absent.api));
            }
        }

        // The same sentence from every other call whose own target is the API, so the
        // mutation and the reads of one DELETED API agree — the divergence the create was
        // reported for. An identifier never minted is left to the sibling suite that owns
        // that matrix.
        if absent.held {
            for (operation, method, uri, body) in api_targeted_requests(&absent.api) {
                let case = format!("{operation} [{method} {uri}] on {}", absent.how);
                let answer = gateway.call(method, &uri, body).await;
                assert_reports(&answer, &case, &no_such_api(&absent.api));
            }
        }
    }

    // The same create against the SURVIVING api and its root is served, so the refusals
    // above are about the API identifier and not about the requests.
    let created = attempt_create_resource(&gateway, &stored, &stored_root, "hello").await;
    assert_eq!(
        created.status, 201,
        "case CreateResource(the surviving api): expected 201, got {} with {:?}",
        created.status, created.body
    );
}

/// RULE, the other half: a `parentId` is resolved INSIDE the API the request names, so the
/// fixed resource sentence answers a parent that names no resource of an API that DOES
/// exist — including a parent that was a resource of a DIFFERENT, now deleted API.
#[tokio::test]
async fn a_create_on_a_stored_api_names_the_resource_for_a_parent_it_does_not_hold() {
    let gateway = Gateway::new();
    let (stored, stored_root) = create_api(&gateway, "stored-api-parent-scope").await;

    // A second API, built out and then deleted, so its resource ids are ids the service
    // really minted and no API holds any more.
    let (gone, gone_root) = create_api(&gateway, "gone-api-parent-scope").await;
    let gone_child = resource_under(&gateway, &gone, &gone_root, "pets").await;
    delete_api(&gateway, &gone).await;

    // A third API that is STILL stored, so its root is a resource id that resolves — just
    // not under the API the request names.
    let (elsewhere, elsewhere_root) = create_api(&gateway, "elsewhere-api-parent-scope").await;

    let mut parents: Vec<(String, String)> = vec![
        (
            "the stale root of a deleted api".to_string(),
            gone_root.clone(),
        ),
        (
            "a stale nested resource of a deleted api".to_string(),
            gone_child.clone(),
        ),
        (
            "the live root of another stored api".to_string(),
            elsewhere_root.clone(),
        ),
    ];
    for unknown in UNKNOWN_PARENTS {
        parents.push((format!("the unknown parent {unknown:?}"), (*unknown).to_string()));
    }

    for (position, parent) in &parents {
        for path_part in PATH_PARTS {
            let case = format!(
                "CreateResource(pathPart={path_part:?}) on the stored api with {position}"
            );
            let answer = attempt_create_resource(&gateway, &stored, parent, path_part).await;
            assert_reports(&answer, &case, NO_SUCH_RESOURCE);
        }
    }

    // Each of those parents is served by the API that DOES hold it, so the refusals above
    // are about the pairing and not about the parents.
    for (api, parent, case) in [
        (&stored, &stored_root, "the stored api's own root"),
        (&elsewhere, &elsewhere_root, "the other stored api's own root"),
    ] {
        let answer = attempt_create_resource(&gateway, api, parent, "hello").await;
        assert_eq!(
            answer.status, 201,
            "case CreateResource({case}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        // Read back through the API that served the create: the child hangs off the parent
        // the request named, in the tree of THAT API, under the path its parent's path and
        // its own part materialise.
        let created = string_member(&answer.body, "id", case);
        assert_eq!(
            answer.body["parentId"].as_str(),
            Some(parent.as_str()),
            "case CreateResource({case}): expected the parent {parent:?}, got {:?}",
            answer.body["parentId"]
        );
        let read = gateway
            .call("GET", &format!("/restapis/{api}/resources/{created}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case GetResource({case}): expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["parentId"].as_str(),
            Some(parent.as_str()),
            "case GetResource({case}): expected the parent {parent:?}, got {:?}",
            read.body["parentId"]
        );
        let paths = resource_paths(&gateway, api).await;
        assert!(
            paths.contains(&"/hello".to_string()),
            "case GetResources({case}): expected the created /hello in {paths:?}"
        );
    }
}
