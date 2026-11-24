// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the EFFECTIVE `StripBasePath` setting of a routing rule's
//! `invokeApi` action.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch`
//! over a signed `http::Request` — so what is asserted is the wire behaviour a client
//! sees, not a handler called directly.
//!
//! The rule asserted here: an `invokeApi` action a writer stored is served under ONE
//! setting for `RoutingRuleActionInvokeApi$StripBasePath` ("The strip base path setting",
//! an optional boolean on the input —
//! `smithy_query(query="shape", shape="RoutingRuleActionInvokeApi")`), so every reader of
//! the rule reports that boolean rather than nothing: the value the request named where
//! it named one, and the service's own default where it did not. The members that follow
//! from it:
//!
//! * `CreateRoutingRule`, `PutRoutingRule`, `GetRoutingRule` and `ListRoutingRules` all
//!   report the same setting for the same action, action position by action position.
//! * A read-modify-write that sends an action back as it was read keeps the setting it
//!   read: the value survives the round trip rather than being reset by a rewrite that
//!   carries it.
//!
//! The rows vary what the rule generalises over: whether the request names the setting at
//! all, which of the two booleans it names, how many actions the rule carries, and
//! whether the actions of one rule name the setting differently from each other.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewayv2_apigatewayv2::state::DEFAULT_STRIP_BASE_PATH;

use crate::api_lifecycle::{as_str, call_in_region, emulator};

/// The region every request in this suite is signed for. Deliberately NOT the emulator's
/// `us-east-1` default, so nothing region-derived can be hardcoded.
const REGION: &str = "ap-south-1";

/// The custom domain name every rule is created under.
const DOMAIN: &str = "stripped.example.com";

/// The stages the seeded api carries, so an action's target can vary without inventing a
/// stage.
const STAGE: &str = "$default";
const OTHER_STAGE: &str = "canary";

/// Send one request signed for [`REGION`] and answer `(status, body)`.
async fn call(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in_region(emulator, REGION, method, uri, body).await
}

async fn create_routing_rule(emulator: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(
        emulator,
        "POST",
        &format!("/v2/domainnames/{DOMAIN}/routingrules"),
        Some(request.clone()),
    )
    .await
}

async fn put_routing_rule(
    emulator: &MultiServiceRouter,
    routing_rule_id: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "PUT",
        &format!("/v2/domainnames/{DOMAIN}/routingrules/{routing_rule_id}"),
        Some(request.clone()),
    )
    .await
}

async fn get_routing_rule(emulator: &MultiServiceRouter, routing_rule_id: &str) -> (u16, Value) {
    call(
        emulator,
        "GET",
        &format!("/v2/domainnames/{DOMAIN}/routingrules/{routing_rule_id}"),
        None,
    )
    .await
}

async fn list_routing_rules(emulator: &MultiServiceRouter) -> (u16, Value) {
    call(
        emulator,
        "GET",
        &format!("/v2/domainnames/{DOMAIN}/routingrules"),
        None,
    )
    .await
}

