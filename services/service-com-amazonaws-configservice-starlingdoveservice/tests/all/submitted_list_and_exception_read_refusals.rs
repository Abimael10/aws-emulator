// SPDX-License-Identifier: CC0-1.0

//! Two refusals of this service, end to end over the wire, each pinned over a TABLE of
//! inputs rather than one recorded call:
//!
//! 1. a constraint clause stated at a LIST member quotes the list the request carried,
//!    printed element by element — `[<element>, <element>, …]`, one entry per submitted
//!    element, an element of a list of modelled structures printed as the object the
//!    request was read into (`<the service's model package>.<Shape>@<identity>`) — and
//!    never a summary of the list's size, which is text no service emits;
//! 2. a remediation exception hangs off the rule's remediation CONFIGURATION, so a read
//!    of the exceptions of a rule holding none is refused 400
//!    `InvalidParameterValueException` rather than answered with an empty list — the
//!    same precondition the write path applies, and whatever the read asks for.
//!
//! Every case drives the emulator the way a client does — an `awsJson1_1` `POST /`
//! through the same `MultiServiceRouter` the production router builds — reusing the
//! `Emulator` harness `config_setup_and_rule_loop` defines.

use serde_json::{json, Value};

use crate::config_setup_and_rule_loop::{managed_rule, with_recorder_and_channel, Emulator};

/// The class prefix Config prints one of its own request objects under, as the recorded
/// clause about an oversized batch spells it: this service's internal model package.
const OBJECT_PACKAGE: &str = "com.amazonaws.starling.dove";

/// The sentence a read of the remediation exceptions of a rule with no remediation
/// configuration is refused with, written out here rather than read from the service's
/// own constant so this suite PINS it instead of restating it.
const NO_REMEDIATION_CONFIGURATION: &str = "No Remediation Configuration found for exception";

/// Resource types a key names, from `smithy_query(query="shape", shape="ResourceType")`.
const EC2_VOLUME: &str = "AWS::EC2::Volume";
/// See [`EC2_VOLUME`].
const S3_BUCKET: &str = "AWS::S3::Bucket";

/// One row of the list-clause table: the operation, the member the list sits at as a
/// clause names it, the element class a clause prints, the declared maximum the submitted
/// list breaks, how many elements it carries, and the request carrying them.
struct ListCase {
    name: &'static str,
    operation: &'static str,
    member: &'static str,
    class: &'static str,
    maximum: usize,
    submitted: usize,
    body: Value,
}

/// `count` aggregate resource identifiers, each naming every member the model requires.
fn identifiers(count: usize) -> Value {
    let account = emulator_server::emulator_account_id().to_string();
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "SourceAccountId": account,
                    "SourceRegion": "us-east-1",
                    "ResourceType": EC2_VOLUME,
                    "ResourceId": format!("vol-{index:06}"),
                })
            })
            .collect(),
    )
}

/// `count` resource keys.
fn resource_keys(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "resourceType": S3_BUCKET, "resourceId": format!("b-{index:06}") }))
            .collect(),
    )
}

/// `count` tags.
fn tags(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "Key": format!("k{index}"), "Value": "v" }))
            .collect(),
    )
}

/// `count` remediation configurations, each naming the three members the model requires
/// (`smithy_query(query="shape", shape="RemediationConfiguration")`: ConfigRuleName,
/// TargetType and TargetId).
fn remediation_configurations(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "ConfigRuleName": format!("rule-{index}"),
                    "TargetType": "SSM_DOCUMENT",
                    "TargetId": "AWS-PublishSNSNotification",
                })
            })
            .collect(),
    )
}

