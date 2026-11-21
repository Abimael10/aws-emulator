// SPDX-License-Identifier: CC0-1.0

//! Starting a new authorization flow for an account association, end to end over the wire:
//! `CreateConnectorDestination` -> `CreateAccountAssociation` ->
//! `StartAccountAssociationRefresh` -> `GetAccountAssociation`.
//!
//! RULE: `StartAccountAssociationRefresh` (POST
//! `/account-associations/{AccountAssociationId}/refresh`, success 200) starts a new
//! authorization flow for the account association the signing region holds under that
//! identifier: the stored record carries the in-progress association state again and no
//! error message, and the answer is the `OAuthAuthorizationUrl` that flow's end user
//! authenticates at — the authorization server of the OAuth configuration of the connector
//! destination the association was created against, carrying the parameters of the
//! authorization request (the grant asked for, the configured scope, and the association the
//! authorization belongs to). A destination that configures no OAuth, or one whose
//! authorization URL the modelled output member cannot carry, leaves the member its modelled
//! empty default. An identifier this region holds no association under is the modelled
//! `ResourceNotFoundException` (HTTP 404), and an association whose deletion has begun
//! starts no flow: the modelled `ConflictException` (HTTP 409), with the record untouched.
//!
//! The recorded unit this was derived from
//! (`iot-managed-integrations.account-association-lifecycle-and-refresh-conflict`) is ONE
//! row of the tables below. The rows differ along what the rule generalises over: the
//! `AssociationState` the record carries when the flow is started (every value the model
//! declares), whether it carries an error message, the OAuth configuration of the
//! destination (absent, minimal, scoped, a scope needing encoding, an authorization URL that
//! already carries a query, and an `http` one the output member cannot carry), the members
//! the association itself carries, the form of an identifier that resolves to nothing, and
//! the region the request is signed for.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the routes, statuses, bodies and modelled errors asserted here are the ones a client
//! sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    ACCOUNT_ASSOCIATION_RESOURCE_TYPE, ASSOCIATION_STATE_DELETING, ASSOCIATION_STATE_VALUES,
    INITIAL_ASSOCIATION_STATE,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::{
    ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState as ServiceState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The collection routes this suite creates on
/// (`smithy_query(query="operation",operation="CreateAccountAssociation")` -> POST
/// `/account-associations`, `CreateConnectorDestination` -> POST `/connector-destinations`,
/// `CreateCloudConnector` -> POST `/cloud-connectors`).
const COLLECTION: &str = "/account-associations";
const CONNECTOR_DESTINATIONS: &str = "/connector-destinations";
const CLOUD_CONNECTORS: &str = "/cloud-connectors";

/// The wire error codes the model's `@httpError` shapes are rendered under.
const NOT_FOUND: &str = "ResourceNotFoundException";
const CONFLICT: &str = "ConflictException";

/// The reason a 404 of this resource carries, as `GetAccountAssociation` is recorded wording
/// the absence of an account association.
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
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    emulator_over(Arc::new(engine::InMemoryEngine::new()))
}

/// [`emulator`] over an engine the caller keeps a handle to, so a test can seed the
/// service's store directly instead of driving requests it has no API for.
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

/// The refresh URI of `id`
/// (`smithy_query(query="operation",operation="StartAccountAssociationRefresh")` ->
/// `/account-associations/{AccountAssociationId}/refresh`).
fn refresh_uri(id: &str) -> String {
    format!("{COLLECTION}/{id}/refresh")
}

/// `StartAccountAssociationRefresh` of `id`, signed for `region`.
async fn refresh_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "POST", &refresh_uri(id), None).await
}

/// [`refresh_in`] in [`REGION`].
async fn refresh(router: &MultiServiceRouter, id: &str) -> Reply {
    refresh_in(router, REGION, id).await
}

/// `GetAccountAssociation` of `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// [`get_in`] in [`REGION`].
async fn get(router: &MultiServiceRouter, id: &str) -> Reply {
    get_in(router, REGION, id).await
}

