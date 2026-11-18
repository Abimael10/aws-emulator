// SPDX-License-Identifier: CC0-1.0

//! Token customization through a PRE-TOKEN-GENERATION trigger against the user
//! pool's FEATURE PLAN, over the wire exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a pool configuration that asks for token customization —
//! a `LambdaConfig.PreTokenGenerationConfig` whose `LambdaVersion` is above the
//! base version, since "higher-numbered versions add fields that support new
//! features"
//! (`smithy_query(query="shape",shape="PreTokenGenerationVersionConfigType")`) —
//! is refused on a plan that does not include it, with the plan the request
//! leaves the pool on; a configuration that asks for the base version, the
//! legacy `PreTokenGeneration` member or no trigger at all asks for nothing
//! gated and is served on every plan.
//!
//! The rows differ along the trigger version asked for (every modelled
//! `PreTokenGenerationLambdaVersionType` value, including `V3_0`, which the
//! issue did not cite), along the plan the pool is on (every modelled
//! `UserPoolTierType` value plus the request that names none) and along which
//! operation configures it (`CreateUserPool` and `UpdateUserPool`). Each
//! assertion names its case and reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The modelled feature plans in INCLUSION order, cheapest first
/// (`smithy_query(query="shape",shape="UserPoolTierType")`).
const PLANS_IN_ORDER: [&str; 3] = ["LITE", "ESSENTIALS", "PLUS"];

/// The plan a request that names none carries: "Defaults to `ESSENTIALS`"
/// (`CreateUserPool.UserPoolTier`).
const DEFAULT_PLAN: &str = "ESSENTIALS";

/// Every modelled pre-token-generation trigger version
/// (`smithy_query(query="shape",shape="PreTokenGenerationLambdaVersionType")`).
const TRIGGER_VERSIONS: [&str; 3] = ["V1_0", "V2_0", "V3_0"];

/// The version that carries no token-customization fields: the legacy
/// `PreTokenGeneration` member predates every higher one
/// (`smithy_query(query="doc",shape="LambdaConfigType",member="PreTokenGeneration")`).
const BASE_TRIGGER_VERSION: &str = "V1_0";

/// The cheapest plan that includes token customization.
const TOKEN_CUSTOMIZATION_FLOOR: &str = "ESSENTIALS";

/// An `ArnType`-shaped function ARN (len 20..2048) standing for the trigger the
/// pool invokes.
const TRIGGER_ARN: &str = "arn:aws:lambda:us-east-1:123456789012:function:pre-token-generation";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
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

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
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

/// The `message` member of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Whether `plan` includes a feature whose floor is `floor`: the plans are
/// ordered by inclusion, so every plan from the floor upwards includes it.
fn plan_includes(plan: &str, floor: &str) -> bool {
    let rank = |named: &str| {
        PLANS_IN_ORDER
            .iter()
            .position(|known| *known == named)
            .unwrap_or_else(|| panic!("{named} is one of the modelled plans"))
    };
    rank(plan) >= rank(floor)
}

/// The refusal a pool whose OWN configuration asks for token customization with
/// `version` owes the plan `plan` it is left on.
fn refusal_message(plan: &str, version: &str) -> String {
    format!(
        "The following features need to be disabled for the {plan} pricing tier configured: \
         Token Customization with Pre-Token Generation Lambda {version}"
    )
}

/// A `LambdaConfig` configuring a pre-token-generation trigger of `version`.
fn versioned_trigger(version: &str) -> Value {
    json!({
        "PreTokenGenerationConfig": { "LambdaVersion": version, "LambdaArn": TRIGGER_ARN },
    })
}

/// Create a pool named `name`, on `plan` when one is named, with `lambda_config`
/// when one is given, and answer the call's status and body.
async fn create_pool(
    router: &MultiServiceRouter,
    name: &str,
    plan: Option<&str>,
    lambda_config: Option<Value>,
) -> (u16, Value) {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(name));
    if let Some(plan) = plan {
        body.insert("UserPoolTier".to_string(), json!(plan));
    }
    if let Some(lambda_config) = lambda_config {
        body.insert("LambdaConfig".to_string(), lambda_config);
    }
    call(router, "CreateUserPool", &Value::Object(body)).await
}

