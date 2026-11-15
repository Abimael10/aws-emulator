// SPDX-License-Identifier: CC0-1.0

//! The Recycle Bin retention rule a stack declares, end to end: the rule exists in that
//! service itself, under the identity it minted, a changed declaration reaches it, deleting the
//! stack takes it away — and a declaration that service REFUSES fails the resource with its own
//! refusal and rolls the stack back.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it is
//! DEPLOYED by calling the service that owns the type — so the handle the stack publishes
//! (`Ref`) resolves in that service, the values the type publishes are the ones that service
//! reported, and what that service refuses is a FAILED RESOURCE rather than a template-format
//! error. `rbin_rule_resource_type.rs` pins the admission half of that rule; this suite pins
//! what the stack then really did.
//!
//! The rows of the deploy table differ along what the rule generalises over: which resource
//! type the rule retains (the three values `ResourceType` publishes), where the retention period
//! sits inside the bound that type's own documentation publishes (the minimum, a value between,
//! and each type's maximum), how the rule SELECTS what it covers (one resource-tag pair, two of
//! them, a Region-level rule with no selection at all, and one with exclusion tags), whether the
//! declaration carries a description and the type's own `Tags`, which published return value the
//! output reads (`Ref`, `Fn::GetAtt Arn`, `Identifier`, `LockState`), the template spelling
//! (JSON with long-form intrinsics, YAML with the short-form tags), and how many rules one stack
//! declares.
//!
//! The refusal rows differ along the CLASS of declaration Recycle Bin refuses: a retention period
//! longer than the resource type it retains supports, for two types whose published maxima
//! differ. Each is the owning service's own refusal, quoted on the resource that failed — never
//! the `Template format error: Unrecognized resource types` a type CloudFormation does not know
//! is refused with, which no row of either table may ever see.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the Recycle Bin calls over that service's own
//! `restJson1` wire — so what is asserted is what a client that deploys the template and then
//! calls Recycle Bin observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, REGION,
    SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value};

/// The resource type under test.
const RULE: &str = "AWS::Rbin::Rule";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of either table may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The status a retention rule reaches once it is retaining resources: "Only retention rules that
/// are in the `available` state retain resources" (`RuleStatus`).
const AVAILABLE: &str = "available";

/// The lock state this type publishes for a rule nobody ever locked, which is the `unlocked`
/// member of `LockState`'s own value set.
const UNLOCKED: &str = "unlocked";

/// The one unit a retention period is measured in: "Currently, only `DAYS` is supported"
/// (`RetentionPeriod$RetentionPeriodUnit`).
const DAYS: &str = "DAYS";

/// The error code Recycle Bin answers for a rule it does not hold, and the status that error
/// carries (the modelled 404 of `GetRule`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The three resource types a retention rule may retain (`ResourceType`), and the longest period
/// each supports: "EBS snapshots and EBS-backed AMIs: 1 - 365 days" and "EBS volumes: 1 - 7 days"
/// (`smithy_query(query="doc", shape="RetentionPeriod", member="RetentionPeriodValue",
/// model_path="services/service-com-amazonaws-rbin-amazonrecyclebin/smithy-model/\
/// com-amazonaws-rbin-amazonrecyclebin.json")`).
const EBS_SNAPSHOT: &str = "EBS_SNAPSHOT";
const EBS_VOLUME: &str = "EBS_VOLUME";
const EC2_IMAGE: &str = "EC2_IMAGE";
const MAX_SNAPSHOT_DAYS: i64 = 365;
const MAX_VOLUME_DAYS: i64 = 7;

