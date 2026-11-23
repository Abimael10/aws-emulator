// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::RDS::EventSubscription` deploys: the
//! subscription is created in Amazon RDS's own control plane under the name the stack publishes,
//! publishing to the Amazon SNS topic the template named; deleting the stack takes it away — and
//! a declaration Amazon RDS refuses (an event category that service defines for no source type,
//! a category defined for ANOTHER source type, a topic ARN Amazon SNS holds no topic under, a
//! source identifier that service holds nothing under, categories stated without the source type
//! they belong to) fails the RESOURCE with that service's own refusal and rolls the stack back.
//!
//! The rule under test: **a CloudFormation resource type whose owning service identifies the
//! resource by a name the declaration states carries that name as its physical id, and the
//! declaration is deployed by CALLING that service — so the handle the stack publishes (`Ref`)
//! resolves in the service that owns the resource, and what that service refuses is a FAILED
//! RESOURCE rather than a stack that completes over a resource nothing holds.** Three crates
//! have to agree for that to hold (CloudFormation submits the template and makes the call,
//! Amazon RDS serves it, and Amazon SNS answers for the topic the subscription notifies), so the
//! suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: how the declaration
//! names the topic (a `Ref` to an `AWS::SNS::Topic` of the same stack, a literal ARN of a topic
//! that service already holds), whether it NAMES the subscription or leaves the name to
//! CloudFormation, which of the optional scoping properties it carries (a source type, the source
//! ids of a resource that service holds, event categories, the enabled flag), whether it carries
//! the type's own `Tags`, the template spelling (JSON with long-form intrinsics, YAML with the
//! short-form tags), and how many subscriptions one stack declares.
//!
//! Every row reads the subscription back through Amazon RDS's own `DescribeEventSubscriptions`,
//! under the name the stack published.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack's resources belong
/// to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it looks
/// again. Generous: the stack's own clock-owed transitions have to come due, and the suite waits
/// for the CONDITION (the stack's status) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The resource type under test, and the property it takes its name from.
const SUBSCRIPTION: &str = "AWS::RDS::EventSubscription";
const NAME_PROPERTY: &str = "SubscriptionName";

/// How long an event subscription's name may be: "The name must be less than 255 characters"
/// (`smithy_query(query="doc", shape="CreateEventSubscriptionMessage",
/// member="SubscriptionName")`), which is the one bound that member publishes — and so the one a
/// name CloudFormation GENERATES has to be inside.
const MAX_SUBSCRIPTION_NAME_CHARACTERS: usize = 254;

/// The DB parameter group family the source this suite's scoped row names belongs to, from the
/// families `DescribeDBEngineVersions` publishes.
const PARAMETER_GROUP_FAMILY: &str = "mysql8.0";

/// The tag keys CloudFormation records the stack and the resource's place in it under, which
/// Amazon RDS accepts on a create's own `Tags` member — so a subscription a stack created carries
/// them beside the declared ones.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

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
    router.register_service(service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// Call one Amazon RDS operation over its awsQuery wire.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// A topic this account really holds, created through Amazon SNS's own `CreateTopic`: the ARN
/// that service minted for it, which is what a declaration may name literally.
async fn create_topic(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "expected CreateTopic to succeed for {name:?}, got {status} with body {body}"
    );
    opt_element(&body, "TopicArn").unwrap_or_else(|| panic!("no TopicArn in {body}"))
}