/// One `SecretsManager` configuration, inside the `SecretsManagerArn` `@pattern`
/// (`^arn:aws:secretsmanager:[0-9a-zA-Z-]{1,32}:\d{12}:secret:[A-Za-z0-9/_+=.@-]{8,520}$`)
/// and the `SecretsManagerVersionId` `@length` (32..64).
fn secret(region: &str, name: &str) -> Value {
    json!({
        "arn": format!(
            "arn:aws:secretsmanager:{region}:{}:secret:{name}-A1b2C3",
            engine::EMULATOR_ACCOUNT_ID
        ),
        "versionId": "a".repeat(32),
    })
}

/// Create a connector destination in `region` carrying `auth_config`, through the wire, and
/// answer the `ConnectorDestinationId` it minted.
///
/// `CreateAccountAssociation` resolves its `ConnectorDestinationId` against this service's
/// own connector-destination store, so every association below is created against a
/// destination a client really created; a connector destination in turn names a C2C
/// connector (`CloudConnectorId` is `@required`), so the connector comes first.
async fn connector_destination_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    auth_config: Value,
) -> String {
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
            "AuthConfig": auth_config,
            "SecretsManager": secret(region, "destination"),
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

/// Create an account association in `region` against `connector_destination_id`, carrying
/// `members`, and answer the `AccountAssociationId` it minted together with the
/// `OAuthAuthorizationUrl` the create reported.
async fn association_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    connector_destination_id: &str,
    members: Value,
) -> (String, String) {
    let mut body = members;
    body["ConnectorDestinationId"] = json!(connector_destination_id);
    let created = call_in(router, region, "POST", COLLECTION, Some(body)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateAccountAssociation, got {} with \
         body {:?}",
        created.status, created.body
    );
    let id = created
        .string("AccountAssociationId")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected CreateAccountAssociation to answer an \
                 AccountAssociationId, got body {:?}",
                created.body
            )
        })
        .to_string();
    (
        id,
        created
            .string("OAuthAuthorizationUrl")
            .unwrap_or_default()
            .to_string(),
    )
}

/// The `AuthConfig` of a destination that configures no OAuth at all: a General
/// Authorization flow, which needs no authorization server.
fn no_oauth() -> Value {
    json!({})
}

/// The `AuthConfig` of a destination whose OAuth configuration names `auth_url` and, where
/// the row has one, `scope`. The three members `OAuthConfig` declares `@required` are always
/// carried (`smithy_query(query="shape",shape="OAuthConfig")` -> `authUrl`, `tokenUrl`,
/// `tokenEndpointAuthenticationScheme`).
fn oauth(auth_url: &str, scope: Option<&str>) -> Value {
    let mut o_auth = json!({
        "authUrl": auth_url,
        "tokenUrl": "https://partner.example.com/oauth2/token",
        "tokenEndpointAuthenticationScheme": "HTTP_BASIC",
    });
    if let Some(scope) = scope {
        o_auth["scope"] = json!(scope);
    }
    json!({"oAuth": o_auth})
}

/// Assert `reply` is the modelled refusal `status`/`error_type` names, carrying `message`.
fn assert_refused(case: &str, reply: &Reply, status: u16, error_type: &str, message: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(error_type),
        "case {case:?}: expected the {error_type} wire code, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the reason {message:?}, got {:?}",
        reply.message()
    );
}

