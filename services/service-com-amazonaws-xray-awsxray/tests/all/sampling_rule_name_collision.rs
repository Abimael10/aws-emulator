// SPDX-License-Identifier: CC0-1.0

//! What `CreateSamplingRule` answers when the `RuleName` it asks for is already
//! held, end to end over the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! The rule: a create whose name the account already holds is refused for the
//! RESOURCE that holds it — one sentence of fixed text, whose only variable part
//! is the internal id that resource is stored under, read from the service's own
//! state and not from the request. So the refusal is the same sentence whichever
//! held rule is in the way, it names ONE id however many callers race for the
//! name, and a name created again after its rule was deleted names a different
//! resource and so a different id. That is what lets a caller tell this
//! `InvalidRequestException` from the operation's others, which carry the same
//! status and the same code.
//!
//! The rows differ along what holds the name (a rule the account created, the
//! service-managed one that exists without being created, a rule re-created
//! under a freed name), along the region the reserved name is asked for in, and
//! along how many callers ask for one name. The calls IN FLIGHT together are
//! graded by `graphs_rules_retrievals_and_insights`, which fires five joined
//! creates for one name and reads each loser's refusal.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::state::{
    DEFAULT_SAMPLING_RULE_NAME, SAMPLING_RULE_EXISTS_PREFIX,
};

use super::xray_wire::{Answer, Xray, DEFAULT_REGION};

/// The refusal message of an answer, or the empty string.
fn said(answer: &Answer) -> &str {
    answer.message().unwrap_or_default()
}

/// A sampling rule body `CreateSamplingRule` accepts, at `priority`.
fn rule_body(name: &str, priority: i64) -> Value {
    json!({
        "SamplingRule": {
            "RuleName": name,
            "ResourceARN": "*",
            "Priority": priority,
            "FixedRate": 0.05,
            "ReservoirSize": 1,
            "ServiceName": "*",
            "ServiceType": "*",
            "Host": "*",
            "HTTPMethod": "*",
            "URLPath": "*",
            // The model documents the sampling rule format as `1`.
            "Version": 1,
        }
    })
}

/// The internal id a refusal names, or `None` when the message is not the
/// collision refusal at all.
fn held_resource_id(answer: &Answer) -> Option<&str> {
    said(answer).strip_prefix(SAMPLING_RULE_EXISTS_PREFIX)
}

/// Assert that `answer` is the collision refusal, and answer the internal id it
/// named.
fn refused_for_the_held_resource(case: &str, answer: &Answer) -> String {
    assert_eq!(
        (answer.status, answer.error_type.as_deref()),
        (400, Some("InvalidRequestException")),
        "case {case:?}: a taken RuleName must be refused as an invalid request, got {answer:?}"
    );
    let internal_id = held_resource_id(answer).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a message beginning {SAMPLING_RULE_EXISTS_PREFIX:?}, got {:?}",
            said(answer)
        )
    });
    assert!(
        !internal_id.is_empty(),
        "case {case:?}: the refusal must name the held resource's internal id, got {:?}",
        said(answer)
    );
    internal_id.to_string()
}

// =============================================================================
// 1. Whatever holds the name, the refusal names that resource
// =============================================================================

#[tokio::test]
async fn a_taken_rule_name_is_refused_naming_the_resource_that_holds_it() {
    // One account, three held names: two rules it created and the
    // service-managed rule every account holds without creating it. The reserved
    // name is asked for in several regions, which the sentence does not carry.
    let xray = Xray::new();
    let mut created_ids: Vec<(String, String)> = Vec::new();
    for name in ["gravel-ingest", "basalt-probe"] {
        let created = xray
            .call("/CreateSamplingRule", &rule_body(name, 500))
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: the first create must succeed, got {created:?}"
        );
        let again = xray
            .call("/CreateSamplingRule", &rule_body(name, 600))
            .await;
        let case = format!("a created rule named {name:?}");
        created_ids.push((name.to_string(), refused_for_the_held_resource(&case, &again)));
    }
    // Each created rule is its own resource, so each refusal named its own id.
    assert_ne!(
        created_ids[0].1, created_ids[1].1,
        "case \"two created rules\": each held rule is its own resource, got {created_ids:?}"
    );

    // The service-managed rule holds its name without anything creating it, so a
    // create for that name is refused for THAT resource: the same id whichever
    // region the call is signed for — it is one resource of the account, not one
    // minted per caller — and not the id of any rule the account created.
    let mut reserved_ids: Vec<(&str, String)> = Vec::new();
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
        let reserved = xray
            .call_in_region(
                region,
                "/CreateSamplingRule",
                &rule_body(DEFAULT_SAMPLING_RULE_NAME, 700),
            )
            .await;
        let case = format!("the service-managed rule in {region:?}");
        reserved_ids.push((region, refused_for_the_held_resource(&case, &reserved)));
    }
    let first = reserved_ids[0].1.clone();
    assert!(
        reserved_ids.iter().all(|(_, id)| *id == first),
        "case \"the service-managed rule\": one resource, so one id in every region, got \
         {reserved_ids:?}"
    );
    assert!(
        created_ids.iter().all(|(_, id)| *id != first),
        "case \"the service-managed rule\": its id must not be a created rule's, got {first:?} \
         beside {created_ids:?}"
    );
}

