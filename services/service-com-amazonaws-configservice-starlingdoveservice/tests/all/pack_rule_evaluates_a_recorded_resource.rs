// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED conformance pack's rules report when the rule it deployed is one placed
//! from a RECORDED resource.
//!
//! The rule under test, the other half of `conformance_pack_rule_evaluation`'s: **a Config
//! Managed rule a conformance pack's deployment created is evaluated by the same pass that
//! evaluates the account's own rules — so once the configuration recorder has recorded a
//! resource the rule's scope covers, the pack's rule-level compliance report
//! (`DescribeConformancePackCompliance`) and its pack-level summary
//! (`GetConformancePackComplianceSummary`) both report the verdict that rule placed that
//! resource at, and nothing else.**
//!
//! Config and S3 are mounted on one engine, reusing the harness
//! `config_recorded_resource_compliance` builds: the recorder reads the buckets it records
//! from S3's own API, and the rule's verdict is computed from the configuration item it
//! recorded for one.
//!
//! The rows differ along what the rule generalises over: the recorded state the verdict is
//! computed from (versioning never set, suspended, enabled) and therefore the verdict the
//! pack rolls up.
//!
//! The second half of this suite is about the VALUES such a pack is deployed with: **a
//! conformance pack is deployed as a CloudFormation stack whose parameter values are the
//! request's `ConformancePackInputParameters`, so every reference the template makes to one
//! of its own parameters — `Ref`, in either of the two forms a CloudFormation template
//! writes it, or an `Fn::Sub` string — resolves to the value the request gave that
//! parameter, or to the `Default` the template declares where the request gave none, before
//! the deployment reads what the template declares. A reference nothing resolves leaves the
//! member it stands in for carrying no value at all rather than a made-up one.** Those rows
//! differ along the form the reference is written in, along where its value comes from and
//! along the value itself.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::state::conformance_pack_rule_name;

use crate::config_recorded_resource_compliance::Emulator;

/// The managed rule the pack deploys, a `Source$SourceIdentifier` value from the Config
/// managed-rule list: "checks if versioning is enabled for your S3 buckets".
const MANAGED_RULE: &str = "S3_BUCKET_VERSIONING_ENABLED";

/// The resource type a conformance pack template declares a Config rule as.
const CONFIG_RULE_TYPE: &str = "AWS::Config::ConfigRule";

/// The resource type Config records an S3 bucket under.
const BUCKET_RESOURCE_TYPE: &str = "AWS::S3::Bucket";

/// `ComplianceType` values this suite reads verdicts as.
const COMPLIANT: &str = "COMPLIANT";
/// See [`COMPLIANT`].
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The bucket versioning statuses `PutBucketVersioning` takes
/// (`com.amazonaws.s3#BucketVersioningStatus`).
const ENABLED: &str = "Enabled";
/// See [`ENABLED`].
const SUSPENDED: &str = "Suspended";

/// How long a poll waits for the deployment, the recording pass and the evaluation the
/// clock owes.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(20);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A template declaring one Config rule of [`MANAGED_RULE`], scoped to the resource type
/// it evaluates.
fn template(rule_name: &str) -> String {
    format!(
        "Resources:\n  Rule0:\n    Type: {CONFIG_RULE_TYPE}\n    Properties:\n      \
         ConfigRuleName: {rule_name}\n      Scope:\n        ComplianceResourceTypes:\n        \
         - {BUCKET_RESOURCE_TYPE}\n      Source:\n        Owner: AWS\n        \
         SourceIdentifier: {MANAGED_RULE}\n"
    )
}

