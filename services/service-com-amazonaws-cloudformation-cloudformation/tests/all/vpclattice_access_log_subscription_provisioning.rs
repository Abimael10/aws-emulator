// SPDX-License-Identifier: CC0-1.0

//! `AWS::VpcLattice::ServiceNetwork` and `AWS::VpcLattice::AccessLogSubscription` end to end:
//! the network and the access-log subscription a stack declares exist in Amazon VPC Lattice
//! itself, under the ARNs the stack publishes, and a destination the owning service does not
//! hold fails the resource with that service's own refusal.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it
//! is DEPLOYED by calling the service that owns the type — so `CreateStack` answers a
//! `StackId`, the handle the stack publishes (`Ref`) resolves in that service, the values the
//! type publishes are the ones that service reported, and what that service refuses is a
//! FAILED RESOURCE inside the resource-provider envelope rather than a template-format
//! `ValidationError`. The `Template format error: Unrecognized resource types` refusal is
//! reserved for a type CloudFormation does not know, which no case of this suite may ever see.
//!
//! The deploy rows differ along what the rule generalises over: which of the destination types
//! `CreateAccessLogSubscription` documents the records go to (a CloudWatch Logs log group, an
//! Amazon S3 bucket), the FORM the log-group destination is stated in (the `Arn` attribute that
//! type publishes, which names the group with everything under it, and the group's own ARN
//! stated without that suffix), whether the declaration states a
//! `ServiceNetworkLogType`, whether it carries its own `Tags`, whether the network's `Name` is
//! declared or left to CloudFormation, the template spelling (JSON with long-form intrinsics,
//! YAML with the short-form tags) and how many subscriptions one network carries.
//!
//! The refusal rows differ along WHICH destination service is asked about a resource it does
//! not hold — CloudWatch Logs for a log group no stack created, Amazon S3 for a bucket no stack
//! created — and each must reach `ROLLBACK_COMPLETE` with the handler's own 404 reason and no
//! `VALIDATION_ERROR` event at all.
//!
//! Everything goes over the wire each service speaks through the one router the harness builds
//! — the CloudFormation calls over its awsQuery wire, the VPC Lattice calls over that service's
//! `restJson1` wire, the CloudWatch Logs calls over its awsJson1_1 wire — so what is asserted
//! is what a client that deploys the template and then calls those services observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, envelope_parts, settled_stack_status,
    stack_event_reasons, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, output_value, router, ACCOUNT_ID, REGION,
};

/// The two resource types under test.
const SERVICE_NETWORK: &str = "AWS::VpcLattice::ServiceNetwork";
const ACCESS_LOG_SUBSCRIPTION: &str = "AWS::VpcLattice::AccessLogSubscription";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no case of this suite may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The SDK name Amazon VPC Lattice is reported under in a failed resource's reason, and the
/// status its not-found carries (the modelled 404 of `CreateAccessLogSubscription`).
const VPC_LATTICE_SDK_NAME: &str = "VpcLattice";
const NOT_FOUND_STATUS: u16 = 404;

/// The `HandlerErrorCode` class a resource provider reports a 404 from the owning service
/// under.
const NOT_FOUND_CLASS: &str = "NotFound";

/// The error code that service answers for a resource it does not hold.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The two values `ServiceNetworkLogType` models
/// (`smithy_query(query="shape",shape="ServiceNetworkLogType",
/// model_path="services/service-com-amazonaws-vpclattice-mercurycontrolplane/smithy-model/\
/// com-amazonaws-vpclattice-mercurycontrolplane.json")`).
const LOG_TYPE_SERVICE: &str = "SERVICE";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The awsJson version Amazon CloudWatch Logs's model declares, and the service shape its
/// `X-Amz-Target` names an operation under.
const LOGS_JSON: &str = "application/x-amz-json-1.1";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// One Amazon VPC Lattice call over the `restJson1` wire its model declares, on the method and
/// URI that operation's own `@http` binding names, signed for `vpc-lattice` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
async fn vpc_lattice(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://vpc-lattice.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("vpc-lattice.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/vpc-lattice/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Amazon VPC Lattice request");
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

/// One CloudWatch Logs call through the same router, signed for `logs` the way a client's SDK
/// signs it.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://logs.{REGION}.amazonaws.com/"))
        .header("content-type", LOGS_JSON)
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/logs/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed CloudWatch Logs request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Whether CloudWatch Logs holds a group under `name`.
async fn holds_the_log_group(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, document) = logs(
        router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": name}),
    )
    .await;
    status == 200
        && document
            .get("logGroups")
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups
                    .iter()
                    .any(|group| group.get("logGroupName").and_then(Value::as_str) == Some(name))
            })
}

