// SPDX-License-Identifier: CC0-1.0

//! The IAM Access Analyzer resources a stack declares, end to end: the analyzer and the archive
//! rule of one exist in that service itself, under the identities the stack publishes, a changed
//! declaration reaches them, dropping the rule from the template takes it away, deleting the
//! stack takes both away — and a declaration that service REFUSES fails the resource and rolls
//! the stack back.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it is
//! DEPLOYED by calling the service that owns the type — so the handle the stack publishes
//! (`Ref`) resolves in that service, the values the type publishes are the ones that service
//! reported, and what that service refuses is a FAILED RESOURCE rather than a template-format
//! error. The `Template format error: Unrecognized resource types: [<types>]` refusal is
//! reserved for a type CloudFormation does NOT know, which the misspelling table below pins.
//!
//! The deploy rows differ along what the rule generalises over: which KIND of analyzer the
//! template declares (four of the six values `Type` publishes, each with its own published
//! per-Region limit), whether the archive rules are declared INLINE on the analyzer or as
//! `AWS::AccessAnalyzer::ArchiveRule` resources of their own, how the criteria are spelled (the
//! criteria map a rule resource states, the `Property` list an inline rule states), how many
//! rules one analyzer carries, whether the declaration carries the type's own `Tags`, which
//! published return value each output reads (`Ref`, `Fn::GetAtt Arn`, `CreatedAt`, `UpdatedAt`),
//! and the template spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the IAM Access Analyzer calls over that
//! service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls IAM Access Analyzer observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, REGION,
    SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value, ACCOUNT_ID};

/// The two resource types under test.
const ANALYZER: &str = "AWS::AccessAnalyzer::Analyzer";
const ARCHIVE_RULE: &str = "AWS::AccessAnalyzer::ArchiveRule";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with: the
/// refusal no deploy row of this suite may ever see, and the one the misspelling table expects
/// verbatim.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The kinds of analyzer `Type` publishes that this suite declares. "You can create only one
/// analyzer per account per Region. You can create up to 5 analyzers per organization per
/// Region" (`smithy_query(query="operation", operation="CreateAnalyzer",
/// model_path="services/service-com-amazonaws-accessanalyzer-accessanalyzer/smithy-model/\
/// com-amazonaws-accessanalyzer-accessanalyzer.json")`), so one row per kind is what a single
/// account and Region can hold.
const ACCOUNT_TYPE: &str = "ACCOUNT";
const ORGANIZATION_TYPE: &str = "ORGANIZATION";
const ACCOUNT_UNUSED_ACCESS_TYPE: &str = "ACCOUNT_UNUSED_ACCESS";
const ORGANIZATION_UNUSED_ACCESS_TYPE: &str = "ORGANIZATION_UNUSED_ACCESS";

/// The status IAM Access Analyzer reports for an analyzer it is monitoring with
/// (`AnalyzerStatus`: "an `ACTIVE` analyzer successfully monitors supported resources").
const ACTIVE: &str = "ACTIVE";

/// The error code that service answers for a resource it does not hold, the status it carries
/// and the fixed texts of the two refusals this suite reads (its `ResourceNotFoundException`
/// `message` is a constant per resource kind, with the identifier in `resourceId`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;
const ANALYZER_NOT_FOUND: &str = "Analyzer not found";
const ARCHIVE_RULE_NOT_FOUND: &str = "Archive rule not found";