/// Deploy `pack` and answer the name Config gave the rule it declared, once the pack
/// reports `CREATE_COMPLETE`.
async fn deploy(case: &str, emulator: &Emulator, pack: &str, rule_name: &str) -> String {
    emulator
        .config(
            "PutConformancePack",
            &json!({ "ConformancePackName": pack, "TemplateBody": template(rule_name) }),
        )
        .await;
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let status = emulator
            .config(
                "DescribeConformancePackStatus",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        let detail = &status["ConformancePackStatusDetails"][0];
        if detail["ConformancePackState"] == json!("CREATE_COMPLETE") {
            let id = detail["ConformancePackId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected the pack id, got {status}"));
            return conformance_pack_rule_name(rule_name, id);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's deployment to settle, got {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `(ConfigRuleName, ComplianceType)` rows the pack's rule-level compliance reports.
async fn rule_rows(emulator: &Emulator, pack: &str) -> Vec<(String, String)> {
    let body = emulator
        .config(
            "DescribeConformancePackCompliance",
            &json!({ "ConformancePackName": pack }),
        )
        .await;
    body["ConformancePackRuleComplianceList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["ConfigRuleName"].as_str().unwrap_or_default().to_string(),
                entry["ComplianceType"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Poll the pack's rule-level compliance until a row reports a verdict either way, and
/// answer the rows.
async fn await_rule_rows(case: &str, emulator: &Emulator, pack: &str) -> Vec<(String, String)> {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let rows = rule_rows(emulator, pack).await;
        if rows
            .iter()
            .any(|(_, compliance)| compliance == COMPLIANT || compliance == NON_COMPLIANT)
        {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's rule to be evaluated, got {rows:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ConformancePackComplianceStatus` the pack's own summary reports.
async fn pack_status(case: &str, emulator: &Emulator, pack: &str) -> String {
    let body = emulator
        .config(
            "GetConformancePackComplianceSummary",
            &json!({ "ConformancePackNames": [pack] }),
        )
        .await;
    body["ConformancePackComplianceSummaryList"][0]["ConformancePackComplianceStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the pack's compliance, got {body}"))
        .to_string()
}

#[tokio::test]
async fn a_packs_rule_reports_the_verdict_it_placed_a_recorded_resource_at() {
    // The rows differ along the recorded state the verdict is computed from.
    let cases: [(&str, Option<&str>, &str); 3] = [
        ("versioning never set", None, NON_COMPLIANT),
        ("versioning suspended", Some(SUSPENDED), NON_COMPLIANT),
        ("versioning enabled", Some(ENABLED), COMPLIANT),
    ];

    for (case, versioning, expected) in cases {
        let emulator = Emulator::new();
        emulator.recording().await;
        let bucket = "pack-rule-bucket";
        emulator.create_bucket(bucket, &[]).await;
        if let Some(status) = versioning {
            emulator.set_versioning(bucket, status).await;
        }

        let pack = "recorded-resource-pack";
        let rule = deploy(case, &emulator, pack, "bucket-versioning").await;
        let rows = await_rule_rows(case, &emulator, pack).await;
        assert_eq!(
            rows,
            vec![(rule.clone(), expected.to_string())],
            "case {case:?}: expected the pack's rule to report {expected}, got {rows:?}"
        );

        // The pack's own summary is the roll-up of exactly those rows.
        let status = pack_status(case, &emulator, pack).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected the pack's compliance to be {expected}, got {status}"
        );

        // And the result behind the row names the recorded resource it was placed on.
        let details = emulator
            .config(
                "GetConformancePackComplianceDetails",
                &json!({ "ConformancePackName": pack }),
            )
            .await;
        let identifier = &details["ConformancePackRuleEvaluationResults"][0]
            ["EvaluationResultIdentifier"]["EvaluationResultQualifier"];
        assert_eq!(
            (
                identifier["ResourceType"].as_str(),
                identifier["ResourceId"].as_str()
            ),
            (Some(BUCKET_RESOURCE_TYPE), Some(bucket)),
            "case {case:?}: expected the result to name the recorded bucket, got {details}"
        );
    }
}

// What a pack is deployed WITH: the values the request gives the template's parameters.

/// The managed rule whose requirements come from the rule's own `InputParameters`, a
/// `Source$SourceIdentifier` value from the Config managed-rule list: "checks if your
/// resources have the tags that you specify".
const REQUIRED_TAGS_RULE: &str = "REQUIRED_TAGS";

/// The template parameter each template below names the bucket by.
const BUCKET_PARAMETER: &str = "BucketNameParam";

/// The bucket with no versioning configuration, which the managed rule finds
/// non-compliant.
const PLAIN_BUCKET: &str = "pack-parameter-plain-bucket";

/// The bucket whose versioning is enabled, which the managed rule finds compliant.
const VERSIONED_BUCKET: &str = "pack-parameter-versioned-bucket";

/// The `Parameters` section lines declaring [`BUCKET_PARAMETER`], carrying `default` as its
/// `Default` where the template declares one.
fn bucket_parameter_lines(default: Option<&str>) -> Vec<String> {
    let mut lines = vec![
        format!("  {BUCKET_PARAMETER}:"),
        "    Type: String".to_string(),
    ];
    if let Some(default) = default {
        lines.push(format!("    Default: {default}"));
    }
    lines
}

/// A YAML template declaring one rule of [`MANAGED_RULE`] whose scope narrows to the bucket
/// `compliance_resource_id` is written as — a reference to a parameter, or a literal.
fn parameterised_template(
    rule_name: &str,
    parameter_lines: &[String],
    compliance_resource_id: &str,
) -> String {
    let mut lines = vec!["Parameters:".to_string()];
    lines.extend(parameter_lines.iter().cloned());
    lines.extend([
        "Resources:".to_string(),
        "  Rule0:".to_string(),
        format!("    Type: {CONFIG_RULE_TYPE}"),
        "    Properties:".to_string(),
        format!("      ConfigRuleName: {rule_name}"),
        "      Scope:".to_string(),
        "        ComplianceResourceTypes:".to_string(),
        format!("        - {BUCKET_RESOURCE_TYPE}"),
        format!("        ComplianceResourceId: {compliance_resource_id}"),
        "      Source:".to_string(),
        "        Owner: AWS".to_string(),
        format!("        SourceIdentifier: {MANAGED_RULE}"),
    ]);
    lines.join("\n")
}

/// The same template written as JSON, where a reference carries the LONG form
/// (`{"Ref": …}`) a JSON template has to write it in.
fn json_template(rule_name: &str) -> String {
    json!({
        "Parameters": { "BucketNameParam": { "Type": "String" } },
        "Resources": {
            "Rule0": {
                "Type": CONFIG_RULE_TYPE,
                "Properties": {
                    "ConfigRuleName": rule_name,
                    "Scope": {
                        "ComplianceResourceTypes": [BUCKET_RESOURCE_TYPE],
                        "ComplianceResourceId": { "Ref": "BucketNameParam" },
                    },
                    "Source": { "Owner": "AWS", "SourceIdentifier": MANAGED_RULE },
                },
            },
        },
    })
    .to_string()
}

/// Deploy `pack` from `template` with the input parameters `input`, and answer the id
/// Config assigned the pack once its deployment reports `CREATE_COMPLETE`.
async fn deploy_template(
    case: &str,
    emulator: &Emulator,
    pack: &str,
    template: &str,
    input: &Value,
) -> String {
    emulator
        .config(
            "PutConformancePack",
            &json!({
                "ConformancePackName": pack,
                "TemplateBody": template,
                "ConformancePackInputParameters": input,
            }),
        )
        .await;
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let status = emulator
            .config(
                "DescribeConformancePackStatus",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        let detail = &status["ConformancePackStatusDetails"][0];
        if detail["ConformancePackState"] == json!("CREATE_COMPLETE") {
            return detail["ConformancePackId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected the pack id, got {status}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's deployment to settle, got {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the pack's own compliance summary until it reports a verdict either way, and answer
/// it.
async fn await_pack_compliance(case: &str, emulator: &Emulator, pack: &str) -> String {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let body = emulator
            .config(
                "GetConformancePackComplianceSummary",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        let status = body["ConformancePackComplianceSummaryList"][0]
            ["ConformancePackComplianceStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == COMPLIANT || status == NON_COMPLIANT {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's compliance to settle, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The recorded resources the pack's rules placed at a verdict, as `(ResourceType,
/// ResourceId)` pairs.
async fn evaluated_resources(emulator: &Emulator, pack: &str) -> Vec<(String, String)> {
    let details = emulator
        .config(
            "GetConformancePackComplianceDetails",
            &json!({ "ConformancePackName": pack }),
        )
        .await;
    details["ConformancePackRuleEvaluationResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            let qualifier = &result["EvaluationResultIdentifier"]["EvaluationResultQualifier"];
            (
                qualifier["ResourceType"].as_str().unwrap_or_default().to_string(),
                qualifier["ResourceId"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A recording account holding both buckets these rows read: one with no versioning
/// configuration and one with versioning enabled.
async fn recording_account_with_two_buckets() -> Emulator {
    let emulator = Emulator::new();
    emulator.recording().await;
    emulator.create_bucket(PLAIN_BUCKET, &[]).await;
    emulator.create_bucket(VERSIONED_BUCKET, &[]).await;
    emulator.set_versioning(VERSIONED_BUCKET, ENABLED).await;
    emulator
}

/// One row of [`a_packs_rule_is_deployed_with_the_parameter_values_the_request_gave`]: how
/// the template names the bucket, where the value for that parameter comes from, and the
/// bucket and verdict that follow.
struct ParameterCase<'a> {
    /// What this row is, named in every assertion.
    case: &'a str,
    /// The reference the YAML template writes `ComplianceResourceId` as, or `None` for the
    /// row whose template is written as JSON — where a reference carries the long form that
    /// spelling has to write it in.
    reference: Option<&'a str>,
    /// The `Default` the template declares for the parameter, where it declares one.
    default: Option<&'a str>,
    /// The value the request gives the parameter, where it gives one.
    supplied: Option<&'a str>,
    /// The bucket the resolved reference scopes the rule to.
    targeted: &'a str,
    /// The `ComplianceType` that bucket's recorded state places it at.
    expected: &'a str,
}

#[tokio::test]
async fn a_packs_rule_is_deployed_with_the_parameter_values_the_request_gave() {
    let short_form = format!("!Ref {BUCKET_PARAMETER}");
    let sub_form = format!("!Sub \"${{{BUCKET_PARAMETER}}}\"");
    // The rows differ along the FORM the reference is written in (a YAML short form, the
    // long form a JSON template writes, an `Fn::Sub` string), along where the value comes
    // from (the request, the parameter's declared default) and along the value itself.
    let cases: [ParameterCase<'_>; 5] = [
        ParameterCase {
            case: "a Ref short form, resolved from the request's input parameters",
            reference: Some(&short_form),
            default: None,
            supplied: Some(PLAIN_BUCKET),
            targeted: PLAIN_BUCKET,
            expected: NON_COMPLIANT,
        },
        ParameterCase {
            case: "the same reference given the other bucket's name",
            reference: Some(&short_form),
            default: None,
            supplied: Some(VERSIONED_BUCKET),
            targeted: VERSIONED_BUCKET,
            expected: COMPLIANT,
        },
        ParameterCase {
            case: "a Ref long form, the template written as JSON",
            reference: None,
            default: None,
            supplied: Some(VERSIONED_BUCKET),
            targeted: VERSIONED_BUCKET,
            expected: COMPLIANT,
        },
        ParameterCase {
            case: "an Fn::Sub string naming the parameter",
            reference: Some(&sub_form),
            default: None,
            supplied: Some(PLAIN_BUCKET),
            targeted: PLAIN_BUCKET,
            expected: NON_COMPLIANT,
        },
        ParameterCase {
            case: "no value given, so the parameter's declared Default stands",
            reference: Some(&short_form),
            default: Some(VERSIONED_BUCKET),
            supplied: None,
            targeted: VERSIONED_BUCKET,
            expected: COMPLIANT,
        },
    ];

    for ParameterCase {
        case,
        reference,
        default,
        supplied,
        targeted,
        expected,
    } in cases
    {
        let emulator = recording_account_with_two_buckets().await;
        let pack = "template-parameter-pack";
        let declared_rule = "bucket-versioning";
        // A row with no YAML reference is the one written as JSON, where the reference
        // carries the long form that spelling has to write it in.
        let template = match reference {
            Some(reference) => {
                parameterised_template(declared_rule, &bucket_parameter_lines(default), reference)
            }
            None => json_template(declared_rule),
        };
        let input = match supplied {
            Some(value) => json!([{ "ParameterName": BUCKET_PARAMETER, "ParameterValue": value }]),
            None => json!([]),
        };

        let pack_id = deploy_template(case, &emulator, pack, &template, &input).await;
        let status = await_pack_compliance(case, &emulator, pack).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected the pack's compliance to be {expected}, got {status}"
        );

        // The rule placed exactly the bucket the resolved reference scoped it to, which is
        // what tells a resolved reference apart from one that narrowed nothing.
        let evaluated = evaluated_resources(&emulator, pack).await;
        assert_eq!(
            evaluated,
            vec![(BUCKET_RESOURCE_TYPE.to_string(), targeted.to_string())],
            "case {case:?}: expected the rule to have evaluated {targeted} alone, got \
             {evaluated:?}"
        );

        // And the rule the pack deployed carries the resolved value in its own scope.
        let rules = emulator
            .config(
                "DescribeConfigRules",
                &json!({
                    "ConfigRuleNames": [conformance_pack_rule_name(declared_rule, &pack_id)]
                }),
            )
            .await;
        let scoped_to = rules["ConfigRules"][0]["Scope"]["ComplianceResourceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            scoped_to, targeted,
            "case {case:?}: expected the deployed rule to be scoped to {targeted}, got {rules}"
        );
    }
}

#[tokio::test]
async fn a_resolved_declaration_is_read_back_and_an_unresolved_reference_carries_no_value() {
    let emulator = Emulator::new();
    let case = "a template naming parameters in a scope and in a rule's input parameters";
    let tag_parameter = "TagKeyParam";
    let unset_parameter = "UnsetParam";
    let tag_key = "owner";

    // Two rules: one whose scope and whose own input parameters both name parameters the
    // request gives values for, and one naming a parameter nothing resolves.
    let template = [
        "Parameters:".to_string(),
        format!("  {BUCKET_PARAMETER}:"),
        "    Type: String".to_string(),
        format!("  {tag_parameter}:"),
        "    Type: String".to_string(),
        format!("  {unset_parameter}:"),
        "    Type: String".to_string(),
        "Resources:".to_string(),
        "  Tagged:".to_string(),
        format!("    Type: {CONFIG_RULE_TYPE}"),
        "    Properties:".to_string(),
        "      ConfigRuleName: tagged".to_string(),
        "      InputParameters:".to_string(),
        format!("        tag1Key: !Ref {tag_parameter}"),
        "      Scope:".to_string(),
        "        ComplianceResourceTypes:".to_string(),
        format!("        - {BUCKET_RESOURCE_TYPE}"),
        format!("        ComplianceResourceId: !Ref {BUCKET_PARAMETER}"),
        "      Source:".to_string(),
        "        Owner: AWS".to_string(),
        format!("        SourceIdentifier: {REQUIRED_TAGS_RULE}"),
        "  Unresolved:".to_string(),
        format!("    Type: {CONFIG_RULE_TYPE}"),
        "    Properties:".to_string(),
        "      ConfigRuleName: unresolved".to_string(),
        "      Scope:".to_string(),
        "        ComplianceResourceTypes:".to_string(),
        format!("        - {BUCKET_RESOURCE_TYPE}"),
        format!("        ComplianceResourceId: !Ref {unset_parameter}"),
        "      Source:".to_string(),
        "        Owner: AWS".to_string(),
        format!("        SourceIdentifier: {MANAGED_RULE}"),
    ]
    .join("\n");

    let pack = "read-back-parameter-pack";
    let pack_id = deploy_template(
        case,
        &emulator,
        pack,
        &template,
        &json!([
            { "ParameterName": BUCKET_PARAMETER, "ParameterValue": PLAIN_BUCKET },
            { "ParameterName": tag_parameter, "ParameterValue": tag_key },
        ]),
    )
    .await;

    let tagged_name = conformance_pack_rule_name("tagged", &pack_id);
    let unresolved_name = conformance_pack_rule_name("unresolved", &pack_id);
    let rules = emulator
        .config(
            "DescribeConfigRules",
            &json!({ "ConfigRuleNames": [&tagged_name, &unresolved_name] }),
        )
        .await;
    let rule_of = |name: &str| -> Value {
        rules["ConfigRules"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|rule| rule["ConfigRuleName"] == json!(name))
            .unwrap_or_else(|| panic!("case {case:?}: expected the rule {name}, got {rules}"))
    };

    // The reference in the rule's SCOPE resolved to the value the request gave.
    let tagged = rule_of(&tagged_name);
    assert_eq!(
        tagged["Scope"]["ComplianceResourceId"].as_str(),
        Some(PLAIN_BUCKET),
        "case {case:?}: expected the rule scoped to {PLAIN_BUCKET}, got {tagged}"
    );

    // So did the one inside the rule's own input parameters, which is the same rule applied
    // to another member of the same declaration.
    let input_parameters: Value = tagged["InputParameters"]
        .as_str()
        .and_then(|document| serde_json::from_str(document).ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a parameters document, got {tagged}"));
    assert_eq!(
        input_parameters,
        json!({ "tag1Key": tag_key }),
        "case {case:?}: expected the rule's input parameters to carry {tag_key}, got \
         {input_parameters}"
    );

    // A reference nothing resolves leaves the member with NO value — the parameter's own
    // name is not a resource id, and the deployment invents none.
    let unresolved = rule_of(&unresolved_name);
    assert_eq!(
        unresolved["Scope"]["ComplianceResourceId"].as_str(),
        None,
        "case {case:?}: expected no ComplianceResourceId on the unresolved rule, got \
         {unresolved}"
    );
    assert_eq!(
        unresolved["Scope"]["ComplianceResourceTypes"],
        json!([BUCKET_RESOURCE_TYPE]),
        "case {case:?}: expected the unresolved rule to keep its resource type, got \
         {unresolved}"
    );
}

/// The longest value a `ConformancePackInputParameter` may carry
/// (`ConformancePackInputParameter$ParameterValue`, `@length(0, 4096)`: "values can have a
/// maximum length of 4096 characters").
const PARAMETER_VALUE_MAX_CHARS: usize = 4_096;

/// How many times the capped row below names that parameter in one `Fn::Sub`: enough that
/// the text it would stand for is past what one deployment substitutes in total (sixteen
/// `TemplateBody` maximums of 51 200 characters each), and one reference short of that is
/// not.
const SUBSTITUTIONS_PAST_THE_CAP: usize = 16 * 51_200 / PARAMETER_VALUE_MAX_CHARS + 1;

#[tokio::test]
async fn a_reference_that_would_grow_the_template_past_the_cap_resolves_to_no_value() {
    let emulator = Emulator::new();
    let case = "one Fn::Sub naming a maximum-length parameter past the substitution cap";
    let big_parameter = "BigParam";
    let big_value = "a".repeat(PARAMETER_VALUE_MAX_CHARS);
    let past_the_cap = format!("${{{big_parameter}}}").repeat(SUBSTITUTIONS_PAST_THE_CAP);

    // Two rules: one whose scope would expand past what a deployment substitutes, and one
    // naming a parameter of ordinary length beside it.
    let template = [
        "Parameters:".to_string(),
        format!("  {big_parameter}:"),
        "    Type: String".to_string(),
        format!("  {BUCKET_PARAMETER}:"),
        "    Type: String".to_string(),
        "Resources:".to_string(),
        "  Capped:".to_string(),
        format!("    Type: {CONFIG_RULE_TYPE}"),
        "    Properties:".to_string(),
        "      ConfigRuleName: capped".to_string(),
        "      Scope:".to_string(),
        "        ComplianceResourceTypes:".to_string(),
        format!("        - {BUCKET_RESOURCE_TYPE}"),
        format!("        ComplianceResourceId: !Sub \"{past_the_cap}\""),
        "      Source:".to_string(),
        "        Owner: AWS".to_string(),
        format!("        SourceIdentifier: {MANAGED_RULE}"),
        "  Ordinary:".to_string(),
        format!("    Type: {CONFIG_RULE_TYPE}"),
        "    Properties:".to_string(),
        "      ConfigRuleName: ordinary".to_string(),
        "      Scope:".to_string(),
        "        ComplianceResourceTypes:".to_string(),
        format!("        - {BUCKET_RESOURCE_TYPE}"),
        format!("        ComplianceResourceId: !Ref {BUCKET_PARAMETER}"),
        "      Source:".to_string(),
        "        Owner: AWS".to_string(),
        format!("        SourceIdentifier: {MANAGED_RULE}"),
    ]
    .join("\n");

    let pack = "capped-parameter-pack";
    let pack_id = deploy_template(
        case,
        &emulator,
        pack,
        &template,
        &json!([
            { "ParameterName": big_parameter, "ParameterValue": big_value },
            { "ParameterName": BUCKET_PARAMETER, "ParameterValue": PLAIN_BUCKET },
        ]),
    )
    .await;

    let capped_name = conformance_pack_rule_name("capped", &pack_id);
    let ordinary_name = conformance_pack_rule_name("ordinary", &pack_id);
    let rules = emulator
        .config(
            "DescribeConfigRules",
            &json!({ "ConfigRuleNames": [&capped_name, &ordinary_name] }),
        )
        .await;
    let scope_of = |name: &str| -> Value {
        rules["ConfigRules"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|rule| rule["ConfigRuleName"] == json!(name))
            .unwrap_or_else(|| panic!("case {case:?}: expected the rule {name}, got {rules}"))["Scope"]
            .clone()
    };

    // The reference past the cap is left unresolved, so the member carries no value — the
    // expansion is not performed and nothing of the parameter's value reaches the rule.
    let capped = scope_of(&capped_name);
    assert_eq!(
        capped["ComplianceResourceId"].as_str(),
        None,
        "case {case:?}: expected no ComplianceResourceId on the capped rule, got {capped}"
    );

    // The cap is on what one pass substitutes, not on resolving at all: the reference of
    // ordinary length beside it resolves as usual.
    let ordinary = scope_of(&ordinary_name);
    assert_eq!(
        ordinary["ComplianceResourceId"].as_str(),
        Some(PLAIN_BUCKET),
        "case {case:?}: expected the ordinary rule scoped to {PLAIN_BUCKET}, got {ordinary}"
    );
}
