// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Redshift::EventSubscription` deploys: the
//! subscription is created in Amazon Redshift's own control plane under the name the stack
//! publishes, publishing to the Amazon SNS topic the template named; a re-deploy that states
//! the subscription differently reaches that same subscription; deleting the stack takes it
//! away — and a declaration naming an ARN Amazon SNS holds no topic under fails the RESOURCE
//! with Amazon Redshift's own refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so the handle the
//! stack publishes (`Ref`) resolves in that service, the values it publishes as `Fn::GetAtt`
//! attributes are the ones that service answered, and what that service refuses is a FAILED
//! RESOURCE rather than a template-format error.** Three crates have to agree for that to hold
//! (CloudFormation submits the template and makes the call, Amazon Redshift serves it, and
//! Amazon SNS answers for the topic the subscription notifies), so the suite lives here: a
//! crate-local test can only ever see one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: how the
//! declaration names the topic (a `Ref` to an `AWS::SNS::Topic` of the same stack, a literal ARN
//! of a topic that service already holds), whether it NAMES the subscription or leaves the name
//! to CloudFormation, which of the optional scoping properties it carries (a severity, a source
//! type with the source ids of a resource of the same stack, event categories, the enabled
//! flag), whether it carries the type's own `Tags`, the template spelling (JSON with long-form
//! intrinsics, YAML with the short-form tags), and how many subscriptions one stack declares.
//!
//! Every row reads the subscription back through Amazon Redshift's own
//! `DescribeEventSubscriptions`, and none of them may ever see the `Template format error:
//! Unrecognized resource types` refusal a type CloudFormation does not know earns.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack's own clock-owed transition has to come due, and the suite
/// waits for the CONDITION (the stack's status) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The resource type under test, and the property it takes its name from.
const SUBSCRIPTION: &str = "AWS::Redshift::EventSubscription";
const NAME_PROPERTY: &str = "SubscriptionName";

/// How long an event subscription's name may be, and the alphabet it is held to: "Cannot be
/// null, empty, or blank. Must contain from 1 to 255 alphanumeric characters or hyphens. First
/// character must be a letter. Cannot end with a hyphen or contain two consecutive hyphens."
/// (`smithy_query(query="doc", shape="CreateEventSubscriptionMessage",
/// member="SubscriptionName")`.)
const MAX_SUBSCRIPTION_NAME_CHARACTERS: usize = 255;

/// The family a declared cluster parameter group belongs to
/// (`CreateClusterParameterGroup$ParameterGroupFamily`), for the row whose subscription is
/// scoped to a source of the same stack.
const PARAMETER_GROUP_FAMILY: &str = "redshift-1.0";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_redshift_redshiftserviceversion20121201::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one query-protocol operation of `service`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one Amazon Redshift operation over its awsQuery wire.
async fn redshift(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "redshift", "2012-12-01", action, members).await
}

/// A topic this account really holds, created through Amazon SNS's own `CreateTopic`: the ARN
/// that service minted for it, which is what a declaration may name literally.
async fn create_topic(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "expected CreateTopic to succeed for {name:?}, got {status} with body {body}"
    );
    opt_element(&body, "TopicArn")
        .unwrap_or_else(|| panic!("no TopicArn in {body}"))
}

