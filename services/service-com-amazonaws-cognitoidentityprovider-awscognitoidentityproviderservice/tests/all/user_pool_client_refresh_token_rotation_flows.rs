// SPDX-License-Identifier: CC0-1.0

//! `CreateUserPoolClient` and `UpdateUserPoolClient` over the wire: the configuration
//! an app client that ROTATES its refresh tokens may hold.
//!
//! The rule under test, one level above the recorded case (a create carrying
//! `RefreshTokenRotation={Feature: ENABLED, RetryGracePeriodSeconds: 0}` beside
//! `ExplicitAuthFlows=[ALLOW_USER_PASSWORD_AUTH, ALLOW_REFRESH_TOKEN_AUTH]`):
//!
//! > Refresh token rotation replaces the legacy refresh-token sign-in — "when enabled,
//! > your app client issues new ID, access, and refresh tokens when users renew their
//! > sessions with refresh tokens" (`RefreshTokenRotationType`, this service's model),
//! > whose renewal is `GetTokensFromRefreshToken` rather than the `REFRESH_TOKEN_AUTH`
//! > sign-in `ALLOW_REFRESH_TOKEN_AUTH` enables — so a write that switches rotation on
//! > while naming that flow is refused `InvalidParameterException` (400) before any app
//! > client is minted or any field of one is changed, while each half alone is a
//! > configuration an app client holds and echoes back.
//!
//! The rows differ along what the rule generalises over: whether rotation is switched on
//! or off, the grace period it asks for across the published bounds of
//! `RetryGracePeriodSeconds` (0..60, `smithy_query(query="shape",
//! shape="RefreshTokenRotationType")`), whether the forbidden flow is named alone or
//! beside other flows of `ExplicitAuthFlowsType`, at which position of the list it is
//! named, and a request that names no flows at all. Every row is driven through BOTH
//! writes — the create, and an update of an app client the rule accepts — and each
//! refusal is checked for effect: a refused create leaves the pool with no app client of
//! that name (`ListUserPoolClients`), and a refused update leaves
//! `DescribeUserPoolClient` reporting the flows and the rotation configuration the app
//! client already held.
//!
//! Each table row names the case it asserts.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// This service's SigV4 signing name, the 4th field of a credential scope.
const SIGNING_NAME: &str = "cognito-idp";

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// This service's real regional endpoint host.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The Region the suite's calls are signed for.
const REGION: &str = "us-east-1";

/// The `ExplicitAuthFlows` value that enables the legacy refresh-token sign-in, which an
/// app client rotating its refresh tokens may not carry.
const LEGACY_REFRESH_FLOW: &str = "ALLOW_REFRESH_TOKEN_AUTH";

/// The status and wire error code a parameter contradiction of these operations is
/// refused with (`InvalidParameterException`, httpError 400, declared by both).
const INVALID_PARAMETER: (u16, &str) = (400, "InvalidParameterException");

/// The flows an app client the rule ACCEPTS is configured with: no row's forbidden flow
/// among them, so rotation may be switched on beside them.
const ACCEPTED_FLOWS: [&str; 2] = ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_USER_SRP_AUTH"];

/// The smallest grace period `RetryGracePeriodSeconds` admits, and the largest: the
/// published bounds of the member (range 0..60).
const MIN_GRACE_SECONDS: i32 = 0;
const MAX_GRACE_SECONDS: i32 = 60;