/// RULE: a refresh starts a NEW authorization flow for the association it names, whatever
/// state the record carries: the record reports the in-progress state again and drops the
/// error message of the flow that ended, and the answer is the URL the new flow's end user
/// authenticates at. The other members of the record are untouched.
///
/// The states a record can carry are the model's own value set
/// (`smithy_query(query="shape",shape="AssociationState")`), and only a create puts a record
/// in the store, so the rows past the one a create leaves are seeded onto the stored record
/// — which is exactly the state an association whose authorization failed, succeeded or
/// expired is in at AWS.
#[tokio::test]
async fn a_refresh_starts_a_new_authorization_flow_from_every_state_that_admits_one() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let auth_url = "https://partner.example.com/oauth2/authorize";

    // Every state the model declares except the deleting one, which admits no new flow and
    // is the subject of its own case below. Each row also says whether the record carries
    // the error message of a flow that failed.
    let rows: Vec<(&str, Option<&str>)> = ASSOCIATION_STATE_VALUES
        .iter()
        .filter(|state| **state != ASSOCIATION_STATE_DELETING)
        .map(|state| {
            let error = (*state == "ASSOCIATION_FAILED").then_some("Authorization was revoked");
            (*state, error)
        })
        .collect();
    assert!(
        rows.len() >= 2,
        "the model declares more than one refreshable AssociationState, got {rows:?}"
    );

    for (state, error_message) in rows {
        let case = format!("state {state}");
        // Each row's association is created against its OWN destination: a destination holds
        // one association whose authorization flow is outstanding at a time, so a create
        // against a destination that already has one is refused.
        let destination =
            connector_destination_in(&router, REGION, &case, oauth(auth_url, None)).await;
        let (id, _) = association_in(
            &router,
            REGION,
            &case,
            &destination,
            json!({"Name": "Partner account", "Description": "The end user account"}),
        )
        .await;

        // Put the record in the state this row is about, as the flow that ended left it.
        {
            let state_handle = engine.state::<ServiceState>();
            let mut store = state_handle.lock();
            let association = store
                .account_association_in_mut(REGION, &id)
                .unwrap_or_else(|| panic!("case {case:?}: the created association is stored"));
            association.association_state = state.to_string();
            association.error_message = error_message.map(str::to_string);
        }

        let started = refresh(&router, &id).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from the refresh, got {} with body {:?}",
            started.status, started.body
        );
        let url = started.string("OAuthAuthorizationUrl").unwrap_or_default();
        assert!(
            url.starts_with(auth_url),
            "case {case:?}: expected the URL of the destination's authorization server \
             {auth_url:?}, got {url:?}"
        );

        // The record now reports the new flow: the in-progress state, no error message, and
        // the URL the refresh handed out.
        let read = get(&router, &id).await;
        assert_eq!(
            read.string("AssociationState"),
            Some(INITIAL_ASSOCIATION_STATE),
            "case {case:?}: expected the read to report {INITIAL_ASSOCIATION_STATE:?}, got {:?}",
            read.string("AssociationState")
        );
        assert!(
            read.member("ErrorMessage").is_none(),
            "case {case:?}: the flow that failed is over, so no ErrorMessage is reported, \
             got {:?}",
            read.member("ErrorMessage")
        );
        assert_eq!(
            read.string("OAuthAuthorizationUrl"),
            Some(url),
            "case {case:?}: expected the read to report the URL the refresh answered, got {:?}",
            read.string("OAuthAuthorizationUrl")
        );
        // Everything else the record holds is left alone.
        for (member, expected) in [
            ("Name", "Partner account"),
            ("Description", "The end user account"),
            ("ConnectorDestinationId", destination.as_str()),
        ] {
            assert_eq!(
                read.string(member),
                Some(expected),
                "case {case:?}: expected the refresh to leave {member} at {expected:?}, got {:?}",
                read.string(member)
            );
        }
        assert_eq!(
            read.string("AccountAssociationId"),
            Some(id.as_str()),
            "case {case:?}: expected the read to report the same identifier, got {:?}",
            read.string("AccountAssociationId")
        );
    }
}

