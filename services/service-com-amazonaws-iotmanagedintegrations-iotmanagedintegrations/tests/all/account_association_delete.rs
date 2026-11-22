// SPDX-License-Identifier: CC0-1.0

//! Removing an account association, end to end over the wire: `CreateConnectorDestination`
//! -> `CreateAccountAssociation` -> `DeleteAccountAssociation` -> `GetAccountAssociation` /
//! `ListAccountAssociations` -> `CreateAccountAssociation` again.
//!
//! RULE: `DeleteAccountAssociation` (DELETE `/account-associations/{AccountAssociationId}`,
//! success 200, `@idempotent`) removes the account association the signing region holds under
//! that identifier: the reads stop reporting it, the connector destination it was created
//! against is free to start a new authorization flow, and a `ClientToken` that minted the
//! removed record no longer replays into it. It promises that end state rather than that this
//! call reached it, so an identifier this region holds no association under — never created,
//! deleted since, or created in another region — removes nothing and answers that SAME
//! success, with no error code and no body; the modelled `ResourceNotFoundException` (HTTP
//! 404) is what this resource's RESOLVING paths (the read, the refresh) answer for it.
//!
//! The rows below differ along what the rule generalises over: the region the calls are
//! signed for, the members the removed record carried, the form of an identifier that
//! resolves to nothing, and whether the create that follows the delete reuses the removed
//! record's `ClientToken`.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the routes, statuses, bodies and modelled errors asserted here are the ones a client
//! sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    ACCOUNT_ASSOCIATION_RESOURCE_TYPE, INITIAL_ASSOCIATION_STATE,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The routes this suite drives
/// (`smithy_query(query="operation",operation="DeleteAccountAssociation")` -> DELETE
/// `/account-associations/{AccountAssociationId}`, `CreateAccountAssociation` -> POST
/// `/account-associations`, `CreateConnectorDestination` -> POST `/connector-destinations`,
/// `CreateCloudConnector` -> POST `/cloud-connectors`).
const COLLECTION: &str = "/account-associations";
const CONNECTOR_DESTINATIONS: &str = "/connector-destinations";
const CLOUD_CONNECTORS: &str = "/cloud-connectors";

/// The wire error code the model's `@httpError` 404 shape is rendered under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence this resource's 404s carry, read and mutation alike, as
/// `GetAccountAssociation` is recorded wording the absence of an account association.
const NOT_FOUND_MESSAGE: &str = "Account association not found.";

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// The reason an error body carries, on the member the model declares for it.
    fn message(&self) -> &str {
        self.string("Message")
            .or_else(|| self.string("message"))
            .unwrap_or_default()
    }

    /// The ids a list answer reports, in the order it reported them.
    fn item_ids(&self) -> Vec<String> {
        self.member("Items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.get("AccountAssociationId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// `CreateAccountAssociation` against `connector_destination_id` carrying `members`, signed
/// for `region`.
async fn create_in(
    router: &MultiServiceRouter,
    region: &str,
    connector_destination_id: &str,
    members: Value,
) -> Reply {
    let mut body = members;
    body["ConnectorDestinationId"] = json!(connector_destination_id);
    call_in(router, region, "POST", COLLECTION, Some(body)).await
}

/// `DeleteAccountAssociation` of `id`, signed for `region`.
async fn delete_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "DELETE", &format!("{COLLECTION}/{id}"), None).await
}

/// `GetAccountAssociation` of `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// `ListAccountAssociations`, signed for `region`.
async fn list_in(router: &MultiServiceRouter, region: &str) -> Reply {
    call_in(router, region, "GET", COLLECTION, None).await
}

