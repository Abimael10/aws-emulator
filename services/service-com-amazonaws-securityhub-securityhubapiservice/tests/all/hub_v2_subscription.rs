// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the Security Hub V2 subscription: `EnableSecurityHubV2`
//! → `DescribeSecurityHubV2` → `EnableSecurityHubFeatureV2` /
//! `DisableSecurityHubFeatureV2`, plus `ListTagsForResource` over the `hubv2/default`
//! resource the enable creates.
//!
//! Every case drives the real restJson1 wire path through `MultiServiceRouter` — the
//! `@httpLabel`-bound `FeatureName`, the JSON bodies and the per-Region SigV4
//! credential scope — so a route, a status code or a serialized member that is wrong
//! on the wire fails here.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The cap the store's Region dimension carries, read from the crate rather than
// restated, so the case that drives it past the bound cannot drift from the bound.
use service_com_amazonaws_securityhub_securityhubapiservice::state::MAX_HUB_V2_REGIONS;

/// The Region the test signs for; the handler reads it out of the SigV4 credential
/// scope, exactly as a signed SDK request carries it.
const REGION: &str = "us-east-1";

/// A second Region, so a case can show the subscription is per account AND Region.
const OTHER_REGION: &str = "eu-west-2";

/// The account every emulated caller presents.
const ACCOUNT_ID: &str = "000000000000";

/// Every opt-in feature the model publishes: the values of the `FeatureName` enum
/// (`smithy_query(query="shape", shape="FeatureName")` reports exactly
/// `["NETWORK_SCANNING"]`), which is the set `DescribeSecurityHubV2` has to report a
/// status for.
const FEATURE_NAMES: &[&str] = &["NETWORK_SCANNING"];

/// The two `FeatureStatus` values (`smithy_query(query="shape",
/// shape="FeatureStatus")`).
const STATUS_ENABLED: &str = "ENABLED";
const STATUS_DISABLED: &str = "DISABLED";

/// One emulator with the Security Hub service mounted, driven over HTTP.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_securityhub_securityhubapiservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Dispatch one request and return `(status, parsed body)`. An empty body parses
    /// as `Value::Null`, which is what an operation with no output members returns.
    async fn call_in(
        &self,
        region: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let serialized = body.map(|b| serde_json::to_vec(&b).expect("serializable request body"));
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261109/{region}/securityhub/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(serialized.unwrap_or_default()))
            .expect("valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let parsed = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    async fn enable_v2_in(&self, region: &str, body: Value) -> (u16, Value) {
        self.call_in(region, "POST", "/hubv2", Some(body)).await
    }

    async fn enable_v2(&self, body: Value) -> (u16, Value) {
        self.enable_v2_in(REGION, body).await
    }

    async fn describe_v2_in(&self, region: &str) -> (u16, Value) {
        self.call_in(region, "GET", "/hubv2", None).await
    }

    async fn describe_v2(&self) -> (u16, Value) {
        self.describe_v2_in(REGION).await
    }

    async fn enable_feature_in(&self, region: &str, feature: &str) -> (u16, Value) {
        self.call_in(region, "POST", &format!("/hubv2/feature/{feature}"), None)
            .await
    }

    async fn enable_feature(&self, feature: &str) -> (u16, Value) {
        self.enable_feature_in(REGION, feature).await
    }

    async fn disable_feature_in(&self, region: &str, feature: &str) -> (u16, Value) {
        self.call_in(region, "DELETE", &format!("/hubv2/feature/{feature}"), None)
            .await
    }

    async fn disable_feature(&self, feature: &str) -> (u16, Value) {
        self.disable_feature_in(REGION, feature).await
    }

    async fn list_tags(&self, resource_arn: &str) -> (u16, Value) {
        self.call_in(
            REGION,
            "GET",
            &format!("/tags/{}", path_escape(resource_arn)),
            None,
        )
        .await
    }

    /// Enable the V2 subscription in `REGION`, asserting it succeeded, and answer the
    /// ARN it reported.
    async fn subscribed(body: Value) -> (Self, String) {
        let harness = Self::new();
        let (status, response) = harness.enable_v2(body).await;
        assert_eq!(
            status, 200,
            "EnableSecurityHubV2 must succeed, got {status} with {response}"
        );
        let arn = response["HubV2Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("EnableSecurityHubV2 must report a HubV2Arn, got {response}"))
            .to_string();
        (harness, arn)
    }
}