/// A DB parameter group this account really holds, created through Amazon RDS's own
/// `CreateDBParameterGroup`: the name that service stored it under, which is what a
/// `db-parameter-group` subscription's `SourceIds` may name.
async fn create_parameter_group(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = rds(
        router,
        "CreateDBParameterGroup",
        &[
            ("DBParameterGroupName", name),
            ("DBParameterGroupFamily", PARAMETER_GROUP_FAMILY),
            ("Description", "the source a declared subscription names"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected CreateDBParameterGroup to succeed for {name:?}, got {status} with body {body}"
    );
    opt_element(&body, "DBParameterGroupName")
        .unwrap_or_else(|| panic!("no DBParameterGroupName in {body}"))
}

/// The `<EventSubscription>` Amazon RDS reports for `name`, or the refusal it answered.
async fn describe_subscription(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    rds(
        router,
        "DescribeEventSubscriptions",
        &[(NAME_PROPERTY, name)],
    )
    .await
}

/// The tags Amazon RDS reports for the resource `arn`, as the map a client reads them as.
async fn tags_of(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, body) = rds(router, "ListTagsForResource", &[("ResourceName", arn)]).await;
    assert_eq!(
        status, 200,
        "expected ListTagsForResource to succeed for {arn:?}, got {status} with body {body}"
    );
    blocks_of(&body, "Tag")
        .into_iter()
        .filter_map(|tag| {
            let key = elements(tag, "Key").into_iter().next()?;
            let value = elements(tag, "Value").into_iter().next()?;
            Some((key, value))
        })
        .collect()
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the stack
/// has settled.
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
    settled_stack(router, case, stack_name).await
}

/// What one row expects of one subscription the stack declares.
struct Declared {
    /// The output key whose value is the subscription's name — what its `Ref` returns.
    name_output: &'static str,
    /// The name the declaration states, or `None` where it leaves the name to CloudFormation.
    declared_name: Option<&'static str>,
    /// The logical id the declaration gives the resource, which CloudFormation records on it as
    /// a tag of its own.
    logical_id: &'static str,
    /// The scalar members Amazon RDS must report for the subscription. A value spelled `{topic}`
    /// is the ARN of the topic this row's subscriptions publish to, and one spelled
    /// `{output:KEY}` is the value the stack published under that output key.
    reported: &'static [(&'static str, &'static str)],
    /// The elements its `SourceIdsList` must carry, with the same placeholders.
    reported_source_ids: &'static [&'static str],
    /// The elements its `EventCategoriesList` must carry.
    reported_categories: &'static [&'static str],
    /// The tags the subscription must carry beside the ones CloudFormation propagates, as the
    /// declaration states them.
    reported_tags: &'static [(&'static str, &'static str)],
}

/// One row: a template and the subscriptions it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