/// That service's own read of one access log subscription, by the ARN the stack published.
async fn get_access_log_subscription(
    router: &MultiServiceRouter,
    identifier: &str,
) -> (u16, Value) {
    vpc_lattice(
        router,
        "GET",
        &format!("/accesslogsubscriptions/{}", encoded(identifier)),
        None,
    )
    .await
}

/// That service's own read of one service network, by the ARN the stack published.
async fn get_service_network(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    vpc_lattice(
        router,
        "GET",
        &format!("/servicenetworks/{}", encoded(identifier)),
        None,
    )
    .await
}

/// Every subscription that service lists for one monitored resource.
async fn list_access_log_subscriptions(
    router: &MultiServiceRouter,
    resource_identifier: &str,
) -> Vec<Value> {
    let (_, answered) = vpc_lattice(
        router,
        "GET",
        &format!(
            "/accesslogsubscriptions?resourceIdentifier={}",
            encoded(resource_identifier)
        ),
        None,
    )
    .await;
    answered
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every service network that service lists for the account.
async fn list_service_networks(router: &MultiServiceRouter) -> Vec<Value> {
    let (_, answered) = vpc_lattice(router, "GET", "/servicenetworks", None).await;
    answered
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds for one resource, by its ARN.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Value {
    let (_, answered) = vpc_lattice(router, "GET", &format!("/tags/{}", encoded(arn)), None).await;
    answered.get("tags").cloned().unwrap_or(Value::Null)
}

/// One identifier escaped as a single path segment, so an ARN's own `:` and `/` address the
/// resource rather than another path.
fn encoded(identifier: &str) -> String {
    identifier
        .chars()
        .map(|character| match character {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// The ARN a CloudWatch Logs log group of `name` carries, in the form that names the GROUP and
/// nothing under it: `arn:<partition>:logs:<region>:<account>:log-group:<name>`. The `Arn`
/// attribute `AWS::Logs::LogGroup` publishes is the same ARN with `:*` after it, which names
/// the group together with everything under it.
fn log_group_arn(name: &str) -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT_ID}:log-group:{name}")
}

/// What one deployed subscription must read back as.
struct Expected {
    /// The output the subscription's `Ref` is published in.
    ref_output: &'static str,
    /// The output its `Fn::GetAtt Id` is published in, where the row reads one.
    id_output: Option<&'static str>,
    /// The destination ARN that service must report for it.
    destination_arn: String,
    /// The `serviceNetworkLogType` the declaration stated, if any.
    log_type: Option<&'static str>,
    /// The declaration's own tags, beside the stack's which CloudFormation propagates.
    tags: &'static [(&'static str, &'static str)],
}

/// One deploy row: a template, the network it declares and the subscriptions on it.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    /// The output the network's `Ref` is published in.
    network_ref_output: &'static str,
    subscriptions: Vec<Expected>,
}

fn rows() -> Vec<Row> {
    let first_group = "/sim2real/als-round-trip";
    let second_group = "/sim2real/als-bare-arn";
    let paired_group = "/sim2real/als-paired";
    vec![
        // The issue's own stack: a network, a log group and a subscription pointing at the
        // group's published `Arn` — which names the group with everything under it — read back
        // through every return value the type publishes, in JSON with long-form intrinsics.
        Row {
            case: "a log group destination addressed by the Arn the log-group type publishes",
            stack_name: "vpclattice-als-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-round-trip"}},
                    "Lg": {
                        "Type": "AWS::Logs::LogGroup",
                        "Properties": {"LogGroupName": first_group, "RetentionInDays": 1}
                    },
                    "Als": {
                        "Type": ACCESS_LOG_SUBSCRIPTION,
                        "Properties": {
                            "ResourceIdentifier": {"Ref": "Sn"},
                            "DestinationArn": {"Fn::GetAtt": ["Lg", "Arn"]},
                            "Tags": [{"Key": "audit", "Value": "round-trip"}]
                        }
                    }
                },
                "Outputs": {
                    "SnRef": {"Value": {"Ref": "Sn"}},
                    "AlsRef": {"Value": {"Ref": "Als"}},
                    "AlsId": {"Value": {"Fn::GetAtt": ["Als", "Id"]}}
                }
            })
            .to_string(),
            network_ref_output: "SnRef",
            subscriptions: vec![Expected {
                ref_output: "AlsRef",
                id_output: Some("AlsId"),
                destination_arn: format!("{}:*", log_group_arn(first_group)),
                log_type: None,
                tags: &[("audit", "round-trip")],
            }],
        },
        // The same class with the destination stated as the group's OWN ARN — no `:*` suffix —
        // a declared `ServiceNetworkLogType`, a network whose name CloudFormation decides, and
        // the template written in YAML with the short-form intrinsics.
        Row {
            case: "a log group destination stated without the everything-under-it suffix, YAML",
            stack_name: "vpclattice-als-bare-arn",
            template: format!(
                "\
Resources:
  Sn:
    Type: {SERVICE_NETWORK}
  Lg:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: {second_group}
  Als:
    Type: {ACCESS_LOG_SUBSCRIPTION}
    DependsOn: Lg
    Properties:
      ResourceIdentifier: !Ref Sn
      DestinationArn: {}
      ServiceNetworkLogType: {LOG_TYPE_SERVICE}
Outputs:
  SnRef:
    Value: !Ref Sn
  AlsRef:
    Value: !Ref Als
",
                log_group_arn(second_group)
            ),
            network_ref_output: "SnRef",
            subscriptions: vec![Expected {
                ref_output: "AlsRef",
                id_output: None,
                destination_arn: log_group_arn(second_group),
                log_type: Some(LOG_TYPE_SERVICE),
                tags: &[],
            }],
        },
        // A destination of ANOTHER supported type: an Amazon S3 bucket created by the same
        // stack, whose ARN carries neither a Region nor an account.
        Row {
            case: "an Amazon S3 bucket destination created by the same stack",
            stack_name: "vpclattice-als-bucket",
            template: json!({
                "Resources": {
                    "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-bucket"}},
                    "Bucket": {
                        "Type": "AWS::S3::Bucket",
                        "Properties": {"BucketName": "sim2real-als-bucket"}
                    },
                    "Als": {
                        "Type": ACCESS_LOG_SUBSCRIPTION,
                        "Properties": {
                            "ResourceIdentifier": {"Ref": "Sn"},
                            "DestinationArn": {"Fn::GetAtt": ["Bucket", "Arn"]}
                        }
                    }
                },
                "Outputs": {
                    "SnRef": {"Value": {"Ref": "Sn"}},
                    "AlsRef": {"Value": {"Ref": "Als"}}
                }
            })
            .to_string(),
            network_ref_output: "SnRef",
            subscriptions: vec![Expected {
                ref_output: "AlsRef",
                id_output: None,
                destination_arn: "arn:aws:s3:::sim2real-als-bucket".to_string(),
                log_type: None,
                tags: &[],
            }],
        },
        // TWO subscriptions on ONE network, to destinations of the two different types, each
        // publishing its own identity.
        Row {
            case: "two subscriptions on one network, to a log group and to a bucket",
            stack_name: "vpclattice-als-pair",
            template: json!({
                "Resources": {
                    "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-pair"}},
                    "Lg": {
                        "Type": "AWS::Logs::LogGroup",
                        "Properties": {"LogGroupName": paired_group}
                    },
                    "Bucket": {
                        "Type": "AWS::S3::Bucket",
                        "Properties": {"BucketName": "sim2real-als-pair"}
                    },
                    "ToGroup": {
                        "Type": ACCESS_LOG_SUBSCRIPTION,
                        "Properties": {
                            "ResourceIdentifier": {"Ref": "Sn"},
                            "DestinationArn": {"Fn::GetAtt": ["Lg", "Arn"]},
                            "ServiceNetworkLogType": LOG_TYPE_SERVICE
                        }
                    },
                    "ToBucket": {
                        "Type": ACCESS_LOG_SUBSCRIPTION,
                        "Properties": {
                            "ResourceIdentifier": {"Ref": "Sn"},
                            "DestinationArn": {"Fn::GetAtt": ["Bucket", "Arn"]},
                            "Tags": [{"Key": "audit", "Value": "pair"}]
                        }
                    }
                },
                "Outputs": {
                    "SnRef": {"Value": {"Ref": "Sn"}},
                    "ToGroupRef": {"Value": {"Ref": "ToGroup"}},
                    "ToBucketRef": {"Value": {"Ref": "ToBucket"}}
                }
            })
            .to_string(),
            network_ref_output: "SnRef",
            subscriptions: vec![
                Expected {
                    ref_output: "ToGroupRef",
                    id_output: None,
                    destination_arn: format!("{}:*", log_group_arn(paired_group)),
                    log_type: Some(LOG_TYPE_SERVICE),
                    tags: &[],
                },
                Expected {
                    ref_output: "ToBucketRef",
                    id_output: None,
                    destination_arn: "arn:aws:s3:::sim2real-als-pair".to_string(),
                    log_type: None,
                    tags: &[("audit", "pair")],
                },
            ],
        },
    ]
}

