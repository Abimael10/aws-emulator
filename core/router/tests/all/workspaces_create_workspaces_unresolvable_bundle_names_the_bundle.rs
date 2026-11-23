// SPDX-License-Identifier: CC0-1.0

//! Which resource a `CreateWorkspaces` per-element refusal names, end to end over the
//! awsJson1_1 wire of the assembled emulator.
//!
//! RULE: `CreateWorkspaces` judges every `WorkspaceRequest` of its batch on its own and
//! reports an element it cannot satisfy as its own `FailedRequests` entry inside a 200
//! (the operation models only `InvalidParameterValuesException` and
//! `ResourceLimitExceededException`, so an unresolvable reference is never a non-200). An
//! element resolves its `BundleId` BEFORE its `DirectoryId`: one naming no stored bundle is
//! reported against the BUNDLE whatever its `DirectoryId` resolves to, and the DIRECTORY is
//! named only for an element whose bundle resolved and whose directory is registered with
//! nothing. Each code carries the fixed sentence of its class, which interpolates no value
//! from the request or from state.
//!
//! The rows differ along what the rule generalises over: whether each reference resolves
//! (all four combinations), the form and length of an unresolvable `BundleId` inside the
//! model's `^wsb-[0-9a-z]{8,63}$` (both published bounds, digits-only and mixed), the form
//! of an unregistered `DirectoryId` inside `^(d-[0-9a-f]{8,63}$)|(wsd-[0-9a-z]{8,63}$)$`,
//! which stored catalog bundle a satisfiable element names, and the size of the batch and
//! the position of the refused elements in it.
//!
//! This suite lives beside the router because the owning crate still roots its integration
//! tests at a hand-declared `tests/all.rs`, which a fix may not edit; the router's
//! `tests/all/` picks a new suite up with nothing to declare. The service is mounted the
//! way `core/router/src/lib.rs` mounts it, and every value asserted here is read back off
//! the wire.
//!
//! Sources: `smithy_query(query="operation", operation="CreateWorkspaces", model_path=
//! "services/service-com-amazonaws-workspaces-workspacesservice/smithy-model/\
//! com-amazonaws-workspaces-workspacesservice.json")` for the 200-only failure channel, and
//! `smithy_query(query="shape", shape="FailedCreateWorkspaceRequest,WorkspaceRequest", …)`
//! for the per-item `ErrorCode`/`ErrorMessage` members and the two id patterns. The stored
//! bundles the satisfiable rows name are taken from `DescribeWorkspaceBundles`, the
//! operation a client lists them with, rather than from a hand-written list.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The per-item `ErrorCode` for an element whose bundle resolves to nothing stored.
const BUNDLE_NOT_FOUND: &str = "ResourceNotFound.Bundle";
/// The per-item `ErrorCode` for an element whose directory is registered with nothing.
const DIRECTORY_NOT_FOUND: &str = "ResourceNotFound.Directory";

/// The fixed sentence AWS answers beside each of those codes. Fixed text for the error
/// CLASS: neither carries the id the request named, nor a region.
const BUNDLE_NOT_FOUND_MESSAGE: &str = "The specified bundle could not be found in the specified region.";
const DIRECTORY_NOT_FOUND_MESSAGE: &str =
    "The specified directory could not be found in the specified region.";

/// Which directory an element names.
#[derive(Debug, Clone, Copy)]
enum Directory {
    /// Registered through `RegisterWorkspaceDirectory` before the batch is submitted.
    Registered,
    /// Registered by nothing; the `&str` is the id, spanning both alternatives of the
    /// model's `DirectoryId` pattern.
    Unregistered(&'static str),
}

/// Which bundle an element names.
#[derive(Debug, Clone, Copy)]
enum Bundle {
    /// The catalog bundle `DescribeWorkspaceBundles` reports at this index.
    Stored(usize),
    /// A bundle the store holds none of; the `&str` is the id, inside the model's
    /// `BundleId` pattern `^wsb-[0-9a-z]{8,63}$`.
    Unheld(&'static str),
}

/// A `DirectoryId` of the `d-` alternative that this suite registers.
const REGISTERED_DIRECTORY: &str = "d-0ff1ce0b0b";
/// Unregistered ids, one per alternative of the pattern.
const UNREGISTERED_D_FORM: &str = "d-dead0beef1";
const UNREGISTERED_WSD_FORM: &str = "wsd-0unregistered";

/// One emulator carrying Amazon WorkSpaces over its own engine, as the production router
/// mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the emulator's concern, not a service's; the credential scope of the
    // request below is what routes it here.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_workspaces_workspacesservice::EmulatedService::new(engine),
    );
    router
}

