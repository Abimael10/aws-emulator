// SPDX-License-Identifier: CC0-1.0

//! What `CreateSamplingRule` answers when the `RuleName` it asks for is already
//! taken, end to end over the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! The rule: X-Ray refuses a duplicate `RuleName` from either of its own two
//! steps — the read of the name and the insert — and each refusal is worded by
//! the step that caught it, both as `InvalidRequestException` at HTTP 400:
//!
//! * a name already HELD and settled there before the request arrived is refused
//!   by the read — whatever rule that create submits, since what the read reads
//!   is the NAME — with the service's fixed sentence for that condition, which
//!   interpolates nothing from the request and nothing from the service's state,
//!   so neither the name asked for nor any identifier of the rule in the way
//!   appears in it, and the same sentence answers whichever held rule is in the
//!   way, in whichever Region, however many callers ask one after another, and
//!   whether or not the name was held by an earlier rule since deleted;
//! * a create that read passes — one that read the name as FREE and then lost
//!   the insert to another create IN FLIGHT beside it — reaches the insert and is
//!   refused by the uniqueness constraint of the store the rule is written
//!   through, whose sentence speaks of resources rather than of rules: it names
//!   the resource holding the name by the internal id it is held under — one id
//!   across every loser of one race, and a different one for a name created again
//!   after a delete, because that is a different resource.
//!
//! Together those are what let a caller tell a name collision from the
//! operation's other `InvalidRequestException`s, which carry the same status and
//! the same code.
//!
//! The rows differ along what holds the name (a rule the account created, the
//! service-managed one that exists without being created, a rule re-created
//! under a freed name), along the Region the reserved name is asked for in,
//! along the priority the losing create carries, along WHICH member of the rule
//! the losing create states differently (and along stating none of them
//! differently, i.e. restating the stored rule exactly), along how many callers
//! ask for one name, and along whether those callers are sequential or in flight
//! together.
//! `graphs_rules_retrievals_and_insights` grades the in-flight case once more,
//! over the five joined creates it fires for one name.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::state::{
    DEFAULT_SAMPLING_RULE_NAME, SAMPLING_RULE_EXISTS_MESSAGE, SAMPLING_RULE_EXISTS_PREFIX,
};

use super::xray_wire::{array, Answer, Xray, DEFAULT_REGION};

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

/// The sentence X-Ray words for a create whose `RuleName` is held by another
/// rule, settled there before the request arrived, written out here as that
/// service states it — so this suite checks AWS's own wording rather than only
/// the constant the crate carries, which
/// [`the_refusals_are_the_services_own_wording`] holds to this same text.
const ALREADY_EXISTS: &str = "Sampling rule already exists";

/// The fixed text of the OTHER refusal, which a create the name read let
/// through is answered with — written out here for the same reason, with the
/// internal id of the resource holding the name as its only variable part.
const RACE_LOST_PREFIX: &str =
    "A resource with the same resourceName but a different internalId already exists: ";

/// Assert that `answer` is the refusal a name held by another rule, settled
/// before the request arrived, is answered with: the modelled 400 and the
/// service's fixed sentence, with nothing interpolated.
fn refused_as_already_existing(case: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_type.as_deref()),
        (400, Some("InvalidRequestException")),
        "case {case:?}: a taken RuleName must be refused as an invalid request, got {answer:?}"
    );
    assert_eq!(
        said(answer),
        ALREADY_EXISTS,
        "case {case:?}: expected the fixed sentence {ALREADY_EXISTS:?}, got {:?}",
        said(answer)
    );
}

/// Assert that `answer` is the refusal a create the NAME READ LET THROUGH is
/// answered with, and answer the internal id it named.
fn refused_for_the_resource_that_won(case: &str, answer: &Answer) -> String {
    assert_eq!(
        (answer.status, answer.error_type.as_deref()),
        (400, Some("InvalidRequestException")),
        "case {case:?}: a create that lost the race must be refused as an invalid request, got \
         {answer:?}"
    );
    let internal_id = said(answer).strip_prefix(RACE_LOST_PREFIX).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a message beginning {RACE_LOST_PREFIX:?}, got {:?}",
            said(answer)
        )
    });
    assert!(
        !internal_id.is_empty(),
        "case {case:?}: the refusal must name the internal id of the rule that won, got {:?}",
        said(answer)
    );
    internal_id.to_string()
}

