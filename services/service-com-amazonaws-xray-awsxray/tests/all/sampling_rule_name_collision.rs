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
//! together — joined by one task, and DISPATCHED TOGETHER from separate tasks,
//! which is the shape a client with a thread per caller sends them in.
//!
//! Nothing here asserts on elapsed time, and no case is decided by how fast the
//! host runs: a row either awaits the answers it is about, asks again only once
//! they have come back, or runs on a paused clock, where virtual time carries
//! the span a create spends between its read of the name and its write.
//! `graphs_rules_retrievals_and_insights` grades the in-flight case once more,
//! over the five joined creates it fires for one name.

use std::sync::Arc;

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

// =============================================================================
// 3. Callers DISPATCHED TOGETHER, each on its own task: what they race in is the
//    span between the service's read of the name and its write
// =============================================================================

/// How many callers race for one name, per row: a pair, a handful, the five of
/// the recorded burst, and more than that.
const RACER_COUNTS: &[usize] = &[2, 3, 5, 9];

/// The one answer of `answers` that stored the rule.
fn the_winner<'a>(case: &str, answers: &'a [Answer]) -> &'a Answer {
    answers
        .iter()
        .find(|answer| answer.status == 200)
        .unwrap_or_else(|| panic!("case {case:?}: one create must have won, got {answers:?}"))
}

/// The `Priority` the rule named `name` is stored with, as `GetSamplingRules`
/// reports it.
async fn stored_priority(xray: &Xray, name: &str) -> Option<i64> {
    let listed = xray
        .call_in_region(DEFAULT_REGION, "/GetSamplingRules", &json!({}))
        .await;
    array(&listed.body, "SamplingRuleRecords")
        .iter()
        .filter_map(|record| record.get("SamplingRule"))
        .find(|rule| rule.get("RuleName").and_then(Value::as_str) == Some(name))
        .and_then(|rule| rule.get("Priority").and_then(Value::as_i64))
}

/// Every `RuleName` the account holds, walked across the pages
/// `GetSamplingRules` serves them in — so a case holding more rules than one
/// page carries still sees all of them.
async fn held_names(xray: &Xray) -> Vec<String> {
    /// More pages than any case here fills, so a walk that does not terminate
    /// fails the case instead of hanging.
    const MAX_PAGES: usize = 50;

    let mut names: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut body = json!({});
        if let Some(token) = token.as_deref() {
            body["NextToken"] = json!(token);
        }
        let page = xray
            .call_in_region(DEFAULT_REGION, "/GetSamplingRules", &body)
            .await;
        assert_eq!(
            page.status, 200,
            "the stored rules must be listable, got {page:?}"
        );
        names.extend(
            array(&page.body, "SamplingRuleRecords")
                .iter()
                .filter_map(|record| record.get("SamplingRule"))
                .filter_map(|rule| rule.get("RuleName").and_then(Value::as_str))
                .map(str::to_string),
        );
        token = page.get("NextToken").as_str().map(str::to_string);
        if token.is_none() {
            return names;
        }
    }
    panic!("the walk of the sampling-rule pages must terminate, got {names:?}")
}

/// `racers` creates of `name`, every one of them sent before any answer about
/// the name could have come back.
///
/// Each call is SPAWNED on its own task, so the calls contend for the service
/// the way callers with a thread each do, rather than being polled one after
/// another by a single task: every one of them is in flight before any of them
/// can take the name. `identical` states one rule for every racer; otherwise
/// each racer carries a priority of its own, which the rule under test reads
/// nothing from.
async fn dispatched_together(
    xray: &Arc<Xray>,
    name: &str,
    racers: usize,
    identical: bool,
) -> Vec<Answer> {
    let mut calls = Vec::with_capacity(racers);
    for racer in 0..racers {
        let xray = Arc::clone(xray);
        let priority = if identical {
            300
        } else {
            300 + i64::try_from(racer).unwrap_or_default()
        };
        let body = rule_body(name, priority);
        calls.push(::tokio::spawn(async move {
            xray.call("/CreateSamplingRule", &body).await
        }));
    }
    let mut answers = Vec::with_capacity(racers);
    for call in calls {
        answers.push(call.await.expect("each racing create is answered"));
    }
    answers
}