/// The `<EventSubscription>` Amazon Redshift reports for `name`, or the refusal it answered.
async fn describe_subscription(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    redshift(
        router,
        "DescribeEventSubscriptions",
        &[(NAME_PROPERTY, name)],
    )
    .await
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || std::time::Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack `stack_name` from `template` and answer its settled `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an {SUBSCRIPTION} is a type a \
         template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// Whether `name` is a name Amazon Redshift's published syntax for an event subscription
/// admits, which is what a name CloudFormation GENERATES has to be inside.
fn is_a_published_subscription_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_SUBSCRIPTION_NAME_CHARACTERS
        && name.starts_with(|first: char| first.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// What one row expects of one subscription the stack declares.
struct Declared {
    /// The output key whose value is the subscription's name — what its `Ref` returns.
    name_output: &'static str,
    /// The name the declaration states, or `None` where it leaves the name to CloudFormation.
    declared_name: Option<&'static str>,
    /// The scalar members Amazon Redshift must report for the subscription. A value spelled
    /// `{topic}` is the ARN of the topic this row's subscriptions publish to, and one spelled
    /// `{output:KEY}` is the value the stack published under that output key.
    reported: &'static [(&'static str, &'static str)],
    /// The elements its `SourceIdsList` must carry, with the same placeholders.
    reported_source_ids: &'static [&'static str],
    /// The elements its `EventCategoriesList` must carry.
    reported_categories: &'static [&'static str],
    /// The tags the subscription must carry, as the declaration states them.
    reported_tags: &'static [(&'static str, &'static str)],
    /// The stack outputs that read a published `Fn::GetAtt` attribute, as
    /// `(output key, the member Amazon Redshift reports it as)`: each must carry exactly what
    /// that service answered, which is what makes the attribute the owning service's own.
    attribute_outputs: &'static [(&'static str, &'static str)],
}

/// One row: a template and the subscriptions it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

/// The rows of the deploy table, built around a topic this account already holds — the topic
/// the rows that name one literally publish to.
fn rows(held_topic: &str) -> Vec<Row> {
    vec![
        // The issue's own stack: a named subscription over an `AWS::SNS::Topic` of the same
        // stack, with the name and the attributes read back through the outputs, in JSON with
        // long-form intrinsics.
        Row {
            case: "a named subscription over a topic of the same stack, JSON spelling",
            stack_name: "redshift-subscription-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Topic": {
                        "Type": "AWS::SNS::Topic",
                        "Properties": { "TopicName": "redshift-events-round-trip" }
                    },
                    "Subscription": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-subscription-round-trip",
                            "SnsTopicArn": {"Ref": "Topic"},
                            "SourceType": "cluster",
                            "Severity": "ERROR",
                            "Enabled": true,
                        }
                    }
                },
                "Outputs": {
                    "SubscriptionName": {"Value": {"Ref": "Subscription"}},
                    "TopicArn": {"Value": {"Ref": "Topic"}},
                    "Status": {"Value": {"Fn::GetAtt": ["Subscription", "Status"]}},
                    "Account": {"Value": {"Fn::GetAtt": ["Subscription", "CustomerAwsId"]}},
                    "HeldName": {
                        "Value": {"Fn::GetAtt": ["Subscription", "CustSubscriptionId"]}
                    },
                    "CreatedAt": {
                        "Value": {"Fn::GetAtt": ["Subscription", "SubscriptionCreationTime"]}
                    }
                }
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: Some("cfn-subscription-round-trip"),
                reported: &[
                    ("SnsTopicArn", "{output:TopicArn}"),
                    ("SourceType", "cluster"),
                    ("Severity", "ERROR"),
                    ("Enabled", "true"),
                ],
                reported_source_ids: &[],
                reported_categories: &[],
                reported_tags: &[],
                attribute_outputs: &[
                    ("Status", "Status"),
                    ("Account", "CustomerAwsId"),
                    ("HeldName", "CustSubscriptionId"),
                    ("CreatedAt", "SubscriptionCreationTime"),
                ],
            }],
        },
        // A literal ARN of a topic Amazon SNS already holds, with the subscription's name left
        // to CloudFormation: the type names itself nowhere, so the name the stack publishes is
        // one CloudFormation generated — and Amazon Redshift has to hold a subscription under
        // exactly it.
        Row {
            case: "an unnamed subscription over a literal topic ARN",
            stack_name: "redshift-subscription-generated-name",
            template: json!({
                "Resources": {
                    "Subscription": {
                        "Type": SUBSCRIPTION,
                        "Properties": { "SnsTopicArn": held_topic }
                    }
                },
                "Outputs": {"SubscriptionName": {"Value": {"Ref": "Subscription"}}}
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: None,
                reported: &[("SnsTopicArn", "{topic}"), ("Enabled", "true")],
                reported_source_ids: &[],
                reported_categories: &[],
                reported_tags: &[],
                attribute_outputs: &[],
            }],
        },
        // A subscription scoped to a SOURCE of the same stack — the cluster parameter group
        // declared beside it — and to two event categories, in YAML with the short-form tags
        // and with the type's own `Tags`. The list attribute the type publishes is read back
        // through `Fn::GetAtt`.
        Row {
            case: "a subscription scoped to a source and categories, YAML spelling with tags",
            stack_name: "redshift-subscription-scoped",
            template: format!(
                "\
Resources:
  Group:
    Type: AWS::Redshift::ClusterParameterGroup
    Properties:
      ParameterGroupName: cfn-subscription-scoped-pg
      ParameterGroupFamily: {PARAMETER_GROUP_FAMILY}
      Description: the group the subscription is scoped to
  Subscription:
    Type: {SUBSCRIPTION}
    Properties:
      {NAME_PROPERTY}: cfn-subscription-scoped
      SnsTopicArn: {held_topic}
      SourceType: cluster-parameter-group
      SourceIds:
        - !Ref Group
      EventCategories:
        - configuration
        - management
      Tags:
        - Key: team
          Value: analytics
Outputs:
  SubscriptionName:
    Value: !Ref Subscription
  GroupName:
    Value: !Ref Group
  Sources:
    Value: !Join [',', !GetAtt Subscription.SourceIdsList]
"
            ),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: Some("cfn-subscription-scoped"),
                reported: &[
                    ("SnsTopicArn", "{topic}"),
                    ("SourceType", "cluster-parameter-group"),
                    ("Enabled", "true"),
                ],
                reported_source_ids: &["{output:GroupName}"],
                reported_categories: &["configuration", "management"],
                reported_tags: &[("team", "analytics")],
                attribute_outputs: &[("Sources", "SourceIdsList")],
            }],
        },
        // Two subscriptions in one stack, one of them created INACTIVE: each publishes its OWN
        // name, and each is a subscription that service holds.
        Row {
            case: "two subscriptions in one stack, one of them inactive",
            stack_name: "redshift-subscription-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-subscription-pair-first",
                            "SnsTopicArn": held_topic,
                            "Severity": "INFO",
                        }
                    },
                    "Second": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-subscription-pair-second",
                            "SnsTopicArn": held_topic,
                            "Enabled": false,
                        }
                    }
                },
                "Outputs": {
                    "FirstName": {"Value": {"Ref": "First"}},
                    "SecondName": {"Value": {"Ref": "Second"}}
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    name_output: "FirstName",
                    declared_name: Some("cfn-subscription-pair-first"),
                    reported: &[
                        ("SnsTopicArn", "{topic}"),
                        ("Severity", "INFO"),
                        ("Enabled", "true"),
                    ],
                    reported_source_ids: &[],
                    reported_categories: &[],
                    reported_tags: &[],
                    attribute_outputs: &[],
                },
                Declared {
                    name_output: "SecondName",
                    declared_name: Some("cfn-subscription-pair-second"),
                    reported: &[("SnsTopicArn", "{topic}"), ("Enabled", "false")],
                    reported_source_ids: &[],
                    reported_categories: &[],
                    reported_tags: &[],
                    attribute_outputs: &[],
                },
            ],
        },
    ]
}

/// The element name a LIST member of the reported record repeats, from each list shape's own
/// member `@xmlName` (`smithy_query(query="shape",
/// shape="SourceIdsList,EventCategoriesList")`); `None` for a member that is not one of them.
fn list_element_of(member: &str) -> Option<&'static str> {
    match member {
        "SourceIdsList" => Some("SourceId"),
        "EventCategoriesList" => Some("EventCategory"),
        _ => None,
    }
}

/// One expected value with its placeholders filled in: `{topic}` is the topic the row's
/// subscriptions publish to, and `{output:KEY}` the value the stack published under that key.
fn filled(expected: &str, topic: &str, outputs: &BTreeMap<String, String>) -> String {
    let mut filled = expected.replace("{topic}", topic);
    for (key, value) in outputs {
        filled = filled.replace(&format!("{{output:{key}}}"), value);
    }
    filled
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_subscription_redshift_holds() {
    // ONE router for the table: the topic the rows name literally is created in it first, so
    // every template is deployed against the account that really holds exactly that topic.
    let router = build_router();
    let held_topic = create_topic(&router, "redshift-events-held").await;

    for row in rows(&held_topic) {
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with failure reasons \
             {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        for declared in &row.declared {
            let name = outputs
                .get(declared.name_output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} output in {body}", declared.name_output)
                })
                .clone();
            match declared.declared_name {
                // A declaration that NAMES the subscription decides the name, so the handle
                // the stack publishes is exactly it.
                Some(stated) => assert_eq!(
                    name, stated,
                    "case {case:?}: expected the declared name to be the published handle, got \
                     {name:?}"
                ),
                // One that does not leaves it to CloudFormation, whose generated name still
                // has to be inside the syntax the owning service publishes.
                None => assert!(
                    is_a_published_subscription_name(&name),
                    "case {case:?}: expected a generated name inside Amazon Redshift's \
                     published syntax, got {name:?}"
                ),
            }

            // THE RULE: the subscription the stack claims to have created is one Amazon
            // Redshift holds, under the name the stack published, scoped as the template
            // declared.
            let (status, reported) = describe_subscription(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon Redshift to report the subscription the stack \
                 created under {name:?}, got {status} with body {reported}"
            );
            let subscription = block_of(&reported, "EventSubscription")
                .unwrap_or_else(|| panic!("case {case:?}: no EventSubscription in {reported}"));
            assert_eq!(
                opt_element(subscription, "CustSubscriptionId").as_deref(),
                Some(name.as_str()),
                "case {case:?}: expected the subscription to carry the published name, got \
                 {subscription}"
            );
            for (member, expected) in declared.reported {
                let expected = filled(expected, &held_topic, &outputs);
                assert_eq!(
                    opt_element(subscription, member).as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}/{name}: expected {member} to be {expected:?}, got \
                     {subscription}"
                );
            }
            let expected_sources: Vec<String> = declared
                .reported_source_ids
                .iter()
                .map(|source| filled(source, &held_topic, &outputs))
                .collect();
            let reported_sources = block_of(subscription, "SourceIdsList")
                .map(|list| elements(list, "SourceId"))
                .unwrap_or_default();
            assert_eq!(
                reported_sources, expected_sources,
                "case {case:?}/{name}: expected the sources {expected_sources:?}, got \
                 {subscription}"
            );
            let reported_categories = block_of(subscription, "EventCategoriesList")
                .map(|list| elements(list, "EventCategory"))
                .unwrap_or_default();
            assert_eq!(
                reported_categories, declared.reported_categories,
                "case {case:?}/{name}: expected the categories {:?}, got {subscription}",
                declared.reported_categories
            );
            let tags = block_of(subscription, "Tags").unwrap_or_default();
            for (key, value) in declared.reported_tags {
                assert!(
                    tags.contains(&format!("<Key>{key}</Key>"))
                        && tags.contains(&format!("<Value>{value}</Value>")),
                    "case {case:?}/{name}: expected the declared tag {key}={value} on the \
                     subscription, got {subscription}"
                );
            }
            // The three tags CloudFormation propagates are in the namespace Amazon Redshift
            // reserves, so a subscription carries the stack's own tags without them.
            assert!(
                !tags.contains("aws:"),
                "case {case:?}/{name}: expected no reserved tag key on the subscription, got \
                 {subscription}"
            );

            // And every published attribute the stack reports is the value that service
            // answered — not one computed here.
            for (output, member) in declared.attribute_outputs {
                let published = outputs
                    .get(*output)
                    .unwrap_or_else(|| panic!("case {case:?}: no {output} output in {body}"));
                let answered = match list_element_of(member) {
                    // A LIST member is published as its elements joined, which is what the
                    // template's own `Fn::Join` over the attribute produces.
                    Some(element) => block_of(subscription, member)
                        .map(|list| elements(list, element).join(","))
                        .unwrap_or_default(),
                    None => opt_element(subscription, member).unwrap_or_default(),
                };
                assert!(
                    !published.is_empty(),
                    "case {case:?}/{name}: expected the {output} output to carry the {member} \
                     Amazon Redshift answered, got an empty value"
                );
                assert_eq!(
                    published, &answered,
                    "case {case:?}/{name}: expected the {output} output to be the {member} \
                     Amazon Redshift reports ({answered:?}), got {published:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_subscriptions_it_created() {
    let router = build_router();
    let case = "two subscriptions deleted with their stack";
    let stack_name = "redshift-subscription-deleted";
    let topic = create_topic(&router, "redshift-events-deleted").await;
    let template = json!({
        "Resources": {
            "First": {
                "Type": SUBSCRIPTION,
                "Properties": {
                    NAME_PROPERTY: "cfn-subscription-deleted-first",
                    "SnsTopicArn": topic,
                }
            },
            "Second": {
                "Type": SUBSCRIPTION,
                "Properties": {
                    NAME_PROPERTY: "cfn-subscription-deleted-second",
                    "SnsTopicArn": topic,
                    "Severity": "INFO",
                }
            }
        },
        "Outputs": {
            "FirstName": {"Value": {"Ref": "First"}},
            "SecondName": {"Value": {"Ref": "Second"}}
        }
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body} with failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );
    let outputs = outputs_of(&body);
    let names: Vec<String> = ["FirstName", "SecondName"]
        .iter()
        .map(|key| {
            outputs
                .get(*key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
                .clone()
        })
        .collect();
    for name in &names {
        let (status, reported) = describe_subscription(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the subscription {name:?} to exist before the delete, got \
             {status} with body {reported}"
        );
    }

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    let settled = settled_deletion(&router, case, stack_name, &names).await;
    assert!(
        !settled.contains("DELETE_FAILED"),
        "case {case:?}: expected the delete to succeed, got {settled}"
    );

    // The subscriptions the stack created are gone from Amazon Redshift, which is what makes a
    // stack's resources the stack's own rather than leftovers.
    for name in &names {
        let (status, reported) = describe_subscription(&router, name).await;
        assert_eq!(
            (status, elements(&reported, "Code")),
            (404, vec!["SubscriptionNotFound".to_string()]),
            "case {case:?}: expected the subscription {name:?} to be gone with the stack, got \
             {status} with body {reported}"
        );
    }
}

/// Wait until the deletion of `stack_name` has run its course: every subscription it created is
/// gone from Amazon Redshift, or the stack itself reports a status that is not in progress (a
/// deleted stack is no longer readable by name at all, which is the same end state). Answers
/// the last stack answer read, for the assertion's message.
///
/// Waits for the CONDITION rather than for a span, under a generous deadline so a loaded host
/// cannot decide the outcome.
async fn settled_deletion(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    names: &[String],
) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut gone = true;
        for name in names {
            let (status, _) = describe_subscription(router, name).await;
            gone &= status != 200;
        }
        let (_, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        if gone || std::time::Instant::now() >= deadline {
            return body;
        }
        assert!(
            !body.contains("DELETE_FAILED"),
            "case {case:?}: expected the delete to succeed, got {body}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// One re-deploy row: the property a second revision of the same template states differently,
/// and the value Amazon Redshift must report for the subscription afterwards.
struct Changed {
    case: &'static str,
    stack_name: &'static str,
    subscription_name: &'static str,
    /// The property the first revision states and the value it states.
    before: (&'static str, serde_json::Value),
    /// The same property in the second revision.
    after: (&'static str, serde_json::Value),
    /// The member Amazon Redshift reports the changed property as, and the value it must carry
    /// once the update has run.
    reported: (&'static str, &'static str),
}

/// The rows differ along WHICH member of `ModifyEventSubscription` the re-deploy changes: the
/// severity it filters on, whether it is active, and the source type it is scoped to.
fn changed_rows() -> Vec<Changed> {
    vec![
        Changed {
            case: "a re-deploy that changes the severity",
            stack_name: "redshift-subscription-reseverity",
            subscription_name: "cfn-subscription-reseverity",
            before: ("Severity", json!("INFO")),
            after: ("Severity", json!("ERROR")),
            reported: ("Severity", "ERROR"),
        },
        Changed {
            case: "a re-deploy that deactivates the subscription",
            stack_name: "redshift-subscription-disabled",
            subscription_name: "cfn-subscription-disabled",
            before: ("Enabled", json!(true)),
            after: ("Enabled", json!(false)),
            reported: ("Enabled", "false"),
        },
        Changed {
            case: "a re-deploy that changes the source type",
            stack_name: "redshift-subscription-resourced",
            subscription_name: "cfn-subscription-resourced",
            before: ("SourceType", json!("cluster")),
            after: ("SourceType", json!("cluster-snapshot")),
            reported: ("SourceType", "cluster-snapshot"),
        },
    ]
}

#[tokio::test]
async fn a_changed_declaration_reaches_the_subscription_redshift_holds() {
    let router = build_router();
    let topic = create_topic(&router, "redshift-events-updated").await;

    for row in changed_rows() {
        let case = row.case;
        let template_of = |(property, value): (&str, serde_json::Value)| {
            json!({
                "Resources": {
                    "Subscription": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: row.subscription_name,
                            "SnsTopicArn": topic,
                            property: value,
                        }
                    }
                },
                "Outputs": {"SubscriptionName": {"Value": {"Ref": "Subscription"}}}
            })
            .to_string()
        };

        let body = create_and_settle(&router, case, row.stack_name, &template_of(row.before)).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with failure reasons \
             {:?}",
            failure_reasons(&router, row.stack_name).await
        );

        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", row.stack_name),
                ("TemplateBody", &template_of(row.after)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
        );
        let settled = settled_stack(&router, case, row.stack_name).await;
        assert_eq!(
            opt_element(&settled, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the change to be applied to the subscription that exists, \
             got {settled} with failure reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );

        // THE RULE, on the update side: the subscription Amazon Redshift holds under the name
        // the stack publishes carries what the new declaration states — the stack never
        // reports an update it applied to nothing.
        let (status, reported) = describe_subscription(&router, row.subscription_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the subscription to survive the update, got {status} with \
             body {reported}"
        );
        let subscription = block_of(&reported, "EventSubscription")
            .unwrap_or_else(|| panic!("case {case:?}: no EventSubscription in {reported}"));
        let (member, expected) = row.reported;
        assert_eq!(
            opt_element(subscription, member).as_deref(),
            Some(expected),
            "case {case:?}: expected {member} to be {expected:?} after the update, got \
             {subscription}"
        );
    }
}

/// One refusal row: the ARN the declaration states, which names no topic this account holds.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    subscription_name: &'static str,
    sns_topic_arn: &'static str,
}

const REFUSED: &[Refused] = &[
    // The issue's own class of refusal: a well-formed topic ARN nothing ever created.
    Refused {
        case: "a topic ARN this account does not hold",
        stack_name: "redshift-subscription-unknown-topic",
        subscription_name: "cfn-subscription-unknown-topic",
        sns_topic_arn: "arn:aws:sns:us-east-1:000000000000:no-such-topic",
    },
    // The same, under another name: the value is the caller's and the refusal names it.
    Refused {
        case: "another topic ARN this account does not hold",
        stack_name: "redshift-subscription-other-unknown-topic",
        subscription_name: "cfn-subscription-other-unknown-topic",
        sns_topic_arn: "arn:aws:sns:us-east-1:000000000000:redshift-events-never-made",
    },
];

#[tokio::test]
async fn a_topic_sns_does_not_hold_rolls_the_stack_back() {
    for row in REFUSED {
        let router = build_router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Subscription": {
                    "Type": SUBSCRIPTION,
                    "Properties": {
                        NAME_PROPERTY: row.subscription_name,
                        "SnsTopicArn": row.sns_topic_arn,
                    }
                }
            },
            "Outputs": {"SubscriptionName": {"Value": {"Ref": "Subscription"}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish a \
             subscription Amazon Redshift never held, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, and
        // never a refusal of the template: no event reports a template-format error at all.
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        // The sentence Amazon Redshift words this class of refusal with, naming the topic the
        // declaration stated, and the `HandlerErrorCode` class a refused REQUEST falls in.
        let sentence = format!(
            "Failed to create Subscription. Topic: {} not found.",
            row.sns_topic_arn
        );
        assert!(
            reasons.iter().any(|reason| reason.contains(&sentence)),
            "case {case:?}: expected a failure reason carrying {sentence:?}, got {reasons:?}"
        );

        // And nothing was created: Amazon Redshift holds no subscription under the declared
        // name.
        let (status, reported) = describe_subscription(&router, row.subscription_name).await;
        assert_eq!(
            (status, elements(&reported, "Code")),
            (404, vec!["SubscriptionNotFound".to_string()]),
            "case {case:?}: expected Amazon Redshift to hold no subscription named {:?}, got \
             {status} with body {reported}",
            row.subscription_name
        );
    }
}