/// The crate's own constants ARE those two sentences, so every other surface
/// that refuses a duplicate name from them states what the service states.
#[test]
fn the_refusals_are_the_services_own_wording() {
    assert_eq!(
        SAMPLING_RULE_EXISTS_MESSAGE, ALREADY_EXISTS,
        "the refusal of a name already held must be the service's own sentence, got {:?}",
        SAMPLING_RULE_EXISTS_MESSAGE
    );
    assert_eq!(
        SAMPLING_RULE_EXISTS_PREFIX, RACE_LOST_PREFIX,
        "the refusal of a lost race must be the service's own sentence, got {:?}",
        SAMPLING_RULE_EXISTS_PREFIX
    );
}

// =============================================================================
// 1. A name held when the request arrived: the one fixed sentence
// =============================================================================

#[tokio::test]
async fn a_taken_rule_name_is_refused_with_the_services_fixed_sentence() {
    // One account, three held names: two rules it created and the
    // service-managed rule every account holds without creating it. The reserved
    // name is asked for in several Regions, which the sentence does not carry
    // either.
    let xray = Xray::new();
    for (name, priority) in [("gravel-ingest", 600), ("basalt-probe", 1_200)] {
        let created = xray
            .call("/CreateSamplingRule", &rule_body(name, 500))
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: the first create must succeed, got {created:?}"
        );
        let again = xray
            .call("/CreateSamplingRule", &rule_body(name, priority))
            .await;
        refused_as_already_existing(&format!("a created rule named {name:?}"), &again);
    }

    // The service-managed rule holds its name without anything creating it, so a
    // create for that name is refused the same way, in every Region the call is
    // signed for.
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
        let reserved = xray
            .call_in_region(
                region,
                "/CreateSamplingRule",
                &rule_body(DEFAULT_SAMPLING_RULE_NAME, 700),
            )
            .await;
        refused_as_already_existing(
            &format!("the service-managed rule in {region:?}"),
            &reserved,
        );
    }
}

#[tokio::test]
async fn the_refusal_is_the_same_sentence_until_the_name_is_freed() {
    let xray = Xray::new();
    let name = "quartz-sampler";
    let created = xray
        .call("/CreateSamplingRule", &rule_body(name, 400))
        .await;
    assert_eq!(
        created.status, 200,
        "the rule under test must be created, got {created:?}"
    );

    // The same held resource, asked for twice: one sentence, repeated.
    let first = xray.call("/CreateSamplingRule", &rule_body(name, 401)).await;
    let second = xray.call("/CreateSamplingRule", &rule_body(name, 402)).await;
    refused_as_already_existing("the first loser", &first);
    refused_as_already_existing("the second loser", &second);

    // The original rule is untouched by the refusals: the priority it was
    // created with still stands.
    let listed = xray
        .call_in_region(DEFAULT_REGION, "/GetSamplingRules", &json!({}))
        .await;
    let stored: Vec<i64> = array(&listed.body, "SamplingRuleRecords")
        .iter()
        .filter_map(|record| record.get("SamplingRule"))
        .filter(|rule| rule.get("RuleName").and_then(Value::as_str) == Some(name))
        .filter_map(|rule| rule.get("Priority").and_then(Value::as_i64))
        .collect();
    assert_eq!(
        stored,
        vec![400],
        "a refused create must leave the held rule as it was, got {listed:?}"
    );

    // Deleted, the name is free again, and the rule created under it is refused
    // with the same sentence as the one before it.
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
    refused_as_already_existing("a re-created rule of the same name", &third);
}

// =============================================================================
// 2. N callers for one name, and WHICH rule each of them submits: one winner,
//    and the refusal of the step that caught each loser
// =============================================================================