#[tokio::test]
async fn a_length_clause_quotes_the_list_the_request_submitted() {
    // Rows differ along what the rule generalises over: which operation and member the
    // list sits at (and so which class its elements are printed as), the declared maximum
    // it breaks, and how many elements the request carried. Every maximum is the model's
    // own (`smithy_query(query="shape",
    // shape="ResourceIdentifiersList,ResourceKeys,TagList,RemediationConfigurations")`).
    let cases = [
        ListCase {
            name: "one past the aggregate batch maximum",
            operation: "BatchGetAggregateResourceConfig",
            member: "resourceIdentifiers",
            class: "AggregateResourceIdentifier",
            maximum: 100,
            submitted: 101,
            body: json!({
                "ConfigurationAggregatorName": "org-wide",
                "ResourceIdentifiers": identifiers(101),
            }),
        },
        ListCase {
            name: "far past the aggregate batch maximum",
            operation: "BatchGetAggregateResourceConfig",
            member: "resourceIdentifiers",
            class: "AggregateResourceIdentifier",
            maximum: 100,
            submitted: 137,
            body: json!({
                "ConfigurationAggregatorName": "org-wide",
                "ResourceIdentifiers": identifiers(137),
            }),
        },
        ListCase {
            name: "one past the resource-key batch maximum",
            operation: "BatchGetResourceConfig",
            member: "resourceKeys",
            class: "ResourceKey",
            maximum: 100,
            submitted: 101,
            body: json!({ "resourceKeys": resource_keys(101) }),
        },
        ListCase {
            name: "one past the tag maximum",
            operation: "TagResource",
            member: "tags",
            class: "Tag",
            maximum: 50,
            submitted: 51,
            body: json!({
                "ResourceArn": "arn:aws:config:us-east-1:000000000000:config-rule/rule-1",
                "Tags": tags(51),
            }),
        },
        ListCase {
            name: "one past the remediation-configuration maximum",
            operation: "PutRemediationConfigurations",
            member: "remediationConfigurations",
            class: "RemediationConfiguration",
            maximum: 25,
            submitted: 26,
            body: json!({ "RemediationConfigurations": remediation_configurations(26) }),
        },
    ];

    let emulator = Emulator::new();
    for case in &cases {
        let name = case.name;
        let (status, parsed) = emulator.call(case.operation, &case.body).await;
        let code = parsed["__type"]
            .as_str()
            .map(|wire| wire.rsplit('#').next().unwrap_or(wire))
            .unwrap_or("<none>");
        assert_eq!(
            (status, code),
            (400, "ValidationException"),
            "case {name:?}: a list past its declared maximum is refused, got {status} {parsed}"
        );
        let message = parsed["message"].as_str().unwrap_or_default();

        // The clause: the envelope, the quoted LIST, the member and the requirement.
        let opening = "1 validation error detected: Value '[";
        let closing = format!(
            "]' at '{}' failed to satisfy constraint: Member must have length less than or \
             equal to {}",
            case.member, case.maximum
        );
        let printed = message
            .strip_prefix(opening)
            .and_then(|rest| rest.strip_suffix(closing.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {name:?}: expected the clause {opening}…{closing}, got {message:?}"
                )
            });
        let elements: Vec<&str> = printed.split(", ").collect();
        assert_eq!(
            elements.len(),
            case.submitted,
            "case {name:?}: expected one printed entry per submitted element, got {message:?}"
        );
        let expected_class = format!("{OBJECT_PACKAGE}.{}@", case.class);
        for element in &elements {
            let identity = element.strip_prefix(expected_class.as_str()).unwrap_or_else(|| {
                panic!(
                    "case {name:?}: expected every element printed as \
                     {expected_class}<identity>, got {element:?}"
                )
            });
            assert!(
                !identity.is_empty() && identity.chars().all(|c| c.is_ascii_hexdigit()),
                "case {name:?}: expected the element's identity in hexadecimal, got {element:?}"
            );
        }
        // The size summary is text no service emits, so it appears nowhere.
        assert!(
            !message.contains(" items"),
            "case {name:?}: the clause states the list and not its size, got {message:?}"
        );
    }

    // The control: a list INSIDE its declared bound is not refused for its length, so the
    // rows above are refused for what they carry rather than for being lists.
    let (status, parsed) = emulator
        .call(
            "BatchGetResourceConfig",
            &json!({ "resourceKeys": resource_keys(100) }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a batch at the declared maximum is served, got {status} {parsed}"
    );
}

/// An emulator holding `rule`, and the remediation configuration the rule needs when
/// `remediated`.
async fn emulator_holding(rule: &str, remediated: bool) -> Emulator {
    let emulator = Emulator::new();
    with_recorder_and_channel(&emulator).await;
    emulator.ok("PutConfigRule", &managed_rule(rule)).await;
    if remediated {
        emulator
            .ok(
                "PutRemediationConfigurations",
                &json!({
                    "RemediationConfigurations": [{
                        "ConfigRuleName": rule,
                        "TargetType": "SSM_DOCUMENT",
                        "TargetId": "AWS-PublishSNSNotification",
                    }],
                }),
            )
            .await;
    }
    emulator
}

#[tokio::test]
async fn an_exception_read_of_a_rule_with_no_remediation_configuration_is_refused() {
    let held = "encrypted-volumes";

    // Rows differ along WHY the rule the read names holds no remediation configuration —
    // a rule the account holds but gave none, a name nothing created, a name differing
    // from the held one only in case, a name differing by surrounding space — and along
    // what the read asks for: every exception the rule holds, a page of them, or the
    // exceptions of named resources.
    let asks: [(&str, Value); 3] = [
        ("every exception the rule holds", json!({})),
        ("a page of them", json!({ "Limit": 1 })),
        (
            "the exceptions of named resources",
            json!({ "ResourceKeys": [{ "ResourceType": EC2_VOLUME, "ResourceId": "vol-0001" }] }),
        ),
    ];

    for (rule_case, rule) in [
        ("a held rule with no remediation configuration", held),
        ("a rule nothing created", "no-such-rule"),
        ("a held rule's name in another case", "Encrypted-Volumes"),
        ("a held rule's name with trailing space", "encrypted-volumes "),
    ] {
        let emulator = emulator_holding(held, false).await;
        for (ask_case, ask) in &asks {
            let case = format!("{rule_case}, asked for {ask_case}");
            let mut body = ask.clone();
            body["ConfigRuleName"] = json!(rule);
            let (status, parsed) = emulator
                .call("DescribeRemediationExceptions", &body)
                .await;
            let code = parsed["__type"]
                .as_str()
                .map(|wire| wire.rsplit('#').next().unwrap_or(wire))
                .unwrap_or("<none>");
            let message = parsed["message"].as_str().unwrap_or_default();
            assert_eq!(
                (status, code, message),
                (400, "InvalidParameterValueException", NO_REMEDIATION_CONFIGURATION),
                "case {case:?}: a rule with no remediation configuration has no exception \
                 list to read, got {status} {parsed}"
            );
        }
    }

    // The control: the same read of a rule that DOES hold a remediation configuration is
    // served, and answers the empty list the rule really holds — so the refusals above
    // are about the missing remediation configuration and not about the read.
    let emulator = emulator_holding(held, true).await;
    for (ask_case, ask) in &asks {
        let mut body = ask.clone();
        body["ConfigRuleName"] = json!(held);
        let answered = emulator.ok("DescribeRemediationExceptions", &body).await;
        assert_eq!(
            answered["RemediationExceptions"],
            json!([]),
            "case {ask_case:?}: a remediated rule holding no exception answers an empty \
             list, got {answered}"
        );
    }
}
