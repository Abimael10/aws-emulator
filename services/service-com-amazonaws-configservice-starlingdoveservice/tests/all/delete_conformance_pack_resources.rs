// SPDX-License-Identifier: CC0-1.0

//! What a conformance pack's DELETE takes with it, beyond the pack record itself.
//!
//! The rule under test: **deleting a conformance pack undoes its deployment — every
//! resource that deployment created goes with the pack (the Config rules its template
//! declared, the remediation actions declared for those rules, and the evaluation results
//! recorded on those rules), nothing another pack deployed is touched, and the name is free
//! again, so a later deployment of the same name is a NEW pack with its own id and its own
//! resources.**
//!
//! The sibling suite `conformance_pack_deletion` pins the pack record and its rules; the
//! rows here differ along the OTHER resources a deployment creates: how many rules the
//! deleted pack declared, how many of them carry a remediation action, and whether the
//! rules had been evaluated before the delete.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::state::conformance_pack_rule_name;

use crate::conformance_pack_rule_evaluation::{
    error_code, Emulator, CONVERGENCE_BUDGET, POLL_INTERVAL,
};

/// The error code every read of a pack this account does not hold answers with.
const NO_SUCH_PACK: &str = "NoSuchConformancePackException";

/// The error code every read of a Config rule this account does not hold answers with.
const NO_SUCH_RULE: &str = "NoSuchConfigRuleException";

/// The two resource types a conformance pack template declares, as `PutConformancePack`
/// documents them ("a YAML template with two resource types: Config rule
/// (AWS::Config::ConfigRule) and remediation action
/// (AWS::Config::RemediationConfiguration)").
const CONFIG_RULE_TYPE: &str = "AWS::Config::ConfigRule";
/// See [`CONFIG_RULE_TYPE`].
const REMEDIATION_TYPE: &str = "AWS::Config::RemediationConfiguration";

/// A managed rule identifier a template declares, from the Config managed-rule list.
const MANAGED_RULE: &str = "S3_BUCKET_VERSIONING_ENABLED";

/// A managed rule this emulator evaluates on its own, so a pack declaring it has
/// evaluation results recorded on its rule without anyone submitting one.
const EVALUATED_MANAGED_RULE: &str = "IAM_PASSWORD_POLICY";

/// The one `RemediationTargetType` the model declares, and an automation document name of
/// the kind its `TargetId` carries ("the target ID is the name of the SSM document").
const TARGET_TYPE: &str = "SSM_DOCUMENT";
/// See [`TARGET_TYPE`].
const TARGET_DOCUMENT: &str = "AWS-ConfigureS3BucketVersioning";

/// A template declaring one Config rule per name in `rules`, plus a remediation action for
/// each name in `remediated`.
fn template(rules: &[&str], remediated: &[&str], source_identifier: &str) -> String {
    let mut body = String::from("Resources:\n");
    for (index, name) in rules.iter().enumerate() {
        body.push_str(&format!(
            "  Rule{index}:\n    Type: {CONFIG_RULE_TYPE}\n    Properties:\n      \
             ConfigRuleName: {name}\n      Source:\n        Owner: AWS\n        \
             SourceIdentifier: {source_identifier}\n"
        ));
    }
    for (index, name) in remediated.iter().enumerate() {
        body.push_str(&format!(
            "  Remediation{index}:\n    Type: {REMEDIATION_TYPE}\n    Properties:\n      \
             ConfigRuleName: {name}\n      TargetType: {TARGET_TYPE}\n      TargetId: \
             {TARGET_DOCUMENT}\n"
        ));
    }
    body
}

/// One deployed pack: the id Config assigned it and the names Config gave the resources its
/// deployment created.
#[derive(Debug)]
struct Deployed {
    id: String,
    rules: Vec<String>,
    remediated: Vec<String>,
}

/// Deploy `pack` from a template declaring `rules` (and a remediation action for each name
/// in `remediated`) and answer what the deployment created, by the names Config gave it.
async fn put(
    case: &str,
    emulator: &Emulator,
    pack: &str,
    rules: &[&str],
    remediated: &[&str],
    source_identifier: &str,
) -> Deployed {
    emulator
        .ok(
            "PutConformancePack",
            &json!({
                "ConformancePackName": pack,
                "TemplateBody": template(rules, remediated, source_identifier),
            }),
        )
        .await;
    let described = emulator
        .ok(
            "DescribeConformancePacks",
            &json!({ "ConformancePackNames": [pack] }),
        )
        .await;
    let id = described["ConformancePackDetails"][0]["ConformancePackId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the pack's id, got {described}"))
        .to_string();
    Deployed {
        rules: rules
            .iter()
            .map(|name| conformance_pack_rule_name(name, &id))
            .collect(),
        remediated: remediated
            .iter()
            .map(|name| conformance_pack_rule_name(name, &id))
            .collect(),
        id,
    }
}