/// A fresh emulator holding [`DOMAIN`] and one api carrying both stages. Answers
/// `(emulator, api_id)`.
async fn seeded() -> (MultiServiceRouter, String) {
    let emulator = emulator();
    let (status, body) = call(
        &emulator,
        "POST",
        "/v2/domainnames",
        Some(json!({
            "domainName": DOMAIN,
            // AWS Certificate Manager is the only source `CertificateArn` documents.
            "domainNameConfigurations": [{
                "certificateArn": engine::emulator_arn(
                    "acm",
                    REGION,
                    &format!("certificate/{DOMAIN}"),
                ),
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "seeding domain name {DOMAIN:?}: expected 201 from CreateDomainName, got {status} {body}"
    );

    let (status, body) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "stripped-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "seeding the api: expected 201 from CreateApi, got {status} {body}"
    );
    let api_id = as_str(&body, "apiId");
    for stage_name in [STAGE, OTHER_STAGE] {
        let (status, stage) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": stage_name })),
        )
        .await;
        assert_eq!(
            status, 201,
            "seeding stage {stage_name:?}: expected 201 from CreateStage, got {status} {stage}"
        );
    }
    (emulator, api_id)
}

/// One row of the table: the `StripBasePath` each action of the rule is written with
/// (`None` for an action that names none), and the setting each one is then served under.
struct StripCase {
    label: &'static str,
    /// What each action of the request names for the setting, action by action.
    requested: Vec<Option<bool>>,
}

impl StripCase {
    /// The actions of this row's request: one per entry of `requested`, alternating the
    /// stage invoked so a multi-action rule names more than one target.
    fn actions(&self, api_id: &str) -> Value {
        Value::Array(
            self.requested
                .iter()
                .enumerate()
                .map(|(index, requested)| {
                    let stage = if index % 2 == 0 { STAGE } else { OTHER_STAGE };
                    let mut invoke_api = json!({ "apiId": api_id, "stage": stage });
                    if let Some(strip) = requested {
                        invoke_api["stripBasePath"] = json!(strip);
                    }
                    json!({ "invokeApi": invoke_api })
                })
                .collect(),
        )
    }

    /// The whole body of this row's write.
    fn request(&self, api_id: &str, priority: i64) -> Value {
        json!({
            "priority": priority,
            "actions": self.actions(api_id),
            "conditions": [{ "matchBasePaths": { "anyOf": ["orders"] } }],
        })
    }

    /// The setting each action of this row is SERVED under: the caller's where the
    /// request named one, and the service's own default where it did not.
    fn effective(&self) -> Vec<bool> {
        self.requested
            .iter()
            .map(|requested| requested.unwrap_or(DEFAULT_STRIP_BASE_PATH))
            .collect()
    }
}

/// The rows span whether the setting is named at all, which boolean is named, how many
/// actions the rule carries, and whether one rule's actions disagree about it.
fn strip_cases() -> Vec<StripCase> {
    vec![
        StripCase { label: "one action naming no setting", requested: vec![None] },
        StripCase { label: "one action naming it false", requested: vec![Some(false)] },
        StripCase { label: "one action naming it true", requested: vec![Some(true)] },
        StripCase {
            label: "two actions, neither naming the setting",
            requested: vec![None, None],
        },
        StripCase {
            label: "two actions, one naming it true and one naming none",
            requested: vec![Some(true), None],
        },
        StripCase {
            label: "three actions, naming none, false and true",
            requested: vec![None, Some(false), Some(true)],
        },
    ]
}

/// The actions a request's `Actions` list is SERVED as: each action with the
/// `stripBasePath` the request named for it, and [`DEFAULT_STRIP_BASE_PATH`] where it
/// named none.
///
/// The suites that assert a reader reports the rule a request described compare against
/// this rather than against the request's own list, which leaves the setting out: the
/// setting a stored action is served under is part of the rule whether or not the writer
/// spelled it.
pub(crate) fn served_actions(requested: &Value) -> Value {
    Value::Array(
        requested
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|action| {
                let mut served = action.clone();
                let strip = served["invokeApi"]["stripBasePath"]
                    .as_bool()
                    .unwrap_or(DEFAULT_STRIP_BASE_PATH);
                served["invokeApi"]["stripBasePath"] = json!(strip);
                served
            })
            .collect(),
    )
}

/// The `stripBasePath` each action of `body` reports, action by action, with `None` for an
/// action that reports no boolean at all.
fn reported(body: &Value) -> Vec<Option<bool>> {
    body["actions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|action| action["invokeApi"]["stripBasePath"].as_bool())
        .collect()
}

/// Assert `body`, as reported by `what`, serves `case` under exactly the settings it is
/// served under.
fn assert_serves_effective_settings(case: &str, what: &str, body: &Value, expected: &[bool]) {
    let actual = reported(body);
    let wanted: Vec<Option<bool>> = expected.iter().copied().map(Some).collect();
    assert_eq!(
        actual, wanted,
        "case {case:?}: expected {what} to report stripBasePath {wanted:?} \
         action by action, got {actual:?} in {body}"
    );
}

#[tokio::test]
async fn every_reader_of_a_rule_reports_the_effective_strip_base_path_of_each_action() {
    let (emulator, api_id) = seeded().await;
    // The rule each row creates, so the collection reader can be asserted once over all
    // of them: `(label, routing_rule_id, effective settings)`.
    let mut created_rules: Vec<(&'static str, String, Vec<bool>)> = Vec::new();

    for (index, case) in strip_cases().into_iter().enumerate() {
        let label = case.label;
        let expected = case.effective();
        let request = case.request(&api_id, index as i64 + 1);

        let (status, created) = create_routing_rule(&emulator, &request).await;
        assert_eq!(
            status, 201,
            "case {label:?}: expected 201 from CreateRoutingRule, got {status} {created}"
        );
        assert_serves_effective_settings(label, "CreateRoutingRule", &created, &expected);

        let routing_rule_id = as_str(&created, "routingRuleId");
        let (status, read) = get_routing_rule(&emulator, &routing_rule_id).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetRoutingRule, got {status} {read}"
        );
        assert_serves_effective_settings(label, "GetRoutingRule", &read, &expected);

        created_rules.push((label, routing_rule_id, expected));
    }

    // The collection reader reports the same setting for every rule it pages over.
    let (status, listed) = list_routing_rules(&emulator).await;
    assert_eq!(
        status, 200,
        "expected 200 from ListRoutingRules, got {status} {listed}"
    );
    let rules = listed["routingRules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (label, routing_rule_id, expected) in &created_rules {
        let found = rules
            .iter()
            .find(|rule| as_str(rule, "routingRuleId") == *routing_rule_id)
            .unwrap_or_else(|| {
                panic!("case {label:?}: expected ListRoutingRules to report {routing_rule_id:?}, got {listed}")
            });
        assert_serves_effective_settings(label, "ListRoutingRules", found, expected);
    }
}

#[tokio::test]
async fn put_routing_rule_serves_the_effective_strip_base_path_and_keeps_it_across_a_round_trip() {
    let (emulator, api_id) = seeded().await;
    // The rule every row rewrites: one action naming no setting, so each rewrite starts
    // from a rule already served under the default.
    let seed = json!({
        "priority": 1,
        "actions": [{ "invokeApi": { "apiId": api_id, "stage": STAGE } }],
        "conditions": [{ "matchBasePaths": { "anyOf": ["seed"] } }],
    });
    let (status, created) = create_routing_rule(&emulator, &seed).await;
    assert_eq!(
        status, 201,
        "seeding the rule: expected 201 from CreateRoutingRule, got {status} {created}"
    );
    let routing_rule_id = as_str(&created, "routingRuleId");

    for (index, case) in strip_cases().into_iter().enumerate() {
        let label = case.label;
        let expected = case.effective();
        let request = case.request(&api_id, index as i64 + 1);

        let (status, written) = put_routing_rule(&emulator, &routing_rule_id, &request).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from PutRoutingRule, got {status} {written}"
        );
        assert_serves_effective_settings(label, "PutRoutingRule", &written, &expected);

        let (status, read) = get_routing_rule(&emulator, &routing_rule_id).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetRoutingRule, got {status} {read}"
        );
        assert_serves_effective_settings(label, "GetRoutingRule", &read, &expected);

        // A read-modify-write that sends the actions back as they were READ keeps the
        // setting: the rewrite carries the effective value, so the rule is served under it
        // afterwards too.
        let round_trip = json!({
            "priority": request["priority"].clone(),
            "actions": read["actions"].clone(),
            "conditions": read["conditions"].clone(),
        });
        let (status, rewritten) = put_routing_rule(&emulator, &routing_rule_id, &round_trip).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from the round-tripping PutRoutingRule, \
             got {status} {rewritten}"
        );
        assert_serves_effective_settings(
            label,
            "the round-tripping PutRoutingRule",
            &rewritten,
            &expected,
        );
        let (status, again) = get_routing_rule(&emulator, &routing_rule_id).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetRoutingRule after the round trip, \
             got {status} {again}"
        );
        assert_serves_effective_settings(
            label,
            "GetRoutingRule after the round trip",
            &again,
            &expected,
        );
    }
}