/// One Recycle Bin call over the `restJson1` wire its model declares, on the method and URI that
/// operation's own `@http` binding names, signed for `rbin` — that model's own `sigv4Name` — in
/// the Region the stack's resources live in.
async fn rbin(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://rbin.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("rbin.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rbin/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Recycle Bin request");
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

/// That service's own read of one rule, by the identifier it minted.
async fn get_rule(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    rbin(router, "GET", &format!("/rules/{identifier}"), None).await
}

/// Every rule that service lists for one resource type, narrowed by the scoping resource tags
/// where the row has any.
async fn list_rules(
    router: &MultiServiceRouter,
    resource_type: &str,
    resource_tags: &[(&str, &str)],
) -> Vec<Value> {
    let mut request = json!({ "ResourceType": resource_type });
    if !resource_tags.is_empty() {
        request["ResourceTags"] = resource_tags_document(resource_tags);
    }
    let (_, answered) = rbin(router, "POST", "/list-rules", Some(&request)).await;
    answered
        .get("Rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one rule, by the rule's own ARN — the `@httpLabel` of
/// `GET /tags/{ResourceArn}`, escaped as a single path segment the way an SDK sends a non-greedy
/// label.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, answered) = rbin(router, "GET", &format!("/tags/{}", form_encode(arn)), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListTagsForResource for {arn}, got {status} with {answered}"
    );
    answered
        .get("Tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("Key").and_then(Value::as_str)?.to_string(),
                        tag.get("Value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One resource-tag selection as the `ResourceTags` list both the declaration and that service's
/// own calls carry it as.
fn resource_tags_document(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({ "ResourceTagKey": key, "ResourceTagValue": value }))
            .collect(),
    )
}

/// Deploy `template` as `stack_name` under the stack's own tag, and answer the `DescribeStacks`
/// body of the COMPLETED stack, quoting the stack's own failure reasons when the create did not
/// complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let body = submitted(router, case, stack_name, template).await;
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
    let _ = body;
    described
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
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an {RULE} is a type a template may \
         declare — got {status} with body {body}"
    );
    body
}