/// RULE: the `OAuthAuthorizationUrl` a refresh answers is the authorization server of the
/// connector destination's OAuth configuration, carrying the parameters of the authorization
/// request: the grant asked for, the configured scope where there is one, and the
/// association the authorization belongs to. A destination configuring no OAuth, or one
/// whose `AuthUrl` the modelled output member cannot carry (it admits `https` only:
/// `smithy_query(query="shape",shape="OAuthAuthorizationUrlOutput")` -> pattern
/// `^$|^(https):\/\/.*$`), leaves the member its modelled empty default.
#[tokio::test]
async fn the_authorization_url_is_built_from_the_destination_s_oauth_configuration() {
    let router = emulator();

    // The rows differ along the destination's OAuth configuration: none at all, an
    // authorization server with and without a scope, a scope whose characters have to be
    // encoded in a query, an authorization URL that already carries a query of its own, and
    // an `http` one the output member cannot carry. `AuthUrl` itself admits both schemes
    // (`smithy_query(query="shape",shape="AuthUrl")` -> `^https?:\/\/…`).
    let rows: Vec<(&str, Value, Option<&str>, Option<&str>)> = vec![
        ("no-oauth-at-all", no_oauth(), None, None),
        (
            "oauth-without-scope",
            oauth("https://partner.example.com/oauth2/authorize", None),
            Some("https://partner.example.com/oauth2/authorize"),
            None,
        ),
        (
            "oauth-with-scope",
            oauth(
                "https://partner.example.com/oauth2/authorize",
                Some("devices.read"),
            ),
            Some("https://partner.example.com/oauth2/authorize"),
            Some("devices.read"),
        ),
        (
            "scope-needing-encoding",
            oauth(
                "https://partner.example.com/oauth2/authorize",
                Some("devices.read devices.write"),
            ),
            Some("https://partner.example.com/oauth2/authorize"),
            Some("devices.read devices.write"),
        ),
        (
            "auth-url-carrying-a-query",
            oauth(
                "https://partner.example.org/authorize?audience=homes",
                Some("lights"),
            ),
            Some("https://partner.example.org/authorize?audience=homes"),
            Some("lights"),
        ),
        (
            "http-auth-url-the-output-member-cannot-carry",
            oauth("http://partner.example.net/authorize", None),
            None,
            None,
        ),
    ];

    for (case, auth_config, expected_server, expected_scope) in rows {
        let destination = connector_destination_in(&router, REGION, case, auth_config).await;
        let (id, created_url) =
            association_in(&router, REGION, case, &destination, json!({})).await;

        let started = refresh(&router, &id).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from the refresh, got {} with body {:?}",
            started.status, started.body
        );
        let url = started.string("OAuthAuthorizationUrl").unwrap_or_default();

        match expected_server {
            // Nothing to authorize against: the modelled empty default, from the create and
            // from the refresh alike.
            None => {
                assert_eq!(
                    url, "",
                    "case {case:?}: expected no authorization URL, got {url:?}"
                );
                assert_eq!(
                    created_url, "",
                    "case {case:?}: expected no authorization URL from the create, got \
                     {created_url:?}"
                );
            }
            Some(server) => {
                assert!(
                    url.starts_with(server),
                    "case {case:?}: expected the URL to address {server:?}, got {url:?}"
                );
                // The query the configured URL already carries is part of the server's
                // address, so it survives beside the parameters this service adds.
                let (_, query) = url
                    .split_once('?')
                    .unwrap_or_else(|| panic!("case {case:?}: expected a query, got {url:?}"));
                let parameters: Vec<(String, String)> = query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect();
                let valued = |name: &str| {
                    parameters
                        .iter()
                        .find(|(parameter, _)| parameter == name)
                        .map(|(_, value)| value.clone())
                };
                assert_eq!(
                    valued("response_type"),
                    Some("code".to_string()),
                    "case {case:?}: expected the authorization-code grant to be asked for, \
                     got {url:?}"
                );
                assert_eq!(
                    valued("state"),
                    Some(id.clone()),
                    "case {case:?}: expected the association's own identifier to tie the \
                     authorization back to it, got {url:?}"
                );
                // A scope the configuration names is carried, encoded for a query; one it
                // does not name is absent rather than empty.
                match expected_scope {
                    None => assert_eq!(
                        valued("scope"),
                        None,
                        "case {case:?}: the configuration names no scope, so the URL carries \
                         none, got {url:?}"
                    ),
                    Some(scope) => assert_eq!(
                        valued("scope").as_deref(),
                        Some(scope.replace(' ', "+").as_str()),
                        "case {case:?}: expected the configured scope {scope:?}, got {url:?}"
                    ),
                }
                if let Some((_, server_query)) = server.split_once('?') {
                    assert!(
                        query.contains(server_query),
                        "case {case:?}: expected the server's own query {server_query:?} to \
                         survive, got {url:?}"
                    );
                }
                // The create of the same association reports the same authorization
                // request, since both are built from the same stored configuration.
                assert!(
                    created_url.starts_with(server),
                    "case {case:?}: expected the create to report the same authorization \
                     server, got {created_url:?}"
                );
            }
        }
    }
}