/// One row: the `RefreshTokenRotation` and `ExplicitAuthFlows` a write carries, and
/// whether the pairing describes an app client this service accepts.
struct Row {
    case: &'static str,
    /// `RefreshTokenRotation.Feature`, or `None` for a write that carries no rotation
    /// configuration at all.
    feature: Option<&'static str>,
    /// `RefreshTokenRotation.RetryGracePeriodSeconds`, when the row asks for one.
    grace_seconds: Option<i32>,
    /// `ExplicitAuthFlows`, or `None` for a write that names no flows.
    flows: Option<&'static [&'static str]>,
    /// The flow the refusal names, or `None` for a configuration that is accepted.
    refused_flow: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: rotation on, the legacy flow beside a password flow",
            feature: Some("ENABLED"),
            grace_seconds: Some(MIN_GRACE_SECONDS),
            flows: Some(&["ALLOW_USER_PASSWORD_AUTH", LEGACY_REFRESH_FLOW]),
            refused_flow: Some(LEGACY_REFRESH_FLOW),
        },
        Row {
            case: "rotation on, the legacy flow the only flow named",
            feature: Some("ENABLED"),
            grace_seconds: None,
            flows: Some(&[LEGACY_REFRESH_FLOW]),
            refused_flow: Some(LEGACY_REFRESH_FLOW),
        },
        Row {
            case: "rotation on with the longest grace period, the legacy flow named first",
            feature: Some("ENABLED"),
            grace_seconds: Some(MAX_GRACE_SECONDS),
            flows: Some(&[
                LEGACY_REFRESH_FLOW,
                "ALLOW_USER_SRP_AUTH",
                "ALLOW_CUSTOM_AUTH",
            ]),
            refused_flow: Some(LEGACY_REFRESH_FLOW),
        },
        Row {
            case: "rotation on, the legacy flow last in a longer list",
            feature: Some("ENABLED"),
            grace_seconds: Some(30),
            flows: Some(&[
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_USER_AUTH",
                LEGACY_REFRESH_FLOW,
            ]),
            refused_flow: Some(LEGACY_REFRESH_FLOW),
        },
        Row {
            case: "rotation on beside flows that do not include the legacy one",
            feature: Some("ENABLED"),
            grace_seconds: Some(MIN_GRACE_SECONDS),
            flows: Some(&ACCEPTED_FLOWS),
            refused_flow: None,
        },
        Row {
            case: "rotation on with the longest grace period and no flows named at all",
            feature: Some("ENABLED"),
            grace_seconds: Some(MAX_GRACE_SECONDS),
            flows: None,
            refused_flow: None,
        },
        Row {
            case: "rotation switched OFF beside the legacy flow",
            feature: Some("DISABLED"),
            grace_seconds: Some(MIN_GRACE_SECONDS),
            flows: Some(&["ALLOW_USER_PASSWORD_AUTH", LEGACY_REFRESH_FLOW]),
            refused_flow: None,
        },
        Row {
            case: "the legacy flow with no rotation configuration at all",
            feature: None,
            grace_seconds: None,
            flows: Some(&["ALLOW_USER_PASSWORD_AUTH", LEGACY_REFRESH_FLOW]),
            refused_flow: None,
        },
    ]
}

impl Row {
    /// The `RefreshTokenRotation` member this row carries, when it carries one.
    fn rotation(&self) -> Option<Value> {
        let feature = self.feature?;
        let mut rotation = Map::new();
        rotation.insert("Feature".to_string(), json!(feature));
        if let Some(seconds) = self.grace_seconds {
            rotation.insert("RetryGracePeriodSeconds".to_string(), json!(seconds));
        }
        Some(Value::Object(rotation))
    }

    /// The app-client configuration this row writes, under the pool `user_pool_id`.
    fn configuration(&self, user_pool_id: &str) -> Map<String, Value> {
        let mut body = Map::new();
        body.insert("UserPoolId".to_string(), json!(user_pool_id));
        if let Some(flows) = self.flows {
            body.insert("ExplicitAuthFlows".to_string(), json!(flows));
        }
        if let Some(rotation) = self.rotation() {
            body.insert("RefreshTokenRotation".to_string(), rotation);
        }
        body
    }
}

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    router
}

/// Dispatch `operation` with `body` and answer the status and the JSON response body.
async fn dispatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", ENDPOINT_HOST)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{SIGNING_NAME}/\
                 aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        );
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and answer its response body.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// The sentence a write that switches rotation on while naming `flow` is refused with.
///
/// Fixed text but for the flow NAME the request carried, which is the value the caller
/// has to drop.
fn refusal_message(flow: &str) -> String {
    format!("{flow} is not a permitted ExplicitAuthFlow when refresh token rotation is enabled.")
}

/// Call `operation` expecting the parameter refusal `flow` earns, and assert its status,
/// its wire error code and its message.
async fn refused(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value, flow: &str) {
    let (status, payload) = dispatch(router, operation, body).await;
    let (expected_status, expected_code) = INVALID_PARAMETER;
    assert_eq!(
        (status, error_code(&payload)),
        (expected_status, expected_code),
        "case {case:?}: {operation} must be refused ({expected_status}, {expected_code:?}), got \
         ({status}, {:?}) with {payload}",
        error_code(&payload)
    );
    let expected = refusal_message(flow);
    assert_eq!(
        error_message(&payload),
        expected,
        "case {case:?}: {operation} must state {expected:?}, got {:?}",
        error_message(&payload)
    );
}

/// Create a pool and answer its id.
async fn pool(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    ok(router, case, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create answers a minted pool id"))
        .to_string()
}

/// The `ClientId` of a created app client.
fn client_id(case: &str, payload: &Value) -> String {
    payload["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create answers a minted client id, got {payload}"))
        .to_string()
}

