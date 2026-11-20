// SPDX-License-Identifier: CC0-1.0

//! EC2 Capacity Manager's ORGANIZATION-wide scope is about an organization, and the
//! organization is AWS Organizations' own resource — so the standing EC2 judges the scope by is
//! reachable over the wire: a client calls `Organizations:CreateOrganization` and the scope
//! opens up behind it.
//!
//! The rule under test: **`UpdateCapacityManagerOrganizationsAccess` "controls whether Capacity
//! Manager can aggregate data from all accounts in your Amazon Web Services Organization or
//! only from the current account", so an account that belongs to no organization has no
//! organization to aggregate across and is refused 400
//! `CapacityManager.OrganizationTrustedAccessDisabled` with "This account can't access Capacity
//! Manager with AWS Organizations. Enable trusted access or use account mode.", storing
//! nothing; once an organization exists, the same call is served and the scope it records is
//! the one `GetCapacityManagerAttributes` then reports.** Whether the account is in an
//! organization is read from Organizations' own store at the moment of the call — EC2 keeps no
//! copy of it — so only a cross-service suite can pin it: each crate's own tests can assert one
//! half.
//!
//! Why it matters: a client that widened the scope of its capacity analytics without belonging
//! to an organization would be told the aggregation covers accounts no component holds.
//!
//! The rows differ along what the rule generalises over: the feature set the organization is
//! created with (which this scope does not turn on), and the direction each update asks for —
//! widening the scope to the organization, narrowing it back to the account, and widening it
//! again — each asserted against what a later read reports.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, xml_escape};

/// The Region every call here is signed for. Capacity Manager's settings are a fact of the
/// account and an organization is partition-global, so the Region selects nothing; one is
/// needed because the credential scope is what routes a request.
const REGION: &str = "us-east-1";

/// The code and the sentence EC2 refuses the organization-wide scope with for an account that
/// belongs to no organization. Spelled out here rather than read from the crate, because the
/// wording IS the requirement.
const TRUSTED_ACCESS_DISABLED: &str = "CapacityManager.OrganizationTrustedAccessDisabled";
const TRUSTED_ACCESS_DISABLED_MESSAGE: &str = "This account can't access Capacity Manager with \
     AWS Organizations. Enable trusted access or use account mode.";

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// EC2 and Organizations mounted on ONE engine, as `core/router/src/lib.rs` mounts them, over
/// the state a freshly booted emulator holds: an account in no organization.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(engine),
    );
    router
}

/// One EC2 call over its ec2Query wire.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// One Organizations call over its awsJson1_1 wire.
async fn organizations(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("organizations.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSOrganizationsV20161128.{operation}"),
        )
        .header("authorization", authorization("organizations"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, decoded)
}