/// RULE: which of the two refusals a duplicate `RuleName` is answered with turns
/// on whether the caller could have been TOLD the name was taken before it sent
/// the request — so every create a client dispatched before any answer about the
/// name existed takes its read while the name is free and is refused by the
/// store's uniqueness constraint, naming the one resource that won it, while a
/// caller that waits for an answer and only then asks again reads the name as
/// held.
///
/// The rows differ along how many callers race, along whether the racers state
/// one rule or each its own, and along whether the name they race for was ever
/// held before — and each row goes on to the caller that asks AFTER the burst
/// was answered, which is the other side of the same rule.
///
/// The clock is PAUSED: the span a create spends between its read of the name
/// and its write runs on the runtime's clock, so virtual time carries it while
/// nothing sleeps, and which answer each racer gets follows from the order the
/// runtime serves them in rather than from how fast or how loaded the host is.
#[tokio::test(start_paused = true)]
async fn creates_dispatched_together_are_refused_for_the_resource_that_won() {
    for racers in RACER_COUNTS.iter().copied() {
        for identical in [true, false] {
            let xray = Arc::new(Xray::new());
            let name = format!("burst-{racers}-{identical}");
            let case = format!(
                "{racers} creates of one name dispatched together, {}",
                if identical {
                    "all stating one rule"
                } else {
                    "each stating its own"
                }
            );

            let answers = dispatched_together(&xray, &name, racers, identical).await;
            let stored_by_the_winner = the_winner(&case, &answers)
                .get("SamplingRuleRecord")
                .get("SamplingRule")
                .and_then(|rule| rule.get("Priority"))
                .and_then(Value::as_i64);
            let named: Vec<String> = one_winner(&case, &answers)
                .into_iter()
                .map(|loser| refused_for_the_resource_that_won(&case, loser))
                .collect();
            let won = named.first().cloned().unwrap_or_default();
            assert!(
                named.iter().all(|id| *id == won),
                "case {case:?}: every loser lost to the one winner, so every refusal names one \
                 id, got {named:?}"
            );
            assert_eq!(
                times_held(&xray, &name).await,
                1,
                "case {case:?}: the name must be held exactly once afterwards"
            );
            // The rule the winner answered with is the rule the store holds: the
            // losers wrote nothing over it.
            assert_eq!(
                stored_priority(&xray, &name).await,
                stored_by_the_winner,
                "case {case:?}: the stored rule must be the one the winner answered with, got \
                 {stored_by_the_winner:?}"
            );

            // Every answer of the burst is in, so a caller asking now could have
            // been told the name was taken: the service's own read of the name
            // is what refuses it, in that step's fixed sentence.
            let afterwards = xray
                .call("/CreateSamplingRule", &rule_body(&name, 900))
                .await;
            refused_as_already_existing(&format!("{case}, then one more caller"), &afterwards);

            // The same race on the freed name: the resource holding it is a new
            // one, so its losers are refused for a different internal id.
            let deleted = xray
                .call("/DeleteSamplingRule", &json!({ "RuleName": name }))
                .await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: the winner's rule must be deletable, got {deleted:?}"
            );
            let again = dispatched_together(&xray, &name, racers, identical).await;
            let case = format!("{case}, on the freed name");
            let renamed: Vec<String> = one_winner(&case, &again)
                .into_iter()
                .map(|loser| refused_for_the_resource_that_won(&case, loser))
                .collect();
            assert!(
                renamed.iter().all(|id| *id != won),
                "case {case:?}: the name holds a different resource now, so its losers must name \
                 a different internal id, got {renamed:?} beside {won:?}"
            );
            assert_eq!(
                times_held(&xray, &name).await,
                1,
                "case {case:?}: the name must be held exactly once afterwards"
            );
        }
    }
}

/// RULE, from the other side: creates of DIFFERENT names dispatched together are
/// not racing one another at all — each one takes the name it asks for, and no
/// caller is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creates_of_different_names_dispatched_together_each_take_their_own() {
    for callers in RACER_COUNTS.iter().copied() {
        let xray = Arc::new(Xray::new());
        let mut calls = Vec::with_capacity(callers);
        for caller in 0..callers {
            let xray = Arc::clone(&xray);
            let name = format!("own-{callers}-{caller}");
            let body = rule_body(&name, 300 + i64::try_from(caller).unwrap_or_default());
            calls.push(::tokio::spawn(async move {
                xray.call("/CreateSamplingRule", &body).await
            }));
        }
        let case = format!("{callers} creates of {callers} names dispatched together");
        for call in calls {
            let answer = call.await.expect("each create is answered");
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a create of a name of its own must succeed, got {answer:?}"
            );
        }
        let held = held_names(&xray).await;
        for caller in 0..callers {
            let name = format!("own-{callers}-{caller}");
            assert_eq!(
                held.iter().filter(|stored| **stored == name).count(),
                1,
                "case {case:?}: {name} must be held exactly once, got {held:?}"
            );
        }
    }
}

