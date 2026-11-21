// SPDX-License-Identifier: CC0-1.0

//! A second `CreateAccountAssociation` against a connector destination whose authorization
//! flow is still outstanding, end to end over the wire: `CreateConnectorDestination` ->
//! `CreateAccountAssociation` -> `CreateAccountAssociation` again ->
//! `GetAccountAssociation` / `ListAccountAssociations`.
//!
//! RULE: a create STARTS an authorization flow against the connector destination it names —
//! it answers the authorization request an end user completes at the third-party platform —
//! so a destination still waiting for a flow it already started to be completed takes no
//! second create: that is the modelled `ConflictException` (HTTP 409) carrying this
//! resource's own fixed sentence, and it writes nothing. A destination whose associations
//! have all LEFT that flow (it succeeded, it failed, its refresh token expired, or the
//! record is being deleted) takes a create again, and the conflict is scoped to one
//! destination in one region.
//!
//! The recorded unit this was derived from
//! (`iot-managed-integrations.account-association-lifecycle-and-refresh-conflict`, at the
//! assertion labelled `duplicate-create-while-outstanding`) is ONE row of the tables below.
//! The rows differ along what the rule generalises over: the `AssociationState` the stored
//! record carries when the second create arrives (every value the model declares), the
//! members the second create carries (including a `ClientToken`, and the first call's own
//! token), the destination and the region the create names, and how many creates follow one
//! another against one destination.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the routes, statuses, bodies and modelled errors asserted here are the ones a client
//! sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    ACCOUNT_ASSOCIATION_CONFLICT_REASON, ASSOCIATION_STATE_VALUES, INITIAL_ASSOCIATION_STATE,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::{
    ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState as ServiceState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The collection routes this suite drives
/// (`smithy_query(query="operation",operation="CreateAccountAssociation")` -> POST
/// `/account-associations`, `CreateConnectorDestination` -> POST `/connector-destinations`,
/// `CreateCloudConnector` -> POST `/cloud-connectors`).
const COLLECTION: &str = "/account-associations";
const CONNECTOR_DESTINATIONS: &str = "/connector-destinations";
const CLOUD_CONNECTORS: &str = "/cloud-connectors";

/// The wire error code the model's `@httpError` 409 shape is rendered under.
const CONFLICT: &str = "ConflictException";

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
    emulator_over(Arc::new(engine::InMemoryEngine::new()))
}

