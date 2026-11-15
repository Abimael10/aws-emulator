// SPDX-License-Identifier: CC0-1.0

//! The CROSS-SERVICE harness every suite that drives a Recycle Bin retention rule shares:
//! an emulator with both this service and the service that OWNS retention rules mounted on
//! one engine, the hop that calls that service, and the rule creation and deletion a suite
//! sets its case up with.
//!
//! A retention rule is the `rbin` service's resource rather than EC2's — "resources (of the
//! specified type) that have at least one of these tag key and value pairs are automatically
//! retained in the Recycle Bin upon deletion", and a rule naming no resource tags "retains
//! all deleted resources of the specified resource type in the Region in which the rule is
//! created" (`rbin`'s `CreateRuleRequest$ResourceTags`) — so a suite about what a rule does
//! to an EC2 delete creates the rule through that service's own API, over its own wire
//! (`restJson1`, a JSON body on `/rules`), rather than seeding a store. This module holds
//! only that plumbing; what each delete then asserts lives in that operation's own suite
//! ([`super::volume_retention_rules`], [`super::snapshots_in_recycle_bin`]).

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_rbin_amazonrecyclebin as rbin;
use smithy_server_runtime::MultiServiceRouter;

/// The `ResourceType` values a retention rule may name, as the service that owns rules
/// spells them ([`rbin::state::RESOURCE_TYPE_EBS_VOLUME`] and its siblings).
/// Source: `smithy_query(query="shape",shape="ResourceType")` on the `rbin` model.
pub const RESOURCE_TYPE_VOLUME: &str = rbin::state::RESOURCE_TYPE_EBS_VOLUME;
pub const RESOURCE_TYPE_SNAPSHOT: &str = rbin::state::RESOURCE_TYPE_EBS_SNAPSHOT;

/// The longest retention period, in days, a rule for an EBS VOLUME may carry: "EBS volumes:
/// 1 - 7 days" (`RetentionPeriod$RetentionPeriodValue`), as the owning service's own catalog
/// states it ([`rbin::state::MAX_RETENTION_DAYS_EBS_VOLUME`]) rather than as a figure a suite
/// of this crate repeats.
pub const MAX_RETENTION_DAYS_VOLUME: i64 = rbin::state::MAX_RETENTION_DAYS_EBS_VOLUME as i64;

/// A Region whose endpoint is NOT the one this crate's suites sign their EC2 calls for, for a
/// case about a rule another Region holds — "the Region in which the rule is created"
/// (`CreateRuleRequest$ResourceTags`) is the one a rule retains in, so a rule created here
/// decides no delete signed for the suite's own Region.
pub const OTHER_REGION: &str = "us-east-1";

/// How long a poll keeps looking for a condition before a suite gives up on it.
///
/// Far above every span either service owes, so a loaded or a slow host reaches the
/// condition well inside it: a suite waits for the CONDITION and never for a fixed time, so
/// its outcome does not depend on how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One retention rule a suite's case holds: what `CreateRule` is called with, and the Region
/// the rule therefore exists in.
#[derive(Debug, Clone, Copy)]
pub struct RetentionRule {
    /// `CreateRuleRequest$ResourceType` — the type of resource the rule retains.
    pub resource_type: &'static str,
    /// `RetentionPeriod$RetentionPeriodValue`, in `DAYS`, which is the one unit the model
    /// declares.
    pub retention_days: i64,
    /// `CreateRuleRequest$ResourceTags` — a rule naming none is a Region-level rule.
    pub resource_tags: &'static [(&'static str, &'static str)],
    /// `CreateRuleRequest$ExcludeResourceTags` — Region-level rules only.
    pub exclude_resource_tags: &'static [(&'static str, &'static str)],
    /// The Region the `CreateRule` call is signed for: "it retains all deleted resources of
    /// the specified resource type in the Region in which the rule is created"
    /// (`CreateRuleRequest$ResourceTags`).
    pub region: &'static str,
}

/// Both services on ONE engine, which is what makes the `rbin` rules the rules an EC2 delete
/// is decided by — exactly as the assembled emulator mounts them. The engine is handed back
/// too, so a suite can read the store a handler wrote.
pub fn emulator_with_retention_rules() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(rbin::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One `rbin` call signed for `region`, which is the Region the rule it addresses exists in.
pub async fn rbin_call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("a serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://rbin.{region}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEC2TEST/20260101/{region}/rbin/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=deadbeef"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {path} answered {status} with a non-JSON body ({error}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The resource tags of a rule, as `CreateRule` takes them.
fn resource_tags(tags: &[(&str, &str)]) -> serde_json::Value {
    serde_json::Value::Array(
        tags.iter()
            .map(|(key, value)| {
                serde_json::json!({ "ResourceTagKey": key, "ResourceTagValue": value })
            })
            .collect(),
    )
}

/// Create `rule` through the service that OWNS retention rules, wait until it is in the one
/// state that retains anything, and answer its identifier.
pub async fn create_available_rule(
    router: &MultiServiceRouter,
    case: &str,
    rule: &RetentionRule,
) -> String {
    let mut body = serde_json::json!({
        "ResourceType": rule.resource_type,
        "RetentionPeriod": {
            "RetentionPeriodValue": rule.retention_days,
            "RetentionPeriodUnit": "DAYS",
        },
    });
    if !rule.resource_tags.is_empty() {
        body["ResourceTags"] = resource_tags(rule.resource_tags);
    }
    if !rule.exclude_resource_tags.is_empty() {
        body["ExcludeResourceTags"] = resource_tags(rule.exclude_resource_tags);
    }
    let (status, answer) = rbin_call(router, rule.region, "POST", "/rules", Some(body)).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected the retention rule to be created, got {status} with {answer}"
    );
    let identifier = answer
        .get("Identifier")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no Identifier in {answer}"))
        .to_string();

    // "Only retention rules that are in the `available` state retain resources"
    // (`CreateRuleResponse$Status`), and a freshly created rule starts out `pending`: the
    // suite waits for the state the rule reports rather than for a span.
    let region = rule.region;
    let probe_identifier = identifier.clone();
    poll_until(case, "the retention rule to become available", || {
        let probe_identifier = probe_identifier.clone();
        async move {
            let (status, answer) =
                rbin_call(router, region, "GET", &format!("/rules/{probe_identifier}"), None).await;
            status == 200
                && answer.get("Status").and_then(serde_json::Value::as_str) == Some("available")
        }
    })
    .await;
    identifier
}

/// Delete the retention rule `identifier` through the service that owns it, so a case can
/// ask what a delete does once the rule that covered it is gone.
pub async fn delete_rule(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    identifier: &str,
) {
    let (status, answer) =
        rbin_call(router, region, "DELETE", &format!("/rules/{identifier}"), None).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected the retention rule {identifier} to be deleted, got {status} \
         with {answer}"
    );
}

/// Look again until `probe` answers `true`, or [`POLL_DEADLINE`] has passed — which is this
/// harness's way of waiting for a condition instead of for a span.
pub async fn poll_until<P, F>(case: &str, waiting_for: &str, mut probe: P)
where
    P: FnMut() -> F,
    F: Future<Output = bool>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if probe().await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waited for {waiting_for} and it never happened"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
