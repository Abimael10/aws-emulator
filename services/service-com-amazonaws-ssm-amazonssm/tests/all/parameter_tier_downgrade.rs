// SPDX-License-Identifier: CC0-1.0

//! What `PutParameter` answers a request that would take an ADVANCED parameter back to the
//! standard tier, end to end over the front door.
//!
//! RULE under test: the tier upgrade is one way — "you can change a standard parameter to
//! an advanced parameter any time. But you can't revert an advanced parameter to a standard
//! parameter" (`smithy_query(query="doc", shape="PutParameterRequest", member="Tier")`) —
//! and the refusal is the service-common `ValidationException` whose text is WHOLLY FIXED:
//! it states the rule, the way out of it and the ceiling the standard tier holds a value
//! to, and carries nothing from the request. A request that does not ask for the lower tier
//! is not refused at all: a put that names no tier, or that leaves the tier to
//! `Intelligent-Tiering`, keeps the tier the parameter already has.
//!
//! Every case goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.PutParameter` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol").
//!
//! The table's rows differ along what the rule generalises over: the parameter's NAME and
//! its depth, the SIZE of the value the downgrade would store, and the tier the request
//! asks for — so a refusal that interpolated any of them could not pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The modelled class the refusal is reported as: the service-common `ValidationException`
/// (`smithy_query(query="operation", operation="PutParameter")`), HTTP 400.
const VALIDATION: &str = "ValidationException";

/// The sentence AWS refuses the downgrade with, in full. Fixed text throughout: the rule,
/// the remediation and the standard tier's own value limit in characters — no name, no size
/// and no tier of the request appears in it.
const DOWNGRADE_MESSAGE: &str = "This parameter uses the advanced-parameter tier. You can't \
                                 downgrade a parameter from the advanced-parameter tier to \
                                 the standard-parameter tier. If necessary, you can delete \
                                 the advanced parameter and recreate it as a standard \
                                 parameter. Be aware that standard parameters have a value \
                                 limit of 4096 characters.";

/// A value of `bytes` characters, which is what puts a parameter over a tier's content size
/// limit.
fn value_of(bytes: usize) -> String {
    "v".repeat(bytes)
}

/// Put `body`, returning `(status, body)`.
async fn put(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "PutParameter", body).await
}

/// Store `name` as an ADVANCED parameter holding `value`.
async fn advanced(router: &MultiServiceRouter, case: &str, name: &str, value: &str) {
    let (status, stored) = put(
        router,
        &json!({ "Name": name, "Type": "String", "Value": value, "Tier": "Advanced" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the advanced parameter must be stored, got {status} with {stored}"
    );
}

/// The tier `name` is reported in.
async fn tier_of(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, described) = call(
        router,
        "DescribeParameters",
        &json!({ "ParameterFilters": [{ "Key": "Name", "Values": [name] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must be served, got {status} with {described}"
    );
    described["Parameters"]
        .as_array()
        .and_then(|parameters| parameters.first())
        .and_then(|parameter| parameter["Tier"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: {name} must report a Tier, got {described}"))
        .to_string()
}

/// A put that asks for the standard tier on an advanced parameter is refused with AWS's
/// fixed sentence, whatever the parameter is called and whatever the put would store.
#[tokio::test]
async fn an_advanced_parameter_cannot_be_reverted_to_the_standard_tier() {
    let router = router();

    // (case, name, the value the advanced parameter holds, the value the downgrade stores)
    let rows: &[(&str, &str, usize, usize)] = &[
        (
            "a value over the standard tier's limit, downgraded to a small one",
            "/tiers/one",
            5 * 1024,
            1024,
        ),
        (
            "a deep name, downgraded to a value within the standard limit",
            "/tiers/deep/second/third/fourth",
            4 * 1024 + 1,
            16,
        ),
        (
            "a name with no hierarchy at all, downgraded to an empty-ish value",
            "flat-advanced-name",
            8 * 1024,
            1,
        ),
    ];

    for (case, name, held, downgraded) in rows {
        advanced(&router, case, name, &value_of(*held)).await;

        let (status, refused) = put(
            &router,
            &json!({
                "Name": name,
                "Type": "String",
                "Value": value_of(*downgraded),
                "Tier": "Standard",
                "Overwrite": true,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the downgrade must be refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {refused}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, DOWNGRADE_MESSAGE,
            "case {case:?}: expected AWS's fixed refusal text, got {message:?}"
        );
        assert!(
            !message.contains(name) && !message.contains(name.trim_start_matches('/')),
            "case {case:?}: the refusal carries nothing of the request, got {message:?}"
        );

        // The refusal changed nothing: the parameter is still the advanced one it was.
        assert_eq!(
            tier_of(&router, case, name).await,
            "Advanced",
            "case {case:?}: a refused downgrade leaves the parameter advanced"
        );
    }
}

/// A put that does NOT ask for the lower tier is served: the parameter keeps the tier it
/// has, and the upgrade in the other direction is allowed.
#[tokio::test]
async fn a_put_that_does_not_ask_for_the_lower_tier_is_served() {
    let router = router();

    // (case, the tier members the put carries, the tier the parameter is then in)
    let rows: &[(&str, Value, &str)] = &[
        (
            "a put naming no tier keeps the advanced tier",
            json!({}),
            "Advanced",
        ),
        (
            "intelligent tiering keeps an advanced parameter advanced",
            json!({ "Tier": "Intelligent-Tiering" }),
            "Advanced",
        ),
        (
            "a put naming the tier it is already in is no downgrade",
            json!({ "Tier": "Advanced" }),
            "Advanced",
        ),
    ];

    for (index, (case, tier, expected)) in rows.iter().enumerate() {
        let name = format!("/tiers/kept/{index}");
        advanced(&router, case, &name, &value_of(5 * 1024)).await;
        let mut body = json!({
            "Name": name,
            "Type": "String",
            "Value": value_of(64),
            "Overwrite": true,
        });
        if let Value::Object(members) = tier {
            for (member, value) in members {
                body[member] = value.clone();
            }
        }
        let (status, stored) = put(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be served, got {status} with {stored}"
        );
        assert_eq!(
            tier_of(&router, case, &name).await,
            *expected,
            "case {case:?}: expected the parameter to be {expected}"
        );
    }

    // The upgrade is the direction that IS allowed: a standard parameter becomes advanced.
    let case = "a standard parameter upgraded to the advanced tier";
    let name = "/tiers/upgraded";
    let (status, stored) = put(
        &router,
        &json!({ "Name": name, "Type": "String", "Value": value_of(16) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the standard parameter must be stored, got {status} with {stored}"
    );
    assert_eq!(
        tier_of(&router, case, name).await,
        "Standard",
        "case {case:?}: a put naming no tier creates a standard parameter"
    );
    let (status, upgraded) = put(
        &router,
        &json!({
            "Name": name,
            "Type": "String",
            "Value": value_of(5 * 1024),
            "Tier": "Advanced",
            "Overwrite": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the upgrade must be served, got {status} with {upgraded}"
    );
    assert_eq!(
        tier_of(&router, case, name).await,
        "Advanced",
        "case {case:?}: the upgrade leaves the parameter advanced"
    );
}