/// Enable Capacity Manager for the account, for the account's own scope — the state every row
/// below starts from, so no row is refused for Capacity Manager being off.
async fn enable_capacity_manager(router: &MultiServiceRouter, case: &str) {
    let (status, body) = ec2(
        router,
        "EnableCapacityManager",
        &[("OrganizationsAccess", "false")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected EnableCapacityManager to be served, got {status} {body}"
    );
}

/// Ask `UpdateCapacityManagerOrganizationsAccess` for `organizations_access`.
async fn update_scope(
    router: &MultiServiceRouter,
    organizations_access: &str,
) -> (u16, String) {
    ec2(
        router,
        "UpdateCapacityManagerOrganizationsAccess",
        &[("OrganizationsAccess", organizations_access)],
    )
    .await
}

/// The `organizationsAccess` `GetCapacityManagerAttributes` reports, which must be served.
async fn reported_scope(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = ec2(router, "GetCapacityManagerAttributes", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the read to be served, got {status} {body}"
    );
    opt_element(&body, "organizationsAccess")
        .unwrap_or_else(|| panic!("case {case:?}: expected an organizationsAccess, got {body}"))
}

#[tokio::test]
async fn the_organization_wide_scope_needs_an_organization() {
    // The feature set the organization is created with, which this scope does not turn on: the
    // member's own value set, plus the request that omits it.
    for feature_set in [None, Some("ALL"), Some("CONSOLIDATED_BILLING")] {
        let world = match feature_set {
            None => "an organization created with no FeatureSet".to_string(),
            Some(value) => format!("an organization created with FeatureSet={value}"),
        };
        let router = build_router();
        enable_capacity_manager(&router, &world).await;

        // Before the organization exists: the account has none to aggregate across, so the
        // widening is refused in EC2's own words and stores nothing.
        let case = format!("{world}: before it exists");
        let (status, body) = update_scope(&router, "true").await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refusal, got {status} {body}"
        );
        assert_eq!(
            (
                opt_element(&body, "Code").unwrap_or_default(),
                opt_element(&body, "Message").unwrap_or_default(),
            ),
            (
                TRUSTED_ACCESS_DISABLED.to_string(),
                // The wire carries the sentence XML-escaped, apostrophe included.
                xml_escape(TRUSTED_ACCESS_DISABLED_MESSAGE),
            ),
            "case {case:?}: expected the no-organization refusal, got {body}"
        );
        let reported = reported_scope(&router, &case).await;
        assert_eq!(
            reported, "false",
            "case {case:?}: expected the refused update to store nothing, got {reported}"
        );

        // The organization, created over the wire by the account that will widen the scope.
        let body = match feature_set {
            None => json!({}),
            Some(value) => json!({ "FeatureSet": value }),
        };
        let (status, created) = organizations(&router, "CreateOrganization", &body).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected CreateOrganization to be served, got {status} {created}"
        );

        // Behind it, each direction of the update is served, and the scope a read reports is
        // the one the update recorded.
        for (step, (asks_for, expected)) in [("true", "true"), ("false", "false"), ("true", "true")]
            .into_iter()
            .enumerate()
        {
            let case = format!("{world}: step {step} asking for {asks_for}");
            let (status, body) = update_scope(&router, asks_for).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status} {body}"
            );
            assert_eq!(
                opt_element(&body, "organizationsAccess").unwrap_or_default(),
                expected,
                "case {case:?}: expected the update to answer organizationsAccess {expected}, \
                 got {body}"
            );
            assert_eq!(
                opt_element(&body, "capacityManagerStatus").unwrap_or_default(),
                "enabled",
                "case {case:?}: expected the status to stay enabled, got {body}"
            );
            let reported = reported_scope(&router, &case).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the stored organizationsAccess {expected}, \
                 got {reported}"
            );
        }
    }
}

/// The organization is what the gate reads, and nothing of Capacity Manager's own: an account
/// in an organization whose scope is already narrowed to itself is refused the NARROWING (there
/// is nothing to turn off) and served the WIDENING — the two refusals this operation gives are
/// about different things.
#[tokio::test]
async fn the_two_refusals_are_about_different_things() {
    let router = build_router();
    let case = "an account in an organization, scope narrowed to itself";
    enable_capacity_manager(&router, case).await;
    let (status, created) = organizations(&router, "CreateOrganization", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateOrganization to be served, got {status} {created}"
    );

    // Nothing to turn off: the account is not aggregating across the organization.
    let (status, body) = update_scope(&router, "false").await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the narrowing to be refused, got {status} {body}"
    );
    assert_eq!(
        (
            opt_element(&body, "Code").unwrap_or_default(),
            opt_element(&body, "Message").unwrap_or_default(),
        ),
        (
            "InvalidParameterValue".to_string(),
            xml_escape("Capacity Manager doesn't have organizations access enabled."),
        ),
        "case {case:?}: expected the nothing-to-turn-off refusal, got {body}"
    );

    // The widening the organization allows.
    let (status, body) = update_scope(&router, "true").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the widening to be served, got {status} {body}"
    );
    let reported = reported_scope(&router, case).await;
    assert_eq!(
        reported, "true",
        "case {case:?}: expected the stored organizationsAccess true, got {reported}"
    );
}
