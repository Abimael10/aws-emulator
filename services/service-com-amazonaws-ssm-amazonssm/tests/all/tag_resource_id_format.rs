// SPDX-License-Identifier: CC0-1.0

//! What a tag request's `ResourceId` has to LOOK LIKE for the `ResourceType` it declares.
//!
//! A tag request addresses a resource by `(ResourceType, ResourceId)`
//! (`AddTagsToResourceRequest$ResourceId`), and the two refusals that pair can draw are
//! different questions:
//!
//! 1. **The id is not of that type's form at all** — a format check on the REQUEST, taken
//!    before anything is looked up: HTTP 400 `ValidationException` naming the form the type
//!    requires and the type the request declared. An id of another type's form names no
//!    resource of this type whatever the Region holds, so the answer cannot depend on the
//!    store.
//! 2. **The id is well formed and names nothing** — the separate `InvalidResourceId`, whose
//!    modelled shape carries no message member at all.
//!
//! Every operation of the tag plane is held to both, and a refused request writes nothing.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`error_code`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.
//!
//! The suites are tables: the rows differ along the resource type declared, the form the
//! submitted id actually has (another type's prefix, a prefix with too few digits, the
//! wrong alphabet, a hyphenated group count), and which operation of the plane is called —
//! rather than the single pair a recorded unit happened to send.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router};

/// The id forms this service publishes for the tag types whose ids have one
/// (`service_com_amazonaws_ssm_amazonssm::state::tagging::resource_id_format`), written as
/// the refusal states them.
const MAINTENANCE_WINDOW_FORMAT: &str = "mw-[0-9a-f]{17}";

/// See [`MAINTENANCE_WINDOW_FORMAT`].
const PATCH_BASELINE_FORMAT: &str = "pb-[0-9a-f]{17}";

/// See [`MAINTENANCE_WINDOW_FORMAT`].
const OPS_ITEM_FORMAT: &str = "oi-[0-9a-f]{12}";

/// See [`MAINTENANCE_WINDOW_FORMAT`].
const ASSOCIATION_FORMAT: &str =
    "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";

/// See [`MAINTENANCE_WINDOW_FORMAT`].
const CLOUD_CONNECTOR_FORMAT: &str =
    "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";

/// The sentence a mis-shaped id is refused with: the form the declared type requires, then
/// that type.
fn format_message(format: &str, resource_type: &str) -> String {
    format!("resourceId should be in format {format} for resource type {resource_type}")
}

/// One tag call of each operation of the plane, for the pair a row names.
async fn tag_calls(
    router: &MultiServiceRouter,
    resource_type: &str,
    resource_id: &str,
) -> Vec<(&'static str, (u16, Value))> {
    vec![
        (
            "AddTagsToResource",
            call(
                router,
                "AddTagsToResource",
                &json!({
                    "ResourceType": resource_type,
                    "ResourceId": resource_id,
                    "Tags": [{ "Key": "owner", "Value": "beta" }],
                }),
            )
            .await,
        ),
        (
            "ListTagsForResource",
            call(
                router,
                "ListTagsForResource",
                &json!({ "ResourceType": resource_type, "ResourceId": resource_id }),
            )
            .await,
        ),
        (
            "RemoveTagsFromResource",
            call(
                router,
                "RemoveTagsFromResource",
                &json!({
                    "ResourceType": resource_type,
                    "ResourceId": resource_id,
                    "TagKeys": ["owner"],
                }),
            )
            .await,
        ),
    ]
}