/// Wait for the deployment the clock owes `pack`, so the pack is no longer held by a
/// workflow in flight.
async fn await_complete(case: &str, emulator: &Emulator, pack: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let status = emulator
            .ok(
                "DescribeConformancePackStatus",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        if status["ConformancePackStatusDetails"][0]["ConformancePackState"]
            == json!("CREATE_COMPLETE")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {pack}'s deployment to settle, got {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The remediation action the account holds for the Config rule `rule`, if it holds one.
async fn remediation_for(emulator: &Emulator, rule: &str) -> Option<Value> {
    let body = emulator
        .ok(
            "DescribeRemediationConfigurations",
            &json!({ "ConfigRuleNames": [rule] }),
        )
        .await;
    body["RemediationConfigurations"]
        .as_array()
        .and_then(|reported| reported.first().cloned())
}

/// Whether the account's own rule read reports the Config rule `rule`.
async fn holds_rule(emulator: &Emulator, rule: &str) -> bool {
    let (status, _) = emulator
        .call("DescribeConfigRules", &json!({ "ConfigRuleNames": [rule] }))
        .await;
    status == 200
}

#[tokio::test]
async fn deleting_a_pack_removes_the_remediation_actions_its_deployment_created() {
    // The rows differ along how many rules the deleted pack declared and how many of them
    // carry a remediation action.
    let cases: [(&str, &[&str], &[&str]); 4] = [
        ("one rule, remediated", &["only-rule"], &["only-rule"]),
        (
            "two rules, one of them remediated",
            &["plain-rule", "fixed-rule"],
            &["fixed-rule"],
        ),
        (
            "two rules, both remediated",
            &["left-rule", "right-rule"],
            &["left-rule", "right-rule"],
        ),
        ("one rule, no remediation at all", &["bare-rule"], &[]),
    ];

    for (case, rules, remediated) in cases {
        let emulator = Emulator::new();
        let deleted_pack = "the-pack";
        let standing_pack = "the-other-pack";
        let deleted = put(
            case,
            &emulator,
            deleted_pack,
            rules,
            remediated,
            MANAGED_RULE,
        )
        .await;
        let standing = put(
            case,
            &emulator,
            standing_pack,
            &["their-rule"],
            &["their-rule"],
            MANAGED_RULE,
        )
        .await;
        await_complete(case, &emulator, deleted_pack).await;
        await_complete(case, &emulator, standing_pack).await;

        // What the deployment created, before the delete: a remediation action keyed by the
        // name Config gave the rule it remediates, targeting the document the template
        // declared.
        for rule in &deleted.remediated {
            let reported = remediation_for(&emulator, rule).await.unwrap_or_else(|| {
                panic!("case {case:?}: expected the deployment to create a remediation for {rule}")
            });
            assert_eq!(
                (
                    reported["ConfigRuleName"].as_str(),
                    reported["TargetType"].as_str(),
                    reported["TargetId"].as_str(),
                ),
                (Some(rule.as_str()), Some(TARGET_TYPE), Some(TARGET_DOCUMENT)),
                "case {case:?}: expected the declared remediation for {rule}, got {reported}"
            );
        }

        emulator
            .ok(
                "DeleteConformancePack",
                &json!({ "ConformancePackName": deleted_pack }),
            )
            .await;

        // The pack's own resources are gone: the rules it deployed, and the remediation
        // actions deployed for them.
        for rule in &deleted.rules {
            assert!(
                !holds_rule(&emulator, rule).await,
                "case {case:?}: expected the deployed rule {rule} to be gone"
            );
            let reported = remediation_for(&emulator, rule).await;
            assert!(
                reported.is_none(),
                "case {case:?}: expected no remediation left for {rule}, got {reported:?}"
            );
        }

        // Another pack's resources are left standing: a delete undoes ONE deployment.
        assert!(
            holds_rule(&emulator, &standing.rules[0]).await,
            "case {case:?}: expected the other pack's rule {} to stand",
            standing.rules[0]
        );
        let theirs = remediation_for(&emulator, &standing.remediated[0]).await;
        assert!(
            theirs.is_some(),
            "case {case:?}: expected the other pack's remediation to stand, got {theirs:?}"
        );

        // The name is free again: deploying it once more is a NEW pack, with its own id and
        // its own resources, and reading it back reports that pack rather than the deleted
        // one.
        let redeployed = put(
            case,
            &emulator,
            deleted_pack,
            rules,
            remediated,
            MANAGED_RULE,
        )
        .await;
        assert_ne!(
            redeployed.id, deleted.id,
            "case {case:?}: expected the redeployed pack to be a new pack, got the deleted \
             pack's id {}",
            deleted.id
        );
        for rule in &redeployed.rules {
            assert!(
                holds_rule(&emulator, rule).await,
                "case {case:?}: expected the redeployment to create {rule}"
            );
        }
        for rule in &redeployed.remediated {
            assert!(
                remediation_for(&emulator, rule).await.is_some(),
                "case {case:?}: expected the redeployment to create a remediation for {rule}"
            );
        }
    }
}

#[tokio::test]
async fn deleting_a_pack_removes_the_evaluation_results_recorded_on_its_rules() {
    let case = "a pack whose rule has been evaluated";
    let emulator = Emulator::new();
    emulator.recording().await;

    let pack = "evaluated-pack";
    let deployed = put(
        case,
        &emulator,
        pack,
        &["evaluated-rule"],
        &[],
        EVALUATED_MANAGED_RULE,
    )
    .await;
    await_complete(case, &emulator, pack).await;
    let rule = deployed.rules[0].clone();

    // The results the rule's evaluation recorded, before the delete: on the rule, which is
    // why the pack's compliance surface and the rule's own read report the same records.
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let own = emulator
            .ok(
                "GetComplianceDetailsByConfigRule",
                &json!({ "ConfigRuleName": rule }),
            )
            .await;
        if own["EvaluationResults"]
            .as_array()
            .is_some_and(|results| !results.is_empty())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's rule to be evaluated, got {own}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let pack_results = emulator
        .ok(
            "GetConformancePackComplianceDetails",
            &json!({ "ConformancePackName": pack }),
        )
        .await;
    assert!(
        pack_results["ConformancePackRuleEvaluationResults"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "case {case:?}: expected the pack to report its rule's results, got {pack_results}"
    );

    emulator
        .ok("DeleteConformancePack", &json!({ "ConformancePackName": pack }))
        .await;

    // A rule's evaluation results live on the rule, so removing the rule removes them:
    // every read that resolves the rule by name refuses it, rather than reporting results
    // for a rule that no longer exists.
    for read in [
        "GetComplianceDetailsByConfigRule",
        "DescribeConfigRuleEvaluationStatus",
    ] {
        let body = match read {
            "GetComplianceDetailsByConfigRule" => json!({ "ConfigRuleName": rule }),
            _ => json!({ "ConfigRuleNames": [rule] }),
        };
        let (status, answered) = emulator.call(read, &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, NO_SUCH_RULE),
            "case {case:?}: expected {read} to refuse the removed rule, got {status} with \
             {answered}"
        );
    }
    let (status, answered) = emulator
        .call(
            "DescribeComplianceByConfigRule",
            &json!({ "ConfigRuleNames": [rule] }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, NO_SUCH_RULE),
        "case {case:?}: expected the rule's compliance read to refuse it, got {status} with \
         {answered}"
    );

    // And the account's unnarrowed compliance read, which refuses nothing, reports no
    // record of that rule at all.
    let all = emulator.ok("DescribeComplianceByConfigRule", &json!({})).await;
    let reported: Vec<&str> = all["ComplianceByConfigRules"]
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .filter_map(|entry| entry["ConfigRuleName"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !reported.contains(&rule.as_str()),
        "case {case:?}: expected no compliance record for the removed rule, got {reported:?}"
    );

    // The pack itself is gone with them.
    let (status, answered) = emulator
        .call(
            "GetConformancePackComplianceDetails",
            &json!({ "ConformancePackName": pack }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, NO_SUCH_PACK),
        "case {case:?}: expected the pack's compliance read to refuse it, got {status} with \
         {answered}"
    );
}