/// Create a connector destination in `region` through the wire and answer the
/// `ConnectorDestinationId` it minted.
///
/// `CreateAccountAssociation` resolves its `ConnectorDestinationId` against this service's
/// own connector-destination store, so every association below is created against a
/// destination a client really created; a connector destination in turn names a C2C
/// connector (`CloudConnectorId` is `@required`), so the connector comes first.
async fn connector_destination_in(router: &MultiServiceRouter, region: &str, case: &str) -> String {
    let connector = call_in(
        router,
        region,
        "POST",
        CLOUD_CONNECTORS,
        Some(json!({
            "Name": "partner cloud connector",
            "EndpointConfig": {"lambda": {"arn": "connector-handler"}},
        })),
    )
    .await;
    assert_eq!(
        connector.status, 201,
        "case {case:?}: expected HTTP 201 from CreateCloudConnector, got {} with body {:?}",
        connector.status, connector.body
    );
    let destination = call_in(
        router,
        region,
        "POST",
        CONNECTOR_DESTINATIONS,
        Some(json!({
            "CloudConnectorId": connector.string("Id").unwrap_or_default(),
            "AuthConfig": {},
            // The ARN of the secret a destination keeps its credentials in, inside the
            // `SecretsManagerArn` `@pattern`
            // (`^arn:aws:secretsmanager:[0-9a-zA-Z-]{1,32}:\d{12}:secret:[A-Za-z0-9/_+=.@-]{8,520}$`)
            // and the `SecretsManagerVersionId` `@length` (32..64).
            "SecretsManager": {
                "arn": format!(
                    "arn:aws:secretsmanager:{region}:{}:secret:destination-A1b2C3",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "versionId": "a".repeat(32),
            },
        })),
    )
    .await;
    assert_eq!(
        destination.status, 201,
        "case {case:?}: expected HTTP 201 from CreateConnectorDestination, got {} with \
         body {:?}",
        destination.status, destination.body
    );
    destination
        .string("Id")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected CreateConnectorDestination to answer an Id, got \
                 body {:?}",
                destination.body
            )
        })
        .to_string()
}

/// Assert one create was accepted and answer the `AccountAssociationId` it minted.
fn accepted_id(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201, got {} with body {:?}",
        reply.status, reply.body
    );
    reply
        .string("AccountAssociationId")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an AccountAssociationId, got body {:?}",
                reply.body
            )
        })
        .to_string()
}

/// Assert `reply` is this delete's modelled success: the bound status, no error code, and no
/// body at all (the output shape is `Unit`).
fn assert_delete_succeeded(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected HTTP 200 from the delete, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type, None,
        "case {case:?}: a successful delete carries no error code, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.body,
        Value::Null,
        "case {case:?}: the modelled output has no members, so the body is empty, got {:?}",
        reply.body
    );
}

/// Assert `reply` is the modelled 404 of this resource: the code, the absence sentence, and
/// the `ResourceId` / `ResourceType` members the recorded body carries. Used for the READS
/// this suite performs: the delete itself answers an absent identifier as a success.
fn assert_not_found(case: &str, reply: &Reply, identifier: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected HTTP 404, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected the modelled {NOT_FOUND} code, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.message(),
        NOT_FOUND_MESSAGE,
        "case {case:?}: expected the sentence {NOT_FOUND_MESSAGE:?}, got {:?}",
        reply.message()
    );
    assert_eq!(
        reply.string("ResourceId"),
        Some(identifier),
        "case {case:?}: expected the 404 to report the identifier the request carried, \
         got {:?}",
        reply.string("ResourceId")
    );
    assert_eq!(
        reply.string("ResourceType"),
        Some(ACCOUNT_ASSOCIATION_RESOURCE_TYPE),
        "case {case:?}: expected the 404 to report the resource type, got {:?}",
        reply.string("ResourceType")
    );
}

/// RULE: a delete removes the record, so the reads stop reporting it and the connector
/// destination it was created against takes a create again — in every region, whatever
/// members the removed record carried.
#[tokio::test]
async fn a_delete_removes_the_record_and_frees_its_destination() {
    // The rows differ along the region the calls are signed for and along the members the
    // removed record carried.
    let rows: Vec<(&str, Value)> = vec![
        ("us-east-1", json!({})),
        ("eu-central-1", json!({"Name": "Partner account"})),
        (
            "ap-southeast-2",
            json!({
                "Name": "Partner account",
                "Description": "the end user account",
                "Tags": {"env": "test"},
            }),
        ),
    ];

    for (region, members) in rows {
        let case = format!("region {region}");
        let router = emulator();
        let destination = connector_destination_in(&router, region, &case).await;
        let held = accepted_id(
            &case,
            &create_in(&router, region, &destination, members).await,
        );

        assert_delete_succeeded(&case, &delete_in(&router, region, &held).await);

        // The reads no longer hold it.
        assert_not_found(&case, &get_in(&router, region, &held).await, &held);
        assert!(
            list_in(&router, region).await.item_ids().is_empty(),
            "case {case:?}: expected the list to report no association after the delete, \
             got {:?}",
            list_in(&router, region).await.body
        );

        // A second delete of the same identifier holds nothing to remove, which is the end
        // state it asks for, so it answers the same success as the first.
        assert_delete_succeeded(
            &format!("{case}, deleted twice"),
            &delete_in(&router, region, &held).await,
        );
        assert_not_found(
            &format!("{case}, read after the second delete"),
            &get_in(&router, region, &held).await,
            &held,
        );

        // The destination is waiting for no authorization flow now, so the create that
        // starts the next one is accepted.
        let next = accepted_id(
            &format!("{case}, after the delete"),
            &create_in(&router, region, &destination, json!({})).await,
        );
        assert_ne!(
            next, held,
            "case {case:?}: the create after a delete mints its own identifier"
        );
        let read = get_in(&router, region, &next).await;
        assert_eq!(
            read.string("AssociationState"),
            Some(INITIAL_ASSOCIATION_STATE),
            "case {case:?}: expected the new association to report \
             {INITIAL_ASSOCIATION_STATE:?}, got {:?}",
            read.string("AssociationState")
        );
        assert_eq!(
            list_in(&router, region).await.item_ids(),
            vec![next.clone()],
            "case {case:?}: expected the store to hold exactly the new association"
        );
    }
}