/// RULE (the two steps, in TIME): a create's read of the name and its write of
/// the rule under it are separated by the span the service spends serving it, so
/// a create that read a free name has not stored it yet while it is still being
/// served — which is exactly what lets a caller dispatched beside it read the
/// same name as free and be refused by the store's uniqueness constraint rather
/// than by the read.
///
/// On a PAUSED clock, so that span is carried by virtual time: the create being
/// served cannot reach its write while anything else has work to do, which makes
/// each step below observable instead of raced, and nothing here waits. The
/// separation itself is read off the store in ONE look, so no case turns on when
/// the runtime next goes idle; the two refusals are then asserted over the wire.
#[tokio::test(start_paused = true)]
async fn the_name_is_not_written_while_the_create_that_read_it_is_being_served() {
    use service_com_amazonaws_xray_awsxray::state::ComAmazonawsXrayAwsxrayState;

    /// More yields than the runtime needs to serve the spawned create up to its
    /// own waiting point, so a create that is never served fails the case
    /// instead of hanging.
    const YIELDS: usize = 100;

    let (xray, engine) = Xray::with_engine();
    let xray = Arc::new(xray);
    let store = engine.state::<ComAmazonawsXrayAwsxrayState>();
    let name = "obsidian-sampler";

    // One create of a free name, in flight on its own task.
    let create = |priority: i64| {
        let xray = Arc::clone(&xray);
        let body = rule_body(name, priority);
        ::tokio::spawn(async move { xray.call("/CreateSamplingRule", &body).await })
    };
    let first = create(500);

    // Hand the runtime back until that create has taken its read of the name:
    // it then counts as in flight for the name, and has not yet written.
    for _ in 0..YIELDS {
        if store
            .lock()
            .sampling_rule_creates_in_flight
            .contains_key(name)
        {
            break;
        }
        ::tokio::task::yield_now().await;
    }
    // Both halves read in one go, with nothing awaited in between, so what is
    // asserted is one state of the store: a create of the name being served,
    // and no rule written under it yet.
    let (in_flight, written) = {
        let store = store.lock();
        (
            store.sampling_rule_creates_in_flight.contains_key(name),
            store.sampling_rules.contains_key(name),
        )
    };
    assert!(
        in_flight,
        "the create must still be being served after it read the name, in flight: {:?}",
        store.lock().sampling_rule_creates_in_flight
    );
    assert!(
        !written,
        "the create being served must not have written its rule yet, stored: {:?}",
        store.lock().sampling_rules.keys().collect::<Vec<_>>()
    );

    // A second caller, dispatched while the first is still being served: it
    // reads the same free name, so what refuses it is the store's constraint,
    // naming the resource that won.
    let second = create(501);
    let answers = vec![
        first.await.expect("the first create is answered"),
        second.await.expect("the second create is answered"),
    ];
    let case = "a create dispatched beside one still being served";
    for loser in one_winner(case, &answers) {
        let named = refused_for_the_resource_that_won(case, loser);
        let holder = store
            .lock()
            .sampling_rule_internal_id_in_use(name)
            .unwrap_or_default();
        assert_eq!(
            named, holder,
            "case {case:?}: the refusal must name the resource that won the name, got {named:?} \
             against {holder:?}"
        );
    }
    assert_eq!(
        times_held(&xray, name).await,
        1,
        "case {case:?}: the name must be held exactly once afterwards"
    );
}

/// RULE, read off the store rather than off any clock: WHICH of the two refusals
/// answers a create of a held name is decided by whether a create of that name
/// is still BEING SERVED — one is refused by the store's uniqueness constraint,
/// naming the resource that holds the name by the internal id the store holds it
/// under, and once that create has been answered the next one is refused by the
/// service's own read of the name.
///
/// The create still in flight is registered through the service's own report of
/// an arrival, so this case states nothing about timing at all: it holds the one
/// condition the selection turns on and asks what each answer is. The rows differ
/// along what holds the name — a rule the account created, and the
/// service-managed rule that holds its name without being created — and along
/// the priority the colliding create carries.
#[tokio::test]
async fn the_refusal_follows_whether_a_create_of_the_name_is_still_being_served() {
    use service_com_amazonaws_xray_awsxray::state::ComAmazonawsXrayAwsxrayState;

    let (xray, engine) = Xray::with_engine();
    let store = engine.state::<ComAmazonawsXrayAwsxrayState>();

    // A rule the account created, and the service-managed rule, which holds its
    // name without a record ever being stored for it.
    let created = xray
        .call("/CreateSamplingRule", &rule_body("marble-sampler", 450))
        .await;
    assert_eq!(
        created.status, 200,
        "the rule under test must be created, got {created:?}"
    );
    let rows: Vec<(&str, &str, i64)> = vec![
        ("a rule the account created", "marble-sampler", 451),
        (
            "the service-managed rule",
            DEFAULT_SAMPLING_RULE_NAME,
            452,
        ),
    ];

    for (case, name, priority) in rows {
        // The internal id the store holds the name under: what the constraint's
        // refusal has to name, read from the state rather than written out here.
        let holder = store
            .lock()
            .sampling_rule_internal_id_in_use(name)
            .unwrap_or_else(|| panic!("case {case:?}: the name {name:?} must be held"));

        // One create of that name reported as arrived and not yet answered —
        // the service's own report, which is what a create being served leaves
        // behind.
        store.lock().sampling_rule_create_arrived(name);
        let raced = xray
            .call("/CreateSamplingRule", &rule_body(name, priority))
            .await;
        let named = refused_for_the_resource_that_won(case, &raced);
        assert_eq!(
            named, holder,
            "case {case:?}: the refusal must name the resource holding the name, got {named:?} \
             against {holder:?}"
        );

        // That create answered, nothing of the name is in flight: the next one
        // is refused by the read of the name instead, in its fixed sentence.
        store.lock().sampling_rule_create_finished(name);
        let late = xray
            .call("/CreateSamplingRule", &rule_body(name, priority + 1))
            .await;
        refused_as_already_existing(&format!("{case}, once nothing is in flight"), &late);
    }
}