/// RULE: a refresh resolves ONE region's association under the identifier it carries, so an
/// identifier this region holds no association under is the modelled
/// `ResourceNotFoundException` and starts no flow — whatever the form of that identifier,
/// and including an identifier another region's endpoint really minted.
#[tokio::test]
async fn a_refresh_of_an_association_this_region_does_not_hold_is_the_modelled_404() {
    let router = emulator();

    let destination =
        connector_destination_in(&router, REGION, "the destination", no_oauth()).await;
    let (held, _) = association_in(&router, REGION, "the held association", &destination, json!({}))
        .await;

    // An association another region's endpoint really minted, which this region resolves no
    // more than it resolves an identifier nobody minted.
    let elsewhere = "eu-central-1";
    let other_destination =
        connector_destination_in(&router, elsewhere, "the other destination", no_oauth()).await;
    let (other_region_id, _) = association_in(
        &router,
        elsewhere,
        "the other region's association",
        &other_destination,
        json!({}),
    )
    .await;

    // The rows differ along the FORM of the identifier: both `@length` bounds of
    // `AccountAssociationId` (len 1..64, `^[0-9a-zA-Z]+$`), mixed case, digits only, an
    // identifier this region minted for another resource kind, and one another region holds.
    let rows: Vec<(&str, String)> = vec![
        ("one-character", "a".to_string()),
        ("sixty-four-characters", "b".repeat(64)),
        ("mixed-case", "AbC123".to_string()),
        ("digits-only", "90210".to_string()),
        ("another-region-s-association", other_region_id.clone()),
    ];

    for (case, id) in rows {
        let refused = refresh(&router, &id).await;
        assert_refused(case, &refused, 404, NOT_FOUND, NOT_FOUND_MESSAGE);
        assert_eq!(
            refused.string("ResourceId"),
            Some(id.as_str()),
            "case {case:?}: expected the identifier the request carried on ResourceId, got {:?}",
            refused.string("ResourceId")
        );
        assert_eq!(
            refused.string("ResourceType"),
            Some(ACCOUNT_ASSOCIATION_RESOURCE_TYPE),
            "case {case:?}: expected the resource type on ResourceType, got {:?}",
            refused.string("ResourceType")
        );
    }

    // The association this region does hold is untouched by every refused call, and the one
    // the other region holds is still in the state its own create left it.
    let read = get(&router, &held).await;
    assert_eq!(
        read.status, 200,
        "case \"the held association\": expected HTTP 200 from the get, got {} with body {:?}",
        read.status, read.body
    );
    let other = get_in(&router, elsewhere, &other_region_id).await;
    assert_eq!(
        other.string("AssociationState"),
        Some(INITIAL_ASSOCIATION_STATE),
        "case \"the other region's association\": expected {INITIAL_ASSOCIATION_STATE:?}, got {:?}",
        other.string("AssociationState")
    );
}