/// Whether `arn` has the form a `RuleArn` publishes
/// (`^arn:\S+:rbin:[a-z\-0-9]{0,63}:[0-9]{12}:rule/[0-9a-zA-Z]{11}…$`).
fn is_a_rule_arn(arn: &str) -> bool {
    let Some((prefix, identifier)) = arn.rsplit_once(":rule/") else {
        return false;
    };
    prefix.starts_with("arn:")
        && prefix.contains(&format!(":rbin:{REGION}:"))
        && identifier.len() == 11
        && identifier.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The identifier Recycle Bin minted, as the ARN it built around it carries it.
fn identifier_in(arn: &str) -> String {
    arn.rsplit_once('/')
        .map(|(_, identifier)| identifier.to_string())
        .unwrap_or_default()
}

/// Read one rule until that service reports it `available`, or answer the last read once the
/// deadline passes: the status is one the CLOCK makes due, so what is waited for is the
/// CONDITION rather than a span of wall clock.
async fn available_rule(router: &MultiServiceRouter, case: &str, identifier: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, rule) = get_rule(router, identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Recycle Bin to report the rule the stack created under \
             {identifier:?}, got {status} with {rule}"
        );
        let reported = rule
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reported == AVAILABLE || std::time::Instant::now() >= deadline {
            return rule;
        }
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// What one row expects of one retention rule the stack declares.
struct Declared {
    /// The output key whose value is what `Ref` returned — the rule's ARN.
    ref_output: &'static str,
    /// The output keys reading the three attributes the type publishes, where the row reads them.
    arn_output: Option<&'static str>,
    identifier_output: Option<&'static str>,
    lock_state_output: Option<&'static str>,
    /// The resource type the rule retains, and the period it retains it for.
    resource_type: &'static str,
    retention_days: i64,
    /// The resource tags the rule selects on, which is also the filter the listing is narrowed
    /// by; empty for a Region-level rule.
    resource_tags: &'static [(&'static str, &'static str)],
    /// The description the declaration states, where it states one.
    description: Option<&'static str>,
    /// The type's own `Tags` the declaration states, which the rule must carry beside the stack's
    /// own propagated tag.
    tags: &'static [(&'static str, &'static str)],
}

/// One row: a template and the rules it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: one tag-level snapshot rule at the published MINIMUM period,
        // with every return value read back, in JSON with long-form intrinsics.
        Row {
            case: "a tag-level snapshot rule at the minimum period, every return value read back",
            stack_name: "rbin-rule-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Rule": {
                        "Type": RULE,
                        "Properties": {
                            "ResourceType": EBS_SNAPSHOT,
                            "RetentionPeriod": {
                                "RetentionPeriodValue": 1,
                                "RetentionPeriodUnit": DAYS,
                            },
                            "ResourceTags": [
                                {"ResourceTagKey": "retain", "ResourceTagValue": "round-trip"}
                            ],
                            "Tags": [{"Key": "sim2real-rbin", "Value": "round-trip"}],
                        }
                    }
                },
                "Outputs": {
                    "RuleRef": {"Value": {"Ref": "Rule"}},
                    "RuleArn": {"Value": {"Fn::GetAtt": ["Rule", "Arn"]}},
                    "RuleIdentifier": {"Value": {"Fn::GetAtt": ["Rule", "Identifier"]}},
                    "RuleLockState": {"Value": {"Fn::GetAtt": ["Rule", "LockState"]}}
                }
            })
            .to_string(),
            declared: vec![Declared {
                ref_output: "RuleRef",
                arn_output: Some("RuleArn"),
                identifier_output: Some("RuleIdentifier"),
                lock_state_output: Some("RuleLockState"),
                resource_type: EBS_SNAPSHOT,
                retention_days: 1,
                resource_tags: &[("retain", "round-trip")],
                description: None,
                tags: &[("sim2real-rbin", "round-trip")],
            }],
        },
        // A REGION-level rule — no resource selection at all — on the second type, at the MAXIMUM
        // period that type supports, carrying a description, in YAML with short-form intrinsics.
        Row {
            case: "a Region-level image rule at the maximum period, with a description, YAML",
            stack_name: "rbin-rule-region-level",
            template: format!(
                "\
Resources:
  Rule:
    Type: {RULE}
    Properties:
      ResourceType: {EC2_IMAGE}
      RetentionPeriod:
        RetentionPeriodValue: {MAX_SNAPSHOT_DAYS}
        RetentionPeriodUnit: {DAYS}
      Description: the images this Region keeps
Outputs:
  RuleRef:
    Value: !Ref Rule
  RuleArn:
    Value: !GetAtt Rule.Arn
  RuleLockState:
    Value: !GetAtt Rule.LockState
"
            ),
            declared: vec![Declared {
                ref_output: "RuleRef",
                arn_output: Some("RuleArn"),
                identifier_output: None,
                lock_state_output: Some("RuleLockState"),
                resource_type: EC2_IMAGE,
                retention_days: MAX_SNAPSHOT_DAYS,
                resource_tags: &[],
                description: Some("the images this Region keeps"),
                tags: &[],
            }],
        },
        // The third type, at ITS own maximum period — a different bound from the one above —
        // selected by TWO resource-tag pairs, with the identifier read back.
        Row {
            case: "a volume rule at its own type's maximum period, two scoping pairs",
            stack_name: "rbin-rule-volume",
            template: json!({
                "Resources": {
                    "Rule": {
                        "Type": RULE,
                        "Properties": {
                            "ResourceType": EBS_VOLUME,
                            "RetentionPeriod": {
                                "RetentionPeriodValue": MAX_VOLUME_DAYS,
                                "RetentionPeriodUnit": DAYS,
                            },
                            "ResourceTags": [
                                {"ResourceTagKey": "retain", "ResourceTagValue": "volumes"},
                                {"ResourceTagKey": "tier", "ResourceTagValue": "gold"}
                            ],
                        }
                    }
                },
                "Outputs": {
                    "RuleRef": {"Value": {"Ref": "Rule"}},
                    "RuleIdentifier": {"Value": {"Fn::GetAtt": ["Rule", "Identifier"]}}
                }
            })
            .to_string(),
            declared: vec![Declared {
                ref_output: "RuleRef",
                arn_output: None,
                identifier_output: Some("RuleIdentifier"),
                lock_state_output: None,
                resource_type: EBS_VOLUME,
                retention_days: MAX_VOLUME_DAYS,
                resource_tags: &[("retain", "volumes"), ("tier", "gold")],
                description: None,
                tags: &[],
            }],
        },
        // TWO rules in one stack, with periods between the published bounds: a tag-level one and
        // a Region-level one carrying EXCLUSION tags. Each publishes its OWN identity.
        Row {
            case: "two rules in one stack, one of them with exclusion tags",
            stack_name: "rbin-rule-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": RULE,
                        "Properties": {
                            "ResourceType": EBS_SNAPSHOT,
                            "RetentionPeriod": {
                                "RetentionPeriodValue": 30,
                                "RetentionPeriodUnit": DAYS,
                            },
                            "ResourceTags": [
                                {"ResourceTagKey": "retain", "ResourceTagValue": "pair-first"}
                            ],
                            "Tags": [{"Key": "half", "Value": "first"}],
                        }
                    },
                    "Second": {
                        "Type": RULE,
                        "Properties": {
                            "ResourceType": EBS_VOLUME,
                            "RetentionPeriod": {
                                "RetentionPeriodValue": 3,
                                "RetentionPeriodUnit": DAYS,
                            },
                            "ExcludeResourceTags": [
                                {"ResourceTagKey": "skip", "ResourceTagValue": "yes"}
                            ],
                        }
                    }
                },
                "Outputs": {
                    "FirstRef": {"Value": {"Ref": "First"}},
                    "FirstIdentifier": {"Value": {"Fn::GetAtt": ["First", "Identifier"]}},
                    "SecondRef": {"Value": {"Ref": "Second"}},
                    "SecondArn": {"Value": {"Fn::GetAtt": ["Second", "Arn"]}}
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    ref_output: "FirstRef",
                    arn_output: None,
                    identifier_output: Some("FirstIdentifier"),
                    lock_state_output: None,
                    resource_type: EBS_SNAPSHOT,
                    retention_days: 30,
                    resource_tags: &[("retain", "pair-first")],
                    description: None,
                    tags: &[("half", "first")],
                },
                Declared {
                    ref_output: "SecondRef",
                    arn_output: Some("SecondArn"),
                    identifier_output: None,
                    lock_state_output: None,
                    resource_type: EBS_VOLUME,
                    retention_days: 3,
                    resource_tags: &[],
                    description: None,
                    tags: &[],
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_retention_rule_recycle_bin_minted() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        for declared in &row.declared {
            let published = output_value(&described, declared.ref_output).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no {} output in {described}",
                    declared.ref_output
                )
            });
            assert!(
                is_a_rule_arn(&published),
                "case {case:?}: expected Ref to publish an ARN of the modelled rule form, got \
                 {published:?}"
            );
            let identifier = identifier_in(&published);

            // THE RULE: the rule the stack claims to have created is a rule Recycle Bin holds,
            // under the identity the stack published, configured as the template declared.
            let rule = available_rule(&router, case, &identifier).await;
            assert_eq!(
                (
                    rule.get("Status").and_then(Value::as_str),
                    rule.get("RuleArn").and_then(Value::as_str),
                    rule.get("Identifier").and_then(Value::as_str),
                ),
                (
                    Some(AVAILABLE),
                    Some(published.as_str()),
                    Some(identifier.as_str()),
                ),
                "case {case:?}: expected an available rule under the published identity, got \
                 {rule}"
            );
            assert_eq!(
                rule.get("ResourceType").and_then(Value::as_str),
                Some(declared.resource_type),
                "case {case:?}: expected the declared resource type, got {rule}"
            );
            assert_eq!(
                (
                    rule.pointer("/RetentionPeriod/RetentionPeriodValue")
                        .and_then(Value::as_i64),
                    rule.pointer("/RetentionPeriod/RetentionPeriodUnit")
                        .and_then(Value::as_str),
                ),
                (Some(declared.retention_days), Some(DAYS)),
                "case {case:?}: expected the declared retention period, got {rule}"
            );
            assert_eq!(
                rule.get("ResourceTags"),
                Some(&resource_tags_document(declared.resource_tags)),
                "case {case:?}: expected the declared resource tags, got {rule}"
            );
            assert_eq!(
                rule.get("Description").and_then(Value::as_str),
                declared.description,
                "case {case:?}: expected the declared description, got {rule}"
            );
            // A rule nobody ever locked is reported with NO lock state at all — "null - The
            // retention rule has never been locked".
            assert!(
                rule.get("LockState").is_none(),
                "case {case:?}: expected no LockState for a rule that was never locked, got \
                 {rule}"
            );

            // ...and the listing that service serves for the rule's own scope resolves the same
            // identity.
            let listed = list_rules(&router, declared.resource_type, declared.resource_tags).await;
            let summary = listed
                .iter()
                .find(|summary| {
                    summary.get("Identifier").and_then(Value::as_str) == Some(&identifier)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected Recycle Bin to list the rule {identifier:?} for \
                         {}, got {listed:?}",
                        declared.resource_type
                    )
                });
            assert_eq!(
                summary.get("RuleArn").and_then(Value::as_str),
                Some(published.as_str()),
                "case {case:?}: expected the listed rule to carry the published ARN, got {summary}"
            );
            assert!(
                summary.get("LockState").is_none(),
                "case {case:?}: expected the listing to omit LockState for a rule that was never \
                 locked, got {summary}"
            );

            // The values the type publishes are that service's own: the ARN it minted, the
            // identifier inside it, and the lock state it keeps — which this type publishes as
            // `unlocked` for a rule that was never locked.
            for (key, expected) in [
                (declared.arn_output, published.clone()),
                (declared.identifier_output, identifier.clone()),
                (declared.lock_state_output, UNLOCKED.to_string()),
            ] {
                let Some(key) = key else { continue };
                let read = output_value(&described, key)
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"));
                assert_eq!(
                    read, expected,
                    "case {case:?}: expected the published {key} to be {expected:?}, got {read:?}"
                );
            }

            // The tags the rule carries are the declaration's own beside the stack's, and none of
            // the records CloudFormation keeps in the namespace AWS reserves for itself.
            let held = list_tags(&router, &published).await;
            let mut expected: BTreeMap<String, String> = declared
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            expected.insert(STACK_TAG_KEY.to_string(), row.stack_name.to_string());
            assert_eq!(
                held, expected,
                "case {case:?}: expected the declared tags beside the stack's own, got {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_declared_retention_period_reaches_the_rule_in_either_spelling() {
    // A template states a value either in JSON's own spelling or as the TEXT a parameter
    // reference resolves to — `Ref` on a parameter answers that parameter's value, which a stack
    // operation carries as a string whatever the parameter's declared type — so both spellings
    // have to reach the owning service as the kind `RetentionPeriodValue` is carried in.
    // (case, the stack, the days the parameter states, the period the rule must report)
    let rows: [(&str, &str, &str, i64); 3] = [
        (
            "a period between the published bounds, as parameter text",
            "rbin-rule-typed-middle",
            "90",
            90,
        ),
        (
            "the published minimum, as parameter text",
            "rbin-rule-typed-minimum",
            "1",
            1,
        ),
        (
            "the published maximum for the declared type, as parameter text",
            "rbin-rule-typed-maximum",
            "365",
            MAX_SNAPSHOT_DAYS,
        ),
    ];

    for (case, stack_name, days, expected) in rows {
        let router = router();
        let template = json!({
            "Parameters": {"RetentionDays": {"Type": "Number"}},
            "Resources": {
                "Rule": {
                    "Type": RULE,
                    "Properties": {
                        "ResourceType": EBS_SNAPSHOT,
                        "RetentionPeriod": {
                            "RetentionPeriodValue": {"Ref": "RetentionDays"},
                            "RetentionPeriodUnit": DAYS,
                        },
                        "ResourceTags": [
                            {"ResourceTagKey": "retain", "ResourceTagValue": "parameterised"}
                        ],
                    }
                }
            },
            "Outputs": {"RuleRef": {"Value": {"Ref": "Rule"}}}
        })
        .to_string();

        let (status, created) = create_stack(
            &router,
            stack_name,
            &template,
            &[
                member("Parameters.member.1.ParameterKey", "RetentionDays"),
                member("Parameters.member.1.ParameterValue", days),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with failure reasons \
             {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );
        let (_, described) = describe_stack(&router, stack_name).await;
        let published = output_value(&described, "RuleRef")
            .unwrap_or_else(|| panic!("case {case:?}: no RuleRef output in {described}"));
        let (status, rule) = get_rule(&router, &identifier_in(&published)).await;
        assert_eq!(
            (
                status,
                rule.pointer("/RetentionPeriod/RetentionPeriodValue")
                    .and_then(Value::as_i64)
            ),
            (200, Some(expected)),
            "case {case:?}: expected the declared period to reach the rule as a number, got \
             {status} with {rule}"
        );
    }
}

#[tokio::test]
async fn a_changed_declaration_is_applied_to_the_rule_the_stack_has() {
    // `UpdateRule` "can update a retention rule's description, resource tags, and retention
    // period", so a deployment that states another period is applied to the rule the stack
    // already has — under the identity that service minted — rather than being dropped or
    // replaced.
    let router = router();
    let case = "an update that widens the retention period and adds a description";
    let stack_name = "rbin-rule-updated";
    let template_of = |days: i64, description: &str| {
        json!({
            "Resources": {
                "Rule": {
                    "Type": RULE,
                    "Properties": {
                        "ResourceType": EBS_SNAPSHOT,
                        "RetentionPeriod": {
                            "RetentionPeriodValue": days,
                            "RetentionPeriodUnit": DAYS,
                        },
                        "ResourceTags": [
                            {"ResourceTagKey": "retain", "ResourceTagValue": "updated"}
                        ],
                        "Description": description,
                    }
                }
            },
            "Outputs": {"RuleRef": {"Value": {"Ref": "Rule"}}}
        })
        .to_string()
    };

    let described = deployed(&router, case, stack_name, &template_of(7, "before")).await;
    let published = output_value(&described, "RuleRef")
        .unwrap_or_else(|| panic!("case {case:?}: no RuleRef output in {described}"));
    let identifier = identifier_in(&published);
    available_rule(&router, case, &identifier).await;

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of(MAX_SNAPSHOT_DAYS, "after")),
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
    let (_, described) = describe_stack(&router, stack_name).await;
    assert_eq!(
        output_value(&described, "RuleRef"),
        Some(published.clone()),
        "case {case:?}: expected the rule to keep the identity Recycle Bin minted, got {described}"
    );

    // The change reached the rule THAT service holds, not just the stack's record — and it is the
    // one rule rather than a second one.
    let (status, rule) = get_rule(&router, &identifier).await;
    assert_eq!(
        (
            status,
            rule.pointer("/RetentionPeriod/RetentionPeriodValue")
                .and_then(Value::as_i64),
            rule.get("Description").and_then(Value::as_str),
        ),
        (200, Some(MAX_SNAPSHOT_DAYS), Some("after")),
        "case {case:?}: expected the re-declared period and description, got {status} with {rule}"
    );
    let listed = list_rules(&router, EBS_SNAPSHOT, &[("retain", "updated")]).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected the one rule to be changed rather than a second one created, got \
         {listed:?}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_retention_rules_it_created() {
    let router = router();
    let case = "two rules deleted with their stack";
    let stack_name = "rbin-rule-deleted";
    let template = json!({
        "Resources": {
            "First": {
                "Type": RULE,
                "Properties": {
                    "ResourceType": EBS_SNAPSHOT,
                    "RetentionPeriod": {"RetentionPeriodValue": 10, "RetentionPeriodUnit": DAYS},
                    "ResourceTags": [
                        {"ResourceTagKey": "retain", "ResourceTagValue": "deleted-first"}
                    ],
                }
            },
            "Second": {
                "Type": RULE,
                "Properties": {
                    "ResourceType": EBS_VOLUME,
                    "RetentionPeriod": {"RetentionPeriodValue": 2, "RetentionPeriodUnit": DAYS},
                    "ResourceTags": [
                        {"ResourceTagKey": "retain", "ResourceTagValue": "deleted-second"}
                    ],
                }
            }
        },
        "Outputs": {
            "FirstRef": {"Value": {"Ref": "First"}},
            "SecondRef": {"Value": {"Ref": "Second"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let identifiers: Vec<String> = ["FirstRef", "SecondRef"]
        .iter()
        .map(|key| {
            identifier_in(
                &output_value(&described, key)
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}")),
            )
        })
        .collect();
    for identifier in &identifiers {
        let (status, rule) = get_rule(&router, identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the rule {identifier:?} to exist before the delete, got \
             {status} with {rule}"
        );
    }

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    settle_the_deletion(&router, case, &identifiers).await;

    // The rules the stack created are gone from the owning service, which is what makes a stack's
    // resources the stack's own rather than leftovers — and the read of one says so in that
    // service's own words.
    for identifier in &identifiers {
        let (status, answered) = get_rule(&router, identifier).await;
        let expected_message = format!("Recycle Bin Rule '{identifier}' does not exist.");
        assert_eq!(
            (
                status,
                answered.get("__type").and_then(Value::as_str),
                answered.get("Message").and_then(Value::as_str),
            ),
            (
                NOT_FOUND_STATUS,
                Some(RESOURCE_NOT_FOUND),
                Some(expected_message.as_str()),
            ),
            "case {case:?}: expected the rule {identifier:?} to be gone with the stack, got \
             {status} with {answered}"
        );
    }
    for (resource_type, pair) in [
        (EBS_SNAPSHOT, ("retain", "deleted-first")),
        (EBS_VOLUME, ("retain", "deleted-second")),
    ] {
        let listed = list_rules(&router, resource_type, &[pair]).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no {resource_type} rule left for {pair:?}, got {listed:?}"
        );
    }
}

/// Wait until every rule of `identifiers` is gone from the owning service.
///
/// Waits for the CONDITION rather than for a span, under the harness's own generous deadline, so
/// a loaded host cannot decide the outcome.
async fn settle_the_deletion(router: &MultiServiceRouter, case: &str, identifiers: &[String]) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut still_held: Vec<&String> = Vec::new();
        for identifier in identifiers {
            let (status, _) = get_rule(router, identifier).await;
            if status == 200 {
                still_held.push(identifier);
            }
        }
        if still_held.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected every rule the stack created to be released, these are still \
             held: {still_held:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// One refusal row: a declaration Recycle Bin refuses, and what the failed resource must report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The resource type the rule would retain, and the period that type does NOT support — one
    /// day past the maximum its own documentation publishes.
    resource_type: &'static str,
    retention_days: i64,
    /// The scoping pair the rolled-back rule would have been listed under.
    scope: (&'static str, &'static str),
}

const REFUSED: &[Refused] = &[
    // The issue's own refusal: 366 days is inside the modelled `@range` (1..3650) and past the
    // 365 days an EBS snapshot is retained for, so it is the owning service that refuses it
    // rather than CloudFormation's own early validation.
    Refused {
        case: "a snapshot rule one day past the maximum that type supports",
        stack_name: "rbin-rule-over-one-year",
        resource_type: EBS_SNAPSHOT,
        retention_days: MAX_SNAPSHOT_DAYS + 1,
        scope: ("retain", "over-one-year"),
    },
    // The same class against a type whose published maximum is a different number, which is what
    // makes the row about the rule rather than about one bound.
    Refused {
        case: "a volume rule one day past the maximum that type supports",
        stack_name: "rbin-rule-over-seven-days",
        resource_type: EBS_VOLUME,
        retention_days: MAX_VOLUME_DAYS + 1,
        scope: ("retain", "over-seven-days"),
    },
];

#[tokio::test]
async fn a_declaration_recycle_bin_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Rule": {
                    "Type": RULE,
                    "Properties": {
                        "ResourceType": row.resource_type,
                        "RetentionPeriod": {
                            "RetentionPeriodValue": row.retention_days,
                            "RetentionPeriodUnit": DAYS,
                        },
                        "ResourceTags": [
                            {"ResourceTagKey": row.scope.0, "ResourceTagValue": row.scope.1}
                        ],
                    }
                }
            },
            "Outputs": {"RuleRef": {"Value": {"Ref": "Rule"}}}
        })
        .to_string();

        // What the OWNING service answers for exactly that create is what the failed resource has
        // to quote, so it is read from that service itself rather than written out here.
        let (status, refusal) = rbin(
            &router,
            "POST",
            "/rules",
            Some(&json!({
                "ResourceType": row.resource_type,
                "RetentionPeriod": {
                    "RetentionPeriodValue": row.retention_days,
                    "RetentionPeriodUnit": DAYS,
                },
                "ResourceTags": [
                    {"ResourceTagKey": row.scope.0, "ResourceTagValue": "direct"}
                ],
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected Recycle Bin to refuse the period itself, got {status} with \
             {refusal}"
        );
        let sentence = refusal
            .get("Message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no Message in {refusal}"))
            .to_string();

        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             identity Recycle Bin never minted, got {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "RuleRef"),
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
            !events.contains(UNRECOGNIZED_TYPES) && !events.contains("VALIDATION_ERROR"),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let expected_envelope = "(Service: Rbin, Status Code: 400,";
        let expected_suffix = "HandlerErrorCode: InvalidRequest";
        assert!(
            reasons.iter().any(|reason| reason.contains(&sentence)
                && reason.contains(expected_envelope)
                && reason.contains("SDK Attempt Count:")
                && reason.contains(expected_suffix)),
            "case {case:?}: expected a failure reason carrying {sentence:?}, \
             {expected_envelope:?} and {expected_suffix:?}, got {reasons:?}"
        );

        // And nothing was left behind: Recycle Bin holds no rule for the declared scope.
        let listed = list_rules(&router, row.resource_type, &[row.scope]).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected Recycle Bin to hold no rule for {:?}, got {listed:?}",
            row.scope
        );
    }
}