/// RULE: a delete resolves ONE region's association under the identifier it carries, so an
/// identifier this region holds no association under removes nothing and answers the
/// modelled success — whatever the form of that identifier, and including one another
/// region's endpoint really minted.
#[tokio::test]
async fn a_delete_of_an_association_this_region_does_not_hold_removes_nothing() {
    let router = emulator();
    let destination = connector_destination_in(&router, REGION, "the destination").await;
    let held = accepted_id(
        "the held association",
        &create_in(&router, REGION, &destination, json!({})).await,
    );

    // An association another region's endpoint really minted, which this region resolves no
    // record under.
    let elsewhere_destination =
        connector_destination_in(&router, "eu-central-1", "another region").await;
    let elsewhere = accepted_id(
        "another region",
        &create_in(&router, "eu-central-1", &elsewhere_destination, json!({})).await,
    );

    // The rows differ along the form of an identifier that resolves to nothing: one inside
    // the `AccountAssociationId` pattern (len 1..64, `^[0-9a-zA-Z]+$`) at each bound, mixed
    // case, digits only, the held id with a character appended, and another region's real
    // identifier.
    let absent: Vec<(&str, String)> = vec![
        ("one character", "a".to_string()),
        ("at the length maximum", "b".repeat(64)),
        ("mixed case", "NoSuchAssociation1".to_string()),
        ("digits only", "1234567890".to_string()),
        ("an extension of the held id", format!("{held}x")),
        ("another region's association", elsewhere.clone()),
    ];
    for (case, identifier) in absent {
        assert_delete_succeeded(case, &delete_in(&router, REGION, &identifier).await);
    }

    // Nothing was removed: both regions still report their own association.
    assert_eq!(
        list_in(&router, REGION).await.item_ids(),
        vec![held.clone()],
        "expected this region to still hold its association"
    );
    assert_eq!(
        list_in(&router, "eu-central-1").await.item_ids(),
        vec![elsewhere.clone()],
        "expected the other region to still hold its association"
    );
}

/// RULE: the idempotency record of a removed association goes with it, so a retry carrying
/// the `ClientToken` that minted it no longer replays into the deleted identifier — it
/// starts a new authorization flow instead.
#[tokio::test]
async fn a_client_token_does_not_replay_into_a_deleted_association() {
    let router = emulator();
    let destination = connector_destination_in(&router, REGION, "the destination").await;

    // The `ClientToken` pattern is `^[a-zA-Z0-9=_-]+$`, length 1..64.
    for token in ["t", "create-token-1", "=_-", &"z".repeat(64)] {
        let case = format!("token {token:?}");
        let first = accepted_id(
            &case,
            &create_in(
                &router,
                REGION,
                &destination,
                json!({"ClientToken": token}),
            )
            .await,
        );
        assert_delete_succeeded(&case, &delete_in(&router, REGION, &first).await);

        let retried = accepted_id(
            &format!("{case}, retried after the delete"),
            &create_in(
                &router,
                REGION,
                &destination,
                json!({"ClientToken": token}),
            )
            .await,
        );
        assert_ne!(
            retried, first,
            "case {case:?}: a token whose association was deleted replays into nothing, so \
             the retry mints its own identifier"
        );
        assert_not_found(&case, &get_in(&router, REGION, &first).await, &first);

        // Leave the destination free for the next row.
        assert_delete_succeeded(
            &format!("{case}, removing the retry's association"),
            &delete_in(&router, REGION, &retried).await,
        );
    }
}
