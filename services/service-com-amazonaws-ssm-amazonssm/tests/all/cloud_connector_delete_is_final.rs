// SPDX-License-Identifier: CC0-1.0

//! What `DeleteCloudConnector` resolves, and what the connector's id names afterwards.
//!
//! Two rules are asserted here, both about the delete rather than about any one connector:
//!
//! 1. the only question a delete resolves is whether the Region the request was signed for
//!    holds a connector under the `CloudConnectorId` it names ("Deletes a cloud connector",
//!    `smithy_query(query="doc", shape="DeleteCloudConnector")`), so a write that already
//!    landed on that connector — a display name, a description or a whole replaced
//!    configuration, applied once or twice — leaves the delete served and the modelled
//!    `ConflictException` unreported: the delete answers the id it removed and the connector
//!    leaves the Region's listing.
//! 2. a delete is FINAL: the id it removed names no connector afterwards, so every operation
//!    of this service that resolves a connector id — the delete itself, the single-connector
//!    read, the update and the validation — is refused HTTP 400 `ResourceNotFoundException`
//!    with the one sentence for that class, never a conflict.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`])
//! — and every connector is minted through `CreateCloudConnector` against a role IAM itself
//! holds, through the helper the sibling delete suite owns
//! ([`crate::cloud_connector_role_and_deletion::create_connector`]); nothing reaches into a
//! store and no second router is written here.
//!
//! The rows span what the rules generalise over: which members a preceding write replaced and
//! how many writes there were, which Region holds the connector, and which operation resolves
//! the id after it is gone.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_connector_role_and_deletion::create_connector;
use crate::cloud_connector_validation::listed;
use crate::hybrid_activation_role::router_with_iam;
use crate::parameter_store::{call_in_region, error_code, error_message, REGION};

/// A second Region, so the rules are asserted at more than the one endpoint.
const OTHER_REGION: &str = "us-east-1";

/// The sentence a connector id that names no connector is refused with, written out here
/// independently of the service: fixed text for the class, naming the resource class and
/// interpolating nothing from the request.
const NOT_FOUND: &str = "CloudConnector not found for account.";

/// An Azure tenant id a replacing `Configuration` targets: `AzureConfiguration$TenantId` is a
/// hex UUID, and a different one from the create's makes the replacement observable.
const REPLACEMENT_TENANT_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// See [`REPLACEMENT_TENANT_ID`]: `AzureConfiguration$ApplicationId` has the same shape.
const REPLACEMENT_APPLICATION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// A `Configuration` naming another tenant and application, which an update REPLACES the
/// stored one with.
fn replacement_configuration() -> Value {
    json!({
        "AzureConfiguration": {
            "TenantId": REPLACEMENT_TENANT_ID,
            "ApplicationId": REPLACEMENT_APPLICATION_ID,
        },
    })
}

/// Apply one update to `id` at `region`, where `members` carries the update's optional
/// members (`DisplayName`, `Description`, `Configuration` —
/// `smithy_query(query="operation", operation="UpdateCloudConnector")`).
async fn update(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    id: &str,
    members: &Value,
) {
    let mut body = members.clone();
    body["CloudConnectorId"] = json!(id);
    let (status, updated) = call_in_region(router, region, "UpdateCloudConnector", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update {body} must be served, got {status} with {updated}"
    );
    assert_eq!(
        updated["CloudConnectorId"].as_str(),
        Some(id),
        "case {case:?}: the update reports the connector it wrote to, got {updated}"
    );
}

/// `GetCloudConnector` of `id` at `region`, which must be served.
async fn read(router: &MultiServiceRouter, case: &str, region: &str, id: &str) -> Value {
    let (status, body) = call_in_region(
        router,
        region,
        "GetCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {body}"
    );
    body
}