/// The three tags CloudFormation records on every resource it provisions, which an analyzer the
/// stack created has to carry beside the stack's own and the declaration's own.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const STACK_ID_TAG: &str = "aws:cloudformation:stack-id";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// One IAM Access Analyzer call over the `restJson1` wire its model declares, on the method and
/// URI that operation's own `@http` binding names, signed for `access-analyzer` — that model's
/// own `sigv4Name` — in the Region the stack's resources live in.
async fn access_analyzer(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://access-analyzer.{REGION}.amazonaws.com{uri}"
        ))
        .header("host", format!("access-analyzer.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/access-analyzer/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed IAM Access Analyzer request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one analyzer, by the name the stack created it under.
async fn get_analyzer(router: &MultiServiceRouter, analyzer_name: &str) -> (u16, Value) {
    access_analyzer(
        router,
        "GET",
        &format!("/analyzer/{}", form_encode(analyzer_name)),
        None,
    )
    .await
}

/// Its own read of one archive rule, by the analyzer and the rule's own name.
async fn get_archive_rule(
    router: &MultiServiceRouter,
    analyzer_name: &str,
    rule_name: &str,
) -> (u16, Value) {
    access_analyzer(
        router,
        "GET",
        &format!(
            "/analyzer/{}/archive-rule/{}",
            form_encode(analyzer_name),
            form_encode(rule_name)
        ),
        None,
    )
    .await
}

/// Every analyzer that service lists for the account and Region.
async fn list_analyzers(router: &MultiServiceRouter) -> Vec<Value> {
    let (_, answered) = access_analyzer(router, "GET", "/analyzer", None).await;
    answered
        .get("analyzers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every archive rule that service lists for one analyzer, by rule name.
async fn list_archive_rule_names(router: &MultiServiceRouter, analyzer_name: &str) -> Vec<String> {
    let (_, answered) = access_analyzer(
        router,
        "GET",
        &format!("/analyzer/{}/archive-rule", form_encode(analyzer_name)),
        None,
    )
    .await;
    answered
        .get("archiveRules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| Some(rule.get("ruleName")?.as_str()?.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The tags that service holds on one resource, by its ARN — the `@httpLabel` of
/// `GET /tags/{resourceArn}`, escaped as a single path segment the way an SDK sends a non-greedy
/// label.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, answered) =
        access_analyzer(router, "GET", &format!("/tags/{}", form_encode(arn)), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListTagsForResource for {arn}, got {status} with {answered}"
    );
    answered
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ARN an analyzer of `analyzer_name` carries: the form `AnalyzerArn` publishes
/// (`^[^:]*:[^:]*:[^:]*:[^:]*:[^:]*:analyzer/.{1,255}$`), under the account every ARN this
/// emulator emits carries.
fn expected_analyzer_arn(analyzer_name: &str) -> String {
    format!("arn:aws:access-analyzer:{REGION}:{ACCOUNT_ID}:analyzer/{analyzer_name}")
}

/// The identity an archive rule of that analyzer carries: the analyzer's own ARN with the rule's
/// path and name beneath it, a rule being a resource OF the analyzer.
fn expected_rule_arn(analyzer_name: &str, rule_name: &str) -> String {
    format!(
        "{}/archive-rule/{rule_name}",
        expected_analyzer_arn(analyzer_name)
    )
}

/// Submit `template` as `stack_name`, carrying the stack's own tag, and answer the `CreateStack`
/// body: a type AWS publishes is ADMITTED, so this never sees the template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an {ANALYZER} is a type a template \
         may declare — got {status} with body {body}"
    );
    body
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// The output `key` of a described stack, or a panic naming the body that carried none.
fn output(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
}

/// One reported instant to the SECOND, which is the precision the two instants an archive rule
/// publishes carry: the fraction of a `date-time` member's text dropped, never rounded.
fn to_the_second(reported: &str) -> String {
    match reported.strip_suffix('Z').and_then(|body| body.split_once('.')) {
        Some((seconds, _)) => format!("{seconds}Z"),
        None => reported.to_string(),
    }
}

/// What one row expects of the archive rules of its analyzer.
struct Rule {
    /// The rule's own name, as the declaration states it and as that service reports it.
    name: &'static str,
    /// The output key whose value is the rule's identity, where the row reads one — a rule
    /// declared INLINE on the analyzer is no resource of the stack, so it publishes none.
    arn_output: Option<&'static str>,
    /// The output keys reading the two instants the type publishes, where the row reads them.
    created_at_output: Option<&'static str>,
    updated_at_output: Option<&'static str>,
    /// The criteria map that service must report for the rule, whichever spelling the
    /// declaration used.
    criteria: fn() -> Value,
}

/// One row: a template, the analyzer it declares and the rules of it.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    analyzer_name: &'static str,
    analyzer_type: &'static str,
    template: String,
    /// The output key whose value is what `Ref` of the analyzer returned, and the one reading
    /// its `Arn` attribute where the row reads it.
    analyzer_ref_output: &'static str,
    analyzer_arn_output: Option<&'static str>,
    /// The type's own `Tags` the declaration states, which the analyzer must carry beside the
    /// stack's propagated ones.
    tags: &'static [(&'static str, &'static str)],
    rules: Vec<Rule>,
}

/// The criteria the suite's rules narrow findings by, in the map spelling an
/// `AWS::AccessAnalyzer::ArchiveRule` states them in and the `Property` list spelling an
/// analyzer's own inline `ArchiveRules` state them in — both of which become the one
/// `FilterCriteriaMap` the API takes, whose criterion members are `eq`, `neq`, `contains` and
/// `exists` (`smithy_query(query="shape", shape="Criterion",
/// model_path="services/service-com-amazonaws-accessanalyzer-accessanalyzer/smithy-model/\
/// com-amazonaws-accessanalyzer-accessanalyzer.json")`).
fn bucket_criteria() -> Value {
    json!({"resourceType": {"eq": ["AWS::S3::Bucket"]}})
}

fn queue_criteria() -> Value {
    json!({"resourceType": {"eq": ["AWS::SQS::Queue"]}})
}

fn public_criteria() -> Value {
    json!({"isPublic": {"exists": true}})
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: an ACCOUNT analyzer and one archive rule declared as a resource
        // of its own, every published return value read back, in JSON with long-form
        // intrinsics.
        Row {
            case: "an account analyzer and one archive rule resource, every return value read",
            stack_name: "cfn-aa-round-trip",
            analyzer_name: "sim2real_round_trip",
            analyzer_type: ACCOUNT_TYPE,
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Analyzer": {
                        "Type": ANALYZER,
                        "Properties": {
                            "AnalyzerName": "sim2real_round_trip",
                            "Type": ACCOUNT_TYPE,
                            "Tags": [{"Key": "sim2real-aa", "Value": "round-trip"}],
                        }
                    },
                    "ArchiveRule": {
                        "Type": ARCHIVE_RULE,
                        "DependsOn": "Analyzer",
                        "Properties": {
                            "AnalyzerName": "sim2real_round_trip",
                            "RuleName": "ArchiveBuckets",
                            "Filter": {"resourceType": {"Eq": ["AWS::S3::Bucket"]}},
                        }
                    }
                },
                "Outputs": {
                    "AnalyzerRef": {"Value": {"Ref": "Analyzer"}},
                    "AnalyzerArn": {"Value": {"Fn::GetAtt": ["Analyzer", "Arn"]}},
                    "RuleRef": {"Value": {"Ref": "ArchiveRule"}},
                    "RuleArn": {"Value": {"Fn::GetAtt": ["ArchiveRule", "Arn"]}},
                    "RuleCreatedAt": {"Value": {"Fn::GetAtt": ["ArchiveRule", "CreatedAt"]}},
                    "RuleUpdatedAt": {"Value": {"Fn::GetAtt": ["ArchiveRule", "UpdatedAt"]}}
                }
            })
            .to_string(),
            analyzer_ref_output: "AnalyzerRef",
            analyzer_arn_output: Some("AnalyzerArn"),
            tags: &[("sim2real-aa", "round-trip")],
            rules: vec![Rule {
                name: "ArchiveBuckets",
                arn_output: Some("RuleArn"),
                created_at_output: Some("RuleCreatedAt"),
                updated_at_output: Some("RuleUpdatedAt"),
                criteria: bucket_criteria,
            }],
        },
        // An ORGANIZATION analyzer whose rules are declared INLINE, in the `Property` list
        // spelling that property uses — two of them, so the row is about the list rather than
        // about one entry — with no `Tags` of its own, in YAML with the short-form intrinsics.
        Row {
            case: "an organization analyzer with two inline archive rules, YAML",
            stack_name: "cfn-aa-inline-rules",
            analyzer_name: "sim2real_inline",
            analyzer_type: ORGANIZATION_TYPE,
            template: format!(
                "\
Resources:
  Analyzer:
    Type: {ANALYZER}
    Properties:
      AnalyzerName: sim2real_inline
      Type: {ORGANIZATION_TYPE}
      ArchiveRules:
        - RuleName: ArchiveBuckets
          Filter:
            - Property: resourceType
              Eq:
                - AWS::S3::Bucket
        - RuleName: ArchivePublic
          Filter:
            - Property: isPublic
              Exists: true
Outputs:
  AnalyzerRef:
    Value: !Ref Analyzer
"
            ),
            analyzer_ref_output: "AnalyzerRef",
            analyzer_arn_output: None,
            tags: &[],
            rules: vec![
                Rule {
                    name: "ArchiveBuckets",
                    arn_output: None,
                    created_at_output: None,
                    updated_at_output: None,
                    criteria: bucket_criteria,
                },
                Rule {
                    name: "ArchivePublic",
                    arn_output: None,
                    created_at_output: None,
                    updated_at_output: None,
                    criteria: public_criteria,
                },
            ],
        },
        // An UNUSED-ACCESS analyzer — a third value of `Type`, carrying the configuration that
        // kind takes — with two rule RESOURCES of its own, one of which states its analyzer as
        // a `Ref` of the declared analyzer (the ARN that type's `Ref` returns) rather than by
        // name.
        Row {
            case: "an account unused-access analyzer with two archive rule resources",
            stack_name: "cfn-aa-unused-access",
            analyzer_name: "sim2real_unused",
            analyzer_type: ACCOUNT_UNUSED_ACCESS_TYPE,
            template: json!({
                "Resources": {
                    "Analyzer": {
                        "Type": ANALYZER,
                        "Properties": {
                            "AnalyzerName": "sim2real_unused",
                            "Type": ACCOUNT_UNUSED_ACCESS_TYPE,
                            "AnalyzerConfiguration": {
                                "UnusedAccessConfiguration": {"UnusedAccessAge": 90}
                            },
                        }
                    },
                    "BucketRule": {
                        "Type": ARCHIVE_RULE,
                        "DependsOn": "Analyzer",
                        "Properties": {
                            "AnalyzerName": "sim2real_unused",
                            "RuleName": "ArchiveBuckets",
                            "Filter": {"resourceType": {"Eq": ["AWS::S3::Bucket"]}},
                        }
                    },
                    "QueueRule": {
                        "Type": ARCHIVE_RULE,
                        "DependsOn": "Analyzer",
                        "Properties": {
                            "AnalyzerName": {"Ref": "Analyzer"},
                            "RuleName": "ArchiveQueues",
                            "Filter": {"resourceType": {"Eq": ["AWS::SQS::Queue"]}},
                        }
                    }
                },
                "Outputs": {
                    "AnalyzerRef": {"Value": {"Ref": "Analyzer"}},
                    "BucketRuleRef": {"Value": {"Ref": "BucketRule"}},
                    "QueueRuleArn": {"Value": {"Fn::GetAtt": ["QueueRule", "Arn"]}}
                }
            })
            .to_string(),
            analyzer_ref_output: "AnalyzerRef",
            analyzer_arn_output: None,
            tags: &[],
            rules: vec![
                Rule {
                    name: "ArchiveBuckets",
                    arn_output: Some("BucketRuleRef"),
                    created_at_output: None,
                    updated_at_output: None,
                    criteria: bucket_criteria,
                },
                Rule {
                    name: "ArchiveQueues",
                    arn_output: Some("QueueRuleArn"),
                    created_at_output: None,
                    updated_at_output: None,
                    criteria: queue_criteria,
                },
            ],
        },
        // The fourth kind, with a name CloudFormation GENERATES — the declaration states none —
        // so the identity the stack publishes is the one the create recorded for whatever name
        // it generated, and no rule at all.
        Row {
            case: "an organization unused-access analyzer whose name CloudFormation generates",
            stack_name: "cfn-aa-generated-name",
            analyzer_name: "",
            analyzer_type: ORGANIZATION_UNUSED_ACCESS_TYPE,
            template: json!({
                "Resources": {
                    "Analyzer": {
                        "Type": ANALYZER,
                        "Properties": {"Type": ORGANIZATION_UNUSED_ACCESS_TYPE}
                    }
                },
                "Outputs": {
                    "AnalyzerRef": {"Value": {"Ref": "Analyzer"}},
                    "AnalyzerArn": {"Value": {"Fn::GetAtt": ["Analyzer", "Arn"]}}
                }
            })
            .to_string(),
            analyzer_ref_output: "AnalyzerRef",
            analyzer_arn_output: Some("AnalyzerArn"),
            tags: &[],
            rules: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn a_declared_analyzer_and_its_archive_rules_are_created_in_the_owning_service() {
    for row in rows() {
        let case = row.case;
        // One router per row: an account holds ONE analyzer of a kind per Region
        // ("you can create only one analyzer per account per Region"), so each row deploys into
        // an account of its own rather than contesting that published limit.
        let router = router();
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        // `Ref` returns the ARN of the analyzer, which is the identity IAM Access Analyzer
        // recorded for it — and the `Arn` attribute is that same value.
        let published = output(case, &described, row.analyzer_ref_output);
        let analyzer_name = if row.analyzer_name.is_empty() {
            published
                .rsplit_once('/')
                .map(|(_, name)| name.to_string())
                .unwrap_or_default()
        } else {
            row.analyzer_name.to_string()
        };
        assert!(
            !analyzer_name.is_empty(),
            "case {case:?}: expected the published identity to carry the analyzer's name, got \
             {published:?}"
        );
        assert_eq!(
            published,
            expected_analyzer_arn(&analyzer_name),
            "case {case:?}: expected Ref to return the analyzer's ARN"
        );
        if let Some(key) = row.analyzer_arn_output {
            assert_eq!(
                output(case, &described, key),
                published,
                "case {case:?}: expected Fn::GetAtt Arn to be the analyzer's own ARN"
            );
        }

        // The analyzer really exists in that service, under the name and the kind the template
        // declared, and its own reads resolve the identity the stack published.
        let (status, answered) = get_analyzer(&router, &analyzer_name).await;
        assert_eq!(
            (
                status,
                answered.pointer("/analyzer/arn").and_then(Value::as_str),
                answered.pointer("/analyzer/type").and_then(Value::as_str),
                answered.pointer("/analyzer/status").and_then(Value::as_str),
            ),
            (
                200,
                Some(published.as_str()),
                Some(row.analyzer_type),
                Some(ACTIVE),
            ),
            "case {case:?}: expected IAM Access Analyzer to hold the analyzer the stack created, \
             got {status} with {answered}"
        );
        let listed: Vec<String> = list_analyzers(&router)
            .await
            .iter()
            .filter_map(|analyzer| Some(analyzer.get("arn")?.as_str()?.to_string()))
            .collect();
        assert!(
            listed.contains(&published),
            "case {case:?}: expected ListAnalyzers to report {published:?}, got {listed:?}"
        );

        // The tags CloudFormation gives a resource it created reached that service too, beside
        // the declaration's own.
        let held = list_tags(&router, &published).await;
        for (key, value) in row.tags {
            assert_eq!(
                held.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected the declared tag {key:?} on the analyzer, got {held:?}"
            );
        }
        assert_eq!(
            held.get(STACK_TAG_KEY).map(String::as_str),
            Some(row.stack_name),
            "case {case:?}: expected the stack's own tag to be propagated, got {held:?}"
        );
        assert_eq!(
            (
                held.get(STACK_NAME_TAG).map(String::as_str),
                held.get(LOGICAL_ID_TAG).map(String::as_str),
            ),
            (Some(row.stack_name), Some("Analyzer")),
            "case {case:?}: expected the stack-name and logical-id tags, got {held:?}"
        );
        assert!(
            held.get(STACK_ID_TAG)
                .is_some_and(|stack_id| stack_id.contains(":stack/")),
            "case {case:?}: expected the stack-id tag to carry the stack's id, got {held:?}"
        );

        // Every rule the template declared — inline on the analyzer or as a resource of its own
        // — is a rule that service holds, under the criteria map either spelling became.
        let reported = list_archive_rule_names(&router, &analyzer_name).await;
        let expected_names: Vec<&str> = row.rules.iter().map(|rule| rule.name).collect();
        assert_eq!(
            reported.len(),
            expected_names.len(),
            "case {case:?}: expected ListArchiveRules to report {expected_names:?}, got \
             {reported:?}"
        );
        for rule in &row.rules {
            assert!(
                reported.iter().any(|name| name == rule.name),
                "case {case:?}: expected ListArchiveRules to report {:?}, got {reported:?}",
                rule.name
            );
            let (status, answered) = get_archive_rule(&router, &analyzer_name, rule.name).await;
            assert_eq!(
                (
                    status,
                    answered
                        .pointer("/archiveRule/filter")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                (200, (rule.criteria)()),
                "case {case:?}: expected the declared criteria of {:?} to reach that service, \
                 got {status} with {answered}",
                rule.name
            );
            if let Some(key) = rule.arn_output {
                assert_eq!(
                    output(case, &described, key),
                    expected_rule_arn(&analyzer_name, rule.name),
                    "case {case:?}: expected the rule's published identity to nest inside the \
                     analyzer's ARN"
                );
            }
            // The two instants a rule publishes are that service's own clock, read off its
            // report of the rule rather than computed by the stack — published as the ISO-8601
            // UTC instant to the second.
            for (key, member_name) in [
                (rule.created_at_output, "createdAt"),
                (rule.updated_at_output, "updatedAt"),
            ] {
                let Some(key) = key else { continue };
                let reported_instant = answered
                    .pointer(&format!("/archiveRule/{member_name}"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !reported_instant.is_empty(),
                    "case {case:?}: expected that service to report the rule's {member_name}, got \
                     {answered}"
                );
                assert_eq!(
                    output(case, &described, key),
                    to_the_second(&reported_instant),
                    "case {case:?}: expected the published {key} to be the instant that service \
                     reported ({reported_instant:?}), to the second"
                );
            }
        }
    }
}

/// Each misspelling, and the type names the refusal lists for it: the RESTRICTIVE half of the
/// rule, which registering a published type must not weaken into a shape test.
///
/// The rows differ along WHERE the spelling goes wrong: a plural resource segment, a namespace
/// that is not the one the service publishes, a resource of the right namespace that does not
/// exist, and a template mixing a real type with an unreal one — the last of which also pins
/// that only the unrecognised name is listed.
const MISSPELLED: &[(&str, &str, &[&str])] = &[
    (
        "a plural resource segment",
        "cfn-aa-misspelled-plural",
        &["AWS::AccessAnalyzer::Analyzers"],
    ),
    (
        "a namespace the service does not publish",
        "cfn-aa-misspelled-namespace",
        &["AWS::IAMAccessAnalyzer::Analyzer"],
    ),
    (
        "a resource the namespace does not publish",
        "cfn-aa-misspelled-resource",
        &["AWS::AccessAnalyzer::Finding"],
    ),
    (
        "two unrecognised types beside a real one",
        "cfn-aa-misspelled-mixed",
        &["AWS::AccessAnalyzer::Analyzers", "AWS::AccessAnalyzer::Rule"],
    ),
];

#[tokio::test]
async fn a_type_cloudformation_does_not_know_is_still_refused() {
    for (case, stack_name, unrecognised) in MISSPELLED {
        let router = router();
        let mut resources = serde_json::Map::new();
        // The real type beside them, so the refusal is shown to list the unrecognised names
        // alone.
        resources.insert(
            "Analyzer".to_string(),
            json!({
                "Type": ANALYZER,
                "Properties": {"AnalyzerName": "sim2real_refused", "Type": ACCOUNT_TYPE}
            }),
        );
        for (index, spelled) in unrecognised.iter().enumerate() {
            resources.insert(
                format!("Unreal{index}"),
                json!({"Type": spelled, "Properties": {}}),
            );
        }
        let template =
            json!({"Resources": Value::Object(resources)}).to_string();

        let (status, body) = create_stack(&router, stack_name, &template, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a type CloudFormation does not know, got \
             {status} with {body}"
        );
        let expected = format!("{UNRECOGNIZED_PREFIX}{}]", unrecognised.join(", "));
        assert!(
            body.contains(&expected),
            "case {case:?}: expected the refusal to be {expected:?}, got {body}"
        );

        // Nothing was created: the stack does not exist and no analyzer was made for it.
        let (status, described) = describe_stack(&router, stack_name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refused template to create no stack, got {status} with \
             {described}"
        );
        let listed = list_analyzers(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no analyzer for a template that was refused, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_update_that_drops_the_rule_takes_it_away_and_leaves_the_analyzer() {
    // The issue's own update: the rule resource is removed from the template, which deletes the
    // rule in that service while the analyzer it hangs off keeps the identity and the instant it
    // was created with.
    let router = router();
    let case = "an update dropping the archive rule resource";
    let stack_name = "cfn-aa-dropped-rule";
    let analyzer_name = "sim2real_dropped";
    let rule_name = "ArchiveBuckets";
    let with_rule = json!({
        "Resources": {
            "Analyzer": {
                "Type": ANALYZER,
                "Properties": {"AnalyzerName": analyzer_name, "Type": ACCOUNT_TYPE}
            },
            "ArchiveRule": {
                "Type": ARCHIVE_RULE,
                "DependsOn": "Analyzer",
                "Properties": {
                    "AnalyzerName": analyzer_name,
                    "RuleName": rule_name,
                    "Filter": {"resourceType": {"Eq": ["AWS::S3::Bucket"]}},
                }
            }
        },
        "Outputs": {"AnalyzerRef": {"Value": {"Ref": "Analyzer"}}}
    })
    .to_string();
    let without_rule = json!({
        "Resources": {
            "Analyzer": {
                "Type": ANALYZER,
                "Properties": {"AnalyzerName": analyzer_name, "Type": ACCOUNT_TYPE}
            }
        },
        "Outputs": {"AnalyzerRef": {"Value": {"Ref": "Analyzer"}}}
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &with_rule).await;
    let published = output(case, &described, "AnalyzerRef");
    let (_, before) = get_analyzer(&router, analyzer_name).await;
    let created_at = before
        .pointer("/analyzer/createdAt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !created_at.is_empty(),
        "case {case:?}: expected that service to report when the analyzer was created, got \
         {before}"
    );

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &without_rule),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    // The rule is gone from that service, in its own words, while the analyzer is untouched.
    let (status, answered) = get_archive_rule(&router, analyzer_name, rule_name).await;
    assert_eq!(
        (
            status,
            answered.get("__type").and_then(Value::as_str),
            answered.get("message").and_then(Value::as_str),
        ),
        (
            NOT_FOUND_STATUS,
            Some(RESOURCE_NOT_FOUND),
            Some(ARCHIVE_RULE_NOT_FOUND),
        ),
        "case {case:?}: expected the dropped rule to be gone, got {status} with {answered}"
    );
    assert!(
        list_archive_rule_names(&router, analyzer_name)
            .await
            .is_empty(),
        "case {case:?}: expected the analyzer to list no archive rule after the update"
    );
    let (status, after) = get_analyzer(&router, analyzer_name).await;
    assert_eq!(
        (
            status,
            after.pointer("/analyzer/arn").and_then(Value::as_str),
            after.pointer("/analyzer/createdAt").and_then(Value::as_str),
        ),
        (200, Some(published.as_str()), Some(created_at.as_str())),
        "case {case:?}: expected the analyzer to keep its identity and its instant, got {status} \
         with {after}"
    );
}

#[tokio::test]
async fn a_changed_filter_is_applied_to_the_rule_the_stack_has() {
    // `UpdateArchiveRule` "updates the criteria and values for the specified archive rule", so a
    // deployment stating other criteria is applied to the rule the stack already has — under the
    // identity it was created with, and with the instant it was created at left alone.
    let router = router();
    let case = "an update restating the rule's criteria";
    let stack_name = "cfn-aa-changed-filter";
    let analyzer_name = "sim2real_changed";
    let rule_name = "ArchiveFindings";
    let template_of = |criteria: Value| {
        json!({
            "Resources": {
                "Analyzer": {
                    "Type": ANALYZER,
                    "Properties": {"AnalyzerName": analyzer_name, "Type": ACCOUNT_TYPE}
                },
                "ArchiveRule": {
                    "Type": ARCHIVE_RULE,
                    "DependsOn": "Analyzer",
                    "Properties": {
                        "AnalyzerName": analyzer_name,
                        "RuleName": rule_name,
                        "Filter": criteria,
                    }
                }
            },
            "Outputs": {
                "RuleRef": {"Value": {"Ref": "ArchiveRule"}},
                "RuleCreatedAt": {"Value": {"Fn::GetAtt": ["ArchiveRule", "CreatedAt"]}}
            }
        })
        .to_string()
    };

    let described = deployed(
        &router,
        case,
        stack_name,
        &template_of(json!({"resourceType": {"Eq": ["AWS::S3::Bucket"]}})),
    )
    .await;
    let published = output(case, &described, "RuleRef");
    let created_at = output(case, &described, "RuleCreatedAt");

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member(
                "TemplateBody",
                &template_of(json!({"resourceType": {"Neq": ["AWS::SQS::Queue"]}})),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    // The change reached the rule THAT service holds, and it is the one rule rather than a
    // second one.
    let (status, answered) = get_archive_rule(&router, analyzer_name, rule_name).await;
    assert_eq!(
        (
            status,
            answered
                .pointer("/archiveRule/filter")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (200, json!({"resourceType": {"neq": ["AWS::SQS::Queue"]}})),
        "case {case:?}: expected the re-declared criteria, got {status} with {answered}"
    );
    assert_eq!(
        list_archive_rule_names(&router, analyzer_name).await,
        vec![rule_name.to_string()],
        "case {case:?}: expected the one rule to be changed rather than a second one created"
    );
    let (_, described) = describe_stack(&router, stack_name).await;
    assert_eq!(
        output(case, &described, "RuleRef"),
        published,
        "case {case:?}: expected the rule to keep the identity it was created with"
    );
    assert_eq!(
        output(case, &described, "RuleCreatedAt"),
        created_at,
        "case {case:?}: expected the instant the rule was created at to be left alone by an \
         update of its criteria"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_analyzer_and_its_rules() {
    let router = router();
    let case = "an analyzer and its rule deleted with their stack";
    let stack_name = "cfn-aa-deleted";
    let analyzer_name = "sim2real_deleted";
    let rule_name = "ArchiveBuckets";
    let template = json!({
        "Resources": {
            "Analyzer": {
                "Type": ANALYZER,
                "Properties": {"AnalyzerName": analyzer_name, "Type": ACCOUNT_TYPE}
            },
            "ArchiveRule": {
                "Type": ARCHIVE_RULE,
                "DependsOn": "Analyzer",
                "Properties": {
                    "AnalyzerName": analyzer_name,
                    "RuleName": rule_name,
                    "Filter": {"resourceType": {"Eq": ["AWS::S3::Bucket"]}},
                }
            }
        },
        "Outputs": {"AnalyzerRef": {"Value": {"Ref": "Analyzer"}}}
    })
    .to_string();

    deployed(&router, case, stack_name, &template).await;
    let (status, answered) = get_archive_rule(&router, analyzer_name, rule_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the rule to exist before the delete, got {status} with {answered}"
    );

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    // Wait for the CONDITION the delete owes — the analyzer being gone from the owning service —
    // under the harness's own generous deadline, so a loaded host cannot decide the outcome.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _) = get_analyzer(&router, analyzer_name).await;
        if status == NOT_FOUND_STATUS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the analyzer the stack created to be deleted with it, that \
             service still reports it; the stack recorded {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }

    // The analyzer and the rule are gone from that service, each in its own words.
    let (status, answered) = get_analyzer(&router, analyzer_name).await;
    assert_eq!(
        (
            status,
            answered.get("__type").and_then(Value::as_str),
            answered.get("message").and_then(Value::as_str),
        ),
        (
            NOT_FOUND_STATUS,
            Some(RESOURCE_NOT_FOUND),
            Some(ANALYZER_NOT_FOUND),
        ),
        "case {case:?}: expected the analyzer to be gone with the stack, got {status} with \
         {answered}"
    );
    let (status, answered) = get_archive_rule(&router, analyzer_name, rule_name).await;
    assert_eq!(
        (status, answered.get("message").and_then(Value::as_str)),
        (NOT_FOUND_STATUS, Some(ANALYZER_NOT_FOUND)),
        "case {case:?}: expected the rule's analyzer to be gone, got {status} with {answered}"
    );
    assert!(
        list_analyzers(&router).await.is_empty(),
        "case {case:?}: expected IAM Access Analyzer to hold no analyzer after the delete"
    );
}

/// One refusal row: a declaration IAM Access Analyzer refuses, and the resource it fails on.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    analyzer_name: &'static str,
    /// The value the declaration states for the analyzer's kind, which is outside the set
    /// `Type` models.
    analyzer_type: &'static str,
}

/// The rows differ along the CLASS of value that is not one of the kinds `Type` publishes: a
/// lower-cased spelling of a real one, a kind that does not exist, and the empty value.
const REFUSED: &[Refused] = &[
    Refused {
        case: "a lower-cased spelling of a kind that exists",
        stack_name: "cfn-aa-invalid-lowercase",
        analyzer_name: "sim2real_invalid_lowercase",
        analyzer_type: "account",
    },
    Refused {
        case: "a kind no value of the set names",
        stack_name: "cfn-aa-invalid-kind",
        analyzer_name: "sim2real_invalid_kind",
        analyzer_type: "INVALID_TYPE",
    },
    Refused {
        case: "no kind at all",
        stack_name: "cfn-aa-invalid-empty",
        analyzer_name: "sim2real_invalid_empty",
        analyzer_type: "",
    },
];

#[tokio::test]
async fn a_declaration_access_analyzer_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Analyzer": {
                    "Type": ANALYZER,
                    "Properties": {
                        "AnalyzerName": row.analyzer_name,
                        "Type": row.analyzer_type,
                    }
                }
            },
            "Outputs": {"AnalyzerRef": {"Value": {"Ref": "Analyzer"}}}
        })
        .to_string();

        // What the OWNING service answers for exactly that create is what the failed resource
        // has to quote, so it is read from that service itself rather than written out here.
        let (status, refusal) = access_analyzer(
            &router,
            "PUT",
            "/analyzer",
            Some(&json!({
                "analyzerName": format!("{}_direct", row.analyzer_name),
                "type": row.analyzer_type,
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected IAM Access Analyzer to refuse the kind itself, got {status} \
             with {refusal}"
        );
        let sentence = refusal
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no message in {refusal}"))
            .to_string();

        // The template is ADMITTED — the type is one AWS publishes — and it is the owning
        // service that refuses the create, so the stack rolls back rather than being refused at
        // submit time.
        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             analyzer that service never recorded, got {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "AnalyzerRef"),
            None,
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, inside
        // the envelope a resource provider reports one in — and never a refusal of the template.
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert!(
            !events.contains(UNRECOGNIZED_PREFIX),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let expected_suffix = "HandlerErrorCode: InvalidRequest";
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(&sentence) && reason.contains(expected_suffix)),
            "case {case:?}: expected a failure reason carrying {sentence:?} and \
             {expected_suffix:?}, got {reasons:?}"
        );

        // And nothing was left behind: that service holds no analyzer at all.
        let listed = list_analyzers(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected IAM Access Analyzer to hold no analyzer, got {listed:?}"
        );
    }
}
