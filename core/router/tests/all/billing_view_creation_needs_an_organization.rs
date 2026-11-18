// SPDX-License-Identifier: CC0-1.0

//! `CreateBillingView` is reserved to an organization's management account, and
//! the organization is AWS Organizations' resource — so the standing the gate
//! judges by is reachable over the wire: a client calls
//! `Organizations:CreateOrganization`, which makes the calling account the
//! management account, and the billing-view lifecycle opens up behind it.
//!
//! The rule: a fact another AWS service owns is read from THAT service's state
//! when the decision is made, so every request a real client can send to
//! establish it is enough to satisfy the gate, and nothing outside the process
//! has to reach into a struct. Only a cross-service suite can pin this — each
//! crate's own tests can only ever assert its own half.
//!
//! The rows vary what the rule generalises over: the feature set the
//! organization is created with (every value the model's `OrganizationFeatureSet`
//! declares, plus the request that omits the member), crossed with
//! `CreateBillingView` requests differing in name length and shape, description,
//! tags, idempotency token and source-view count. Behind the opened gate the
//! suite drives the rest of the lifecycle the created view makes reachable —
//! update, associate, list sources, tag, list tags and delete.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::NOT_ORGANIZATION_MANAGEMENT_ACCOUNT;
use service_com_amazonaws_organizations_awsorganizationsv20161128::ops::types::ORGANIZATION_FEATURE_SET_VALUES;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names `service`, which
/// is the routing key the front door reads. The signature is never checked: the
/// router here runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// Billing and Organizations mounted on ONE engine, as `core/router/src/lib.rs`
/// mounts them, over the state a freshly booted emulator holds: an account in no
/// organization.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_billing_awsbilling::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(engine),
    );
    router
}

/// Send one awsJson call and return its HTTP status and decoded body.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.us-east-1.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, decoded)
}

/// One Billing call, over its awsJson1_0 wire.
async fn billing(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        "billing",
        "application/x-amz-json-1.0",
        &format!("AWSBilling.{operation}"),
        body,
    )
    .await
}

/// One Organizations call, over its awsJson1_1 wire.
async fn organizations(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        "organizations",
        "application/x-amz-json-1.1",
        &format!("AWSOrganizationsV20161128.{operation}"),
        body,
    )
    .await
}

/// The `__type` an awsJson error body carries, without its namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The account every emulated service presents, and so the management account of
/// the organization `CreateOrganization` creates.
fn account_id() -> &'static str {
    engine::emulator_account_id()
}

/// The ARN of the `PRIMARY` billing view the account owns by default — the one
/// source view that exists before anything is created.
fn primary_arn() -> String {
    format!("arn:aws:billing::{}:billingview/primary", account_id())
}

/// `CreateBillingView` requests differing in every member the operation takes:
/// the name at both modelled length bounds (1 and 128) and in a shape its
/// pattern admits, with and without a description, with tags, and with an
/// idempotency token. Source:
/// `smithy_query(query="operation", operation="CreateBillingView")`.
fn create_requests() -> Vec<(String, Value)> {
    let primary = primary_arn();
    vec![
        (
            "the shortest name the model admits".to_string(),
            json!({ "name": "a", "sourceViews": [primary] }),
        ),
        (
            "the longest name the model admits".to_string(),
            json!({ "name": "n".repeat(128), "sourceViews": [primary] }),
        ),
        (
            "a described, tagged view".to_string(),
            json!({
                "name": "a view _+=.-@ 1",
                "description": "spend of one team",
                "sourceViews": [primary],
                "resourceTags": [{ "key": "team", "value": "finance" }],
            }),
        ),
        (
            "an idempotent request".to_string(),
            json!({
                "name": "with-token",
                "sourceViews": [primary],
                "clientToken": "organization-gate-1",
            }),
        ),
    ]
}

