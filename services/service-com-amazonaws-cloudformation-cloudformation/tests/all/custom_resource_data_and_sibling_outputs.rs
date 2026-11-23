// SPDX-License-Identifier: CC0-1.0

//! What a stack that reached `CREATE_COMPLETE` publishes for every output its template
//! declares.
//!
//! The rule under test: **every `Output` a stack reports carries its `OutputValue` beside its
//! `OutputKey` — the value the output's `Value` expression resolves to against the stack's own
//! state, including an expression reading a resource identity the owning service assigned and
//! one reading the response `Data` a custom resource's provider answered with.** A key
//! reported without a value is what the standard `{o["OutputKey"]: o["OutputValue"]}` read of a
//! settled stack's outputs raises on, so an output's value is part of the contract a template
//! publishes rather than a best effort. Every output these rows declare names something the
//! stack HAS; an output whose expression resolves to nothing at all (`Ref` of `AWS::NoValue`,
//! an `Fn::Select` past the end of its list) is reported with its key and no value, which
//! `stack_outputs.rs` pins.
//!
//! Every row drives the emulator through its own wire: the stack operations over
//! CloudFormation's awsQuery endpoint, the provider function created in AWS Lambda by the stack
//! itself and run by that service, the answer sent by that function's own code, and the topic
//! read back through Amazon SNS's own `GetTopicAttributes` — so a published ARN is one a client
//! can really call.
//!
//! The rows differ along what the rule generalises over: the KIND of expression an output
//! carries (a `Ref` to a resource whose identity Amazon SNS assigns, a `Ref` to one whose
//! identity the provider minted, `Fn::GetAtt` on a published attribute, `Fn::GetAtt` on a
//! provider's response `Data`, and an `Fn::Join` composing two of them), how many `Data`
//! entries the provider answers with and what text they carry, whether the topic's name is
//! declared or generated, whether the output also declares a `Description` and an
//! `ExportName`, and whether the stack's parameter value comes from the request or from the
//! template's own default.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    create_stack, member, router_with_engine, settled_stack, xml_text, REGION,
};
use crate::stack_lifecycle::{sns, ACCOUNT_ID};
use crate::stack_outputs::{wire_outputs, WireOutput};

/// The provider the stacks of this suite declare: it answers the request type it was called
/// with, the marker its own properties carry (which is where the stack's PARAMETER reaches it)
/// and whatever further `Data` entries those properties ask for, under the physical resource id
/// they name — so what a stack publishes for it could only have come from a run of this code.
/// It answers through the response module CloudFormation supplies to inline code.
const PROVIDER: &str = r#"
import json

import cfnresponse


def handler(event, context):
    properties = event["ResourceProperties"]
    data = {
        "RequestType": event["RequestType"],
        "StatedMarker": properties.get("Marker", ""),
    }
    data.update(json.loads(properties.get("AnswerData", "{}")))
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        data,
        physicalResourceId=properties.get("AnswerId") or None,
    )
"#;