/// RULE: an id that is not of the declared `ResourceType`'s own form is refused
/// `ValidationException` naming that form — by every operation of the tag plane, and before
/// any lookup, so the answer is the same whether or not the Region holds anything.
#[tokio::test]
async fn a_mismatched_resource_id_is_a_validation_refusal_naming_the_types_form() {
    let router = router();

    // (case, resource type, the id sent, the form the refusal must name)
    let rows: [(&str, &str, &str, &str); 9] = [
        (
            "a window id under the patch baseline type",
            "PatchBaseline",
            "mw-0123456789abcdef0",
            PATCH_BASELINE_FORMAT,
        ),
        (
            "a baseline id under the window type",
            "MaintenanceWindow",
            "pb-0123456789abcdef0",
            MAINTENANCE_WINDOW_FORMAT,
        ),
        (
            "a window id of too few digits",
            "MaintenanceWindow",
            "mw-012345abcde",
            MAINTENANCE_WINDOW_FORMAT,
        ),
        (
            "a window id in upper case",
            "MaintenanceWindow",
            "mw-0123456789ABCDEF0",
            MAINTENANCE_WINDOW_FORMAT,
        ),
        (
            "a baseline id carrying a digit too many",
            "PatchBaseline",
            "pb-0123456789abcdef01",
            PATCH_BASELINE_FORMAT,
        ),
        (
            "a UUID under the OpsItem type",
            "OpsItem",
            "aa112233-4455-6677-8899-aabbccddeeff",
            OPS_ITEM_FORMAT,
        ),
        (
            "an OpsItem id of too many digits",
            "OpsItem",
            "oi-0123456789abcd",
            OPS_ITEM_FORMAT,
        ),
        (
            "prose under the association type",
            "Association",
            "not-an-association-id",
            ASSOCIATION_FORMAT,
        ),
        (
            "a cloud connector id in upper case",
            "CloudConnector",
            "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
            CLOUD_CONNECTOR_FORMAT,
        ),
    ];

    for (case, resource_type, resource_id, format) in rows {
        let expected = format_message(format, resource_type);
        for (operation, (status, response)) in tag_calls(&router, resource_type, resource_id).await
        {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse a mis-shaped id, got {status} \
                 {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: {operation} must answer the format refusal, got {response}"
            );
            let message = error_message(&response);
            assert_eq!(
                message, expected,
                "case {case:?}: {operation} expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// RULE, the other half: an id that HAS its type's form but names nothing is the separate
/// `InvalidResourceId`, which carries no message. The rows differ along the type and along
/// which published form the id takes.
#[tokio::test]
async fn a_well_formed_id_naming_nothing_is_invalid_resource_id() {
    let router = router();

    // (case, resource type, a well-formed id of that type that names nothing)
    let rows: [(&str, &str, &str); 5] = [
        (
            "a window id nothing created",
            "MaintenanceWindow",
            "mw-00000000000000000",
        ),
        (
            "a baseline id nothing created",
            "PatchBaseline",
            "pb-00000000000000000",
        ),
        ("an OpsItem id nothing created", "OpsItem", "oi-0123456789ab"),
        (
            "an association id nothing created",
            "Association",
            "aa112233-4455-6677-8899-aabbccddeeff",
        ),
        (
            "a connector id nothing created",
            "CloudConnector",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ),
    ];

    for (case, resource_type, resource_id) in rows {
        for (operation, (status, response)) in tag_calls(&router, resource_type, resource_id).await
        {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse an id naming nothing, got {status} \
                 {response}"
            );
            assert_eq!(
                error_code(&response),
                "InvalidResourceId",
                "case {case:?}: {operation} must answer the existence refusal, got {response}"
            );
            assert_eq!(
                error_message(&response),
                "",
                "case {case:?}: {operation} must carry no message, got {response}"
            );
        }
    }
}

/// A resource type whose "ID of the resource" is a NAME or an opaque id the service
/// publishes no form for constrains nothing ahead of the lookup, so such a pair is still
/// answered by whether the Region holds the resource.
#[tokio::test]
async fn a_type_whose_ids_have_no_published_form_is_answered_by_the_lookup() {
    let router = router();

    // (case, resource type, an id of no particular form)
    let rows: [(&str, &str, &str); 4] = [
        ("a document name", "Document", "ADocumentNobodyCreated"),
        ("a parameter name", "Parameter", "/Tags/NobodyPutThis"),
        (
            "an automation execution id",
            "Automation",
            "example-c160-4567-8519-012345abcde",
        ),
        (
            "a managed node id",
            "ManagedInstance",
            "mi-1a2b3c4d5e6f78901",
        ),
    ];

    for (case, resource_type, resource_id) in rows {
        for (operation, (status, response)) in tag_calls(&router, resource_type, resource_id).await
        {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse an id naming nothing, got {status} \
                 {response}"
            );
            assert_eq!(
                error_code(&response),
                "InvalidResourceId",
                "case {case:?}: {operation} must answer the existence refusal rather than the \
                 format one, got {response}"
            );
        }
    }
}

/// A call refused for the id's form writes NOTHING: the tags the resource really carries are
/// what a later read reports, whichever type the refused call declared.
#[tokio::test]
async fn a_format_refusal_leaves_the_resources_tags_as_they_were() {
    let router = router();

    // A window exists and carries a tag, written through the front door.
    let (status, created) = call(
        &router,
        "CreateMaintenanceWindow",
        &json!({
            "Name": "tag-format-window",
            "Schedule": "cron(0 16 ? * TUE *)",
            "Duration": 3,
            "Cutoff": 1,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must be created first, got {status} {created}"
    );
    let window_id = created["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a WindowId, got {created}"))
        .to_string();
    let (status, tagged) = call(
        &router,
        "AddTagsToResource",
        &json!({
            "ResourceType": "MaintenanceWindow",
            "ResourceId": window_id,
            "Tags": [{ "Key": "owner", "Value": "alpha" }],
        }),
    )
    .await;
    assert_eq!(status, 200, "the window must be tagged, got {status} {tagged}");

    // The window's own id under another type's name is the format refusal, and it changes
    // nothing: the id is a real one, so only the DECLARED type makes it mis-shaped.
    for (case, resource_type, format) in [
        ("the patch baseline type", "PatchBaseline", PATCH_BASELINE_FORMAT),
        ("the OpsItem type", "OpsItem", OPS_ITEM_FORMAT),
    ] {
        let expected = format_message(format, resource_type);
        for (operation, (status, response)) in tag_calls(&router, resource_type, &window_id).await {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse the window's id, got {status} \
                 {response}"
            );
            assert_eq!(
                error_message(&response),
                expected,
                "case {case:?}: {operation} expected {expected:?}, got {response}"
            );
        }
    }

    let (status, listed) = call(
        &router,
        "ListTagsForResource",
        &json!({ "ResourceType": "MaintenanceWindow", "ResourceId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window's own tag listing must still be served, got {status} {listed}"
    );
    assert_eq!(
        listed["TagList"],
        json!([{ "Key": "owner", "Value": "alpha" }]),
        "the refused calls must have left the window's tags as they were, got {listed}"
    );

    // And the Region the window lives in is the one its tags are read through: the same
    // well-formed id at another endpoint names no window.
    let (status, elsewhere) = call_in_region(
        &router,
        "eu-central-1",
        "ListTagsForResource",
        &json!({ "ResourceType": "MaintenanceWindow", "ResourceId": window_id }),
    )
    .await;
    assert_eq!(
        status, 400,
        "another Region holds no such window, got {status} {elsewhere}"
    );
    assert_eq!(
        error_code(&elsewhere),
        "InvalidResourceId",
        "another Region's endpoint must answer the existence refusal, got {elsewhere}"
    );
}