/// Assert exactly one of `answers` won, and answer the losers in order.
fn one_winner<'a>(case: &str, answers: &'a [Answer]) -> Vec<&'a Answer> {
    let winners = answers
        .iter()
        .filter(|answer| answer.status == 200)
        .count();
    assert_eq!(
        winners, 1,
        "case {case:?}: exactly one create may win, got {answers:?}"
    );
    answers.iter().filter(|answer| answer.status != 200).collect()
}

/// How many times `name` is held in the account.
async fn times_held(xray: &Xray, name: &str) -> usize {
    let listed = xray
        .call_in_region(DEFAULT_REGION, "/GetSamplingRules", &json!({}))
        .await;
    array(&listed.body, "SamplingRuleRecords")
        .iter()
        .filter(|record| {
            record
                .get("SamplingRule")
                .and_then(|rule| rule.get("RuleName"))
                .and_then(Value::as_str)
                == Some(name)
        })
        .count()
}

#[tokio::test]
async fn callers_asking_one_after_another_each_read_the_name_as_held() {
    // Each call here is answered before the next is made, so every loser's read
    // of the name finds the winner's rule already there — and each loser asks
    // for a rule that DIFFERS from it, which is what that read refuses. The rows
    // differ along how many callers ask for the name.
    for callers in [2_usize, 5, 9] {
        let xray = Xray::new();
        let name = format!("queue-{callers}");
        let mut answers: Vec<Answer> = Vec::new();
        for caller in 0..callers {
            // A priority of its own per caller, so no call states the rule the
            // call before it stored.
            let body = rule_body(&name, 300 + caller as i64);
            answers.push(xray.call("/CreateSamplingRule", &body).await);
        }
        let case = format!("{callers} creates of one name, one after another");
        for loser in one_winner(&case, &answers) {
            refused_as_already_existing(&case, loser);
        }
        assert_eq!(
            times_held(&xray, &name).await,
            1,
            "case {case:?}: the name must be held exactly once afterwards"
        );
    }
}

/// RULE: what the service's own read of the name refuses is a name already HELD,
/// so the rule a late create submits decides nothing: a create stating exactly
/// the rule already stored under the name and one differing in ANY member of it
/// are both refused by that read, with the one fixed sentence.
///
/// The rows walk every member of `SamplingRule` a caller supplies
/// (`smithy_query(query="shape",shape="SamplingRule")`), each stated differently
/// in turn, beside the row that states none of them differently. All of them are
/// sequential calls, so what separates the rows is the rule they submit and
/// nothing about timing.
#[tokio::test]
async fn a_create_of_a_held_name_is_refused_by_the_name_read_whatever_rule_it_submits() {
    let xray = Xray::new();
    let name = "granite-sampler";
    // The rule under test carries a value for every member a caller may state,
    // so a row can differ in any one of them.
    let stored = json!({
        "SamplingRule": {
            "RuleName": name,
            "ResourceARN": "*",
            "Priority": 500,
            "FixedRate": 0.05,
            "ReservoirSize": 1,
            "ServiceName": "*",
            "ServiceType": "*",
            "Host": "*",
            "HTTPMethod": "*",
            "URLPath": "*",
            "Version": 1,
            "Attributes": { "stage": "beta" },
            "SamplingRateBoost": { "MaxRate": 0.5, "CooldownWindowMinutes": 10 },
        }
    });
    let created = xray.call("/CreateSamplingRule", &stored).await;
    assert_eq!(
        created.status, 200,
        "the rule under test must be created, got {created:?}"
    );

    // Stating exactly that rule again: the name is held, which is what the read
    // refuses, so the fixed sentence answers rather than the constraint's.
    let identical = xray.call("/CreateSamplingRule", &stored).await;
    refused_as_already_existing("the rule already stored", &identical);
    // Asked again, the answer is the same sentence: nothing about the stored
    // resource or the request enters it.
    let twice = xray.call("/CreateSamplingRule", &stored).await;
    refused_as_already_existing("the rule already stored, twice", &twice);

    // One member stated differently, per member: the name is held either way, so
    // the same read refuses it with the same sentence.
    let differences: Vec<(&str, Value)> = vec![
        ("ResourceARN", json!("arn:aws:ec2:us-east-1:000000000000:instance/i-0")),
        ("Priority", json!(501)),
        ("FixedRate", json!(0.1)),
        ("ReservoirSize", json!(2)),
        ("ServiceName", json!("checkout")),
        ("ServiceType", json!("AWS::EC2::Instance")),
        ("Host", json!("example.com")),
        ("HTTPMethod", json!("GET")),
        ("URLPath", json!("/checkout/*")),
        ("Attributes", json!({ "stage": "prod" })),
        (
            "SamplingRateBoost",
            json!({ "MaxRate": 0.6, "CooldownWindowMinutes": 10 }),
        ),
    ];
    for (member, value) in differences {
        let mut body = stored.clone();
        body["SamplingRule"][member] = value.clone();
        let answer = xray.call("/CreateSamplingRule", &body).await;
        refused_as_already_existing(&format!("a rule differing in {member} ({value})"), &answer);
    }

    // None of those refusals touched the stored rule: the name is still held
    // exactly once.
    assert_eq!(
        times_held(&xray, name).await,
        1,
        "the name must still be held exactly once"
    );
}