/// Submit one stack of this suite, carrying the stack tag CloudFormation propagates.
async fn submitted(router: &MultiServiceRouter, case: &str, stack_name: &str, template: &str) {
    let (status, created) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 and a StackId from CreateStack, got {status} with \
         {created}"
    );
    assert!(
        !created.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected no template-format error for types AWS publishes, got {created}"
    );
    assert!(
        created.contains("<StackId>"),
        "case {case:?}: expected CreateStack to answer a StackId, got {created}"
    );
}

/// Submit one stack, wait for it to settle `CREATE_COMPLETE` and answer its `DescribeStacks`
/// body.
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
    let (_, described) = describe_stack(router, stack_name).await;
    described
}

/// The value of one published output, or a panic naming the body it was missing from.
fn published(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
}

#[tokio::test]
async fn a_declared_access_log_subscription_is_created_in_vpc_lattice() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        // The network the stack published resolves in that service by the ARN its `Ref`
        // returned, and the account's own listing reports that same ARN.
        let network = published(case, &described, row.network_ref_output);
        assert!(
            network.contains(":vpc-lattice:") && network.contains(":servicenetwork/sn-"),
            "case {case:?}: expected the network's Ref to be its VPC Lattice ARN, got {network:?}"
        );
        let (status, read) = get_service_network(&router, &network).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected VPC Lattice to hold the network {network:?}, got {status} \
             with {read}"
        );
        let listed = list_service_networks(&router).await;
        assert!(
            listed
                .iter()
                .any(|summary| summary.get("arn").and_then(Value::as_str)
                    == Some(network.as_str())),
            "case {case:?}: expected ListServiceNetworks to report {network:?}, got {listed:?}"
        );

        // And every subscription the stack declared is one that service holds, monitoring that
        // network, with the destination the template named.
        let reported = list_access_log_subscriptions(&router, &network).await;
        assert_eq!(
            reported.len(),
            row.subscriptions.len(),
            "case {case:?}: expected {} subscription(s) for {network:?}, got {reported:?}",
            row.subscriptions.len()
        );
        for expected in &row.subscriptions {
            let arn = published(case, &described, expected.ref_output);
            assert!(
                arn.contains(":accesslogsubscription/als-"),
                "case {case:?}: expected the subscription's Ref to be its ARN, got {arn:?}"
            );
            let (status, subscription) = get_access_log_subscription(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected VPC Lattice to hold the subscription {arn:?}, got \
                 {status} with {subscription}"
            );
            assert_eq!(
                subscription.get("destinationArn").and_then(Value::as_str),
                Some(expected.destination_arn.as_str()),
                "case {case:?}: expected the declared destination, got {subscription}"
            );
            assert_eq!(
                subscription.get("resourceArn").and_then(Value::as_str),
                Some(network.as_str()),
                "case {case:?}: expected the subscription to monitor the declared network, got \
                 {subscription}"
            );
            assert_eq!(
                subscription
                    .get("serviceNetworkLogType")
                    .and_then(Value::as_str),
                expected.log_type,
                "case {case:?}: expected the declared log type, got {subscription}"
            );
            // The id inside the ARN is what that service reports, and it is what the type
            // publishes as `Id` where the row reads it.
            let reported_id = subscription
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                arn.ends_with(reported_id) && reported_id.starts_with("als-"),
                "case {case:?}: expected the ARN to carry the id that service minted, got {arn:?} \
                 and {reported_id:?}"
            );
            if let Some(key) = expected.id_output {
                let read = published(case, &described, key);
                assert_eq!(
                    read, reported_id,
                    "case {case:?}: expected the published {key} to be the id that service \
                     minted, got {read:?}"
                );
            }
            // The listing reports the same subscription as the read does.
            assert!(
                reported
                    .iter()
                    .any(|summary| summary.get("arn").and_then(Value::as_str)
                        == Some(arn.as_str())),
                "case {case:?}: expected the listing to report {arn:?}, got {reported:?}"
            );
            // The tags it carries are the declaration's own beside the stack's.
            let held = list_tags(&router, &arn).await;
            for (key, value) in expected.tags {
                assert_eq!(
                    held.get(*key).and_then(Value::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?}, got {held}"
                );
            }
            assert_eq!(
                held.get(STACK_TAG_KEY).and_then(Value::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own tag on the subscription, got {held}"
            );
        }
    }
}