/// Call one WorkSpaces operation over the wire and return `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "workspaces.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("WorkspacesService.{operation}"))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIAEMULATOREXAMPLE/20260101/us-east-1/workspaces/aws4_request, SignedHeaders=host, Signature=00",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `BundleId`s `DescribeWorkspaceBundles` reports for the Amazon-provided catalog: the
/// bundles a create may name are the ones the store holds, so the rows read them off the
/// operation a client lists them with.
async fn stored_bundle_ids(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "DescribeWorkspaceBundles", &json!({ "Owner": "AMAZON" })).await;
    assert_eq!(
        status, 200,
        "DescribeWorkspaceBundles must answer 200, body {body:?}"
    );
    let bundles: Vec<String> = body["Bundles"]
        .as_array()
        .unwrap_or_else(|| panic!("Bundles must be a list, got {body:?}"))
        .iter()
        .map(|bundle| {
            bundle["BundleId"]
                .as_str()
                .unwrap_or_else(|| panic!("every bundle carries a BundleId, got {bundle:?}"))
                .to_string()
        })
        .collect();
    assert!(
        bundles.len() >= 2,
        "the catalog must hold bundles for the satisfiable rows to name, got {bundles:?}"
    );
    bundles
}

/// Register `directory_id` the way a client would, so an element naming it resolves.
async fn register_directory(router: &MultiServiceRouter, directory_id: &str) {
    let (status, body) = call(
        router,
        "RegisterWorkspaceDirectory",
        &json!({
            "DirectoryId": directory_id,
            "SubnetIds": ["subnet-11111111", "subnet-22222222"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "RegisterWorkspaceDirectory({directory_id}) must answer 200, body {body:?}"
    );
}

/// Submit `elements` as one `CreateWorkspaces` batch and return `(failed, pending)`.
async fn create_batch(router: &MultiServiceRouter, elements: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let (status, body) = call(router, "CreateWorkspaces", &json!({ "Workspaces": elements })).await;
    assert_eq!(
        status, 200,
        "a per-element refusal is still HTTP 200, body {body:?}"
    );
    let failed = body["FailedRequests"]
        .as_array()
        .unwrap_or_else(|| panic!("FailedRequests must be a list, got {body:?}"))
        .clone();
    let pending = body["PendingRequests"]
        .as_array()
        .unwrap_or_else(|| panic!("PendingRequests must be a list, got {body:?}"))
        .clone();
    (failed, pending)
}

/// The fixed message of `code`.
fn expected_message(code: &str) -> &'static str {
    match code {
        BUNDLE_NOT_FOUND => BUNDLE_NOT_FOUND_MESSAGE,
        DIRECTORY_NOT_FOUND => DIRECTORY_NOT_FOUND_MESSAGE,
        other => panic!("no fixed message for the per-item code {other}"),
    }
}

/// Whether `workspace_id` is reported by `DescribeWorkspaces`, the reader a client uses.
async fn is_described(router: &MultiServiceRouter, workspace_id: &str) -> bool {
    let (status, body) = call(
        router,
        "DescribeWorkspaces",
        &json!({ "WorkspaceIds": [workspace_id] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeWorkspaces({workspace_id}) must answer 200, body {body:?}"
    );
    body["Workspaces"]
        .as_array()
        .is_some_and(|workspaces| {
            workspaces
                .iter()
                .any(|workspace| workspace["WorkspaceId"].as_str() == Some(workspace_id))
        })
}

#[tokio::test]
async fn an_element_whose_bundle_does_not_resolve_is_reported_against_the_bundle() {
    let router = emulator();
    let bundles = stored_bundle_ids(&router).await;
    register_directory(&router, REGISTERED_DIRECTORY).await;

    // (case, directory, bundle, the expected per-item ErrorCode or None when satisfiable).
    // The rows span all four combinations of the two lookups, and vary the ids inside the
    // two published patterns.
    let cases: [(&str, Directory, Bundle, Option<&str>); 9] = [
        ("both resolve, first catalog bundle", Directory::Registered, Bundle::Stored(0), None),
        (
            "both resolve, last catalog bundle",
            Directory::Registered,
            Bundle::Stored(bundles.len() - 1),
            None,
        ),
        (
            // The pattern's 8-character lower bound, digits only.
            "bundle unheld, shortest id",
            Directory::Registered,
            Bundle::Unheld("wsb-00000000"),
            Some(BUNDLE_NOT_FOUND),
        ),
        (
            // The pattern's 63-character upper bound.
            "bundle unheld, longest id",
            Directory::Registered,
            Bundle::Unheld(
                "wsb-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            ),
            Some(BUNDLE_NOT_FOUND),
        ),
        (
            "bundle unheld, digits and letters mixed",
            Directory::Registered,
            Bundle::Unheld("wsb-0a1b2c3d4e5f6"),
            Some(BUNDLE_NOT_FOUND),
        ),
        (
            // Neither resolves: the bundle is resolved first, so the bundle is named.
            "neither resolves, d- directory form",
            Directory::Unregistered(UNREGISTERED_D_FORM),
            Bundle::Unheld("wsb-00000000"),
            Some(BUNDLE_NOT_FOUND),
        ),
        (
            "neither resolves, wsd- directory form",
            Directory::Unregistered(UNREGISTERED_WSD_FORM),
            Bundle::Unheld("wsb-0a1b2c3d4e5f6"),
            Some(BUNDLE_NOT_FOUND),
        ),
        (
            // Only the directory fails to resolve: the directory is named.
            "directory unregistered, d- form",
            Directory::Unregistered(UNREGISTERED_D_FORM),
            Bundle::Stored(0),
            Some(DIRECTORY_NOT_FOUND),
        ),
        (
            "directory unregistered, wsd- form",
            Directory::Unregistered(UNREGISTERED_WSD_FORM),
            Bundle::Stored(bundles.len() - 1),
            Some(DIRECTORY_NOT_FOUND),
        ),
    ];

    for (index, (case, directory, bundle, expected)) in cases.into_iter().enumerate() {
        let directory_id = match directory {
            Directory::Registered => REGISTERED_DIRECTORY.to_string(),
            Directory::Unregistered(id) => id.to_string(),
        };
        let bundle_id = match bundle {
            Bundle::Stored(at) => bundles[at].clone(),
            Bundle::Unheld(id) => id.to_string(),
        };
        // One user per row: a directory user holds at most one WorkSpace.
        let user_name = format!("user{index}");
        let element = json!({
            "DirectoryId": directory_id,
            "UserName": user_name,
            "BundleId": bundle_id,
        });
        let (failed, pending) = create_batch(&router, std::slice::from_ref(&element)).await;

        match expected {
            None => {
                assert!(
                    failed.is_empty(),
                    "case {case:?}: both references resolve, so nothing may fail, got {failed:?}"
                );
                assert_eq!(
                    pending.len(),
                    1,
                    "case {case:?}: the element must be created, got {pending:?}"
                );
                // The round trip: the created WorkSpace is what the reader reports.
                let workspace_id = pending[0]["WorkspaceId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a minted WorkspaceId, got {pending:?}"))
                    .to_string();
                assert!(
                    is_described(&router, &workspace_id).await,
                    "case {case:?}: DescribeWorkspaces must report the created {workspace_id}"
                );
            }
            Some(code) => {
                assert_eq!(
                    failed.len(),
                    1,
                    "case {case:?}: exactly one entry reports the element, got {failed:?}"
                );
                assert!(
                    pending.is_empty(),
                    "case {case:?}: nothing may be created for a refused element, got {pending:?}"
                );
                let actual_code = failed[0]["ErrorCode"].as_str();
                assert_eq!(
                    actual_code,
                    Some(code),
                    "case {case:?}: expected ErrorCode {code}, got {actual_code:?}"
                );
                let actual_message = failed[0]["ErrorMessage"].as_str();
                assert_eq!(
                    actual_message,
                    Some(expected_message(code)),
                    "case {case:?}: expected the fixed message of {code}, got {actual_message:?}"
                );
                let message = actual_message.unwrap_or_default();
                assert!(
                    !message.contains(&bundle_id) && !message.contains(&directory_id),
                    "case {case:?}: the fixed message must interpolate no id, got {message:?}"
                );
                // The refused element is echoed as submitted, so a client can retry it.
                assert_eq!(
                    failed[0]["WorkspaceRequest"]["BundleId"].as_str(),
                    Some(bundle_id.as_str()),
                    "case {case:?}: the entry echoes the BundleId it was asked about, got {failed:?}"
                );
                assert_eq!(
                    failed[0]["WorkspaceRequest"]["DirectoryId"].as_str(),
                    Some(directory_id.as_str()),
                    "case {case:?}: the entry echoes the DirectoryId it was asked about, got \
                     {failed:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn each_element_of_a_batch_is_judged_and_reported_on_its_own() {
    // The same four kinds of element, submitted together: the batch's size and the place
    // each element sits in it change nothing about which resource its entry names, and the
    // satisfiable elements beside them are still created.
    let orders: [(&str, [usize; 4]); 3] = [
        ("submitted order", [0, 1, 2, 3]),
        ("refusals first", [1, 2, 3, 0]),
        ("refusals last", [0, 3, 2, 1]),
    ];

    for (case, order) in orders {
        let router = emulator();
        let bundles = stored_bundle_ids(&router).await;
        register_directory(&router, REGISTERED_DIRECTORY).await;

        // index 0 is satisfiable; 1 names no stored bundle; 2 names an unregistered
        // directory; 3 resolves neither.
        let kinds: [(Value, Option<&str>); 4] = [
            (
                json!({
                    "DirectoryId": REGISTERED_DIRECTORY,
                    "UserName": "satisfiable",
                    "BundleId": bundles[0],
                }),
                None,
            ),
            (
                json!({
                    "DirectoryId": REGISTERED_DIRECTORY,
                    "UserName": "no_bundle",
                    "BundleId": "wsb-00000000",
                }),
                Some(BUNDLE_NOT_FOUND),
            ),
            (
                json!({
                    "DirectoryId": UNREGISTERED_WSD_FORM,
                    "UserName": "no_directory",
                    "BundleId": bundles[bundles.len() - 1],
                }),
                Some(DIRECTORY_NOT_FOUND),
            ),
            (
                json!({
                    "DirectoryId": UNREGISTERED_D_FORM,
                    "UserName": "neither",
                    "BundleId": "wsb-0a1b2c3d4e5f6",
                }),
                Some(BUNDLE_NOT_FOUND),
            ),
        ];

        let elements: Vec<Value> = order.iter().map(|at| kinds[*at].0.clone()).collect();
        let expected_codes: Vec<&str> = order.iter().filter_map(|at| kinds[*at].1).collect();

        let (failed, pending) = create_batch(&router, &elements).await;
        let actual_codes: Vec<String> = failed
            .iter()
            .map(|entry| entry["ErrorCode"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            actual_codes, expected_codes,
            "case {case:?}: each entry must name the resource its own element failed on, got \
             {failed:?}"
        );
        assert_eq!(
            pending.len(),
            1,
            "case {case:?}: only the satisfiable element may be created, got {pending:?}"
        );

        // And the refused elements persisted nothing a later read can see.
        let (status, body) = call(&router, "DescribeWorkspaces", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeWorkspaces must answer 200, body {body:?}"
        );
        let described: Vec<String> = body["Workspaces"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: Workspaces must be a list, got {body:?}"))
            .iter()
            .map(|workspace| {
                workspace["UserName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            described,
            vec!["satisfiable".to_string()],
            "case {case:?}: exactly the satisfiable element is held, got {described:?}"
        );
    }
}