/// RULE: an association whose deletion has begun starts no new authorization flow: the
/// modelled `ConflictException`, whose reason names the state the record carries, and
/// nothing is written — the record still reports that state and the URL of the flow it was
/// in.
#[tokio::test]
async fn a_refresh_of_an_association_being_deleted_is_the_modelled_conflict() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let auth_url = "https://partner.example.com/oauth2/authorize";
    let destination = connector_destination_in(
        &router,
        REGION,
        "the destination",
        oauth(auth_url, Some("devices.read")),
    )
    .await;
    let (id, created_url) = association_in(
        &router,
        REGION,
        "the association",
        &destination,
        json!({"Name": "Partner account"}),
    )
    .await;

    // Only a delete puts a record in this state, so the state a delete leaves is seeded.
    {
        let state_handle = engine.state::<ServiceState>();
        let mut store = state_handle.lock();
        let association = store
            .account_association_in_mut(REGION, &id)
            .unwrap_or_else(|| panic!("the created association is stored"));
        association.association_state = ASSOCIATION_STATE_DELETING.to_string();
    }

    let refused = refresh(&router, &id).await;
    assert_refused(
        "deleting",
        &refused,
        409,
        CONFLICT,
        &format!(
            "Account association is in {ASSOCIATION_STATE_DELETING} state, so its \
             authorization cannot be refreshed"
        ),
    );

    // Nothing was written: the record is still the one the delete left.
    let read = get(&router, &id).await;
    assert_eq!(
        read.string("AssociationState"),
        Some(ASSOCIATION_STATE_DELETING),
        "case \"deleting\": expected the record to stay in {ASSOCIATION_STATE_DELETING:?}, got {:?}",
        read.string("AssociationState")
    );
    assert_eq!(
        read.string("OAuthAuthorizationUrl"),
        Some(created_url.as_str()),
        "case \"deleting\": expected the URL of the flow it was in, got {:?}",
        read.string("OAuthAuthorizationUrl")
    );
}

/// RULE: a refresh starts a flow for ONE association, so it changes nothing another
/// association reports — and a second refresh of the same association starts another flow,
/// answering the same authorization request for a record already in the in-progress state.
#[tokio::test]
async fn a_refresh_touches_only_the_association_it_names() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let destination = connector_destination_in(
        &router,
        REGION,
        "the destination",
        oauth("https://partner.example.com/oauth2/authorize", None),
    )
    .await;
    // The association left alone is created against its OWN destination: a destination holds
    // one association whose authorization flow is outstanding at a time.
    let other_destination = connector_destination_in(
        &router,
        REGION,
        "another destination",
        oauth("https://partner.example.com/oauth2/authorize", None),
    )
    .await;
    let (refreshed, _) =
        association_in(&router, REGION, "refreshed", &destination, json!({})).await;
    let (untouched, _) =
        association_in(&router, REGION, "untouched", &other_destination, json!({})).await;

    // The association that is left alone has the error message of a failed flow, which no
    // refresh of ANOTHER association may clear.
    {
        let state_handle = engine.state::<ServiceState>();
        let mut store = state_handle.lock();
        let association = store
            .account_association_in_mut(REGION, &untouched)
            .unwrap_or_else(|| panic!("the created association is stored"));
        association.association_state = "ASSOCIATION_FAILED".to_string();
        association.error_message = Some("Authorization was revoked".to_string());
    }

    let first = refresh(&router, &refreshed).await;
    let second = refresh(&router, &refreshed).await;
    for (case, started) in [("first", &first), ("second", &second)] {
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from the refresh, got {} with body {:?}",
            started.status, started.body
        );
    }
    assert_eq!(
        second.string("OAuthAuthorizationUrl"),
        first.string("OAuthAuthorizationUrl"),
        "case \"second\": the same stored configuration and association answer the same \
         authorization request, got {:?} after {:?}",
        second.string("OAuthAuthorizationUrl"),
        first.string("OAuthAuthorizationUrl")
    );

    let read = get(&router, &untouched).await;
    assert_eq!(
        read.string("AssociationState"),
        Some("ASSOCIATION_FAILED"),
        "case \"untouched\": expected the other association's state to stand, got {:?}",
        read.string("AssociationState")
    );
    assert_eq!(
        read.string("ErrorMessage"),
        Some("Authorization was revoked"),
        "case \"untouched\": expected the other association's error message to stand, got {:?}",
        read.string("ErrorMessage")
    );
}