/// One refusal row: a destination no service of this account holds, and the service that is
/// asked about it.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The destination ARN the template states, which names a resource nothing created.
    destination_arn: String,
}

fn refused() -> Vec<Refused> {
    vec![
        // The issue's own refusal: a log group nothing created. CloudWatch Logs answers that it
        // holds no such group, so it is the VPC Lattice handler that refuses the create rather
        // than CloudFormation's own early validation.
        Refused {
            case: "a destination log group no stack created",
            stack_name: "vpclattice-als-missing-group",
            destination_arn: log_group_arn("/sim2real/als-missing"),
        },
        // The same class against another destination service, which is what makes the row about
        // the destination rather than about CloudWatch Logs.
        Refused {
            case: "a destination bucket no stack created",
            stack_name: "vpclattice-als-missing-bucket",
            destination_arn: "arn:aws:s3:::sim2real-als-missing".to_string(),
        },
    ]
}

#[tokio::test]
async fn a_destination_the_owning_service_does_not_hold_rolls_the_stack_back() {
    for row in refused() {
        let router = router();
        let case = row.case;
        let network_name = "als-rolled-back";
        let template = json!({
            "Resources": {
                "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": network_name}},
                "Als": {
                    "Type": ACCESS_LOG_SUBSCRIPTION,
                    "Properties": {
                        "ResourceIdentifier": {"Ref": "Sn"},
                        "DestinationArn": row.destination_arn,
                    }
                }
            },
            "Outputs": {"AlsRef": {"Value": {"Ref": "Als"}}}
        })
        .to_string();

        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             identity VPC Lattice never minted, got {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "AlsRef"),
            None,
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, inside
        // the envelope a resource provider reports one in — and never a refusal of the
        // template.
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES) && !events.contains("VALIDATION_ERROR"),
            "case {case:?}: expected no template-format error for types AWS publishes, got \
             {events}"
        );
        let expected_sentence = format!(
            "AccessLogSubscription destination {} cannot be found.",
            row.destination_arn
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let reason = reasons
            .iter()
            .find(|reason| reason.contains(&expected_sentence))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a failure reason carrying {expected_sentence:?}, got \
                     {reasons:?}"
                )
            });
        let (quoted, _, class) = envelope_parts(case, reason);
        assert_eq!(
            class, NOT_FOUND_CLASS,
            "case {case:?}: expected the provider to classify a 404 as {NOT_FOUND_CLASS}, got \
             {reason:?}"
        );
        assert_sdk_diagnostic_with_attempts(case, &quoted, VPC_LATTICE_SDK_NAME, NOT_FOUND_STATUS);

        // And nothing was left behind: the rolled-back network is gone from that service's own
        // listing.
        let listed = list_service_networks(&router).await;
        assert!(
            listed
                .iter()
                .all(|summary| summary.get("name").and_then(Value::as_str)
                    != Some(network_name)),
            "case {case:?}: expected no service network left for {network_name:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_update_that_drops_the_subscription_removes_it_and_leaves_the_network() {
    let router = router();
    let case = "an update that drops the subscription";
    let stack_name = "vpclattice-als-dropped";
    let group = "/sim2real/als-dropped";
    let with_subscription = json!({
        "Resources": {
            "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-dropped"}},
            "Lg": {"Type": "AWS::Logs::LogGroup", "Properties": {"LogGroupName": group}},
            "Als": {
                "Type": ACCESS_LOG_SUBSCRIPTION,
                "Properties": {
                    "ResourceIdentifier": {"Ref": "Sn"},
                    "DestinationArn": {"Fn::GetAtt": ["Lg", "Arn"]}
                }
            }
        },
        "Outputs": {
            "SnRef": {"Value": {"Ref": "Sn"}},
            "AlsRef": {"Value": {"Ref": "Als"}}
        }
    })
    .to_string();
    let without_subscription = json!({
        "Resources": {
            "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-dropped"}},
            "Lg": {"Type": "AWS::Logs::LogGroup", "Properties": {"LogGroupName": group}}
        },
        "Outputs": {"SnRef": {"Value": {"Ref": "Sn"}}}
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &with_subscription).await;
    let network = published(case, &described, "SnRef");
    let subscription = published(case, &described, "AlsRef");
    let (status, _) = get_access_log_subscription(&router, &subscription).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the subscription to exist before the update, got {status}"
    );

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &without_subscription),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    // The subscription the update dropped is gone from that service, in its own words — which
    // name back the identifier the READ submitted, so the read is made by the bare id.
    await_absence(&router, case, &subscription).await;
    let id = subscription
        .rsplit('/')
        .next()
        .expect("the subscription ARN carries its id");
    let (status, answered) = get_access_log_subscription(&router, id).await;
    assert_eq!(
        (
            status,
            answered.get("message").and_then(Value::as_str),
            answered.get("resourceType").and_then(Value::as_str),
        ),
        (
            NOT_FOUND_STATUS,
            Some(format!("AccessLogSubscription {id} cannot be found.").as_str()),
            Some("ACCESS_LOG_SUBSCRIPTION"),
        ),
        "case {case:?}: expected the dropped subscription to be gone in that service's own \
         words, got {status} with {answered}"
    );
    // ...while the network the update kept still answers.
    let (status, read) = get_service_network(&router, &network).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the network the update kept to still answer, got {status} with \
         {read}"
    );
    assert!(
        list_access_log_subscriptions(&router, &network)
            .await
            .is_empty(),
        "case {case:?}: expected no subscription left on the network"
    );
}