/// [`emulator`] over an engine the caller keeps a handle to, so a case can put a stored
/// record in a state no implemented operation reaches.
fn emulator_over(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
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

/// [`create_in`] in [`REGION`].
async fn create(
    router: &MultiServiceRouter,
    connector_destination_id: &str,
    members: Value,
) -> Reply {
    create_in(router, REGION, connector_destination_id, members).await
}

/// `GetAccountAssociation` of `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// `ListAccountAssociations` filtered on `connector_destination_id`, signed for `region`.
async fn list_for_in(
    router: &MultiServiceRouter,
    region: &str,
    connector_destination_id: &str,
) -> Reply {
    call_in(
        router,
        region,
        "GET",
        &format!("{COLLECTION}?ConnectorDestinationId={connector_destination_id}"),
        None,
    )
    .await
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
            // A destination that configures an OAuth authorization server, so a create
            // against it answers a real authorization request to be completed.
            "AuthConfig": {"oAuth": {
                "authUrl": "https://partner.example.com/oauth2/authorize",
                "tokenUrl": "https://partner.example.com/oauth2/token",
                "tokenEndpointAuthenticationScheme": "HTTP_BASIC",
            }},
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

/// Assert `reply` is the modelled conflict: HTTP 409, the `ConflictException` code, this
/// resource's fixed sentence and NO record in the body.
fn assert_conflict(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 409,
        "case {case:?}: expected HTTP 409, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(CONFLICT),
        "case {case:?}: expected the modelled {CONFLICT} code, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.message(),
        ACCOUNT_ASSOCIATION_CONFLICT_REASON,
        "case {case:?}: expected the fixed sentence {ACCOUNT_ASSOCIATION_CONFLICT_REASON:?}, \
         got {:?}",
        reply.message()
    );
    assert!(
        reply.member("AccountAssociationId").is_none()
            && reply.member("Arn").is_none()
            && reply.member("OAuthAuthorizationUrl").is_none(),
        "case {case:?}: a refused create answers no association, got body {:?}",
        reply.body
    );
}

/// Put the stored association `id` in `state`, which is where the authorization flow that
/// ended left it: no implemented operation of this slice moves a record out of the
/// in-progress state, so the states past it are seeded onto the record.
fn seed_state(engine: &engine::InMemoryEngine, region: &str, id: &str, state: &str, case: &str) {
    let handle = engine.state::<ServiceState>();
    let mut store = handle.lock();
    let association = store
        .account_association_in_mut(region, id)
        .unwrap_or_else(|| panic!("case {case:?}: the created association is stored"));
    association.association_state = state.to_string();
}

/// RULE: only a destination whose authorization flow is still OUTSTANDING refuses a second
/// create; a destination whose association has left that flow takes one. The rows are every
/// `AssociationState` the model declares
/// (`smithy_query(query="shape",shape="AssociationState")`), so the state the recorded unit
/// exercised is one row of them.
#[tokio::test]
async fn a_second_create_is_refused_only_while_the_destinations_flow_is_outstanding() {
    assert!(
        ASSOCIATION_STATE_VALUES.contains(&INITIAL_ASSOCIATION_STATE),
        "the in-progress state is one the model declares, got {ASSOCIATION_STATE_VALUES:?}"
    );

    for state in ASSOCIATION_STATE_VALUES {
        let case = format!("stored state {state}");
        // A fresh store per row, so the only association it holds is this row's own.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_over(Arc::clone(&engine));
        let destination = connector_destination_in(&router, REGION, &case).await;

        let first = create(&router, &destination, json!({"Name": "Partner account"})).await;
        let held = accepted_id(&case, &first);
        let held_url = first
            .string("OAuthAuthorizationUrl")
            .unwrap_or_default()
            .to_string();
        seed_state(&engine, REGION, &held, state, &case);

        let second = create(&router, &destination, json!({"Name": "Second account"})).await;
        if *state == INITIAL_ASSOCIATION_STATE {
            assert_conflict(&case, &second);

            // The refusal is effect-free: the association already outstanding keeps its
            // identifier, its state and the authorization request it named...
            let read = get_in(&router, REGION, &held).await;
            assert_eq!(
                read.string("AccountAssociationId"),
                Some(held.as_str()),
                "case {case:?}: expected the stored association to keep its identifier, \
                 got {:?}",
                read.body
            );
            assert_eq!(
                read.string("AssociationState"),
                Some(INITIAL_ASSOCIATION_STATE),
                "case {case:?}: expected the stored association to keep {INITIAL_ASSOCIATION_STATE:?}, \
                 got {:?}",
                read.string("AssociationState")
            );
            assert_eq!(
                read.string("OAuthAuthorizationUrl"),
                Some(held_url.as_str()),
                "case {case:?}: expected the stored association to keep its authorization \
                 URL, got {:?}",
                read.string("OAuthAuthorizationUrl")
            );
            // ...and no second record was written for the destination.
            let listed = list_for_in(&router, REGION, &destination).await;
            assert_eq!(
                listed.item_ids(),
                vec![held.clone()],
                "case {case:?}: expected the destination to hold exactly the association it \
                 already had, got body {:?}",
                listed.body
            );
        } else {
            let minted = accepted_id(&case, &second);
            assert_ne!(
                minted, held,
                "case {case:?}: a create that is accepted mints its own identifier"
            );
            let listed = list_for_in(&router, REGION, &destination).await;
            assert_eq!(
                listed.item_ids(),
                vec![held.clone(), minted.clone()],
                "case {case:?}: expected the destination to hold both associations in \
                 insertion order, got body {:?}",
                listed.body
            );
        }
    }
}

/// RULE: the conflict is about the destination's outstanding flow, not about what the second
/// create asks for, so it is refused whatever members that create carries — while a
/// `ClientToken` the FIRST create carried replays that create's own answer, which is a retry
/// rather than a duplicate.
#[tokio::test]
async fn the_conflict_does_not_depend_on_the_members_the_second_create_carries() {
    let router = emulator();
    let destination = connector_destination_in(&router, REGION, "the destination").await;

    // The first create carries a token, so the replay path below has one to resolve. The
    // `ClientToken` pattern is `^[a-zA-Z0-9=_-]+$`, length 1..64.
    let first_token = "first-create-token";
    let first = create(
        &router,
        &destination,
        json!({"Name": "Partner account", "ClientToken": first_token}),
    )
    .await;
    let held = accepted_id("the first create", &first);

    // The rows differ along the members the duplicate carries: none beyond the required
    // destination id, the same name as the record already stored, a different name, a
    // description, tags, a general authorization, every member at once, and a `ClientToken`
    // this service has never seen.
    let duplicates: Vec<(&str, Value)> = vec![
        ("required only", json!({})),
        ("the same name", json!({"Name": "Partner account"})),
        ("another name", json!({"Name": "Another account"})),
        ("a description", json!({"Description": "the end user account"})),
        ("tags", json!({"Tags": {"env": "test"}})),
        (
            "a general authorization",
            json!({"GeneralAuthorization": {"AuthMaterialName": "material-1"}}),
        ),
        (
            "every member",
            json!({
                "Name": "N",
                "Description": "D",
                "Tags": {"only": "one"},
                "GeneralAuthorization": {"AuthMaterialName": "m+a/t=e-r_1"},
            }),
        ),
        ("a fresh client token", json!({"ClientToken": "another-token"})),
    ];
    for (case, members) in duplicates {
        let refused = create(&router, &destination, members).await;
        assert_conflict(case, &refused);
    }

    // A retry carrying the FIRST create's token is answered with that create's record,
    // performing no further action — the idempotency replay comes before the conflict.
    let replayed = create(
        &router,
        &destination,
        json!({"Name": "Partner account", "ClientToken": first_token}),
    )
    .await;
    assert_eq!(
        accepted_id("the replayed retry", &replayed),
        held,
        "expected the retry to replay the first create's association"
    );

    // Still exactly one record for the destination.
    let listed = list_for_in(&router, REGION, &destination).await;
    assert_eq!(
        listed.item_ids(),
        vec![held.clone()],
        "expected the destination to hold exactly one association, got body {:?}",
        listed.body
    );
}

/// RULE: an outstanding flow occupies ONE destination in ONE region, so a create against
/// another destination — or against this destination in another region — is accepted; and
/// once a destination's flow has ended, the create that starts the next flow is accepted and
/// occupies the destination in its turn.
#[tokio::test]
async fn the_conflict_is_scoped_to_one_destination_in_one_region() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let destination = connector_destination_in(&router, REGION, "the destination").await;
    let other_destination = connector_destination_in(&router, REGION, "another destination").await;
    let held = accepted_id(
        "the first create",
        &create(&router, &destination, json!({})).await,
    );

    // Another destination of the same region is waiting for no flow of its own.
    let elsewhere = accepted_id(
        "another destination",
        &create(&router, &other_destination, json!({})).await,
    );
    assert_ne!(
        elsewhere, held,
        "a create against another destination mints its own association"
    );
    assert_conflict(
        "the occupied destination",
        &create(&router, &destination, json!({})).await,
    );

    // Each region holds its own destinations and associations, so another region's endpoint
    // is waiting for nothing.
    for region in ["eu-central-1", "ap-southeast-2"] {
        let case = format!("region {region}");
        let regional = connector_destination_in(&router, region, &case).await;
        let minted = accepted_id(&case, &create_in(&router, region, &regional, json!({})).await);
        assert_conflict(
            &format!("{case}, again"),
            &create_in(&router, region, &regional, json!({})).await,
        );
        assert_ne!(
            minted, held,
            "case {case:?}: a create in another region mints its own association"
        );
    }

    // Once the flow the destination was waiting for has ended, the next create is accepted —
    // and that one occupies the destination in its turn, so a third create conflicts again.
    seed_state(
        &engine,
        REGION,
        &held,
        "ASSOCIATION_SUCCEEDED",
        "the flow that ended",
    );
    let next = accepted_id(
        "the next flow",
        &create(&router, &destination, json!({})).await,
    );
    assert_conflict(
        "the destination occupied by the next flow",
        &create(&router, &destination, json!({})).await,
    );
    let listed = list_for_in(&router, REGION, &destination).await;
    assert_eq!(
        listed.item_ids(),
        vec![held.clone(), next.clone()],
        "expected the destination to hold the two accepted associations in insertion order, \
         got body {:?}",
        listed.body
    );
}