/// The app-client names `ListUserPoolClients` reports for `user_pool_id`.
async fn listed_client_names(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
) -> Vec<String> {
    let listed = ok(
        router,
        case,
        "ListUserPoolClients",
        &json!({ "UserPoolId": user_pool_id, "MaxResults": 60 }),
    )
    .await;
    listed["UserPoolClients"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a listing reports an array, got {listed}"))
        .iter()
        .map(|client| {
            client["ClientName"]
                .as_str()
                .unwrap_or("<no ClientName>")
                .to_string()
        })
        .collect()
}

/// The app client `DescribeUserPoolClient` reports.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
    client_id: &str,
) -> Value {
    ok(
        router,
        case,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await["UserPoolClient"]
        .clone()
}

/// RULE: a create that switches refresh token rotation on while naming the legacy
/// refresh-token flow is refused `InvalidParameterException` and mints no app client;
/// every other pairing of the two members is accepted and echoed back.
#[tokio::test]
async fn a_create_refuses_rotation_together_with_the_legacy_refresh_flow() {
    let router = router();
    let user_pool_id = pool(&router, "create", "rotation-flow-create-pool").await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("rotation-flow-client-{index}");
        let mut body = row.configuration(&user_pool_id);
        body.insert("ClientName".to_string(), json!(name));
        let body = Value::Object(body);

        match row.refused_flow {
            Some(flow) => {
                refused(&router, case, "CreateUserPoolClient", &body, flow).await;
                // A refused create mints nothing: the pool holds no app client of that
                // name.
                let listed = listed_client_names(&router, case, &user_pool_id).await;
                assert!(
                    !listed.contains(&name),
                    "case {case:?}: a refused create must leave no app client named \
                     {name:?}, got {listed:?}"
                );
            }
            None => {
                let created = ok(&router, case, "CreateUserPoolClient", &body).await;
                let client_id = client_id(case, &created);
                // The accepted configuration is what the app client HOLDS, read back
                // through the describe.
                let stored = described(&router, case, &user_pool_id, &client_id).await;
                let expected_rotation = row.rotation();
                let actual_rotation = stored.get("RefreshTokenRotation").cloned();
                assert_eq!(
                    actual_rotation, expected_rotation,
                    "case {case:?}: the app client must hold RefreshTokenRotation \
                     {expected_rotation:?}, got {actual_rotation:?}"
                );
                if let Some(flows) = row.flows {
                    let stored_flows = stored["ExplicitAuthFlows"].clone();
                    assert_eq!(
                        stored_flows,
                        json!(flows),
                        "case {case:?}: the app client must hold ExplicitAuthFlows \
                         {flows:?}, got {stored_flows}"
                    );
                }
            }
        }
    }
}

/// RULE: the same pairing refused on a create is refused on an update, and a refused
/// update changes nothing — the app client keeps the flows and the rotation configuration
/// it held.
#[tokio::test]
async fn a_refused_update_leaves_the_app_client_exactly_as_it_was() {
    let router = router();
    let user_pool_id = pool(&router, "update", "rotation-flow-update-pool").await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        // The app client every row updates: rotation on beside flows the rule accepts,
        // which is the configuration a caller enabling rotation starts from.
        let created = ok(
            &router,
            case,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientName": format!("rotation-flow-update-client-{index}"),
                "ExplicitAuthFlows": ACCEPTED_FLOWS,
                "RefreshTokenRotation": {
                    "Feature": "ENABLED",
                    "RetryGracePeriodSeconds": MIN_GRACE_SECONDS,
                },
            }),
        )
        .await;
        let client_id = client_id(case, &created);
        let before = described(&router, case, &user_pool_id, &client_id).await;

        let mut body = row.configuration(&user_pool_id);
        body.insert("ClientId".to_string(), json!(client_id));
        let body = Value::Object(body);

        match row.refused_flow {
            Some(flow) => {
                refused(&router, case, "UpdateUserPoolClient", &body, flow).await;
                let after = described(&router, case, &user_pool_id, &client_id).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused update must change nothing, got {after} \
                     where the app client held {before}"
                );
            }
            None => {
                let updated = ok(&router, case, "UpdateUserPoolClient", &body).await;
                let stored = described(&router, case, &user_pool_id, &client_id).await;
                assert_eq!(
                    updated["UserPoolClient"], stored,
                    "case {case:?}: the update's answer must be what a later read sees, \
                     got {} and {stored}",
                    updated["UserPoolClient"]
                );
                let expected_rotation = row.rotation();
                let actual_rotation = stored.get("RefreshTokenRotation").cloned();
                assert_eq!(
                    actual_rotation, expected_rotation,
                    "case {case:?}: the app client must hold RefreshTokenRotation \
                     {expected_rotation:?}, got {actual_rotation:?}"
                );
            }
        }
    }
}