/// What one output's value must be.
enum Expect {
    /// Exactly this text.
    Exact(&'static str),
    /// The ARN Amazon SNS keys the row's topic by: `arn:aws:sns:<region>:<account>:<name>`,
    /// whose last element is the declared name where the template declares one. Asserted to be
    /// an ARN that service really answers for, so the published value is one a client can call.
    TopicArn { named: Option<&'static str> },
    /// This text with another output's value substituted for `{}` — an output composing two
    /// others publishes what those two resolved to.
    Composed {
        around: &'static str,
        from: &'static str,
    },
}

/// One declared output of a row: its key, the `Value` expression it declares, what that must
/// resolve to, and the export name it publishes under where it declares one.
struct Out {
    key: &'static str,
    value: Value,
    expect: Expect,
    exported: Option<&'static str>,
}

/// One row: the stack it deploys and the outputs it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The `TopicName` the sibling topic is declared with, or `None` to let CloudFormation
    /// name it.
    topic_name: Option<&'static str>,
    /// The `Data` entries the provider is asked to answer with, as a JSON object.
    answer_data: String,
    /// The physical resource id the provider answers with, or `None` to let it pick.
    answer_id: Option<&'static str>,
    /// The `Marker` parameter value the REQUEST states, or `None` to leave the template's own
    /// declared default in force — the marker the provider is called with either way.
    stated_marker: Option<&'static str>,
    outputs: Vec<Out>,
}

/// The `Data` entry the provider answers with for the rows that read one back, carrying text a
/// form body and an XML envelope both have to deliver unharmed: spaces, punctuation and a
/// multi-byte character.
const ECHOED: &str = "a value, with spaces & a ✓";

/// The name the row that DECLARES its topic's name uses, so `Fn::GetAtt` on the topic's name
/// and the last element of its ARN are both pinned to a value the template chose.
const DECLARED_TOPIC_NAME: &str = "cfn-published-outputs-topic";

/// The `Marker` value the template DECLARES as its parameter's default, which is what reaches
/// the provider for a row whose request states none.
const DEFAULT_MARKER: &str = "the-template-default";

/// The `Description` the template declares beside the value of an output that also publishes an
/// export, so what the response reports for that member is pinned to the declared text.
const DECLARED_DESCRIPTION: &str = "what this stack publishes for its readers";

fn rows() -> Vec<Row> {
    vec![
        // The shape a client reads a custom resource's response data and a sibling resource's
        // identity in: one output per declared contract, both resolved.
        Row {
            case: "a custom resource's response Data and a sibling topic's Ref",
            stack_name: "published-outputs-data-and-ref",
            topic_name: None,
            answer_data: "{}".to_string(),
            answer_id: None,
            stated_marker: Some("stated-by-the-request"),
            outputs: vec![
                Out {
                    key: "RequestType",
                    value: json!({"Fn::GetAtt": ["MyResource", "RequestType"]}),
                    expect: Expect::Exact("Create"),
                    exported: None,
                },
                Out {
                    key: "StatedMarker",
                    value: json!({"Fn::GetAtt": ["MyResource", "StatedMarker"]}),
                    expect: Expect::Exact("stated-by-the-request"),
                    exported: None,
                },
                Out {
                    key: "TopicArn",
                    value: json!({"Ref": "AlwaysTopic"}),
                    expect: Expect::TopicArn { named: None },
                    exported: None,
                },
            ],
        },
        // A published ATTRIBUTE of a declared-by-name resource, a `Data` entry carrying text
        // the wire has to deliver unharmed, an output COMPOSING two of them, and one that also
        // declares a `Description` and an `ExportName` — the value is published the same way
        // whatever else the output declares beside it.
        Row {
            case: "a published attribute, an echoed Data entry and a composition of the two",
            stack_name: "published-outputs-attributes",
            topic_name: Some(DECLARED_TOPIC_NAME),
            answer_data: json!({"Echo": ECHOED}).to_string(),
            answer_id: None,
            stated_marker: None,
            outputs: vec![
                Out {
                    key: "Echo",
                    value: json!({"Fn::GetAtt": ["MyResource", "Echo"]}),
                    expect: Expect::Exact(ECHOED),
                    exported: None,
                },
                // The parameter the request left unstated reaches the provider as the
                // template's own declared default, and that is what the output publishes.
                Out {
                    key: "StatedMarker",
                    value: json!({"Fn::GetAtt": ["MyResource", "StatedMarker"]}),
                    expect: Expect::Exact(DEFAULT_MARKER),
                    exported: None,
                },
                Out {
                    key: "TopicArn",
                    value: json!({"Ref": "AlwaysTopic"}),
                    expect: Expect::TopicArn {
                        named: Some(DECLARED_TOPIC_NAME),
                    },
                    exported: None,
                },
                Out {
                    key: "TopicName",
                    value: json!({"Fn::GetAtt": ["AlwaysTopic", "TopicName"]}),
                    expect: Expect::Exact(DECLARED_TOPIC_NAME),
                    exported: None,
                },
                Out {
                    key: "Joined",
                    value: json!({
                        "Fn::Join": [
                            "|",
                            [
                                {"Ref": "AlwaysTopic"},
                                {"Fn::GetAtt": ["MyResource", "RequestType"]},
                            ],
                        ],
                    }),
                    expect: Expect::Composed {
                        around: "{}|Create",
                        from: "TopicArn",
                    },
                    exported: Some("published-outputs-joined"),
                },
            ],
        },
        // Several `Data` entries, every one of them readable, and a physical resource id the
        // PROVIDER minted: what `Ref` publishes for a custom resource is the id its provider
        // answered with.
        Row {
            case: "several Data entries and a provider-minted physical resource id",
            stack_name: "published-outputs-many-entries",
            topic_name: None,
            answer_data: json!({"Echo": "first", "Marker": "second", "Count": "3"}).to_string(),
            answer_id: Some("probe/42"),
            stated_marker: Some("a marker of its own"),
            outputs: vec![
                Out {
                    key: "ThingRef",
                    value: json!({"Ref": "MyResource"}),
                    expect: Expect::Exact("probe/42"),
                    exported: None,
                },
                Out {
                    key: "StatedMarker",
                    value: json!({"Fn::GetAtt": ["MyResource", "StatedMarker"]}),
                    expect: Expect::Exact("a marker of its own"),
                    exported: None,
                },
                Out {
                    key: "AttEcho",
                    value: json!({"Fn::GetAtt": ["MyResource", "Echo"]}),
                    expect: Expect::Exact("first"),
                    exported: None,
                },
                Out {
                    key: "AttMarker",
                    value: json!({"Fn::GetAtt": ["MyResource", "Marker"]}),
                    expect: Expect::Exact("second"),
                    exported: None,
                },
                Out {
                    key: "AttCount",
                    value: json!({"Fn::GetAtt": ["MyResource", "Count"]}),
                    expect: Expect::Exact("3"),
                    exported: None,
                },
                Out {
                    key: "RequestType",
                    value: json!({"Fn::GetAtt": ["MyResource", "RequestType"]}),
                    expect: Expect::Exact("Create"),
                    exported: None,
                },
                Out {
                    key: "TopicArn",
                    value: json!({"Ref": "AlwaysTopic"}),
                    expect: Expect::TopicArn { named: None },
                    exported: None,
                },
            ],
        },
    ]
}

/// The template one row deploys: the execution role the provider assumes, the provider itself
/// with this suite's inline source, the custom resource that provider backs, the sibling topic,
/// and the row's declared outputs.
fn template(row: &Row) -> String {
    let mut topic_properties = Map::new();
    if let Some(name) = row.topic_name {
        topic_properties.insert("TopicName".to_string(), json!(name));
    }
    let mut resource_properties = Map::new();
    resource_properties.insert(
        "ServiceToken".to_string(),
        json!({"Fn::GetAtt": ["ProviderFunction", "Arn"]}),
    );
    resource_properties.insert("Marker".to_string(), json!({"Ref": "Marker"}));
    resource_properties.insert("AnswerData".to_string(), json!(row.answer_data));
    if let Some(id) = row.answer_id {
        resource_properties.insert("AnswerId".to_string(), json!(id));
    }
    let mut outputs = Map::new();
    for out in &row.outputs {
        let mut declared = Map::new();
        declared.insert("Value".to_string(), out.value.clone());
        if let Some(export_name) = out.exported {
            declared.insert(
                "Description".to_string(),
                json!(DECLARED_DESCRIPTION),
            );
            declared.insert("Export".to_string(), json!({"Name": export_name}));
        }
        outputs.insert(out.key.to_string(), Value::Object(declared));
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {"Marker": {"Type": "String", "Default": DEFAULT_MARKER}},
        "Resources": {
            "ProviderRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            "ProviderFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": PROVIDER},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ProviderRole", "Arn"]},
                    "Runtime": "python3.12",
                },
            },
            "MyResource": {
                "Type": "Custom::Sim2RealTest",
                "Properties": Value::Object(resource_properties),
            },
            "AlwaysTopic": {
                "Type": "AWS::SNS::Topic",
                "Properties": Value::Object(topic_properties),
            },
        },
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Every output a `DescribeStacks` answer reports, keyed by `OutputKey` — read with the
/// target's one member-by-member `Outputs` reader (`crate::stack_outputs::wire_outputs`), so an
/// entry carrying no `OutputValue` is visible as the `None` it is rather than borrowing the
/// next entry's value.
fn outputs_of(body: &str) -> BTreeMap<String, WireOutput> {
    wire_outputs(body)
        .into_iter()
        .filter_map(|output| output.key.clone().map(|key| (key, output)))
        .collect()
}