/// RULE: a refresh of the outstanding association is not a duplicate create — it starts a
/// new flow for the record that already exists — so it is accepted and leaves the
/// destination occupied by that same record.
#[tokio::test]
async fn a_refresh_of_the_outstanding_association_is_not_a_conflict() {
    let router = emulator();
    let destination = connector_destination_in(&router, REGION, "the destination").await;
    let held = accepted_id(
        "the first create",
        &create(&router, &destination, json!({"Name": "Partner account"})).await,
    );

    let refreshed = call_in(
        &router,
        REGION,
        "POST",
        &format!("{COLLECTION}/{held}/refresh"),
        None,
    )
    .await;
    assert_eq!(
        refreshed.status, 200,
        "expected HTTP 200 from StartAccountAssociationRefresh, got {} with body {:?}",
        refreshed.status, refreshed.body
    );

    let read = get_in(&router, REGION, &held).await;
    assert_eq!(
        read.string("AssociationState"),
        Some(INITIAL_ASSOCIATION_STATE),
        "expected the refreshed association to report {INITIAL_ASSOCIATION_STATE:?}, got {:?}",
        read.string("AssociationState")
    );
    assert_conflict(
        "a create after the refresh",
        &create(&router, &destination, json!({})).await,
    );
    let listed = list_for_in(&router, REGION, &destination).await;
    assert_eq!(
        listed.item_ids(),
        vec![held.clone()],
        "expected the destination to hold exactly the refreshed association, got body {:?}",
        listed.body
    );
}