/// The names of the `CUSTOM` views the account holds.
async fn custom_view_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = billing(
        router,
        "ListBillingViews",
        &json!({ "billingViewTypes": ["CUSTOM"] }),
    )
    .await;
    assert_eq!(status, 200, "the control listing answers, got {listed:?}");
    listed["billingViews"]
        .as_array()
        .expect("billingViews is a list")
        .iter()
        .map(|view| view["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn creating_an_organization_over_the_wire_opens_billing_view_creation() {
    // The feature sets a client can ask for: every value the model declares
    // (source: `ORGANIZATION_FEATURE_SET_VALUES`, the model's
    // `OrganizationFeatureSet` enum), plus the request that omits the member and
    // takes the operation's documented default. The management account of the
    // organization is the calling account in each, so the gate must open in each.
    let mut feature_sets: Vec<Option<&str>> = vec![None];
    feature_sets.extend(ORGANIZATION_FEATURE_SET_VALUES.iter().copied().map(Some));

    for feature_set in feature_sets {
        let world = format!("featureSet {feature_set:?}");
        let router = build_router();

        // Before any organization exists the account is standalone, so every
        // request — whatever it carries — reads the management-account refusal.
        for (what, body) in create_requests() {
            let case = format!("{what}, before the organization is created, {world}");
            let (status, got) = billing(&router, "CreateBillingView", &body).await;
            assert_eq!(
                (status, error_code(&got).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {got:?}"
            );
            assert_eq!(
                got["message"].as_str(),
                Some(NOT_ORGANIZATION_MANAGEMENT_ACCOUNT),
                "case {case:?}: expected the management-account sentence, got {got:?}"
            );
        }
        let names = custom_view_names(&router).await;
        assert!(
            names.is_empty(),
            "case {world:?}: a refused create stores no view, listed {names:?}"
        );

        // The one request that establishes the fact, sent by a client that
        // speaks only HTTP.
        let request = match feature_set {
            None => json!({}),
            Some(value) => json!({ "FeatureSet": value }),
        };
        let (status, created) = organizations(&router, "CreateOrganization", &request).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected the organization to be created, got {status} {created:?}"
        );
        assert_eq!(
            created["Organization"]["MasterAccountId"].as_str(),
            Some(account_id()),
            "case {world:?}: the calling account manages the new organization, got {created:?}"
        );

        // Behind the established fact every one of those same requests is
        // served, and what it stored reads back through Get.
        let mut minted = Vec::new();
        for (what, body) in create_requests() {
            let case = format!("{what}, after the organization is created, {world}");
            let (status, created) = billing(&router, "CreateBillingView", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be served, got {status} {created:?}"
            );
            let arn = created["arn"].as_str().unwrap_or_default().to_string();
            assert!(
                arn.starts_with(&format!("arn:aws:billing::{}:billingview/", account_id())),
                "case {case:?}: expected a billing-view ARN of the caller's account, got {arn:?}"
            );
            assert!(
                created["createdAt"].is_number(),
                "case {case:?}: the modelled createdAt is reported, got {created:?}"
            );

            let (status, got) = billing(&router, "GetBillingView", &json!({ "arn": arn })).await;
            assert_eq!(
                (status, got["billingView"]["name"].as_str()),
                (200, body["name"].as_str()),
                "case {case:?}: the created view reads back, got {status} {got:?}"
            );
            assert_eq!(
                got["billingView"]["billingViewType"].as_str(),
                Some("CUSTOM"),
                "case {case:?}: a created view is CUSTOM, got {got:?}"
            );
            minted.push(arn);
        }

        // The replay of the request that carried a token answers with the ARN
        // its first call minted, rather than a second view.
        let (_, replayed) = billing(
            &router,
            "CreateBillingView",
            &create_requests()
                .last()
                .map(|(_, body)| body.clone())
                .expect("the request table is not empty"),
        )
        .await;
        assert_eq!(
            replayed["arn"].as_str(),
            minted.last().map(String::as_str),
            "case {world:?}: the idempotent replay answers with the first ARN, got {replayed:?}"
        );

        let names = custom_view_names(&router).await;
        assert_eq!(
            names.len(),
            minted.len(),
            "case {world:?}: every served create is listed once, listed {names:?}"
        );

        // A view minted over TWO source views: one of them the account's
        // PRIMARY, the other a view the calls above created.
        let over_two = json!({
            "name": "over-two-sources",
            "sourceViews": [primary_arn(), minted[0].clone()],
        });
        let (status, created) = billing(&router, "CreateBillingView", &over_two).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected a two-source create to be served, got {status} {created:?}"
        );
        let composite = created["arn"].as_str().unwrap_or_default().to_string();
        let (status, sources) = billing(
            &router,
            "ListSourceViewsForBillingView",
            &json!({ "arn": composite }),
        )
        .await;
        let listed: Vec<&str> = sources["sourceViews"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            (status, listed.len()),
            (200, 2),
            "case {world:?}: both sources are reported, got {status} {sources:?}"
        );
    }
}