/// Deploy one row and answer the outputs the settled stack reports.
async fn deployed(router: &MultiServiceRouter, row: &Row) -> BTreeMap<String, WireOutput> {
    let case = row.case;
    let mut extra = vec![
        member("Capabilities.member.1", "CAPABILITY_IAM"),
        member("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
    ];
    if let Some(marker) = row.stated_marker {
        extra.push(member("Parameters.member.1.ParameterKey", "Marker"));
        extra.push(member("Parameters.member.1.ParameterValue", marker));
    }
    let (status_code, created) =
        create_stack(router, row.stack_name, &template(row), &extra).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status_code} with {created}"
    );
    let body = settled_stack(case, router, REGION, row.stack_name).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to settle on its provider's answer, got {body}"
    );
    let reported = outputs_of(&body);
    let keys: Vec<&str> = reported.keys().map(String::as_str).collect();
    let mut declared: Vec<&str> = row.outputs.iter().map(|out| out.key).collect();
    declared.sort_unstable();
    assert_eq!(
        keys, declared,
        "case {case:?}: expected the stack to report one Output per declared output, got {body}"
    );
    reported
}

#[tokio::test]
async fn every_output_of_a_settled_stack_carries_its_resolved_value() {
    for row in rows() {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let reported = deployed(&router, &row).await;
        // The half of the rule a client's `{o["OutputKey"]: o["OutputValue"]}` read needs: a
        // value for EVERY declared output, read before anything is asserted about any one of
        // them.
        let mut values = BTreeMap::new();
        for out in &row.outputs {
            let entry = reported.get(out.key).unwrap_or_else(|| {
                panic!("case {case:?}: expected an Output {:?}, got {reported:?}", out.key)
            });
            let value = entry.value.clone().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the Output {:?} of a CREATE_COMPLETE stack carries no \
                     OutputValue at all, got {entry:?}",
                    out.key
                )
            });
            assert!(
                !value.is_empty(),
                "case {case:?}: the Output {:?} carries an empty OutputValue",
                out.key
            );
            values.insert(out.key, value);
        }
        // ...and the other half: each value is what that output's expression resolves to
        // against the stack's own state.
        for out in &row.outputs {
            let actual = &values[out.key];
            match &out.expect {
                Expect::Exact(expected) => assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the Output {:?} to publish {expected:?}, got \
                     {actual:?}",
                    out.key
                ),
                Expect::TopicArn { named } => {
                    let prefix = format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:");
                    assert!(
                        actual.starts_with(&prefix),
                        "case {case:?}: expected the Output {:?} to publish a topic ARN under \
                         {prefix:?}, got {actual:?}",
                        out.key
                    );
                    if let Some(name) = named {
                        assert_eq!(
                            actual,
                            &format!("{prefix}{name}"),
                            "case {case:?}: expected the Output {:?} to publish the declared \
                             topic's ARN, got {actual:?}",
                            out.key
                        );
                    }
                    // The published value is BACKED: the service that owns the topic answers
                    // for the ARN the stack published, so a client reading the stack's contract
                    // can call what it names.
                    let (status_code, attributes) =
                        sns(&router, "GetTopicAttributes", &[("TopicArn", actual)]).await;
                    assert_eq!(
                        status_code, 200,
                        "case {case:?}: expected Amazon SNS to answer for the published ARN \
                         {actual:?}, got {status_code} with {attributes}"
                    );
                }
                Expect::Composed { around, from } => {
                    let expected = around.replace("{}", &values[from]);
                    assert_eq!(
                        actual, &expected,
                        "case {case:?}: expected the Output {:?} to publish the composition \
                         {expected:?}, got {actual:?}",
                        out.key
                    );
                }
            }
            // An output that declares a description and an export publishes those beside its
            // value, so the value is reported whatever else the entry carries.
            if let Some(export_name) = out.exported {
                let entry = &reported[out.key];
                assert_eq!(
                    entry.export_name.as_deref(),
                    Some(export_name),
                    "case {case:?}: expected the Output {:?} to report its ExportName, got \
                     {entry:?}",
                    out.key
                );
                assert_eq!(
                    entry.description.as_deref(),
                    Some(DECLARED_DESCRIPTION),
                    "case {case:?}: expected the Output {:?} to report the Description the \
                     template declares, got {entry:?}",
                    out.key
                );
            }
        }
    }
}