/// The STORED pool, as the reporting operation renders it.
async fn described_pool(router: &MultiServiceRouter, user_pool_id: &str) -> Value {
    ok(
        router,
        "DescribeUserPool",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await["UserPool"]
        .clone()
}

/// `UpdateUserPool` leaving the pool on `plan` with `lambda_config`.
async fn update_pool(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    plan: Option<&str>,
    lambda_config: Value,
) -> (u16, Value) {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("LambdaConfig".to_string(), lambda_config);
    if let Some(plan) = plan {
        body.insert("UserPoolTier".to_string(), json!(plan));
    }
    call(router, "UpdateUserPool", &Value::Object(body)).await
}

/// The plans a row is driven over: each modelled one, plus the request that
/// names none and so carries the documented default.
fn plans() -> Vec<(String, Option<&'static str>, &'static str)> {
    PLANS_IN_ORDER
        .iter()
        .map(|plan| (format!("the {plan} plan"), Some(*plan), *plan))
        .chain(std::iter::once((
            "the default plan".to_string(),
            None,
            DEFAULT_PLAN,
        )))
        .collect()
}

#[tokio::test]
async fn an_update_asking_for_token_customization_is_refused_below_its_floor_plan() {
    let router = router();

    for (index, version) in TRIGGER_VERSIONS.iter().enumerate() {
        for (plan_case, requested, effective) in plans() {
            let case = format!("a {version} pre-token-generation trigger on {plan_case}");
            let slug = format!("{index}-{}", requested.unwrap_or("default").to_lowercase());

            // Every pool starts with a trigger on the BASE version, which asks
            // for nothing gated: that create must be served on every plan, the
            // cheapest included.
            let (status, created) = create_pool(
                &router,
                &format!("token-customization-{slug}"),
                requested,
                Some(versioned_trigger(BASE_TRIGGER_VERSION)),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the base trigger version asks for nothing gated, got {status} \
                 with {created}"
            );
            let user_pool_id = created["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a minted pool id: {created}"))
                .to_string();

            let before = described_pool(&router, &user_pool_id).await;
            let (status, payload) = update_pool(
                &router,
                &user_pool_id,
                requested,
                versioned_trigger(version),
            )
            .await;
            let after = described_pool(&router, &user_pool_id).await;

            // The base version asks for nothing gated, and every plan from the
            // floor upwards includes token customization.
            if *version == BASE_TRIGGER_VERSION
                || plan_includes(effective, TOKEN_CUSTOMIZATION_FLOOR)
            {
                assert_eq!(
                    status, 200,
                    "case {case:?}: this plan serves the configuration, got {status} with \
                     {payload}"
                );
                assert_eq!(
                    after["LambdaConfig"]["PreTokenGenerationConfig"],
                    versioned_trigger(version)["PreTokenGenerationConfig"],
                    "case {case:?}: the stored trigger must read back, got {after}"
                );
                continue;
            }

            assert_eq!(
                (status, error_code(&payload)),
                (400, "FeatureUnavailableInTierException"),
                "case {case:?}: expected a 400 FeatureUnavailableInTierException, got {status} \
                 with {payload}"
            );
            let expected = refusal_message(effective, version);
            let message = error_message(&payload);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // A refused update writes nothing: the pool still reports the
            // trigger it already had, on the base version.
            assert_eq!(
                after, before,
                "case {case:?}: a refused update must leave the pool unchanged, got {after}"
            );
            assert_eq!(
                after["LambdaConfig"]["PreTokenGenerationConfig"]["LambdaVersion"],
                json!(BASE_TRIGGER_VERSION),
                "case {case:?}: the pool must keep the trigger version it held, got {after}"
            );
        }
    }
}

#[tokio::test]
async fn the_plan_a_create_configures_judges_its_token_customization() {
    let router = router();

    for (index, version) in TRIGGER_VERSIONS.iter().enumerate() {
        for (plan_case, requested, effective) in plans() {
            let case = format!("a pool created with a {version} trigger on {plan_case}");
            let slug = format!("{index}-{}", requested.unwrap_or("default").to_lowercase());

            let pools_before = ok(&router, "ListUserPools", &json!({ "MaxResults": 60 })).await;
            let (status, payload) = create_pool(
                &router,
                &format!("created-token-customization-{slug}"),
                requested,
                Some(versioned_trigger(version)),
            )
            .await;

            // A request that names NO plan configures none, so nothing it asks
            // for can be refused for the plan: the pool lands on the cheapest
            // plan that includes what it asked for.
            let gated = *version != BASE_TRIGGER_VERSION;
            if gated && requested.is_some() && !plan_includes(effective, TOKEN_CUSTOMIZATION_FLOOR)
            {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (
                        400,
                        "FeatureUnavailableInTierException",
                        refusal_message(effective, version).as_str()
                    ),
                    "case {case:?}: expected the tier refusal naming the trigger version, got \
                     {status} with {payload}"
                );
                let pools_after = ok(&router, "ListUserPools", &json!({ "MaxResults": 60 })).await;
                assert_eq!(
                    pools_after, pools_before,
                    "case {case:?}: a refused create stores no pool, got {pools_after}"
                );
                continue;
            }

            assert_eq!(
                status, 200,
                "case {case:?}: the configuration is served, got {status} with {payload}"
            );
            let user_pool_id = payload["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a minted pool id: {payload}"))
                .to_string();
            let expected_plan = match requested {
                Some(requested) => requested,
                None if gated => TOKEN_CUSTOMIZATION_FLOOR,
                None => DEFAULT_PLAN,
            };
            let stored = described_pool(&router, &user_pool_id).await;
            assert_eq!(
                stored["UserPoolTier"],
                json!(expected_plan),
                "case {case:?}: the stored pool must be on {expected_plan:?}, got {stored}"
            );
            assert_eq!(
                stored["LambdaConfig"]["PreTokenGenerationConfig"],
                versioned_trigger(version)["PreTokenGenerationConfig"],
                "case {case:?}: the stored trigger must read back, got {stored}"
            );
        }
    }
}