#[tokio::test]
async fn the_lifecycle_behind_the_gate_is_reachable_over_the_wire() {
    // The operations that can only ever answer their refusal arm while no view
    // can be created: each one is driven here against a view created over the
    // wire, after an organization was established the same way.
    let router = build_router();
    let (status, created) = organizations(&router, "CreateOrganization", &json!({})).await;
    assert_eq!(
        status, 200,
        "the organization is created, got {status} {created:?}"
    );

    let (status, created) = billing(
        &router,
        "CreateBillingView",
        &json!({ "name": "lifecycle", "sourceViews": [primary_arn()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the view is created, got {status} {created:?}"
    );
    let arn = created["arn"].as_str().unwrap_or_default().to_string();

    let (status, updated) = billing(
        &router,
        "UpdateBillingView",
        &json!({ "arn": arn, "name": "lifecycle-renamed" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"update\": got {status} {updated:?}"
    );
    let (status, got) = billing(&router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        (status, got["billingView"]["name"].as_str()),
        (200, Some("lifecycle-renamed")),
        "case \"update\": the new name reads back, got {status} {got:?}"
    );

    // A second view to associate as a source of the first.
    let (status, second) = billing(
        &router,
        "CreateBillingView",
        &json!({ "name": "lifecycle-source", "sourceViews": [primary_arn()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"associate\": the source view is created, got {status} {second:?}"
    );
    let source = second["arn"].as_str().unwrap_or_default().to_string();
    let (status, associated) = billing(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": arn, "sourceViews": [source] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"associate\": got {status} {associated:?}"
    );
    let (status, sources) = billing(
        &router,
        "ListSourceViewsForBillingView",
        &json!({ "arn": arn }),
    )
    .await;
    let listed: Vec<&str> = sources["sourceViews"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        (status, listed.contains(&source.as_str())),
        (200, true),
        "case \"associate\": the associated source is reported, got {status} {sources:?}"
    );

    let (status, tagged) = billing(
        &router,
        "TagResource",
        &json!({
            "resourceArn": arn,
            "resourceTags": [{ "key": "owner", "value": "finance" }],
        }),
    )
    .await;
    assert_eq!(status, 200, "case \"tag\": got {status} {tagged:?}");
    let (status, tags) = billing(
        &router,
        "ListTagsForResource",
        &json!({ "resourceArn": arn }),
    )
    .await;
    let reported: Vec<(&str, &str)> = tags["resourceTags"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|tag| {
                    (
                        tag["key"].as_str().unwrap_or_default(),
                        tag["value"].as_str().unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        (status, reported.as_slice()),
        (200, [("owner", "finance")].as_slice()),
        "case \"tag\": the stored tag reads back, got {status} {tags:?}"
    );

    let (status, deleted) = billing(&router, "DeleteBillingView", &json!({ "arn": arn })).await;
    assert_eq!(status, 200, "case \"delete\": got {status} {deleted:?}");
    let (status, got) = billing(&router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        (status, error_code(&got).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"delete\": the deleted view is gone, got {status} {got:?}"
    );
}