#[tokio::test]
async fn a_changed_destination_reaches_the_subscription_the_stack_has() {
    // `UpdateAccessLogSubscription` takes the subscription's destination, so a deployment that
    // states another one is applied to the subscription the stack already has — under the ARN
    // that service minted — rather than being dropped or replaced.
    let router = router();
    let case = "an update that re-points the destination at another log group";
    let stack_name = "vpclattice-als-repointed";
    let template_of = |destination: &str| {
        json!({
            "Resources": {
                "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-repointed"}},
                "First": {
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": {"LogGroupName": "/sim2real/als-first"}
                },
                "Second": {
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": {"LogGroupName": "/sim2real/als-second"}
                },
                "Als": {
                    "Type": ACCESS_LOG_SUBSCRIPTION,
                    "Properties": {
                        "ResourceIdentifier": {"Ref": "Sn"},
                        "DestinationArn": {"Fn::GetAtt": [destination, "Arn"]}
                    }
                }
            },
            "Outputs": {"AlsRef": {"Value": {"Ref": "Als"}}}
        })
        .to_string()
    };

    let described = deployed(&router, case, stack_name, &template_of("First")).await;
    let before = published(case, &described, "AlsRef");

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of("Second")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    let (_, described) = describe_stack(&router, stack_name).await;
    let after = published(case, &described, "AlsRef");
    assert_eq!(
        after, before,
        "case {case:?}: expected the changed destination to reach the subscription the stack \
         has rather than replacing it"
    );
    let (status, subscription) = get_access_log_subscription(&router, &after).await;
    assert_eq!(
        (
            status,
            subscription.get("destinationArn").and_then(Value::as_str),
        ),
        (
            200,
            Some(format!("{}:*", log_group_arn("/sim2real/als-second")).as_str()),
        ),
        "case {case:?}: expected the new destination on the same subscription, got {status} with \
         {subscription}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_network_the_subscription_and_the_log_group() {
    let router = router();
    let case = "a network, its subscription and its log group deleted with the stack";
    let stack_name = "vpclattice-als-deleted";
    let group = "/sim2real/als-deleted";
    let template = json!({
        "Resources": {
            "Sn": {"Type": SERVICE_NETWORK, "Properties": {"Name": "als-deleted"}},
            "Lg": {"Type": "AWS::Logs::LogGroup", "Properties": {"LogGroupName": group}},
            "Als": {
                "Type": ACCESS_LOG_SUBSCRIPTION,
                "Properties": {
                    "ResourceIdentifier": {"Ref": "Sn"},
                    "DestinationArn": {"Fn::GetAtt": ["Lg", "Arn"]}
                }
            }
        },
        "Outputs": {
            "SnRef": {"Value": {"Ref": "Sn"}},
            "AlsRef": {"Value": {"Ref": "Als"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let network = published(case, &described, "SnRef");
    let subscription = published(case, &described, "AlsRef");
    assert!(
        holds_the_log_group(&router, group).await,
        "case {case:?}: expected CloudWatch Logs to hold {group:?} before the delete"
    );

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
    );
    await_absence(&router, case, &subscription).await;

    // The resources the stack created are gone from the services that own them, and the read of
    // the network says so in that service's own words — which name back the identifier the READ
    // submitted, so the read is made by the bare id.
    let network_id = network
        .rsplit('/')
        .next()
        .expect("the network ARN carries its id");
    let (status, answered) = get_service_network(&router, network_id).await;
    assert_eq!(
        (
            status,
            answered.get("message").and_then(Value::as_str),
            answered.get("resourceType").and_then(Value::as_str),
        ),
        (
            NOT_FOUND_STATUS,
            Some(format!("ServiceNetwork {network_id} cannot be found.").as_str()),
            Some("SERVICE_NETWORK"),
        ),
        "case {case:?}: expected the network to be gone with the stack, got {status} with \
         {answered}"
    );
    let (status, answered) = get_access_log_subscription(&router, &subscription).await;
    assert_eq!(
        (status, answered.get("__type").and_then(Value::as_str)),
        (NOT_FOUND_STATUS, Some(RESOURCE_NOT_FOUND)),
        "case {case:?}: expected the subscription to be gone with the stack, got {status} with \
         {answered}"
    );
    assert!(
        !holds_the_log_group(&router, group).await,
        "case {case:?}: expected CloudWatch Logs to hold no {group:?} after the delete"
    );
}

/// Wait until that service no longer holds the subscription `arn`.
///
/// Waits for the CONDITION rather than for a span, under the harness's own generous deadline,
/// so a loaded host cannot decide the outcome.
async fn await_absence(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _) = get_access_log_subscription(router, arn).await;
        if status != 200 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the subscription {arn:?} to be released"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}