#[tokio::test]
async fn raising_the_plan_lets_the_same_trigger_version_through() {
    let router = router();

    // The plan, not the function, is what the refusal is about: the SAME
    // trigger is refused on the cheapest plan and served once the pool is
    // raised to the floor plan. Every update below carries only what it is
    // about, so the plan the pool is judged against is the one it HOLDS: a
    // request that names no plan asks for no plan change.
    let (status, created) = create_pool(&router, "token-customization-upgrade", Some("LITE"), None)
        .await;
    assert_eq!(
        status, 200,
        "a pool on the cheapest plan is created, got {status} with {created}"
    );
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();

    let case = "the base trigger version on the cheapest plan";
    let (status, payload) = update_pool(
        &router,
        &user_pool_id,
        None,
        versioned_trigger(BASE_TRIGGER_VERSION),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the base version asks for nothing gated, got {status} with {payload}"
    );
    let stored = described_pool(&router, &user_pool_id).await;
    assert_eq!(
        stored["UserPoolTier"],
        json!("LITE"),
        "case {case:?}: an update naming no plan must leave the pool's plan alone, got {stored}"
    );

    // The same refusal whether the request repeats the plan the pool is on or
    // names none at all.
    for (case, requested) in [
        ("the same trigger raised to V2_0, naming no plan", None),
        (
            "the same trigger raised to V2_0, repeating the pool's plan",
            Some("LITE"),
        ),
    ] {
        let (status, payload) =
            update_pool(&router, &user_pool_id, requested, versioned_trigger("V2_0")).await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (
                400,
                "FeatureUnavailableInTierException",
                refusal_message("LITE", "V2_0").as_str()
            ),
            "case {case:?}: expected the tier refusal, got {status} with {payload}"
        );
        let stored = described_pool(&router, &user_pool_id).await;
        assert_eq!(
            stored["LambdaConfig"]["PreTokenGenerationConfig"]["LambdaVersion"],
            json!(BASE_TRIGGER_VERSION),
            "case {case:?}: the refused update must leave the stored version alone, got {stored}"
        );
    }

    let case = "raising the pool to the floor plan";
    let (status, payload) = call(
        &router,
        "UpdateUserPool",
        &json!({ "UserPoolId": &user_pool_id, "UserPoolTier": TOKEN_CUSTOMIZATION_FLOOR }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming a plan changes it, got {status} with {payload}"
    );

    let case = "the same trigger once the pool is raised to the floor plan";
    let (status, payload) =
        update_pool(&router, &user_pool_id, None, versioned_trigger("V2_0")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the floor plan serves token customization, got {status} with {payload}"
    );
    let stored = described_pool(&router, &user_pool_id).await;
    assert_eq!(
        stored["LambdaConfig"]["PreTokenGenerationConfig"],
        versioned_trigger("V2_0")["PreTokenGenerationConfig"],
        "case {case:?}: the stored trigger must read back, got {stored}"
    );
    assert_eq!(
        stored["UserPoolTier"],
        json!(TOKEN_CUSTOMIZATION_FLOOR),
        "case {case:?}: the pool must be on the plan the update named, got {stored}"
    );
}

#[tokio::test]
async fn a_trigger_configured_without_a_version_asks_for_nothing_gated() {
    let router = router();

    // The legacy `PreTokenGeneration` member is "the legacy configuration of a
    // pre token generation Lambda trigger ... Set this parameter for legacy
    // purposes"
    // (`smithy_query(query="doc",shape="LambdaConfigType",member="PreTokenGeneration")`),
    // and a pool that configures no trigger at all asks for nothing either, so
    // both are served on every plan.
    for (case, lambda_config) in [
        (
            "the legacy trigger member alone",
            json!({ "PreTokenGeneration": TRIGGER_ARN }),
        ),
        ("another trigger stage entirely", json!({ "PreSignUp": TRIGGER_ARN })),
        ("no trigger at all", json!({})),
    ] {
        for (plan_case, requested, effective) in plans() {
            let case = format!("{case} on {plan_case}");
            let slug = format!(
                "{}-{}",
                case.len(),
                requested.unwrap_or("default").to_lowercase()
            );
            let (status, created) = create_pool(
                &router,
                &format!("ungated-trigger-{slug}"),
                requested,
                Some(lambda_config.clone()),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: this configuration asks for nothing gated, got {status} with \
                 {created}"
            );
            let user_pool_id = created["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a minted pool id: {created}"))
                .to_string();
            let stored = described_pool(&router, &user_pool_id).await;
            assert_eq!(
                stored["UserPoolTier"],
                json!(effective),
                "case {case:?}: an ungated configuration must not raise the plan, got {stored}"
            );

            let (status, payload) =
                update_pool(&router, &user_pool_id, requested, lambda_config.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the same configuration through the update path is served, got \
                 {status} with {payload}"
            );
        }
    }
}