/// The rows of the deploy table, built around a topic and a source this account already holds —
/// the topic the rows that name one literally publish to, and the resource the scoped row's
/// `SourceIds` names.
fn rows(held_topic: &str, held_source: &str) -> Vec<Row> {
    vec![
        // The issue's own stack: a named subscription over an `AWS::SNS::Topic` of the same
        // stack, with the name read back through an output, in JSON with long-form intrinsics.
        Row {
            case: "a named subscription over a topic of the same stack, JSON spelling",
            stack_name: "rds-subscription-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Topic": {
                        "Type": "AWS::SNS::Topic",
                        "Properties": { "TopicName": "rds-events-round-trip" }
                    },
                    "EventSub": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-rds-subscription-round-trip",
                            "SnsTopicArn": {"Ref": "Topic"},
                            "SourceType": "db-instance",
                            "EventCategories": ["availability", "low storage"],
                            "Enabled": true,
                        }
                    }
                },
                "Outputs": {
                    "SubscriptionName": {"Value": {"Ref": "EventSub"}},
                    "TopicArn": {"Value": {"Ref": "Topic"}}
                }
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: Some("cfn-rds-subscription-round-trip"),
                logical_id: "EventSub",
                reported: &[
                    ("SnsTopicArn", "{output:TopicArn}"),
                    ("SourceType", "db-instance"),
                    ("Enabled", "true"),
                    ("Status", "active"),
                ],
                reported_source_ids: &[],
                reported_categories: &["availability", "low storage"],
                reported_tags: &[],
            }],
        },
        // A literal ARN of a topic Amazon SNS already holds, with the subscription's name left to
        // CloudFormation: the type names itself nowhere, so the name the stack publishes is one
        // CloudFormation generated — and Amazon RDS has to hold a subscription under exactly it.
        Row {
            case: "an unnamed subscription over a literal topic ARN",
            stack_name: "rds-subscription-generated-name",
            template: json!({
                "Resources": {
                    "EventSub": {
                        "Type": SUBSCRIPTION,
                        "Properties": { "SnsTopicArn": held_topic }
                    }
                },
                "Outputs": {"SubscriptionName": {"Value": {"Ref": "EventSub"}}}
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: None,
                logical_id: "EventSub",
                reported: &[("SnsTopicArn", "{topic}"), ("Enabled", "true")],
                reported_source_ids: &[],
                reported_categories: &[],
                reported_tags: &[],
            }],
        },
        // A subscription scoped to a SOURCE Amazon RDS holds and to one event category of that
        // source type, in YAML with the short-form tags and with the type's own `Tags`.
        Row {
            case: "a subscription scoped to a source and a category, YAML spelling with tags",
            stack_name: "rds-subscription-scoped",
            template: format!(
                "\
Resources:
  EventSub:
    Type: {SUBSCRIPTION}
    Properties:
      {NAME_PROPERTY}: cfn-rds-subscription-scoped
      SnsTopicArn: {held_topic}
      SourceType: db-parameter-group
      SourceIds:
        - {held_source}
      EventCategories:
        - configuration change
      Enabled: false
      Tags:
        - Key: team
          Value: platform
Outputs:
  SubscriptionName:
    Value: !Ref EventSub
"
            ),
            declared: vec![Declared {
                name_output: "SubscriptionName",
                declared_name: Some("cfn-rds-subscription-scoped"),
                logical_id: "EventSub",
                reported: &[
                    ("SnsTopicArn", "{topic}"),
                    ("SourceType", "db-parameter-group"),
                    ("Enabled", "false"),
                ],
                reported_source_ids: &["{source}"],
                reported_categories: &["configuration change"],
                reported_tags: &[("team", "platform")],
            }],
        },
        // Two subscriptions in one stack, scoped to different source types: each publishes its
        // OWN name, and each is a subscription that service holds.
        Row {
            case: "two subscriptions in one stack, scoped to different source types",
            stack_name: "rds-subscription-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-rds-subscription-pair-first",
                            "SnsTopicArn": held_topic,
                            "SourceType": "db-cluster",
                            "EventCategories": ["global-failover"],
                        }
                    },
                    "Second": {
                        "Type": SUBSCRIPTION,
                        "Properties": {
                            NAME_PROPERTY: "cfn-rds-subscription-pair-second",
                            "SnsTopicArn": held_topic,
                            "SourceType": "db-snapshot",
                            "EventCategories": ["creation", "deletion"],
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
                    declared_name: Some("cfn-rds-subscription-pair-first"),
                    logical_id: "First",
                    reported: &[("SnsTopicArn", "{topic}"), ("SourceType", "db-cluster")],
                    reported_source_ids: &[],
                    reported_categories: &["global-failover"],
                    reported_tags: &[],
                },
                Declared {
                    name_output: "SecondName",
                    declared_name: Some("cfn-rds-subscription-pair-second"),
                    logical_id: "Second",
                    reported: &[("SnsTopicArn", "{topic}"), ("SourceType", "db-snapshot")],
                    reported_source_ids: &[],
                    reported_categories: &["creation", "deletion"],
                    reported_tags: &[],
                },
            ],
        },
    ]
}