#[tokio::test]
async fn callers_in_flight_together_are_refused_for_the_resource_that_won() {
    // The calls are IN FLIGHT together, so each one reads the name while it is
    // still free and all but one lose the insert: what refuses a loser is then
    // the store's uniqueness constraint, naming the resource that won. The rows
    // differ along how many callers race, and each row re-runs its race on a
    // name a deleted rule had held, which is a DIFFERENT resource and so a
    // different id.
    for callers in [2_usize, 5] {
        let xray = Xray::new();
        let name = format!("race-{callers}");
        let body = rule_body(&name, 300);
        let answers = in_flight(&xray, &body, callers).await;
        let case = format!("{callers} creates of one name, in flight together");
        let named: Vec<String> = one_winner(&case, &answers)
            .into_iter()
            .map(|loser| refused_for_the_resource_that_won(&case, loser))
            .collect();
        let one_id = named.first().cloned().unwrap_or_default();
        assert!(
            named.iter().all(|id| *id == one_id),
            "case {case:?}: every loser lost to the one winner, so every refusal names one id, \
             got {named:?}"
        );
        assert_eq!(
            times_held(&xray, &name).await,
            1,
            "case {case:?}: the name must be held exactly once afterwards"
        );

        // The same race again on the freed name: the resource holding it is a
        // new one, so the id its losers are refused for is a different id.
        let deleted = xray
            .call("/DeleteSamplingRule", &json!({ "RuleName": name }))
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the winner's rule must be deletable, got {deleted:?}"
        );
        let again = in_flight(&xray, &body, callers).await;
        let case = format!("{callers} creates of one freed name, in flight together");
        let renamed: Vec<String> = one_winner(&case, &again)
            .into_iter()
            .map(|loser| refused_for_the_resource_that_won(&case, loser))
            .collect();
        assert!(
            renamed.iter().all(|id| *id != one_id),
            "case {case:?}: the name holds a different resource now, so its losers must name a \
             different internal id, got {renamed:?} beside {one_id:?}"
        );
    }
}

/// `callers` creates of the same rule, all IN FLIGHT together: the calls are
/// joined rather than awaited one at a time, so every one of them has read the
/// name before the winner's insert lands.
async fn in_flight(xray: &Xray, body: &Value, callers: usize) -> Vec<Answer> {
    let create = || xray.call("/CreateSamplingRule", body);
    match callers {
        2 => {
            let (first, second) = ::tokio::join!(create(), create());
            vec![first, second]
        }
        5 => {
            let (first, second, third, fourth, fifth) =
                ::tokio::join!(create(), create(), create(), create(), create());
            vec![first, second, third, fourth, fifth]
        }
        other => panic!("this suite joins 2 or 5 creates, not {other}"),
    }
}

// The same in-flight rule is graded once more by
// `graphs_rules_retrievals_and_insights::concurrent_creates_of_one_rule_name_have_exactly_one_winner`,
// which fires five joined creates for one name and reads each loser's refusal for the one internal
// id the winner is stored under: the one-winner rule stands there, in one place.