/// RULE: a delete resolves only whether this Region holds a connector under the id, so a
/// write that already landed on that connector does not make the delete a conflict — it is
/// served, reports the id it removed, and the connector leaves the Region's listing.
#[tokio::test]
async fn a_delete_is_served_whatever_write_preceded_it() {
    /// One row: the writes applied to a freshly minted connector before it is deleted.
    struct Row {
        case: &'static str,
        /// The Region the connector is created, updated and deleted through.
        region: &'static str,
        /// The IAM role the create names, one per row so each create resolves its own.
        role: &'static str,
        /// The updates to apply in order, each one update's optional members.
        updates: Vec<Value>,
        /// The `DisplayName` the read must report before the delete, or `None` to leave
        /// whatever the create persisted.
        display_name: Option<&'static str>,
        /// The `Description` the read must report before the delete, or `None` for the same.
        description: Option<&'static str>,
    }

    let rows = vec![
        Row {
            case: "no write between the create and the delete",
            region: REGION,
            role: "SsmUntouchedRole",
            updates: vec![],
            display_name: None,
            description: None,
        },
        Row {
            case: "a display name replaced",
            region: REGION,
            role: "SsmRenamedRole",
            updates: vec![json!({ "DisplayName": "Azure tenant, renamed" })],
            display_name: Some("Azure tenant, renamed"),
            description: None,
        },
        Row {
            case: "a description added",
            region: REGION,
            role: "SsmDescribedRole",
            updates: vec![json!({ "Description": "the production tenant" })],
            display_name: None,
            description: Some("the production tenant"),
        },
        Row {
            case: "a display name and a description in one update",
            region: REGION,
            role: "SsmBothMembersRole",
            updates: vec![json!({
                "DisplayName": "Azure tenant, both",
                "Description": "renamed and described at once",
            })],
            display_name: Some("Azure tenant, both"),
            description: Some("renamed and described at once"),
        },
        Row {
            case: "a whole configuration replaced",
            region: REGION,
            role: "SsmReconfiguredRole",
            updates: vec![json!({ "Configuration": replacement_configuration() })],
            display_name: None,
            description: None,
        },
        Row {
            case: "two updates in a row, the second replacing the first's name",
            region: REGION,
            role: "SsmTwiceWrittenRole",
            updates: vec![
                json!({ "DisplayName": "Azure tenant, first" }),
                json!({
                    "DisplayName": "Azure tenant, second",
                    "Description": "written twice",
                }),
            ],
            display_name: Some("Azure tenant, second"),
            description: Some("written twice"),
        },
        Row {
            case: "a connector written and deleted through another Region",
            region: OTHER_REGION,
            role: "SsmElsewhereWrittenRole",
            updates: vec![json!({ "DisplayName": "Azure tenant, elsewhere" })],
            display_name: Some("Azure tenant, elsewhere"),
            description: None,
        },
    ];

    for row in rows {
        let case = row.case;
        // A router per row, so each row's stores hold only its own role and connector.
        let router = router_with_iam();
        let id = create_connector(&router, case, row.region, row.role).await;
        let created = read(&router, case, row.region, &id).await;

        for members in &row.updates {
            update(&router, case, row.region, &id, members).await;
        }

        // What the writes left: the read is where a later caller sees them, and it is the
        // same connector the delete below resolves.
        let written = read(&router, case, row.region, &id).await;
        let expected_display = row
            .display_name
            .map(str::to_string)
            .or_else(|| created["DisplayName"].as_str().map(str::to_string));
        assert_eq!(
            written["DisplayName"].as_str().map(str::to_string),
            expected_display,
            "case {case:?}: expected the display name {expected_display:?}, got {written}"
        );
        let expected_description = row
            .description
            .map(str::to_string)
            .or_else(|| created["Description"].as_str().map(str::to_string));
        assert_eq!(
            written["Description"].as_str().map(str::to_string),
            expected_description,
            "case {case:?}: expected the description {expected_description:?}, got {written}"
        );
        if row
            .updates
            .iter()
            .any(|members| members.get("Configuration").is_some())
        {
            assert_eq!(
                written["Configuration"]["AzureConfiguration"]["TenantId"].as_str(),
                Some(REPLACEMENT_TENANT_ID),
                "case {case:?}: the replacing configuration is what the read reports, got \
                 {written}"
            );
        }

        let (status, deleted) = call_in_region(
            &router,
            row.region,
            "DeleteCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must be served after the writes, got {status} with \
             {deleted}"
        );
        assert_eq!(
            deleted["CloudConnectorId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the delete reports the id it removed, got {deleted}"
        );
        assert!(
            listed(&router, row.region).await.is_empty(),
            "case {case:?}: the deleted connector leaves the Region's listing, which still \
             reports {:?}",
            listed(&router, row.region).await
        );
    }
}

/// RULE: a delete is final — the id it removed names no connector, so every operation that
/// resolves a connector id is refused `ResourceNotFoundException` with the one sentence for
/// the class, whatever write preceded the delete.
#[tokio::test]
async fn every_operation_resolving_a_deleted_connector_is_refused_not_found() {
    /// One row: the operation put to the id after the delete, and the members it carries
    /// beside that id.
    struct Row {
        case: &'static str,
        operation: &'static str,
        members: Value,
    }

    let rows = vec![
        Row {
            case: "the delete again",
            operation: "DeleteCloudConnector",
            members: json!({}),
        },
        Row {
            case: "the single-connector read",
            operation: "GetCloudConnector",
            members: json!({}),
        },
        Row {
            case: "an update of the display name",
            operation: "UpdateCloudConnector",
            members: json!({ "DisplayName": "Azure tenant, after the delete" }),
        },
        Row {
            case: "an update replacing the configuration",
            operation: "UpdateCloudConnector",
            members: json!({ "Configuration": replacement_configuration() }),
        },
        Row {
            case: "the validation",
            operation: "ValidateCloudConnector",
            members: json!({}),
        },
    ];

    for region in [REGION, OTHER_REGION] {
        for row in &rows {
            let case = row.case;
            let router = router_with_iam();
            let id = create_connector(&router, case, region, "SsmDeletedConnectorRole").await;
            // One update lands first, so the id the rows name is one this service minted,
            // wrote to and then removed.
            update(
                &router,
                case,
                region,
                &id,
                &json!({ "Description": "written before the delete" }),
            )
            .await;
            let (status, deleted) = call_in_region(
                &router,
                region,
                "DeleteCloudConnector",
                &json!({ "CloudConnectorId": id }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} at {region}: the delete must be served, got {status} with \
                 {deleted}"
            );

            let mut body = row.members.clone();
            body["CloudConnectorId"] = json!(id);
            let (status, refused) = call_in_region(&router, region, row.operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?} at {region}: {} must be refused, got {status} with {refused}",
                row.operation
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?} at {region}: expected ResourceNotFoundException, got {refused}"
            );
            assert_eq!(
                error_message(&refused),
                NOT_FOUND,
                "case {case:?} at {region}: expected {NOT_FOUND:?}, got {:?}",
                error_message(&refused)
            );
        }
    }
}
