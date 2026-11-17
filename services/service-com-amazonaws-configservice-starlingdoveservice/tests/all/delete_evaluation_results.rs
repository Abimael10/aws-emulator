// SPDX-License-Identifier: CC0-1.0

//! `DeleteEvaluationResults`: discarding what one Config rule found.
//!
//! The rule pinned here: deleting the evaluation results of the named Config rule
//! discards EVERY verdict recorded against that rule — whichever writer recorded it,
//! whichever resource and evaluation mode it is about and whatever it found — and
//! nothing else. The rule itself, the history of Config invoking it and every other
//! rule's verdicts stay, so from then on each compliance surface computes its answer
//! from an empty set of results: `GetComplianceDetailsByConfigRule` reports no result
//! and the rule rolls up to `INSUFFICIENT_DATA`, which is what a rule nothing has
//! evaluated reports. Because there is then nothing left to delete, the call repeats as
//! a no-op; a name this account holds no rule under is refused and deletes nothing.
//! A rule an AWS SERVICE owns is not the caller's to act on at all: the delete is
//! refused with `AccessDeniedException` naming that owner and every verdict recorded
//! against the rule survives, as are the two sibling calls that would act on such a
//! rule (`DeleteConfigRule`, `PutConfigRule`) — while a verdict DELIVERED for it is
//! still accepted, because reporting to a rule is not acting on it.
//!
//! Every case drives the emulator the way a client does — an `awsJson1_1` `POST /`
//! through the same `MultiServiceRouter` the production router builds — over a TABLE of
//! inputs rather than one recorded call: the rows differ along how many verdicts the
//! rule holds, which resource types and ids they are about, what each one found, which
//! evaluation mode each was recorded in and which writer recorded it.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::state::{
    EvaluationResultEntry, CONFORMANCE_PACK_SERVICE_PRINCIPAL, EVALUATION_MODE_DETECTIVE,
    EVALUATION_MODE_PROACTIVE,
};
use service_com_amazonaws_configservice_starlingdoveservice::ComAmazonawsConfigserviceStarlingdoveserviceState as ConfigState;

use crate::config_setup_and_rule_loop::{managed_rule, with_recorder_and_channel, Emulator};
use crate::external_evaluation::put_process_check_rule;
use crate::put_evaluations::{custom_lambda_rule, result_token};

/// Every published `ComplianceType` value, from
/// `smithy_query(query="shape", shape="ComplianceType")`: `COMPLIANT`, `NON_COMPLIANT`,
/// `NOT_APPLICABLE`, `INSUFFICIENT_DATA`.
const COMPLIANT: &str = "COMPLIANT";
/// See [`COMPLIANT`].
const NON_COMPLIANT: &str = "NON_COMPLIANT";
/// See [`COMPLIANT`].
const NOT_APPLICABLE: &str = "NOT_APPLICABLE";
/// See [`COMPLIANT`]. The value Config derives for a rule no result places either way,
/// which is what a rule nothing has evaluated reports
/// (`DescribeComplianceByConfigRule`: "If Config has no evaluation results for the
/// rule, it returns INSUFFICIENT_DATA").
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// Resource types a verdict names, from
/// `smithy_query(query="shape", shape="ResourceType")`; `AWS::::Account` is the
/// account-level type Config accepts for an evaluation of the account itself
/// (`ExternalEvaluation$ComplianceResourceType`).
const EC2_VOLUME: &str = "AWS::EC2::Volume";
/// See [`EC2_VOLUME`].
const S3_BUCKET: &str = "AWS::S3::Bucket";
/// See [`EC2_VOLUME`].
const ACCOUNT: &str = "AWS::::Account";

/// The longest `ConfigRuleName` this operation accepts
/// (`smithy_query(query="operation", operation="DeleteEvaluationResults")`:
/// `ConfigRuleName` targets `StringWithCharLimit64`, `length 1..64`).
const CONFIG_RULE_NAME_MAX: usize = 64;

/// Past the span an evaluation pass takes to complete, so the invocation history this
/// delete must leave alone has settled before it is read.
const PAST_THE_PASS: Duration = Duration::from_millis(800);

/// The rule every case deletes the results of.
const UNDER_TEST: &str = "rule-under-test";