/// One expected value with its placeholders filled in: `{topic}` is the topic the row's
/// subscriptions publish to, `{source}` the source its `SourceIds` names, and `{output:KEY}` the
/// value the stack published under that key.
fn filled(
    expected: &str,
    topic: &str,
    source: &str,
    outputs: &BTreeMap<String, String>,
) -> String {
    let mut filled = expected.replace("{topic}", topic).replace("{source}", source);
    for (key, value) in outputs {
        filled = filled.replace(&format!("{{output:{key}}}"), value);
    }
    filled
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_subscription_rds_holds() {
    // ONE router for the table: the topic and the source the rows name literally are created in
    // it first, so every template is deployed against the account that really holds them.
    let router = build_router();
    let held_topic = create_topic(&router, "rds-events-held").await;
    let held_source = create_parameter_group(&router, "cfn-rds-subscription-source").await;

    for row in rows(&held_topic, &held_source) {
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with failure reasons {:?}",
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
                // A declaration that NAMES the subscription decides the name, so the handle the
                // stack publishes is exactly it.
                Some(stated) => assert_eq!(
                    name, stated,
                    "case {case:?}: expected the declared name to be the published handle, got \
                     {name:?}"
                ),
                // One that does not leaves it to CloudFormation, whose generated name still has
                // to be inside the one bound the owning service publishes for it.
                None => assert!(
                    !name.is_empty() && name.chars().count() <= MAX_SUBSCRIPTION_NAME_CHARACTERS,
                    "case {case:?}: expected a generated name inside Amazon RDS's published \
                     bound of {MAX_SUBSCRIPTION_NAME_CHARACTERS} characters, got {name:?}"
                ),
            }

            // THE RULE: the subscription the stack claims to have created is one Amazon RDS
            // holds, under the name the stack published, scoped as the template declared.
            let (status, reported) = describe_subscription(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon RDS to report the subscription the stack created \
                 under {name:?}, got {status} with body {reported}"
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
                let expected = filled(expected, &held_topic, &held_source, &outputs);
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
                .map(|source| filled(source, &held_topic, &held_source, &outputs))
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

            // The tags the subscription carries are read through the owning service's own tag
            // report, on the ARN it minted for the subscription: the declared ones, and the ones
            // CloudFormation records about the stack and the resource's place in it.
            let arn = opt_element(subscription, "EventSubscriptionArn")
                .unwrap_or_else(|| panic!("case {case:?}: no EventSubscriptionArn in {reported}"));
            let tags = tags_of(&router, &arn).await;
            for (key, value) in declared.reported_tags {
                assert_eq!(
                    tags.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}/{name}: expected the declared tag {key}={value} on the \
                     subscription, got {tags:?}"
                );
            }
            assert_eq!(
                tags.get(STACK_NAME_TAG).map(String::as_str),
                Some(row.stack_name),
                "case {case:?}/{name}: expected the stack's own name to be recorded on the \
                 subscription, got {tags:?}"
            );
            assert_eq!(
                tags.get(LOGICAL_ID_TAG).map(String::as_str),
                Some(declared.logical_id),
                "case {case:?}/{name}: expected the resource's logical id to be recorded on the \
                 subscription, got {tags:?}"
            );
        }
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_subscriptions_it_created() {
    let router = build_router();
    let case = "two subscriptions deleted with their stack";
    let stack_name = "rds-subscription-deleted";
    let topic = create_topic(&router, "rds-events-deleted").await;
    let template = json!({
        "Resources": {
            "First": {
                "Type": SUBSCRIPTION,
                "Properties": {
                    NAME_PROPERTY: "cfn-rds-subscription-deleted-first",
                    "SnsTopicArn": topic,
                }
            },
            "Second": {
                "Type": SUBSCRIPTION,
                "Properties": {
                    NAME_PROPERTY: "cfn-rds-subscription-deleted-second",
                    "SnsTopicArn": topic,
                    "SourceType": "db-instance",
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

    // The subscriptions the stack created are gone from Amazon RDS, which is what makes a stack's
    // resources the stack's own rather than leftovers.
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
/// gone from Amazon RDS, or the stack itself reports a status that is not in progress (a deleted
/// stack is no longer readable by name at all, which is the same end state). Answers the last
/// stack answer read, for the assertion's message.
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
        let (_, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
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

/// One refusal row: the declaration Amazon RDS will not accept, and the sentence that service
/// words its refusal with.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    subscription_name: &'static str,
    /// The properties the declaration carries beside its name, as the template states them.
    properties: serde_json::Value,
    /// The fixed text of Amazon RDS's own refusal, whose variable parts are the values this row
    /// declared.
    sentence: String,
}

/// The refusal rows differ along WHICH value of the declaration the owning service refuses: an
/// event category it defines for no source type at all, one it defines for ANOTHER source type
/// than the declared one, a topic ARN Amazon SNS holds no topic under, a source identifier that
/// service holds nothing under, and categories stated without the source type they belong to.
fn refused_rows(absent_topic: &str) -> Vec<Refused> {
    vec![
        // The issue's own stack: a typo'd category, which names no event of the declared source
        // type.
        Refused {
            case: "an event category no source type defines",
            stack_name: "rds-subscription-undefined-category",
            subscription_name: "cfn-rds-subscription-undefined-category",
            properties: json!({
                "SourceType": "db-instance",
                "EventCategories": ["availabilty"],
            }),
            sentence: "Category : availabilty not found for source type db-instance.".to_string(),
        },
        // A category that IS published — for another source type: the categories of a
        // subscription are the ones its own source type defines.
        Refused {
            case: "an event category defined for another source type",
            stack_name: "rds-subscription-foreign-category",
            subscription_name: "cfn-rds-subscription-foreign-category",
            properties: json!({
                "SourceType": "db-cluster",
                "EventCategories": ["failure", "backtrack"],
            }),
            sentence: "Category : backtrack not found for source type db-cluster.".to_string(),
        },
        // A well-formed topic ARN nothing ever created: the subscription's delivery target does
        // not exist, so there is nothing for it to publish to.
        Refused {
            case: "a topic ARN this account does not hold",
            stack_name: "rds-subscription-unknown-topic",
            subscription_name: "cfn-rds-subscription-unknown-topic",
            properties: json!({"SnsTopicArn": absent_topic}),
            sentence: format!(
                "Failed to create Subscription because of Topic {absent_topic} Not Found."
            ),
        },
        // A source identifier of the declared source type that Amazon RDS holds nothing under.
        Refused {
            case: "a source identifier this account does not hold",
            stack_name: "rds-subscription-unknown-source",
            subscription_name: "cfn-rds-subscription-unknown-source",
            properties: json!({
                "SourceType": "db-instance",
                "SourceIds": ["no-such-db-instance"],
            }),
            sentence: "Could not find source :no-such-db-instance".to_string(),
        },
        // Categories without the source type they belong to: there is nothing to resolve them
        // against.
        Refused {
            case: "event categories without a source type",
            stack_name: "rds-subscription-categories-alone",
            subscription_name: "cfn-rds-subscription-categories-alone",
            properties: json!({"EventCategories": ["availability"]}),
            sentence: "If SourceType is null, Categories must also be null.".to_string(),
        },
    ]
}

#[tokio::test]
async fn a_declaration_rds_refuses_rolls_the_stack_back() {
    let absent_topic = format!("arn:aws:sns:{REGION}:000000000000:no-such-topic");
    for row in refused_rows(&absent_topic) {
        let router = build_router();
        let case = row.case;
        // Every row but the one about the topic publishes to a topic this account really holds,
        // so the only value the owning service can refuse is the one the row is about.
        let topic = create_topic(&router, "rds-events-refused").await;
        let mut properties = json!({NAME_PROPERTY: row.subscription_name, "SnsTopicArn": topic});
        for (property, value) in row
            .properties
            .as_object()
            .expect("a row's properties are an object")
        {
            properties[property] = value.clone();
        }
        let template = json!({
            "Resources": {
                "EventSub": {"Type": SUBSCRIPTION, "Properties": properties}
            },
            "Outputs": {"SubscriptionName": {"Value": {"Ref": "EventSub"}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than complete \
             over a subscription Amazon RDS never held, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed: the
        // sentence Amazon RDS worded, quoted as the provider reported it.
        let reasons = failure_reasons(&router, row.stack_name).await;
        assert!(
            reasons.iter().any(|reason| reason.contains(&row.sentence)),
            "case {case:?}: expected a failure reason carrying {:?}, got {reasons:?}",
            row.sentence
        );

        // And nothing was created: Amazon RDS holds no subscription under the declared name.
        let (status, reported) = describe_subscription(&router, row.subscription_name).await;
        assert_eq!(
            (status, elements(&reported, "Code")),
            (404, vec!["SubscriptionNotFound".to_string()]),
            "case {case:?}: expected Amazon RDS to hold no subscription named {:?}, got {status} \
             with body {reported}",
            row.subscription_name
        );
    }
}