/// Escape an ARN for the single, non-greedy `{ResourceArn}` path segment: only `/`
/// (and a literal `%`) would otherwise end the segment, and the server
/// percent-decodes the label back to the ARN.
fn path_escape(arn: &str) -> String {
    arn.replace('%', "%25").replace('/', "%2F")
}

/// The ARN the V2 resource of `region` answers to, in the form the published Security
/// Hub V2 resource ARN carries: resource type `hubv2` (CloudWatch Logs'
/// `PutDeliverySource` documents "For Security Hub, use
/// `arn:aws:securityhub:us-east-1:111122223333:hubv2/*`") and the single name the
/// account/Region holds one of.
fn expected_hub_v2_arn(region: &str) -> String {
    format!("arn:aws:securityhub:{region}:{ACCOUNT_ID}:hubv2/default")
}

/// A `TagMap` JSON object from a list of pairs, so a case's expected tags and the
/// request that sets them come from one value.
fn tag_object(tags: &[(&str, &str)]) -> Value {
    let map: Map<String, Value> = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
        .collect();
    Value::Object(map)
}

/// The error code an error body carries, in the envelope this service renders.
fn error_code(body: &Value) -> String {
    body["Code"]
        .as_str()
        .or_else(|| body["Type"].as_str())
        .or_else(|| body["__type"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The error message an error body carries.
fn error_message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The `Features` map of a `DescribeSecurityHubV2` response, as `(name, status)`
/// pairs sorted by name.
fn feature_statuses(body: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = body["Features"]
        .as_object()
        .map(|features| {
            features
                .iter()
                .map(|(name, detail)| {
                    (
                        name.clone(),
                        detail["FeatureStatus"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    pairs.sort();
    pairs
}

/// The `UpdatedAt` a `DescribeSecurityHubV2` response reports for one feature.
fn feature_updated_at(body: &Value, feature: &str) -> Value {
    body["Features"][feature]["UpdatedAt"].clone()
}

// ---------------------------------------------------------------------------
// EnableSecurityHubV2 / DescribeSecurityHubV2
// ---------------------------------------------------------------------------

/// One row of the enable table: the case name, the request body, and the tag set the
/// created resource must carry.
type EnableCase = (&'static str, Value, Vec<(&'static str, &'static str)>);

/// RULE: `DescribeSecurityHubV2` reports the hub V2 resource `EnableSecurityHubV2`
/// persisted for the caller's account and Region — the same `HubV2Arn` the enable
/// answered, the instant it was created, and every opt-in feature the model publishes
/// with its current status — identically on every call.
///
/// The table spans the request forms `EnableSecurityHubV2` accepts: no members at all,
/// one tag, and a tag set with several entries (`Tags` is the only input member,
/// `smithy_query(query="operation", operation="EnableSecurityHubV2")`).
#[tokio::test]
async fn describe_reports_the_enabled_subscription() {
    let cases: Vec<EnableCase> = vec![
        ("no members", json!({}), Vec::new()),
        (
            "one tag",
            json!({ "Tags": { "team": "security" } }),
            vec![("team", "security")],
        ),
        (
            "several tags",
            json!({ "Tags": { "team": "security", "env": "prod", "owner": "platform" } }),
            vec![("env", "prod"), ("owner", "platform"), ("team", "security")],
        ),
    ];

    for (case, body, expected_tags) in cases {
        let (harness, arn) = Harness::subscribed(body).await;
        assert_eq!(
            arn,
            expected_hub_v2_arn(REGION),
            "case {case:?}: EnableSecurityHubV2 must answer the hub V2 ARN, got {arn:?}"
        );

        let (status, first) = harness.describe_v2().await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSecurityHubV2 expected 200, got {status} with {first}"
        );
        assert_eq!(
            first["HubV2Arn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: DescribeSecurityHubV2 must report the created ARN, got {first}"
        );
        let subscribed_at = first["SubscribedAt"].as_str().unwrap_or_default().to_string();
        assert!(
            aws_smithy_types::DateTime::from_str(
                &subscribed_at,
                aws_smithy_types::date_time::Format::DateTime,
            )
            .is_ok(),
            "case {case:?}: SubscribedAt must be an ISO-8601 instant, got {subscribed_at:?}"
        );
        let expected_features: Vec<(String, String)> = FEATURE_NAMES
            .iter()
            .map(|name| ((*name).to_string(), STATUS_DISABLED.to_string()))
            .collect();
        assert_eq!(
            feature_statuses(&first),
            expected_features,
            "case {case:?}: every opt-in feature must be reported DISABLED, got {first}"
        );

        // A read reports the same resource every time: nothing about it is minted per
        // call.
        let (status, second) = harness.describe_v2().await;
        assert_eq!(
            status, 200,
            "case {case:?}: second DescribeSecurityHubV2 expected 200, got {status} with {second}"
        );
        assert_eq!(
            second, first,
            "case {case:?}: two reads of one subscription must agree, got {second} then {first}"
        );

        // The tags the enable carried are the resource's tags, readable through the
        // tagging API that addresses it by ARN.
        let (status, tags) = harness.list_tags(&arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTagsForResource expected 200, got {status} with {tags}"
        );
        assert_eq!(
            tags["Tags"],
            tag_object(&expected_tags),
            "case {case:?}: ListTagsForResource must report the tags the enable carried, got {tags}"
        );
    }
}

/// RULE: an account/Region with no hub V2 subscription has no service resource to
/// describe, so `DescribeSecurityHubV2` is refused with HTTP 404
/// `ResourceNotFoundException`; the subscription is per account AND Region, so
/// enabling one Region leaves every other Region without one.
#[tokio::test]
async fn describe_without_a_subscription_is_not_found() {
    let harness = Harness::new();
    for region in [REGION, OTHER_REGION] {
        let (status, body) = harness.describe_v2_in(region).await;
        assert_eq!(
            status, 404,
            "case {region:?}: DescribeSecurityHubV2 before enable expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {region:?}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Account {ACCOUNT_ID} is not subscribed to AWS Security Hub"),
            "case {region:?}: unexpected refusal message in {body}"
        );
    }

    let (status, _) = harness.enable_v2_in(REGION, json!({})).await;
    assert_eq!(status, 200, "EnableSecurityHubV2 in {REGION} must succeed");

    let (status, body) = harness.describe_v2_in(REGION).await;
    assert_eq!(
        status, 200,
        "case \"enabled Region\": expected 200, got {status} with {body}"
    );
    let (status, body) = harness.describe_v2_in(OTHER_REGION).await;
    assert_eq!(
        status, 404,
        "case \"other Region\": expected 404, got {status} with {body}"
    );

    // Enabling the other Region gives it its own resource, with its own ARN.
    let (status, response) = harness.enable_v2_in(OTHER_REGION, json!({})).await;
    assert_eq!(
        status, 200,
        "EnableSecurityHubV2 in {OTHER_REGION} expected 200, got {status} with {response}"
    );
    assert_eq!(
        response["HubV2Arn"].as_str(),
        Some(expected_hub_v2_arn(OTHER_REGION).as_str()),
        "case \"other Region\": the ARN must name that Region, got {response}"
    );
}

/// RULE: an account/Region holds exactly one hub V2 resource, and the model declares
/// no conflict error for `EnableSecurityHubV2`, so a repeated enable answers the
/// existing resource's ARN and changes nothing about it — not its `SubscribedAt`, not
/// its tags and not its feature statuses.
///
/// The table spans what a repeated enable can carry: the same tags, different tags,
/// and no tags at all.
#[tokio::test]
async fn repeated_enable_leaves_the_subscription_alone() {
    let cases: Vec<(&str, Value)> = vec![
        ("same tags", json!({ "Tags": { "team": "security" } })),
        ("different tags", json!({ "Tags": { "team": "platform", "env": "dev" } })),
        ("no tags", json!({})),
    ];

    for (case, second_body) in cases {
        let (harness, arn) =
            Harness::subscribed(json!({ "Tags": { "team": "security" } })).await;
        let (_, before) = harness.describe_v2().await;

        let (status, response) = harness.enable_v2(second_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a repeated EnableSecurityHubV2 expected 200, got {status} with {response}"
        );
        assert_eq!(
            response["HubV2Arn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: a repeated enable must answer the existing ARN, got {response}"
        );

        let (status, after) = harness.describe_v2().await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSecurityHubV2 expected 200, got {status} with {after}"
        );
        assert_eq!(
            after, before,
            "case {case:?}: a repeated enable must change nothing, got {after} after {before}"
        );

        let (status, tags) = harness.list_tags(&arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTagsForResource expected 200, got {status} with {tags}"
        );
        assert_eq!(
            tags["Tags"],
            tag_object(&[("team", "security")]),
            "case {case:?}: the first enable's tags must survive, got {tags}"
        );
    }
}

// ---------------------------------------------------------------------------
// EnableSecurityHubFeatureV2 / DisableSecurityHubFeatureV2
// ---------------------------------------------------------------------------

/// RULE: an opt-in feature's status is what the last opt-in operation set it to, and
/// `DescribeSecurityHubV2` reports that status with the instant it was set; both
/// operations are idempotent, so asking for the status a feature already has changes
/// nothing — including its `UpdatedAt`.
///
/// Every feature the model publishes is driven through the whole sequence, so the rule
/// is asserted over the feature catalog rather than over one entry of it.
#[tokio::test]
async fn feature_status_follows_the_opt_in_operations() {
    for feature in FEATURE_NAMES {
        let (harness, _) = Harness::subscribed(json!({})).await;

        let (_, created) = harness.describe_v2().await;
        let created_at = feature_updated_at(&created, feature);
        assert!(
            !created_at.is_null(),
            "case {feature:?}: a subscribed feature must report an UpdatedAt, got {created}"
        );

        let (status, body) = harness.enable_feature(feature).await;
        assert_eq!(
            status, 200,
            "case {feature:?}: EnableSecurityHubFeatureV2 expected 200, got {status} with {body}"
        );
        let (_, enabled) = harness.describe_v2().await;
        assert_eq!(
            enabled["Features"][feature]["FeatureStatus"].as_str(),
            Some(STATUS_ENABLED),
            "case {feature:?}: the feature must read back ENABLED, got {enabled}"
        );
        let enabled_at = feature_updated_at(&enabled, feature);
        assert!(
            !enabled_at.is_null(),
            "case {feature:?}: an enabled feature must report an UpdatedAt, got {enabled}"
        );

        // Idempotent: a second enable is a no-op, so even the timestamp stands still.
        let (status, body) = harness.enable_feature(feature).await;
        assert_eq!(
            status, 200,
            "case {feature:?}: a repeated enable expected 200, got {status} with {body}"
        );
        let (_, again) = harness.describe_v2().await;
        assert_eq!(
            again, enabled,
            "case {feature:?}: a repeated enable must change nothing, got {again} after {enabled}"
        );

        let (status, body) = harness.disable_feature(feature).await;
        assert_eq!(
            status, 200,
            "case {feature:?}: DisableSecurityHubFeatureV2 expected 200, got {status} with {body}"
        );
        let (_, disabled) = harness.describe_v2().await;
        assert_eq!(
            disabled["Features"][feature]["FeatureStatus"].as_str(),
            Some(STATUS_DISABLED),
            "case {feature:?}: the feature must read back DISABLED, got {disabled}"
        );

        // And the disable is idempotent in exactly the same way.
        let (status, body) = harness.disable_feature(feature).await;
        assert_eq!(
            status, 200,
            "case {feature:?}: a repeated disable expected 200, got {status} with {body}"
        );
        let (_, still) = harness.describe_v2().await;
        assert_eq!(
            still, disabled,
            "case {feature:?}: a repeated disable must change nothing, got {still} after {disabled}"
        );

        // The subscription's own identity is untouched by any feature change.
        assert_eq!(
            still["HubV2Arn"], created["HubV2Arn"],
            "case {feature:?}: the ARN must not move, got {still} after {created}"
        );
        assert_eq!(
            still["SubscribedAt"], created["SubscribedAt"],
            "case {feature:?}: SubscribedAt must not move, got {still} after {created}"
        );
    }
}

/// RULE: a feature is a property of the subscription — "The service must be enabled
/// before you can enable a feature" (`smithy_query(query="doc",
/// shape="EnableSecurityHubFeatureV2")`) — so an account/Region with no subscription
/// is refused with HTTP 404 `ResourceNotFoundException`, and a Region's features are
/// its own.
#[tokio::test]
async fn feature_operations_need_a_subscription() {
    for feature in FEATURE_NAMES {
        let harness = Harness::new();
        for (case, status_and_body) in [
            ("enable", harness.enable_feature(feature).await),
            ("disable", harness.disable_feature(feature).await),
        ] {
            let (status, body) = status_and_body;
            assert_eq!(
                status, 404,
                "case {case:?}/{feature:?}: expected 404 before enable, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ResourceNotFoundException",
                "case {case:?}/{feature:?}: expected ResourceNotFoundException, got {body}"
            );
        }

        // Subscribed in one Region only: the feature call of the other Region still
        // has no resource to address.
        let (status, _) = harness.enable_v2_in(REGION, json!({})).await;
        assert_eq!(status, 200, "EnableSecurityHubV2 in {REGION} must succeed");
        let (status, body) = harness.enable_feature_in(REGION, feature).await;
        assert_eq!(
            status, 200,
            "case {feature:?}: enable in the subscribed Region expected 200, got {status} with {body}"
        );
        let (status, body) = harness.enable_feature_in(OTHER_REGION, feature).await;
        assert_eq!(
            status, 404,
            "case {feature:?}: enable in {OTHER_REGION} expected 404, got {status} with {body}"
        );
        let (_, described) = harness.describe_v2_in(REGION).await;
        assert_eq!(
            described["Features"][feature]["FeatureStatus"].as_str(),
            Some(STATUS_ENABLED),
            "case {feature:?}: the subscribed Region's feature must be ENABLED, got {described}"
        );
    }
}

/// RULE: `FeatureName` is an enum, so a value outside it names no feature this service
/// has and the call is refused as invalid input (HTTP 400) rather than served.
///
/// The rows span the ways a path value can be wrong: an unknown name, the right name
/// in the wrong case, and a value from another of this service's enums.
#[tokio::test]
async fn unknown_feature_names_are_refused() {
    let (harness, _) = Harness::subscribed(json!({})).await;
    for case in ["NOT_A_FEATURE", "network_scanning", "SECURITY_HUB_V2"] {
        for (operation, (status, body)) in [
            ("enable", harness.enable_feature(case).await),
            ("disable", harness.disable_feature(case).await),
        ] {
            assert_eq!(
                status, 400,
                "case {case:?}/{operation}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}/{operation}: expected ValidationException, got {body}"
            );
        }
    }

    // Nothing the refusals touched: every feature is still as the subscription
    // created it.
    let (_, described) = harness.describe_v2().await;
    let expected: Vec<(String, String)> = FEATURE_NAMES
        .iter()
        .map(|name| ((*name).to_string(), STATUS_DISABLED.to_string()))
        .collect();
    assert_eq!(
        feature_statuses(&described),
        expected,
        "a refused feature call must change no status, got {described}"
    );
}

/// RULE: the Region a subscription is filed under comes from the caller's own
/// credential scope, so the store holds subscriptions in at most
/// [`MAX_HUB_V2_REGIONS`] Regions: a subscription that would be the first in a Region
/// past that is refused (HTTP 400 `ValidationException`) and nothing is created, while
/// a Region already subscribed needs no room for a further one.
#[tokio::test]
async fn the_subscribed_regions_are_bounded() {
    let harness = Harness::new();
    let region_of = |index: usize| format!("eu-test-{index}");

    for index in 0..MAX_HUB_V2_REGIONS {
        let region = region_of(index);
        let (status, body) = harness.enable_v2_in(&region, json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?} (Region {} of {MAX_HUB_V2_REGIONS}): expected 200, got {status} \
             with {body}",
            index + 1
        );
        assert_eq!(
            body["HubV2Arn"].as_str(),
            Some(expected_hub_v2_arn(&region).as_str()),
            "case {region:?}: the ARN must name that Region, got {body}"
        );
    }

    let beyond = region_of(MAX_HUB_V2_REGIONS);
    let (status, body) = harness.enable_v2_in(&beyond, json!({})).await;
    assert_eq!(
        status, 400,
        "case {beyond:?} (one Region past the cap): expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {beyond:?}: expected ValidationException, got {body}"
    );
    let message = error_message(&body);
    assert!(
        message.contains(&MAX_HUB_V2_REGIONS.to_string()) && message.contains(&beyond),
        "case {beyond:?}: the refusal must name the bound and the Region, got {message:?}"
    );
    let (status, body) = harness.describe_v2_in(&beyond).await;
    assert_eq!(
        status, 404,
        "case {beyond:?}: the refused enable must have created nothing, got {status} with {body}"
    );

    // A Region the store already holds a subscription in is never refused by the cap.
    let held = region_of(0);
    let (status, body) = harness.enable_v2_in(&held, json!({})).await;
    assert_eq!(
        status, 200,
        "case {held:?} (already subscribed): expected 200, got {status} with {body}"
    );
}