#[tokio::test]
async fn re_reading_the_refusal_names_the_same_resource_until_it_is_deleted() {
    let xray = Xray::new();
    let name = "quartz-sampler";
    let created = xray
        .call("/CreateSamplingRule", &rule_body(name, 400))
        .await;
    assert_eq!(
        created.status, 200,
        "the rule under test must be created, got {created:?}"
    );

    // The same held resource, asked for twice: one resource, one id.
    let first = xray.call("/CreateSamplingRule", &rule_body(name, 401)).await;
    let second = xray.call("/CreateSamplingRule", &rule_body(name, 402)).await;
    let first_id = refused_for_the_held_resource("the first loser", &first);
    let second_id = refused_for_the_held_resource("the second loser", &second);
    assert_eq!(
        first_id, second_id,
        "case \"one held resource\": both refusals name the one rule in the way, got \
         {first_id:?} and {second_id:?}"
    );

    // Deleted and created again, the name holds a DIFFERENT resource, so the
    // refusal for it names a different id — which is what distinguishes "the
    // name is taken by another resource" from "the name is taken by this one".
    let deleted = xray
        .call("/DeleteSamplingRule", &json!({ "RuleName": name }))
        .await;
    assert_eq!(
        deleted.status, 200,
        "the rule must be deletable, got {deleted:?}"
    );
    let recreated = xray
        .call("/CreateSamplingRule", &rule_body(name, 403))
        .await;
    assert_eq!(
        recreated.status, 200,
        "the freed name must be creatable again, got {recreated:?}"
    );
    let third = xray.call("/CreateSamplingRule", &rule_body(name, 404)).await;
    let third_id = refused_for_the_held_resource("a re-created rule of the same name", &third);
    assert_ne!(
        third_id, first_id,
        "case \"a re-created rule of the same name\": the name holds a different resource now, \
         so the refusal must name a different internal id, got {third_id:?}"
    );
}

// =============================================================================
// 2. N callers racing for one name: one winner, and one id in every refusal
// =============================================================================

/// How many 200s and which internal ids a set of answers to one name carries.
fn one_winner(case: &str, answers: &[Answer]) -> String {
    let winners = answers
        .iter()
        .filter(|answer| answer.status == 200)
        .count();
    assert_eq!(
        winners, 1,
        "case {case:?}: exactly one create may win, got {answers:?}"
    );
    let named: Vec<String> = answers
        .iter()
        .filter(|answer| answer.status != 200)
        .map(|loser| refused_for_the_held_resource(case, loser))
        .collect();
    let one_id = named.first().cloned().unwrap_or_default();
    assert!(
        named.iter().all(|id| *id == one_id),
        "case {case:?}: every loser lost to the one winner, so every refusal names one id, got \
         {named:?}"
    );
    one_id
}

/// How many times `name` is held in the account.
async fn times_held(xray: &Xray, name: &str) -> usize {
    let listed = xray
        .call_in_region(DEFAULT_REGION, "/GetSamplingRules", &json!({}))
        .await;
    listed
        .get("SamplingRuleRecords")
        .as_array()
        .map(|records| {
            records
                .iter()
                .filter(|record| {
                    record
                        .get("SamplingRule")
                        .and_then(|rule| rule.get("RuleName"))
                        .and_then(Value::as_str)
                        == Some(name)
                })
                .count()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn many_creates_of_one_name_have_one_winner_and_name_one_resource() {
    // The uniqueness check and the insert are ONE step on the store, so the
    // outcome does not depend on how the calls interleave. The rows differ along
    // how many callers ask for the name.
    for callers in [2_usize, 5, 9] {
        let xray = Xray::new();
        let name = format!("race-{callers}");
        let body = rule_body(&name, 300);
        let mut answers: Vec<Answer> = Vec::new();
        for _ in 0..callers {
            answers.push(xray.call("/CreateSamplingRule", &body).await);
        }
        let case = format!("{callers} creates of one name");
        one_winner(&case, &answers);
        assert_eq!(
            times_held(&xray, &name).await,
            1,
            "case {case:?}: the name must be held exactly once afterwards"
        );
    }
}

// The calls IN FLIGHT together are the concern of
// `graphs_rules_retrievals_and_insights::concurrent_creates_of_one_rule_name_have_exactly_one_winner`,
// which fires five joined creates for one name and now reads each loser's refusal for the one
// internal id the winner is stored under: the one-winner rule stands there, in one place.