/// A second rule, holding a verdict of its own, which is what shows a delete is scoped
/// to the rule the request names.
const OTHER: &str = "other-rule";

/// One verdict a rule recorded: the type and id of the resource it is about, and the
/// compliance it places that resource at.
type Verdict = (&'static str, &'static str, &'static str);

/// The verdict the rule beside the one under test holds throughout.
const OTHER_VERDICT: Verdict = (S3_BUCKET, "audit-bucket", NON_COMPLIANT);

/// How the rule under test came to hold the verdicts a case deletes.
#[derive(Debug, Clone, Copy)]
enum Recorded {
    /// Nothing has evaluated the rule, so it holds no verdict at all.
    Nothing,
    /// The rule's own evaluation DELIVERED these verdicts over the wire with
    /// `PutEvaluations`, which is how a Config Custom Lambda rule's verdicts reach
    /// Config.
    Delivered(&'static [Verdict]),
    /// A verdict on the account delivered from OUTSIDE Config with
    /// `PutExternalEvaluation` carrying this annotation, which is how a Config Process
    /// Check rule's verdict reaches Config.
    External(&'static str),
    /// These verdicts recorded in each of these evaluation modes, the way Config's own
    /// evaluation pass over the configuration state it recorded records them.
    InModes(&'static [&'static str], &'static [Verdict]),
}

/// The account this emulator serves, which is the account a process check evaluates.
fn account() -> String {
    emulator_server::emulator_account_id().to_string()
}

/// Now, as the epoch seconds the wire carries a modelled timestamp as.
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_secs_f64()
}

/// Record one evaluation result per entry of `verdicts`, in each mode of `modes`,
/// against the rule `name` — the way Config's own evaluation pass records what it found,
/// through the bounded writer the store exposes.
fn record_in_modes(emulator: &Emulator, name: &str, verdicts: &[Verdict], modes: &[&str]) {
    let state = emulator.engine.state::<ConfigState>();
    let mut store = state.lock();
    let rule = store
        .config_rules
        .get_mut(name)
        .unwrap_or_else(|| panic!("the rule {name:?} must have been created first"));
    for mode in modes {
        for (resource_type, resource_id, compliance) in verdicts {
            rule.record_evaluation_result(EvaluationResultEntry::new(
                resource_type,
                resource_id,
                mode,
                compliance,
                SystemTime::UNIX_EPOCH,
            ));
        }
    }
}

/// How many results `verdicts` recorded in `modes` leave on a rule: a rule holds one
/// result per (resource type, resource id, evaluation mode), so verdicts naming one
/// resource twice in a mode leave one.
fn distinct_results(verdicts: &[Verdict], modes: &[&str]) -> usize {
    let mut keys = BTreeSet::new();
    for mode in modes {
        for (resource_type, resource_id, _) in verdicts {
            keys.insert((*resource_type, *resource_id, *mode));
        }
    }
    keys.len()
}

/// Create the rule `name` in `emulator` and give it the verdicts `recorded` names,
/// answering how many evaluation results it then holds.
async fn seed(emulator: &Emulator, name: &str, recorded: Recorded) -> usize {
    match recorded {
        Recorded::Nothing => {
            emulator.ok("PutConfigRule", &managed_rule(name)).await;
            0
        }
        Recorded::Delivered(verdicts) => {
            emulator.ok("PutConfigRule", &custom_lambda_rule(name)).await;
            let evaluations: Vec<Value> = verdicts
                .iter()
                .enumerate()
                .map(|(index, (resource_type, resource_id, compliance))| {
                    json!({
                        "ComplianceResourceType": resource_type,
                        "ComplianceResourceId": resource_id,
                        "ComplianceType": compliance,
                        "OrderingTimestamp": 1_700_000_000u64 + index as u64,
                    })
                })
                .collect();
            emulator
                .ok(
                    "PutEvaluations",
                    &json!({
                        "ResultToken": result_token(emulator, name),
                        "Evaluations": evaluations,
                    }),
                )
                .await;
            distinct_results(verdicts, &[EVALUATION_MODE_DETECTIVE])
        }
        Recorded::External(annotation) => {
            put_process_check_rule(emulator, name).await;
            emulator
                .ok(
                    "PutExternalEvaluation",
                    &json!({
                        "ConfigRuleName": name,
                        "ExternalEvaluation": {
                            "ComplianceResourceType": ACCOUNT,
                            "ComplianceResourceId": account(),
                            "ComplianceType": NON_COMPLIANT,
                            "Annotation": annotation,
                            "OrderingTimestamp": now_seconds(),
                        },
                    }),
                )
                .await;
            1
        }
        Recorded::InModes(modes, verdicts) => {
            emulator.ok("PutConfigRule", &managed_rule(name)).await;
            record_in_modes(emulator, name, verdicts, modes);
            distinct_results(verdicts, modes)
        }
    }
}

/// Every evaluation result the rule `name` holds, as `GetComplianceDetailsByConfigRule`
/// reports them.
async fn reported(case: &str, emulator: &Emulator, name: &str) -> Vec<Value> {
    let body = emulator
        .ok(
            "GetComplianceDetailsByConfigRule",
            &json!({ "ConfigRuleName": name, "Limit": 100 }),
        )
        .await;
    body["EvaluationResults"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EvaluationResults list, got {body}"))
        .clone()
}

/// The `ComplianceType` `DescribeComplianceByConfigRule` rolls the rule `name` up to.
async fn rolled_up(case: &str, emulator: &Emulator, name: &str) -> String {
    let body = emulator
        .ok(
            "DescribeComplianceByConfigRule",
            &json!({ "ConfigRuleNames": [name] }),
        )
        .await;
    let entries = body["ComplianceByConfigRules"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected ComplianceByConfigRules, got {body}"));
    let entry = entries
        .iter()
        .find(|entry| entry["ConfigRuleName"] == json!(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected an entry for {name:?}, got {body}"));
    entry["Compliance"]["ComplianceType"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ComplianceType, got {entry}"))
        .to_string()
}

/// The names of every Config rule this account holds.
async fn rule_names(case: &str, emulator: &Emulator) -> BTreeSet<String> {
    let body = emulator.ok("DescribeConfigRules", &json!({})).await;
    body["ConfigRules"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ConfigRules list, got {body}"))
        .iter()
        .filter_map(|rule| rule["ConfigRuleName"].as_str().map(str::to_string))
        .collect()
}

/// The `ConfigRuleEvaluationStatus` the rule `name` reports, where Config reports one
/// for its kind: the record of Config INVOKING the rule, which is not what the rule
/// found and so is not what a delete of its results discards.
async fn evaluation_status(case: &str, emulator: &Emulator, name: &str) -> Option<Value> {
    let body = emulator
        .ok(
            "DescribeConfigRuleEvaluationStatus",
            &json!({ "ConfigRuleNames": [name] }),
        )
        .await;
    body["ConfigRulesEvaluationStatus"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a ConfigRulesEvaluationStatus list, got {body}")
        })
        .iter()
        .find(|status| status["ConfigRuleName"] == json!(name))
        .cloned()
}

/// Delete the results of the rule `name` and assert the response carries no member, the
/// way the modelled empty response does.
async fn delete_results(case: &str, emulator: &Emulator, name: &str) {
    let answered = emulator
        .ok(
            "DeleteEvaluationResults",
            &json!({ "ConfigRuleName": name }),
        )
        .await;
    assert!(
        answered
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "case {case:?}: the response carries no member, got {answered}"
    );
}

#[tokio::test]
async fn deleting_a_rules_results_discards_every_verdict_recorded_against_that_rule() {
    // (case, what the rule under test holds). The rows differ along how many verdicts
    // the rule holds, which resource types and ids they are about, what each one found,
    // which evaluation mode each was recorded in, and which writer recorded it.
    let cases: [(&str, Recorded); 7] = [
        ("a rule nothing has evaluated", Recorded::Nothing),
        (
            "one delivered verdict",
            Recorded::Delivered(&[(EC2_VOLUME, "vol-0001", NON_COMPLIANT)]),
        ),
        (
            "several delivered verdicts of several resource types",
            Recorded::Delivered(&[
                (EC2_VOLUME, "vol-0002", COMPLIANT),
                (EC2_VOLUME, "vol-0003", NON_COMPLIANT),
                (S3_BUCKET, "logs-bucket", NOT_APPLICABLE),
                (ACCOUNT, "000000000000", COMPLIANT),
            ]),
        ),
        (
            "a verdict delivered from outside Config",
            Recorded::External("the quarterly access review has not run"),
        ),
        (
            "results of Config's own detective pass",
            Recorded::InModes(
                &[EVALUATION_MODE_DETECTIVE],
                &[
                    (S3_BUCKET, "config-bucket", COMPLIANT),
                    (S3_BUCKET, "public-bucket", NON_COMPLIANT),
                ],
            ),
        ),
        (
            "results of a proactive pass only",
            Recorded::InModes(
                &[EVALUATION_MODE_PROACTIVE],
                &[(EC2_VOLUME, "vol-0004", NON_COMPLIANT)],
            ),
        ),
        (
            "one resource placed in both evaluation modes",
            Recorded::InModes(
                &[EVALUATION_MODE_DETECTIVE, EVALUATION_MODE_PROACTIVE],
                &[(EC2_VOLUME, "vol-0005", COMPLIANT)],
            ),
        ),
    ];

    for (case, recorded) in cases {
        let emulator = Emulator::new();
        with_recorder_and_channel(&emulator).await;
        emulator.ok("PutConfigRule", &managed_rule(OTHER)).await;
        record_in_modes(
            &emulator,
            OTHER,
            &[OTHER_VERDICT],
            &[EVALUATION_MODE_DETECTIVE],
        );

        let expected = seed(&emulator, UNDER_TEST, recorded).await;
        let before = reported(case, &emulator, UNDER_TEST).await;
        assert_eq!(
            before.len(),
            expected,
            "case {case:?}: expected {expected} recorded result(s) before the delete, \
             got {before:#?}"
        );

        // The pass a write owes the rule completes on its own a span later; letting it
        // settle first is what makes the invocation history below a stable value to
        // compare the post-delete one against.
        tokio::time::sleep(PAST_THE_PASS).await;
        let invocations_before = evaluation_status(case, &emulator, UNDER_TEST).await;

        delete_results(case, &emulator, UNDER_TEST).await;

        let after = reported(case, &emulator, UNDER_TEST).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected the rule to hold no evaluation result after the \
             delete, got {after:#?}"
        );
        let rolls_up_to = rolled_up(case, &emulator, UNDER_TEST).await;
        assert_eq!(
            rolls_up_to, INSUFFICIENT_DATA,
            "case {case:?}: expected the rule to roll up to {INSUFFICIENT_DATA} once no \
             result places it, got {rolls_up_to:?}"
        );

        // The rule and the record of Config invoking it are not what was deleted.
        let held = rule_names(case, &emulator).await;
        assert!(
            held.contains(UNDER_TEST) && held.contains(OTHER),
            "case {case:?}: expected both rules to survive the delete, got {held:?}"
        );
        let invocations_after = evaluation_status(case, &emulator, UNDER_TEST).await;
        assert_eq!(
            invocations_after, invocations_before,
            "case {case:?}: expected the rule's invocation history to be untouched, got \
             {invocations_after:?} after {invocations_before:?}"
        );

        // Nothing but the named rule's results was discarded.
        let other_after = reported(case, &emulator, OTHER).await;
        assert_eq!(
            other_after.len(),
            1,
            "case {case:?}: expected the other rule to keep its verdict, got {other_after:#?}"
        );
        let other_rolls_up_to = rolled_up(case, &emulator, OTHER).await;
        assert_eq!(
            other_rolls_up_to, NON_COMPLIANT,
            "case {case:?}: expected the other rule to still roll up to {NON_COMPLIANT}, \
             got {other_rolls_up_to:?}"
        );

        // There is nothing left to delete, so the call repeats as a no-op rather than a
        // failure.
        delete_results(case, &emulator, UNDER_TEST).await;
        let repeated = reported(case, &emulator, UNDER_TEST).await;
        assert!(
            repeated.is_empty(),
            "case {case:?}: expected a repeat of the delete to change nothing, got \
             {repeated:#?}"
        );
    }
}

#[tokio::test]
async fn refuses_a_name_this_account_holds_no_rule_under_and_deletes_nothing() {
    let emulator = Emulator::new();
    with_recorder_and_channel(&emulator).await;
    emulator.ok("PutConfigRule", &managed_rule(UNDER_TEST)).await;
    record_in_modes(
        &emulator,
        UNDER_TEST,
        &[(EC2_VOLUME, "vol-0001", NON_COMPLIANT)],
        &[EVALUATION_MODE_DETECTIVE],
    );

    let deleted = "deleted-rule";
    emulator.ok("PutConfigRule", &managed_rule(deleted)).await;
    emulator
        .ok("DeleteConfigRule", &json!({ "ConfigRuleName": deleted }))
        .await;

    // The longest name the member accepts, which names no rule here: the published
    // bound itself reaches the handler and is answered as a missing rule.
    let longest = "n".repeat(CONFIG_RULE_NAME_MAX);

    // The rows differ along WHY the name names no rule: never created, created and
    // deleted again, a held name in another case, a held name with more after it, and a
    // name at the published length maximum.
    let cases: [(&str, &str); 5] = [
        ("a name nothing created", "no-such-rule"),
        ("a rule that was deleted", deleted),
        ("a held name in another case", "RULE-UNDER-TEST"),
        ("a held name with more after it", "rule-under-test-2"),
        ("a name at the published length maximum", longest.as_str()),
    ];

    for (case, requested) in cases {
        let (status, parsed) = emulator
            .call(
                "DeleteEvaluationResults",
                &json!({ "ConfigRuleName": requested }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed}"
        );
        let code = parsed["__type"]
            .as_str()
            .map(|wire| wire.rsplit('#').next().unwrap_or(wire))
            .unwrap_or("<none>");
        assert_eq!(
            code, "NoSuchConfigRuleException",
            "case {case:?}: expected NoSuchConfigRuleException, got {code:?} in {parsed}"
        );
        let expected = format!(
            "The ConfigRule '{requested}' provided in the request is invalid. Please check \
             the configRule name."
        );
        let message = parsed["message"].as_str().unwrap_or("<none>");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal worded {expected:?}, got {message:?}"
        );
        let kept = reported(case, &emulator, UNDER_TEST).await;
        assert_eq!(
            kept.len(),
            1,
            "case {case:?}: a refused delete discards nothing, got {kept:#?}"
        );
    }

    // One character past the published maximum is refused at the request boundary,
    // before the handler runs at all.
    let (status, code) = emulator
        .refused(
            "DeleteEvaluationResults",
            &json!({ "ConfigRuleName": "n".repeat(CONFIG_RULE_NAME_MAX + 1) }),
        )
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case \"one past the published maximum\": expected HTTP 400 ValidationException, \
         got {status} {code}"
    );

    // The name is keyed the way the rule was stored, so one padded with whitespace
    // names the same rule and deletes its results.
    delete_results(
        "a held name padded with whitespace",
        &emulator,
        &format!("  {UNDER_TEST}  "),
    )
    .await;
    let after = reported("a held name padded with whitespace", &emulator, UNDER_TEST).await;
    assert!(
        after.is_empty(),
        "case \"a held name padded with whitespace\": expected the rule's results to be \
         discarded, got {after:#?}"
    );
}

/// The fixed text the refusal of a call on a rule an AWS service owns OPENS with.
const DENIED_PREFIX: &str = "An AWS service owns ";

/// The fixed text that same refusal ENDS with.
const DENIED_SUFFIX: &str = ". You do not have permissions to take action on this rule.";

/// How long a poll waits for a condition the service owes before giving up. Generous:
/// nothing here asserts on elapsed time, the loop only has to outlast a loaded host.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The resource type a conformance pack template declares a Config rule as, from
/// `PutConformancePack`'s own model doc.
const CONFIG_RULE_TYPE: &str = "AWS::Config::ConfigRule";

/// The Config Managed rule a process check IS, named from *List of Config Managed
/// Rules*: the rule whose verdicts are delivered from outside Config.
const PROCESS_CHECK_IDENTIFIER: &str = "AWS_CONFIG_PROCESS_CHECK";

/// Record `owner` as the service that created the rule `name`, the way a service that
/// deploys a rule leaves `CreatedBy` on it — through the crate's own store seam, which
/// is how this suite seeds state no operation of this API mints for a caller.
fn owned_by(emulator: &Emulator, name: &str, owner: &str) {
    let state = emulator.engine.state::<ConfigState>();
    let mut store = state.lock();
    let rule = store
        .config_rules
        .get_mut(name)
        .unwrap_or_else(|| panic!("the rule {name:?} must have been created first"));
    rule.created_by = Some(owner.to_string());
}

/// Deliver one verdict for the process check rule `name`.
async fn deliver(emulator: &Emulator, name: &str, annotation: &str) {
    emulator
        .ok(
            "PutExternalEvaluation",
            &json!({
                "ConfigRuleName": name,
                "ExternalEvaluation": {
                    "ComplianceResourceType": ACCOUNT,
                    "ComplianceResourceId": account(),
                    "ComplianceType": NON_COMPLIANT,
                    "Annotation": annotation,
                    "OrderingTimestamp": now_seconds(),
                },
            }),
        )
        .await;
}

/// Call `operation` and require the refusal a call on a rule owned by `owner` answers:
/// HTTP 400 `AccessDeniedException`, naming that owner between the two fixed texts.
async fn refused_as_owned(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: &Value,
    owner: &str,
) {
    let (status, parsed) = emulator.call(operation, body).await;
    let code = parsed["__type"]
        .as_str()
        .map(|wire| wire.rsplit('#').next().unwrap_or(wire))
        .unwrap_or("<none>")
        .to_string();
    assert_eq!(
        (status, code.as_str()),
        (400, "AccessDeniedException"),
        "case {case:?}: expected 400 AccessDeniedException from {operation}, got {status} \
         {code:?} with {parsed}"
    );
    let message = parsed["message"].as_str().unwrap_or("<none>");
    let expected = format!("{DENIED_PREFIX}{owner}{DENIED_SUFFIX}");
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?} from {operation}, got {message:?}"
    );
}

/// A conformance pack template declaring one process check Config rule named
/// `rule_name`.
fn pack_template(rule_name: &str) -> String {
    format!(
        "Resources:\n  OnlyRule:\n    Type: {CONFIG_RULE_TYPE}\n    Properties:\n      \
         ConfigRuleName: {rule_name}\n      Source:\n        Owner: AWS\n        \
         SourceIdentifier: {PROCESS_CHECK_IDENTIFIER}\n"
    )
}

/// Wait for a pack to have deployed the rule it declared as `declared` and answer the
/// name Config gave it, polling for the CONDITION rather than for a span.
async fn deployed_rule_name(emulator: &Emulator, declared: &str) -> String {
    let started = ::std::time::Instant::now();
    loop {
        let held = rule_names("a pack-deployed rule", emulator).await;
        if let Some(name) = held.iter().find(|name| name.starts_with(declared)) {
            return name.clone();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "the rule the pack declares as {declared:?} must be deployed, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_pack_deployed_rules_verdicts_survive_the_delete_its_owner_denies() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "PutConformancePack",
            &json!({
                "ConformancePackName": "owned-pack",
                "TemplateBody": pack_template("process-check"),
            }),
        )
        .await;
    let rule = deployed_rule_name(&emulator, "process-check").await;
    let case = "a pack-deployed rule";

    // The pack's own deployment is what recorded the owner, so the rule reports it.
    let described = emulator.ok("DescribeConfigRules", &json!({})).await;
    let detail = described["ConfigRules"]
        .as_array()
        .and_then(|rules| {
            rules
                .iter()
                .find(|held| held["ConfigRuleName"] == json!(rule))
        })
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the deployed rule must be readable"));
    assert_eq!(
        detail["CreatedBy"],
        json!(CONFORMANCE_PACK_SERVICE_PRINCIPAL),
        "case {case:?}: the pack's rule reports the service that created it, got {detail}"
    );

    // A verdict DELIVERED for the rule is accepted: reporting to a rule a service owns
    // is not acting on it.
    deliver(&emulator, &rule, "delivered for the pack's rule").await;
    let delivered = reported(case, &emulator, &rule).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: the delivered verdict must be recorded, got {delivered:#?}"
    );

    // Every call that would act on the rule is refused, each naming the owner.
    let body = json!({ "ConfigRuleName": rule });
    for operation in ["DeleteEvaluationResults", "DeleteConfigRule"] {
        refused_as_owned(
            &emulator,
            case,
            operation,
            &body,
            CONFORMANCE_PACK_SERVICE_PRINCIPAL,
        )
        .await;
    }
    refused_as_owned(
        &emulator,
        case,
        "PutConfigRule",
        &managed_rule(&rule),
        CONFORMANCE_PACK_SERVICE_PRINCIPAL,
    )
    .await;

    // Nothing moved: the rule is still the one the pack deployed, and the delivered
    // verdict survived the delete that was refused.
    let after = emulator.ok("DescribeConfigRules", &json!({})).await;
    let still = after["ConfigRules"]
        .as_array()
        .and_then(|rules| {
            rules
                .iter()
                .find(|held| held["ConfigRuleName"] == json!(rule))
        })
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the refused delete must leave the rule"));
    assert_eq!(
        still, detail,
        "case {case:?}: the refused calls must leave the rule as the pack deployed it, \
         got {still}"
    );
    assert_eq!(
        reported(case, &emulator, &rule).await,
        delivered,
        "case {case:?}: the refused delete must leave every verdict recorded against \
         the rule"
    );
}

#[tokio::test]
async fn whichever_service_owns_a_rule_the_calls_that_act_on_it_are_refused() {
    // Rows: the service principals a Config rule can carry in `CreatedBy` — the one a
    // conformance pack deploys through, and two other Amazon Web Services service
    // principals — so the refusal is shown to be about the ownership rather than about
    // one owner. `None` is the control: a rule this account created itself.
    let owners: [(&str, Option<&str>); 4] = [
        (
            "a conformance pack's own rule",
            Some(CONFORMANCE_PACK_SERVICE_PRINCIPAL),
        ),
        (
            "a rule another service created",
            Some("securityhub.amazonaws.com"),
        ),
        (
            "a rule a third service created",
            Some("config-multiaccountsetup.amazonaws.com"),
        ),
        ("a rule this account created itself", None),
    ];

    for (case, owner) in owners {
        let emulator = Emulator::new();
        with_recorder_and_channel(&emulator).await;
        put_process_check_rule(&emulator, UNDER_TEST).await;
        if let Some(owner) = owner {
            owned_by(&emulator, UNDER_TEST, owner);
        }
        // A verdict is delivered either way, so the delete has something to destroy.
        deliver(&emulator, UNDER_TEST, "a delivered verdict").await;
        let delivered = reported(case, &emulator, UNDER_TEST).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the delivered verdict must be recorded, got {delivered:#?}"
        );

        let body = json!({ "ConfigRuleName": UNDER_TEST });
        match owner {
            Some(owner) => {
                refused_as_owned(&emulator, case, "DeleteEvaluationResults", &body, owner).await;
                assert_eq!(
                    reported(case, &emulator, UNDER_TEST).await,
                    delivered,
                    "case {case:?}: the refused delete must leave the verdict"
                );

                refused_as_owned(
                    &emulator,
                    case,
                    "PutConfigRule",
                    &managed_rule(UNDER_TEST),
                    owner,
                )
                .await;

                refused_as_owned(&emulator, case, "DeleteConfigRule", &body, owner).await;
                assert!(
                    rule_names(case, &emulator).await.contains(UNDER_TEST),
                    "case {case:?}: the refused delete must leave the rule"
                );
            }
            None => {
                // The control: this account's own rule is served by all three calls.
                delete_results(case, &emulator, UNDER_TEST).await;
                assert!(
                    reported(case, &emulator, UNDER_TEST).await.is_empty(),
                    "case {case:?}: the delete must discard the rule's verdicts"
                );
                emulator.ok("PutConfigRule", &managed_rule(UNDER_TEST)).await;
                emulator.ok("DeleteConfigRule", &body).await;
                assert!(
                    !rule_names(case, &emulator).await.contains(UNDER_TEST),
                    "case {case:?}: the delete must remove the rule"
                );
            }
        }
    }
}
